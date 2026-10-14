// SPDX-License-Identifier: GPL-3.0-or-later
//! Compressor and limiter.

use crate::effects::{Effect, effect_state};
use crate::params::ParamSpec;
use crate::util::{db_to_gain, gain_to_db};

fn time_coeff(ms: f32, sample_rate: f32) -> f32 {
    let samples = (ms * 0.001 * sample_rate).max(1.0);
    (-1.0 / samples).exp()
}

pub(crate) const COMPRESSOR_PARAMS: &[ParamSpec] = &[
    ParamSpec::linear("threshold", "Threshold", -60.0, 0.0, -18.0, "dB"),
    ParamSpec::log("ratio", "Ratio", 1.0, 20.0, 4.0, ":1"),
    ParamSpec::log("attack", "Attack", 0.1, 200.0, 10.0, "ms"),
    ParamSpec::log("release", "Release", 5.0, 2_000.0, 120.0, "ms"),
    ParamSpec::linear("knee", "Knee", 0.0, 12.0, 6.0, "dB"),
    ParamSpec::linear("makeup", "Makeup", 0.0, 24.0, 0.0, "dB"),
];

/// A stereo-linked, feed-forward compressor working in the log domain.
pub struct Compressor {
    sample_rate: f32,
    threshold: f32,
    ratio: f32,
    attack: f32,
    release: f32,
    knee: f32,
    makeup: f32,
    /// Current gain reduction in dB (positive numbers reduce).
    reduction_db: f32,
}

impl Compressor {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            threshold: -18.0,
            ratio: 4.0,
            attack: time_coeff(10.0, sample_rate),
            release: time_coeff(120.0, sample_rate),
            knee: 6.0,
            makeup: 1.0,
            reduction_db: 0.0,
        }
    }

    /// Static curve: how many dB to reduce a signal at `level_db`.
    fn target_reduction(&self, level_db: f32) -> f32 {
        let over = level_db - self.threshold;
        let slope = 1.0 - 1.0 / self.ratio;
        if self.knee > 0.0 && over.abs() <= self.knee * 0.5 {
            let x = over + self.knee * 0.5;
            slope * x * x / (2.0 * self.knee)
        } else if over > 0.0 {
            slope * over
        } else {
            0.0
        }
    }

    /// Current gain reduction in dB, for meters.
    pub fn reduction_db(&self) -> f32 {
        self.reduction_db
    }
}

impl Effect for Compressor {
    fn set_param(&mut self, name: &str, value: f32) -> bool {
        match name {
            "threshold" => self.threshold = value,
            "ratio" => self.ratio = value.max(1.0),
            "attack" => self.attack = time_coeff(value, self.sample_rate),
            "release" => self.release = time_coeff(value, self.sample_rate),
            "knee" => self.knee = value,
            "makeup" => self.makeup = db_to_gain(value),
            _ => return false,
        }
        true
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let level = gain_to_db(l.abs().max(r.abs()));
            let target = self.target_reduction(level);
            let coeff = if target > self.reduction_db {
                self.attack
            } else {
                self.release
            };
            self.reduction_db = target + (self.reduction_db - target) * coeff;
            let g = db_to_gain(-self.reduction_db) * self.makeup;
            *l *= g;
            *r *= g;
        }
    }

    fn reset(&mut self) {
        self.reduction_db = 0.0;
    }

    effect_state!(reduction_db);
}

pub(crate) const LIMITER_PARAMS: &[ParamSpec] = &[
    ParamSpec::linear("ceiling", "Ceiling", -24.0, 0.0, -1.0, "dB"),
    ParamSpec::log("release", "Release", 1.0, 1_000.0, 60.0, "ms"),
];

/// A peak limiter with instant attack: the output never exceeds the ceiling.
pub struct Limiter {
    sample_rate: f32,
    ceiling: f32,
    release: f32,
    gain: f32,
}

impl Limiter {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            ceiling: db_to_gain(-1.0),
            release: time_coeff(60.0, sample_rate),
            gain: 1.0,
        }
    }
}

impl Effect for Limiter {
    fn set_param(&mut self, name: &str, value: f32) -> bool {
        match name {
            "ceiling" => self.ceiling = db_to_gain(value),
            "release" => self.release = time_coeff(value, self.sample_rate),
            _ => return false,
        }
        true
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let peak = l.abs().max(r.abs());
            let needed = if peak > self.ceiling {
                self.ceiling / peak
            } else {
                1.0
            };
            // Recover towards unity, but never more than the sample allows.
            self.gain = (1.0 + (self.gain - 1.0) * self.release).min(needed);
            *l *= self.gain;
            *r *= self.gain;
        }
    }

    fn reset(&mut self) {
        self.gain = 1.0;
    }

    effect_state!(gain);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_holds_the_ceiling() {
        let mut lim = Limiter::new(48_000.0);
        lim.set_param("ceiling", -6.0);
        let ceiling = db_to_gain(-6.0);
        let mut l: Vec<f32> = (0..512).map(|i| ((i as f32) * 0.1).sin() * 2.0).collect();
        let mut r = l.clone();
        lim.process(&mut l, &mut r);
        assert!(l.iter().all(|s| s.abs() <= ceiling + 1e-6));
    }

    #[test]
    fn compressor_reduces_loud_signals_only() {
        let mut c = Compressor::new(48_000.0);
        c.set_param("threshold", -20.0);
        c.set_param("ratio", 10.0);
        c.set_param("knee", 0.0);
        let mut quiet = vec![0.01; 4_800];
        let mut quiet_r = quiet.clone();
        c.process(&mut quiet, &mut quiet_r);
        assert!((quiet[4_799] - 0.01).abs() < 1e-4);
        let mut loud = vec![1.0; 48_000];
        let mut loud_r = loud.clone();
        c.process(&mut loud, &mut loud_r);
        // 0 dB in, -20 threshold, 10:1 → about -18 dB out.
        let out_db = gain_to_db(loud[47_999]);
        assert!((out_db + 18.0).abs() < 0.5, "{out_db}");
    }
}
