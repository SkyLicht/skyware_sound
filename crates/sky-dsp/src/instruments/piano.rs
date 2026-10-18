// SPDX-License-Identifier: GPL-3.0-or-later
//! A sampled piano on the multi-sample engine.
//!
//! The library brings the recordings and how they are laid out; the piano
//! decides how hard each note plays: velocity goes through a curve, then
//! picks the velocity layer and the gain. A mono library is spread across
//! the stereo field by key, as a piano sounds from the bench.

use std::sync::Arc;

use crate::MAX_BLOCK;
use crate::instruments::Instrument;
use crate::instruments::sampled::{NoteShape, SampleLibrary, SampleVoices};
use crate::params::ParamSpec;
use crate::util::db_to_gain;

pub(crate) const PIANO_PARAMS: &[ParamSpec] = &[
    ParamSpec::linear("tone", "Tone", -12.0, 12.0, 0.0, "dB"),
    ParamSpec::linear("vel_curve", "Velocity curve", -1.0, 1.0, 0.0, ""),
    ParamSpec::linear("dynamics", "Dynamics", 0.0, 60.0, 30.0, "dB"),
    ParamSpec::log("release", "Release", 10.0, 400.0, 100.0, "%"),
    ParamSpec::linear("width", "Width", -100.0, 100.0, 100.0, "%"),
    ParamSpec::linear("tune", "Tune", -100.0, 100.0, 0.0, "ct"),
    ParamSpec::linear("gain", "Gain", -60.0, 12.0, 0.0, "dB"),
];

/// Where the tone control tilts the spectrum around.
const TILT_HZ: f32 = 700.0;
/// How far a mono library spreads the outer keys from the centre.
const KEY_SPREAD: f32 = 0.6;
/// Recorded pianos are normalized close to full scale. This brings a key
/// down to where a synth note peaks, so chords have room.
const HEADROOM_DB: f32 = -9.0;

pub struct Piano {
    voices: SampleVoices,
    tone: f32,
    vel_curve: f32,
    dynamics: f32,
    release: f32,
    width: f32,
    tune: f32,
    gain: f32,
    tilt_coeff: f32,
    /// The tone control's low-pass state, left and right.
    tilt: [f32; 2],
    scratch: [Vec<f32>; 2],
}

impl Piano {
    pub fn new(sample_rate: f32, library: Arc<SampleLibrary>) -> Self {
        Self {
            voices: SampleVoices::new(sample_rate, library),
            tone: 0.0,
            vel_curve: 0.0,
            dynamics: 30.0,
            release: 1.0,
            width: 1.0,
            tune: 0.0,
            gain: db_to_gain(HEADROOM_DB),
            tilt_coeff: 1.0 - (-std::f32::consts::TAU * TILT_HZ / sample_rate).exp(),
            tilt: [0.0; 2],
            scratch: [vec![0.0; MAX_BLOCK], vec![0.0; MAX_BLOCK]],
        }
    }

    pub fn library(&self) -> &Arc<SampleLibrary> {
        self.voices.library()
    }

    /// Velocity 1-127 after the curve, as 0 to 1.
    fn curved(&self, velocity: u8) -> f32 {
        let v = f32::from(velocity.clamp(1, 127)) / 127.0;
        v.powf(2f32.powf(-1.5 * self.vel_curve))
    }

    fn shape(&self, key: u8, curved: f32) -> NoteShape {
        // From the bench, low keys sit on the left.
        let place = ((f32::from(key) - 64.5) / 44.0).clamp(-1.0, 1.0);
        NoteShape {
            gain: db_to_gain(-self.dynamics * (1.0 - curved)),
            tune: self.tune / 100.0,
            pan: 0.0,
            mono_pan: place * self.width * KEY_SPREAD,
            width: self.width,
            release: self.release,
        }
    }
}

impl Instrument for Piano {
    fn set_param(&mut self, name: &str, value: f32) -> bool {
        match name {
            "tone" => self.tone = value,
            "vel_curve" => self.vel_curve = value.clamp(-1.0, 1.0),
            "dynamics" => self.dynamics = value.max(0.0),
            "release" => self.release = value.max(1.0) / 100.0,
            "width" => self.width = value.clamp(-100.0, 100.0) / 100.0,
            "tune" => self.tune = value,
            "gain" => self.gain = db_to_gain(value + HEADROOM_DB),
            _ => return false,
        }
        true
    }

    fn note_on(&mut self, pitch: u8, velocity: u8) {
        let curved = self.curved(velocity);
        let layer = (curved * 127.0).round().clamp(1.0, 127.0) as u8;
        let shape = self.shape(pitch, curved);
        self.voices.note_on(pitch, layer, &shape);
    }

