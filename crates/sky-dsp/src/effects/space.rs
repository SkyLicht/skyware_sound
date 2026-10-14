// SPDX-License-Identifier: GPL-3.0-or-later
//! Delay and reverb.

use crate::effects::{Effect, effect_state};
use crate::params::ParamSpec;
use crate::util::Smoother;

pub(crate) const DELAY_PARAMS: &[ParamSpec] = &[
    ParamSpec::log("time", "Time", 1.0, 2_000.0, 375.0, "ms"),
    ParamSpec::linear("feedback", "Feedback", 0.0, 0.95, 0.35, ""),
    ParamSpec::linear("mix", "Mix", 0.0, 1.0, 0.3, ""),
    ParamSpec::log("damping", "Damping", 500.0, 20_000.0, 8_000.0, "Hz"),
    ParamSpec::toggle("ping_pong", "Ping-pong", false),
];

const MAX_DELAY_MS: f32 = 2_000.0;

/// A stereo delay line with a one-pole low-pass in the feedback path.
pub struct Delay {
    sample_rate: f32,
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
                self.time_samples =
                    (value.clamp(1.0, MAX_DELAY_MS) * 0.001 * self.sample_rate).max(1.0)
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
];

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
pub struct Reverb {
    feedback: f32,
    damp: f32,
    width: f32,
    mix: f32,
    channels: [ReverbChannel; 2],
}

impl Reverb {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            feedback: 0.6 * 0.28 + 0.7,
            damp: 0.5 * 0.4,
            width: 1.0,
            mix: 0.25,
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
            _ => return false,
        }
        true
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let wet = self.mix * 3.0;
        let wet1 = wet * (self.width * 0.5 + 0.5);
        let wet2 = wet * ((1.0 - self.width) * 0.5);
        let [cl, cr] = &mut self.channels;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let input = (*l + *r) * 0.015;
            let ol = cl.tick(input, self.feedback, self.damp);
            let or = cr.tick(input, self.feedback, self.damp);
            let dry = 1.0 - self.mix;
            *l = *l * dry + ol * wet1 + or * wet2;
            *r = *r * dry + or * wet1 + ol * wet2;
        }
    }

    fn reset(&mut self) {
        for c in &mut self.channels {
            c.clear();
        }
    }

    effect_state!(channels);
}
