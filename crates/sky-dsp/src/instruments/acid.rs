// SPDX-License-Identifier: GPL-3.0-or-later
//! The Acid bass: a monophonic bass line synth in the style of the classic
//! acid machines. One saw or square oscillator runs into a resonant 18 dB
//! low-pass filter that squelches without ever whistling on its own, swept
//! by a decaying envelope. Accented notes (velocity above 100) play louder,
//! with a short, brighter sweep that builds up over consecutive accents;
//! a note that starts while another is held slides to its pitch without
//! starting the envelopes again. A drive stage follows the filter.

use std::f32::consts::PI;

use crate::instruments::synth::oscillator;
use crate::instruments::{Adsr, Instrument, instrument_state};
use crate::params::ParamSpec;
use crate::util::{db_to_gain, midi_to_hz};

pub(crate) const ACID_PARAMS: &[ParamSpec] = &[
    ParamSpec::choice("wave", "Wave", &["saw", "square"], 0.0),
    ParamSpec::linear("tune", "Tune", -12.0, 12.0, 0.0, "st"),
    ParamSpec::log("cutoff", "Cutoff", 40.0, 8_000.0, 500.0, "Hz"),
    ParamSpec::linear("resonance", "Resonance", 0.0, 1.0, 0.7, ""),
    ParamSpec::linear("env_mod", "Env mod", 0.0, 1.0, 0.5, ""),
    ParamSpec::log("decay", "Decay", 30.0, 3_000.0, 400.0, "ms"),
    ParamSpec::linear("accent", "Accent", 0.0, 1.0, 0.6, ""),
    ParamSpec::log("slide", "Slide time", 5.0, 500.0, 60.0, "ms"),
    ParamSpec::linear("drive", "Drive", 0.0, 24.0, 6.0, "dB"),
    ParamSpec::linear("gain", "Gain", -60.0, 12.0, -6.0, "dB"),
];

/// Velocities above this play accented.
pub const ACCENT_THRESHOLD: u8 = 100;

const CONTROL_RATE: usize = 16;
/// Keys held at once that the slide logic remembers.
const STACK: usize = 16;
/// How far the envelope opens the filter at full env mod, in octaves.
const ENV_OCTAVES: f32 = 4.0;
/// How far a full accent sweep opens the filter, in octaves.
const ACCENT_OCTAVES: f32 = 6.0;
/// Accented notes sweep the filter this fast, whatever the decay.
const ACCENT_DECAY_MS: f32 = 200.0;
/// How fast the accent sweep charges from the envelope, per second: one
/// accent with the shortest decay charges it to about half.
const ACCENT_CHARGE: f32 = 17.0;
/// How slowly the accent sweep lets go: consecutive accents land before it
/// has, so each one opens the filter further.
const ACCENT_HOLD_MS: f32 = 150.0;
/// Feedback below the 8 where a three-pole ladder starts to oscillate, so
/// full resonance squelches but never whistles on its own.
const MAX_FEEDBACK: f32 = 7.2;

#[derive(Clone, Copy, Debug)]
struct Params {
    wave: u8,
    tune: f32,
    cutoff: f32,
    resonance: f32,
    env_mod: f32,
    decay: f32,
    accent: f32,
    slide: f32,
    drive: f32,
    gain: f32,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            wave: 0,
            tune: 0.0,
            cutoff: 500.0,
            resonance: 0.7,
            env_mod: 0.5,
            decay: 400.0,
            accent: 0.6,
            slide: 60.0,
            drive: db_to_gain(6.0),
            gain: db_to_gain(-6.0),
        }
    }
}

/// Three one-pole low-passes in a feedback loop, solved without a unit
/// delay (topology-preserving transform) so the resonance stays in tune
/// and stable at any cutoff. The input saturates before the poles.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Ladder {
    s: [f32; 3],
}

