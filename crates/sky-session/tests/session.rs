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
    ChannelRef, Clip, ClipContent, Command, GameAsset, GameAssetKind, Origin, Project, Target,
    TimeRange, Track, Transaction,
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
    assert_eq!(view.activity[0].targets, vec![Target::Song]);

    // Undoing to the first AI change also undoes the user change after it.
    assert_eq!(session.undo_to(first).unwrap(), 3);
    let view = session.view();
    assert!(view.activity.iter().all(|a| a.undone));
    assert_eq!(view.project.tempo.initial_bpm(), 120.0);
    assert!(session.undo_to(first).is_err());
}

#[test]
fn ai_activity_names_the_clips_and_effects_it_touched() {
    let project = demo_project();
    let clip = project.tracks[1].clips[0].id;
    let pad = ChannelRef::Track(project.tracks[2].id);
    let reverb = project.tracks[2].channel.effects[0].id;
    let session = headless(project);
    session
        .apply(mcp(
            "edit",
            vec![
                Command::SetClipName {
                    id: clip,
                    name: "Riff".into(),
                },
                Command::SetEffectEnabled {
                    channel: pad,
                    effect: reverb,
                    enabled: false,
                },
            ],
        ))
        .unwrap();
    let view = session.view();
    assert_eq!(
        view.activity[0].targets,
        vec![Target::Clip(clip), Target::Effect(pad, reverb)]
    );
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
                        muted: false,
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
fn unsaved_projects_export_like_one_off_exports() {
    let tmp = tempfile::tempdir().unwrap();
    let opts = GameExportOptions {
        dir: Some(tmp.path().join("audio")),
        ..Default::default()
    };
    // Each run of the demo is a new project ID; exporting again into the
    // same folder still works.
    for _ in 0..2 {
        let out = headless(demo_project()).export_game(&opts).unwrap();
        assert!(out.removed.is_empty());
    }
    let manifest = sky_game::read_manifest(&tmp.path().join("audio"))
        .unwrap()
        .unwrap();
    assert_eq!(manifest.assets.len(), 2);
    assert!(manifest.assets.iter().all(|a| a.project_id().is_none()));

    // Once saved, the project takes its entries over.
    let session = headless(demo_project());
    session
        .save(Some(&tmp.path().join("Demo.skyproj")))
        .unwrap();
    session.export_game(&opts).unwrap();
    let manifest = sky_game::read_manifest(&tmp.path().join("audio"))
        .unwrap()
        .unwrap();
    let id = session.project().id.to_string();
    assert!(
        manifest
            .assets
            .iter()
            .all(|a| a.project_id() == Some(id.as_str()))
    );
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
        muted: false,
        content: ClipContent::pattern("bd sd bd:1 sd", 0, PPQ * 4),
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
                muted: false,
                content: ClipContent::pattern("bd", 0, PPQ * 4),
            }),
        }))
        .unwrap();
    let report = session.license_report();
    assert!(report.credits.is_empty());
    assert_eq!(report.warnings.len(), 1);
    assert!(report.warnings[0].starts_with("Test kit does not state a license"));
}

fn peak_of(engine: &mut sky_engine::Engine, frames: usize) -> f32 {
    let mut l = vec![0.0f32; frames];
    let mut r = vec![0.0f32; frames];
    engine.process_planar(&mut l, &mut r);
    l.iter().chain(&r).fold(0.0f32, |m, s| m.max(s.abs()))
}

