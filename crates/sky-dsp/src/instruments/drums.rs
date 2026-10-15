// SPDX-License-Identifier: GPL-3.0-or-later
//! A synthesized drum machine in the style of classic analog boxes.
//!
//! Every sound is generated from oscillators and noise, so the kit ships no
//! samples and carries no sample licenses. Notes follow General MIDI drums.
//!
//! Each piece has its own tune, decay and level on top of the kit-wide
//! controls, and the instrument reads them when a note starts, so a
//! pattern's parameter controls can retune a single hit. The industrial
//! style adds metal to the snare and drives every piece.

use std::f32::consts::TAU;

use crate::filters::{Svf, SvfCoeffs};
use crate::instruments::{Instrument, instrument_state};
use crate::params::ParamSpec;
use crate::util::{Rng, db_to_gain, decay_coeff};

pub(crate) const DRUM_PARAMS: &[ParamSpec] = &[
    ParamSpec::choice("style", "Style", &["808", "909", "industrial"], 0.0),
    ParamSpec::linear("tune", "Tune", -12.0, 12.0, 0.0, "st"),
    ParamSpec::log("decay", "Decay", 0.25, 4.0, 1.0, "x"),
    ParamSpec::linear("tone", "Tone", 0.0, 1.0, 0.5, ""),
    ParamSpec::linear("gain", "Gain", -60.0, 12.0, -3.0, "dB"),
    ParamSpec::linear("kick_tune", "Kick tune", -24.0, 24.0, 0.0, "st"),
    ParamSpec::log("kick_decay", "Kick decay", 0.1, 8.0, 1.0, "x"),
    ParamSpec::linear("kick_sweep", "Kick pitch sweep", 0.0, 3.0, 1.0, "x"),
    ParamSpec::linear("kick_click", "Kick click", 0.0, 1.0, 0.0, ""),
    ParamSpec::linear("kick_drive", "Kick drive", 0.0, 36.0, 0.0, "dB"),
    ParamSpec::linear("kick_level", "Kick level", -60.0, 12.0, 0.0, "dB"),
    ParamSpec::linear("snare_tune", "Snare tune", -24.0, 24.0, 0.0, "st"),
    ParamSpec::log("snare_decay", "Snare decay", 0.1, 8.0, 1.0, "x"),
    ParamSpec::linear("snare_snap", "Snare snap", 0.0, 2.0, 1.0, "x"),
    ParamSpec::linear("snare_level", "Snare level", -60.0, 12.0, 0.0, "dB"),
    ParamSpec::log("clap_decay", "Clap decay", 0.1, 8.0, 1.0, "x"),
    ParamSpec::linear("clap_level", "Clap level", -60.0, 12.0, 0.0, "dB"),
    ParamSpec::linear("hat_tune", "Hat tune", -24.0, 24.0, 0.0, "st"),
    ParamSpec::log("hat_decay", "Closed hat decay", 0.1, 8.0, 1.0, "x"),
    ParamSpec::log("open_decay", "Open hat decay", 0.1, 8.0, 1.0, "x"),
    ParamSpec::linear("hat_level", "Hat level", -60.0, 12.0, 0.0, "dB"),
    ParamSpec::linear("tom_tune", "Tom tune", -24.0, 24.0, 0.0, "st"),
    ParamSpec::log("tom_decay", "Tom decay", 0.1, 8.0, 1.0, "x"),
    ParamSpec::linear("tom_level", "Tom level", -60.0, 12.0, 0.0, "dB"),
    ParamSpec::linear("cymbal_tune", "Cymbal tune", -24.0, 24.0, 0.0, "st"),
    ParamSpec::log("cymbal_decay", "Cymbal decay", 0.1, 8.0, 1.0, "x"),
    ParamSpec::linear("cymbal_level", "Cymbal level", -60.0, 12.0, 0.0, "dB"),
    ParamSpec::linear("rim_level", "Rim level", -60.0, 12.0, 0.0, "dB"),
    ParamSpec::linear("cowbell_level", "Cowbell level", -60.0, 12.0, 0.0, "dB"),
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
    /// Saturation gain; 1 is clean.
    drive: f32,
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
        if self.drive > 1.0 {
            // Same peak, more weight: tanh keeps a full-scale hit at full scale.
            out = (out * self.drive).tanh() / self.drive.tanh();
        }
        out * self.level
    }
}

