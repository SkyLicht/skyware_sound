// SPDX-License-Identifier: GPL-3.0-or-later
//! The session end to end: changes, undo, the journal, saving with pool
//! audio, the live engine, and loop export.

use std::path::Path;

use sky_core::{ClipId, PPQ};
use sky_dsp::AudioData;
use sky_io::{WavFormat, write_wav};
use sky_model::templates::{demo_project, empty_project};
use sky_model::{Clip, ClipContent, Command, Origin, Project, Track, Transaction};
use sky_session::{LoopExportOptions, Session, SessionError};

fn mcp(tool: &str, commands: Vec<Command>) -> Transaction {
    Transaction::new(
        tool,
        Origin::Mcp {
            client: "test".into(),
            tool: tool.into(),
        },
        commands,
    )
}

fn sine_file(path: &Path, seconds: f64) {
    let sr = 48_000;
    let frames = (seconds * sr as f64) as usize;
    let tone: Vec<f32> = (0..frames)
        .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / sr as f32).sin() * 0.5)
        .collect();
    let audio = AudioData {
        sample_rate: sr,
        channels: vec![tone.clone(), tone],
    };
    write_wav(path, &audio, WavFormat::Pcm24, false).unwrap();
}

fn headless(project: Project) -> Session {
    Session::new(project, None).0
}

#[test]
fn apply_and_undo_through_the_session() {
    let session = headless(demo_project());
    let before = session.project();
    let rev = session.view().revision;
    session
        .apply(Transaction::single(Command::SetTempo { bpm: 90.0 }))
        .unwrap();
    let view = session.view();
    assert!(view.revision > rev);
    assert!(view.dirty);
    assert_eq!(view.project.tempo.initial_bpm(), 90.0);
    assert_eq!(view.undo_label.as_deref(), Some("Change tempo"));

    session.undo().unwrap();
    assert_eq!(*session.project(), *before);
    session.redo().unwrap();
    assert_eq!(session.project().tempo.initial_bpm(), 90.0);
}

#[test]
fn bad_changes_leave_the_project_alone() {
    let session = headless(demo_project());
    let before = session.project();
    let err = session
        .apply(Transaction::single(Command::SetTempo { bpm: 5000.0 }))
        .unwrap_err();
    assert!(matches!(err, SessionError::Model(_)));
    assert_eq!(*session.project(), *before);
    assert!(!session.view().dirty);
}

#[test]
fn mcp_changes_show_in_the_activity_list_and_undo_from_there() {
    let session = headless(demo_project());
    let first = session
        .apply(mcp("set_tempo", vec![Command::SetTempo { bpm: 100.0 }]))
        .unwrap();
    session
        .apply(Transaction::single(Command::SetProjectName {
            name: "Mine".into(),
        }))
        .unwrap();
    session
        .apply(mcp("set_tempo", vec![Command::SetTempo { bpm: 110.0 }]))
        .unwrap();
    let view = session.view();
    assert_eq!(view.activity.len(), 2);
    assert_eq!(view.activity[0].tool, "set_tempo");
    assert!(!view.activity[0].undone);

    // Undoing to the first AI change also undoes the user change after it.
    assert_eq!(session.undo_to(first).unwrap(), 3);
    let view = session.view();
    assert!(view.activity.iter().all(|a| a.undone));
    assert_eq!(view.project.tempo.initial_bpm(), 120.0);
    assert!(session.undo_to(first).is_err());
}

#[test]
fn save_and_open_keep_pool_audio() {
    let tmp = tempfile::tempdir().unwrap();
    let wav = tmp.path().join("tone.wav");
    sine_file(&wav, 0.5);

    let session = headless(empty_project("Pool"));
    let id = session.import_audio(&wav, Origin::User).unwrap();
    let track = Track::new_audio("Tone");
    let track_id = track.id;
    session
        .apply(Transaction::user(
            "Add tone",
            vec![
                Command::AddTrack {
                    track: Box::new(track),
                    index: None,
                },
                Command::AddClip {
                    track: track_id,
                    clip: Box::new(Clip {
                        id: ClipId::new(),
                        name: "Tone".into(),
                        start: 0,
                        length: PPQ,
                        content: ClipContent::Audio {
                            source: id,
                            offset: 0,
                            gain_db: 0.0,
                            fade_in: 0,
                            fade_out: 0,
                        },
                    }),
                },
            ],
        ))
        .unwrap();

    let dir = tmp.path().join("Pool.skyproj");
    session.save(Some(&dir)).unwrap();
    assert!(!session.view().dirty);
    let entry = &session.project().audio_pool[0];
    assert!(dir.join(&entry.file).is_file());

    let other = headless(empty_project("Other"));
    assert_eq!(other.open(&dir).unwrap(), 0);
    assert_eq!(*other.project(), *session.project());
    let audio = other.audio(id).expect("pool audio is loaded on open");
    assert_eq!(audio.frames(), 24_000);

    let rendered = other.render_range(0, PPQ, 0.0).unwrap();
    assert!(rendered.peak() > 0.4, "the audio clip plays");
}

