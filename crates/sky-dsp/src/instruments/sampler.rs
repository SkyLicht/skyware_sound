// SPDX-License-Identifier: GPL-3.0-or-later
//! A simple keyboard sampler.

use std::sync::Arc;

use crate::AudioData;
use crate::instruments::{Adsr, Instrument, instrument_state};
use crate::params::ParamSpec;
use crate::util::db_to_gain;

pub(crate) const SAMPLER_PARAMS: &[ParamSpec] = &[
    ParamSpec::linear("root", "Root note", 0.0, 127.0, 60.0, ""),
    ParamSpec::log("attack", "Attack", 0.5, 5_000.0, 1.0, "ms"),
    ParamSpec::log("release", "Release", 1.0, 10_000.0, 200.0, "ms"),
    ParamSpec::toggle("one_shot", "One-shot", true),
    ParamSpec::linear("gain", "Gain", -60.0, 12.0, 0.0, "dB"),
    ParamSpec::linear("start", "Start", 0.0, 1.0, 0.0, ""),
    ParamSpec::linear("end", "End", 0.0, 1.0, 1.0, ""),
];

const VOICES: usize = 16;

#[derive(Clone, Copy, Debug, Default)]
struct Voice {
    note: u8,
    pos: f64,
    rate: f64,
    velocity: f32,
    age: u64,
    env: Adsr,
}

pub struct Sampler {
    sample_rate: f32,
    sample: Option<Arc<AudioData>>,
    root: u8,
    attack: f32,
    release: f32,
    one_shot: bool,
    gain: f32,
    /// Where notes start and stop in the sample, as shares of its length.
    start: f32,
    end: f32,
    voices: [Voice; VOICES],
    clock: u64,
}

impl Sampler {
    pub fn new(sample_rate: f32, sample: Option<Arc<AudioData>>) -> Self {
        Self {
            sample_rate,
            sample,
            root: 60,
            attack: 1.0,
            release: 200.0,
            one_shot: true,
            gain: 1.0,
            start: 0.0,
            end: 1.0,
            voices: [Voice::default(); VOICES],
            clock: 0,
        }
    }
}

impl Instrument for Sampler {
    fn set_param(&mut self, name: &str, value: f32) -> bool {
        match name {
            "root" => self.root = value.round().clamp(0.0, 127.0) as u8,
            "attack" => self.attack = value,
            "release" => self.release = value,
            "one_shot" => self.one_shot = value >= 0.5,
            "gain" => self.gain = db_to_gain(value),
            "start" => self.start = value.clamp(0.0, 1.0),
            "end" => self.end = value.clamp(0.0, 1.0),
            _ => return false,
        }
        true
    }

    fn note_on(&mut self, pitch: u8, velocity: u8) {
        let Some(sample) = &self.sample else {
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
        let semis = pitch as f64 - self.root as f64;
        let rate = 2f64.powf(semis / 12.0) * sample.sample_rate as f64 / self.sample_rate as f64;
        let start = (self.start as f64 * sample.frames() as f64).floor();
        let v = &mut self.voices[i];
        v.note = pitch;
        v.pos = start;
        v.rate = rate;
        v.velocity = velocity as f32 / 127.0;
        v.age = self.clock;
        v.env
            .set(self.attack, 1.0, 1.0, self.release, self.sample_rate);
        v.env.trigger();
    }

    fn note_off(&mut self, pitch: u8) {
        if self.one_shot {
            return;
        }
        for v in &mut self.voices {
            if v.note == pitch {
                v.env.release();
            }
        }
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
        let Some(sample) = &self.sample else {
            return;
        };
        let (src_l, src_r) = (sample.left(), sample.right());
        // Notes stop at the end marker, or at the start one if they cross.
        let end = (self.end.max(self.start) as f64 * src_l.len() as f64).ceil() as usize;
        let len = end.min(src_l.len());
        if len < 2 {
            return;
        }
        for v in self.voices.iter_mut().filter(|v| v.env.is_active()) {
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

    instrument_state!(voices, clock);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sample whose value is its frame's place in it, 0 to 1.
    fn ramp(frames: usize) -> Arc<AudioData> {
        let mono: Vec<f32> = (0..frames).map(|i| i as f32 / frames as f32).collect();
        Arc::new(AudioData::from_interleaved(&mono, 1, 48_000))
    }

    /// What a note at the root plays, frame by frame.
    fn play(start: f32, end: f32) -> Vec<f32> {
        let mut s = Sampler::new(48_000.0, Some(ramp(1_000)));
        s.set_param("attack", 0.5);
        s.set_param("start", start);
        s.set_param("end", end);
        s.note_on(60, 127);
        let mut l = vec![0.0; 1_200];
        let mut r = vec![0.0; 1_200];
        s.render(&mut l, &mut r);
        l
    }

    #[test]
    fn notes_play_between_the_markers() {
        let whole = play(0.0, 1.0);
        let sounding = whole.iter().filter(|s| **s != 0.0).count();
        assert!(sounding > 990, "{sounding}");
        let part = play(0.5, 0.75);
        let sounding: Vec<f32> = part.iter().copied().filter(|s| *s != 0.0).collect();
        assert!((240..=250).contains(&sounding.len()), "{}", sounding.len());
        // It starts half way in, once the short attack is through.
        let first = sounding[30];
        assert!((0.52..0.54).contains(&first), "{first}");
        assert!(sounding.iter().all(|s| *s < 0.76));
        // Crossed markers play nothing.
        assert!(play(0.8, 0.2).iter().all(|s| *s == 0.0));
    }
}