/// A piece's own tune, decay and level.
#[derive(Clone, Copy, Debug)]
struct PieceParams {
    /// Semitones added to the kit's tune.
    tune: f32,
    decay: f32,
    level: f32,
}

impl Default for PieceParams {
    fn default() -> Self {
        Self {
            tune: 0.0,
            decay: 1.0,
            level: 1.0,
        }
    }
}

/// The groups of pieces that share controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    Kick,
    Snare,
    Clap,
    Hat,
    Tom,
    Cymbal,
    Rim,
    Cowbell,
}

const GROUPS: usize = 8;

fn group_of(piece: Piece) -> Group {
    match piece {
        Piece::Kick => Group::Kick,
        Piece::Snare => Group::Snare,
        Piece::Clap => Group::Clap,
        Piece::ClosedHat | Piece::OpenHat => Group::Hat,
        Piece::LowTom | Piece::MidTom | Piece::HighTom => Group::Tom,
        Piece::Crash | Piece::Ride => Group::Cymbal,
        Piece::Rim => Group::Rim,
        Piece::Cowbell => Group::Cowbell,
    }
}

/// The group a per-piece parameter belongs to, and the control after the
/// prefix: `kick_tune` is the kick's `tune`.
fn split_param(name: &str) -> Option<(Group, &str)> {
    let (prefix, control) = name.split_once('_')?;
    let group = match prefix {
        "kick" => Group::Kick,
        "snare" => Group::Snare,
        "clap" => Group::Clap,
        "hat" => Group::Hat,
        "tom" => Group::Tom,
        "cymbal" => Group::Cymbal,
        "rim" => Group::Rim,
        "cowbell" => Group::Cowbell,
        _ => return None,
    };
    Some((group, control))
}

