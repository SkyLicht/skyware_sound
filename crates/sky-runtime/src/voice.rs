// SPDX-License-Identifier: MIT OR Apache-2.0
//! One playing sound: read position, interpolation, envelopes and pan.
//!
//! Everything reachable from [`Voice::render_add`] runs on the audio thread
//! and must not allocate, free, lock or panic.

use std::sync::Arc;

use crate::decode::AudioData;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VoiceState {
    Free,
    Playing,
    /// Fading out after `stop`; frees itself when the fade reaches zero.
    Stopping,
}

/// A linear ramp of a gain value, advanced once per output frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Ramp {
    pub value: f32,
    target: f32,
    step: f32,
    remaining: u32,
}

impl Ramp {
    pub(crate) const fn at(value: f32) -> Self {
        Self {
            value,
            target: value,
            step: 0.0,
            remaining: 0,
        }
    }

    /// Moves to `target` over `frames` frames; zero frames jumps at once.
    pub(crate) fn ramp_to(&mut self, target: f32, frames: u32) {
        self.target = target;
        if frames == 0 {
            self.value = target;
            self.remaining = 0;
        } else {
            self.step = (target - self.value) / frames as f32;
            self.remaining = frames;
        }
    }

    /// Advances one frame and returns the gain for it.
    #[inline(always)]
    pub(crate) fn next(&mut self) -> f32 {
        if self.remaining > 0 {
            self.remaining -= 1;
            if self.remaining == 0 {
                self.value = self.target;
            } else {
                self.value += self.step;
            }
        }
        self.value
    }

    pub(crate) fn is_settled(&self) -> bool {
        self.remaining == 0
    }
}

/// The repeating region of a loop voice, in source frames.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LoopRegion {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy)]
struct Region {
    start: i64,
    end: i64,
    len: i64,
    start_f: f64,
    end_f: f64,
    len_f: f64,
}

impl Region {
    fn new(r: LoopRegion) -> Option<Self> {
        let start = i64::try_from(r.start).ok()?;
        let end = i64::try_from(r.end).ok()?;
        let len = end - start;
        (len > 0).then_some(Self {
            start,
            end,
            len,
            start_f: start as f64,
            end_f: end as f64,
            len_f: len as f64,
        })
    }
}

#[derive(Debug)]
pub(crate) struct Voice {
    pub generation: u32,
    pub state: VoiceState,
    pub is_loop: bool,
    /// Start order, used to steal the oldest SFX voice.
    pub started: u64,
    /// Kept after the voice ends so the audio thread never frees memory; it
    /// is replaced (and possibly freed) by the next `play` on this slot.
    pub audio: Option<Arc<AudioData>>,
    pub region: Option<LoopRegion>,
    pub pos: f64,
    pub rate: f64,
    /// Whether a loop voice has jumped back at least once.
    pub wrapped: bool,
    pub gain: Ramp,
    pub fade: Ramp,
    /// Random gain offset picked at trigger time, kept across `set_voice_gain`.
    pub random_gain_db: f32,
    pub pan_left: f32,
    pub pan_right: f32,
}

