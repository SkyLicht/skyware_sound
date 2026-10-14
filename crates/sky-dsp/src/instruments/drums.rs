// SPDX-License-Identifier: GPL-3.0-or-later
//! A synthesized drum machine in the style of classic analog boxes.
//!
//! Every sound is generated from oscillators and noise, so the kit ships no
//! samples and carries no sample licenses. Notes follow General MIDI drums.

use std::f32::consts::TAU;

use crate::filters::{Svf, SvfCoeffs};
use crate::instruments::{Instrument, instrument_state};
use crate::params::ParamSpec;
use crate::util::{Rng, db_to_gain, decay_coeff};

pub(crate) const DRUM_PARAMS: &[ParamSpec] = &[
    ParamSpec::choice("style", "Style", &["808", "909"], 0.0),
    ParamSpec::linear("tune", "Tune", -12.0, 12.0, 0.0, "st"),
    ParamSpec::log("decay", "Decay", 0.25, 4.0, 1.0, "x"),
    ParamSpec::linear("tone", "Tone", 0.0, 1.0, 0.5, ""),
    ParamSpec::linear("gain", "Gain", -60.0, 12.0, -3.0, "dB"),
];

/// Indices into the kit's voice table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Piece {
    Kick,
    Rim,
    Snare,
    Clap,
    ClosedHat,
    OpenHat,
    LowTom,
    MidTom,
    HighTom,
    Crash,
    Ride,
    Cowbell,
}

const PIECES: usize = 12;

fn piece_for_note(note: u8) -> Option<Piece> {
    Some(match note {
        35 | 36 => Piece::Kick,
        37 => Piece::Rim,
        38 | 40 => Piece::Snare,
        39 => Piece::Clap,
        42 | 44 => Piece::ClosedHat,
        46 => Piece::OpenHat,
        41 | 43 => Piece::LowTom,
        45 | 47 => Piece::MidTom,
        48 | 50 => Piece::HighTom,
        49 | 52 | 55 | 57 => Piece::Crash,
        51 | 53 | 59 => Piece::Ride,
        56 => Piece::Cowbell,
        _ => return None,
    })
}

/// Square-wave partials of the classic metallic hat and cymbal sound.
const METAL_FREQS: [f32; 6] = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];

#[derive(Clone, Copy, Debug, Default)]
struct Voice {
    active: bool,
    age: u32,
    level: f32,
    pan: f32,
    // Pitched body.
    phase: f32,
    freq_end: f32,
    freq_sweep: f32,
    sweep: f32,
    sweep_coeff: f32,
    tone_env: f32,
    tone_coeff: f32,
    tone_level: f32,
    // Noise.
    noise_env: f32,
    noise_coeff: f32,
    noise_level: f32,
    noise_filter: Svf,
    noise_coeffs: Option<SvfCoeffs>,
    noise_band: bool,
    // Metallic partials (hats, cymbals, cowbell).
    metal_phase: [f32; 6],
    metal_mul: f32,
    metal_count: u8,
    metal_level: f32,
    metal_filter: Svf,
    metal_coeffs: Option<SvfCoeffs>,
    // Clap re-triggers.
    bursts: u8,
    burst_gap: u32,
}

