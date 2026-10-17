// SPDX-License-Identifier: GPL-3.0-or-later
//! Gain, filter, EQ and distortion.

use crate::effects::{Effect, effect_state};
use crate::filters::{Biquad, BiquadCoeffs, Svf, SvfCoeffs};
use crate::params::ParamSpec;
use crate::util::{Smoother, db_to_gain};

pub(crate) const GAIN_PARAMS: &[ParamSpec] = &[
    ParamSpec::linear("gain", "Gain", -60.0, 24.0, 0.0, "dB"),
    ParamSpec::linear("width", "Width", 0.0, 2.0, 1.0, ""),
];

pub struct Gain {
    gain: f32,
    width: f32,
    smooth: Smoother,
}

impl Gain {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            gain: 1.0,
            width: 1.0,
            smooth: Smoother::new(1.0, 10.0, sample_rate),
        }
    }
}

impl Effect for Gain {
    fn set_param(&mut self, name: &str, value: f32) -> bool {
        match name {
            "gain" => self.gain = db_to_gain(value),
            "width" => self.width = value,
            _ => return false,
        }
        true
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let g = self.smooth.next(self.gain);
            let mid = (*l + *r) * 0.5;
            let side = (*l - *r) * 0.5 * self.width;
            *l = (mid + side) * g;
            *r = (mid - side) * g;
        }
    }

    fn reset(&mut self) {
        self.smooth.set(self.gain);
    }

    effect_state!(smooth);
}

pub(crate) const FILTER_PARAMS: &[ParamSpec] = &[
    ParamSpec::choice("mode", "Mode", &["lowpass", "highpass", "bandpass"], 0.0),
    ParamSpec::log("cutoff", "Cutoff", 20.0, 20_000.0, 1_000.0, "Hz"),
    ParamSpec::linear("resonance", "Resonance", 0.0, 1.0, 0.2, ""),
];

pub struct Filter {
    sample_rate: f32,
    mode: u8,
    cutoff: f32,
    resonance: f32,
    state: [Svf; 2],
    cutoff_smooth: Smoother,
}

impl Filter {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            mode: 0,
            cutoff: 1_000.0,
            resonance: 0.2,
            state: [Svf::default(); 2],
            cutoff_smooth: Smoother::new(1_000.0, 20.0, sample_rate / 16.0),
        }
    }
}

impl Effect for Filter {
    fn set_param(&mut self, name: &str, value: f32) -> bool {
        match name {
            "mode" => self.mode = value.round().clamp(0.0, 2.0) as u8,
            "cutoff" => self.cutoff = value,
            "resonance" => self.resonance = value,
            _ => return false,
        }
        true
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (lc, rc) in left.chunks_mut(16).zip(right.chunks_mut(16)) {
            let cutoff = self.cutoff_smooth.next(self.cutoff);
            let c = SvfCoeffs::new(cutoff, self.resonance, self.sample_rate);
            for (l, r) in lc.iter_mut().zip(rc.iter_mut()) {
                let ol = self.state[0].tick(&c, *l);
                let or = self.state[1].tick(&c, *r);
                (*l, *r) = match self.mode {
                    1 => (ol.high, or.high),
                    2 => (ol.band, or.band),
                    _ => (ol.low, or.low),
                };
            }
        }
    }

    fn reset(&mut self) {
        self.state = [Svf::default(); 2];
        self.cutoff_smooth.set(self.cutoff);
    }

    effect_state!(state, cutoff_smooth);
}

pub(crate) const EQ_PARAMS: &[ParamSpec] = &[
    ParamSpec::log("low_freq", "Low freq", 20.0, 1_000.0, 120.0, "Hz"),
    ParamSpec::linear("low_gain", "Low gain", -18.0, 18.0, 0.0, "dB"),
    ParamSpec::log("mid_freq", "Mid freq", 100.0, 10_000.0, 1_000.0, "Hz"),
    ParamSpec::linear("mid_gain", "Mid gain", -18.0, 18.0, 0.0, "dB"),
    ParamSpec::log("mid_q", "Mid Q", 0.1, 10.0, 0.7, ""),
    ParamSpec::log("high_freq", "High freq", 1_000.0, 20_000.0, 8_000.0, "Hz"),
    ParamSpec::linear("high_gain", "High gain", -18.0, 18.0, 0.0, "dB"),
];

