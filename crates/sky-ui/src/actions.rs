// SPDX-License-Identifier: GPL-3.0-or-later
//! Edits the UI makes, as small functions over a [`Session`].
//!
//! Each one builds commands, applies them as one transaction and returns a
//! message a person can read on failure. Drags pass a coalescing key so that
//! every change of one gesture merges into a single undo step.

use std::collections::BTreeMap;

use sky_core::{ClipId, EffectId, NoteId, PPQ, Ticks, TrackId};
use sky_dsp::{EffectType, InstrumentType};
use sky_model::{
    ChannelRef, Clip, ClipContent, Command, EffectSlot, InstrumentSlot, Note, Project, StepClip,
    StepLane, Swing, Track, Transaction,
};
use sky_session::Session;

pub type ActionResult<T> = Result<T, String>;

/// Applies a transaction, optionally merged with the previous one of the same key.
pub fn apply(session: &Session, tx: Transaction, key: Option<u64>) -> ActionResult<u64> {
    let tx = match key {
        Some(k) => tx.coalescing(k),
        None => tx,
    };
    session.apply(tx).map_err(|e| e.to_string())
}

fn single(session: &Session, command: Command, key: Option<u64>) -> ActionResult<u64> {
    apply(session, Transaction::single(command), key)
}

/// The kinds of track the UI can add.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewTrack {
    Instrument(InstrumentType),
    Audio,
}

impl NewTrack {
    pub const ALL: [NewTrack; 6] = [
        NewTrack::Instrument(InstrumentType::Synth),
        NewTrack::Instrument(InstrumentType::Acid),
        NewTrack::Instrument(InstrumentType::Drums),
        NewTrack::Instrument(InstrumentType::Sampler),
        NewTrack::Instrument(InstrumentType::Bank),
        NewTrack::Audio,
    ];

    pub fn label(self) -> &'static str {
        match self {
            NewTrack::Instrument(InstrumentType::Synth) => "Synth",
            NewTrack::Instrument(InstrumentType::Drums) => "Drums",
            NewTrack::Instrument(InstrumentType::Sampler) => "Sampler",
            NewTrack::Instrument(InstrumentType::Bank) => "Sample bank",
            NewTrack::Instrument(InstrumentType::Acid) => "Acid bass",
            NewTrack::Audio => "Audio",
        }
    }
}

/// A name like "Synth 3" that no track in the project uses yet.
pub fn unique_track_name(project: &Project, base: &str) -> String {
    project.unique_track_name(base)
}

pub fn new_track(project: &Project, kind: NewTrack) -> Track {
    let name = unique_track_name(project, kind.label());
    match kind {
        NewTrack::Instrument(t) => Track::new_instrument(name, t),
        NewTrack::Audio => Track::new_audio(name),
    }
}

/// Adds a track after `after` (or at the end) and returns its ID.
pub fn add_track(
    session: &Session,
    kind: NewTrack,
    after: Option<TrackId>,
) -> ActionResult<TrackId> {
    let project = session.project();
    let track = new_track(&project, kind);
    let id = track.id;
    let index = after
        .and_then(|a| project.track_index(a).ok())
        .map(|i| i + 1);
    let label = format!("Add {} track", kind.label().to_lowercase());
    apply(
        session,
        Transaction::user(
            label,
            vec![Command::AddTrack {
                track: Box::new(track),
                index,
            }],
        ),
        None,
    )?;
    Ok(id)
}

pub fn delete_track(session: &Session, id: TrackId) -> ActionResult<u64> {
    single(session, Command::RemoveTrack { id }, None)
}

pub fn move_track(session: &Session, id: TrackId, index: usize) -> ActionResult<u64> {
    single(session, Command::MoveTrack { id, index }, None)
}

pub fn rename_track(session: &Session, id: TrackId, name: &str) -> ActionResult<u64> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a track needs a name".into());
    }
    single(
        session,
        Command::SetTrackName {
            id,
            name: name.to_string(),
        },
        None,
    )
}