impl Voice {
    #[inline]
    fn tick(&mut self, sample_rate: f32, rng: &mut Rng) -> f32 {
        let mut out = 0.0;
        if self.tone_level > 0.0 {
            let freq = self.freq_end + self.freq_sweep * self.sweep;
            self.sweep *= self.sweep_coeff;
            self.phase += freq / sample_rate;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
            out += (self.phase * TAU).sin() * self.tone_env * self.tone_level;
            self.tone_env *= self.tone_coeff;
        }
        if self.noise_level > 0.0 {
            if self.bursts > 0 && self.age > 0 && self.age.is_multiple_of(self.burst_gap) {
                self.bursts -= 1;
                self.noise_env = 1.0;
            }
            let n = rng.bipolar();
            let shaped = match &self.noise_coeffs {
                Some(c) => {
                    let o = self.noise_filter.tick(c, n);
                    if self.noise_band {
                        o.band * 2.0
                    } else {
                        o.high
                    }
                }
                None => n,
            };
            out += shaped * self.noise_env * self.noise_level;
            // Claps hold each burst briefly before the final tail.
            let coeff = if self.bursts > 0 {
                self.noise_coeff.powf(4.0)
            } else {
                self.noise_coeff
            };
            self.noise_env *= coeff;
        }
        if self.metal_level > 0.0 {
            let mut metal = 0.0;
            for (i, phase) in self
                .metal_phase
                .iter_mut()
                .take(self.metal_count as usize)
                .enumerate()
            {
                *phase += METAL_FREQS[i] * self.metal_mul / sample_rate;
                if *phase >= 1.0 {
                    *phase -= 1.0;
                }
                metal += if *phase < 0.5 { 1.0 } else { -1.0 };
            }
            let shaped = match &self.metal_coeffs {
                Some(c) => self.metal_filter.tick(c, metal / 6.0).band * 3.0,
                None => metal / 6.0,
            };
            out += shaped * self.tone_env * self.metal_level;
            if self.tone_level <= 0.0 {
                self.tone_env *= self.tone_coeff;
            }
        }
        self.age = self.age.wrapping_add(1);
        let alive = self.tone_env > 1e-4 || self.noise_env > 1e-4 || self.bursts > 0;
        if !alive {
            self.active = false;
        }
        out * self.level
    }
}

#[derive(Clone, Copy, Debug)]
struct Params {
    style: u8,
    tune: f32,
    decay: f32,
    tone: f32,
    gain: f32,
}

pub struct DrumKit {
    sample_rate: f32,
    params: Params,
    voices: [Voice; PIECES],
    rng: Rng,
}

impl DrumKit {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            params: Params {
                style: 0,
                tune: 0.0,
                decay: 1.0,
                tone: 0.5,
                gain: db_to_gain(-3.0),
            },
            voices: [Voice::default(); PIECES],
            rng: Rng::new(0x5EED),
        }
    }

    fn configure(&self, piece: Piece, velocity: f32) -> Voice {
        let p = self.params;
        let sr = self.sample_rate;
        let is909 = p.style == 1;
        let tune = 2f32.powf(p.tune / 12.0);
        let d = |ms: f32| decay_coeff(ms * p.decay, sr);
        let bright = 0.5 + p.tone;
        let mut v = Voice {
            active: true,
            level: velocity,
            tone_env: 1.0,
            noise_env: 1.0,
            sweep: 1.0,
            sweep_coeff: 1.0,
            tone_coeff: d(200.0),
            noise_coeff: d(100.0),
            metal_mul: 1.0,
            metal_count: 6,
            burst_gap: 1,
            ..Voice::default()
        };
        match piece {
            Piece::Kick => {
                let (start, end, decay) = if is909 {
                    (230.0, 52.0, 300.0)
                } else {
                    (160.0, 47.0, 550.0)
                };
                v.freq_end = end * tune;
                v.freq_sweep = (start - end) * tune;
                v.sweep_coeff = decay_coeff(if is909 { 25.0 } else { 40.0 }, sr);
                v.tone_coeff = d(decay);
                v.tone_level = 1.0;
                v.noise_level = if is909 { 0.25 * bright } else { 0.0 };
                v.noise_coeff = decay_coeff(4.0, sr);
                v.noise_coeffs = Some(SvfCoeffs::new(3_000.0, 0.1, sr));
            }
            Piece::Snare => {
                v.freq_end = if is909 { 190.0 } else { 175.0 } * tune;
                v.freq_sweep = 60.0 * tune;
                v.sweep_coeff = decay_coeff(15.0, sr);
                v.tone_coeff = d(110.0);
                v.tone_level = if is909 { 0.45 } else { 0.6 };
                v.noise_coeff = d(if is909 { 220.0 } else { 170.0 });
                v.noise_level = if is909 { 0.7 } else { 0.5 };
                v.noise_coeffs = Some(SvfCoeffs::new(1_800.0 * bright, 0.2, sr));
            }
            Piece::Rim => {
                v.freq_end = 1_700.0 * tune;
                v.tone_coeff = d(25.0);
                v.tone_level = 0.6;
                v.noise_coeff = d(8.0);
                v.noise_level = 0.4;
                v.noise_band = true;
                v.noise_coeffs = Some(SvfCoeffs::new(2_500.0, 0.6, sr));
            }
            Piece::Clap => {
                v.tone_env = 0.0;
                v.noise_coeff = d(if is909 { 180.0 } else { 140.0 });
                v.noise_level = 0.9;
                v.noise_band = true;
                v.noise_coeffs = Some(SvfCoeffs::new(1_100.0 * bright, 0.45, sr));
                v.bursts = 3;
                v.burst_gap = (sr * 0.011) as u32;
            }
            Piece::ClosedHat | Piece::OpenHat => {
                let open = piece == Piece::OpenHat;
                v.tone_coeff = d(if open { 380.0 } else { 55.0 });
                v.noise_coeff = v.tone_coeff;
                v.metal_level = if is909 { 0.25 } else { 0.5 };
                v.metal_mul = tune * if is909 { 1.3 } else { 1.0 };
                v.metal_coeffs = Some(SvfCoeffs::new(9_000.0 * bright.min(1.2), 0.3, sr));
                v.noise_level = if is909 { 0.35 } else { 0.08 };
                v.noise_coeffs = Some(SvfCoeffs::new(7_000.0, 0.1, sr));
                v.pan = 0.2;
            }
            Piece::LowTom | Piece::MidTom | Piece::HighTom => {
                let (freq, pan) = match piece {
                    Piece::LowTom => (85.0, -0.3),
                    Piece::MidTom => (125.0, 0.0),
                    _ => (170.0, 0.3),
                };
                v.freq_end = freq * tune;
                v.freq_sweep = freq * 0.5 * tune;
                v.sweep_coeff = decay_coeff(60.0, sr);
                v.tone_coeff = d(if is909 { 280.0 } else { 380.0 });
                v.tone_level = 0.9;
                v.noise_level = 0.05;
                v.noise_coeff = d(30.0);
                v.pan = pan;
            }
            Piece::Crash | Piece::Ride => {
                let crash = piece == Piece::Crash;
                v.tone_coeff = d(if crash { 1_400.0 } else { 2_200.0 });
                v.noise_coeff = d(if crash { 900.0 } else { 300.0 });
                v.metal_level = 0.45;
                v.metal_mul = tune * if crash { 1.7 } else { 2.3 };
                v.metal_coeffs = Some(SvfCoeffs::new(7_000.0 * bright.min(1.3), 0.2, sr));
                v.noise_level = if crash { 0.25 } else { 0.08 };
                v.noise_coeffs = Some(SvfCoeffs::new(5_000.0, 0.1, sr));
                v.pan = if crash { -0.25 } else { 0.25 };
            }
            Piece::Cowbell => {
                v.tone_coeff = d(260.0);
                v.metal_level = 0.6;
                v.metal_count = 2;
                v.metal_mul = tune * 2.6;
                v.metal_coeffs = Some(SvfCoeffs::new(2_600.0, 0.5, sr));
                v.noise_env = 0.0;
            }
        }
        v
    }
}

