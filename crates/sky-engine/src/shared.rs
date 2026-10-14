// SPDX-License-Identifier: GPL-3.0-or-later
//! State the audio thread publishes for the UI and MCP, through atomics so
//! the audio thread never waits for a reader.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};

/// Meter slots: one per track by position, plus the master in its own slot.
pub const MAX_METERS: usize = 256;

/// Peak levels since the last read. Non-negative f32 bit patterns order the
/// same way as the floats, so `fetch_max` on the bits keeps the true peak.
#[derive(Default)]
pub struct Meter {
    left: AtomicU32,
    right: AtomicU32,
}

impl Meter {
    #[inline]
    pub fn record(&self, left: f32, right: f32) {
        self.left.fetch_max(left.abs().to_bits(), Ordering::Relaxed);
        self.right
            .fetch_max(right.abs().to_bits(), Ordering::Relaxed);
    }

    /// Returns the peaks since the last call and resets them.
    pub fn take(&self) -> (f32, f32) {
        (
            f32::from_bits(self.left.swap(0, Ordering::Relaxed)),
            f32::from_bits(self.right.swap(0, Ordering::Relaxed)),
        )
    }
}

pub struct EngineShared {
    pub playhead: AtomicI64,
    pub playing: AtomicBool,
    pub sample_rate: AtomicU32,
    /// Snapshot version the audio thread is playing.
    pub version: AtomicU64,
    /// Device buffers that arrived late or were lost, counted by the device layer.
    pub dropouts: AtomicU64,
    /// Callbacks whose processing took longer than their buffer lasts.
    pub overruns: AtomicU64,
    /// Share of the buffer time the last callback used, as f32 bits.
    pub load: AtomicU32,
    pub callbacks: AtomicU64,
    pub master: Meter,
    pub tracks: Vec<Meter>,
}

impl EngineShared {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            playhead: AtomicI64::new(0),
            playing: AtomicBool::new(false),
            sample_rate: AtomicU32::new(sample_rate),
            version: AtomicU64::new(0),
            dropouts: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            load: AtomicU32::new(0),
            callbacks: AtomicU64::new(0),
            master: Meter::default(),
            tracks: (0..MAX_METERS).map(|_| Meter::default()).collect(),
        }
    }

    pub fn playhead(&self) -> i64 {
        self.playhead.load(Ordering::Relaxed)
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    pub fn load(&self) -> f32 {
        f32::from_bits(self.load.load(Ordering::Relaxed))
    }

    pub fn track_meter(&self, index: usize) -> Option<&Meter> {
        self.tracks.get(index)
    }
}
