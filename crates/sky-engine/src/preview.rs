// SPDX-License-Identifier: GPL-3.0-or-later
//! The preview player: audio played straight to the output after the master
//! channel, such as a sample being browsed or a game loop rendered the way
//! it will export. It runs whether or not the transport does.

use std::sync::Arc;

use sky_dsp::AudioData;

/// Audio for the preview player.
pub struct Preview {
    /// Shown in [`EngineShared::preview`](crate::EngineShared::preview)
    /// while it plays. Not 0.
    pub id: u64,
    pub audio: Arc<AudioData>,
    /// The source frame to start at.
    pub start: f64,
    /// The source frame playback jumps back to at the end, so a loop
    /// repeats until stopped; `None` plays once.
    pub loop_start: Option<f64>,
    /// Source frames per output frame: the sample-rate ratio times any
    /// pitch shift.
    pub rate: f64,
    pub gain: f32,
}

/// How long a preview takes to fade in when it starts part way through, and
/// to fade out when stopped, so neither clicks.
const FADE_SECONDS: f32 = 0.005;

/// A preview as it plays.
pub(crate) struct PreviewVoice {
    pub(crate) preview: Box<Preview>,
    pos: f64,
    /// The fade gain, 0 to 1.
    env: f32,
    /// The fade's change per frame.
    step: f32,
    stopping: bool,
    pub(crate) done: bool,
}

impl PreviewVoice {
    pub(crate) fn new(preview: Box<Preview>, sample_rate: u32) -> Self {
        let pos = if preview.start.is_finite() {
            preview.start.max(0.0)
        } else {
            0.0
        };
        Self {
            // From the very start there is nothing to fade in from.
            env: if pos == 0.0 { 1.0 } else { 0.0 },
            step: 1.0 / (FADE_SECONDS * sample_rate.max(1) as f32),
            pos,
            stopping: false,
            done: false,
            preview,
        }
    }

    /// Fades out and ends.
    pub(crate) fn stop(&mut self) {
        self.stopping = true;
    }

    /// Adds the next `left.len()` frames into `left` and `right`.
    pub(crate) fn render_add(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.done {
            return;
        }
        let audio = &self.preview.audio;
        let (src_l, src_r) = (audio.left(), audio.right());
        let len = src_l.len().min(src_r.len());
        let rate = self.preview.rate;
        if len == 0 || !(rate.is_finite() && rate > 0.0) {
            self.done = true;
            return;
        }
        let end = len as f64;
        let loop_start = self
            .preview
            .loop_start
            .filter(|s| s.is_finite() && *s >= 0.0 && *s < end);
        let gain = self.preview.gain;
        for (out_l, out_r) in left.iter_mut().zip(right.iter_mut()) {
            if self.stopping {
                self.env -= self.step;
                if self.env <= 0.0 {
                    self.done = true;
                    return;
                }
            } else if self.env < 1.0 {
                self.env = (self.env + self.step).min(1.0);
            }
            if self.pos >= end {
                match loop_start {
                    Some(s) => self.pos = s + (self.pos - end) % (end - s),
                    None => {
                        self.done = true;
                        return;
                    }
                }
            }
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            // Past the last frame a loop reads on from its start; a one-shot
            // holds its last sample.
            let next = if i + 1 < len {
                i + 1
            } else {
                loop_start.map_or(i, |s| s as usize)
            };
            let at = |src: &[f32], k: usize| src.get(k).copied().unwrap_or(0.0);
            let (l0, l1) = (at(src_l, i), at(src_l, next));
            let (r0, r1) = (at(src_r, i), at(src_r, next));
            let g = gain * self.env;
            *out_l += (l0 + (l1 - l0) * frac) * g;
            *out_r += (r0 + (r1 - r0) * frac) * g;
            self.pos += rate;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice(frames: usize, start: f64, loop_start: Option<f64>) -> PreviewVoice {
        let ramp: Vec<f32> = (0..frames).map(|i| i as f32 + 1.0).collect();
        PreviewVoice::new(
            Box::new(Preview {
                id: 1,
                audio: Arc::new(AudioData {
                    sample_rate: 48_000,
                    channels: vec![ramp],
                }),
                start,
                loop_start,
                rate: 1.0,
                gain: 1.0,
            }),
            48_000,
        )
    }

    #[test]
    fn a_one_shot_plays_once_and_ends() {
        let mut v = voice(4, 0.0, None);
        let (mut l, mut r) = (vec![0.0; 8], vec![0.0; 8]);
        v.render_add(&mut l, &mut r);
        assert_eq!(l, [1.0, 2.0, 3.0, 4.0, 0.0, 0.0, 0.0, 0.0]);
        // Mono audio plays on both sides.
        assert_eq!(l, r);
        assert!(v.done);
    }

    #[test]
    fn a_loop_jumps_back_to_its_start() {
        let mut v = voice(4, 0.0, Some(2.0));
        let (mut l, mut r) = (vec![0.0; 8], vec![0.0; 8]);
        v.render_add(&mut l, &mut r);
        assert_eq!(l, [1.0, 2.0, 3.0, 4.0, 3.0, 4.0, 3.0, 4.0]);
        assert!(!v.done);
    }

    #[test]
    fn stopping_fades_out_then_ends() {
        let mut v = voice(48_000, 0.0, Some(0.0));
        let (mut l, mut r) = (vec![0.0; 64], vec![0.0; 64]);
        v.render_add(&mut l, &mut r);
        v.stop();
        let (mut l, mut r) = (vec![0.0; 480], vec![0.0; 480]);
        v.render_add(&mut l, &mut r);
        assert!(v.done);
        // The fade takes 5 ms: 240 frames at 48 kHz.
        assert!(l.get(100).is_some_and(|s| *s > 0.0));
        assert!(l.get(250..).is_some_and(|t| t.iter().all(|s| *s == 0.0)));
    }

    #[test]
    fn starting_part_way_fades_in() {
        let mut v = voice(48_000, 1_000.0, None);
        let (mut l, mut r) = (vec![0.0; 2], vec![0.0; 2]);
        v.render_add(&mut l, &mut r);
        assert!(l[0] < 1_001.0 * 0.01, "{l:?}");
    }
}