#[derive(Clone, Copy, Debug)]
struct Params {
    style: u8,
    tune: f32,
    decay: f32,
    tone: f32,
    gain: f32,
    pieces: [PieceParams; GROUPS],
    kick_sweep: f32,
    kick_click: f32,
    kick_drive: f32,
    snare_snap: f32,
    open_decay: f32,
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
                pieces: [PieceParams::default(); GROUPS],
                kick_sweep: 1.0,
                kick_click: 0.0,
                kick_drive: 1.0,
                snare_snap: 1.0,
                open_decay: 1.0,
            },
            voices: [Voice::default(); PIECES],
            rng: Rng::new(0x5EED),
        }
    }

    fn configure(&self, piece: Piece, velocity: f32) -> Voice {
        let p = self.params;
        let sr = self.sample_rate;
        let is909 = p.style == 1;
        let industrial = p.style == 2;
        let own = p.pieces[group_of(piece) as usize];
        let tune = 2f32.powf((p.tune + own.tune) / 12.0);
        let open_decay = if piece == Piece::OpenHat {
            p.open_decay
        } else {
            1.0
        };
        let decay = p.decay * own.decay * open_decay;
        let d = |ms: f32| decay_coeff(ms * decay, sr);
        let bright = 0.5 + p.tone;
        let mut v = Voice {
            active: true,
            level: velocity * own.level,
            tone_env: 1.0,
            noise_env: 1.0,
            sweep: 1.0,
            sweep_coeff: 1.0,
            tone_coeff: d(200.0),
            noise_coeff: d(100.0),
            metal_mul: 1.0,
            metal_count: 6,
            burst_gap: 1,
            drive: 1.0,
            ..Voice::default()
        };
        match piece {
            Piece::Kick => {
                let (start, end, decay, sweep_ms) = if industrial {
                    (300.0, 45.0, 450.0, 30.0)
                } else if is909 {
                    (230.0, 52.0, 300.0, 25.0)
                } else {
                    (160.0, 47.0, 550.0, 40.0)
                };
                v.freq_end = end * tune;
                v.freq_sweep = (start - end) * tune * p.kick_sweep;
                v.sweep_coeff = decay_coeff(sweep_ms, sr);
                v.tone_coeff = d(decay);
                v.tone_level = 1.0;
                let body_noise = if industrial {
                    0.4 * bright
                } else if is909 {
                    0.25 * bright
                } else {
                    0.0
                };
                v.noise_level = body_noise + 0.8 * p.kick_click;
                v.noise_coeff = decay_coeff(4.0, sr);
                v.noise_coeffs = Some(SvfCoeffs::new(3_000.0, 0.1, sr));
                v.drive = p.kick_drive * if industrial { 4.0 } else { 1.0 };
            }
            Piece::Snare => {
                v.freq_end = if industrial {
                    200.0
                } else if is909 {
                    190.0
                } else {
                    175.0
                } * tune;
                v.freq_sweep = 60.0 * tune;
                v.sweep_coeff = decay_coeff(15.0, sr);
                v.tone_coeff = d(if industrial { 160.0 } else { 110.0 });
                v.tone_level = if is909 || industrial { 0.45 } else { 0.6 };
                v.noise_coeff = d(if industrial {
                    250.0
                } else if is909 {
                    220.0
                } else {
                    170.0
                });
                v.noise_level = if industrial {
                    0.8
                } else if is909 {
                    0.7
                } else {
                    0.5
                } * p.snare_snap;
                if industrial {
                    // Metal partials ring with the body: a struck plate.
                    v.metal_level = 0.25;
                    v.metal_mul = 1.9 * tune;
                    v.metal_coeffs = Some(SvfCoeffs::new(3_500.0 * bright.min(1.2), 0.4, sr));
                    v.noise_band = true;
                    v.noise_coeffs = Some(SvfCoeffs::new(2_500.0 * bright, 0.3, sr));
                    v.drive = 2.0;
                } else {
                    v.noise_coeffs = Some(SvfCoeffs::new(1_800.0 * bright, 0.2, sr));
                }
            }
            Piece::Rim => {
                v.freq_end = if industrial { 1_200.0 } else { 1_700.0 } * tune;
                v.tone_coeff = d(25.0);
                v.tone_level = 0.6;
                v.noise_coeff = d(8.0);
                v.noise_level = if industrial { 0.6 } else { 0.4 };
                v.noise_band = true;
                v.noise_coeffs = Some(SvfCoeffs::new(2_500.0, 0.6, sr));
            }
            Piece::Clap => {
                v.tone_env = 0.0;
                v.noise_coeff = d(if industrial {
                    220.0
                } else if is909 {
                    180.0
                } else {
                    140.0
                });
                v.noise_level = 0.9;
                v.noise_band = true;
                let center = if industrial { 1_500.0 } else { 1_100.0 };
                v.noise_coeffs = Some(SvfCoeffs::new(center * bright, 0.45, sr));
                v.bursts = if industrial { 4 } else { 3 };
                v.burst_gap = (sr * if industrial { 0.014 } else { 0.011 }) as u32;
                if industrial {
                    v.drive = 2.0;
                }
            }
            Piece::ClosedHat | Piece::OpenHat => {
                let open = piece == Piece::OpenHat;
                v.tone_coeff = d(if open { 380.0 } else { 55.0 });
                v.noise_coeff = v.tone_coeff;
                v.metal_level = if industrial {
                    0.45
                } else if is909 {
                    0.25
                } else {
                    0.5
                };
                v.metal_mul = tune
                    * if industrial {
                        1.6
                    } else if is909 {
                        1.3
                    } else {
                        1.0
                    };
                let top = if industrial { 10_000.0 } else { 9_000.0 };
                v.metal_coeffs = Some(SvfCoeffs::new(top * bright.min(1.2), 0.3, sr));
                v.noise_level = if industrial {
                    0.5
                } else if is909 {
                    0.35
                } else {
                    0.08
                };
                v.noise_coeffs = Some(SvfCoeffs::new(7_000.0, 0.1, sr));
                v.pan = 0.2;
                if industrial {
                    v.drive = 1.5;
                }
            }
            Piece::LowTom | Piece::MidTom | Piece::HighTom => {
                let (freq, pan) = match piece {
                    Piece::LowTom => (85.0, -0.3),
                    Piece::MidTom => (125.0, 0.0),
                    _ => (170.0, 0.3),
                };
                let freq = freq * if industrial { 0.85 } else { 1.0 };
                v.freq_end = freq * tune;
                v.freq_sweep = freq * if industrial { 1.0 } else { 0.5 } * tune;
                v.sweep_coeff = decay_coeff(60.0, sr);
                v.tone_coeff = d(if is909 { 280.0 } else { 380.0 });
                v.tone_level = 0.9;
                v.noise_level = if industrial { 0.15 } else { 0.05 };
                v.noise_coeff = d(30.0);
                v.pan = pan;
                if industrial {
                    v.drive = 2.0;
                }
            }
            Piece::Crash | Piece::Ride => {
                let crash = piece == Piece::Crash;
                v.tone_coeff = d(if crash { 1_400.0 } else { 2_200.0 });
                v.noise_coeff = d(if crash { 900.0 } else { 300.0 });
                v.metal_level = 0.45;
                v.metal_mul = tune
                    * match (crash, industrial) {
                        (true, false) => 1.7,
                        (true, true) => 2.1,
                        (false, false) => 2.3,
                        (false, true) => 2.6,
                    };
                v.metal_coeffs = Some(SvfCoeffs::new(7_000.0 * bright.min(1.3), 0.2, sr));
                v.noise_level = match (crash, industrial) {
                    (true, false) => 0.25,
                    (true, true) => 0.35,
                    (false, false) => 0.08,
                    (false, true) => 0.15,
                };
                v.noise_coeffs = Some(SvfCoeffs::new(5_000.0, 0.1, sr));
                v.pan = if crash { -0.25 } else { 0.25 };
            }
            Piece::Cowbell => {
                v.tone_coeff = d(260.0);
                v.metal_level = 0.6;
                v.metal_count = if industrial { 3 } else { 2 };
                v.metal_mul = tune * if industrial { 3.1 } else { 2.6 };
                v.metal_coeffs = Some(SvfCoeffs::new(2_600.0, 0.5, sr));
                v.noise_env = 0.0;
                if industrial {
                    v.drive = 2.0;
                }
            }
        }
        v
    }
}

