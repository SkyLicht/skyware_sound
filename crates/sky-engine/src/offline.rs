// SPDX-License-Identifier: GPL-3.0-or-later
//! Offline rendering: the same engine, without a device, faster than real time.

use sky_dsp::AudioData;

use crate::compile::EngineSnapshot;
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
    engine.process_planar(body_l, body_r);
    let _ = engine.snapshot_mut();
    AudioData {
        sample_rate,
        channels: vec![left, right],
    }
}
