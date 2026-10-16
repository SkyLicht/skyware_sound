// SPDX-License-Identifier: GPL-3.0-or-later
//! Command and undo behaviour, including the property test that random
//! command sequences always undo back to where they started.

use proptest::prelude::*;
use sky_core::manifest::{Category, RandomRange, Selection};
use sky_core::{AssetId, ClipId, EffectId, NoteId, PPQ, TrackId};
use sky_dsp::{EffectType, InstrumentType};
use sky_model::notes::parse_note_list;
use sky_model::templates::demo_project;
use sky_model::*;

fn tx(commands: Vec<Command>) -> Transaction {
    Transaction::user("test", commands)
}

#[test]
fn every_command_undoes_exactly() {
    let mut project = demo_project();
    let start = project.clone();
    let mut history = History::new();
    let drums = project.tracks[0].id;
    let bass = project.tracks[1].id;
    let bass_clip = project.tracks[1].clips[0].id;
    let pattern_clip = project.tracks[0].clips[0].id;
    let reverb = project.tracks[2].channel.effects[0].id;
    let duck = EffectSlot::new(EffectType::Duck);
    let new_track = Track::new_instrument("Lead", InstrumentType::Synth);
    let lead = new_track.id;
    let music = loop_asset("battle", 0, PPQ * 16);
    let mut music_edit = music.clone();
    music_edit.name = "battle_intro".into();
    music_edit.kind = GameAssetKind::Loop {
        range: TimeRange::new(PPQ * 4, PPQ * 20).unwrap(),
        intro_start: Some(0),
    };
    let steps_sfx = sfx_asset("hit", 3);
    let step_clip = ClipId::new();
    let note_ids: Vec<NoteId>;
    {
        let ClipContent::Midi { notes } = &project.tracks[1].clips[0].content else {
            panic!()
        };
        note_ids = notes.iter().take(3).map(|n| n.id).collect();
    }
    let steps = vec![
        Command::SetProjectName {
            name: "Renamed".into(),
        },
        Command::SetTempo { bpm: 93.5 },
        Command::SetTimeSignature {
            numerator: 7,
            denominator: 8,
        },
        Command::SetScale {
            scale: Some(Scale::new(2, ScaleMode::Dorian).unwrap()),
        },
        Command::SetScale { scale: None },
        Command::SetLoopRegion {
            start: PPQ,
            end: PPQ * 9,
        },
        Command::SetLoopEnabled { enabled: false },
        Command::AddTrack {
            track: Box::new(new_track),
            index: Some(0),
        },
        Command::MoveTrack { id: lead, index: 3 },
        Command::SetTrackName {
            id: drums,
            name: "Kit".into(),
        },
        Command::SetChannel {
            channel: ChannelRef::Track(bass),
            volume_db: Some(-6.0),
            pan: Some(0.3),
            mute: None,
            solo: Some(true),
        },
        Command::SetChannel {
            channel: ChannelRef::Master,
            volume_db: Some(-3.0),
            pan: None,
            mute: Some(true),
            solo: None,
        },
        Command::SetInstrumentParam {
            track: bass,
            param: "cutoff".into(),
            value: Some(900.0),
        },
        Command::SetInstrumentParam {
            track: bass,
            param: "gain".into(),
            value: None,
        },
        Command::SetInstrument {
            track: lead,
            instrument: Some(InstrumentSlot::new(InstrumentType::Drums)),
        },
        Command::AddEffect {
            channel: ChannelRef::Track(bass),
            effect: EffectSlot::new(EffectType::Delay),
            index: None,
        },
        Command::AddEffect {
            channel: ChannelRef::Track(bass),
            effect: duck.clone(),
            index: Some(0),
        },
        Command::SetEffectSidechain {
            channel: ChannelRef::Track(bass),
            effect: duck.id,
            source: Some(drums),
        },
        Command::SetEffectParam {
            channel: ChannelRef::Track(project.tracks[2].id),
            effect: reverb,
            param: "mix".into(),
            value: Some(0.9),
        },
        Command::SetEffectEnabled {
            channel: ChannelRef::Track(project.tracks[2].id),
            effect: reverb,
            enabled: false,
        },
        Command::RemoveEffect {
            channel: ChannelRef::Track(project.tracks[2].id),
            id: reverb,
        },
        Command::AddClip {
            track: lead,
            clip: Box::new(Clip {
                id: ClipId::new(),
                name: "x".into(),
                start: 0,
                length: PPQ * 4,
                muted: false,
                content: ClipContent::Midi { notes: vec![] },
            }),
        },
        Command::AddClip {
            track: lead,
            clip: Box::new(Clip {
                id: step_clip,
                name: "steps".into(),
                start: PPQ * 4,
                length: PPQ * 4,
                muted: false,
                content: ClipContent::Steps(StepClip::new(
                    16,
                    vec![StepLane::parse(36, "x . x/ c3!").unwrap()],
                )),
            }),
        },
        Command::SetSteps {
            clip: step_clip,
            steps: Box::new(StepClip::new(
                8,
                vec![
                    StepLane::parse(36, "x x . -").unwrap(),
                    StepLane::parse(42, "x?50 x*3").unwrap(),
                ],
            )),
        },
        Command::MoveClip {
            id: bass_clip,
            track: bass,
            start: PPQ * 8,
        },
        Command::SetClipLength {
            id: bass_clip,
            length: PPQ * 2,
        },
        Command::SetClipName {
            id: bass_clip,
            name: "Moved".into(),
        },
        Command::SetClipMuted {
            id: bass_clip,
            muted: true,
        },
        Command::AddNotes {
            clip: bass_clip,
            notes: parse_note_list("C5 1/4, D5 1/4").unwrap(),
        },
        Command::RemoveNotes {
            clip: bass_clip,
            ids: note_ids.clone(),
        },
        Command::SetPattern {
            clip: pattern_clip,
            source: "bd sd".into(),
            seed: 9,
            cycle: PPQ * 2,
            controls: [("velocity".to_string(), "127 60".to_string())].into(),
            swing: Swing::new(62, 16),
        },
        Command::SetGameExport {
            export_dir: Some("../game/assets/audio".into()),
            bits: 16,
        },
        Command::AddGameAsset {
            asset: Box::new(music.clone()),
            index: None,
        },
        Command::AddGameAsset {
            asset: Box::new(steps_sfx.clone()),
            index: Some(0),
        },
        Command::UpdateGameAsset {
            asset: Box::new(music_edit),
        },
        Command::RemoveGameAsset { id: steps_sfx.id },
        Command::RemoveClip { id: pattern_clip },
        Command::RemoveTrack { id: drums },
    ];
    let mut snapshots = vec![project.clone()];
    for step in steps {
        let label = step.describe();
        history
            .apply(&mut project, tx(vec![step]))
            .unwrap_or_else(|e| panic!("{label}: {e}"));
        snapshots.push(project.clone());
    }
    // Undo one step at a time and compare with the snapshot before it.
    for expected in snapshots.iter().rev().skip(1) {
        history.undo(&mut project).unwrap();
        assert_eq!(&project, expected);
    }
    assert_eq!(project, start);
    // Redo everything and land on the final state.
    while history.can_redo() {
        history.redo(&mut project).unwrap();
    }
    assert_eq!(&project, snapshots.last().unwrap());
}

