// SPDX-License-Identifier: GPL-3.0-or-later
//! Sidechain ducking driven by another track's notes.
//!
//! Instead of listening to the kick's audio, the duck reads when the kick
//! track's notes start (the engine passes them in with
//! [`Effect::sidechain_note`]) and dips the volume in a tempo-synced curve.
//! It reacts on the exact sample of the note, with no detection delay, and
//! keeps working when the kick track is muted.

use crate::effects::{Effect, effect_state};
use crate::params::ParamSpec;

pub(crate) const DUCK_PARAMS: &[ParamSpec] = &[
    ParamSpec::linear("depth", "Depth", 0.0, 1.0, 0.75, ""),
    ParamSpec::log("length", "Length", 0.0625, 4.0, 0.5, "beats"),
    ParamSpec::linear("curve", "Curve", -1.0, 1.0, 0.5, ""),
    ParamSpec::linear("attack", "Attack", 0.0, 30.0, 2.0, "ms"),
    ParamSpec::choice("trigger", "Trigger", &["any note", "kick only"], 0.0),
];

/// Most note starts one block can carry; more are ignored.
const MAX_PENDING: usize = 64;

pub struct Duck {
    sample_rate: f32,
    bpm: f64,
    depth: f32,
    beats: f32,
    /// The recovery curve's exponent: above 1 stays down longer, below 1
    /// comes back sooner.
    power: f32,
    attack_ms: f32,
    kick_only: bool,
    length: f32,
    attack: f32,
    /// Frame offsets of the notes that start in the next block.
    pending: [u32; MAX_PENDING],
    pending_len: usize,
    /// Frames since the last note started.
    elapsed: f32,
    /// The gain when the last note started, so a dip that starts before
    /// the previous one has recovered does not jump.
    from: f32,
    gain: f32,
    /// The lowest gain in the last block, for meters.
    block_gain: f32,
}

impl Duck {
    pub fn new(sample_rate: f32) -> Self {
        let mut duck = Self {
            sample_rate,
            bpm: 120.0,
            depth: 0.75,
            beats: 0.5,
            power: 2.0,
            attack_ms: 2.0,
            kick_only: false,
            length: 0.0,
            attack: 0.0,
            pending: [0; MAX_PENDING],
            pending_len: 0,
            elapsed: f32::MAX,
            from: 1.0,
            gain: 1.0,
            block_gain: 1.0,
        };
        duck.update_times();
        duck
    }

    fn update_times(&mut self) {
        let beat = 60.0 / self.bpm.max(1.0) as f32 * self.sample_rate;
        self.length = (self.beats * beat).max(1.0);
        self.attack = (self.attack_ms * 0.001 * self.sample_rate).min(self.length * 0.5);
    }

    /// The gain `elapsed` frames after a note started.
    #[inline]
    fn envelope(&self) -> f32 {
        crate::curves::duck_gain(
            self.from,
            self.depth,
            self.power,
            self.attack,
            self.length,
            self.elapsed,
        )
    }
}

impl Effect for Duck {
    fn set_param(&mut self, name: &str, value: f32) -> bool {
        match name {
            "depth" => self.depth = value.clamp(0.0, 1.0),
            "length" => self.beats = value.clamp(0.0625, 4.0),
            "curve" => self.power = crate::curves::duck_power(value),
            "attack" => self.attack_ms = value.clamp(0.0, 30.0),
            "trigger" => self.kick_only = value >= 0.5,
            _ => return false,
        }
        self.update_times();
        true
    }

    fn set_tempo(&mut self, bpm: f64) {
        self.bpm = bpm;
        self.update_times();
    }