impl Ladder {
    /// One sample through the filter. `g` is the warped cutoff,
    /// `tan(pi * fc / sr)`, and `k` the feedback.
    #[inline]
    pub(crate) fn tick(&mut self, x: f32, g: f32, k: f32) -> f32 {
        let gg = g / (1.0 + g);
        let h = 1.0 - gg;
        let [s1, s2, s3] = self.s;
        // What the last pole would output from the states alone.
        let free = gg * gg * h * s1 + gg * h * s2 + h * s3;
        let u = ((x - k * free) / (1.0 + k * gg * gg * gg)).tanh();
        let y1 = gg * u + h * s1;
        let y2 = gg * y1 + h * s2;
        let y3 = gg * y2 + h * s3;
        self.s = [2.0 * y1 - s1, 2.0 * y2 - s2, 2.0 * y3 - s3];
        y3
    }

    fn reset(&mut self) {
        self.s = [0.0; 3];
    }
}

#[derive(Clone, Copy, Debug)]
struct Voice {
    /// Keys held, the newest last.
    keys: [u8; STACK],
    held: usize,
    /// The key sounding, in semitones, and the one it slides towards.
    pitch: f32,
    target: f32,
    phase: f32,
    amp: Adsr,
    /// The filter envelope, from 1 down to 0.
    env: f32,
    accented: bool,
    /// The accent sweep, which builds up over consecutive accents.
    sweep: f32,
    /// The accent's extra level, smoothed so a slide into an accent does
    /// not click.
    boost: f32,
    filter: Ladder,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            keys: [0; STACK],
            held: 0,
            pitch: 36.0,
            target: 36.0,
            phase: 0.0,
            amp: Adsr::default(),
            env: 0.0,
            accented: false,
            sweep: 0.0,
            boost: 0.0,
            filter: Ladder::default(),
        }
    }
}

impl Voice {
    fn top(&self) -> Option<u8> {
        self.held
            .checked_sub(1)
            .and_then(|i| self.keys.get(i))
            .copied()
    }

    fn remove(&mut self, key: u8) {
        let mut kept = 0;
        for i in 0..self.held.min(STACK) {
            let k = self.keys[i];
            if k != key {
                self.keys[kept] = k;
                kept += 1;
            }
        }
        self.held = kept;
    }

    fn push(&mut self, key: u8) {
        self.remove(key);
        if self.held == STACK {
            // Forget the oldest key.
            self.keys.copy_within(1.., 0);
            self.held -= 1;
        }
        self.keys[self.held] = key;
        self.held += 1;
    }
}

fn per_sample(ms: f32, sample_rate: f32) -> f32 {
    // Reaches -60 dB after `ms`.
    (-6.907_755 / (ms * 0.001 * sample_rate).max(1.0)).exp()
}

fn follow(ms: f32, sample_rate: f32) -> f32 {
    // A one-pole follower with time constant `ms`.
    1.0 - (-1.0 / (ms * 0.001 * sample_rate).max(1.0)).exp()
}

pub struct Acid {
    sample_rate: f32,
    params: Params,
    voice: Voice,
}

impl Acid {
    pub fn new(sample_rate: f32) -> Self {
        let mut voice = Voice::default();
        voice.amp.set(2.0, 1_500.0, 0.5, 8.0, sample_rate);
        Self {
            sample_rate,
            params: Params::default(),
            voice,
        }
    }
}

impl Instrument for Acid {
    fn set_param(&mut self, name: &str, value: f32) -> bool {
        let p = &mut self.params;
        match name {
            "wave" => p.wave = value.round().clamp(0.0, 1.0) as u8,
            "tune" => p.tune = value,
            "cutoff" => p.cutoff = value,
            "resonance" => p.resonance = value.clamp(0.0, 1.0),
            "env_mod" => p.env_mod = value,
            "decay" => p.decay = value,
            "accent" => p.accent = value,
            "slide" => p.slide = value,
            "drive" => p.drive = db_to_gain(value),
            "gain" => p.gain = db_to_gain(value),
            _ => return false,
        }
        true
    }

    fn note_on(&mut self, pitch: u8, velocity: u8) {
        let v = &mut self.voice;
        let legato = v.held > 0 && v.amp.is_active();
        v.push(pitch);
        v.target = f32::from(pitch);
        v.accented = velocity > ACCENT_THRESHOLD;
        if legato {
            // Slide: the pitch glides and nothing starts again.
            return;
        }
        if !v.amp.is_active() {
            v.phase = 0.0;
            v.filter.reset();
        }
        v.pitch = v.target;
        v.env = 1.0;
        v.amp.trigger();
    }

