// SPDX-License-Identifier: GPL-3.0-or-later
//! The shapes behind the effects and envelopes as plain functions, so an
//! editor draws exactly what the processors do. The processors call the
//! same functions where they can.

use std::f32::consts::PI;

/// How far below the start an exponential stage has fallen when its time
/// is up: -60 dB, as the envelopes count their decay and release times.
pub const SIXTY_DB: f32 = 6.907_755;

/// The state-variable filter's response at `freq`, in dB. `mode` is 0 for
/// low-pass, 1 for high-pass and 2 for band-pass, as the filter effect
/// numbers them.
pub fn svf_magnitude_db(
    mode: u8,
    cutoff_hz: f32,
    resonance: f32,
    freq: f32,
    sample_rate: f32,
) -> f32 {
    // The filter is the bilinear transform of an analog prototype with its
    // cutoff prewarped, so the analog response at the warped frequency is
    // exact.
    let cutoff = cutoff_hz.clamp(10.0, sample_rate * 0.49);
    let g = f64::from((PI * cutoff / sample_rate).tan());
    let k = f64::from(2.0 - 1.95 * resonance.clamp(0.0, 1.0));
    let w = f64::from((PI * freq.clamp(1.0, sample_rate * 0.499) / sample_rate).tan()) / g;
    let denom = ((1.0 - w * w).powi(2) + (k * w).powi(2)).sqrt();
    let num = match mode {
        1 => w * w,
        2 => w,
        _ => 1.0,
    };
    (20.0 * (num / denom).max(1e-9).log10()) as f32
}

/// How many dB the compressor turns down a signal at `level_db`, before
/// its attack and release smooth it.
pub fn compressor_reduction_db(level_db: f32, threshold_db: f32, ratio: f32, knee_db: f32) -> f32 {
    let over = level_db - threshold_db;
    let slope = 1.0 - 1.0 / ratio.max(1.0);
    if knee_db > 0.0 && over.abs() <= knee_db * 0.5 {
        let x = over + knee_db * 0.5;
        slope * x * x / (2.0 * knee_db)
    } else if over > 0.0 {
        slope * over
    } else {
        0.0
    }
}

/// The distortion's curve: what it makes of `x` with `drive` as a gain.
/// `mode` is 0 for soft, 1 for hard and 2 for the wavefolder.
#[inline]
pub fn distortion_shape(mode: u8, drive: f32, x: f32) -> f32 {
    let x = x * drive;
    match mode {
        1 => x.clamp(-1.0, 1.0),
        2 => {
            // A triangle wavefolder: reflects the signal back inside [-1, 1].
            let t = (x + 1.0).rem_euclid(4.0);
            if t < 2.0 { t - 1.0 } else { 3.0 - t }
        }
        _ => x.tanh(),
    }
}

/// The exponent of the duck's recovery for its `curve` parameter: above 1
/// stays down longer, below 1 comes back sooner.
pub fn duck_power(curve: f32) -> f32 {
    4f32.powf(curve.clamp(-1.0, 1.0))
}

/// The duck's gain `elapsed` after a note started, when the gain was
/// `from`. `attack`, `length` and `elapsed` share any unit.
#[inline]
pub fn duck_gain(from: f32, depth: f32, power: f32, attack: f32, length: f32, elapsed: f32) -> f32 {
    if elapsed >= length {
        return 1.0;
    }
    let trough = 1.0 - depth;
    if elapsed < attack {
        return from + (trough - from) * (elapsed / attack);
    }
    let x = (elapsed - attack) / (length - attack).max(f32::MIN_POSITIVE);
    trough + (1.0 - trough) * x.powf(power)
}

/// The level of an ADSR envelope `t_ms` after its note started, when the
/// note is let go at `held_ms`: a linear attack, then a decay towards the
/// sustain and a release that each fall by 60 dB in their time.
pub fn adsr_level(
    attack_ms: f32,
    decay_ms: f32,
    sustain: f32,
    release_ms: f32,
    held_ms: f32,
    t_ms: f32,
) -> f32 {
    let held = |t: f32| -> f32 {
        let attack = attack_ms.max(0.0);
        if t < attack {
            return t / attack.max(f32::MIN_POSITIVE);
        }
        let s = sustain.clamp(0.0, 1.0);
        s + (1.0 - s) * (-SIXTY_DB * (t - attack) / decay_ms.max(1e-3)).exp()
    };
    if t_ms <= 0.0 {
        return 0.0;
    }
    if t_ms < held_ms {
        return held(t_ms);
    }
    let from = held(held_ms.max(0.0));
    from * (-SIXTY_DB * (t_ms - held_ms) / release_ms.max(1e-3)).exp()
}

