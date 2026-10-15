// SPDX-License-Identifier: GPL-3.0-or-later
//! Auditioning: live notes on a track's instrument and the preview player,
//! with the transport stopped.

use std::sync::Arc;

use sky_core::{AudioId, TrackId};
use sky_dsp::{AudioData, InstrumentType};
use sky_engine::{AudioCache, EngineMsg, Preview, compile, create_engine};
use sky_model::{BankSample, Project, SampleBank, Track};

const SR: u32 = 48_000;

fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

fn mono(samples: Vec<f32>, sample_rate: u32) -> AudioData {
    AudioData {
        sample_rate,
        channels: vec![samples],
    }
}

fn synth_project() -> (Project, TrackId) {
    let mut p = Project::new("t");
    p.loop_enabled = false;
    let t = Track::new_instrument("Synth", InstrumentType::Synth);
    let id = t.id;
    p.tracks.push(t);
    (p, id)
}

/// Renders `frames` frames and returns the left channel.
fn run(engine: &mut sky_engine::Engine, frames: usize) -> Vec<f32> {
    let mut l = vec![0.0; frames];
    let mut r = vec![0.0; frames];
    engine.process_planar(&mut l, &mut r);
    l
}

#[test]
fn live_notes_sound_while_stopped_and_timed_ones_end() {
    let (p, track) = synth_project();
    let (mut handle, mut engine) = create_engine(compile(&p, &AudioCache::new(), SR, 1));
    assert_eq!(peak(&run(&mut engine, 4_800)), 0.0);

    // A held note keeps sounding until it is let go.
    handle.send(EngineMsg::NoteOn {
        track,
        pitch: 60,
        velocity: 100,
        frames: None,
    });
    run(&mut engine, SR as usize);
    assert!(peak(&run(&mut engine, 4_800)) > 0.05, "held note stopped");
    handle.send(EngineMsg::NoteOff { track, pitch: 60 });
    run(&mut engine, SR as usize);
    assert!(
        peak(&run(&mut engine, 4_800)) < 1e-3,
        "released note rang on"
    );

    // A timed note ends by itself after its 100 ms and its release.
    handle.send(EngineMsg::NoteOn {
        track,
        pitch: 64,
        velocity: 100,
        frames: Some(SR / 10),
    });
    assert!(peak(&run(&mut engine, 4_800)) > 0.05);
    run(&mut engine, SR as usize);
    assert!(
        peak(&run(&mut engine, 4_800)) < 1e-3,
        "timed note did not end"
    );

    // A track that is gone, or has no instrument, is ignored.
    handle.send(EngineMsg::NoteOn {
        track: TrackId::new(),
        pitch: 60,
        velocity: 100,
        frames: Some(100),
    });
    assert_eq!(peak(&run(&mut engine, 4_800)), 0.0);
}

#[test]
fn muted_tracks_stay_silent_when_auditioned() {
    let (mut p, track) = synth_project();
    p.tracks[0].channel.mute = true;
    let (mut handle, mut engine) = create_engine(compile(&p, &AudioCache::new(), SR, 1));
    handle.send(EngineMsg::NoteOn {
        track,
        pitch: 60,
        velocity: 100,
        frames: None,
    });
    assert_eq!(peak(&run(&mut engine, 4_800)), 0.0);
}

#[test]
fn bank_sounds_play_by_number() {
    let mut p = Project::new("t");
    let id = AudioId::new();
    let mut cache = AudioCache::new();
    cache.insert(id, Arc::new(mono(vec![0.5; 4_800], SR)));
    let mut t = Track::new_instrument("Bank", InstrumentType::Bank);
    t.instrument.as_mut().unwrap().bank = Some(SampleBank {
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
    let track = t.id;
    p.tracks.push(t);
    let (mut handle, mut engine) = create_engine(compile(&p, &cache, SR, 1));
    handle.send(EngineMsg::SoundOn {
        track,
        sound: 0,
        velocity: 127,
    });
    assert!(peak(&run(&mut engine, 2_400)) > 0.1);
}

fn preview(id: u64, audio: AudioData, loop_start: Option<f64>) -> Box<Preview> {
    Box::new(Preview {
        id,
        rate: f64::from(audio.sample_rate) / f64::from(SR),
        audio: Arc::new(audio),
        start: 0.0,
        loop_start,
        gain: 1.0,
    })
}

#[test]
fn previews_play_after_the_master_and_are_freed() {
    // A muted master silences the project, not the preview.
    let mut p = Project::new("t");
    p.master.mute = true;
    let (mut handle, mut engine) = create_engine(compile(&p, &AudioCache::new(), SR, 1));
    let shared = handle.shared().clone();

    handle.send(EngineMsg::Preview(preview(
        7,
        mono(vec![0.25; 1_000], SR),
        None,
    )));
    let out = run(&mut engine, 2_000);
    assert_eq!(out[0], 0.25);
    assert_eq!(out[999], 0.25);
    assert_eq!(out[1_000], 0.0);
    // It ended by itself: nothing plays and it went back to be freed.
    assert_eq!(shared.preview(), None);
    assert_eq!(handle.collect_garbage(), 1);

    // A loop plays until stopped, at the device's rate: 24 kHz audio plays
    // at half speed against 48 kHz.
    handle.send(EngineMsg::Preview(preview(
        8,
        mono(vec![0.5; 100], SR / 2),
        Some(0.0),
    )));
    let out = run(&mut engine, 10_000);
    assert!(out.iter().all(|s| *s == 0.5));
    assert_eq!(shared.preview(), Some(8));
    handle.send(EngineMsg::StopPreview);
    let out = run(&mut engine, 2_000);
    assert!(out[0] > 0.0 && out[0] < 0.5, "no fade out: {}", out[0]);
    assert_eq!(peak(&out[500..]), 0.0);
    assert_eq!(shared.preview(), None);
    assert_eq!(handle.collect_garbage(), 1);

    // A new preview replaces the one playing.
    handle.send(EngineMsg::Preview(preview(
        9,
        mono(vec![0.5; 100], SR),
        Some(0.0),
    )));
    run(&mut engine, 64);
    handle.send(EngineMsg::Preview(preview(
        10,
        mono(vec![0.125; 100], SR),
        Some(0.0),
    )));
    let out = run(&mut engine, 64);
    assert_eq!(out[0], 0.125);
    assert_eq!(shared.preview(), Some(10));
    assert_eq!(handle.collect_garbage(), 1);
}