impl Instrument for DrumKit {
    fn set_param(&mut self, name: &str, value: f32) -> bool {
        let p = &mut self.params;
        match name {
            "style" => p.style = value.round().clamp(0.0, 1.0) as u8,
            "tune" => p.tune = value,
            "decay" => p.decay = value.max(0.05),
            "tone" => p.tone = value.clamp(0.0, 1.0),
            "gain" => p.gain = db_to_gain(value),
            _ => return false,
        }
        true
    }

    fn note_on(&mut self, pitch: u8, velocity: u8) {
        let Some(piece) = piece_for_note(pitch) else {
            return;
        };
        if piece == Piece::ClosedHat {
            // A closed hat chokes the open one, as on the real machines.
            self.voices[Piece::OpenHat as usize].active = false;
        }
        let vel = velocity as f32 / 127.0;
        self.voices[piece as usize] = self.configure(piece, vel * vel);
    }

    fn note_off(&mut self, _pitch: u8) {}

    fn all_notes_off(&mut self) {}

    fn reset(&mut self) {
        for v in &mut self.voices {
            v.active = false;
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sample_rate;
        let gain = self.params.gain;
        for v in self.voices.iter_mut().filter(|v| v.active) {
            let (gl, gr) = crate::util::pan_gains(v.pan);
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                if !v.active {
                    break;
                }
                let s = v.tick(sr, &mut self.rng) * gain;
                *l += s * gl;
                *r += s * gr;
            }
        }
    }

    instrument_state!(voices, rng);
}
