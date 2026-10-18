// SPDX-License-Identifier: GPL-3.0-or-later
//! The audio callback path must never allocate, including snapshot swaps,
//! transport changes, loop wraps, live notes, previews and the sustain
//! pedal.

use std::sync::Arc;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use sky_core::{AudioId, ClipId, PPQ};
use sky_dsp::instruments::{SampleData, SampleLibrary, Zone};
use sky_dsp::{AudioData, EffectType, InstrumentType};
use sky_engine::{AudioCache, EngineMsg, LibraryCache, Preview, compile_with, create_engine};
use sky_model::templates::demo_project;
use sky_model::{BankSample, Clip, ClipContent, EffectSlot, Note, Pedal, SampleBank, Swing, Track};

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
        packs: Vec::new(),
    });
    bank.clips.push(Clip {
        id: ClipId::new(),
        name: "p".into(),
        start: 0,
        length: project.ticks_per_bar(),
        muted: false,
        content: ClipContent::pattern("bd*16", 0, project.ticks_per_bar()),
    });
    let bank_id = bank.id;
    project.tracks.push(bank);
    let synth = project.tracks[1].id;
    let preview = |id: u64, loop_start: Option<f64>| {
        EngineMsg::Preview(Box::new(Preview {
            id,
            audio: Arc::new(AudioData::silent(2, 9_600, 44_100)),
            start: 100.0,
            loop_start,
            rate: 44_100.0 / 48_000.0,
            gain: 0.5,
        }))
    };
    // Step clips with locks, ratchets and slides, on drums and the Acid.
    let bar = project.ticks_per_bar();
    let mut beat = sky_model::StepLane::parse(42, "x*4 x?50 x@1:2 x>20").unwrap();
    sky_model::steps::parse_locks(&mut beat.steps, "hat_decay", "0.5 . 2 .").unwrap();
    project.tracks[0].clips.push(Clip {
        id: ClipId::new(),
        name: "s".into(),
        start: 0,
        length: bar,
        muted: false,
        content: ClipContent::Steps(sky_model::StepClip::new(16, vec![beat])),
    });
    let mut acid = Track::new_instrument("Acid", InstrumentType::Acid);
    let mut line = sky_model::StepLane::parse(36, "c2/ g2! - c3/ . c2*3 d#2 x").unwrap();
    sky_model::steps::parse_locks(&mut line.steps, "cutoff", "200 . . 2000 . . . .").unwrap();
    acid.clips.push(Clip {
        id: ClipId::new(),
        name: "a".into(),
        start: 0,
        length: bar,
        muted: false,
        content: ClipContent::Steps(sky_model::StepClip::new(16, vec![line])),
    });
    project.tracks.push(acid);
    // A piano with the pedal going down and up in a MIDI clip and a pattern.
    let libraries: LibraryCache = [(
        String::new(),
        Arc::new(SampleLibrary::new(
            "test",
            vec![SampleData::from_audio(&AudioData::silent(2, 9_600, 48_000))],
            vec![Zone::new(0, 60)],
        )),
    )]
    .into();
    let mut piano = Track::new_instrument("Piano", InstrumentType::Piano);
    piano.clips.push(Clip {
        id: ClipId::new(),
        name: "m".into(),
        start: 0,
        length: bar,
        muted: false,
        content: ClipContent::Midi {
            notes: (0..8)
                .map(|i| Note::new(i * PPQ / 2, PPQ / 4, 60 + i as u8, 100))
                .collect(),
            pedal: vec![Pedal::new(0, PPQ), Pedal::new(PPQ * 2, PPQ / 2)],
        },
    });
    let mut pattern = ClipContent::pattern("c4*8", 0, bar);
    if let ClipContent::Pattern { controls, .. } = &mut pattern {
        controls.insert("sustain".into(), "1 0 1 0".into());
    }
    piano.clips.push(Clip {
        id: ClipId::new(),
        name: "p".into(),
        start: bar / 2,
        length: bar,
        muted: false,
        content: pattern,
    });
    let piano_id = piano.id;
    project.tracks.push(piano);
    let compile = |project: &sky_model::Project, cache: &AudioCache, sr: u32, version: u64| {
        compile_with(project, cache, &libraries, sr, version)
    };
    let (mut handle, mut engine) = create_engine(compile(&project, &cache, 48_000, 1));
    let mut out = vec![0.0f32; 2 * 1_024];
    handle.send(EngineMsg::Play);
    for i in 0..200u64 {
        match i {
            5 => handle.send(EngineMsg::NoteOn {
                track: synth,
                pitch: 72,
                velocity: 90,
                frames: None,
            }),
            // More timed notes than there are slots for them.
            6 => {
                for pitch in 30..70 {
                    handle.send(EngineMsg::NoteOn {
                        track: synth,
                        pitch,
                        velocity: 90,
                        frames: Some(3_000),
                    });
                }
            }
            25 => handle.send(EngineMsg::NoteOff {
                track: synth,
                pitch: 72,
            }),
            // The pedal held while keys are played to hear them.
            26 => handle.send(EngineMsg::Controller {
                track: piano_id,
                controller: sky_dsp::SUSTAIN_PEDAL,
                value: 127,
            }),
            27 => handle.send(EngineMsg::NoteOn {
                track: piano_id,
                pitch: 40,
                velocity: 90,
                frames: Some(500),
            }),
            30 => handle.send(EngineMsg::SoundOn {
                track: bank_id,
                sound: 0,
                velocity: 127,
            }),
            40 => handle.send(preview(1, None)),
            45 => handle.send(preview(2, Some(0.0))),
            60 => handle.send(preview(3, Some(4_000.0))),
            100 => handle.send(EngineMsg::StopPreview),
            120 => handle.send(preview(4, None)),
            130 => handle.send(EngineMsg::AllSoundOff),
            _ => {}
        }
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