pub struct Eq {
    sample_rate: f32,
    values: [f32; 7],
    coeffs: [BiquadCoeffs; 3],
    state: [[Biquad; 3]; 2],
}

impl Eq {
    pub fn new(sample_rate: f32) -> Self {
        let mut eq = Self {
            sample_rate,
            values: [120.0, 0.0, 1_000.0, 0.0, 0.7, 8_000.0, 0.0],
            coeffs: [BiquadCoeffs::IDENTITY; 3],
            state: [[Biquad::default(); 3]; 2],
        };
        eq.update();
        eq
    }

    fn update(&mut self) {
        self.coeffs = eq_coeffs(&self.values, self.sample_rate);
    }
}

/// The EQ's low shelf, mid peak and high shelf for `values` in the order
/// of its parameters.
pub fn eq_coeffs(values: &[f32; 7], sample_rate: f32) -> [BiquadCoeffs; 3] {
    let [lf, lg, mf, mg, mq, hf, hg] = *values;
    [
        BiquadCoeffs::low_shelf(lf, lg, sample_rate),
        BiquadCoeffs::peaking(mf, mq, mg, sample_rate),
        BiquadCoeffs::high_shelf(hf, hg, sample_rate),
    ]
}

impl Effect for Eq {
    fn set_param(&mut self, name: &str, value: f32) -> bool {
        let Some(i) = EQ_PARAMS.iter().position(|p| p.name == name) else {
            return false;
        };
        self.values[i] = value;
        self.update();
        true
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (ch, buf) in [left, right].into_iter().enumerate() {
            let state = &mut self.state[ch];
            for s in buf.iter_mut() {
                let mut x = *s;
                for (band, c) in state.iter_mut().zip(&self.coeffs) {
                    x = band.tick(c, x);
                }
                *s = x;
            }
        }
    }

    fn reset(&mut self) {
        self.state = [[Biquad::default(); 3]; 2];
    }

    effect_state!(state);
}

pub(crate) const DISTORTION_PARAMS: &[ParamSpec] = &[
    ParamSpec::choice("mode", "Mode", &["soft", "hard", "fold"], 0.0),
    ParamSpec::linear("drive", "Drive", 0.0, 48.0, 12.0, "dB"),
    ParamSpec::linear("mix", "Mix", 0.0, 1.0, 1.0, ""),
    ParamSpec::linear("output", "Output", -24.0, 12.0, 0.0, "dB"),
];

pub struct Distortion {
    mode: u8,
    drive: f32,
    mix: f32,
    output: f32,
}

impl Distortion {
    pub fn new(_sample_rate: f32) -> Self {
        Self {
            mode: 0,
            drive: db_to_gain(12.0),
            mix: 1.0,
            output: 1.0,
        }
    }

    #[inline]
    fn shape(&self, x: f32) -> f32 {
        crate::curves::distortion_shape(self.mode, self.drive, x)
    }
}

impl Effect for Distortion {
    fn set_param(&mut self, name: &str, value: f32) -> bool {
        match name {
            "mode" => self.mode = value.round().clamp(0.0, 2.0) as u8,
            "drive" => self.drive = db_to_gain(value),
            "mix" => self.mix = value,
            "output" => self.output = db_to_gain(value),
            _ => return false,
        }
        true
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for s in left.iter_mut().chain(right.iter_mut()) {
            let wet = self.shape(*s);
            *s = (*s * (1.0 - self.mix) + wet * self.mix) * self.output;
        }
    }

    fn reset(&mut self) {}

    effect_state!();
}
