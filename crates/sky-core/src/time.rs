// SPDX-License-Identifier: MIT OR Apache-2.0
//! Time and tempo.
//!
//! Musical time is kept in ticks, 960 per quarter note. Audio time is kept in
//! samples. A [`TempoMap`] converts between the two exactly enough that loop
//! points land on the same sample every time.

use serde::{Deserialize, Serialize};

use crate::CoreError;

/// Ticks per quarter note.
pub const PPQ: i64 = 960;

/// Musical time in ticks (960 per quarter note).
pub type Ticks = i64;

/// Audio time in sample frames.
pub type SampleTime = i64;

/// One tempo change: from `tick` on, the tempo is `bpm` quarter notes per minute.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempoPoint {
    pub tick: Ticks,
    pub bpm: f64,
}

/// A piecewise-constant tempo map. The first point is always at tick 0.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempoMap {
    points: Vec<TempoPoint>,
}

impl Default for TempoMap {
    fn default() -> Self {
        Self::constant(120.0)
    }
}

fn seconds_per_tick(bpm: f64) -> f64 {
    60.0 / (bpm * PPQ as f64)
}

pub fn validate_bpm(bpm: f64) -> Result<f64, CoreError> {
    if bpm.is_finite() && (10.0..=999.0).contains(&bpm) {
        Ok(bpm)
    } else {
        Err(CoreError::InvalidTempo(bpm))
    }
}

impl TempoMap {
    pub fn constant(bpm: f64) -> Self {
        Self {
            points: vec![TempoPoint { tick: 0, bpm }],
        }
    }

    /// Builds a map from points; they are sorted and the first is moved to tick 0.
    pub fn from_points(mut points: Vec<TempoPoint>) -> Result<Self, CoreError> {
        if points.is_empty() {
            return Ok(Self::default());
        }
        for p in &points {
            validate_bpm(p.bpm)?;
        }
        points.sort_by_key(|p| p.tick);
        points.dedup_by_key(|p| p.tick);
        if let Some(first) = points.first_mut() {
            first.tick = 0;
        }
        Ok(Self { points })
    }

    pub fn points(&self) -> &[TempoPoint] {
        &self.points
    }

    /// The tempo at tick 0, which is the project tempo for constant-tempo songs.
    pub fn initial_bpm(&self) -> f64 {
        self.points.first().map_or(120.0, |p| p.bpm)
    }

    pub fn bpm_at(&self, tick: Ticks) -> f64 {
        let idx = self.points.partition_point(|p| p.tick <= tick);
        self.points
            .get(idx.saturating_sub(1))
            .map_or(120.0, |p| p.bpm)
    }

    /// Seconds from tick 0 to `tick`.
    pub fn tick_to_seconds(&self, tick: Ticks) -> f64 {
        if tick < 0 {
            return tick as f64 * seconds_per_tick(self.initial_bpm());
        }
        let mut seconds = 0.0;
        for (i, p) in self.points.iter().enumerate() {
            if tick <= p.tick {
                break;
            }
            let seg_end = self.points.get(i + 1).map_or(tick, |n| n.tick.min(tick));
            seconds += (seg_end - p.tick) as f64 * seconds_per_tick(p.bpm);
        }
        seconds
    }

    /// The (fractional) tick reached after `seconds` from tick 0.
    pub fn seconds_to_ticks(&self, seconds: f64) -> f64 {
        if seconds < 0.0 {
            return seconds / seconds_per_tick(self.initial_bpm());
        }
        let mut elapsed = 0.0;
        for (i, p) in self.points.iter().enumerate() {
            let secs_per_tick = seconds_per_tick(p.bpm);
            match self.points.get(i + 1) {
                Some(next) => {
                    let seg = (next.tick - p.tick) as f64 * secs_per_tick;
                    if elapsed + seg >= seconds {
                        return p.tick as f64 + (seconds - elapsed) / secs_per_tick;
                    }
                    elapsed += seg;
                }
                None => return p.tick as f64 + (seconds - elapsed) / secs_per_tick,
            }
        }
        0.0
    }

    /// The sample frame at `tick`, rounded to the nearest sample.
    pub fn tick_to_sample(&self, tick: Ticks, sample_rate: u32) -> SampleTime {
        (self.tick_to_seconds(tick) * sample_rate as f64).round() as SampleTime
    }

    /// The (fractional) tick at sample frame `sample`.
    pub fn sample_to_ticks(&self, sample: SampleTime, sample_rate: u32) -> f64 {
        self.seconds_to_ticks(sample as f64 / sample_rate as f64)
    }

