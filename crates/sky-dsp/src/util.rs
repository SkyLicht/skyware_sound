// SPDX-License-Identifier: GPL-3.0-or-later
//! Small helpers shared by the processors.

/// Decibels to linear gain. Anything at or below -120 dB is silence.
#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    if db <= -120.0 {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// Linear gain to decibels, floored at -120 dB.
#[inline]
pub fn gain_to_db(gain: f32) -> f32 {
    if gain <= 1e-6 {
        -120.0
    } else {
        20.0 * gain.log10()
    }
}

/// Balance-style stereo pan: centre is unity on both sides, hard left
/// silences the right channel and vice versa.
#[inline]
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let pan = pan.clamp(-1.0, 1.0);
    ((1.0 - pan).min(1.0), (1.0 + pan).min(1.0))
}

/// A one-pole parameter smoother, to avoid zipper noise when a value jumps.
#[derive(Clone, Copy, Debug)]
pub struct Smoother {
    current: f32,
    coeff: f32,
}

impl Smoother {
    pub fn new(value: f32, time_ms: f32, sample_rate: f32) -> Self {
        let samples = (time_ms * 0.001 * sample_rate).max(1.0);
        Self {
            current: value,
            coeff: 1.0 - (-1.0 / samples).exp(),
        }
    }

    #[inline]
    pub fn next(&mut self, target: f32) -> f32 {
        self.current += (target - self.current) * self.coeff;
        // f32 steps stall just short of the target, so snap once close.
        if (target - self.current).abs() <= 1e-5 * target.abs().max(1.0) {
            self.current = target;
        }
        self.current
    }

    pub fn current(&self) -> f32 {
        self.current
    }

    pub fn set(&mut self, value: f32) {
        self.current = value;
    }
}

/// A fast deterministic noise source (xorshift64*).
#[derive(Clone, Copy, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `[-1, 1)`.
    #[inline]
    pub fn bipolar(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 23) as f32 - 1.0
    }
}

/// Exponential decay coefficient that falls to -60 dB after `time_ms`.
#[inline]
pub fn decay_coeff(time_ms: f32, sample_rate: f32) -> f32 {
    let samples = (time_ms * 0.001 * sample_rate).max(1.0);
    (-6.907_755 / samples).exp()
}

/// MIDI note number to frequency in Hz.
#[inline]
pub fn midi_to_hz(note: f32) -> f32 {
    440.0 * 2f32.powf((note - 69.0) / 12.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_round_trip() {
        for db in [-60.0, -6.0, 0.0, 6.0] {
            assert!((gain_to_db(db_to_gain(db)) - db).abs() < 1e-4);
        }
        assert_eq!(db_to_gain(-200.0), 0.0);
    }

    #[test]
    fn pan_is_unity_in_the_centre() {
        assert_eq!(pan_gains(0.0), (1.0, 1.0));
        assert_eq!(pan_gains(-1.0), (1.0, 0.0));
        assert_eq!(pan_gains(1.0), (0.0, 1.0));
    }

    #[test]
    fn smoother_converges() {
        let mut s = Smoother::new(0.0, 5.0, 48_000.0);
        for _ in 0..5_000 {
            s.next(1.0);
        }
        assert_eq!(s.current(), 1.0);
    }

    #[test]
    fn noise_is_bipolar_and_deterministic() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        for _ in 0..1000 {
            let x = a.bipolar();
            assert!((-1.0..1.0).contains(&x));
            assert_eq!(x, b.bipolar());
        }
    }
}
