// SPDX-License-Identifier: GPL-3.0-or-later
//! Voices that each play one decoded sample straight through, for the
//! sample bank and the sampler.
//!
//! They play the way the multi-sample engine in [`sampled`] does: 4-point
//! cubic interpolation, and when every voice is sounding, the oldest fades
//! out over a few milliseconds while the new note starts in a spare slot,
//! instead of jumping to the new sample.
//!
//! [`sampled`]: crate::instruments::sampled

use crate::AudioData;
use crate::instruments::Adsr;
use crate::instruments::sampled::{STEAL_MS, cubic};

/// The slots an instrument with `voices` voices needs: those plus room for
/// stolen ones to fade.
pub(crate) const fn slots(voices: usize) -> usize {
    voices + voices / 2
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Voice {
    /// Which of the instrument's samples it plays.
    pub sample: u32,
    /// The key that started it.
    pub key: u8,
    /// Frame position in the sample.
    pub pos: f64,
    pub rate: f64,
    pub velocity: f32,
    pub age: u64,
    pub env: Adsr,
    /// 1 while playing; above 0 and falling when stolen.
    fade: f32,
    /// How much `fade` falls each frame once stolen.
    fade_step: f32,
    stolen: bool,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            sample: 0,
            key: 0,
            pos: 0.0,
            rate: 1.0,
            velocity: 0.0,
            age: 0,
            env: Adsr::default(),
            fade: 1.0,
            fade_step: 1.0,
            stolen: false,
        }
    }
}

impl Voice {
    /// A voice that starts now, with its envelope at the start of its
    /// attack.
    pub(crate) fn new(sample: u32, key: u8, pos: f64, rate: f64, velocity: f32, age: u64) -> Self {
        let mut env = Adsr::default();
        env.trigger();
        Self {
            sample,
            key,
            pos,
            rate,
            velocity,
            age,
            env,
            ..Self::default()
        }
    }

    /// Adds the voice into `left` and `right`, playing `data` up to frame
    /// `end`, at `gain` on top of its envelope and velocity. Frames before
    /// the start and past the end of the audio read as silence.
    pub(crate) fn render(
        &mut self,
        data: &AudioData,
        end: usize,
        gain: f32,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        let (src_l, src_r) = (data.left(), data.right());
        let mono = std::ptr::eq(src_l, src_r);
        let end = end.min(src_l.len()).min(src_r.len()) as f64;
        let at = |ch: &[f32], i: usize| ch.get(i).copied().unwrap_or(0.0);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            if self.pos >= end {
                self.env.reset();
                break;
            }
            let i = self.pos as usize;
            let t = (self.pos - i as f64) as f32;
            let read = |ch: &[f32]| {
                cubic(
                    at(ch, i.wrapping_sub(1)),
                    at(ch, i),
                    at(ch, i + 1),
                    at(ch, i + 2),
                    t,
                )
            };
            let sl = read(src_l);
            let sr = if mono { sl } else { read(src_r) };
            let mut g = self.env.tick() * self.velocity * gain;
            if self.stolen {
                self.fade -= self.fade_step;
                if self.fade <= 0.0 {
                    self.env.reset();
                    break;
                }
                g *= self.fade;
            }
            *l += sl * g;
            *r += sr * g;
            self.pos += self.rate;
        }
    }
}

/// The slot a new note goes in. When `sounding` voices already sound, the
/// oldest makes way, released ones first: it fades out over a few
/// milliseconds while the new note takes a free slot. With no slot free,
/// the quietest fading one is taken.
pub(crate) fn free_slot(voices: &mut [Voice], sounding: usize, sample_rate: f32) -> usize {
    let busy = |v: &Voice| v.env.is_active() && !v.stolen;
    if voices.iter().filter(|v| busy(v)).count() >= sounding {
        let victim = voices
            .iter()
            .enumerate()
            .filter(|(_, v)| busy(v))
            .min_by_key(|(_, v)| (!v.env.is_released(), v.age))
            .map(|(i, _)| i);
        if let Some(v) = victim.and_then(|i| voices.get_mut(i)) {
            v.stolen = true;
            v.fade_step = 1.0 / (STEAL_MS * 0.001 * sample_rate).max(1.0);
        }
    }
    if let Some(i) = voices.iter().position(|v| !v.env.is_active()) {
        return i;
    }
    voices
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.fade.total_cmp(&b.1.fade))
        .map_or(0, |(i, _)| i)
}
