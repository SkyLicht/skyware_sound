// SPDX-License-Identifier: GPL-3.0-or-later
//! A sample bank: named sounds, each with variations, like Strudel's
//! `s("bd sd:2").bank("RolandTR808")`.
//!
//! The engine builds a [`BankMap`] from the project when it compiles a
//! track. Pattern sounds start a sample directly with
//! [`Instrument::sound_on`]; MIDI notes go through the map's keys, which
//! hold either the drum sounds on their General MIDI notes or one pitched
//! sound spread across the keyboard.

use std::sync::Arc;

use crate::AudioData;
use crate::instruments::{Adsr, Instrument};
use crate::params::ParamSpec;
use crate::util::db_to_gain;

pub(crate) const BANK_PARAMS: &[ParamSpec] = &[
    ParamSpec::linear("tune", "Tune", -24.0, 24.0, 0.0, "st"),
    ParamSpec::log("attack", "Attack", 0.5, 5_000.0, 0.5, "ms"),
    ParamSpec::log("release", "Release", 1.0, 10_000.0, 100.0, "ms"),
    ParamSpec::toggle("one_shot", "One-shot notes", true),
    ParamSpec::linear("gain", "Gain", -60.0, 12.0, 0.0, "dB"),
];

const VOICES: usize = 32;
/// The key of a voice started by a pattern sound rather than a note.
const NO_KEY: u8 = u8::MAX;

/// What one MIDI key plays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeyZone {
    /// Index into [`BankMap::samples`].
    pub sample: u32,
    /// How far to shift the sample's pitch, in semitones.
    pub semitones: f32,
}

/// The samples a bank plays and how notes reach them.
#[derive(Clone, Debug, Default)]
pub struct BankMap {
    /// Every sample the bank can play; [`Instrument::sound_on`] indexes it.
    pub samples: Vec<Arc<AudioData>>,
    /// What each MIDI key 0-127 plays. Shorter lists leave the rest silent.
    pub keys: Vec<Option<KeyZone>>,
}

impl BankMap {
    /// True when both maps play the same decoded audio in the same order.
    fn same_samples(&self, other: &BankMap) -> bool {
        self.samples.len() == other.samples.len()
            && self
                .samples
                .iter()
                .zip(&other.samples)
                .all(|(a, b)| Arc::ptr_eq(a, b))
    }
}

#[derive(Clone, Copy, Debug)]
struct Voice {
    sample: u32,
    key: u8,
    pos: f64,
    rate: f64,
    velocity: f32,
    age: u64,
    env: Adsr,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            sample: 0,
            key: NO_KEY,
            pos: 0.0,
            rate: 1.0,
            velocity: 0.0,
            age: 0,
            env: Adsr::default(),
        }
    }
}

pub struct Bank {
    sample_rate: f32,
    map: Arc<BankMap>,
    tune: f32,
    attack: f32,
    release: f32,
    one_shot: bool,
    gain: f32,
    voices: [Voice; VOICES],
    clock: u64,
}

impl Bank {
    pub fn new(sample_rate: f32, map: Arc<BankMap>) -> Self {
        Self {
            sample_rate,
            map,
            tune: 0.0,
            attack: 0.5,
            release: 100.0,
            one_shot: true,
            gain: 1.0,
            voices: [Voice::default(); VOICES],
            clock: 0,
        }
    }

    fn start(&mut self, sample: u32, key: u8, semitones: f32, velocity: u8) {
        let Some(data) = self.map.samples.get(sample as usize) else {
            return;
        };
        let i = self
            .voices
            .iter()
            .position(|v| !v.env.is_active())
            .unwrap_or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, v)| v.age)
                    .map_or(0, |(i, _)| i)
            });
        self.clock += 1;
        let shift = f64::from(semitones + self.tune) / 12.0;
        let rate = 2f64.powf(shift) * f64::from(data.sample_rate) / f64::from(self.sample_rate);
        let v = &mut self.voices[i];
        v.sample = sample;
        v.key = key;
        v.pos = 0.0;
        v.rate = rate;
        v.velocity = f32::from(velocity) / 127.0;
        v.age = self.clock;
        v.env
            .set(self.attack, 1.0, 1.0, self.release, self.sample_rate);
        v.env.trigger();
    }
}

impl Instrument for Bank {
    fn set_param(&mut self, name: &str, value: f32) -> bool {
        match name {
            "tune" => self.tune = value,
            "attack" => self.attack = value,
            "release" => self.release = value,
            "one_shot" => self.one_shot = value >= 0.5,
            "gain" => self.gain = db_to_gain(value),
            _ => return false,
        }
        true
    }

    fn note_on(&mut self, pitch: u8, velocity: u8) {
        if let Some(Some(zone)) = self.map.keys.get(pitch as usize).copied() {
            self.start(zone.sample, pitch, zone.semitones, velocity);
        }
    }