pub fn set_track_color(
    session: &Session,
    id: TrackId,
    color: [u8; 3],
    key: Option<u64>,
) -> ActionResult<u64> {
    single(session, Command::SetTrackColor { id, color }, key)
}

/// Changes any of a channel's fader, pan, mute or solo.
#[derive(Clone, Copy, Debug, Default)]
pub struct ChannelChange {
    pub volume_db: Option<f32>,
    pub pan: Option<f32>,
    pub mute: Option<bool>,
    pub solo: Option<bool>,
}

pub fn set_channel(
    session: &Session,
    channel: ChannelRef,
    change: ChannelChange,
    key: Option<u64>,
) -> ActionResult<u64> {
    let label = if change.volume_db.is_some() {
        "Change volume"
    } else if change.pan.is_some() {
        "Change pan"
    } else if change.mute.is_some() {
        "Toggle mute"
    } else {
        "Toggle solo"
    };
    apply(
        session,
        Transaction::user(
            label,
            vec![Command::SetChannel {
                channel,
                volume_db: change.volume_db,
                pan: change.pan,
                mute: change.mute,
                solo: change.solo,
            }],
        ),
        key,
    )
}

pub fn set_tempo(session: &Session, bpm: f64, key: Option<u64>) -> ActionResult<u64> {
    single(session, Command::SetTempo { bpm }, key)
}

pub fn set_time_signature(
    session: &Session,
    numerator: u32,
    denominator: u32,
    key: Option<u64>,
) -> ActionResult<u64> {
    single(
        session,
        Command::SetTimeSignature {
            numerator,
            denominator,
        },
        key,
    )
}

pub fn set_loop_region(session: &Session, start: Ticks, end: Ticks) -> ActionResult<u64> {
    single(session, Command::SetLoopRegion { start, end }, None)
}

pub fn set_loop_enabled(session: &Session, enabled: bool) -> ActionResult<u64> {
    single(session, Command::SetLoopEnabled { enabled }, None)
}

pub fn set_instrument_param(
    session: &Session,
    track: TrackId,
    param: &str,
    value: Option<f32>,
    key: Option<u64>,
) -> ActionResult<u64> {
    single(
        session,
        Command::SetInstrumentParam {
            track,
            param: param.to_string(),
            value,
        },
        key,
    )
}

pub fn set_instrument(
    session: &Session,
    track: TrackId,
    slot: InstrumentSlot,
    label: &str,
) -> ActionResult<u64> {
    apply(
        session,
        Transaction::user(
            label,
            vec![Command::SetInstrument {
                track,
                instrument: Some(slot),
            }],
        ),
        None,
    )
}

pub fn add_effect(
    session: &Session,
    channel: ChannelRef,
    kind: EffectType,
) -> ActionResult<EffectId> {
    let mut effect = EffectSlot::new(kind);
    if kind.takes_sidechain() {
        let own = match channel {
            ChannelRef::Track(t) => Some(t),
            ChannelRef::Master => None,
        };
        effect.sidechain = session.project().default_sidechain(own);
    }
    let id = effect.id;
    apply(
        session,
        Transaction::user(
            format!("Add {}", kind.label()),
            vec![Command::AddEffect {
                channel,
                effect,
                index: None,
            }],
        ),
        None,
    )?;
    Ok(id)
}

pub fn remove_effect(session: &Session, channel: ChannelRef, id: EffectId) -> ActionResult<u64> {
    single(session, Command::RemoveEffect { channel, id }, None)
}

pub fn move_effect(
    session: &Session,
    channel: ChannelRef,
    id: EffectId,
    index: usize,
) -> ActionResult<u64> {
    single(session, Command::MoveEffect { channel, id, index }, None)
}

pub fn set_effect_enabled(
    session: &Session,
    channel: ChannelRef,
    effect: EffectId,
    enabled: bool,
) -> ActionResult<u64> {
    single(
        session,
        Command::SetEffectEnabled {
            channel,
            effect,
            enabled,
        },
        None,
    )
}

