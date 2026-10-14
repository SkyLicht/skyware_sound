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
        let v = &mut self.voices[i];
        v.note = pitch;
        v.pos = 0.0;
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
        let len = src_l.len();
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
