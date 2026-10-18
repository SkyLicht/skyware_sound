// SPDX-License-Identifier: GPL-3.0-or-later
//! The audio-thread side of the engine. Nothing here allocates, locks or
//! panics once the engine is running.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use rtrb::{Consumer, Producer, PushError, RingBuffer};
use sky_core::TrackId;
use sky_dsp::loudness::BlockMeter;
use sky_dsp::{Instrument, MAX_BLOCK, SUSTAIN_PEDAL};

use crate::compile::{
    AudioClipNode, ChannelNode, EngineSnapshot, NoteEvent, SourceNode, TrackNode, Triggers,
};
use crate::preview::{Preview, PreviewVoice};
use crate::shared::{EngineShared, LoudnessBlock, Reduction};

/// Messages from the control thread to the audio thread.
pub enum EngineMsg {
    /// Replace the playing snapshot.
    Swap(Box<EngineSnapshot>),
    Play,
    Stop,
    /// Move the playhead to a sample position.
    Seek(i64),
    /// Silence every instrument and clear every effect tail.
    AllSoundOff,
    /// Starts a key on a track's instrument now, whether or not the
    /// transport runs, to hear a note as it is played or placed. With
    /// `frames` it ends by itself after that many frames.
    NoteOn {
        track: TrackId,
        pitch: u8,
        velocity: u8,
        frames: Option<u32>,
    },
    NoteOff {
        track: TrackId,
        pitch: u8,
    },
    /// Moves a controller of a track's instrument now, such as the
    /// sustain pedal while keys are played to hear them. Playback moves
    /// the pedal again where a clip's pedal changes.
    Controller {
        track: TrackId,
        controller: u8,
        value: u8,
    },
    /// Starts sound number `sound` of a track's sample bank as a one-shot.
    SoundOn {
        track: TrackId,
        sound: u32,
        velocity: u8,
    },
    /// Plays audio straight to the output after the master channel,
    /// replacing any preview playing.
    Preview(Box<Preview>),
    /// Fades the preview out.
    StopPreview,
}