    fn note_off(&mut self, pitch: u8) {
        // Release samples play as hard as the note started; the engine
        // scales them by the note's own gain.
        let shape = self.shape(pitch, 1.0);
        self.voices.note_off(pitch, &shape);
    }

    fn all_notes_off(&mut self) {
        self.voices.release_all();
    }

    fn reset(&mut self) {
        self.voices.reset();
        self.tilt = [0.0; 2];
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let low = db_to_gain(-0.5 * self.tone);
        let high = db_to_gain(0.5 * self.tone);
        let n = left.len().min(right.len());
        let mut done = 0;
        while done < n {
            let len = (n - done).min(MAX_BLOCK);
            let [sl, sr] = &mut self.scratch;
            let (sl, sr) = (&mut sl[..len], &mut sr[..len]);
            sl.fill(0.0);
            sr.fill(0.0);
            self.voices.render(sl, sr);
            let outs = [&mut left[done..done + len], &mut right[done..done + len]];
            for ((out, src), z) in outs.into_iter().zip([&*sl, &*sr]).zip(&mut self.tilt) {
                for (o, s) in out.iter_mut().zip(src) {
                    *z += self.tilt_coeff * (s - *z);
                    *o += (*z * low + (s - *z) * high) * self.gain;
                }
            }
            done += len;
        }
    }

    fn adopt(&mut self, previous: &mut dyn Instrument) -> bool {
        let Some(prev) = previous.as_any_mut().downcast_mut::<Self>() else {
            return false;
        };
        if !self.voices.adopt(&mut prev.voices) {
            return false;
        }
        self.tilt = prev.tilt;
        true
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instruments::sampled::tests::library;

    fn render(p: &mut Piano, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        p.render(&mut l, &mut r);
        (l, r)
    }

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    #[test]
    fn velocity_picks_the_layer_and_the_level() {
        let lib = library();
        let loudness = |vel: u8, params: &[(&str, f32)]| {
            let mut p = Piano::new(48_000.0, lib.clone());
            for (k, v) in params {
                assert!(p.set_param(k, *v));
            }
            p.note_on(60, vel);
            let zone = p.voices_zone();
            (peak(&render(&mut p, 4_800).0), zone)
        };
        let (soft, soft_zone) = loudness(40, &[]);
        let (loud, loud_zone) = loudness(120, &[]);
        assert_eq!((soft_zone, loud_zone), (Some(0), Some(1)));
        assert!(loud > soft * 4.0, "{soft} -> {loud}");
        // Without dynamics every velocity plays at full level.
        let (flat, _) = loudness(40, &[("dynamics", 0.0)]);
        let (top, _) = loudness(127, &[("dynamics", 0.0)]);
        assert!((flat - top).abs() < 0.02, "{flat} {top}");
        // A lighter curve reaches the loud layer sooner.
        let (_, zone) = loudness(50, &[("vel_curve", 1.0)]);
        assert_eq!(zone, Some(1));
    }

    #[test]
    fn mono_libraries_spread_by_key() {
        // Sample 0 of the test library is mono: the soft layer.
        let lib = library();
        let side = |key: u8, width: f32| {
            let mut p = Piano::new(48_000.0, lib.clone());
            p.set_param("width", width);
            p.note_on(key, 30);
            let (l, r) = render(&mut p, 2_400);
            peak(&l) / peak(&r).max(1e-9)
        };
        assert!(side(48, 100.0) > 1.2, "low keys sit left");
        assert!(side(72, 100.0) < 0.95, "high keys sit right");
        assert!(side(48, -100.0) < 0.85, "from the audience it flips");
        assert!((side(48, 0.0) - 1.0).abs() < 0.01);
    }

    #[test]
    fn tone_tilts_the_spectrum() {
        let lib = library();
        let level = |key: u8, tone: f32| {
            let mut p = Piano::new(48_000.0, lib.clone());
            p.set_param("tone", tone);
            p.note_on(key, 120);
            peak(&render(&mut p, 4_800).0[2_400..])
        };
        // How much louder 880 Hz plays than 220 Hz.
        let brightness = |tone: f32| level(72, tone) / level(48, tone);
        let flat = brightness(0.0);
        assert!((flat - 1.0).abs() < 0.05, "{flat}");
        assert!(brightness(12.0) > flat * 1.5);
        assert!(brightness(-12.0) < flat * 0.8);
    }

    impl Piano {
        fn voices_zone(&self) -> Option<u32> {
            self.voices.newest_zone()
        }
    }
}
