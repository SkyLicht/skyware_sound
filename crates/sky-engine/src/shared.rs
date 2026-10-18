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

/// The keys an instrument holds and whether its sustain pedal is down,
/// for an editor to light them.
#[derive(Default)]
pub struct KeyTap {
    low: AtomicU64,
    high: AtomicU64,
    pedal: AtomicBool,
}

impl KeyTap {
    #[inline]
    pub fn record(&self, keys: u128, pedal: bool) {
        self.low.store(keys as u64, Ordering::Relaxed);
        self.high.store((keys >> 64) as u64, Ordering::Relaxed);
        self.pedal.store(pedal, Ordering::Relaxed);
    }

    /// The keys held, one bit per key from bit 0.
    pub fn keys(&self) -> u128 {
        u128::from(self.low.load(Ordering::Relaxed))
            | u128::from(self.high.load(Ordering::Relaxed)) << 64
    }

    pub fn pedal(&self) -> bool {
        self.pedal.load(Ordering::Relaxed)
    }
}

/// Insert slots per channel whose gain reduction is metered.
pub const REDUCTION_SLOTS: usize = 16;

/// The most gain reduction since the last read, in dB, as f32 bits that
/// `fetch_max` orders like the floats.
#[derive(Default)]
pub struct Reduction(AtomicU32);

impl Reduction {
    #[inline]
    pub fn record(&self, db: f32) {
        if db > 0.0 {
            self.0.fetch_max(db.to_bits(), Ordering::Relaxed);
        }
    }

    /// Returns the most reduction since the last call and resets it.
    pub fn take(&self) -> f32 {
        f32::from_bits(self.0.swap(0, Ordering::Relaxed))
    }
}

/// Loudness blocks kept for readers: 6.4 seconds of 100 ms blocks.
pub const LOUDNESS_SLOTS: usize = 64;

/// One 100 ms block of the master's loudness.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoudnessBlock {
    /// K-weighted mean power, summed over both sides.
    pub power: f64,
    /// Whether the song was playing when the block ended.
    pub playing: bool,
}

/// The newest loudness blocks of the master in a ring, so a reader that
/// looks in a few times a second sees every block.
pub struct LoudnessTap {
    /// Each block's power as bits, and whether the song was playing.
    slots: Vec<(AtomicU64, AtomicBool)>,
    written: AtomicU64,
}

impl Default for LoudnessTap {
    fn default() -> Self {
        Self {
            slots: (0..LOUDNESS_SLOTS)
                .map(|_| (AtomicU64::new(0), AtomicBool::new(false)))
                .collect(),
            written: AtomicU64::new(0),
        }
    }
}

impl LoudnessTap {
    fn slot(&self, n: u64) -> Option<&(AtomicU64, AtomicBool)> {
        self.slots.get((n % LOUDNESS_SLOTS as u64) as usize)
    }

    /// Adds a block. Only the audio thread calls this.
    pub fn push(&self, block: LoudnessBlock) {
        let n = self.written.load(Ordering::Relaxed);
        if let Some((power, playing)) = self.slot(n) {
            power.store(block.power.to_bits(), Ordering::Relaxed);
            playing.store(block.playing, Ordering::Relaxed);
        }
        self.written.store(n + 1, Ordering::Release);
    }

    /// Blocks pushed so far.
    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Acquire)
    }

    /// Calls `f` with each block pushed since `seen`, oldest first, and
    /// moves `seen` on. Blocks that fell out of the ring are skipped.
    pub fn read(&self, seen: &mut u64, mut f: impl FnMut(LoudnessBlock)) {
        let n = self.written();
        let start = (*seen).max(n.saturating_sub(LOUDNESS_SLOTS as u64));
        for (power, playing) in (start..n).filter_map(|i| self.slot(i)) {
            f(LoudnessBlock {
                power: f64::from_bits(power.load(Ordering::Relaxed)),
                playing: playing.load(Ordering::Relaxed),
            });
        }
        *seen = n;
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
    /// The id of the preview playing, or 0.
    pub preview: AtomicU64,
    pub master: Meter,
    /// True peak of the master, looked for between the samples.
    pub master_true_peak: Meter,
    /// K-weighted 100 ms blocks of the master, for loudness meters.
    pub loudness: LoudnessTap,
    pub tracks: Vec<Meter>,
    /// Each track's held keys and pedal, by meter slot.
    keys: Vec<KeyTap>,
    /// Gain reduction of each insert slot: [`REDUCTION_SLOTS`] per track
    /// meter slot, then the master's.
    reductions: Vec<Reduction>,
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
            preview: AtomicU64::new(0),
            master: Meter::default(),
            master_true_peak: Meter::default(),
            loudness: LoudnessTap::default(),
            tracks: (0..MAX_METERS).map(|_| Meter::default()).collect(),
            keys: (0..MAX_METERS).map(|_| KeyTap::default()).collect(),
            reductions: (0..(MAX_METERS + 1) * REDUCTION_SLOTS)
                .map(|_| Reduction::default())
                .collect(),
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

    /// The id of the preview playing, if one is.
    pub fn preview(&self) -> Option<u64> {
        Some(self.preview.load(Ordering::Relaxed)).filter(|id| *id != 0)
    }

    pub fn track_meter(&self, index: usize) -> Option<&Meter> {
        self.tracks.get(index)
    }

    /// The keys and pedal of the track in meter slot `index`.
    pub fn track_keys(&self, index: usize) -> Option<&KeyTap> {
        self.keys.get(index)
    }

    /// The gain-reduction meters of a channel's insert slots, by effect
    /// position: a track's by its meter slot, or the master's for `None`.
    /// Tracks past the last meter slot get none.
    pub fn reductions(&self, track: Option<usize>) -> &[Reduction] {
        let channel = match track {
            Some(t) if t < MAX_METERS => t,
            Some(_) => return &[],
            None => MAX_METERS,
        };
        let from = channel * REDUCTION_SLOTS;
        self.reductions
            .get(from..from + REDUCTION_SLOTS)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_loudness_tap_hands_out_each_block_once_in_order() {
        let tap = LoudnessTap::default();
        let mut seen = 0;
        let mut got = Vec::new();
        tap.read(&mut seen, |b| got.push(b));
        assert!(got.is_empty());
        for i in 0..5 {
            tap.push(LoudnessBlock {
                power: i as f64,
                playing: i % 2 == 0,
            });
        }
        tap.read(&mut seen, |b| got.push(b));
        assert_eq!(got.len(), 5);
        assert_eq!(got[3].power, 3.0);
        assert!(got[4].playing && !got[3].playing);
        got.clear();
        tap.read(&mut seen, |b| got.push(b));
        assert!(got.is_empty());
        // A reader that falls behind gets the newest ring's worth.
        for i in 0..200 {
            tap.push(LoudnessBlock {
                power: i as f64,
                playing: true,
            });
        }
        tap.read(&mut seen, |b| got.push(b));
        assert_eq!(got.len(), LOUDNESS_SLOTS);
        assert_eq!(got.last().unwrap().power, 199.0);
        assert_eq!(got[0].power, (200 - LOUDNESS_SLOTS) as f64);
    }
}
