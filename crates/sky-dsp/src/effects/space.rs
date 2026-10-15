// SPDX-License-Identifier: GPL-3.0-or-later
//! Delay and reverb.

use crate::effects::{Effect, effect_state};
use crate::params::ParamSpec;
use crate::util::Smoother;

/// Note values a synced delay can take: `t` is a triplet, `d` dotted, so
/// `1/8d` is the techno-favourite 3/16.
const SYNC_OPTIONS: &[&str] = &[
    "off", "1/32", "1/16t", "1/16", "1/16d", "1/8t", "1/8", "1/8d", "1/4t", "1/4", "1/4d", "1/2",
    "1/2d", "1/1",
];
/// The length of each [`SYNC_OPTIONS`] entry in quarter-note beats.
const SYNC_BEATS: [f32; 14] = [
    0.0,
    0.125,
    1.0 / 6.0,
    0.25,
    0.375,
    1.0 / 3.0,
    0.5,
    0.75,
    2.0 / 3.0,
    1.0,
    1.5,
    2.0,
    3.0,
    4.0,
];

pub(crate) const DELAY_PARAMS: &[ParamSpec] = &[
    ParamSpec::log("time", "Time", 1.0, 2_000.0, 375.0, "ms"),
    ParamSpec::choice("sync", "Sync", SYNC_OPTIONS, 0.0),
    ParamSpec::linear("feedback", "Feedback", 0.0, 0.95, 0.35, ""),
    ParamSpec::linear("mix", "Mix", 0.0, 1.0, 0.3, ""),
    ParamSpec::log("damping", "Damping", 500.0, 20_000.0, 8_000.0, "Hz"),
    ParamSpec::toggle("ping_pong", "Ping-pong", false),
];

/// The delay line's length: a synced whole note at 60 BPM. Longer synced
/// times are cut to this.
const MAX_DELAY_MS: f32 = 4_000.0;

/// A stereo delay line with a one-pole low-pass in the feedback path.
pub struct Delay {
    sample_rate: f32,
    time_ms: f32,
    /// Index into [`SYNC_BEATS`]; 0 is off, where `time_ms` sets the time.
    sync: usize,
    bpm: f64,
    time_samples: f32,
    feedback: f32,
    mix: f32,
    damp_coeff: f32,
    ping_pong: bool,
    buffers: [Vec<f32>; 2],
    write: usize,
    lp: [f32; 2],
    time_smooth: Smoother,
}

impl Delay {
    pub fn new(sample_rate: f32) -> Self {
        let len = (MAX_DELAY_MS * 0.001 * sample_rate) as usize + 4;
        let time = 375.0 * 0.001 * sample_rate;
        Self {
            sample_rate,
            time_ms: 375.0,
            sync: 0,
            bpm: 120.0,
            time_samples: time,
            feedback: 0.35,
            mix: 0.3,
            damp_coeff: 0.0,
            ping_pong: false,
            buffers: [vec![0.0; len], vec![0.0; len]],
            write: 0,
            lp: [0.0; 2],
            time_smooth: Smoother::new(time, 50.0, sample_rate),
        }
    }

    fn update_time(&mut self) {
        let ms = match SYNC_BEATS.get(self.sync) {
            Some(&beats) if beats > 0.0 => beats * 60_000.0 / self.bpm.max(1.0) as f32,
            _ => self.time_ms,
        };
        self.time_samples = (ms.clamp(1.0, MAX_DELAY_MS) * 0.001 * self.sample_rate).max(1.0);
    }

    #[inline]
    fn read(buf: &[f32], write: usize, delay: f32) -> f32 {
        let len = buf.len();
        let pos = write as f32 - delay;
        let pos = if pos < 0.0 { pos + len as f32 } else { pos };
        let i = pos as usize % len;
        let frac = pos - pos.floor();
        let a = buf[i];
        let b = buf[(i + 1) % len];
        a + (b - a) * frac
    }
}

impl Effect for Delay {
    fn set_param(&mut self, name: &str, value: f32) -> bool {
        match name {
            "time" => {
                self.time_ms = value.clamp(1.0, MAX_DELAY_MS);
                self.update_time();
            }
            "sync" => {
                self.sync = (value.round().max(0.0) as usize).min(SYNC_BEATS.len() - 1);
                self.update_time();
            }
            "feedback" => self.feedback = value.clamp(0.0, 0.95),
            "mix" => self.mix = value.clamp(0.0, 1.0),
            "damping" => {
                let w = 2.0 * std::f32::consts::PI * value / self.sample_rate;
                self.damp_coeff = (-w).exp();
            }
            "ping_pong" => self.ping_pong = value >= 0.5,
            _ => return false,
        }
        true
    }

