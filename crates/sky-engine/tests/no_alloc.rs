// SPDX-License-Identifier: GPL-3.0-or-later
//! The audio callback path must never allocate, including snapshot swaps,
//! transport changes and loop wraps.

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use sky_engine::{AudioCache, EngineMsg, compile, create_engine};
use sky_model::templates::demo_project;

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

#[test]
fn processing_never_allocates() {
    let mut project = demo_project();
    project.loop_region.end = project.ticks_per_bar();
    let cache = AudioCache::new();
    let (mut handle, mut engine) = create_engine(compile(&project, &cache, 48_000, 1));
    let mut out = vec![0.0f32; 2 * 1_024];
    handle.send(EngineMsg::Play);
    for i in 0..200u64 {
        if i % 20 == 10 {
            project.tracks[1].channel.volume_db = -(i as f32 % 12.0);
            handle.send(EngineMsg::Swap(Box::new(compile(
                &project, &cache, 48_000, i,
            ))));
        }
        if i == 150 {
            handle.send(EngineMsg::Seek(10_000));
        }
        if i == 180 {
            handle.send(EngineMsg::Stop);
        }
        assert_no_alloc(|| engine.process_interleaved(&mut out, 2));
        handle.collect_garbage();
    }
}