/// An exponential decay's level `t_ms` in, falling 60 dB in `decay_ms`, as
/// the filter envelopes of the synth and the acid bass do.
pub fn decay_level(decay_ms: f32, t_ms: f32) -> f32 {
    if t_ms <= 0.0 {
        return 1.0;
    }
    (-SIXTY_DB * t_ms / decay_ms.max(1e-3)).exp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::EffectType;
    use crate::filters::{Svf, SvfCoeffs};
    use crate::instruments::Adsr;
    use crate::util::gain_to_db;

    /// The level a sine at `freq` comes out at, in dB, once settled: its
    /// RMS over the second half second, against the input's.
    fn measured_db(freq: f32, mut f: impl FnMut(f32) -> f32) -> f32 {
        let sr = 48_000.0;
        let mut sum = 0.0f64;
        for i in 0..48_000 {
            let y = f((2.0 * PI * freq * i as f32 / sr).sin());
            if i >= 24_000 {
                sum += f64::from(y) * f64::from(y);
            }
        }
        gain_to_db(((sum / 24_000.0) * 2.0).sqrt() as f32)
    }

    #[test]
    fn the_filter_curve_matches_the_filter() {
        let sr = 48_000.0;
        for (mode, cutoff, res) in [(0u8, 1_000.0, 0.2), (1, 500.0, 0.8), (2, 2_000.0, 0.5)] {
            let c = SvfCoeffs::new(cutoff, res, sr);
            for freq in [100.0, 700.0, 1_000.0, 2_000.0, 6_000.0] {
                let mut s = Svf::default();
                let got = measured_db(freq, |x| {
                    let o = s.tick(&c, x);
                    match mode {
                        1 => o.high,
                        2 => o.band,
                        _ => o.low,
                    }
                });
                let want = svf_magnitude_db(mode, cutoff, res, freq, sr);
                assert!((got - want).abs() < 0.2, "{mode} {freq}: {got} vs {want}");
            }
        }
    }

    #[test]
    fn the_eq_curve_matches_the_eq() {
        let sr = 48_000.0;
        let values = [200.0, 6.0, 1_500.0, -9.0, 2.0, 6_000.0, 4.0];
        let mut eq = EffectType::Eq.create(sr);
        for (spec, v) in EffectType::Eq.params().iter().zip(values) {
            eq.set_param(spec.name, v);
        }
        let coeffs = crate::effects::eq_coeffs(&values, sr);
        for freq in [50.0, 200.0, 1_500.0, 6_000.0, 15_000.0] {
            let got = measured_db(freq, |x| {
                let (mut l, mut r) = ([x], [x]);
                eq.process(&mut l, &mut r);
                l[0]
            });
            let want: f32 = coeffs.iter().map(|c| c.magnitude_db(freq, sr)).sum();
            assert!((got - want).abs() < 0.2, "{freq}: {got} vs {want}");
        }
        // A shelf is half way at its frequency.
        let low = coeffs[0].magnitude_db(200.0, sr);
        assert!((low - 3.0).abs() < 0.3, "{low}");
    }

    #[test]
    fn the_envelope_curve_matches_the_envelope() {
        let sr = 48_000.0;
        let (a, d, s, r) = (10.0, 100.0, 0.5, 200.0);
        let mut env = Adsr::default();
        env.set(a, d, s, r, sr);
        env.trigger();
        let held = 300.0;
        for i in 0..(sr as usize) {
            let t = i as f32 * 1_000.0 / sr;
            if (t - held).abs() < 0.5 / sr * 1_000.0 {
                env.release();
            }
            let got = env.tick();
            let want = adsr_level(a, d, s, r, held, t + 1_000.0 / sr);
            assert!((got - want).abs() < 0.01, "{t} ms: {got} vs {want}");
        }
    }

    #[test]
    fn the_compressor_curve_bends_at_the_threshold() {
        assert_eq!(compressor_reduction_db(-30.0, -20.0, 4.0, 0.0), 0.0);
        assert_eq!(compressor_reduction_db(-10.0, -20.0, 4.0, 0.0), 7.5);
        // Within the knee the curve eases in.
        let soft = compressor_reduction_db(-20.0, -20.0, 4.0, 6.0);
        assert!(soft > 0.0 && soft < 1.0, "{soft}");
    }

    #[test]
    fn the_duck_dips_and_comes_back() {
        assert_eq!(duck_gain(1.0, 0.75, 2.0, 0.0, 10.0, 0.0), 0.25);
        assert_eq!(duck_gain(1.0, 0.75, 2.0, 0.0, 10.0, 10.0), 1.0);
        let half = duck_gain(1.0, 0.75, 2.0, 0.0, 10.0, 5.0);
        assert!((half - (0.25 + 0.75 * 0.25)).abs() < 1e-6);
        assert_eq!(duck_gain(1.0, 1.0, 1.0, 2.0, 10.0, 1.0), 0.5);
    }

    #[test]
    fn hard_clipping_holds_at_one() {
        assert_eq!(distortion_shape(1, 4.0, 0.5), 1.0);
        assert!((distortion_shape(0, 1.0, 0.5) - 0.5f32.tanh()).abs() < 1e-6);
        assert!((distortion_shape(2, 2.0, 0.75) - 0.5).abs() < 1e-6);
    }
}