pub fn set_effect_sidechain(
    session: &Session,
    channel: ChannelRef,
    effect: EffectId,
    source: Option<TrackId>,
) -> ActionResult<u64> {
    single(
        session,
        Command::SetEffectSidechain {
            channel,
            effect,
            source,
        },
        None,
    )
}

pub fn set_effect_param(
    session: &Session,
    channel: ChannelRef,
    effect: EffectId,
    param: &str,
    value: Option<f32>,
    key: Option<u64>,
) -> ActionResult<u64> {
    single(
        session,
        Command::SetEffectParam {
            channel,
            effect,
            param: param.to_string(),
            value,
        },
        key,
    )
}

/// Adds an empty MIDI clip and returns its ID.
pub fn add_midi_clip(
    session: &Session,
    track: TrackId,
    start: Ticks,
    length: Ticks,
) -> ActionResult<ClipId> {
    let clip = Clip {
        id: ClipId::new(),
        name: "Clip".into(),
        start: start.max(0),
        length: length.max(1),
        content: ClipContent::Midi { notes: Vec::new() },
    };
    let id = clip.id;
    apply(
        session,
        Transaction::user(
            "Add MIDI clip",
            vec![Command::AddClip {
                track,
                clip: Box::new(clip),
            }],
        ),
        None,
    )?;
    Ok(id)
}

/// Adds a pattern clip whose cycle is one bar.
pub fn add_pattern_clip(
    session: &Session,
    track: TrackId,
    start: Ticks,
    length: Ticks,
    source: &str,
) -> ActionResult<ClipId> {
    let project = session.project();
    let clip = Clip {
        id: ClipId::new(),
        name: "Pattern".into(),
        start: start.max(0),
        length: length.max(1),
        content: ClipContent::pattern(source, 0, project.ticks_per_bar()),
    };
    let id = clip.id;
    apply(
        session,
        Transaction::user(
            "Add pattern clip",
            vec![Command::AddClip {
                track,
                clip: Box::new(clip),
            }],
        ),
        None,
    )?;
    Ok(id)
}

/// Whether a track's step lanes are drums, one per piece, rather than a
/// melody whose steps each play their own key.
pub fn plays_drums(track: &Track) -> bool {
    track.instrument.as_ref().is_some_and(|i| match i.kind {
        InstrumentType::Drums => true,
        InstrumentType::Bank => i.bank.as_ref().is_none_or(|b| b.pitched.is_none()),
        _ => false,
    })
}

/// A one-bar step clip to start from: a beat on drums, a bass line on the
/// Acid bass, and an empty lane on anything else.
pub fn starter_steps(track: &Track) -> StepClip {
    let lane =
        |note: u8, text: &str| StepLane::parse(note, text).unwrap_or(StepLane::new(note, 16));
    let lanes = if plays_drums(track) {
        vec![
            lane(36, "x . . . x . . . x . . . x . . ."),
            lane(39, ". . . . x . . . . . . . x . . ."),
            lane(42, ". . x . . . x . . . x . . . x ."),
        ]
    } else if track
        .instrument
        .as_ref()
        .is_some_and(|i| i.kind == InstrumentType::Acid)
    {
        vec![lane(36, "x x c3! x . x/ d#2 x g1 . X c3/ a#2 x . d#2")]
    } else {
        vec![StepLane::new(48, 16)]
    };
    StepClip::new(16, lanes)
}

/// Adds a step clip with `steps`.
pub fn add_step_clip(
    session: &Session,
    track: TrackId,
    start: Ticks,
    length: Ticks,
    steps: StepClip,
) -> ActionResult<ClipId> {
    let clip = Clip {
        id: ClipId::new(),
        name: "Steps".into(),
        start: start.max(0),
        length: length.max(1),
        content: ClipContent::Steps(steps),
    };
    let id = clip.id;
    apply(
        session,
        Transaction::user(
            "Add step clip",
            vec![Command::AddClip {
                track,
                clip: Box::new(clip),
            }],
        ),
        None,
    )?;
    Ok(id)
}

