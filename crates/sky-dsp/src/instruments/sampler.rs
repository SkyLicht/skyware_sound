// SPDX-License-Identifier: GPL-3.0-or-later
//! A simple keyboard sampler. Voices play as the multi-sample engine's do
//! (see [`voice`](super::voice)).

use std::sync::Arc;

use crate::AudioData;
use crate::instruments::voice::{self, Voice};
use crate::instruments::{Instrument, instrument_state};
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

/// Voices that may sound at once.
const VOICES: usize = 16;
const SLOTS: usize = voice::slots(VOICES);

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
    voices: [Voice; SLOTS],
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
            voices: [Voice::default(); SLOTS],
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
        let i = voice::free_slot(&mut self.voices, VOICES, self.sample_rate);
        self.clock += 1;
        let semis = pitch as f64 - self.root as f64;
        let rate = 2f64.powf(semis / 12.0) * sample.sample_rate as f64 / self.sample_rate as f64;
        let start = (self.start as f64 * sample.frames() as f64).floor();
        let velocity = velocity as f32 / 127.0;
        let v = &mut self.voices[i];
        *v = Voice::new(0, pitch, start, rate, velocity, self.clock);
        v.env
            .set(self.attack, 1.0, 1.0, self.release, self.sample_rate);
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
        // Notes stop at the end marker, or at the start one if they cross.
        let frames = sample.frames() as f64;
        let start = (f64::from(self.start) * frames).floor();
        let end = (f64::from(self.end) * frames).ceil().max(start) as usize;
        for v in self.voices.iter_mut().filter(|v| v.env.is_active()) {
            v.render(sample, end, self.gain, left, right);
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

    #[test]
    fn a_stolen_voice_fades_instead_of_clicking() {
        // A sample that rises from silence to a steady level over its
        // first millisecond.
        let rise: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 / 48.0).min(1.0) * 0.5)
            .collect();
        let sample = Arc::new(AudioData::from_interleaved(&rise, 1, 48_000));
        let mut s = Sampler::new(48_000.0, Some(sample));
        let render = |s: &mut Sampler, frames: usize| {
            let mut l = vec![0.0; frames];
            let mut r = vec![0.0; frames];
            s.render(&mut l, &mut r);
            l
        };
        for _ in 0..VOICES {
            s.note_on(60, 100);
        }
        let before = render(&mut s, 256);
        s.note_on(60, 100);
        let after = render(&mut s, 1_024);
        // Started again from the top, the oldest note would drop from its
        // level to silence in one step.
        let level = 0.5 * 100.0 / 127.0;
        let biggest = std::iter::once(after[0] - before[255])
            .chain(after.windows(2).map(|w| w[1] - w[0]))
            .fold(0.0f32, |m, d| m.max(d.abs()));
        assert!(biggest < level / 10.0, "a step of {biggest}");
        let sounding = s.voices.iter().filter(|v| v.env.is_active()).count();
        assert_eq!(sounding, VOICES);
    }
}