    fn set_tempo(&mut self, bpm: f64) {
        self.bpm = bpm;
        self.update_time();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let len = self.buffers[0].len();
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let delay = self.time_smooth.next(self.time_samples);
            let dl = Self::read(&self.buffers[0], self.write, delay);
            let dr = Self::read(&self.buffers[1], self.write, delay);
            self.lp[0] = dl + (self.lp[0] - dl) * self.damp_coeff;
            self.lp[1] = dr + (self.lp[1] - dr) * self.damp_coeff;
            let (in_l, in_r) = if self.ping_pong {
                (
                    (*l + *r) * 0.5 + self.lp[1] * self.feedback,
                    self.lp[0] * self.feedback,
                )
            } else {
                (
                    *l + self.lp[0] * self.feedback,
                    *r + self.lp[1] * self.feedback,
                )
            };
            self.buffers[0][self.write] = in_l;
            self.buffers[1][self.write] = in_r;
            self.write = (self.write + 1) % len;
            *l = *l * (1.0 - self.mix) + dl * self.mix;
            *r = *r * (1.0 - self.mix) + dr * self.mix;
        }
    }

    fn reset(&mut self) {
        for b in &mut self.buffers {
            b.fill(0.0);
        }
        self.lp = [0.0; 2];
        self.time_smooth.set(self.time_samples);
    }

    effect_state!(buffers, write, lp, time_smooth);
}

pub(crate) const REVERB_PARAMS: &[ParamSpec] = &[
    ParamSpec::linear("size", "Size", 0.0, 1.0, 0.6, ""),
    ParamSpec::linear("damping", "Damping", 0.0, 1.0, 0.5, ""),
    ParamSpec::linear("width", "Width", 0.0, 1.0, 1.0, ""),
    ParamSpec::linear("mix", "Mix", 0.0, 1.0, 0.25, ""),
    ParamSpec::linear("predelay", "Pre-delay", 0.0, 250.0, 0.0, "ms"),
    ParamSpec::linear("gate", "Gate", 0.0, 1_000.0, 0.0, "ms"),
    ParamSpec::linear("gate_threshold", "Gate threshold", -60.0, 0.0, -30.0, "dB"),
];

const MAX_PREDELAY_MS: f32 = 250.0;

// Delay lengths at 44.1 kHz for the classic parallel-comb, series-allpass
// room design; the right channel is offset by a small spread.
const COMB_TUNING: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASS_TUNING: [usize; 4] = [556, 441, 341, 225];
const STEREO_SPREAD: usize = 23;

#[derive(Default)]
struct Comb {
    buf: Vec<f32>,
    idx: usize,
    store: f32,
}

impl Comb {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(1)],
            idx: 0,
            store: 0.0,
        }
    }

    #[inline]
    fn tick(&mut self, input: f32, feedback: f32, damp: f32) -> f32 {
        let out = self.buf[self.idx];
        self.store = out * (1.0 - damp) + self.store * damp;
        self.buf[self.idx] = input + self.store * feedback;
        self.idx = (self.idx + 1) % self.buf.len();
        out
    }
}

#[derive(Default)]
struct Allpass {
    buf: Vec<f32>,
    idx: usize,
}

impl Allpass {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(1)],
            idx: 0,
        }
    }

    #[inline]
    fn tick(&mut self, input: f32) -> f32 {
        let buffered = self.buf[self.idx];
        let out = buffered - input;
        self.buf[self.idx] = input + buffered * 0.5;
        self.idx = (self.idx + 1) % self.buf.len();
        out
    }
}

struct ReverbChannel {
    combs: Vec<Comb>,
    allpasses: Vec<Allpass>,
}

impl ReverbChannel {
    fn new(sample_rate: f32, spread: usize) -> Self {
        let scale = sample_rate / 44_100.0;
        Self {
            combs: COMB_TUNING
                .iter()
                .map(|t| Comb::new(((t + spread) as f32 * scale) as usize))
                .collect(),
            allpasses: ALLPASS_TUNING
                .iter()
                .map(|t| Allpass::new(((t + spread) as f32 * scale) as usize))
                .collect(),
        }
    }

    #[inline]
    fn tick(&mut self, input: f32, feedback: f32, damp: f32) -> f32 {
        let mut out = 0.0;
        for c in &mut self.combs {
            out += c.tick(input, feedback, damp);
        }
        for a in &mut self.allpasses {
            out = a.tick(out);
        }
        out
    }

    fn clear(&mut self) {
        for c in &mut self.combs {
            c.buf.fill(0.0);
            c.store = 0.0;
        }
        for a in &mut self.allpasses {
            a.buf.fill(0.0);
        }
    }
}