    fn sidechain_note(&mut self, offset: usize, pitch: u8) {
        if self.kick_only && !matches!(pitch, 35 | 36) {
            return;
        }
        if let Some(slot) = self.pending.get_mut(self.pending_len) {
            *slot = offset.min(u32::MAX as usize) as u32;
            self.pending_len += 1;
        }
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.block_gain = 1.0;
        let mut next = 0;
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            while next < self.pending_len && self.pending[next] as usize <= i {
                self.from = self.gain;
                self.elapsed = 0.0;
                next += 1;
            }
            let g = self.envelope();
            self.gain = g;
            self.block_gain = self.block_gain.min(g);
            if self.elapsed < self.length {
                self.elapsed += 1.0;
            }
            *l *= g;
            *r *= g;
        }
        // Notes past the end of a short block start at its end.
        if next < self.pending_len {
            self.from = self.gain;
            self.elapsed = 0.0;
        }
        self.pending_len = 0;
    }

    fn gain_reduction_db(&self) -> f32 {
        -crate::util::gain_to_db(self.block_gain)
    }

    fn reset(&mut self) {
        self.pending_len = 0;
        self.elapsed = f32::MAX;
        self.from = 1.0;
        self.gain = 1.0;
        self.block_gain = 1.0;
    }

    effect_state!(elapsed, from, gain);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(duck: &mut Duck, frames: usize) -> Vec<f32> {
        let mut l = vec![1.0; frames];
        let mut r = vec![1.0; frames];
        duck.process(&mut l, &mut r);
        assert_eq!(l, r);
        l
    }

    #[test]
    fn dips_on_a_note_and_recovers_within_its_length() {
        let mut duck = Duck::new(48_000.0);
        duck.set_tempo(120.0);
        duck.set_param("depth", 1.0);
        duck.set_param("length", 1.0);
        duck.set_param("attack", 0.0);
        assert!(block(&mut duck, 512).iter().all(|&g| g == 1.0));
        duck.sidechain_note(100, 36);
        let out = block(&mut duck, 512);
        assert_eq!(out[99], 1.0);
        assert!(out[100] < 0.01, "{}", out[100]);
        // One beat at 120 BPM is 24000 frames; half way it is still down
        // (curve 0.5 holds), and after the beat it is back to unity.
        let mut rest = Vec::new();
        for _ in 0..50 {
            rest.extend(block(&mut duck, 512));
        }
        let half = rest[12_000 - 412];
        assert!(half > 0.1 && half < 0.5, "{half}");
        assert_eq!(rest[24_000 - 412 + 1], 1.0);
    }

    #[test]
    fn kick_only_ignores_other_notes() {
        let mut duck = Duck::new(48_000.0);
        duck.set_param("trigger", 1.0);
        duck.sidechain_note(0, 42);
        assert!(block(&mut duck, 256).iter().all(|&g| g == 1.0));
        duck.sidechain_note(0, 36);
        assert!(block(&mut duck, 256)[200] < 0.9);
    }

    #[test]
    fn length_follows_the_tempo() {
        let mut slow = Duck::new(48_000.0);
        slow.set_tempo(60.0);
        let mut fast = Duck::new(48_000.0);
        fast.set_tempo(240.0);
        for d in [&mut slow, &mut fast] {
            d.set_param("length", 1.0);
            d.sidechain_note(0, 36);
        }
        let mut s = Vec::new();
        let mut f = Vec::new();
        for _ in 0..40 {
            s.extend(block(&mut slow, 512));
            f.extend(block(&mut fast, 512));
        }
        // At 240 BPM a beat is 12000 frames; at 60 BPM, 48000.
        assert_eq!(f[12_001], 1.0);
        assert!(s[12_001] < 0.9);
    }

    #[test]
    fn a_new_note_mid_dip_does_not_jump() {
        let mut duck = Duck::new(48_000.0);
        duck.set_param("attack", 5.0);
        duck.sidechain_note(0, 36);
        let first = block(&mut duck, 512);
        duck.sidechain_note(0, 36);
        let second = block(&mut duck, 512);
        let step = (second[0] - first[511]).abs();
        assert!(step < 0.02, "jumped by {step}");
    }
}
