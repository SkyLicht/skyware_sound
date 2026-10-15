// SPDX-License-Identifier: GPL-3.0-or-later
//! The audio callback path must never allocate, including snapshot swaps,
//! transport changes and loop wraps.

use std::sync::Arc;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use sky_core::{AudioId, ClipId};
use sky_dsp::{AudioData, EffectType, InstrumentType};
use sky_engine::{AudioCache, EngineMsg, compile, create_engine};
use sky_model::templates::demo_project;
use sky_model::{BankSample, Clip, ClipContent, EffectSlot, SampleBank, Swing, Track};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

#[test]
fn processing_never_allocates() {
    let mut project = demo_project();
    project.loop_region.end = project.ticks_per_bar();
    // Per-note parameter values, swing and a duck driven by the drums.
    let drums = project.tracks[0].id;
    if let ClipContent::Pattern {
        controls, swing, ..
    } = &mut project.tracks[0].clips[0].content
    {
        controls.insert("velocity".into(), "127 [0 60] 90?".into());
        controls.insert("kick_tune".into(), "<-12 0 7>".into());
        controls.insert("hat_decay".into(), "0.5 2 ~".into());
        *swing = Swing::new(60, 16);
    }
    project.tracks[1].channel.effects.push(
        EffectSlot::new(EffectType::Duck)
            .with_params(&[("attack", 0.0)])
            .with_sidechain(drums),
    );
    project.master.effects.insert(
        0,
        EffectSlot::new(EffectType::Duck)
            .with_params(&[("trigger", 1.0)])
            .with_sidechain(drums),
    );
    let mut cache = AudioCache::new();
    // A sample bank playing pattern sounds, so its one-shots run too.
    let id = AudioId::new();
    cache.insert(id, Arc::new(AudioData::silent(2, 4_800, 44_100)));
    let mut bank = Track::new_instrument("Bank", InstrumentType::Bank);
    bank.instrument.as_mut().unwrap().bank = Some(SampleBank {
        name: "Test".into(),
        sounds: [(
            "bd".to_string(),
            vec![BankSample {
                audio: id,
                root: None,
            }],
        )]
        .into(),
        pitched: None,
    });
    bank.clips.push(Clip {
        id: ClipId::new(),
        name: "p".into(),
        start: 0,
        length: project.ticks_per_bar(),
        content: ClipContent::pattern("bd*16", 0, project.ticks_per_bar()),
    });
    project.tracks.push(bank);
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