/// What the audio thread hands back to be freed on the control thread.
enum Garbage {
    Snapshot(#[allow(dead_code)] Box<EngineSnapshot>),
    Preview(#[allow(dead_code)] Box<Preview>),
}

/// Most live notes that end by themselves sounding at once.
const MAX_LIVE: usize = 32;

/// A live note that ends by itself.
#[derive(Clone, Copy)]
struct LiveNote {
    track: TrackId,
    pitch: u8,
    /// Frames until it ends.
    left: i64,
}

/// The control-thread end of the engine's queues.
pub struct EngineHandle {
    tx: Producer<EngineMsg>,
    trash: Consumer<Garbage>,
    pending: VecDeque<EngineMsg>,
    shared: Arc<EngineShared>,
}

impl EngineHandle {
    /// Queues a message. If the queue is full the message waits here and
    /// is retried by [`flush`](Self::flush); a newer snapshot replaces an
    /// older one that has not gone out yet.
    pub fn send(&mut self, msg: EngineMsg) {
        if let EngineMsg::Swap(_) = msg {
            self.pending.retain(|m| !matches!(m, EngineMsg::Swap(_)));
        }
        self.pending.push_back(msg);
        self.flush();
    }

    pub fn flush(&mut self) {
        while let Some(msg) = self.pending.pop_front() {
            match self.tx.push(msg) {
                Ok(()) => {}
                Err(PushError::Full(msg)) => {
                    self.pending.push_front(msg);
                    break;
                }
            }
        }
    }

    /// Frees snapshots and previews the audio thread has finished with.
    /// Call regularly.
    pub fn collect_garbage(&mut self) -> usize {
        let mut freed = 0;
        while let Ok(old) = self.trash.pop() {
            drop(old);
            freed += 1;
        }
        freed
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn shared(&self) -> &Arc<EngineShared> {
        &self.shared
    }
}

pub struct Engine {
    snapshot: Box<EngineSnapshot>,
    rx: Consumer<EngineMsg>,
    trash: Producer<Garbage>,
    pending_trash: Option<Garbage>,
    shared: Arc<EngineShared>,
    playing: bool,
    position: i64,
    mix: [Vec<f32>; 2],
    smoothing: f32,
    live: [Option<LiveNote>; MAX_LIVE],
    preview: Option<PreviewVoice>,
    /// Loudness and true peak of the master, for the meters.
    loudness: BlockMeter,
}

/// Creates the two ends of an engine around a first snapshot.
pub fn create_engine(snapshot: EngineSnapshot) -> (EngineHandle, Engine) {
    let (tx, rx) = RingBuffer::new(256);
    let (trash_tx, trash_rx) = RingBuffer::new(64);
    let shared = Arc::new(EngineShared::new(snapshot.sample_rate));
    let loudness = BlockMeter::new(snapshot.sample_rate);
    let smoothing = 1.0 - (-1.0 / (0.01 * snapshot.sample_rate.max(1) as f32)).exp();
    let handle = EngineHandle {
        tx,
        trash: trash_rx,
        pending: VecDeque::new(),
        shared: shared.clone(),
    };
    let engine = Engine {
        snapshot: Box::new(snapshot),
        rx,
        trash: trash_tx,
        pending_trash: None,
        shared,
        playing: false,
        position: 0,
        mix: [vec![0.0; MAX_BLOCK], vec![0.0; MAX_BLOCK]],
        smoothing,
        live: [None; MAX_LIVE],
        preview: None,
        loudness,
    };
    (handle, engine)
}

/// The instrument of a track, if it has one.
fn instrument_of(snapshot: &mut EngineSnapshot, track: TrackId) -> Option<&mut dyn Instrument> {
    match &mut snapshot.tracks.iter_mut().find(|t| t.id == track)?.source {
        SourceNode::Instrument {
            instrument: Some(inst),
            ..
        } => Some(inst.as_mut()),
        _ => None,
    }
}

fn seek_cursor(notes: &[NoteEvent], position: i64) -> usize {
    notes.partition_point(|n| n.time < position)
}

/// Lets go of every note, and of the pedal without a sound.
fn release_all(track: &mut TrackNode) {
    if let SourceNode::Instrument {
        instrument: Some(inst),
        ..
    } = &mut track.source
    {
        inst.all_notes_off();
    }
    track.state.active = [-1; 128];
    track.state.pedal = false;
}

/// Whether the pedal is down at sample `t`, and when it next moves.
fn pedal_at(pedal: &[(i64, i64)], t: i64) -> (bool, Option<i64>) {
    let i = pedal.partition_point(|s| s.1 <= t);
    match pedal.get(i) {
        Some(&(start, end)) if start <= t => (true, Some(end)),
        Some(&(start, _)) => (false, Some(start)),
        None => (false, None),
    }
}

fn reseek(track: &mut TrackNode, position: i64) {
    if let SourceNode::Instrument { notes, .. } = &track.source {
        track.state.cursor = seek_cursor(notes, position);
    }
}

fn adopt_effects(new: &mut ChannelNode, old: &mut ChannelNode) {
    for fx in new.effects.iter_mut() {
        if let Some(prev) = old.effects.iter_mut().find(|o| o.id == fx.id) {
            fx.effect.adopt(prev.effect.as_mut());
        }
    }
    new.current = old.current;
}

/// Moves running state from `old` into `new`, matching tracks and effects by ID.
fn adopt_snapshot(new: &mut EngineSnapshot, old: &mut EngineSnapshot, position: i64) {
    for nt in new.tracks.iter_mut() {
        if let Some(ot) = old.tracks.iter_mut().find(|t| t.id == nt.id) {
            if let (
                SourceNode::Instrument {
                    instrument: Some(ni),
                    ..
                },
                SourceNode::Instrument {
                    instrument: Some(oi),
                    ..
                },
            ) = (&mut nt.source, &mut ot.source)
                && ni.adopt(oi.as_mut())
            {
                nt.state.pedal = ot.state.pedal;
            }
            adopt_effects(&mut nt.channel, &mut ot.channel);
            std::mem::swap(&mut nt.state.active, &mut ot.state.active);
        }
        reseek(nt, position);
    }
    adopt_effects(&mut new.master, &mut old.master);
}

fn mix_clip(clip: &AudioClipNode, pos: i64, left: &mut [f32], right: &mut [f32]) {
    let len = left.len() as i64;
    let start = clip.start.max(pos);
    let end = clip.end.min(pos + len);
    if end <= start {
        return;
    }
    let src_l = clip.data.left();
    let src_r = clip.data.right();
    for t in start..end {
        let rel = t - clip.start;
        let src = clip.offset + rel as f64 * clip.rate;
        let idx = src as usize;
        let frac = (src - idx as f64) as f32;
        let (Some(&l0), Some(&l1), Some(&r0), Some(&r1)) = (
            src_l.get(idx),
            src_l.get(idx + 1),
            src_r.get(idx),
            src_r.get(idx + 1),
        ) else {
            break;
        };
        let mut gain = clip.gain;
        if clip.fade_in > 0 && rel < clip.fade_in {
            gain *= rel as f32 / clip.fade_in as f32;
        }
        let to_end = clip.end - t;
        if clip.fade_out > 0 && to_end < clip.fade_out {
            gain *= to_end as f32 / clip.fade_out as f32;
        }
        let i = (t - pos) as usize;
        if let (Some(l), Some(r)) = (left.get_mut(i), right.get_mut(i)) {
            *l += (l0 + (l1 - l0) * frac) * gain;
            *r += (r0 + (r1 - r0) * frac) * gain;
        }
    }
}

/// Renders a track's source for `[pos, pos + len)` into its buffer at
/// `offset`, noting each note it starts in `triggers`.
fn render_source(
    track: &mut TrackNode,
    triggers: &mut Triggers,
    playing: bool,
    pos: i64,
    offset: usize,
    len: usize,
) {
    let [bl, br] = &mut track.buf;
    let (Some(bl), Some(br)) = (
        bl.get_mut(offset..offset + len),
        br.get_mut(offset..offset + len),
    ) else {
        return;
    };
    bl.fill(0.0);
    br.fill(0.0);
    match &mut track.source {
        SourceNode::Audio(clips) => {
            if playing {
                for clip in clips.iter() {
                    mix_clip(clip, pos, bl, br);
                }
            }
        }
        SourceNode::Instrument {
            instrument,
            notes,
            locks,
            pedal,
        } => {
            let Some(inst) = instrument else {
                return;
            };
            if !playing {
                inst.render(bl, br);
                return;
            }
            let state = &mut track.state;
            let end = pos + len as i64;
            let mut t = pos;
            loop {
                let next_on = notes
                    .get(state.cursor)
                    .map(|n| n.time)
                    .filter(|&time| time < end);
                let next_off = state
                    .active
                    .iter()
                    .copied()
                    .filter(|&e| e >= 0 && e < end)
                    .min();
                // Now, when the pedal is not where it should be.
                let (down, moves) = pedal_at(pedal, t);
                let next_pedal = if down == state.pedal {
                    moves.filter(|&m| m < end)
                } else {
                    Some(t)
                };
                let next = [next_on, next_off, next_pedal]
                    .into_iter()
                    .flatten()
                    .min()
                    .unwrap_or(end)
                    .max(t);
                if next > t {
                    let a = (t - pos) as usize;
                    let b = (next - pos) as usize;
                    if let (Some(l), Some(r)) = (bl.get_mut(a..b), br.get_mut(a..b)) {
                        inst.render(l, r);
                    }
                    t = next;
                }
                if t >= end {
                    break;
                }
                for (pitch, e) in state.active.iter_mut().enumerate() {
                    if *e >= 0 && *e <= t {
                        inst.note_off(pitch as u8);
                        *e = -1;
                    }
                }
                // Between the notes that end and the ones that start, so a
                // chord struck as the pedal moves is caught by it.
                let (down, _) = pedal_at(pedal, t);
                if down != state.pedal {
                    inst.controller(SUSTAIN_PEDAL, if down { 127 } else { 0 });
                    state.pedal = down;
                }
                while let Some(n) = notes.get(state.cursor) {
                    if n.time > t {
                        break;
                    }
                    if n.end > t {
                        let range = n.lock_start as usize..n.lock_end as usize;
                        for lock in locks.get(range).unwrap_or_default() {
                            inst.set_param(lock.param, lock.value);
                        }
                        triggers.push(offset + (t - pos) as usize, n.pitch);
                        if let Some(sound) = n.sound {
                            // Bank sounds are one-shots: nothing to end.
                            inst.sound_on(sound, n.velocity);
                        } else if let Some(slot) = state.active.get_mut(n.pitch as usize) {
                            if *slot >= 0 {
                                inst.note_off(n.pitch);
                            }
                            *slot = n.end;
                            inst.note_on(n.pitch, n.velocity);
                        }
                    }
                    state.cursor += 1;
                }
            }
        }
    }
}

/// Passes the note starts of each effect's sidechain track to it.
fn feed_sidechains(channel: &mut ChannelNode, triggers: &[Triggers]) {
    for fx in channel.effects.iter_mut().filter(|f| f.enabled) {
        if let Some(source) = fx.sidechain.and_then(|i| triggers.get(i)) {
            for (offset, pitch) in source.iter() {
                fx.effect.sidechain_note(offset, pitch);
            }
        }
    }
}

/// Applies a channel's smoothed gain and pan in place and returns the peaks.
fn apply_channel(
    channel: &mut ChannelNode,
    k: f32,
    left: &mut [f32],
    right: &mut [f32],
    reductions: &[Reduction],
) -> (f32, f32) {
    for (i, fx) in channel.effects.iter_mut().enumerate() {
        if fx.enabled {
            fx.effect.process(left, right);
            if let Some(meter) = reductions.get(i) {
                meter.record(fx.effect.gain_reduction_db());
            }
        }
    }
    let [cl, cr] = &mut channel.current;
    let (tl, tr) = (channel.gain_l, channel.gain_r);
    let mut peaks = (0.0f32, 0.0f32);
    for (l, r) in left.iter_mut().zip(right.iter_mut()) {
        *cl += (tl - *cl) * k;
        *cr += (tr - *cr) * k;
        *l *= *cl;
        *r *= *cr;
        peaks.0 = peaks.0.max(l.abs());
        peaks.1 = peaks.1.max(r.abs());
    }
    peaks
}

impl Engine {
    pub fn shared(&self) -> &Arc<EngineShared> {
        &self.shared
    }

    pub fn sample_rate(&self) -> u32 {
        self.snapshot.sample_rate
    }

    pub fn position(&self) -> i64 {
        self.position
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Starts playback at `position` immediately (offline rendering).
    pub fn start_at(&mut self, position: i64) {
        self.position = position.max(0);
        self.playing = true;
        for t in self.snapshot.tracks.iter_mut() {
            release_all(t);
            reseek(t, self.position);
        }
    }

    pub(crate) fn snapshot_mut(&mut self) -> &mut EngineSnapshot {
        &mut self.snapshot
    }

    fn handle_messages(&mut self) {
        loop {
            if let Some(p) = self.pending_trash.take()
                && let Err(PushError::Full(p)) = self.trash.push(p)
            {
                // Keep it until the control thread empties the queue; no
                // new snapshot is taken in the meantime.
                self.pending_trash = Some(p);
                return;
            }
            let Ok(msg) = self.rx.pop() else {
                break;
            };
            match msg {
                EngineMsg::Swap(mut new) => {
                    adopt_snapshot(&mut new, &mut self.snapshot, self.position);
                    let old = std::mem::replace(&mut self.snapshot, new);
                    self.shared
                        .version
                        .store(self.snapshot.version, Ordering::Relaxed);
                    self.discard(Garbage::Snapshot(old));
                }
                EngineMsg::Play => {
                    if !self.playing {
                        self.playing = true;
                        for t in self.snapshot.tracks.iter_mut() {
                            reseek(t, self.position);
                        }
                    }
                }
                EngineMsg::Stop => {
                    self.playing = false;
                    for t in self.snapshot.tracks.iter_mut() {
                        release_all(t);
                    }
                }
                EngineMsg::Seek(pos) => {
                    self.position = pos.max(0);
                    for t in self.snapshot.tracks.iter_mut() {
                        release_all(t);
                        reseek(t, self.position);
                    }
                }
                EngineMsg::AllSoundOff => {
                    let snap = &mut self.snapshot;
                    for t in snap.tracks.iter_mut() {
                        if let SourceNode::Instrument {
                            instrument: Some(inst),
                            ..
                        } = &mut t.source
                        {
                            inst.reset();
                        }
                        t.state.active = [-1; 128];
                        t.state.pedal = false;
                        for fx in t.channel.effects.iter_mut() {
                            fx.effect.reset();
                        }
                    }
                    for fx in snap.master.effects.iter_mut() {
                        fx.effect.reset();
                    }
                    self.live = [None; MAX_LIVE];
                    if let Some(v) = self.preview.as_mut() {
                        v.done = true;
                    }
                }
                EngineMsg::NoteOn {
                    track,
                    pitch,
                    velocity,
                    frames,
                } => {
                    let pitch = pitch.min(127);
                    if let Some(inst) = instrument_of(&mut self.snapshot, track) {
                        inst.note_on(pitch, velocity.clamp(1, 127));
                        self.set_live(track, pitch, frames);
                    }
                }
                EngineMsg::NoteOff { track, pitch } => {
                    if let Some(inst) = instrument_of(&mut self.snapshot, track) {
                        inst.note_off(pitch.min(127));
                    }
                    self.set_live(track, pitch.min(127), None);
                }
                EngineMsg::Controller {
                    track,
                    controller,
                    value,
                } => {
                    if let Some(inst) = instrument_of(&mut self.snapshot, track) {
                        inst.controller(controller, value.min(127));
                    }
                }
                EngineMsg::SoundOn {
                    track,
                    sound,
                    velocity,
                } => {
                    if let Some(inst) = instrument_of(&mut self.snapshot, track) {
                        inst.sound_on(sound, velocity.clamp(1, 127));
                    }
                }
                EngineMsg::Preview(preview) => {
                    let id = preview.id;
                    let voice = PreviewVoice::new(preview, self.snapshot.sample_rate);
                    if let Some(old) = self.preview.replace(voice) {
                        self.discard(Garbage::Preview(old.preview));
                    }
                    self.shared.preview.store(id, Ordering::Relaxed);
                }
                EngineMsg::StopPreview => {
                    if let Some(v) = self.preview.as_mut() {
                        v.stop();
                    }
                }
            }
        }
    }

    /// Hands `garbage` to the control thread to free. When its queue is
    /// full, it waits here, and no new message is taken until it is gone.
    fn discard(&mut self, garbage: Garbage) {
        if let Err(PushError::Full(g)) = self.trash.push(garbage) {
            self.pending_trash = Some(g);
        }
    }

    /// Notes when a live note ends: after `frames`, or (`None`) when told.
    fn set_live(&mut self, track: TrackId, pitch: u8, frames: Option<u32>) {
        for slot in self.live.iter_mut() {
            if slot.is_some_and(|n| n.track == track && n.pitch == pitch) {
                *slot = None;
            }
        }
        let Some(frames) = frames else {
            return;
        };
        let note = LiveNote {
            track,
            pitch,
            left: i64::from(frames),
        };
        if let Some(slot) = self.live.iter_mut().find(|s| s.is_none()) {
            *slot = Some(note);
            return;
        }
        // All taken: end the note closest to its end now and take its slot.
        let Some(slot) = self
            .live
            .iter_mut()
            .min_by_key(|s| s.map_or(i64::MIN, |n| n.left))
        else {
            return;
        };
        if let Some(old) = slot.replace(note)
            && let Some(inst) = instrument_of(&mut self.snapshot, old.track)
        {
            inst.note_off(old.pitch);
        }
    }

    /// Ends the live notes whose time is up after `frames` more frames.
    fn advance_live(&mut self, frames: usize) {
        for i in 0..MAX_LIVE {
            let Some(slot) = self.live.get_mut(i) else {
                break;
            };
            let Some(note) = slot.as_mut() else {
                continue;
            };
            note.left -= frames as i64;
            if note.left > 0 {
                continue;
            }
            let (track, pitch) = (note.track, note.pitch);
            *slot = None;
            if let Some(inst) = instrument_of(&mut self.snapshot, track) {
                inst.note_off(pitch);
            }
        }
    }

    /// Lets go of a preview that has finished, once the garbage queue has
    /// room for it.
    fn retire_preview(&mut self) {
        if !self.preview.as_ref().is_some_and(|v| v.done) {
            return;
        }
        self.shared.preview.store(0, Ordering::Relaxed);
        if self.pending_trash.is_some() {
            return;
        }
        if let Some(v) = self.preview.take() {
            self.discard(Garbage::Preview(v.preview));
        }
    }

    /// Renders `frames` (at most [`MAX_BLOCK`]) into the mix buffers.
    fn render_block(&mut self, frames: usize) {
        let frames = frames.min(MAX_BLOCK);
        let snap = &mut *self.snapshot;
        for t in snap.triggers.iter_mut() {
            t.clear();
        }
        let mut done = 0;
        while done < frames {
            let mut len = frames - done;
            let loop_range = snap.loop_range.filter(|(s, e)| e > s);
            if self.playing
                && let Some((_, le)) = loop_range
                && self.position < le
                && self.position + len as i64 > le
            {
                len = (le - self.position) as usize;
            }
            for (track, triggers) in snap.tracks.iter_mut().zip(snap.triggers.iter_mut()) {
                render_source(track, triggers, self.playing, self.position, done, len);
            }
            done += len;
            if self.playing {
                let before = self.position;
                self.position += len as i64;
                if let Some((ls, le)) = loop_range
                    && before < le
                    && self.position >= le
                {
                    self.position = ls;
                    for t in snap.tracks.iter_mut() {
                        release_all(t);
                        reseek(t, ls);
                    }
                }
            }
            if len == 0 {
                break;
            }
        }

        let [ml, mr] = &mut self.mix;
        let (Some(ml), Some(mr)) = (ml.get_mut(..frames), mr.get_mut(..frames)) else {
            return;
        };
        ml.fill(0.0);
        mr.fill(0.0);
        let k = self.smoothing;
        for track in snap.tracks.iter_mut() {
            let [bl, br] = &mut track.buf;
            let (Some(bl), Some(br)) = (bl.get_mut(..frames), br.get_mut(..frames)) else {
                continue;
            };
            feed_sidechains(&mut track.channel, &snap.triggers);
            let reductions = self.shared.reductions(Some(track.meter));
            let (pl, pr) = apply_channel(&mut track.channel, k, bl, br, reductions);
            if let Some(m) = self.shared.track_meter(track.meter) {
                m.record(pl, pr);
            }
            if let Some(tap) = self.shared.track_keys(track.meter) {
                match &track.source {
                    SourceNode::Instrument {
                        instrument: Some(inst),
                        ..
                    } => tap.record(inst.held_keys(), inst.pedal_down()),
                    _ => tap.record(0, false),
                }
            }
            for ((m, s), (n, t)) in ml
                .iter_mut()
                .zip(bl.iter())
                .zip(mr.iter_mut().zip(br.iter()))
            {
                *m += *s;
                *n += *t;
            }
        }
        feed_sidechains(&mut snap.master, &snap.triggers);
        let (pl, pr) = apply_channel(&mut snap.master, k, ml, mr, self.shared.reductions(None));
        self.shared.master.record(pl, pr);
        let (shared, playing) = (&self.shared, self.playing);
        let (tl, tr) = self.loudness.process(ml, mr, |power| {
            shared.loudness.push(LoudnessBlock { power, playing });
        });
        self.shared.master_true_peak.record(tl, tr);
        if let Some(v) = self.preview.as_mut() {
            v.render_add(ml, mr);
        }
        self.retire_preview();
        self.advance_live(frames);
    }

    fn publish(&self) {
        self.shared.playhead.store(self.position, Ordering::Relaxed);
        self.shared.playing.store(self.playing, Ordering::Relaxed);
    }

    /// Fills an interleaved device buffer. Channel 0 is left and channel 1
    /// right; extra channels are silent and a mono device gets the sum.
    pub fn process_interleaved(&mut self, out: &mut [f32], channels: usize) {
        let channels = channels.max(1);
        self.handle_messages();
        let total = out.len() / channels;
        let mut done = 0;
        while done < total {
            let n = (total - done).min(MAX_BLOCK);
            self.render_block(n);
            if let Some(chunk) = out.get_mut(done * channels..(done + n) * channels) {
                let [ml, mr] = &self.mix;
                for ((frame, l), r) in chunk
                    .chunks_exact_mut(channels)
                    .zip(ml.iter())
                    .zip(mr.iter())
                {
                    match frame {
                        [m] => *m = (l + r) * 0.5,
                        [a, b, rest @ ..] => {
                            *a = *l;
                            *b = *r;
                            rest.fill(0.0);
                        }
                        [] => {}
                    }
                }
            }
            done += n;
        }
        self.publish();
    }

    /// Fills planar stereo buffers of equal length.
    pub fn process_planar(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.handle_messages();
        let total = left.len().min(right.len());
        let mut done = 0;
        while done < total {
            let n = (total - done).min(MAX_BLOCK);
            self.render_block(n);
            let [ml, mr] = &self.mix;
            if let (Some(l), Some(r), Some(sl), Some(sr)) = (
                left.get_mut(done..done + n),
                right.get_mut(done..done + n),
                ml.get(..n),
                mr.get(..n),
            ) {
                l.copy_from_slice(sl);
                r.copy_from_slice(sr);
            }
            done += n;
        }
        self.publish();
    }
}