/// An algorithmic stereo room reverb built from parallel combs and series
/// allpasses. No impulse responses, so nothing needs licensing.
///
/// The gate gives the 1980s gated snare: the tail plays for `gate`
/// milliseconds after the input last rose above `gate_threshold`, then
/// cuts off quickly.
pub struct Reverb {
    sample_rate: f32,
    feedback: f32,
    damp: f32,
    width: f32,
    mix: f32,
    predelay: usize,
    /// Hold time in frames; 0 turns the gate off.
    gate_hold: u32,
    gate_threshold: f32,
    gate_open: f32,
    gate_attack: f32,
    gate_release: f32,
    /// Frames since the input was last above the threshold.
    since_loud: u32,
    pre: Vec<f32>,
    pre_write: usize,
    channels: [ReverbChannel; 2],
}

impl Reverb {
    pub fn new(sample_rate: f32) -> Self {
        let pre_len = (MAX_PREDELAY_MS * 0.001 * sample_rate) as usize + 1;
        let coeff = |ms: f32| 1.0 - (-1.0 / (ms * 0.001 * sample_rate).max(1.0)).exp();
        Self {
            sample_rate,
            feedback: 0.6 * 0.28 + 0.7,
            damp: 0.5 * 0.4,
            width: 1.0,
            mix: 0.25,
            predelay: 0,
            gate_hold: 0,
            gate_threshold: 0.0316,
            gate_open: 1.0,
            gate_attack: coeff(0.5),
            gate_release: coeff(8.0),
            since_loud: u32::MAX,
            pre: vec![0.0; pre_len],
            pre_write: 0,
            channels: [
                ReverbChannel::new(sample_rate, 0),
                ReverbChannel::new(sample_rate, STEREO_SPREAD),
            ],
        }
    }
}

impl Effect for Reverb {
    fn set_param(&mut self, name: &str, value: f32) -> bool {
        match name {
            "size" => self.feedback = value.clamp(0.0, 1.0) * 0.28 + 0.7,
            "damping" => self.damp = value.clamp(0.0, 1.0) * 0.4,
            "width" => self.width = value.clamp(0.0, 1.0),
            "mix" => self.mix = value.clamp(0.0, 1.0),
            "predelay" => {
                let frames = value.clamp(0.0, MAX_PREDELAY_MS) * 0.001 * self.sample_rate;
                self.predelay = (frames as usize).min(self.pre.len() - 1);
            }
            "gate" => {
                self.gate_hold = (value.clamp(0.0, 1_000.0) * 0.001 * self.sample_rate) as u32
            }
            "gate_threshold" => self.gate_threshold = crate::util::db_to_gain(value.min(0.0)),
            _ => return false,
        }
        true
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let wet = self.mix * 3.0;
        let wet1 = wet * (self.width * 0.5 + 0.5);
        let wet2 = wet * ((1.0 - self.width) * 0.5);
        let dry = 1.0 - self.mix;
        let pre_len = self.pre.len();
        let [cl, cr] = &mut self.channels;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let mono = (*l + *r) * 0.5;
            let input = if self.predelay == 0 {
                mono
            } else {
                self.pre[self.pre_write] = mono;
                let read = (self.pre_write + pre_len - self.predelay.min(pre_len - 1)) % pre_len;
                self.pre_write = (self.pre_write + 1) % pre_len;
                self.pre[read]
            } * 0.03;
            let ol = cl.tick(input, self.feedback, self.damp);
            let or = cr.tick(input, self.feedback, self.damp);
            let gate = if self.gate_hold == 0 {
                1.0
            } else {
                if mono.abs() >= self.gate_threshold {
                    self.since_loud = 0;
                } else {
                    self.since_loud = self.since_loud.saturating_add(1);
                }
                let (target, coeff) = if self.since_loud < self.gate_hold {
                    (1.0, self.gate_attack)
                } else {
                    (0.0, self.gate_release)
                };
                self.gate_open += (target - self.gate_open) * coeff;
                self.gate_open
            };
            *l = *l * dry + (ol * wet1 + or * wet2) * gate;
            *r = *r * dry + (or * wet1 + ol * wet2) * gate;
        }
    }

    fn reset(&mut self) {
        for c in &mut self.channels {
            c.clear();
        }
        self.pre.fill(0.0);
        self.pre_write = 0;
        self.since_loud = u32::MAX;
        self.gate_open = if self.gate_hold == 0 { 1.0 } else { 0.0 };
    }

    effect_state!(channels, pre, pre_write, since_loud, gate_open);
}
