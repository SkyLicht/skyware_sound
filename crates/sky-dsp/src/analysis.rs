// SPDX-License-Identifier: GPL-3.0-or-later
//! Offline analysis, so an AI that cannot listen can still reason about a
//! render: loudness, peaks, onsets, spectral balance and loop seams.

use serde::Serialize;

use crate::AudioData;
use crate::filters::{Biquad, BiquadCoeffs};
use crate::util::gain_to_db;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BandBalance {
    /// RMS level below 250 Hz, in dBFS.
    pub low_db: f32,
    /// RMS level from 250 Hz to 4 kHz, in dBFS.
    pub mid_db: f32,
    /// RMS level above 4 kHz, in dBFS.
    pub high_db: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    pub duration_seconds: f64,
    pub sample_rate: u32,
    pub channels: usize,
    pub peak_dbfs: f32,
    pub true_peak_dbfs: Option<f32>,
    pub rms_dbfs: f32,
    /// EBU R128 integrated loudness. None when the audio is too short or silent.
    pub integrated_lufs: Option<f64>,
    pub loudness_range_lu: Option<f64>,
    /// Short-term (3 s window) loudness, one value per second.
    pub short_term_lufs: Vec<f32>,
    pub clipped_samples: usize,
    pub dc_offset: f32,
    /// Fraction of 10 ms windows quieter than -60 dBFS.
    pub silence_ratio: f32,
    /// Times of detected note or hit onsets, in seconds (at most 64).
    pub onsets_seconds: Vec<f32>,
    pub bands: BandBalance,
    /// Correlation between left and right, from -1 to 1. None for mono.
    pub stereo_correlation: Option<f32>,
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn rms(samples: impl Iterator<Item = f32>) -> f32 {
    let (sum, n) = samples.fold((0.0f64, 0usize), |(s, n), x| {
        (s + (x as f64) * (x as f64), n + 1)
    });
    if n == 0 {
        0.0
    } else {
        (sum / n as f64).sqrt() as f32
    }
}

fn loudness(audio: &AudioData) -> (Option<f64>, Option<f64>, Option<f32>, Vec<f32>) {
    let channels = audio.channel_count().min(2) as u32;
    let mode = ebur128::Mode::I | ebur128::Mode::S | ebur128::Mode::LRA | ebur128::Mode::TRUE_PEAK;
    let Ok(mut meter) = ebur128::EbuR128::new(channels, audio.sample_rate.max(1), mode) else {
        return (None, None, None, Vec::new());
    };
    let mut short_term = Vec::new();
    let second = audio.sample_rate as usize;
    let frames = audio.frames();
    let mut start = 0;
    while start < frames {
        let end = (start + second).min(frames);
        let planes: Vec<&[f32]> = audio
            .channels
            .iter()
            .take(channels as usize)
            .map(|c| &c[start..end])
            .collect();
        if meter.add_frames_planar_f32(&planes).is_err() {
            break;
        }
        if let Ok(st) = meter.loudness_shortterm()
            && st.is_finite()
        {
            short_term.push(round1(st) as f32);
        }
        start = end;
    }
    let finite = |r: Result<f64, ebur128::Error>| r.ok().filter(|v| v.is_finite());
    let integrated = finite(meter.loudness_global()).map(round1);
    let lra = finite(meter.loudness_range()).map(round1);
    let true_peak = (0..channels)
        .filter_map(|c| meter.true_peak(c).ok())
        .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))))
        .map(|p| gain_to_db(p as f32));
    (integrated, lra, true_peak, short_term)
}

fn onsets(mono: &[f32], sample_rate: u32) -> Vec<f32> {
    let hop = (sample_rate as usize / 100).max(1);
    let energies: Vec<f32> = mono
        .chunks(hop)
        .map(|c| gain_to_db(rms(c.iter().copied())))
        .collect();
    let mut out = Vec::new();
    let mut last: Option<usize> = None;
    let mut avg = -120.0f32;
    for (i, e) in energies.iter().enumerate() {
        let rising = *e > -50.0 && *e - avg > 6.0;
        let spaced = last.is_none_or(|l| i - l >= 5);
        if rising && spaced {
            out.push((i * hop) as f32 / sample_rate as f32);
            last = Some(i);
            if out.len() >= 64 {
                break;
            }
        }
        avg = avg * 0.7 + e * 0.3;
    }
    out
}