#[test]
fn the_journal_recovers_unsaved_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Crash.skyproj");
    let session = headless(demo_project());
    session.save(Some(&dir)).unwrap();
    session
        .apply(Transaction::single(Command::SetTempo { bpm: 133.0 }))
        .unwrap();
    session
        .apply(Transaction::single(Command::SetProjectName {
            name: "After".into(),
        }))
        .unwrap();
    session.undo().unwrap();
    let expected = session.project();
    // "Crash": drop the session without saving.
    drop(session);

    let recovered = headless(empty_project("x"));
    assert_eq!(recovered.open(&dir).unwrap(), 3);
    assert_eq!(*recovered.project(), *expected);
    assert!(recovered.view().dirty);

    recovered.save(None).unwrap();
    let again = headless(empty_project("y"));
    assert_eq!(again.open(&dir).unwrap(), 0);
    assert_eq!(*again.project(), *expected);
}

#[test]
fn save_needs_a_folder() {
    let session = headless(demo_project());
    assert!(matches!(session.save(None), Err(SessionError::NoProjectPath)));
}

#[test]
fn live_engine_follows_changes_and_transport() {
    let (session, engine) = Session::new(demo_project(), Some(48_000));
    let mut engine = engine.unwrap();
    session.play().unwrap();
    let mut l = vec![0.0f32; 4_800];
    let mut r = vec![0.0f32; 4_800];
    engine.process_planar(&mut l, &mut r);
    let t = session.transport();
    assert!(t.running && t.playing);
    assert_eq!(t.position_samples, 4_800);
    assert!(l.iter().any(|s| s.abs() > 1e-4), "the demo is audible");

    session
        .apply(Transaction::single(Command::SetChannel {
            channel: sky_model::ChannelRef::Master,
            volume_db: None,
            pan: None,
            mute: Some(true),
            solo: None,
        }))
        .unwrap();
    // Let the master gain smoothing settle, then check for silence.
    for _ in 0..4 {
        engine.process_planar(&mut l, &mut r);
    }
    engine.process_planar(&mut l, &mut r);
    assert!(l.iter().all(|s| s.abs() < 1e-4), "muting the master silences it");

    session.stop().unwrap();
    engine.process_planar(&mut l, &mut r);
    assert!(!session.transport().playing);
    session.seek(PPQ * 4).unwrap();
    engine.process_planar(&mut l[..1], &mut r[..1]);
    assert_eq!(engine.position(), 96_000);
}

#[test]
fn transport_without_an_engine_says_so() {
    let session = headless(demo_project());
    assert!(matches!(session.play(), Err(SessionError::NoEngine)));
    assert!(!session.transport().running);
}

#[test]
fn render_loop_is_the_loop_length() {
    let session = headless(demo_project());
    let audio = session.render_loop(None).unwrap();
    // Four bars of 4/4 at 120 BPM is eight seconds.
    assert_eq!(audio.frames(), 8 * 48_000);
    assert!(audio.peak() > 0.01);
}

#[test]
fn export_loop_writes_a_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let session = headless(demo_project());
    let out = session
        .export_loop(&LoopExportOptions {
            name: Some("Menu Theme".into()),
            dir: Some(tmp.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();
    assert!(out.exported.file.is_file());
    assert!(out.exported.manifest.is_file());
    assert_eq!(out.bars, 4.0);
    assert_eq!(out.seam.verdict, "seamless", "{:?}", out.seam);

    let manifest = sky_game::read_manifest(tmp.path()).unwrap().unwrap();
    assert!(manifest.find("menu_theme").is_some());

    // Asset names are file-safe identifiers the game refers to.
    // The game runtime loads the export and plays it.
    let mut rt = sky_runtime::Runtime::new(48_000);
    rt.load_manifest(tmp.path()).unwrap();
    assert!(rt.warnings().is_empty(), "{:?}", rt.warnings());
    rt.play_by_name("menu_theme", Default::default()).unwrap();
    let mut buf = vec![0.0f32; 2 * 48_000];
    rt.render(&mut buf);
    assert!(buf.iter().any(|s| s.abs() > 0.01));
}