    fn note_off(&mut self, pitch: u8) {
        let v = &mut self.voice;
        let sounding = v.top() == Some(pitch);
        v.remove(pitch);
        match v.top() {
            None => v.amp.release(),
            // Back to the key still held, gliding.
            Some(key) if sounding => v.target = f32::from(key),
            Some(_) => {}
        }
    }

    fn all_notes_off(&mut self) {
        self.voice.held = 0;
        self.voice.amp.release();
    }

    fn reset(&mut self) {
        let amp = self.voice.amp;
        self.voice = Voice {
            amp,
            ..Voice::default()
        };
        self.voice.amp.reset();
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let p = self.params;
        let sr = self.sample_rate;
        let v = &mut self.voice;
        if !v.amp.is_active() {
            return;
        }
        let glide = follow(p.slide / 3.0, sr);
        let decay = per_sample(
            if v.accented {
                p.decay.min(ACCENT_DECAY_MS)
            } else {
                p.decay
            },
            sr,
        );
        let charge = ACCENT_CHARGE / sr;
        let leak = follow(ACCENT_HOLD_MS, sr);
        let boost_k = follow(5.0, sr);
        let boost_target = if v.accented { p.accent } else { 0.0 };
        let k = p.resonance * MAX_FEEDBACK;
        // The ladder loses bass as the feedback rises; give some back.
        let makeup = 1.0 + 0.5 * k;
        let drive_norm = 1.0 / p.drive.tanh();
        for (lc, rc) in left
            .chunks_mut(CONTROL_RATE)
            .zip(right.chunks_mut(CONTROL_RATE))
        {
            let dt = (midi_to_hz(v.pitch + p.tune) / sr).min(0.49);
            let octaves = p.env_mod * ENV_OCTAVES * v.env + p.accent * ACCENT_OCTAVES * v.sweep;
            let fc = (p.cutoff * 2f32.powf(octaves)).min(sr * 0.42);
            let g = (PI * fc / sr).tan();
            for (l, r) in lc.iter_mut().zip(rc.iter_mut()) {
                v.pitch += (v.target - v.pitch) * glide;
                let osc = oscillator(p.wave, v.phase, dt);
                v.phase += dt;
                if v.phase >= 1.0 {
                    v.phase -= 1.0;
                }
                let filtered = v.filter.tick(osc * 0.6, g, k) * makeup;
                let driven = (filtered * p.drive).tanh() * drive_norm;
                v.env *= decay;
                let accent_in = if v.accented { v.env } else { 0.0 };
                v.sweep = (v.sweep + accent_in * charge - v.sweep * leak).min(2.0);
                v.boost += (boost_target - v.boost) * boost_k;
                let out = driven * v.amp.tick() * (1.0 + v.boost) * p.gain;
                *l += out;
                *r += out;
            }
            if !v.amp.is_active() {
                break;
            }
        }
    }

    instrument_state!(voice);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(inst: &mut Acid, seconds: f32) -> Vec<f32> {
        let mut l = vec![0.0; (48_000.0 * seconds) as usize];
        let mut r = l.clone();
        for (lc, rc) in l.chunks_mut(128).zip(r.chunks_mut(128)) {
            inst.render(lc, rc);
        }
        l
    }

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    /// How much of the signal is high frequencies: the energy of its
    /// changes against its energy.
    fn brightness(buf: &[f32]) -> f32 {
        let diff: f32 = buf.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum();
        let energy: f32 = buf.iter().map(|s| s * s).sum();
        diff / energy.max(1e-12)
    }

    /// Rising zero crossings per second in a window, a rough pitch.
    fn crossings(buf: &[f32]) -> f32 {
        let n = buf.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        n as f32 * 48_000.0 / buf.len() as f32
    }

    fn acid() -> Acid {
        let mut a = Acid::new(48_000.0);
        for spec in ACID_PARAMS {
            a.set_param(spec.name, spec.default);
        }
        a
    }