fn loop_asset(name: &str, start: i64, end: i64) -> GameAsset {
    GameAsset {
        id: AssetId::new(),
        name: name.into(),
        category: Category::Music,
        tags: vec![],
        kind: GameAssetKind::Loop {
            range: TimeRange::new(start, end).unwrap(),
            intro_start: None,
        },
    }
}

fn sfx_asset(name: &str, count: usize) -> GameAsset {
    GameAsset {
        id: AssetId::new(),
        name: name.into(),
        category: Category::Sfx,
        tags: vec!["impact".into()],
        kind: GameAssetKind::sfx(TimeRange::new(0, PPQ * 4).unwrap().split(count)),
    }
}

fn try_apply(history: &mut History, project: &mut Project, c: Command) -> Result<(), String> {
    history
        .apply(project, tx(vec![c]))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn try_add(history: &mut History, project: &mut Project, asset: GameAsset) -> String {
    try_apply(
        history,
        project,
        Command::AddGameAsset {
            asset: Box::new(asset),
            index: None,
        },
    )
    .unwrap_err()
}

#[test]
fn game_assets_are_validated() {
    let mut project = demo_project();
    let mut h = History::new();
    let hit = sfx_asset("hit", 4);
    try_apply(
        &mut h,
        &mut project,
        Command::AddGameAsset {
            asset: Box::new(hit.clone()),
            index: None,
        },
    )
    .unwrap();
    let before = project.clone();

    let e = try_add(&mut h, &mut project, sfx_asset("Hit Sound", 2));
    assert!(e.contains("lowercase letters"), "{e}");
    let e = try_add(&mut h, &mut project, sfx_asset("hit", 2));
    assert!(e.contains("already called"), "{e}");
    let mut dup = sfx_asset("other", 2);
    dup.id = hit.id;
    let e = try_add(&mut h, &mut project, dup);
    assert!(e.contains("already exists"), "{e}");
    let mut none = sfx_asset("empty", 1);
    none.kind = GameAssetKind::sfx(vec![]);
    let e = try_add(&mut h, &mut project, none);
    assert!(e.contains("1 to 64 variations"), "{e}");
    let mut wild = sfx_asset("wild", 2);
    if let GameAssetKind::Sfx {
        pitch_semitones, ..
    } = &mut wild.kind
    {
        *pitch_semitones = RandomRange {
            min: 2.0,
            max: -2.0,
        };
    }
    let e = try_add(&mut h, &mut project, wild);
    assert!(e.contains("random pitch"), "{e}");
    let mut long_tail = sfx_asset("tail", 2);
    if let GameAssetKind::Sfx {
        tail_seconds,
        selection,
        ..
    } = &mut long_tail.kind
    {
        *tail_seconds = 30.0;
        *selection = Selection::Shuffle;
    }
    let e = try_add(&mut h, &mut project, long_tail);
    assert!(e.contains("tail"), "{e}");
    let mut late_intro = loop_asset("music", PPQ * 4, PPQ * 8);
    late_intro.kind = GameAssetKind::Loop {
        range: TimeRange::new(PPQ * 4, PPQ * 8).unwrap(),
        intro_start: Some(PPQ * 4),
    };
    let e = try_add(&mut h, &mut project, late_intro);
    assert!(e.contains("intro"), "{e}");
    let e = try_apply(
        &mut h,
        &mut project,
        Command::SetGameExport {
            export_dir: None,
            bits: 20,
        },
    )
    .unwrap_err();
    assert!(e.contains("16, 24 or 32"), "{e}");
    let e = try_apply(
        &mut h,
        &mut project,
        Command::RemoveGameAsset { id: AssetId::new() },
    )
    .unwrap_err();
    assert!(e.contains("no game asset"), "{e}");
    assert_eq!(project, before);

    // Renaming to a name nobody else has works, and an empty export folder
    // means "none".
    let mut renamed = hit.clone();
    renamed.name = "hit_metal".into();
    try_apply(
        &mut h,
        &mut project,
        Command::UpdateGameAsset {
            asset: Box::new(renamed),
        },
    )
    .unwrap();
    try_apply(
        &mut h,
        &mut project,
        Command::SetGameExport {
            export_dir: Some("  ".into()),
            bits: 32,
        },
    )
    .unwrap();
    assert_eq!(project.game.export_dir, None);
    assert_eq!(project.game.bits, 32);
    assert!(project.game.find("hit_metal").is_some());
}

#[test]
fn game_settings_round_trip_and_stay_out_of_plain_projects() {
    let mut project = sky_model::templates::empty_project("Plain");
    let plain = sky_model::file::project_to_json(&project);
    assert!(!plain.contains("\"game\""), "{plain}");
    project.game.export_dir = Some("C:/game/audio".into());
    project.game.assets.push(loop_asset("battle", 0, PPQ * 16));
    project.game.assets.push(sfx_asset("hit", 4));
    let json = sky_model::file::project_to_json(&project);
    assert!(json.contains("\"kind\": \"sfx\""), "{json}");
    assert!(json.contains("\"tailSeconds\""), "{json}");
    let back = sky_model::file::project_from_json(&json).unwrap();
    assert_eq!(back, project);
}

#[test]
fn the_song_key_is_checked_saved_and_summarized() {
    let mut project = demo_project();
    let mut history = History::new();
    assert_eq!(
        summary::project_summary(&project)["scale"],
        serde_json::json!("A minor")
    );
    let bad = Command::SetScale {
        scale: Some(Scale {
            root: 12,
            mode: ScaleMode::Major,
        }),
    };
    assert!(try_apply(&mut history, &mut project, bad).is_err());
    let phrygian = Scale::new(6, ScaleMode::Phrygian).unwrap();
    try_apply(
        &mut history,
        &mut project,
        Command::SetScale {
            scale: Some(phrygian),
        },
    )
    .unwrap();
    assert_eq!(phrygian.label(), "F# Phrygian");
    let json = serde_json::to_value(&project).unwrap();
    assert_eq!(
        json["scale"],
        serde_json::json!({"root": 6, "mode": "phrygian"})
    );
    try_apply(
        &mut history,
        &mut project,
        Command::SetScale { scale: None },
    )
    .unwrap();
    // No key: none in the file or the summary, and older files load.
    let json = serde_json::to_value(&project).unwrap();
    assert!(json.get("scale").is_none());
    assert!(summary::project_summary(&project).get("scale").is_none());
    let back: Project = serde_json::from_value(json).unwrap();
    assert_eq!(back.scale, None);
}

#[test]
fn ranges_split_into_equal_variations() {
    let r = TimeRange::new(PPQ, PPQ * 5).unwrap();
    let parts = r.split(4);
    assert_eq!(parts.len(), 4);
    assert_eq!(parts[0], TimeRange::new(PPQ, PPQ * 2).unwrap());
    assert_eq!(parts[3].end, PPQ * 5);
    let odd = TimeRange::new(0, 10).unwrap().split(3);
    assert_eq!(
        odd.iter().map(|p| (p.start, p.end)).collect::<Vec<_>>(),
        vec![(0, 3), (3, 6), (6, 10)]
    );
    // More parts than ticks: one tick each.
    assert_eq!(TimeRange::new(0, 2).unwrap().split(5).len(), 2);
    assert_eq!(r.split(0).len(), 1);
    // The span of variations given in any order.
    let mut shuffled = parts.clone();
    shuffled.reverse();
    assert_eq!(TimeRange::covering(&shuffled), Some(r));
    assert_eq!(TimeRange::covering(&[]), None);
}

#[test]
fn failed_transactions_leave_the_project_untouched() {
    let mut project = demo_project();
    let before = project.clone();
    let mut history = History::new();
    let err = history
        .apply(
            &mut project,
            tx(vec![
                Command::SetTempo { bpm: 100.0 },
                Command::RemoveTrack { id: TrackId::new() },
            ]),
        )
        .unwrap_err();
    assert!(err.to_string().contains("step 2"), "{err}");
    assert_eq!(project, before);
    assert!(!history.can_undo());
}

#[test]
fn validation_messages_are_specific() {
    let mut project = demo_project();
    let bass = project.tracks[1].id;
    let e = Command::SetInstrumentParam {
        track: bass,
        param: "cutof".into(),
        value: Some(1.0),
    }
    .apply(&mut project)
    .unwrap_err()
    .to_string();
    assert!(e.contains("cutoff"), "should list valid names: {e}");
    let e = Command::SetInstrumentParam {
        track: bass,
        param: "cutoff".into(),
        value: Some(1e6),
    }
    .apply(&mut project)
    .unwrap_err()
    .to_string();
    assert!(e.contains("between 20 and 20000"), "{e}");
    let e = Command::SetTempo { bpm: 2000.0 }
        .apply(&mut project)
        .unwrap_err()
        .to_string();
    assert!(e.contains("BPM"), "{e}");
    let e = Command::RemoveEffect {
        channel: ChannelRef::Master,
        id: EffectId::new(),
    }
    .apply(&mut project)
    .unwrap_err();
    assert!(matches!(e, ModelError::EffectNotFound(_)));
}

#[test]
fn pattern_controls_and_swing_are_checked() {
    let mut project = demo_project();
    let clip = project.tracks[0].clips[0].id;
    let set = |controls: &[(&str, &str)], swing: Swing| Command::SetPattern {
        clip,
        source: "bd*4".into(),
        seed: 1,
        cycle: PPQ * 4,
        controls: controls
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        swing,
    };
    let straight = Swing::default();
    // Any instrument's parameter is accepted, not only the current one's,
    // so changing the instrument never breaks a clip.
    set(
        &[("velocity", "127 80"), ("cutoff", "<400 4000>")],
        straight,
    )
    .apply(&mut project)
    .unwrap();
    let error = |c: Command, project: &mut Project| c.apply(project).unwrap_err().to_string();
    let e = error(set(&[("cutof", "1")], straight), &mut project);
    assert!(e.contains("velocity") && e.contains("cutof"), "{e}");
    let e = error(set(&[("velocity", "100 bd")], straight), &mut project);
    assert!(e.contains("velocity") && e.contains("takes numbers"), "{e}");
    let many: Vec<(String, String)> = ["velocity", "cutoff", "resonance", "attack", "decay"]
        .iter()
        .chain(&["sustain", "release", "detune", "gain"])
        .map(|n| (n.to_string(), "1".to_string()))
        .collect();
    let many: Vec<(&str, &str)> = many.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let e = error(set(&many, straight), &mut project);
    assert!(e.contains("at most 8"), "{e}");
    let e = error(set(&[], Swing::new(80, 16)), &mut project);
    assert!(e.contains("75"), "{e}");
    let e = error(set(&[], Swing::new(60, 12)), &mut project);
    assert!(e.contains("16"), "{e}");
}

#[test]
fn sidechains_need_a_duck_and_an_instrument_track() {
    let mut project = demo_project();
    let drums = project.tracks[0].id;
    let bass = ChannelRef::Track(project.tracks[1].id);
    let reverb = project.tracks[2].channel.effects[0].id;
    let pad = ChannelRef::Track(project.tracks[2].id);
    let e = Command::SetEffectSidechain {
        channel: pad,
        effect: reverb,
        source: Some(drums),
    }
    .apply(&mut project)
    .unwrap_err()
    .to_string();
    assert!(e.contains("only a duck"), "{e}");
    let duck = EffectSlot::new(EffectType::Duck);
    let id = duck.id;
    Command::AddEffect {
        channel: bass,
        effect: duck,
        index: None,
    }
    .apply(&mut project)
    .unwrap();
    let audio = Track::new_audio("Loop");
    let audio_id = audio.id;
    Command::AddTrack {
        track: Box::new(audio),
        index: None,
    }
    .apply(&mut project)
    .unwrap();
    let e = Command::SetEffectSidechain {
        channel: bass,
        effect: id,
        source: Some(audio_id),
    }
    .apply(&mut project)
    .unwrap_err()
    .to_string();
    assert!(e.contains("instrument track"), "{e}");
    assert!(
        Command::SetEffectSidechain {
            channel: bass,
            effect: id,
            source: Some(TrackId::new()),
        }
        .apply(&mut project)
        .is_err()
    );
    let undo = Command::SetEffectSidechain {
        channel: bass,
        effect: id,
        source: Some(drums),
    }
    .apply(&mut project)
    .unwrap();
    assert_eq!(
        undo,
        Command::SetEffectSidechain {
            channel: bass,
            effect: id,
            source: None,
        }
    );
    // Deleting the source leaves the duck pointing at nothing, and undoing
    // the deletions in reverse order still works.
    let mut history = History::new();
    history
        .apply(&mut project, tx(vec![Command::RemoveTrack { id: drums }]))
        .unwrap();
    let bass_id = project.tracks[0].id;
    history
        .apply(&mut project, tx(vec![Command::RemoveTrack { id: bass_id }]))
        .unwrap();
    history.undo(&mut project).unwrap();
    history.undo(&mut project).unwrap();
    assert_eq!(project.tracks[1].channel.effects[0].sidechain, Some(drums));
}

#[test]
fn old_files_load_without_controls_swing_or_sidechain() {
    let clip: ClipContent =
        serde_json::from_str(r#"{"type":"pattern","source":"bd sd","seed":2,"cycle":3840}"#)
            .unwrap();
    assert_eq!(clip, ClipContent::pattern("bd sd", 2, 3840));
    // And they save the same way when nothing new is used.
    assert_eq!(
        serde_json::to_string(&clip).unwrap(),
        r#"{"type":"pattern","source":"bd sd","seed":2,"cycle":3840}"#
    );
    let slot = EffectSlot::new(EffectType::Duck);
    let json = serde_json::to_string(&slot).unwrap();
    assert!(!json.contains("sidechain"), "{json}");
}

#[test]
fn step_clips_are_checked() {
    let mut project = demo_project();
    let mut history = History::new();
    let drums = project.tracks[0].id;
    let pattern = project.tracks[0].clips[0].id;
    let audio = Track::new_audio("Audio");
    let audio_id = audio.id;
    history
        .apply(
            &mut project,
            tx(vec![Command::AddTrack {
                track: Box::new(audio),
                index: None,
            }]),
        )
        .unwrap();
    let clip = |steps: StepClip| Clip {
        id: ClipId::new(),
        name: "s".into(),
        start: 0,
        length: PPQ * 4,
        muted: false,
        content: ClipContent::Steps(steps),
    };
    let good = StepClip::new(16, vec![StepLane::parse(36, "x . . .").unwrap()]);
    let add = |track, steps| Command::AddClip {
        track,
        clip: Box::new(clip(steps)),
    };
    let err = |project: &mut Project, history: &mut History, c: Command| {
        history.apply(project, tx(vec![c])).unwrap_err().to_string()
    };
    assert!(err(&mut project, &mut history, add(audio_id, good.clone())).contains("instrument"));
    let mut bad = good.clone();
    bad.lanes[0].steps[1].locks.insert("nonsense".into(), 1.0);
    assert!(err(&mut project, &mut history, add(drums, bad)).contains("nonsense"));
    let mut bad = good.clone();
    bad.lanes[0].steps[0].ratchet = 9;
    let text = err(&mut project, &mut history, add(drums, bad));
    assert!(text.contains("lane 1") && text.contains("step 1"), "{text}");
    // SetSteps only changes step clips.
    let text = err(
        &mut project,
        &mut history,
        Command::SetSteps {
            clip: pattern,
            steps: Box::new(good.clone()),
        },
    );
    assert!(text.contains("pattern") && text.contains("steps"), "{text}");
    history
        .apply(&mut project, tx(vec![add(drums, good)]))
        .unwrap();
    // A saved step clip loads back the same.
    let json = serde_json::to_string(&project).unwrap();
    let back: Project = serde_json::from_str(&json).unwrap();
    assert_eq!(back, project);
}

#[test]
fn swing_moves_every_second_step() {
    let sixteenth = PPQ / 4;
    let swing = Swing::new(75, 16);
    // The on-beat step stays; the off-beat one moves 3/4 of the way
    // through the pair; the next pair starts on time.
    assert_eq!(swing.apply(0), 0);
    assert_eq!(swing.apply(sixteenth), sixteenth * 3 / 2);
    assert_eq!(swing.apply(2 * sixteenth), 2 * sixteenth);
    assert_eq!(Swing::default().apply(sixteenth), sixteenth);
    // Times keep their order.
    let mut last = -1;
    for t in 0..PPQ * 2 {
        let s = Swing::new(66, 8).apply(t);
        assert!(s >= last);
        last = s;
    }
}

#[test]
fn coalesced_steps_merge_into_one_undo() {
    let mut project = demo_project();
    let start = project.clone();
    let mut history = History::new();
    let bass = project.tracks[1].id;
    for v in [-1.0, -2.0, -3.0, -4.0] {
        let t = Transaction::single(Command::SetChannel {
            channel: ChannelRef::Track(bass),
            volume_db: Some(v),
            pan: None,
            mute: None,
            solo: None,
        })
        .coalescing(42);
        history.apply(&mut project, t).unwrap();
    }
    assert_eq!(history.undo_steps().len(), 1);
    history.undo(&mut project).unwrap();
    assert_eq!(project, start);
}

fn arb_command(track_ids: Vec<TrackId>, clip_ids: Vec<ClipId>) -> impl Strategy<Value = Command> {
    let tracks = prop::sample::select(track_ids);
    let clips = prop::sample::select(clip_ids);
    prop_oneof![
        (40.0f64..200.0).prop_map(|bpm| Command::SetTempo { bpm }),
        (0i64..8, 1i64..8).prop_map(|(s, l)| Command::SetLoopRegion {
            start: s * PPQ,
            end: (s + l) * PPQ
        }),
        any::<bool>().prop_map(|enabled| Command::SetLoopEnabled { enabled }),
        (tracks.clone(), -40.0f32..6.0, -1.0f32..1.0).prop_map(|(t, v, p)| Command::SetChannel {
            channel: ChannelRef::Track(t),
            volume_db: Some(v),
            pan: Some(p),
            mute: None,
            solo: None
        }),
        (tracks.clone(), "[a-z]{1,8}").prop_map(|(id, name)| Command::SetTrackName { id, name }),
        (tracks.clone(), 0usize..4).prop_map(|(id, index)| Command::MoveTrack { id, index }),
        tracks.clone().prop_map(|id| Command::RemoveTrack { id }),
        tracks.clone().prop_map(|t| Command::AddEffect {
            channel: ChannelRef::Track(t),
            effect: EffectSlot::new(EffectType::Delay),
            index: Some(0)
        }),
        (tracks.clone(), 20.0f32..20_000.0).prop_map(|(t, v)| Command::SetInstrumentParam {
            track: t,
            param: "cutoff".into(),
            value: Some(v)
        }),
        (clips.clone(), tracks.clone(), 0i64..16).prop_map(|(id, track, s)| Command::MoveClip {
            id,
            track,
            start: s * PPQ
        }),
        (clips.clone(), 1i64..16).prop_map(|(id, l)| Command::SetClipLength {
            id,
            length: l * PPQ
        }),
        clips.clone().prop_map(|id| Command::RemoveClip { id }),
        (clips.clone(), 0i64..32, 36u8..90).prop_map(|(clip, s, p)| Command::AddNotes {
            clip,
            notes: vec![sky_model::Note::new(s * 120, 240, p, 100)]
        }),
        (clips, "[a-z ~*]{0,12}", 50u8..80).prop_map(|(clip, source, swing)| {
            Command::SetPattern {
                clip,
                source,
                seed: 3,
                cycle: PPQ * 4,
                controls: [("velocity".to_string(), "100 [60 127]".to_string())].into(),
                swing: Swing::new(swing, 16),
            }
        }),
        Just(Command::AddTrack {
            track: Box::new(Track::new_audio("Audio")),
            index: None
        }),
        ("[a-z]{1,6}", 0i64..8, 1i64..8).prop_map(|(name, s, l)| Command::AddGameAsset {
            asset: Box::new(loop_asset(&name, s * PPQ, (s + l) * PPQ)),
            index: None
        }),
        (
            prop::option::of("[a-z/]{1,12}"),
            prop::sample::select(vec![16u16, 24, 32])
        )
            .prop_map(|(export_dir, bits)| Command::SetGameExport { export_dir, bits }),
    ]
}

static DEMO: std::sync::LazyLock<Project> = std::sync::LazyLock::new(demo_project);

fn arb_sequences() -> impl Strategy<Value = Vec<Vec<Command>>> {
    let tracks: Vec<TrackId> = DEMO.tracks.iter().map(|t| t.id).collect();
    let clips: Vec<ClipId> = DEMO
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter().map(|c| c.id))
        .collect();
    prop::collection::vec(
        prop::collection::vec(arb_command(tracks, clips), 1..4),
        1..30,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn random_sequences_undo_to_the_start(sequences in arb_sequences()) {
        let mut project = DEMO.clone();
        let start = project.clone();
        let mut history = History::new();
        let mut applied = 0;
        for cmds in sequences {
            if history.apply(&mut project, tx(cmds)).is_ok() {
                applied += 1;
            }
        }
        let after = project.clone();
        while history.can_undo() {
            history.undo(&mut project).unwrap();
        }
        prop_assert_eq!(&project, &start);
        while history.can_redo() {
            history.redo(&mut project).unwrap();
        }
        prop_assert_eq!(&project, &after);
        prop_assert!(applied <= 30);
    }
}

#[test]
fn random_sequences_mostly_apply() {
    // Guards the property test above against generating only failing commands.
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let mut ok = 0;
    let mut total = 0;
    for _ in 0..50 {
        let seqs = arb_sequences().new_tree(&mut runner).unwrap().current();
        let mut project = DEMO.clone();
        let mut history = History::new();
        for cmds in seqs {
            total += 1;
            if history.apply(&mut project, tx(cmds)).is_ok() {
                ok += 1;
            }
        }
    }
    assert!(ok * 3 > total, "only {ok} of {total} transactions applied");
}

fn pool_audio(project: &mut Project, name: &str) -> sky_core::AudioId {
    let id = sky_core::AudioId::new();
    project.audio_pool.push(PoolEntry {
        id,
        name: name.into(),
        file: format!("audio/{id}.wav"),
        channels: 1,
        sample_rate: 48_000,
        frames: 100,
        source: Some(AudioSource {
            pack: "Test pack".into(),
            url: format!("https://example.com/{name}.wav"),
            license: License::Cc0,
            credit: None,
        }),
    });
    id
}

fn bank_of(sounds: &[(&str, &[sky_core::AudioId])]) -> SampleBank {
    SampleBank {
        name: "Test".into(),
        sounds: sounds
            .iter()
            .map(|(name, ids)| {
                (
                    name.to_string(),
                    ids.iter()
                        .map(|&audio| BankSample { audio, root: None })
                        .collect(),
                )
            })
            .collect(),
        pitched: None,
    }
}

#[test]
fn sample_banks_are_validated_and_keep_their_audio() {
    let mut project = Project::new("Banks");
    let a = pool_audio(&mut project, "bd");
    let b = pool_audio(&mut project, "sn");
    let track = Track::new_instrument("Kit", InstrumentType::Bank);
    let id = track.id;
    let mut history = History::new();
    history
        .apply(
            &mut project,
            tx(vec![Command::AddTrack {
                track: Box::new(track),
                index: None,
            }]),
        )
        .unwrap();
    let mut slot = InstrumentSlot::new(InstrumentType::Bank);
    slot.bank = Some(bank_of(&[("bd", &[a]), ("sn", &[b, a])]));
    let set = |slot: &InstrumentSlot| Command::SetInstrument {
        track: id,
        instrument: Some(slot.clone()),
    };
    history.apply(&mut project, tx(vec![set(&slot)])).unwrap();

    // The audio stays while the bank uses it.
    let err = history
        .apply(&mut project, tx(vec![Command::RemovePoolAudio { id: a }]))
        .unwrap_err();
    assert!(err.to_string().contains("sample bank"), "{err}");

    let mut bad = slot.clone();
    bad.kind = InstrumentType::Synth;
    assert!(history.apply(&mut project, tx(vec![set(&bad)])).is_err());
    let mut bad = slot.clone();
    bad.bank = Some(bank_of(&[("bd", &[sky_core::AudioId::new()])]));
    assert!(history.apply(&mut project, tx(vec![set(&bad)])).is_err());
    let mut bad = slot.clone();
    bad.bank = Some(bank_of(&[("bad name", &[a])]));
    assert!(history.apply(&mut project, tx(vec![set(&bad)])).is_err());
    let mut bad = slot.clone();
    bad.bank = Some(bank_of(&[("bd", &[])]));
    assert!(history.apply(&mut project, tx(vec![set(&bad)])).is_err());
    let mut bad = slot.clone();
    bad.bank.as_mut().unwrap().pitched = Some("piano".into());
    assert!(history.apply(&mut project, tx(vec![set(&bad)])).is_err());

    // Saving and loading keeps the bank and the sources.
    let json = serde_json::to_string(&project).unwrap();
    let back: Project = serde_json::from_str(&json).unwrap();
    assert_eq!(back, project);
    assert!(json.contains("\"license\":\"CC0-1.0\""), "{json}");
}

#[test]
fn bank_sounds_resolve_by_name_case_and_drum_alias() {
    let a = sky_core::AudioId::new();
    let b = sky_core::AudioId::new();
    let bank = bank_of(&[("bd", &[a]), ("SN", &[b]), ("casio", &[a, b])]);
    assert_eq!(
        bank.resolve("bd").map(|(n, s)| (n, s.len())),
        Some(("bd", 1))
    );
    assert_eq!(bank.resolve("casio").map(|(_, s)| s.len()), Some(2));
    // A different case, then another name for the same drum.
    assert_eq!(bank.resolve("sn").map(|(n, _)| n), Some("SN"));
    assert_eq!(bank.resolve("sd").map(|(n, _)| n), Some("SN"));
    assert_eq!(bank.resolve("kick").map(|(n, _)| n), Some("bd"));
    assert_eq!(bank.resolve("hh"), None);
    assert_eq!(bank.sample_count(), 4);
}
