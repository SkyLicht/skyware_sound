// SPDX-License-Identifier: GPL-3.0-or-later
//! Command and undo behaviour, including the property test that random
//! command sequences always undo back to where they started.

use proptest::prelude::*;
use sky_core::{ClipId, EffectId, NoteId, PPQ, TrackId};
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
    let new_track = Track::new_instrument("Lead", InstrumentType::Synth);
    let lead = new_track.id;
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
                content: ClipContent::Midi { notes: vec![] },
            }),
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
        },
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
        (clips, "[a-z ~*]{0,12}").prop_map(|(clip, source)| Command::SetPattern {
            clip,
            source,
            seed: 3,
            cycle: PPQ * 4
        }),
        Just(Command::AddTrack {
            track: Box::new(Track::new_audio("Audio")),
            index: None
        }),
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
