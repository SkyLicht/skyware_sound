// SPDX-License-Identifier: GPL-3.0-or-later
//! Filters: a state-variable filter that stays stable under fast
//! modulation, and RBJ biquads for equalizers.

use std::f32::consts::PI;

/// Coefficients for [`Svf`], a topology-preserving-transform state variable filter.
#[derive(Clone, Copy, Debug)]
pub struct SvfCoeffs {
    k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
}

impl SvfCoeffs {
    /// `resonance` runs from 0 (gentle) to 1 (close to self-oscillation).
    pub fn new(cutoff_hz: f32, resonance: f32, sample_rate: f32) -> Self {
        let cutoff = cutoff_hz.clamp(10.0, sample_rate * 0.49);
        let g = (PI * cutoff / sample_rate).tan();
        let k = 2.0 - 1.95 * resonance.clamp(0.0, 1.0);
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        Self { k, a1, a2, a3 }
    }
}

/// One channel of state-variable filter state.
#[derive(Clone, Copy, Debug, Default)]
pub struct Svf {
    ic1eq: f32,
    ic2eq: f32,
}

/// The three simultaneous outputs of the state-variable filter.
#[derive(Clone, Copy, Debug)]
pub struct SvfOut {
    pub low: f32,
    pub band: f32,
    pub high: f32,
}

impl Svf {
    #[inline]
    pub fn tick(&mut self, c: &SvfCoeffs, input: f32) -> SvfOut {
        let v3 = input - self.ic2eq;
        let v1 = c.a1 * self.ic1eq + c.a2 * v3;
        let v2 = self.ic2eq + c.a2 * self.ic1eq + c.a3 * v3;
        self.ic1eq = 2.0 * v1 - self.ic1eq;
        self.ic2eq = 2.0 * v2 - self.ic2eq;
        SvfOut {
            low: v2,
            band: v1,
            high: input - c.k * v1 - v2,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Biquad coefficients, normalized so a0 = 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BiquadCoeffs {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl BiquadCoeffs {
    pub const IDENTITY: BiquadCoeffs = BiquadCoeffs {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    fn normalize(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    fn omega(freq: f32, sample_rate: f32) -> (f32, f32) {
        let w = 2.0 * PI * freq.clamp(10.0, sample_rate * 0.49) / sample_rate;
        (w.cos(), w.sin())
    }

    pub fn peaking(freq: f32, q: f32, gain_db: f32, sample_rate: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let (cos, sin) = Self::omega(freq, sample_rate);
        let alpha = sin / (2.0 * q.max(0.05));
        Self::normalize(
            1.0 + alpha * a,
            -2.0 * cos,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos,
            1.0 - alpha / a,
        )
    }

    pub fn low_shelf(freq: f32, gain_db: f32, sample_rate: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let (cos, sin) = Self::omega(freq, sample_rate);
        let alpha = sin / 2.0 * std::f32::consts::SQRT_2;
        let sq = 2.0 * a.sqrt() * alpha;
        Self::normalize(
            a * ((a + 1.0) - (a - 1.0) * cos + sq),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
            a * ((a + 1.0) - (a - 1.0) * cos - sq),
            (a + 1.0) + (a - 1.0) * cos + sq,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos),
            (a + 1.0) + (a - 1.0) * cos - sq,
        )
    }

    pub fn high_shelf(freq: f32, gain_db: f32, sample_rate: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let (cos, sin) = Self::omega(freq, sample_rate);
        let alpha = sin / 2.0 * std::f32::consts::SQRT_2;
        let sq = 2.0 * a.sqrt() * alpha;
        Self::normalize(
            a * ((a + 1.0) + (a - 1.0) * cos + sq),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
            a * ((a + 1.0) + (a - 1.0) * cos - sq),
            (a + 1.0) - (a - 1.0) * cos + sq,
            2.0 * ((a - 1.0) - (a + 1.0) * cos),
            (a + 1.0) - (a - 1.0) * cos - sq,
        )
    }

    pub fn lowpass(freq: f32, q: f32, sample_rate: f32) -> Self {
        let (cos, sin) = Self::omega(freq, sample_rate);
        let alpha = sin / (2.0 * q.max(0.05));
        Self::normalize(
            (1.0 - cos) / 2.0,
            1.0 - cos,
            (1.0 - cos) / 2.0,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        )
    }

    pub fn highpass(freq: f32, q: f32, sample_rate: f32) -> Self {
        let (cos, sin) = Self::omega(freq, sample_rate);
        let alpha = sin / (2.0 * q.max(0.05));
        Self::normalize(
            (1.0 + cos) / 2.0,
            -(1.0 + cos),
            (1.0 + cos) / 2.0,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        )
    }
}

/// One channel of biquad state (transposed direct form II).
#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad {
    z1: f32,
    z2: f32,
}

impl Biquad {
    #[inline]
    pub fn tick(&mut self, c: &BiquadCoeffs, x: f32) -> f32 {
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        y
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms_after_filter(freq: f32, mut f: impl FnMut(f32) -> f32) -> f32 {
        let sr = 48_000.0;
        let n = 48_000;
        let mut sum = 0.0;
        for i in 0..n {
            let x = (2.0 * PI * freq * i as f32 / sr).sin();
            let y = f(x);
            if i > n / 2 {
                sum += y * y;
            }
        }
        (sum / (n / 2) as f32).sqrt()
    }

    #[test]
    fn svf_lowpass_attenuates_highs() {
        let c = SvfCoeffs::new(500.0, 0.0, 48_000.0);
        let low = {
            let mut s = Svf::default();
            rms_after_filter(100.0, |x| s.tick(&c, x).low)
        };
        let mut s = Svf::default();
        let high = rms_after_filter(8_000.0, |x| s.tick(&c, x).low);
        assert!(low > 0.6, "low {low}");
        assert!(high < 0.02, "high {high}");
    }

    #[test]
    fn peaking_boost_matches_gain_at_centre() {
        let c = BiquadCoeffs::peaking(1_000.0, 1.0, 6.0, 48_000.0);
        let mut b = Biquad::default();
        let out = rms_after_filter(1_000.0, |x| b.tick(&c, x));
        let expected = std::f32::consts::FRAC_1_SQRT_2 * 10f32.powf(6.0 / 20.0);
        assert!((out - expected).abs() < 0.02, "{out} vs {expected}");
    }
}
