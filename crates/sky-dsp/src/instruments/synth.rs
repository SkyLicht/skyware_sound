// SPDX-License-Identifier: GPL-3.0-or-later
//! A polyphonic subtractive synth.

use std::f32::consts::TAU;

use crate::filters::{Svf, SvfCoeffs};
use crate::instruments::{Adsr, Instrument, instrument_state};
use crate::params::ParamSpec;
use crate::util::{db_to_gain, midi_to_hz};

pub(crate) const SYNTH_PARAMS: &[ParamSpec] = &[
    ParamSpec::choice("wave", "Wave", &["saw", "square", "triangle", "sine"], 0.0),
    ParamSpec::linear("octave", "Octave", -3.0, 3.0, 0.0, "oct"),
    ParamSpec::linear("detune", "Detune", 0.0, 50.0, 7.0, "cents"),
    ParamSpec::log("cutoff", "Cutoff", 20.0, 20_000.0, 2_000.0, "Hz"),
    ParamSpec::linear("resonance", "Resonance", 0.0, 1.0, 0.2, ""),
    ParamSpec::linear("env_amount", "Filter env", 0.0, 8.0, 2.0, "oct"),
    ParamSpec::log("filter_decay", "Filter decay", 1.0, 5_000.0, 300.0, "ms"),
    ParamSpec::log("attack", "Attack", 0.5, 5_000.0, 5.0, "ms"),
    ParamSpec::log("decay", "Decay", 1.0, 5_000.0, 200.0, "ms"),
    ParamSpec::linear("sustain", "Sustain", 0.0, 1.0, 0.7, ""),
    ParamSpec::log("release", "Release", 1.0, 10_000.0, 300.0, "ms"),
    ParamSpec::linear("gain", "Gain", -60.0, 12.0, -9.0, "dB"),
];

const VOICES: usize = 16;
const CONTROL_RATE: usize = 16;

#[derive(Clone, Copy, Debug)]
struct Params {
    wave: u8,
    octave: f32,
    detune: f32,
    cutoff: f32,
    resonance: f32,
    env_amount: f32,
    filter_decay: f32,
    attack: f32,
    decay: f32,
    sustain: f32,
    release: f32,
    gain: f32,
}

#[derive(Clone, Copy, Debug, Default)]
struct Voice {
    note: u8,
    velocity: f32,
    age: u64,
    held: bool,
    phase: [f32; 2],
    amp: Adsr,
    fenv: Adsr,
    filter: Svf,
}

#[inline]
fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let t = t / dt;
        t + t - t * t - 1.0
    } else if t > 1.0 - dt {
        let t = (t - 1.0) / dt;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

#[inline]
pub(crate) fn oscillator(wave: u8, phase: f32, dt: f32) -> f32 {
    match wave {
        1 => {
            let naive = if phase < 0.5 { 1.0 } else { -1.0 };
            naive + poly_blep(phase, dt) - poly_blep((phase + 0.5) % 1.0, dt)
        }
        2 => 4.0 * (phase - 0.5).abs() - 1.0,
        3 => (phase * TAU).sin(),
        _ => 2.0 * phase - 1.0 - poly_blep(phase, dt),
    }
}

pub struct Synth {
    sample_rate: f32,
    params: Params,
    voices: [Voice; VOICES],
    clock: u64,
}

impl Synth {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            params: Params {
                wave: 0,
                octave: 0.0,
                detune: 7.0,
                cutoff: 2_000.0,
                resonance: 0.2,
                env_amount: 2.0,
                filter_decay: 300.0,
                attack: 5.0,
                decay: 200.0,
                sustain: 0.7,
                release: 300.0,
                gain: db_to_gain(-9.0),
            },
            voices: [Voice::default(); VOICES],
            clock: 0,
        }
    }

    fn pick_voice(&self, pitch: u8) -> usize {
        if let Some(i) = self
            .voices
            .iter()
            .position(|v| v.amp.is_active() && v.note == pitch)
        {
            return i;
        }
        if let Some(i) = self.voices.iter().position(|v| !v.amp.is_active()) {
            return i;
        }
        // Steal the quietest released voice, else the oldest one.
        let released = self
            .voices
            .iter()
            .enumerate()
            .filter(|(_, v)| v.amp.is_released())
            .min_by(|a, b| a.1.amp.level().total_cmp(&b.1.amp.level()))
            .map(|(i, _)| i);
        released.unwrap_or_else(|| {
            self.voices
                .iter()
                .enumerate()
                .min_by_key(|(_, v)| v.age)
                .map_or(0, |(i, _)| i)
        })
    }
}