/// Changes a step clip through `edit`.
pub fn edit_steps(
    session: &Session,
    clip: ClipId,
    key: Option<u64>,
    edit: impl FnOnce(&mut StepClip),
) -> ActionResult<u64> {
    let project = session.project();
    let (_, c) = project.find_clip(clip).map_err(|e| e.to_string())?;
    let ClipContent::Steps(steps) = &c.content else {
        return Err(format!("{} is not a step clip", c.name));
    };
    let mut steps = steps.clone();
    drop(project);
    edit(&mut steps);
    single(
        session,
        Command::SetSteps {
            clip,
            steps: Box::new(steps),
        },
        key,
    )
}

/// Adds an audio clip that plays a pool entry from its start, at its full length.
pub fn add_audio_clip(
    session: &Session,
    track: TrackId,
    start: Ticks,
    source: sky_core::AudioId,
) -> ActionResult<ClipId> {
    let project = session.project();
    let entry = project.pool_entry(source).map_err(|e| e.to_string())?;
    let clip = sky_session::audio_clip(&project, entry, start);
    let id = clip.id;
    apply(
        session,
        Transaction::user(
            format!("Add {}", entry.name),
            vec![Command::AddClip {
                track,
                clip: Box::new(clip),
            }],
        ),
        None,
    )?;
    Ok(id)
}

pub fn rename_clip(session: &Session, id: ClipId, name: &str) -> ActionResult<u64> {
    single(
        session,
        Command::SetClipName {
            id,
            name: name.trim().to_string(),
        },
        None,
    )
}

/// Changes an audio clip's gain or fades.
#[derive(Clone, Copy, Debug, Default)]
pub struct AudioClipChange {
    pub gain_db: Option<f32>,
    pub fade_in: Option<Ticks>,
    pub fade_out: Option<Ticks>,
}

pub fn set_audio_clip(
    session: &Session,
    id: ClipId,
    change: AudioClipChange,
    key: Option<u64>,
) -> ActionResult<u64> {
    single(
        session,
        Command::SetAudioClip {
            id,
            gain_db: change.gain_db,
            fade_in: change.fade_in.map(|f| f.max(0)),
            fade_out: change.fade_out.map(|f| f.max(0)),
            offset: None,
        },
        key,
    )
}

pub fn add_note(session: &Session, clip: ClipId, note: Note) -> ActionResult<NoteId> {
    let id = note.id;
    apply(
        session,
        Transaction::user(
            "Add note",
            vec![Command::AddNotes {
                clip,
                notes: vec![note],
            }],
        ),
        None,
    )?;
    Ok(id)
}

pub fn update_notes(
    session: &Session,
    clip: ClipId,
    notes: Vec<Note>,
    label: &str,
    key: Option<u64>,
) -> ActionResult<u64> {
    if notes.is_empty() {
        return Err("no notes to change".into());
    }
    apply(
        session,
        Transaction::user(label, vec![Command::UpdateNotes { clip, notes }]),
        key,
    )
}

pub fn remove_notes(session: &Session, clip: ClipId, ids: Vec<NoteId>) -> ActionResult<u64> {
    if ids.is_empty() {
        return Err("no notes selected".into());
    }
    let label = if ids.len() == 1 {
        "Delete note"
    } else {
        "Delete notes"
    };
    apply(
        session,
        Transaction::user(label, vec![Command::RemoveNotes { clip, ids }]),
        None,
    )
}

/// A pattern clip's settings, for changing some and keeping the rest.
#[derive(Clone, Debug)]
pub struct PatternFields {
    pub source: String,
    pub seed: u64,
    pub cycle: Ticks,
    pub controls: BTreeMap<String, String>,
    pub swing: Swing,
}

