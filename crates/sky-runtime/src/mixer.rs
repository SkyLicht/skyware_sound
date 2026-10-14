// SPDX-License-Identifier: MIT OR Apache-2.0
//! The audio-thread half of the runtime: the voice pool and the gains that
//! apply to the whole mix, changed only by [`MixCommand`]s.
//!
//! Everything here runs on the audio thread and must not allocate, free,
//! lock or panic. Decoded audio reaches a voice inside a `Start` command and
//! leaves it either back to the game thread (see [`Mixer::with_returns`]) or,
//! when there is no game thread, when the next `Start` on that slot replaces
//! it from the caller's thread.

use std::sync::Arc;

use sky_core::manifest::Category;

use crate::decode::AudioData;
use crate::voice::{LoopRegion, Ramp, Voice, VoiceState};

/// How a voice starts. Everything random or derived from the asset was
/// decided by the game thread.
#[derive(Debug)]
pub(crate) struct VoiceStart {
    pub generation: u32,
    pub audio: Arc<AudioData>,
    pub region: Option<LoopRegion>,
    pub rate: f64,
    pub gain: f32,
    pub fade_in_frames: u32,
    pub pan: (f32, f32),
    pub category: usize,
}

/// One change to the mix.
#[derive(Debug)]
pub(crate) enum MixCommand {
    Start {
        slot: u32,
        start: VoiceStart,
    },
    Stop {
        slot: u32,
        generation: u32,
        frames: u32,
    },
    StopAll {
        frames: u32,
    },
    Gain {
        slot: u32,
        generation: u32,
        amp: f32,
        frames: u32,
    },
    Pan {
        slot: u32,
        generation: u32,
        left: f32,
        right: f32,
        frames: u32,
    },
    Rate {
        slot: u32,
        generation: u32,
        rate: f64,
    },
    Master {
        amp: f32,
        frames: u32,
    },
    Category {
        index: usize,
        amp: f32,
        frames: u32,
    },
}

/// A voice that ended, and the audio it held, going back to the game thread.
#[derive(Debug)]
pub(crate) struct Ended {
    pub slot: u32,
    pub generation: u32,
    pub audio: Option<Arc<AudioData>>,
}

/// Where ended voices are reported in the threaded runtime.
struct Returns {
    queue: rtrb::Producer<Ended>,
    /// Audio displaced by a `Start` while the queue was full; sent on later.
    held: Box<[Option<Arc<AudioData>>]>,
}

pub(crate) struct Mixer {
    voices: Box<[Voice]>,
    master: Ramp,
    categories: [Ramp; Category::ALL.len()],
    returns: Option<Returns>,
}

impl Mixer {
    pub(crate) fn new(max_voices: usize) -> Self {
        Self {
            voices: (0..max_voices).map(|_| Voice::new()).collect(),
            master: Ramp::at(1.0),
            categories: [Ramp::at(1.0); Category::ALL.len()],
            returns: None,
        }
    }

    /// Reports ended voices and returns their audio through `queue` instead
    /// of keeping it.
    pub(crate) fn with_returns(mut self, queue: rtrb::Producer<Ended>) -> Self {
        let held = (0..self.voices.len()).map(|_| None).collect();
        self.returns = Some(Returns { queue, held });
        // Voices that ended before the split still hold their audio.
        for i in 0..self.voices.len() {
            let v = &mut self.voices[i];
            if !v.is_active() && v.audio.is_some() {
                v.unreported = true;
            }
        }
        self
    }

    pub(crate) fn max_voices(&self) -> usize {
        self.voices.len()
    }

    pub(crate) fn is_playing(&self, slot: u32, generation: u32) -> bool {
        self.voices
            .get(slot as usize)
            .is_some_and(|v| v.generation == generation && v.is_active())
    }

