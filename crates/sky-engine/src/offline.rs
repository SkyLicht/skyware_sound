// SPDX-License-Identifier: GPL-3.0-or-later
//! Offline rendering: the same engine, without a device, faster than real time.

use sky_dsp::AudioData;

use crate::compile::{EngineSnapshot, SourceNode};
use crate::engine::create_engine;

/// Renders `frames` stereo frames starting at sample `start`. When `looping`
/// is false the snapshot's loop region is ignored.
pub fn render(mut snapshot: EngineSnapshot, start: i64, frames: usize, looping: bool) -> AudioData {
    if !looping {
        snapshot.loop_range = None;
    }
    let sample_rate = snapshot.sample_rate;
    let (_handle, mut engine) = create_engine(snapshot);
    crate::enable_flush_to_zero();
    engine.start_at(start);
    let mut left = vec![0.0f32; frames];
    let mut right = vec![0.0f32; frames];
    engine.process_planar(&mut left, &mut right);
    AudioData {
        sample_rate,
        channels: vec![left, right],
    }
}

/// Renders what starts in `[start, end)` as a one-shot sound, `frames`
/// long, so it can ring out past `end`: notes and clips that start at or
/// after `end` are left out, notes still held at `end` are released there,
/// and audio clips still playing fade out over 10 ms. Releases and effect
/// tails carry on into the frames after `end`.
pub fn render_one_shot(
    mut snapshot: EngineSnapshot,
    start: i64,
    end: i64,
    frames: usize,
) -> AudioData {
    let fade = i64::from(snapshot.sample_rate / 100).max(1);
    for track in snapshot.tracks.iter_mut() {
        match &mut track.source {
            SourceNode::Instrument { notes, .. } => {
                notes.retain(|n| n.time < end);
                for n in notes.iter_mut() {
                    n.end = n.end.min(end);
                }
            }
            SourceNode::Audio(clips) => {
                clips.retain(|c| c.start < end);
                for c in clips.iter_mut().filter(|c| c.end > end) {
                    c.end = end;
                    c.fade_out = c.fade_out.max(fade).min(end - c.start);
                }
            }
        }
    }
    render(snapshot, start, frames, false)
}

/// Renders the loop `[loop_start, loop_end)` `passes` times with the
/// transport looping, and keeps only the last pass. Reverb and delay tails
/// and released notes from the end of one pass are then already in the start
/// of the next, so the result repeats with no gap and no click.
pub fn render_loop(
    mut snapshot: EngineSnapshot,
    loop_start: i64,
    loop_end: i64,
    passes: usize,
) -> AudioData {
    let len = (loop_end - loop_start).max(0) as usize;
    let passes = passes.max(1);
    snapshot.loop_range = Some((loop_start, loop_end));
    let sample_rate = snapshot.sample_rate;
    let (_handle, mut engine) = create_engine(snapshot);
    crate::enable_flush_to_zero();
    engine.start_at(loop_start);
    let mut left = vec![0.0f32; len];
    let mut right = vec![0.0f32; len];
    for _ in 0..passes {
        engine.process_planar(&mut left, &mut right);
    }
    let _ = engine.snapshot_mut();
    AudioData {
        sample_rate,
        channels: vec![left, right],
    }
}

/// Renders an intro that plays once followed by the loop body: plays from
/// `intro_start` into the loop `[loop_start, loop_end)` and around it once
/// more, then keeps the intro from the first time through and the body from
/// the second. The body repeats seamlessly, as with [`render_loop`]; the
/// result is `loop_start - intro_start` intro frames followed by the body.
///
/// The body is what follows its own end, so a note held from the intro into
/// the body is not in it. When the two differ, the last 20 ms of the intro
/// cross-fade into the end of the body, so entering the body from the intro
/// is as smooth as the loop point instead of a cut.
pub fn render_intro_loop(
    mut snapshot: EngineSnapshot,
    intro_start: i64,
    loop_start: i64,
    loop_end: i64,
) -> AudioData {
    let intro_start = intro_start.clamp(0, loop_start.max(0));
    let intro = (loop_start - intro_start).max(0) as usize;
    let body = (loop_end - loop_start).max(0) as usize;
    snapshot.loop_range = Some((loop_start, loop_end));
    let sample_rate = snapshot.sample_rate;
    let (_handle, mut engine) = create_engine(snapshot);
    crate::enable_flush_to_zero();
    engine.start_at(intro_start);
    let mut left = vec![0.0f32; intro + body];
    let mut right = vec![0.0f32; intro + body];
    let (intro_l, body_l) = left.split_at_mut(intro);
    let (intro_r, body_r) = right.split_at_mut(intro);
    engine.process_planar(intro_l, intro_r);
    // The first time through the body, then the second, which is kept.
    engine.process_planar(body_l, body_r);
    let fade = (sample_rate as usize / 50).min(intro).min(body);
    let head = |ch: &[f32]| ch.iter().take(fade).copied().collect::<Vec<_>>();
    let (entry_l, entry_r) = (head(body_l), head(body_r));
    engine.process_planar(body_l, body_r);
    let _ = engine.snapshot_mut();
    let differs =
        |entry: &[f32], kept: &[f32]| entry.iter().zip(kept).any(|(a, b)| (a - b).abs() > 1e-6);
    if differs(&entry_l, body_l) || differs(&entry_r, body_r) {
        for (intro, body) in [(&mut *intro_l, &*body_l), (&mut *intro_r, &*body_r)] {
            let skip = intro.len() - fade;
            let tail = intro.iter_mut().skip(skip);
            let end = body.iter().skip(body.len() - fade);
            for (k, (x, b)) in tail.zip(end).enumerate() {
                let w = (k + 1) as f32 / fade as f32;
                *x += (b - *x) * w;
            }
        }
    }
    AudioData {
        sample_rate,
        channels: vec![left, right],
    }
}