#[test]
fn auditions_play_while_stopped_and_change_nothing() {
    let (session, engine) = Session::new(demo_project(), Some(48_000));
    let mut engine = engine.unwrap();
    let project = session.project();
    let bass = project.tracks[1].id;
    let revision = session.view().revision;
    assert_eq!(peak_of(&mut engine, 4_800), 0.0);

    session.note_on(bass, 48, 100, Some(0.1)).unwrap();
    assert!(peak_of(&mut engine, 4_800) > 0.01, "the key is heard");
    assert!(!session.transport().playing);
    // The bass's release is short: a second later it is quiet again.
    peak_of(&mut engine, 48_000);
    assert!(peak_of(&mut engine, 4_800) < 1e-3);

    let audio = std::sync::Arc::new(AudioData {
        sample_rate: 24_000,
        channels: vec![vec![0.25; 2_400]],
    });
    let opts = sky_session::PreviewOptions {
        loop_start: Some(0),
        semitones: 12.0,
        gain_db: -6.0,
        ..Default::default()
    };
    let id = session.preview(audio.clone(), opts).unwrap();
    // An octave up undoes the half rate: 24 kHz audio plays at 48 kHz
    // speed. At -6 dB, 0.25 plays at about 0.125.
    let p = peak_of(&mut engine, 48_000);
    assert!((p - 0.125).abs() < 0.01, "{p}");
    assert_eq!(session.previewing(), Some(id));
    session.stop_preview().unwrap();
    peak_of(&mut engine, 4_800);
    assert_eq!(session.previewing(), None);

    // None of it is a change to the project.
    assert_eq!(session.view().revision, revision);
    assert!(session.view().history.is_empty());

    assert!(matches!(
        session.note_on(bass, 128, 100, None),
        Err(SessionError::Invalid(_))
    ));
    let mut with_audio = demo_project();
    with_audio.tracks.push(Track::new_audio("Audio"));
    let audio_track = with_audio.tracks[3].id;
    session.new_project(with_audio);
    assert!(session.note_on(audio_track, 60, 100, None).is_err());
    let off = headless(demo_project());
    let bass = off.project().tracks[1].id;
    assert!(matches!(
        off.note_on(bass, 60, 100, None),
        Err(SessionError::NoEngine)
    ));
    assert!(matches!(
        off.preview(audio, sky_session::PreviewOptions::default()),
        Err(SessionError::NoEngine)
    ));
    assert_eq!(off.previewing(), None);
}

#[test]
fn bank_sounds_play_by_name_and_variation() {
    let tmp = tempfile::tempdir().unwrap();
    let pack = tmp.path().join("pack");
    std::fs::create_dir_all(&pack).unwrap();
    pack_folder(&pack);
    let mut project = empty_project("Banks");
    let track = Track::new_instrument("Beat", sky_dsp::InstrumentType::Bank);
    let track_id = track.id;
    project.tracks.push(track);
    let (session, engine) = Session::new(project, Some(48_000));
    let mut engine = engine.unwrap();
    session.set_library(sky_library::Library::new(tmp.path().join("cache")));
    let request = pack_request(&pack, sky_model::License::Cc0);
    session.load_bank(track_id, &request, Origin::User).unwrap();

    peak_of(&mut engine, 480);
    session.play_sound(track_id, "bd", 1, 127).unwrap();
    assert!(peak_of(&mut engine, 4_800) > 0.1);
    let err = session.play_sound(track_id, "cp", 0, 127).unwrap_err();
    assert!(err.to_string().contains("no loaded sound"), "{err}");
}

#[test]
fn game_assets_render_as_they_export() {
    let mut project = demo_project();
    let bar = project.ticks_per_bar();
    project.game.assets = vec![
        game_asset(
            "battle",
            Category::Music,
            GameAssetKind::Loop {
                range: TimeRange::new(bar, bar * 3).unwrap(),
                intro_start: Some(0),
            },
        ),
        game_asset(
            "hit",
            Category::Sfx,
            GameAssetKind::sfx(TimeRange::new(0, bar).unwrap().split(4)),
        ),
    ];
    let session = headless(project);
    let battle = session.render_game_asset("battle").unwrap();
    // One bar of intro, then two bars of body: 2 s and 4 s at 120 BPM.
    assert_eq!(battle.audio.len(), 1);
    assert_eq!(battle.loop_start, 96_000);
    assert_eq!(battle.audio[0].frames(), 96_000 * 3);
    assert!(battle.seam.is_some());
    let hit = session.render_game_asset("hit").unwrap();
    assert_eq!(hit.audio.len(), 4);
    assert!(hit.seam.is_none());

    let tmp = tempfile::tempdir().unwrap();
    let out = session
        .export_game(&GameExportOptions {
            dir: Some(tmp.path().to_path_buf()),
            only: Vec::new(),
        })
        .unwrap();
    let Asset::Loop(l) = &out.assets[0].asset else {
        panic!()
    };
    assert_eq!(l.loop_start, battle.loop_start as u64);
    let Asset::Sfx(s) = &out.assets[1].asset else {
        panic!()
    };
    for (file, audio) in s.variations.iter().zip(&hit.audio) {
        assert_eq!(file.length_samples, audio.frames() as u64);
    }
    assert!(session.render_game_asset("nope").is_err());
}

