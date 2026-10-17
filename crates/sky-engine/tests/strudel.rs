// SPDX-License-Identifier: GPL-3.0-or-later
//! Strudel code in pattern clips: what its notes, gains, sounds and
//! controls become on each instrument.

use std::collections::BTreeMap;
use std::sync::Arc;

use sky_core::{AudioId, ClipId, PPQ};
use sky_dsp::{AudioData, InstrumentType};
use sky_engine::compile::{NoteEvent, SourceNode};
use sky_engine::{AudioCache, compile};
use sky_model::*;

const SR: u32 = 48_000;

fn clip(content: ClipContent) -> Clip {
    Clip {
        id: ClipId::new(),
        name: "p".into(),
        start: 0,
        length: PPQ * 4,
        muted: false,
        content,
    }
}

fn project(track: Track) -> Project {
    let mut p = Project::new("t");
    p.loop_enabled = false;
    p.tracks.push(track);
    p
}

fn track(kind: InstrumentType, content: ClipContent) -> Track {
    let mut t = Track::new_instrument("t", kind);
    t.clips.push(clip(content));
    t
}

/// Each note, with its parameter values by name.
fn notes(p: &Project, audio: &AudioCache) -> Vec<(NoteEvent, BTreeMap<&'static str, f32>)> {
    let snap = compile(p, audio, SR, 1);
    let SourceNode::Instrument { notes, locks, .. } = &snap.tracks[0].source else {
        panic!("an instrument track");
    };
    notes
        .iter()
        .map(|n| {
            let params = locks[n.lock_start as usize..n.lock_end as usize]
                .iter()
                .map(|l| (l.param, l.value))
                .collect();
            (*n, params)
        })
        .collect()
}

fn code(kind: InstrumentType, src: &str) -> Vec<(NoteEvent, BTreeMap<&'static str, f32>)> {
    notes(
        &project(track(kind, ClipContent::pattern(src, 0, PPQ * 4))),
        &AudioCache::new(),
    )
}

#[test]
fn notes_and_controls_play_on_a_synth() {
    let out = code(
        InstrumentType::Synth,
        r#"note("c3 e3").s("sawtooth").lpf(800).resonance(10).attack(0.01).release(.5)"#,
    );
    assert_eq!(out.len(), 2);
    assert_eq!((out[0].0.pitch, out[1].0.pitch), (48, 52));
    assert_eq!(out[0].0.sound, None);
    assert_eq!(out[0].0.velocity, 100);
    let params = &out[0].1;
    assert_eq!(params["wave"], 0.0);
    assert_eq!(params["cutoff"], 800.0);
    assert!((params["resonance"] - 0.4).abs() < 1e-6);
    assert!((params["attack"] - 10.0).abs() < 1e-3);
    assert!((params["release"] - 500.0).abs() < 1e-3);
    // What the code leaves alone keeps the track's value.
    assert!(!params.contains_key("detune"));
}

