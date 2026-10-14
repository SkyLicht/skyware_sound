// SPDX-License-Identifier: GPL-3.0-or-later
//! The session end to end: changes, undo, the journal, saving with pool
//! audio, the live engine, and loop export.

use std::path::Path;

use sky_core::manifest::{Asset, Category, RandomRange, Selection};
use sky_core::{AssetId, ClipId, PPQ};
use sky_dsp::AudioData;
use sky_io::{WavFormat, write_wav};
use sky_model::templates::{demo_project, empty_project};
use sky_model::{
    Clip, ClipContent, Command, GameAsset, GameAssetKind, Origin, Project, TimeRange, Track,
    Transaction,
};
use sky_session::{GameExportOptions, LoopExportOptions, Session, SessionError};

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
fn save_copy_leaves_the_session_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let session = headless(demo_project());
    session
        .apply(Transaction::single(Command::SetTempo { bpm: 99.0 }))
        .unwrap();
    let copy = tmp.path().join("autosave.skyproj");
    session.save_copy(&copy).unwrap();
    let view = session.view();
    assert!(view.dirty && view.path.is_none());
    let other = headless(empty_project("x"));
    other.open(&copy).unwrap();
    assert_eq!(*other.project(), *session.project());
}

#[test]
fn save_needs_a_folder() {
    let session = headless(demo_project());
    assert!(matches!(
        session.save(None),
        Err(SessionError::NoProjectPath)
    ));
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
    assert!(
        l.iter().all(|s| s.abs() < 1e-4),
        "muting the master silences it"
    );

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

fn game_asset(name: &str, category: Category, kind: GameAssetKind) -> GameAsset {
    GameAsset {
        id: AssetId::new(),
        name: name.into(),
        category,
        tags: vec![],
        kind,
    }
}

#[test]
fn export_game_writes_loops_with_intros_and_sfx_sets() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = demo_project();
    let bar = project.ticks_per_bar();
    let battle = game_asset(
        "battle",
        Category::Music,
        GameAssetKind::Loop {
            range: TimeRange::new(bar, bar * 4).unwrap(),
            intro_start: Some(0),
        },
    );
    let mut hits = game_asset(
        "hit",
        Category::Sfx,
        GameAssetKind::sfx(TimeRange::new(0, bar).unwrap().split(4)),
    );
    if let GameAssetKind::Sfx {
        selection,
        pitch_semitones,
        tail_seconds,
        mono,
        ..
    } = &mut hits.kind
    {
        *selection = Selection::RoundRobin;
        *pitch_semitones = RandomRange::new(-1.0, 1.0);
        *tail_seconds = 0.25;
        *mono = true;
    }
    project.game.assets = vec![battle.clone(), hits.clone()];
    project.game.export_dir = Some("game/audio".into());
    let session = headless(project);

    // An unsaved project has no folder for a relative export folder.
    assert!(session.game_export_dir().is_err());
    session
        .save(Some(&tmp.path().join("Game.skyproj")))
        .unwrap();
    let dir = session.game_export_dir().unwrap();
    assert_eq!(dir, tmp.path().join("Game.skyproj").join("game/audio"));

    let out = session.export_game(&GameExportOptions::default()).unwrap();
    assert_eq!(out.dir, dir);
    assert_eq!(out.files.len(), 5);
    let beat = 24_000;
    let Asset::Loop(l) = &out.assets[0].asset else {
        panic!()
    };
    // Bar 1 plays once, bars 2 to 4 repeat.
    assert_eq!(l.id, battle.id);
    assert_eq!((l.loop_start, l.loop_end), (4 * beat, 16 * beat));
    assert_eq!(l.bars, Some(3.0));
    let seam = out.assets[0].seam.as_ref().unwrap();
    assert_eq!(seam.verdict, "seamless", "{seam:?}");
    let Asset::Sfx(s) = &out.assets[1].asset else {
        panic!()
    };
    assert_eq!(s.variations.len(), 4);
    assert_eq!(s.selection, Selection::RoundRobin);
    for v in &s.variations {
        assert_eq!(v.channels, 1);
        // A beat plus at most the tail, with trailing silence cut.
        assert!(v.length_samples <= beat + 12_000, "{v:?}");
        assert!(dir.join(&v.path).is_file());
    }
    assert_eq!(s.variations[0].path, "sfx/hit_01.wav");

    // The game runtime plays both without warnings.
    let mut rt = sky_runtime::Runtime::new(48_000);
    rt.load_manifest(&dir).unwrap();
    assert!(rt.warnings().is_empty(), "{:?}", rt.warnings());
    rt.play_by_name("battle", Default::default()).unwrap();
    rt.play_by_name("hit", Default::default()).unwrap();
    let mut buf = vec![0.0f32; 2 * 48_000];
    rt.render(&mut buf);
    assert!(buf.iter().any(|x| x.abs() > 0.01));

    // Exporting one asset leaves the others alone.
    let only = session
        .export_game(&GameExportOptions {
            only: vec!["hit".into()],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(only.assets.len(), 1);
    assert!(only.removed.is_empty());
    assert!(
        session
            .export_game(&GameExportOptions {
                only: vec!["nope".into()],
                ..Default::default()
            })
            .is_err()
    );

    // Dropping the SFX set from the project removes it from the game folder.
    session
        .apply(mcp(
            "game_asset_remove",
            vec![Command::RemoveGameAsset { id: hits.id }],
        ))
        .unwrap();
    let out = session.export_game(&GameExportOptions::default()).unwrap();
    assert_eq!(out.removed, ["hit"]);
    assert_eq!(out.deleted.len(), 4);
    assert!(!dir.join("sfx/hit_01.wav").exists());
    let manifest = sky_game::read_manifest(&dir).unwrap().unwrap();
    assert_eq!(manifest.assets.len(), 1);
}

#[test]
fn export_game_needs_assets() {
    let tmp = tempfile::tempdir().unwrap();
    let mut project = demo_project();
    project.game.assets.clear();
    let session = headless(project);
    let e = session
        .export_game(&GameExportOptions {
            dir: Some(tmp.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap_err();
    assert!(e.to_string().contains("no game assets"), "{e}");
}

/// A local pack folder: `bd` with two variations, `sd`, and `keys` pitched.
fn pack_folder(dir: &Path) {
    for name in ["bd0", "bd1", "sd", "keys_c4"] {
        sine_file(&dir.join(format!("{name}.wav")), 0.2);
    }
    std::fs::write(
        dir.join("strudel.json"),
        r#"{
            "Kit_bd": ["bd0.wav", "bd1.wav"],
            "Kit_sd": "sd.wav",
            "keys": { "C4": "keys_c4.wav" }
        }"#,
    )
    .unwrap();
}

fn pack_request(dir: &Path, license: sky_model::License) -> sky_session::BankRequest {
    let mut source = sky_library::PackSource::parse(&dir.to_string_lossy(), Some(license));
    source.name = "Test kit".into();
    if license.needs_credit() {
        source.credit = Some("Test kit by Someone (CC BY 4.0)".into());
    }
    sky_session::BankRequest {
        source,
        bank: Some("Kit".into()),
        sounds: Vec::new(),
        add: false,
        pitched: None,
    }
}

#[test]
fn banks_load_from_packs_play_undo_and_credit_exports() {
    let tmp = tempfile::tempdir().unwrap();
    let pack = tmp.path().join("pack");
    std::fs::create_dir_all(&pack).unwrap();
    pack_folder(&pack);
    let mut project = empty_project("Banks");
    let mut track = Track::new_instrument("Beat", sky_dsp::InstrumentType::Synth);
    track.clips.push(Clip {
        id: ClipId::new(),
        name: "Beat".into(),
        start: 0,
        length: PPQ * 4,
        content: ClipContent::Pattern {
            source: "bd sd bd:1 sd".into(),
            seed: 0,
            cycle: PPQ * 4,
        },
    });
    let track_id = track.id;
    project.tracks.push(track);
    let session = headless(project);
    session.set_library(sky_library::Library::new(tmp.path().join("cache")));

    let request = pack_request(&pack, sky_model::License::CcBy4);
    let loaded = session.load_bank(track_id, &request, Origin::User).unwrap();
    assert_eq!(loaded.bank, "Kit");
    assert_eq!(
        loaded.sounds,
        [("bd".to_string(), 2), ("sd".to_string(), 1)]
    );
    assert_eq!((loaded.new_files, loaded.reused_files), (3, 0));
    assert!(loaded.pitched.is_none(), "a drum bank plays drums by note");
    let project = session.project();
    let slot = project.track(track_id).unwrap().instrument.clone().unwrap();
    assert_eq!(slot.kind, sky_dsp::InstrumentType::Bank);
    assert_eq!(project.audio_pool.len(), 3);
    let source = project.audio_pool[0].source.clone().unwrap();
    assert_eq!(source.pack, "Test kit");
    assert_eq!(source.license, sky_model::License::CcBy4);
    assert_eq!(
        session.view().undo_label.as_deref(),
        Some("Load Kit sounds")
    );

    // The pattern now plays the samples.
    let audio = session.render_range(0, PPQ * 4, 0.0).unwrap();
    assert!(audio.peak() > 0.1, "peak {}", audio.peak());

    // A loop export carries the credit into the manifest.
    session.save(Some(&tmp.path().join("song"))).unwrap();
    let out = session.export_loop(&LoopExportOptions::default()).unwrap();
    assert_eq!(out.licenses.credits, ["Test kit by Someone (CC BY 4.0)"]);
    let manifest = std::fs::read_to_string(&out.exported.manifest).unwrap();
    assert!(manifest.contains("Test kit by Someone"), "{manifest}");

    // Adding the pitched sound keeps the drums and reuses their files.
    let mut keys = pack_request(&pack, sky_model::License::CcBy4);
    keys.bank = None;
    keys.sounds = vec!["keys".into()];
    keys.add = true;
    keys.pitched = Some("keys".into());
    let more = session.load_bank(track_id, &keys, Origin::User).unwrap();
    assert_eq!(more.bank, "Kit + keys");
    assert_eq!(more.sounds.len(), 3);
    assert_eq!(more.pitched.as_deref(), Some("keys"));
    let again = session.load_bank(track_id, &request, Origin::User).unwrap();
    assert_eq!((again.new_files, again.reused_files), (0, 3));
    assert_eq!(session.project().audio_pool.len(), 4);

    // The bank's audio is saved and opens again with the project.
    session.save(None).unwrap();
    let reopened = headless(empty_project("Other"));
    reopened.open(&tmp.path().join("song")).unwrap();
    let audio = reopened.render_range(0, PPQ * 4, 0.0).unwrap();
    assert!(audio.peak() > 0.1);

    // Undo takes the loads back out, pool audio included.
    session.undo().unwrap();
    session.undo().unwrap();
    session.undo().unwrap();
    let project = session.project();
    assert!(project.audio_pool.is_empty());
    assert_eq!(
        project
            .track(track_id)
            .unwrap()
            .instrument
            .as_ref()
            .unwrap()
            .kind,
        sky_dsp::InstrumentType::Synth
    );
}

#[test]
fn unknown_licenses_warn_and_bad_requests_change_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let pack = tmp.path().join("pack");
    std::fs::create_dir_all(&pack).unwrap();
    pack_folder(&pack);
    let mut project = empty_project("Banks");
    let track = Track::new_instrument("Beat", sky_dsp::InstrumentType::Bank);
    let track_id = track.id;
    project.tracks.push(track);
    let session = headless(project);
    session.set_library(sky_library::Library::new(tmp.path().join("cache")));

    let mut request = pack_request(&pack, sky_model::License::Unknown);
    request.bank = Some("Nope".into());
    let err = session
        .load_bank(track_id, &request, Origin::User)
        .unwrap_err();
    assert!(err.to_string().contains("no bank \"Nope\""), "{err}");
    assert!(session.view().undo_label.is_none());

    request.bank = Some("Kit".into());
    session.load_bank(track_id, &request, Origin::User).unwrap();
    // No clips yet: nothing would be exported.
    assert!(session.license_report().is_empty());
    session
        .apply(Transaction::single(Command::AddClip {
            track: track_id,
            clip: Box::new(Clip {
                id: ClipId::new(),
                name: "p".into(),
                start: 0,
                length: PPQ * 4,
                content: ClipContent::Pattern {
                    source: "bd".into(),
                    seed: 0,
                    cycle: PPQ * 4,
                },
            }),
        }))
        .unwrap();
    let report = session.license_report();
    assert!(report.credits.is_empty());
    assert_eq!(report.warnings.len(), 1);
    assert!(report.warnings[0].starts_with("Test kit does not state a license"));
}