    pub(crate) fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.is_active()).count()
    }

    /// Whether the voice in `slot` is still sounding.
    pub(crate) fn slot_active(&self, slot: usize) -> bool {
        self.voices.get(slot).is_some_and(Voice::is_active)
    }

    /// Drops the audio that finished voices still hold. Only for the
    /// single-threaded runtime, on the caller's thread.
    pub(crate) fn release_finished(&mut self) {
        for voice in self.voices.iter_mut().filter(|v| !v.is_active()) {
            voice.audio = None;
        }
    }

    pub(crate) fn apply(&mut self, cmd: MixCommand) {
        match cmd {
            MixCommand::Start { slot, start } => self.start(slot as usize, start),
            MixCommand::Stop {
                slot,
                generation,
                frames,
            } => {
                if let Some(i) = self.live(slot, generation) {
                    self.stop(i, frames);
                }
            }
            MixCommand::StopAll { frames } => {
                for i in 0..self.voices.len() {
                    if self.voices[i].is_active() {
                        self.stop(i, frames);
                    }
                }
            }
            MixCommand::Gain {
                slot,
                generation,
                amp,
                frames,
            } => {
                if let Some(i) = self.live(slot, generation) {
                    self.voices[i].gain.ramp_to(amp, frames);
                }
            }
            MixCommand::Pan {
                slot,
                generation,
                left,
                right,
                frames,
            } => {
                if let Some(i) = self.live(slot, generation) {
                    let v = &mut self.voices[i];
                    v.pan_left.ramp_to(left, frames);
                    v.pan_right.ramp_to(right, frames);
                }
            }
            MixCommand::Rate {
                slot,
                generation,
                rate,
            } => {
                if let Some(i) = self.live(slot, generation) {
                    self.voices[i].rate = rate;
                }
            }
            MixCommand::Master { amp, frames } => self.master.ramp_to(amp, frames),
            MixCommand::Category { index, amp, frames } => {
                if let Some(c) = self.categories.get_mut(index) {
                    c.ramp_to(amp, frames);
                }
            }
        }
    }

    /// The slot index if `generation` is the voice sounding in it.
    fn live(&self, slot: u32, generation: u32) -> Option<usize> {
        let i = slot as usize;
        self.voices
            .get(i)
            .is_some_and(|v| v.generation == generation && v.is_active())
            .then_some(i)
    }

    fn start(&mut self, slot: usize, start: VoiceStart) {
        if slot >= self.voices.len() {
            return;
        }
        // Whatever the slot held goes back first: a voice being stolen, or
        // audio from a voice that ended and was not reported yet.
        if self.voices[slot].is_active() || self.voices[slot].unreported {
            self.voices[slot].state = VoiceState::Free;
            self.report(slot);
        }
        let VoiceStart {
            generation,
            audio,
            region,
            rate,
            gain,
            fade_in_frames,
            pan,
            category,
        } = start;
        let old = {
            let v = &mut self.voices[slot];
            v.generation = generation;
            v.state = VoiceState::Playing;
            v.region = region;
            v.pos = 0.0;
            v.rate = rate;
            v.wrapped = false;
            v.gain = Ramp::at(gain);
            let mut fade = Ramp::at(if fade_in_frames > 0 { 0.0 } else { 1.0 });
            fade.ramp_to(1.0, fade_in_frames);
            v.fade = fade;
            v.pan_left = Ramp::at(pan.0);
            v.pan_right = Ramp::at(pan.1);
            v.category = category.min(Category::ALL.len() - 1);
            v.unreported = false;
            v.audio.replace(audio)
        };
        // Without a game thread to give it to, this runs on the caller's
        // thread and old audio is dropped here. With one, the slot's audio
        // was already sent back by `report`, unless the queue was full.
        if let Some(old) = old {
            self.hold(old);
        }
    }

    fn stop(&mut self, i: usize, frames: u32) {
        let v = &mut self.voices[i];
        if frames == 0 {
            v.state = VoiceState::Free;
            self.ended(i);
        } else {
            v.state = VoiceState::Stopping;
            v.fade.ramp_to(0.0, frames);
        }
    }

    /// A voice just became free.
    fn ended(&mut self, i: usize) {
        if self.returns.is_some() {
            self.voices[i].unreported = true;
            self.report(i);
        }
    }

    /// Tells the game thread that the voice in `slot` ended and hands its
    /// audio back. If the queue is full the voice stays unreported and is
    /// tried again on the next render.
    fn report(&mut self, slot: usize) {
        let Some(returns) = self.returns.as_mut() else {
            return;
        };
        let v = &mut self.voices[slot];
        let ended = Ended {
            slot: slot as u32,
            generation: v.generation,
            audio: v.audio.take(),
        };
        match returns.queue.push(ended) {
            Ok(()) => v.unreported = false,
            Err(rtrb::PushError::Full(back)) => v.audio = back.audio,
        }
    }

    /// Keeps audio that could not be sent back, to send it later.
    fn hold(&mut self, audio: Arc<AudioData>) {
        let Some(returns) = self.returns.as_mut() else {
            // Single-threaded: the caller's thread drops it.
            drop(audio);
            return;
        };
        let pushed = returns.queue.push(Ended {
            slot: u32::MAX,
            generation: 0,
            audio: Some(audio),
        });
        if let Err(rtrb::PushError::Full(back)) = pushed
            && let Some(audio) = back.audio
            && let Some(free) = returns.held.iter_mut().find(|h| h.is_none())
        {
            *free = Some(audio);
        }
    }

    /// Retries the reports and returns that did not fit in the queue.
    fn flush_returns(&mut self) {
        let Some(returns) = self.returns.as_mut() else {
            return;
        };
        for held in returns.held.iter_mut().filter(|h| h.is_some()) {
            if returns.queue.is_full() {
                return;
            }
            let _ = returns.queue.push(Ended {
                slot: u32::MAX,
                generation: 0,
                audio: held.take(),
            });
        }
        for i in 0..self.voices.len() {
            if self.voices[i].unreported && !self.voices[i].is_active() {
                self.report(i);
            }
        }
    }

    /// Mixes every voice into `out`, interleaved stereo, overwriting it.
    pub(crate) fn render(&mut self, out: &mut [f32]) {
        self.flush_returns();
        out.fill(0.0);
        let even = out.len() & !1;
        let (out, _) = out.split_at_mut(even);
        let frames = even / 2;
        for i in 0..self.voices.len() {
            let v = &mut self.voices[i];
            if !v.is_active() {
                continue;
            }
            let category = self.categories[v.category.min(Category::ALL.len() - 1)];
            v.render_add(out, category);
            if !v.is_active() {
                self.ended(i);
            }
        }
        for c in &mut self.categories {
            c.advance(frames);
        }
        if self.master.is_settled() && self.master.value == 1.0 {
            return;
        }
        for frame in out.as_chunks_mut::<2>().0 {
            let g = self.master.next();
            frame[0] *= g;
            frame[1] *= g;
        }
    }
}