    #[test]
    fn accents_play_louder_and_brighter() {
        let note = |velocity: u8| {
            let mut a = acid();
            a.note_on(36, velocity);
            render(&mut a, 0.1)
        };
        let (plain, accent) = (note(100), note(127));
        assert!(peak(&accent) > 1.2 * peak(&plain));
        let window = 960..4_800;
        assert!(
            brightness(&accent[window.clone()]) > 1.3 * brightness(&plain[window.clone()]),
            "{} {}",
            brightness(&plain[window.clone()]),
            brightness(&accent[window])
        );
    }

    #[test]
    fn consecutive_accents_build_up() {
        let mut a = acid();
        let mut first = 0.0;
        let mut last = 0.0;
        // Accented sixteenths at 128 BPM.
        for i in 0..4 {
            a.note_on(36, 127);
            let buf = render(&mut a, 0.06);
            a.note_off(36);
            let _ = render(&mut a, 0.057);
            let b = brightness(&buf[480..2_400]);
            if i == 0 {
                first = b;
            }
            last = b;
        }
        assert!(last > 1.1 * first, "{first} -> {last}");
    }

    #[test]
    fn a_note_over_a_held_one_slides_without_starting_again() {
        let mut a = acid();
        a.set_param("resonance", 0.0);
        a.set_param("env_mod", 0.0);
        a.set_param("cutoff", 8_000.0);
        a.note_on(45, 100); // 110 Hz
        let _ = render(&mut a, 0.2);
        let env_before = a.voice.env;
        a.note_on(57, 100); // 220 Hz, legato
        assert_eq!(a.voice.env, env_before, "the envelope started again");
        let early = render(&mut a, 0.02);
        let settled = {
            let _ = render(&mut a, 0.1);
            render(&mut a, 0.1)
        };
        let mid = crossings(&early);
        assert!((120.0..210.0).contains(&mid), "{mid}");
        let end = crossings(&settled);
        assert!((210.0..230.0).contains(&end), "{end}");
        // Letting go of the new key glides back to the one still held.
        a.note_off(57);
        let _ = render(&mut a, 0.2);
        let back = crossings(&render(&mut a, 0.1));
        assert!((100.0..120.0).contains(&back), "{back}");
        // Letting go of the last key ends the note.
        a.note_off(45);
        let _ = render(&mut a, 0.1);
        assert!(!a.voice.amp.is_active());
    }

    #[test]
    fn a_new_note_after_a_gap_starts_again() {
        let mut a = acid();
        a.note_on(36, 100);
        let _ = render(&mut a, 0.3);
        a.note_off(36);
        let decayed = a.voice.env;
        a.note_on(43, 100);
        assert_eq!(a.voice.env, 1.0);
        assert!(decayed < 0.5);
        assert_eq!(a.voice.pitch, 43.0);
    }

    #[test]
    fn full_resonance_squelches_but_never_rings_on_its_own() {
        let g = (PI * 1_000.0 / 48_000.0).tan();
        let mut f = Ladder::default();
        let mut out = Vec::new();
        for i in 0..48_000 {
            out.push(f.tick(if i == 0 { 1.0 } else { 0.0 }, g, MAX_FEEDBACK));
        }
        let head = peak(&out[..4_800]);
        let tail = peak(&out[24_000..]);
        assert!(head > 0.01 && tail < 1e-6 * head.max(1.0), "{head} {tail}");
        // And the full instrument stays finite and bounded.
        let mut a = acid();
        a.set_param("resonance", 1.0);
        a.set_param("env_mod", 1.0);
        a.set_param("drive", 24.0);
        a.set_param("gain", 0.0);
        a.note_on(24, 127);
        let buf = render(&mut a, 0.5);
        assert!(buf.iter().all(|s| s.is_finite() && s.abs() <= 2.5));
        assert!(peak(&buf) > 0.1);
    }

    #[test]
    fn the_key_stack_never_overflows() {
        let mut a = acid();
        for key in 0..40 {
            a.note_on(key, 100);
        }
        assert_eq!(a.voice.held, STACK);
        for key in 0..40 {
            a.note_off(key);
        }
        assert_eq!(a.voice.held, 0);
    }
}