#[test]
fn waveform_sounds_pick_the_wave() {
    let out = code(InstrumentType::Synth, r#"s("square triangle sine saw")"#);
    let waves: Vec<f32> = out.iter().map(|(_, p)| p["wave"]).collect();
    assert_eq!(waves, [1.0, 2.0, 3.0, 0.0]);
    // Without a note, Strudel's synths play C2.
    assert!(out.iter().all(|(n, _)| n.pitch == 36));
    // The acid has only saw and square: a triangle keeps its usual wave.
    let out = code(
        InstrumentType::Acid,
        r#"note("c2").s("<square triangle>").slow(1)"#,
    );
    assert_eq!(out[0].1["wave"], 1.0);
}

#[test]
fn gain_and_velocity_set_how_hard_notes_play() {
    let velocities = |src: &str| -> Vec<u8> {
        code(InstrumentType::Synth, src)
            .into_iter()
            .map(|(n, _)| n.velocity)
            .collect()
    };
    assert_eq!(velocities(r#"note("c3")"#), [100]);
    // Strudel's gain is 0.8 unless given, so 0.4 is half as loud.
    assert_eq!(
        velocities(r#"note("c3*4").gain("0.4 0.8 1 2")"#),
        [50, 100, 125, 127]
    );
    assert_eq!(velocities(r#"note("c3").velocity(0.5)"#), [50]);
    assert_eq!(velocities(r#"note("c3").gain(1).velocity(0.5)"#), [63]);
    // Gain 0 is no note at all.
    assert_eq!(velocities(r#"note("c3 e3").gain("0 1")"#), [125]);
    assert_eq!(velocities(r#"note("c3").echo(3, 1/8, 0.5)"#), [100, 50, 25]);
}

#[test]
fn code_wins_over_the_clips_control_lanes() {
    let mut controls = BTreeMap::new();
    controls.insert("cutoff".to_string(), "400".to_string());
    controls.insert("velocity".to_string(), "60".to_string());
    let content = |src: &str| ClipContent::Pattern {
        source: src.into(),
        seed: 0,
        cycle: PPQ * 4,
        controls: controls.clone(),
        swing: Swing::default(),
    };
    let p = project(track(
        InstrumentType::Synth,
        content(r#"note("c3 e3").lpf("<800 ~>").gain("1 ~")"#),
    ));
    let out = notes(&p, &AudioCache::new());
    assert_eq!(out[0].1["cutoff"], 800.0);
    assert_eq!(out[0].0.velocity, 125);
    // Where the code sets nothing, the lanes still do.
    let p = project(track(InstrumentType::Synth, content(r#"note("c3")"#)));
    let out = notes(&p, &AudioCache::new());
    assert_eq!(out[0].1["cutoff"], 400.0);
    assert_eq!(out[0].0.velocity, 60);
}

#[test]
fn numbers_from_n_play_as_notes() {
    let out = code(InstrumentType::Synth, r#"n("0 2 4").scale("C:major")"#);
    let pitches: Vec<u8> = out.iter().map(|(n, _)| n.pitch).collect();
    assert_eq!(pitches, [48, 52, 55]);
    let out = code(InstrumentType::Synth, r#"n("60 62")"#);
    assert_eq!(out[1].0.pitch, 62);
}

#[test]
fn the_every_example_plays_as_written() {
    let out = code(
        InstrumentType::Drums,
        r#"s("bd*2 sd").fast(2).every(4, rev)"#,
    );
    let pitches: Vec<u8> = out.iter().map(|(n, _)| n.pitch).collect();
    // The first cycle is reversed: sd bd bd sd bd bd.
    assert_eq!(pitches, [38, 36, 36, 38, 36, 36]);
}

/// A bank with `bd` (no recorded note) and a `piano` recorded at C4 and C5.
fn bank(src: &str) -> (Project, AudioCache) {
    let mut p = Project::new("bank");
    p.loop_enabled = false;
    let mut audio = AudioCache::new();
    let mut sample = |p: &mut Project, root: Option<u8>| {
        let id = AudioId::new();
        audio.insert(
            id,
            Arc::new(AudioData {
                sample_rate: SR,
                channels: vec![vec![0.5; 480]],
            }),
        );
        p.audio_pool.push(PoolEntry {
            id,
            name: "dc".into(),
            file: format!("audio/{id}.wav"),
            channels: 1,
            sample_rate: SR,
            frames: 480,
            source: None,
        });
        BankSample { audio: id, root }
    };
    let mut sounds = BTreeMap::new();
    sounds.insert("bd".to_string(), vec![sample(&mut p, None)]);
    sounds.insert(
        "piano".to_string(),
        vec![sample(&mut p, Some(60)), sample(&mut p, Some(72))],
    );
    let mut t = Track::new_instrument("Bank", InstrumentType::Bank);
    t.instrument.as_mut().unwrap().bank = Some(SampleBank {
        name: "Test".into(),
        sounds,
        pitched: None,
        packs: Vec::new(),
    });
    t.clips.push(clip(ClipContent::pattern(src, 0, PPQ * 4)));
    p.tracks.push(t);
    (p, audio)
}

fn bank_notes(src: &str) -> Vec<(Option<u32>, Option<f32>)> {
    let (p, audio) = bank(src);
    notes(&p, &audio)
        .into_iter()
        .map(|(n, params)| (n.sound, params.get("tune").copied()))
        .collect()
}

#[test]
fn notes_pick_the_nearest_recording_and_tune_it() {
    // Samples 1 and 2 are the piano's C4 and C5.
    assert_eq!(
        bank_notes(r#"note("c4 d4 c5 b4").s("piano")"#),
        [
            (Some(1), Some(0.0)),
            (Some(1), Some(2.0)),
            (Some(2), Some(0.0)),
            (Some(2), Some(-1.0)),
        ]
    );
    // A sample with no recorded note is C2, as in Strudel.
    assert_eq!(bank_notes(r#"s("bd").note("c3")"#), [(Some(0), Some(12.0))]);
    assert_eq!(bank_notes(r#"s("bd").speed(2)"#), [(Some(0), Some(12.0))]);
    assert_eq!(bank_notes(r#"s("bd")"#), [(Some(0), None)]);
}

#[test]
fn a_banks_sounds_fall_back_to_their_plain_names() {
    let (p, audio) = bank(r#"s("bd").bank("RolandTR909")"#);
    let out = notes(&p, &audio);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0.sound, Some(0));
    // It is still a kick for anything listening for one.
    assert_eq!(out[0].0.pitch, 36);
}