impl Voice {
    pub(crate) fn new() -> Self {
        Self {
            generation: 0,
            state: VoiceState::Free,
            is_loop: false,
            started: 0,
            audio: None,
            region: None,
            pos: 0.0,
            rate: 1.0,
            wrapped: false,
            gain: Ramp::at(1.0),
            fade: Ramp::at(1.0),
            random_gain_db: 0.0,
            pan_left: 1.0,
            pan_right: 1.0,
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.state != VoiceState::Free
    }

    /// Mixes this voice into `out` (interleaved stereo, even length).
    pub(crate) fn render_add(&mut self, out: &mut [f32]) {
        let Some(audio) = self.audio.as_deref() else {
            self.state = VoiceState::Free;
            return;
        };
        let region = if self.is_loop {
            match self.region.and_then(Region::new) {
                Some(r) => Some(r),
                None => {
                    self.state = VoiceState::Free;
                    return;
                }
            }
        } else {
            None
        };
        let left: &[f32] = &audio.left;
        let right: Option<&[f32]> = audio.right.as_deref();
        let frames = audio.frames as f64;
        let rate = self.rate;
        let (pan_left, pan_right) = (self.pan_left, self.pan_right);
        let stopping = self.state == VoiceState::Stopping;

        let mut pos = self.pos;
        let mut wrapped = self.wrapped;
        let mut gain = self.gain;
        let mut fade = self.fade;
        let mut finished = region.is_none() && pos >= frames;

        if !finished {
            for frame in out.chunks_exact_mut(2) {
                let g = gain.next() * fade.next();
                let ip = pos.floor();
                let t = (pos - ip) as f32;
                let i = ip as i64;
                let l = sample(left, i, t, region, wrapped);
                let (l, r) = match right {
                    Some(right) => (
                        l * pan_left,
                        sample(right, i, t, region, wrapped) * pan_right,
                    ),
                    None => (l * pan_left, l * pan_right),
                };
                frame[0] += l * g;
                frame[1] += r * g;

                if stopping && fade.is_settled() && fade.value <= 0.0 {
                    finished = true;
                    break;
                }

                pos += rate;
                match region {
                    Some(r) => {
                        if pos >= r.end_f {
                            // Keep the fractional remainder so the seam is seamless.
                            pos -= r.len_f;
                            if pos >= r.end_f {
                                pos = r.start_f + (pos - r.start_f) % r.len_f;
                            }
                            wrapped = true;
                        }
                    }
                    None => {
                        if pos >= frames {
                            finished = true;
                            break;
                        }
                    }
                }
            }
        }

        self.pos = pos;
        self.wrapped = wrapped;
        self.gain = gain;
        self.fade = fade;
        if finished {
            self.state = VoiceState::Free;
        }
    }
}

/// Reads channel `ch` at position `i + t` with 4-point Hermite interpolation.
#[inline(always)]
fn sample(ch: &[f32], i: i64, t: f32, region: Option<Region>, wrapped: bool) -> f32 {
    let x0 = fetch(ch, i, region, wrapped);
    if t == 0.0 {
        // Exact at integer positions, so same-rate playback is bit-exact.
        return x0;
    }
    let xm1 = fetch(ch, i.wrapping_sub(1), region, wrapped);
    let x1 = fetch(ch, i.wrapping_add(1), region, wrapped);
    let x2 = fetch(ch, i.wrapping_add(2), region, wrapped);
    hermite(t, xm1, x0, x1, x2)
}

/// One source sample; loop voices see the stream intro, body, body, ...
#[inline(always)]
fn fetch(ch: &[f32], i: i64, region: Option<Region>, wrapped: bool) -> f32 {
    let mut i = i;
    if let Some(r) = region {
        if i >= r.end {
            i = r
                .start
                .wrapping_add(i.wrapping_sub(r.start).rem_euclid(r.len));
        } else if wrapped && i < r.start {
            i = i.wrapping_add(r.len);
        }
    }
    if i < 0 {
        return 0.0;
    }
    ch.get(i as usize).copied().unwrap_or(0.0)
}

#[inline(always)]
fn hermite(t: f32, xm1: f32, x0: f32, x1: f32, x2: f32) -> f32 {
    let c1 = 0.5 * (x1 - xm1);
    let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
    let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
    ((c3 * t + c2) * t + c1) * t + x0
}

/// Left and right gains for `pan` in `[-1, 1]`.
///
/// Mono sources use the equal-power law (-3 dB per side at center). Stereo
/// sources use an equal-power balance: both sides are at unity at center and
/// the side panned away from fades out along a cosine curve.
pub(crate) fn pan_gains(pan: f32, stereo: bool) -> (f32, f32) {
    let pan = f64::from(pan.clamp(-1.0, 1.0));
    let quarter = std::f64::consts::FRAC_PI_2;
    let (l, r) = if stereo {
        if pan >= 0.0 {
            ((pan * quarter).cos(), 1.0)
        } else {
            (1.0, (-pan * quarter).cos())
        }
    } else {
        let theta = (pan + 1.0) * 0.5 * quarter;
        (theta.cos(), theta.sin())
    };
    // cos(pi/2) is not exactly zero in floating point; snap it.
    let snap = |g: f64| if g < 1e-9 { 0.0 } else { g.min(1.0) as f32 };
    (snap(l), snap(r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramp_reaches_target_exactly() {
        let mut r = Ramp::at(1.0);
        r.ramp_to(0.0, 4);
        let values: Vec<f32> = (0..6).map(|_| r.next()).collect();
        assert_eq!(values, vec![0.75, 0.5, 0.25, 0.0, 0.0, 0.0]);
        assert!(r.is_settled());
        r.ramp_to(0.5, 0);
        assert_eq!(r.next(), 0.5);
    }

    #[test]
    fn hermite_hits_samples_and_lines() {
        assert_eq!(hermite(0.0, 1.0, 2.0, 3.0, 4.0), 2.0);
        assert!((hermite(0.5, 1.0, 2.0, 3.0, 4.0) - 2.5).abs() < 1e-6);
    }

    #[test]
    fn pan_laws() {
        let (l, r) = pan_gains(0.0, false);
        assert!((l - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert_eq!(l, r);
        assert_eq!(pan_gains(-1.0, false), (1.0, 0.0));
        assert_eq!(pan_gains(1.0, false), (0.0, 1.0));
        assert_eq!(pan_gains(0.0, true), (1.0, 1.0));
        assert_eq!(pan_gains(1.0, true), (0.0, 1.0));
        assert_eq!(pan_gains(-1.0, true), (1.0, 0.0));
        let (l, r) = pan_gains(0.5, false);
        assert!((l * l + r * r - 1.0).abs() < 1e-6);
    }

    #[test]
    fn fetch_wraps_inside_the_loop() {
        let data = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let region = Region::new(LoopRegion { start: 2, end: 5 });
        assert_eq!(fetch(&data, 5, region, false), 2.0);
        assert_eq!(fetch(&data, 6, region, false), 3.0);
        assert_eq!(fetch(&data, 1, region, false), 1.0);
        assert_eq!(fetch(&data, 1, region, true), 4.0);
        assert_eq!(fetch(&data, -1, None, false), 0.0);
        assert_eq!(fetch(&data, 6, None, false), 0.0);
    }
}