fn pack_sound(url: &str) -> sky_session::AudioInput {
    let tone: Vec<f32> = (0..4_800).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
    sky_session::AudioInput::Pack {
        name: "Kit bd".into(),
        sound: "BD".into(),
        audio: std::sync::Arc::new(AudioData {
            sample_rate: 48_000,
            channels: vec![tone.clone(), tone],
        }),
        source: sky_model::AudioSource {
            pack: "Test kit".into(),
            url: url.into(),
            license: sky_model::License::Cc0,
            credit: None,
        },
        root: None,
    }
}

#[test]
fn audio_lands_where_it_is_dropped_in_one_undo_step() {
    use sky_dsp::InstrumentType;
    use sky_session::{AudioInput, AudioTarget};

    let tmp = tempfile::tempdir().unwrap();
    let wav = tmp.path().join("tone.wav");
    sine_file(&wav, 0.5);
    let mut project = empty_project("Drops");
    let vox = Track::new_audio("Vox");
    let keys = Track::new_instrument("Keys", InstrumentType::Sampler);
    let kit = Track::new_instrument("Kit", InstrumentType::Bank);
    let lead = Track::new_instrument("Lead", InstrumentType::Synth);
    let ids = [vox.id, keys.id, kit.id, lead.id];
    project.tracks.extend([vox, keys, kit, lead]);
    let session = headless(project);
    let [vox, keys, kit, lead] = ids;
    let file = || AudioInput::File(wav.clone());

    // A file onto an audio track: into the pool and a clip, one step.
    let history = session.view().history.len();
    let placed = session
        .place_audio(
            file(),
            AudioTarget::Clip {
                track: vox,
                start: PPQ * 4,
            },
            Origin::User,
        )
        .unwrap();
    assert!(!placed.reused);
    let project = session.project();
    assert_eq!(project.audio_pool.len(), 1);
    let (track, clip) = project.find_clip(placed.clip.unwrap()).unwrap();
    assert_eq!((track.id, clip.start), (vox, PPQ * 4));
    assert_eq!(clip.length, PPQ, "half a second at 120 bpm is one beat");
    assert_eq!(session.view().history.len(), history + 1);
    assert_eq!(session.view().undo_label.as_deref(), Some("Add tone"));
    assert!(session.audio(placed.audio).is_some());
    session.undo().unwrap();
    let project = session.project();
    assert!(project.audio_pool.is_empty());
    assert!(project.track(vox).unwrap().clips.is_empty());

    // On a new track after the first one; the same audio again on another.
    let placed = session
        .place_audio(
            file(),
            AudioTarget::NewTrack {
                start: 0,
                index: Some(1),
            },
            Origin::User,
        )
        .unwrap();
    assert!(placed.new_track);
    let project = session.project();
    assert_eq!(project.tracks[1].id, placed.track.unwrap());
    assert_eq!(project.tracks[1].name, "tone");
    assert_eq!(project.tracks[1].clips.len(), 1);
    assert_eq!(
        session.view().undo_label.as_deref(),
        Some("Add tone on a new track")
    );
    let again = session
        .place_audio(
            AudioInput::Pool(placed.audio),
            AudioTarget::NewTrack {
                start: 0,
                index: None,
            },
            Origin::User,
        )
        .unwrap();
    assert!(again.reused);
    assert_eq!(again.audio, placed.audio);
    let project = session.project();
    assert_eq!(project.audio_pool.len(), 1);
    assert_eq!(project.tracks.last().unwrap().name, "tone 2");

    // Into a sampler.
    session
        .place_audio(
            AudioInput::Pool(placed.audio),
            AudioTarget::Sampler(keys),
            Origin::User,
        )
        .unwrap();
    let project = session.project();
    let slot = project.track(keys).unwrap().instrument.clone().unwrap();
    assert_eq!(slot.sample, Some(placed.audio));
    assert_eq!(
        session.view().undo_label.as_deref(),
        Some("Load tone into Keys")
    );

    // A pack sound into a sample bank: the same address is used again and
    // becomes a second variation.
    let first = session
        .place_audio(
            pack_sound("https://example.com/bd.wav"),
            AudioTarget::Bank(kit),
            Origin::User,
        )
        .unwrap();
    assert_eq!(first.sound, Some(("bd".to_string(), 0)));
    let second = session
        .place_audio(
            pack_sound("https://example.com/bd.wav"),
            AudioTarget::Bank(kit),
            Origin::User,
        )
        .unwrap();
    assert!(second.reused);
    assert_eq!(second.audio, first.audio);
    assert_eq!(second.sound, Some(("bd".to_string(), 1)));
    let project = session.project();
    assert_eq!(project.audio_pool.len(), 2);
    let bank = project
        .track(kit)
        .unwrap()
        .instrument
        .as_ref()
        .unwrap()
        .bank
        .clone()
        .unwrap();
    assert_eq!(bank.name, "Test kit");
    assert_eq!(bank.sounds["bd"].len(), 2);
    let entry = project.pool_entry(first.audio).unwrap();
    assert_eq!(entry.source.as_ref().unwrap().pack, "Test kit");

    // Where audio cannot go, nothing changes, not even the pool.
    let before = session.view().revision;
    for (target, what) in [
        (
            AudioTarget::Clip {
                track: lead,
                start: 0,
            },
            "Lead is not an audio track",
        ),
        (AudioTarget::Sampler(vox), "Vox is not a sampler"),
        (AudioTarget::Bank(keys), "Keys is not a sample bank"),
    ] {
        let e = session
            .place_audio(file(), target, Origin::User)
            .unwrap_err();
        assert!(e.to_string().contains(what), "{e}");
    }
    let text = tmp.path().join("notes.wav");
    std::fs::write(&text, "not audio").unwrap();
    assert!(
        session
            .place_audio(AudioInput::File(text), AudioTarget::Pool, Origin::User)
            .is_err()
    );
    assert_eq!(session.view().revision, before);
    assert_eq!(session.project().audio_pool.len(), 2);

    // A saved project gets the file's audio in its folder.
    let dir = tmp.path().join("Drops.skyproj");
    session.save(Some(&dir)).unwrap();
    let placed = session
        .place_audio(file(), AudioTarget::Pool, Origin::User)
        .unwrap();
    let entry = session.project().pool_entry(placed.audio).unwrap().clone();
    assert!(dir.join(&entry.file).is_file());
    assert_eq!(session.view().undo_label.as_deref(), Some("Import tone"));
}

#[test]
fn several_files_import_as_one_step() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("kick.wav");
    let b = tmp.path().join("snare.wav");
    let bad = tmp.path().join("readme.wav");
    sine_file(&a, 0.1);
    sine_file(&b, 0.1);
    std::fs::write(&bad, "not audio").unwrap();
    let session = headless(empty_project("Import"));

    let done = session
        .import_files(&[a.clone(), bad.clone(), b.clone()], Origin::User)
        .unwrap();
    assert_eq!(done.audio.len(), 2);
    assert_eq!(done.failed.len(), 1);
    let project = session.project();
    let names: Vec<&str> = project.audio_pool.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["kick", "snare"]);
    assert_eq!(session.view().undo_label.as_deref(), Some("Import 2 files"));
    session.undo().unwrap();
    assert!(session.project().audio_pool.is_empty());

    // Nothing readable is an error that changes nothing.
    let before = session.view().revision;
    let e = session
        .import_files(&[bad.clone(), bad], Origin::User)
        .unwrap_err();
    assert!(e.to_string().contains("none of the 2 files"), "{e}");
    assert_eq!(session.view().revision, before);
}