fn bands(mono: &[f32], sample_rate: u32) -> BandBalance {
    let sr = sample_rate as f32;
    let lp = BiquadCoeffs::lowpass(250.0, 0.707, sr);
    let hp_mid = BiquadCoeffs::highpass(250.0, 0.707, sr);
    let lp_mid = BiquadCoeffs::lowpass(4_000.0, 0.707, sr);
    let hp = BiquadCoeffs::highpass(4_000.0, 0.707, sr);
    let (mut a, mut b, mut c, mut d) = (
        Biquad::default(),
        Biquad::default(),
        Biquad::default(),
        Biquad::default(),
    );
    let (mut low, mut mid, mut high) = (0.0f64, 0.0f64, 0.0f64);
    for &x in mono {
        let l = a.tick(&lp, x);
        let m = c.tick(&lp_mid, b.tick(&hp_mid, x));
        let h = d.tick(&hp, x);
        low += (l * l) as f64;
        mid += (m * m) as f64;
        high += (h * h) as f64;
    }
    let n = mono.len().max(1) as f64;
    let db = |e: f64| (gain_to_db((e / n).sqrt() as f32) * 10.0).round() / 10.0;
    BandBalance {
        low_db: db(low),
        mid_db: db(mid),
        high_db: db(high),
    }
}