impl Instrument for DrumKit {
    fn set_param(&mut self, name: &str, value: f32) -> bool {
        let p = &mut self.params;
        match name {
            "style" => p.style = value.round().clamp(0.0, 2.0) as u8,
            "tune" => p.tune = value,
            "decay" => p.decay = value.max(0.05),
            "tone" => p.tone = value.clamp(0.0, 1.0),
            "gain" => p.gain = db_to_gain(value),
            "kick_sweep" => p.kick_sweep = value.clamp(0.0, 3.0),
            "kick_click" => p.kick_click = value.clamp(0.0, 1.0),
            "kick_drive" => p.kick_drive = db_to_gain(value.clamp(0.0, 36.0)),
            "snare_snap" => p.snare_snap = value.clamp(0.0, 2.0),
            "open_decay" => p.open_decay = value.clamp(0.1, 8.0),
            _ => {
                let Some((group, control)) = split_param(name) else {
                    return false;
                };
                let own = &mut p.pieces[group as usize];
                match (group, control) {
                    (Group::Rim | Group::Cowbell, "tune" | "decay") => return false,
                    (Group::Clap, "tune") => return false,
                    (_, "tune") => own.tune = value.clamp(-24.0, 24.0),
                    (_, "decay") => own.decay = value.clamp(0.1, 8.0),
                    // The bottom of the range mutes the piece.
                    (_, "level") if value <= -60.0 => own.level = 0.0,
                    (_, "level") => own.level = db_to_gain(value),
                    _ => return false,
                }
            }
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