impl Instrument for Synth {
    fn set_param(&mut self, name: &str, value: f32) -> bool {
        let p = &mut self.params;
        match name {
            "wave" => p.wave = value.round().clamp(0.0, 3.0) as u8,
            "octave" => p.octave = value.round(),
            "detune" => p.detune = value,
            "cutoff" => p.cutoff = value,
            "resonance" => p.resonance = value,
            "env_amount" => p.env_amount = value,
            "filter_decay" => p.filter_decay = value,
            "attack" => p.attack = value,
            "decay" => p.decay = value,
            "sustain" => p.sustain = value,
            "release" => p.release = value,
            "gain" => p.gain = db_to_gain(value),
            _ => return false,
        }
        true
    }

    fn note_on(&mut self, pitch: u8, velocity: u8) {
        let i = self.pick_voice(pitch);
        self.clock += 1;
        let p = self.params;
        let sr = self.sample_rate;
        let v = &mut self.voices[i];
        if !v.amp.is_active() {
            v.phase = [0.0, 0.37];
            v.filter.reset();
        }
        v.note = pitch;
        v.velocity = (velocity as f32 / 127.0).powf(1.5);
        v.age = self.clock;
        v.held = true;
        v.amp.set(p.attack, p.decay, p.sustain, p.release, sr);
        v.fenv.set(0.5, p.filter_decay, 0.0, p.filter_decay, sr);
        v.amp.trigger();
        v.fenv.trigger();
    }

    fn note_off(&mut self, pitch: u8) {
        for v in &mut self.voices {
            if v.held && v.note == pitch {
                v.held = false;
                v.amp.release();
                v.fenv.release();
            }
        }
    }

    fn all_notes_off(&mut self) {
        for v in &mut self.voices {
            v.held = false;
            v.amp.release();
            v.fenv.release();
        }
    }

    fn reset(&mut self) {
        for v in &mut self.voices {
            *v = Voice::default();
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let p = self.params;
        let sr = self.sample_rate;
        let detune_ratio = 2f32.powf(p.detune / 1200.0 / 2.0);
        for v in self.voices.iter_mut().filter(|v| v.amp.is_active()) {
            let base = midi_to_hz(v.note as f32 + p.octave * 12.0);
            let dts = [
                (base * detune_ratio / sr).min(0.49),
                (base / detune_ratio / sr).min(0.49),
            ];
            for (lc, rc) in left
                .chunks_mut(CONTROL_RATE)
                .zip(right.chunks_mut(CONTROL_RATE))
            {
                let cutoff = (p.cutoff * 2f32.powf(p.env_amount * v.fenv.level())).min(sr * 0.45);
                let coeffs = SvfCoeffs::new(cutoff, p.resonance, sr);
                for (l, r) in lc.iter_mut().zip(rc.iter_mut()) {
                    let mut osc = 0.0;
                    for (phase, dt) in v.phase.iter_mut().zip(dts) {
                        osc += oscillator(p.wave, *phase, dt);
                        *phase += dt;
                        if *phase >= 1.0 {
                            *phase -= 1.0;
                        }
                    }
                    let filtered = v.filter.tick(&coeffs, osc * 0.5).low;
                    v.fenv.tick();
                    let out = filtered * v.amp.tick() * v.velocity * p.gain;
                    *l += out;
                    *r += out;
                }
                if !v.amp.is_active() {
                    break;
                }
            }
        }
    }

    instrument_state!(voices, clock);
}
