// SPDX-License-Identifier: GPL-3.0-or-later
//! In-memory audio.

/// Planar 32-bit float audio.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioData {
    pub sample_rate: u32,
    /// One vector per channel, all the same length.
    pub channels: Vec<Vec<f32>>,
}

impl AudioData {
    pub fn silent(channels: usize, frames: usize, sample_rate: u32) -> Self {
        Self {
            sample_rate,
            channels: vec![vec![0.0; frames]; channels.max(1)],
        }
    }

    pub fn from_interleaved(samples: &[f32], channels: usize, sample_rate: u32) -> Self {
        let channels = channels.max(1);
        let frames = samples.len() / channels;
        let mut out = vec![Vec::with_capacity(frames); channels];
        for frame in samples.chunks_exact(channels) {
            for (ch, s) in out.iter_mut().zip(frame) {
                ch.push(*s);
            }
        }
        Self {
            sample_rate,
            channels: out,
        }
    }

    pub fn to_interleaved(&self) -> Vec<f32> {
        let frames = self.frames();
        let n = self.channel_count();
        let mut out = Vec::with_capacity(frames * n);
        for i in 0..frames {
            for ch in &self.channels {
                out.push(ch.get(i).copied().unwrap_or(0.0));
            }
        }
        out
    }

    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    pub fn duration_seconds(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frames() as f64 / self.sample_rate as f64
        }
    }

    /// The left channel, or the only channel of mono audio.
    pub fn left(&self) -> &[f32] {
        self.channels.first().map_or(&[], Vec::as_slice)
    }

    /// The right channel, or the left channel again for mono audio.
    pub fn right(&self) -> &[f32] {
        self.channels
            .get(1)
            .or_else(|| self.channels.first())
            .map_or(&[], Vec::as_slice)
    }

    /// Absolute sample peak across channels.
    pub fn peak(&self) -> f32 {
        self.channels
            .iter()
            .flat_map(|c| c.iter())
            .fold(0.0f32, |m, s| m.max(s.abs()))
    }

    /// Frames `[start, end)` as new audio, clamped to the available range.
    pub fn slice(&self, start: usize, end: usize) -> AudioData {
        let end = end.min(self.frames());
        let start = start.min(end);
        AudioData {
            sample_rate: self.sample_rate,
            channels: self
                .channels
                .iter()
                .map(|c| c[start..end].to_vec())
                .collect(),
        }
    }

    /// Scales every sample by `gain`.
    pub fn apply_gain(&mut self, gain: f32) {
        for ch in &mut self.channels {
            for s in ch {
                *s *= gain;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleave_round_trip() {
        let a = AudioData::from_interleaved(&[1.0, -1.0, 0.5, -0.5], 2, 48_000);
        assert_eq!(a.frames(), 2);
        assert_eq!(a.left(), &[1.0, 0.5]);
        assert_eq!(a.right(), &[-1.0, -0.5]);
        assert_eq!(a.to_interleaved(), vec![1.0, -1.0, 0.5, -0.5]);
        assert_eq!(a.peak(), 1.0);
        assert_eq!(a.slice(1, 10).frames(), 1);
    }
}