    /// The exact length in samples of the span `[start, end)`, and how far
    /// that length is from a whole number of samples. A non-zero remainder
    /// means a loop of that span cannot repeat with sample-perfect timing.
    pub fn span_samples_exact(&self, start: Ticks, end: Ticks, sample_rate: u32) -> (f64, f64) {
        let exact = (self.tick_to_seconds(end) - self.tick_to_seconds(start)) * sample_rate as f64;
        let remainder = (exact - exact.round()).abs();
        (exact, remainder)
    }
}

/// A time signature such as 4/4 or 7/8.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeSignature {
    pub numerator: u32,
    pub denominator: u32,
}

impl Default for TimeSignature {
    fn default() -> Self {
        Self {
            numerator: 4,
            denominator: 4,
        }
    }
}

impl TimeSignature {
    pub fn new(numerator: u32, denominator: u32) -> Result<Self, CoreError> {
        let ok_den = matches!(denominator, 1 | 2 | 4 | 8 | 16 | 32);
        if !ok_den || !(1..=32).contains(&numerator) {
            return Err(CoreError::InvalidTimeSignature(numerator, denominator));
        }
        Ok(Self {
            numerator,
            denominator,
        })
    }

    /// Ticks in one beat (one denominator note).
    pub fn ticks_per_beat(&self) -> Ticks {
        PPQ * 4 / self.denominator.max(1) as Ticks
    }

    pub fn ticks_per_bar(&self) -> Ticks {
        self.ticks_per_beat() * self.numerator as Ticks
    }

    /// Ticks at the start of `bar`, counting bars from 0.
    pub fn bar_to_ticks(&self, bar: f64) -> Ticks {
        (bar * self.ticks_per_bar() as f64).round() as Ticks
    }

    /// Bars, beats and ticks, all counted from 1 for display (`3.2.480`).
    pub fn format_position(&self, tick: Ticks) -> String {
        let tick = tick.max(0);
        let bar = tick / self.ticks_per_bar();
        let in_bar = tick % self.ticks_per_bar();
        let beat = in_bar / self.ticks_per_beat();
        let rest = in_bar % self.ticks_per_beat();
        format!("{}.{}.{:03}", bar + 1, beat + 1, rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_tempo_conversions() {
        let map = TempoMap::constant(120.0);
        assert_eq!(map.tick_to_seconds(PPQ), 0.5);
        assert_eq!(map.tick_to_sample(PPQ * 4, 48_000), 96_000);
        assert!((map.seconds_to_ticks(1.0) - 1920.0).abs() < 1e-9);
        assert!((map.sample_to_ticks(48_000, 48_000) - 1920.0).abs() < 1e-9);
    }

    #[test]
    fn tempo_changes_accumulate() {
        let map = TempoMap::from_points(vec![
            TempoPoint {
                tick: 0,
                bpm: 120.0,
            },
            TempoPoint {
                tick: PPQ * 4,
                bpm: 60.0,
            },
        ])
        .unwrap();
        // 4 beats at 120 = 2 s, then 1 beat at 60 = 1 s.
        assert!((map.tick_to_seconds(PPQ * 5) - 3.0).abs() < 1e-12);
        assert!((map.seconds_to_ticks(3.0) - (PPQ * 5) as f64).abs() < 1e-9);
        assert_eq!(map.bpm_at(PPQ * 3), 120.0);
        assert_eq!(map.bpm_at(PPQ * 4), 60.0);
    }

    #[test]
    fn round_trip_ticks_and_samples() {
        let map = TempoMap::from_points(vec![
            TempoPoint { tick: 0, bpm: 97.0 },
            TempoPoint {
                tick: 7 * PPQ,
                bpm: 141.5,
            },
        ])
        .unwrap();
        for tick in [0, 1, 959, 960, 6719, 6720, 6721, 100_000] {
            let secs = map.tick_to_seconds(tick);
            assert!(
                (map.seconds_to_ticks(secs) - tick as f64).abs() < 1e-6,
                "tick {tick}"
            );
        }
    }

    #[test]
    fn loop_span_exactness() {
        let map = TempoMap::constant(120.0);
        let (exact, rem) = map.span_samples_exact(0, PPQ * 16 * 4, 48_000);
        assert_eq!(exact, 1_536_000.0);
        assert_eq!(rem, 0.0);
        let odd = TempoMap::constant(127.0);
        let (_, rem) = odd.span_samples_exact(0, PPQ * 4, 44_100);
        assert!(rem > 0.0);
    }

    #[test]
    fn time_signature_math() {
        let ts = TimeSignature::new(7, 8).unwrap();
        assert_eq!(ts.ticks_per_beat(), 480);
        assert_eq!(ts.ticks_per_bar(), 3360);
        assert_eq!(
            TimeSignature::default().format_position(PPQ * 5 + 480),
            "2.2.480"
        );
        assert!(TimeSignature::new(4, 3).is_err());
        assert!(validate_bpm(5.0).is_err());
    }
}