/// Changes a pattern clip through `edit`, keeping whatever it leaves alone.
pub fn edit_pattern(
    session: &Session,
    clip: ClipId,
    key: Option<u64>,
    edit: impl FnOnce(&mut PatternFields),
) -> ActionResult<u64> {
    let project = session.project();
    let (_, c) = project.find_clip(clip).map_err(|e| e.to_string())?;
    let ClipContent::Pattern {
        source,
        seed,
        cycle,
        controls,
        swing,
    } = &c.content
    else {
        return Err(format!("{} is not a pattern clip", c.name));
    };
    let mut fields = PatternFields {
        source: source.clone(),
        seed: *seed,
        cycle: *cycle,
        controls: controls.clone(),
        swing: *swing,
    };
    drop(project);
    edit(&mut fields);
    single(
        session,
        Command::SetPattern {
            clip,
            source: fields.source,
            seed: fields.seed,
            cycle: fields.cycle.max(1),
            controls: fields.controls,
            swing: fields.swing,
        },
        key,
    )
}

pub fn set_pattern(
    session: &Session,
    clip: ClipId,
    source: &str,
    seed: u64,
    cycle: Ticks,
    key: Option<u64>,
) -> ActionResult<u64> {
    edit_pattern(session, clip, key, |f| {
        f.source = source.to_string();
        f.seed = seed;
        f.cycle = cycle;
    })
}

/// Sets one control pattern of a clip; `None` removes it.
pub fn set_pattern_control(
    session: &Session,
    clip: ClipId,
    param: &str,
    source: Option<&str>,
) -> ActionResult<u64> {
    edit_pattern(session, clip, None, |f| match source {
        Some(s) => {
            f.controls.insert(param.to_string(), s.to_string());
        }
        None => {
            f.controls.remove(param);
        }
    })
}

pub fn set_swing(
    session: &Session,
    clip: ClipId,
    swing: Swing,
    key: Option<u64>,
) -> ActionResult<u64> {
    edit_pattern(session, clip, key, |f| f.swing = swing)
}

/// A project with `tracks` synth tracks, each with a pattern or MIDI clip,
/// for testing how the UI copes with large sessions.
pub fn stress_project(tracks: usize) -> Project {
    let mut p = Project::new(format!("Stress test ({tracks} tracks)"));
    let bar = p.ticks_per_bar();
    let patterns = [
        "c4 e4 g4 c5",
        "<a3 f3 c4 g3>*2 ~ e4",
        "[c4 ~ g4] e4*2",
        "c3(3,8), g4*4?",
        "<c4 e4> [g4 b4]",
    ];
    for i in 0..tracks {
        let mut t = Track::new_instrument(format!("Synth {}", i + 1), InstrumentType::Synth);
        let hue = (i as f32 * 0.137).fract();
        t.color = hue_to_rgb(hue);
        // Many voices at full level would only clip; keep the sum sane.
        t.channel.volume_db = -24.0;
        let clip = if i % 2 == 0 {
            Clip {
                id: ClipId::new(),
                name: format!("Pattern {}", i + 1),
                start: 0,
                length: bar * 8,
                content: ClipContent::pattern(patterns[i % patterns.len()], i as u64, bar),
            }
        } else {
            let notes = (0..32)
                .map(|n| {
                    let pitch = 48 + ((n * 7 + i * 3) % 24) as u8;
                    Note::new(n as Ticks * PPQ, PPQ / 2, pitch, 70 + (n % 4) as u8 * 15)
                })
                .collect();
            Clip {
                id: ClipId::new(),
                name: format!("Notes {}", i + 1),
                start: 0,
                length: bar * 8,
                content: ClipContent::Midi { notes },
            }
        };
        t.clips.push(clip);
        p.tracks.push(t);
    }
    p
}

/// A pleasant, not too bright color for a hue in 0..1.
pub fn hue_to_rgb(hue: f32) -> [u8; 3] {
    let h = hue.rem_euclid(1.0) * 6.0;
    let (s, v) = (0.45f32, 0.82f32);
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    ]
}