/// Measures a whole piece of audio.
pub fn analyze(audio: &AudioData) -> Analysis {
    let frames = audio.frames();
    let left = audio.left();
    let right = audio.right();
    let mono: Vec<f32> = left.iter().zip(right).map(|(l, r)| (l + r) * 0.5).collect();
    let all = || audio.channels.iter().flat_map(|c| c.iter().copied());
    let peak = all().fold(0.0f32, |m, s| m.max(s.abs()));
    let clipped = all().filter(|s| s.abs() >= 0.999).count();
    let dc = if frames == 0 {
        0.0
    } else {
        mono.iter().sum::<f32>() / frames as f32
    };
    let window = (audio.sample_rate as usize / 100).max(1);
    let windows = mono.chunks(window).count().max(1);
    let silent = mono
        .chunks(window)
        .filter(|c| gain_to_db(rms(c.iter().copied())) < -60.0)
        .count();
    let correlation = (audio.channel_count() >= 2).then(|| {
        let (mut lr, mut ll, mut rr) = (0.0f64, 0.0f64, 0.0f64);
        for (l, r) in left.iter().zip(right) {
            lr += (*l as f64) * (*r as f64);
            ll += (*l as f64) * (*l as f64);
            rr += (*r as f64) * (*r as f64);
        }
        if ll == 0.0 || rr == 0.0 {
            1.0
        } else {
            ((lr / (ll * rr).sqrt()) * 100.0).round() as f32 / 100.0
        }
    });
    let (integrated, lra, true_peak, short_term) = loudness(audio);
    let r1 = |x: f32| (x * 10.0).round() / 10.0;
    Analysis {
        duration_seconds: (audio.duration_seconds() * 1000.0).round() / 1000.0,
        sample_rate: audio.sample_rate,
        channels: audio.channel_count(),
        peak_dbfs: r1(gain_to_db(peak)),
        true_peak_dbfs: true_peak.map(r1),
        rms_dbfs: r1(gain_to_db(rms(all()))),
        integrated_lufs: integrated,
        loudness_range_lu: lra,
        short_term_lufs: short_term,
        clipped_samples: clipped,
        dc_offset: (dc * 10_000.0).round() / 10_000.0,
        silence_ratio: ((silent as f32 / windows as f32) * 100.0).round() / 100.0,
        onsets_seconds: onsets(&mono, audio.sample_rate)
            .into_iter()
            .map(|t| (t * 1000.0).round() / 1000.0)
            .collect(),
        bands: bands(&mono, audio.sample_rate),
        stereo_correlation: correlation,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeamReport {
    /// The jump between the last sample of the loop and its first sample.
    pub sample_jump: f32,
    /// The largest step between neighbouring samples near the seam, for comparison.
    pub typical_step: f32,
    /// RMS over the last 50 ms of the loop, in dBFS.
    pub level_before_db: f32,
    /// RMS over the first 50 ms of the loop, in dBFS.
    pub level_after_db: f32,
    pub level_jump_db: f32,
    /// `seamless` or `click`.
    pub verdict: &'static str,
    /// Set when the level changes a lot across the seam. That is often the
    /// music itself (a drum hit on beat one), so it is a hint, not a fault.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<&'static str>,
}

/// Checks how cleanly `[loop_start, loop_end)` repeats.
pub fn seam_check(audio: &AudioData, loop_start: usize, loop_end: usize) -> SeamReport {
    let frames = audio.frames();
    let end = loop_end.min(frames);
    let start = loop_start.min(end.saturating_sub(1));
    let neighbourhood = 64usize;
    let window = (audio.sample_rate as usize / 20).max(1);
    let mut jump = 0.0f32;
    let mut typical = 0.0f32;
    let mut before = 0.0f32;
    let mut after = 0.0f32;
    for ch in &audio.channels {
        if end < 2 || end <= start {
            break;
        }
        jump = jump.max((ch[start] - ch[end - 1]).abs());
        let steps = |range: std::ops::Range<usize>| {
            range
                .clone()
                .zip(range.skip(1))
                .map(|(a, b)| (ch[b] - ch[a]).abs())
                .fold(0.0f32, f32::max)
        };
        typical = typical
            .max(steps(end.saturating_sub(neighbourhood).max(start)..end))
            .max(steps(start..(start + neighbourhood).min(end)));
        before = before.max(rms(ch[end.saturating_sub(window).max(start)..end]
            .iter()
            .copied()));
        after = after.max(rms(ch[start..(start + window).min(end)].iter().copied()));
    }
    let before_db = gain_to_db(before);
    let after_db = gain_to_db(after);
    let level_jump = (before_db - after_db).abs();
    let verdict = if jump > typical * 2.0 + 0.01 {
        "click"
    } else {
        "seamless"
    };
    let note = (level_jump > 6.0 && before_db.max(after_db) > -50.0).then_some(
        "the level changes by more than 6 dB across the seam; fine if the music does that, \
         but a fade at the end of the loop would make the repeat audible",
    );
    let r3 = |x: f32| (x * 1000.0).round() / 1000.0;
    let r1 = |x: f32| (x * 10.0).round() / 10.0;
    SeamReport {
        sample_jump: r3(jump),
        typical_step: r3(typical),
        level_before_db: r1(before_db),
        level_after_db: r1(after_db),
        level_jump_db: r1(level_jump),
        verdict,
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, seconds: f32, amp: f32) -> AudioData {
        let n = (48_000.0 * seconds) as usize;
        let s: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / 48_000.0).sin() * amp)
            .collect();
        AudioData {
            sample_rate: 48_000,
            channels: vec![s.clone(), s],
        }
    }

    #[test]
    fn analysis_of_a_sine() {
        let a = analyze(&sine(1_000.0, 5.0, 0.5));
        assert!((a.peak_dbfs + 6.0).abs() < 0.2, "{}", a.peak_dbfs);
        assert!((a.rms_dbfs + 9.0).abs() < 0.2, "{}", a.rms_dbfs);
        let lufs = a.integrated_lufs.unwrap();
        assert!((lufs + 6.0).abs() < 1.0, "{lufs}");
        assert_eq!(a.stereo_correlation, Some(1.0));
        assert!(a.bands.mid_db > a.bands.low_db + 10.0);
        assert_eq!(a.clipped_samples, 0);
    }

    #[test]
    fn onsets_find_hits() {
        let mut audio = AudioData::silent(1, 48_000, 48_000);
        for hit in [0usize, 12_000, 24_000, 36_000] {
            for i in 0..2_000 {
                audio.channels[0][hit + i] = ((i as f32) * 0.3).sin() * (1.0 - i as f32 / 2_000.0);
            }
        }
        let a = analyze(&audio);
        assert_eq!(a.onsets_seconds.len(), 4, "{:?}", a.onsets_seconds);
    }

    #[test]
    fn seam_check_spots_clicks() {
        // 1000 Hz at 48 kHz has exactly 48 samples per cycle, so a loop of
        // 4800 samples is seamless and one of 4824 samples is not.
        let audio = sine(1_000.0, 1.0, 0.5);
        assert_eq!(seam_check(&audio, 0, 4_800).verdict, "seamless");
        assert_eq!(seam_check(&audio, 0, 4_812).verdict, "click");
    }
}
