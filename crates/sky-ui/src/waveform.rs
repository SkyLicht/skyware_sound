// SPDX-License-Identifier: GPL-3.0-or-later
//! Waveform overviews of pool audio, computed once per audio file on a
//! background thread and cached.

use std::collections::HashMap;
use std::sync::Arc;

use sky_core::AudioId;
use sky_dsp::AudioData;

/// Frames summarized by one peak bucket.
pub const BUCKET: usize = 256;

/// Minimum and maximum sample of each bucket, across channels.
pub struct Peaks {
    pub sample_rate: u32,
    pub frames: usize,
    pub min: Vec<f32>,
    pub max: Vec<f32>,
}

impl Peaks {
    pub fn compute(audio: &AudioData) -> Self {
        let frames = audio.frames();
        let buckets = frames.div_ceil(BUCKET);
        let mut min = vec![0.0f32; buckets];
        let mut max = vec![0.0f32; buckets];
        for ch in &audio.channels {
            for (b, chunk) in ch.chunks(BUCKET).enumerate() {
                let (lo, hi) = chunk
                    .iter()
                    .fold((0.0f32, 0.0f32), |(lo, hi), s| (lo.min(*s), hi.max(*s)));
                if let (Some(m), Some(x)) = (min.get_mut(b), max.get_mut(b)) {
                    *m = m.min(lo);
                    *x = x.max(hi);
                }
            }
        }
        Self {
            sample_rate: audio.sample_rate,
            frames,
            min,
            max,
        }
    }

    /// The lowest and highest sample between two source frames.
    pub fn range(&self, from: f64, to: f64) -> Option<(f32, f32)> {
        if self.min.is_empty() || to < 0.0 || from >= self.frames as f64 {
            return None;
        }
        let a = (from.max(0.0) as usize / BUCKET).min(self.min.len() - 1);
        let b = ((to.max(0.0) as usize / BUCKET) + 1).clamp(a + 1, self.min.len());
        let lo = self.min.get(a..b)?.iter().copied().fold(0.0f32, f32::min);
        let hi = self.max.get(a..b)?.iter().copied().fold(0.0f32, f32::max);
        Some((lo, hi))
    }
}

enum Slot {
    Pending,
    Ready(Arc<Peaks>),
    /// The audio is not loaded (a missing file); not retried until cleared.
    Missing,
}

/// Peaks per pool entry.
#[derive(Default)]
pub struct WaveformCache {
    map: HashMap<AudioId, Slot>,
}

impl WaveformCache {
    /// The cached peaks, and whether the caller should start computing them.
    pub fn get(&mut self, id: AudioId) -> (Option<Arc<Peaks>>, bool) {
        match self.map.get(&id) {
            Some(Slot::Ready(p)) => (Some(p.clone()), false),
            Some(Slot::Pending | Slot::Missing) => (None, false),
            None => {
                self.map.insert(id, Slot::Pending);
                (None, true)
            }
        }
    }

    pub fn insert(&mut self, id: AudioId, peaks: Option<Arc<Peaks>>) {
        let slot = match peaks {
            Some(p) => Slot::Ready(p),
            None => Slot::Missing,
        };
        self.map.insert(id, slot);
    }

    /// Forgets everything, after the project or its pool changed.
    pub fn clear(&mut self) {
        self.map.clear();
    }
}
