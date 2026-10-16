// SPDX-License-Identifier: GPL-3.0-or-later
//! Live loudness and true peak, measured the way ITU-R BS.1770 and EBU R128
//! describe.
//!
//! [`BlockMeter`] runs on the audio thread: it K-weights the signal, hands
//! out the mean power of each 100 ms block, and finds the true peak by
//! looking between the samples at up to four times the rate. [`Loudness`]
//! runs anywhere else and turns those blocks into momentary, short-term and
//! integrated loudness.

use std::collections::VecDeque;
use std::f64::consts::PI;

/// Length of one block, in seconds.
pub const BLOCK_SECONDS: f64 = 0.1;
/// Blocks in a momentary window (400 ms).
const MOMENTARY_BLOCKS: usize = 4;
/// Blocks in a short-term window (3 s).
const SHORT_TERM_BLOCKS: usize = 30;
/// Blocks quieter than this never count towards integrated loudness.
const ABSOLUTE_GATE_LUFS: f64 = -70.0;
/// Blocks this far below the ungated average are left out as well.
const RELATIVE_GATE_LU: f64 = -10.0;

/// Loudness in LUFS of a K-weighted mean power summed over channels.
pub fn power_to_lufs(power: f64) -> f64 {
    if power > 0.0 {
        -0.691 + 10.0 * power.log10()
    } else {
        f64::NEG_INFINITY
    }
}

fn audible(lufs: f64) -> Option<f64> {
    (lufs >= ABSOLUTE_GATE_LUFS).then_some(lufs)
}

fn lufs_to_power(lufs: f64) -> f64 {
    10f64.powf((lufs + 0.691) / 10.0)
}

/// A second-order filter in transposed direct form II, in f64 so the
/// low shelf of the K-weighting stays exact.
#[derive(Clone, Copy, Debug, Default)]
struct Section {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Section {
    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }

    /// Clears state too small to matter, before it turns denormal.
    fn flush(&mut self) {
        for z in &mut self.z {
            if z.abs() < 1e-30 {
                *z = 0.0;
            }
        }
    }
}