    fn note_off(&mut self, pitch: u8) {
        if self.one_shot {
            return;
        }
        for v in &mut self.voices {
            if v.key == pitch {
                v.env.release();
            }
        }
    }

    fn sound_on(&mut self, sound: u32, velocity: u8) {
        self.start(sound, NO_KEY, 0.0, velocity);
    }

    fn all_notes_off(&mut self) {
        for v in &mut self.voices {
            v.env.release();
        }
    }

    fn reset(&mut self) {
        for v in &mut self.voices {
            v.env.reset();
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let map = &self.map;
        for v in self.voices.iter_mut().filter(|v| v.env.is_active()) {
            let Some(data) = map.samples.get(v.sample as usize) else {
                v.env.reset();
                continue;
            };
            let (src_l, src_r) = (data.left(), data.right());
            let len = src_l.len().min(src_r.len());
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                let i = v.pos as usize;
                if i + 1 >= len {
                    v.env.reset();
                    break;
                }
                let frac = (v.pos - i as f64) as f32;
                let sl = src_l[i] + (src_l[i + 1] - src_l[i]) * frac;
                let sr = src_r[i] + (src_r[i + 1] - src_r[i]) * frac;
                let g = v.env.tick() * v.velocity * self.gain;
                *l += sl * g;
                *r += sr * g;
                v.pos += v.rate;
            }
        }
    }

    fn adopt(&mut self, previous: &mut dyn Instrument) -> bool {
        match previous.as_any_mut().downcast_mut::<Self>() {
            // Voices point into the sample list, so they only carry over
            // when it has not changed.
            Some(prev) if prev.map.same_samples(&self.map) => {
                std::mem::swap(&mut self.voices, &mut prev.voices);
                std::mem::swap(&mut self.clock, &mut prev.clock);
                true
            }
            _ => false,
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(frames: usize, sample_rate: u32) -> Arc<AudioData> {
        Arc::new(AudioData {
            sample_rate,
            channels: vec![vec![0.5; frames]],
        })
    }

    fn map() -> Arc<BankMap> {
        let mut keys = vec![None; 128];
        keys[36] = Some(KeyZone {
            sample: 0,
            semitones: 0.0,
        });
        keys[60] = Some(KeyZone {
            sample: 1,
            semitones: 12.0,
        });
        Arc::new(BankMap {
            samples: vec![click(4_800, 24_000), click(4_800, 48_000)],
            keys,
        })
    }

    fn render(bank: &mut Bank, frames: usize) -> Vec<f32> {
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        bank.render(&mut l, &mut r);
        l
    }

    /// How many frames sounded, give or take the interpolation's last frame.
    fn assert_sounding(out: &[f32], expected: usize) {
        let n = out.iter().filter(|s| s.abs() > 1e-4).count();
        assert!(
            n.abs_diff(expected) <= 2,
            "{n} frames sounded, expected {expected}"
        );
    }

    #[test]
    fn sounds_play_their_whole_sample_at_its_own_rate() {
        let mut bank = Bank::new(48_000.0, map());
        // 4800 frames recorded at 24 kHz last 9600 frames at 48 kHz.
        bank.sound_on(0, 127);
        assert_sounding(&render(&mut bank, 12_000), 9_600);
        bank.sound_on(7, 127);
        assert_sounding(&render(&mut bank, 512), 0);
    }

    #[test]
    fn keys_follow_the_map_and_pitch() {
        let mut bank = Bank::new(48_000.0, map());
        bank.note_on(61, 100);
        assert_sounding(&render(&mut bank, 512), 0);
        // Key 60 plays sample 1 an octave up: twice as fast.
        bank.note_on(60, 100);
        assert_sounding(&render(&mut bank, 4_800), 2_400);
        bank.set_param("tune", -12.0);
        bank.note_on(60, 100);
        assert_sounding(&render(&mut bank, 6_000), 4_800);
    }

    #[test]
    fn gated_notes_stop_but_sounds_ring() {
        let mut bank = Bank::new(48_000.0, map());
        bank.set_param("one_shot", 0.0);
        bank.set_param("release", 1.0);
        bank.note_on(36, 100);
        bank.sound_on(0, 100);
        let _ = render(&mut bank, 100);
        bank.note_off(36);
        let _ = render(&mut bank, 200);
        assert_eq!(bank.voices.iter().filter(|v| v.env.is_active()).count(), 1);
    }

    #[test]
    fn voices_carry_over_only_with_the_same_samples() {
        let shared = map();
        let mut a = Bank::new(48_000.0, shared.clone());
        a.sound_on(0, 100);
        let _ = render(&mut a, 64);
        let mut b = Bank::new(48_000.0, Arc::new((*shared).clone()));
        assert!(b.adopt(&mut a));
        assert_sounding(&render(&mut b, 64), 64);
        let mut c = Bank::new(48_000.0, Arc::new(BankMap::default()));
        assert!(!c.adopt(&mut b));
    }
}