/// The two stages of the K-weighting filter at any sample rate: a high
/// shelf for the head, then a high-pass.
fn k_weighting(rate: f64) -> [Section; 2] {
    let f0 = 1_681.974_450_955_533;
    let gain_db = 3.999_843_853_973_347;
    let q = 0.707_175_236_955_419_6;
    let k = (PI * f0 / rate).tan();
    let vh = 10f64.powf(gain_db / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Section {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    let f0 = 38.135_470_876_024_44;
    let q = 0.500_327_037_323_877_3;
    let k = (PI * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let high_pass = Section {
        b: [1.0, -2.0, 1.0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    [shelf, high_pass]
}

const TAPS: usize = 48;

/// Upsamples one channel and keeps the largest value it sees between the
/// samples, with a Hann-windowed sinc as libebur128 does.
#[derive(Clone, Debug)]
struct Oversampler {
    factor: usize,
    coeffs: [f32; TAPS],
    /// The last `TAPS / factor` samples twice over, newest first from `pos`,
    /// so they can be read as one slice.
    history: [f32; TAPS * 2],
    pos: usize,
}

impl Oversampler {
    fn new(rate: u32) -> Self {
        let factor = if rate < 96_000 {
            4
        } else if rate < 192_000 {
            2
        } else {
            1
        };
        let mut coeffs = [0.0f32; TAPS];
        let window = TAPS as f64;
        for (j, c) in coeffs.iter_mut().enumerate() {
            let j = j as f64;
            let w = 0.5 * (1.0 - (2.0 * PI * j / window).cos());
            let m = j - window / 2.0;
            let x = m * PI / factor as f64;
            *c = if m.abs() > 1e-6 { w * x.sin() / x } else { w } as f32;
        }
        Self {
            factor,
            coeffs,
            history: [0.0; TAPS * 2],
            pos: 0,
        }
    }

    fn taps(&self) -> usize {
        TAPS / self.factor
    }

    /// The largest absolute value among the samples made from `x`.
    #[inline]
    fn peak(&mut self, x: f32) -> f32 {
        if self.factor == 1 {
            return x.abs();
        }
        let taps = self.taps();
        self.pos = if self.pos == 0 {
            taps - 1
        } else {
            self.pos - 1
        };
        self.history[self.pos] = x;
        self.history[self.pos + taps] = x;
        let recent = &self.history[self.pos..self.pos + taps];
        let mut peak = 0.0f32;
        for phase in 0..self.factor {
            let mut y = 0.0f32;
            for (t, s) in recent.iter().enumerate() {
                y += self.coeffs[t * self.factor + phase] * s;
            }
            peak = peak.max(y.abs());
        }
        peak
    }

    fn reset(&mut self) {
        self.history = [0.0; TAPS * 2];
        self.pos = 0;
    }
}

/// Measures a stereo signal on the audio thread. Never allocates after
/// [`new`](Self::new).
#[derive(Clone, Debug)]
pub struct BlockMeter {
    filters: [[Section; 2]; 2],
    oversamplers: [Oversampler; 2],
    block_len: usize,
    filled: usize,
    /// Sum of the squared K-weighted samples of both channels this block.
    sum: f64,
}

impl BlockMeter {
    pub fn new(sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate.max(1));
        let k = k_weighting(rate);
        Self {
            filters: [k, k],
            oversamplers: [Oversampler::new(sample_rate), Oversampler::new(sample_rate)],
            block_len: ((rate * BLOCK_SECONDS).round() as usize).max(1),
            filled: 0,
            sum: 0.0,
        }
    }

    /// Measures a block of left and right samples. `on_block` gets the mean
    /// power of each 100 ms block that ends inside it; the return value is
    /// the true peak of each side, as linear gain.
    pub fn process(
        &mut self,
        left: &[f32],
        right: &[f32],
        mut on_block: impl FnMut(f64),
    ) -> (f32, f32) {
        let mut peaks = [0.0f32; 2];
        for (&l, &r) in left.iter().zip(right) {
            for (ch, x) in [l, r].into_iter().enumerate() {
                let x = if x.is_finite() { x } else { 0.0 };
                let [shelf, high_pass] = &mut self.filters[ch];
                let y = high_pass.run(shelf.run(f64::from(x)));
                self.sum += y * y;
                peaks[ch] = peaks[ch].max(x.abs()).max(self.oversamplers[ch].peak(x));
            }
            self.filled += 1;
            if self.filled == self.block_len {
                on_block(self.sum / self.block_len as f64);
                self.filled = 0;
                self.sum = 0.0;
                for section in self.filters.iter_mut().flatten() {
                    section.flush();
                }
            }
        }
        (peaks[0], peaks[1])
    }

    /// Forgets the signal so far, as after a seek.
    pub fn reset(&mut self) {
        for section in self.filters.iter_mut().flatten() {
            section.z = [0.0; 2];
        }
        for o in &mut self.oversamplers {
            o.reset();
        }
        self.filled = 0;
        self.sum = 0.0;
    }
}

/// Momentary, short-term and integrated loudness from the 100 ms blocks of
/// a [`BlockMeter`].
#[derive(Clone, Debug, Default)]
pub struct Loudness {
    /// The newest blocks, oldest first, at most a short-term window's worth.
    recent: VecDeque<f64>,
    /// The power of each 400 ms window measured since the last reset that
    /// is louder than the absolute gate.
    gated: Vec<f64>,
}

impl Loudness {
    /// Adds the next block. `integrate` says whether it counts towards the
    /// integrated loudness, as while the song plays.
    pub fn push(&mut self, power: f64, integrate: bool) {
        let power = if power.is_finite() {
            power.max(0.0)
        } else {
            0.0
        };
        if self.recent.len() == SHORT_TERM_BLOCKS {
            self.recent.pop_front();
        }
        self.recent.push_back(power);
        if integrate && self.recent.len() >= MOMENTARY_BLOCKS {
            let window = self.window(MOMENTARY_BLOCKS);
            if power_to_lufs(window) >= ABSOLUTE_GATE_LUFS {
                self.gated.push(window);
            }
        }
    }

    /// Mean power of the newest `blocks` blocks, with silence before the first.
    fn window(&self, blocks: usize) -> f64 {
        self.recent.iter().rev().take(blocks).sum::<f64>() / blocks as f64
    }

    /// Loudness over the last 400 ms, in LUFS, or None when quieter than
    /// the absolute gate.
    pub fn momentary(&self) -> Option<f64> {
        audible(power_to_lufs(self.window(MOMENTARY_BLOCKS)))
    }

    /// Loudness over the last 3 s, in LUFS, or None when quieter than the
    /// absolute gate.
    pub fn short_term(&self) -> Option<f64> {
        audible(power_to_lufs(self.window(SHORT_TERM_BLOCKS)))
    }

    /// Gated loudness of everything integrated since the last reset, in
    /// LUFS, or None before anything above the gate.
    pub fn integrated(&self) -> Option<f64> {
        if self.gated.is_empty() {
            return None;
        }
        let mean = self.gated.iter().sum::<f64>() / self.gated.len() as f64;
        let threshold = lufs_to_power(power_to_lufs(mean) + RELATIVE_GATE_LU);
        let (sum, n) = self
            .gated
            .iter()
            .filter(|p| **p >= threshold)
            .fold((0.0, 0usize), |(s, n), p| (s + p, n + 1));
        (n > 0)
            .then(|| power_to_lufs(sum / n as f64))
            .filter(|l| l.is_finite())
    }

    /// Starts the integrated loudness again.
    pub fn reset_integrated(&mut self) {
        self.gated.clear();
    }

    /// Forgets everything.
    pub fn clear(&mut self) {
        self.recent.clear();
        self.gated.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AudioData;

    fn sine(freq: f32, gain: f32, seconds: f32, rate: u32) -> Vec<f32> {
        let n = (seconds * rate as f32) as usize;
        (0..n)
            .map(|i| gain * (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin())
            .collect()
    }

    /// Runs a stereo signal through the meter in device-sized chunks.
    fn measure(left: &[f32], right: &[f32], rate: u32) -> (Loudness, f32) {
        let mut meter = BlockMeter::new(rate);
        let mut loud = Loudness::default();
        let mut peak = 0.0f32;
        for (l, r) in left.chunks(256).zip(right.chunks(256)) {
            let (pl, pr) = meter.process(l, r, |p| loud.push(p, true));
            peak = peak.max(pl).max(pr);
        }
        (loud, peak)
    }

    fn reference(left: &[f32], right: &[f32], rate: u32) -> (f64, f64, f64, f64) {
        let mode =
            ebur128::Mode::I | ebur128::Mode::M | ebur128::Mode::S | ebur128::Mode::TRUE_PEAK;
        let mut m = ebur128::EbuR128::new(2, rate, mode).unwrap();
        m.add_frames_planar_f32(&[left, right]).unwrap();
        let tp = m.true_peak(0).unwrap().max(m.true_peak(1).unwrap());
        (
            m.loudness_momentary().unwrap(),
            m.loudness_shortterm().unwrap(),
            m.loudness_global().unwrap(),
            tp,
        )
    }

    #[test]
    fn a_full_scale_1k_sine_reads_minus_3_lufs_per_channel() {
        // BS.1770: a 0 dBFS 1 kHz sine in one channel reads -3.01 LUFS.
        let rate = 48_000;
        let left = sine(1000.0, 1.0, 5.0, rate);
        let right = vec![0.0; left.len()];
        let (loud, _) = measure(&left, &right, rate);
        let i = loud.integrated().unwrap();
        assert!((i + 3.01).abs() < 0.05, "{i}");
    }

    #[test]
    fn matches_libebur128_on_music_like_signals() {
        for rate in [44_100, 48_000, 96_000] {
            // A loud part and a quiet part, so the gates matter.
            let mut left = sine(110.0, 0.5, 4.0, rate);
            left.extend(sine(3000.0, 0.05, 3.0, rate));
            left.extend(sine(440.0, 0.3, 2.5, rate));
            let mut right = sine(220.0, 0.25, 4.0, rate);
            right.extend(sine(60.0, 0.02, 3.0, rate));
            right.extend(sine(880.0, 0.4, 2.5, rate));
            let (loud, peak) = measure(&left, &right, rate);
            let (m, s, i, tp) = reference(&left, &right, rate);
            let close = |a: f64, b: f64| (a - b).abs() < 0.05;
            assert!(
                close(loud.momentary().unwrap(), m),
                "{rate}: M {:?} vs {m}",
                loud.momentary()
            );
            assert!(
                close(loud.short_term().unwrap(), s),
                "{rate}: S {:?} vs {s}",
                loud.short_term()
            );
            assert!(
                close(loud.integrated().unwrap(), i),
                "{rate}: I {:?} vs {i}",
                loud.integrated()
            );
            assert!(
                (f64::from(peak) - tp).abs() < 0.002,
                "{rate}: TP {peak} vs {tp}"
            );
        }
    }

    #[test]
    fn true_peak_finds_the_peak_between_samples() {
        // A quarter-rate sine sampled at its 45° points never shows its top.
        let rate = 48_000;
        let n = 4800;
        let x: Vec<f32> = (0..n)
            .map(|i| (std::f32::consts::FRAC_PI_4 + std::f32::consts::FRAC_PI_2 * i as f32).sin())
            .collect();
        let sample_peak = x.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((sample_peak - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3);
        let (_, peak) = measure(&x, &x, rate);
        assert!(peak > 0.95, "{peak}");
        let analysis = crate::analysis::analyze(&AudioData {
            sample_rate: rate,
            channels: vec![x.clone(), x],
        });
        let tp = crate::util::db_to_gain(analysis.true_peak_dbfs.unwrap());
        assert!((peak - tp).abs() < 0.01, "{peak} vs {tp}");
    }

    #[test]
    fn silence_reads_nothing_and_quiet_blocks_are_gated_out() {
        let rate = 48_000;
        let quiet = sine(1000.0, 1e-4, 2.0, rate);
        let (loud, peak) = measure(&quiet, &quiet, rate);
        assert!(loud.integrated().is_none());
        assert_eq!(loud.momentary(), None);
        assert!(peak < 2e-4);
        let zeros = vec![0.0; 48_000];
        let (loud, peak) = measure(&zeros, &zeros, rate);
        assert_eq!(loud.momentary(), None);
        assert_eq!(loud.short_term(), None);
        assert_eq!(peak, 0.0);
    }

    #[test]
    fn integration_waits_for_the_flag_and_resets() {
        let mut loud = Loudness::default();
        let p = lufs_to_power(-20.0);
        for _ in 0..10 {
            loud.push(p, false);
        }
        assert_eq!(loud.integrated(), None);
        assert!((loud.momentary().unwrap() + 20.0).abs() < 1e-9);
        for _ in 0..10 {
            loud.push(p, true);
        }
        assert!((loud.integrated().unwrap() + 20.0).abs() < 1e-9);
        loud.reset_integrated();
        assert_eq!(loud.integrated(), None);
        assert!(loud.short_term().is_some());
        loud.clear();
        assert_eq!(loud.short_term(), None);
    }

    #[test]
    fn non_finite_samples_do_not_poison_the_meter() {
        let rate = 48_000;
        let mut meter = BlockMeter::new(rate);
        let mut loud = Loudness::default();
        let bad = [f32::NAN, f32::INFINITY, 0.5, -0.5];
        let (pl, pr) = meter.process(&bad, &bad, |p| loud.push(p, true));
        assert!(pl.is_finite() && pr.is_finite());
        let tone = sine(1000.0, 0.5, 1.0, rate);
        meter.process(&tone, &tone, |p| loud.push(p, true));
        assert!(loud.momentary().unwrap().is_finite());
    }
}
