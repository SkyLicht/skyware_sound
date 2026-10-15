// SPDX-License-Identifier: GPL-3.0-or-later
//! Commands: every change to a project.
//!
//! A command is plain serializable data. [`Command::apply`] changes the
//! project and returns the command that undoes it. IDs are chosen when a
//! command is built, never while applying, so replaying a command log always
//! produces the same project.

use serde::{Deserialize, Serialize};
use sky_core::manifest::{MAX_ASSET_NAME, RandomRange, is_asset_name};
use sky_core::time::validate_bpm;
use sky_core::{
    AssetId, AudioId, ClipId, EffectId, MarkerId, NoteId, TempoMap, Ticks, TimeSignature, TrackId,
};

use crate::error::ModelError;
use crate::project::*;

/// A mixer channel: the master or a track's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChannelRef {
    Master,
    Track(TrackId),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum Command {
    SetProjectName {
        name: String,
    },
    SetTempo {
        bpm: f64,
    },
    /// Replaces the whole tempo map; used to undo tempo changes exactly.
    SetTempoMap {
        map: TempoMap,
    },
    SetTimeSignature {
        numerator: u32,
        denominator: u32,
    },
    SetLoopRegion {
        start: Ticks,
        end: Ticks,
    },
    SetLoopEnabled {
        enabled: bool,
    },
    AddMarker {
        marker: Marker,
    },
    RemoveMarker {
        id: MarkerId,
    },
    AddTrack {
        track: Box<Track>,
        index: Option<usize>,
    },
    RemoveTrack {
        id: TrackId,
    },
    MoveTrack {
        id: TrackId,
        index: usize,
    },
    SetTrackName {
        id: TrackId,
        name: String,
    },
    SetTrackColor {
        id: TrackId,
        color: [u8; 3],
    },
    #[serde(rename_all = "camelCase")]
    SetChannel {
        channel: ChannelRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        volume_db: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pan: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mute: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        solo: Option<bool>,
    },
    SetInstrument {
        track: TrackId,
        instrument: Option<InstrumentSlot>,
    },
    /// Sets an instrument parameter; `None` restores the default.
    SetInstrumentParam {
        track: TrackId,
        param: String,
        value: Option<f32>,
    },
    AddEffect {
        channel: ChannelRef,
        effect: EffectSlot,
        index: Option<usize>,
    },
    RemoveEffect {
        channel: ChannelRef,
        id: EffectId,
    },
    MoveEffect {
        channel: ChannelRef,
        id: EffectId,
        index: usize,
    },
    /// Sets an effect parameter; `None` restores the default.
    SetEffectParam {
        channel: ChannelRef,
        effect: EffectId,
        param: String,
        value: Option<f32>,
    },
    SetEffectEnabled {
        channel: ChannelRef,
        effect: EffectId,
        enabled: bool,
    },
    AddClip {
        track: TrackId,
        clip: Box<Clip>,
    },
    RemoveClip {
        id: ClipId,
    },
    /// Moves a clip in time and optionally onto another track.
    MoveClip {
        id: ClipId,
        track: TrackId,
        start: Ticks,
    },
    SetClipLength {
        id: ClipId,
        length: Ticks,
    },
    SetClipName {
        id: ClipId,
        name: String,
    },
    #[serde(rename_all = "camelCase")]
    SetAudioClip {
        id: ClipId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gain_db: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fade_in: Option<Ticks>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fade_out: Option<Ticks>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offset: Option<i64>,
    },
    AddNotes {
        clip: ClipId,
        notes: Vec<Note>,
    },
    RemoveNotes {
        clip: ClipId,
        ids: Vec<NoteId>,
    },
    /// Replaces notes that already exist (matched by ID): move, resize or re-velocity.
    UpdateNotes {
        clip: ClipId,
        notes: Vec<Note>,
    },
    SetPattern {
        clip: ClipId,
        source: String,
        seed: u64,
        cycle: Ticks,
    },
    AddPoolAudio {
        entry: PoolEntry,
        index: Option<usize>,
    },
    RemovePoolAudio {
        id: AudioId,
    },
    /// Sets where game exports go and their bit depth.
    #[serde(rename_all = "camelCase")]
    SetGameExport {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        export_dir: Option<String>,
        bits: u16,
    },
    AddGameAsset {
        asset: Box<GameAsset>,
        index: Option<usize>,
    },
    RemoveGameAsset {
        id: AssetId,
    },
    /// Replaces a game asset that already exists (matched by ID).
    UpdateGameAsset {
        asset: Box<GameAsset>,
    },
}

fn channel_mut(project: &mut Project, channel: ChannelRef) -> Result<&mut Channel, ModelError> {
    match channel {
        ChannelRef::Master => Ok(&mut project.master),
        ChannelRef::Track(id) => Ok(&mut project.track_mut(id)?.channel),
    }
}

fn check_volume(db: f32) -> Result<f32, ModelError> {
    if db.is_finite() && (-120.0..=12.0).contains(&db) {
        Ok(db)
    } else {
        Err(ModelError::InvalidValue(format!(
            "volume must be between -120 and +12 dB, got {db}"
        )))
    }
}

fn check_pan(pan: f32) -> Result<f32, ModelError> {
    if pan.is_finite() && (-1.0..=1.0).contains(&pan) {
        Ok(pan)
    } else {
        Err(ModelError::InvalidValue(format!(
            "pan must be between -1 (left) and 1 (right), got {pan}"
        )))
    }
}

fn check_clip_bounds(start: Ticks, length: Ticks) -> Result<(), ModelError> {
    if start < 0 || length <= 0 {
        return Err(ModelError::InvalidValue(format!(
            "a clip needs start >= 0 and length > 0, got start {start} length {length}"
        )));
    }
    Ok(())
}

fn validate_param(
    specs: &[sky_dsp::ParamSpec],
    owner: &str,
    name: &str,
    value: f32,
) -> Result<f32, ModelError> {
    let spec = sky_dsp::params::find(specs, name).ok_or_else(|| {
        let names: Vec<&str> = specs.iter().map(|s| s.name).collect();
        ModelError::InvalidParam(format!(
            "{owner} has no parameter {name:?}; it has {}",
            names.join(", ")
        ))
    })?;
    spec.validate(value).map_err(ModelError::InvalidParam)
}

fn validate_clip(project: &Project, track: &Track, clip: &Clip) -> Result<(), ModelError> {
    check_clip_bounds(clip.start, clip.length)?;
    match &clip.content {
        ClipContent::Audio { source, .. } => {
            if track.kind != TrackKind::Audio {
                return Err(ModelError::InvalidValue(
                    "audio clips go on audio tracks".into(),
                ));
            }
            project.pool_entry(*source)?;
        }
        ClipContent::Midi { notes } => {
            if track.kind != TrackKind::Instrument {
                return Err(ModelError::InvalidValue(
                    "MIDI clips go on instrument tracks".into(),
                ));
            }
            for n in notes {
                n.validate()?;
            }
        }
        ClipContent::Pattern { source, cycle, .. } => {
            if track.kind != TrackKind::Instrument {
                return Err(ModelError::InvalidValue(
                    "pattern clips go on instrument tracks".into(),
                ));
            }
            validate_pattern(source, *cycle)?;
        }
    }
    Ok(())
}

pub fn validate_pattern(source: &str, cycle: Ticks) -> Result<(), ModelError> {
    if cycle <= 0 {
        return Err(ModelError::InvalidValue(format!(
            "a pattern cycle must be longer than 0 ticks, got {cycle}"
        )));
    }
    sky_pattern::parse(source).map_err(|e| ModelError::Pattern(e.to_string()))?;
    Ok(())
}

fn validate_instrument(project: &Project, slot: &InstrumentSlot) -> Result<(), ModelError> {
    for (name, value) in &slot.params {
        validate_param(slot.kind.params(), slot.kind.name(), name, *value)?;
    }
    if let Some(sample) = slot.sample {
        project.pool_entry(sample)?;
    }
    if let Some(bank) = &slot.bank {
        if slot.kind != sky_dsp::InstrumentType::Bank {
            return Err(ModelError::InvalidValue(format!(
                "only a sample-bank instrument can hold sounds, not a {}",
                slot.kind.name()
            )));
        }
        validate_bank(project, bank)?;
    }
    Ok(())
}

/// The longest sound name a bank accepts.
const MAX_SOUND_NAME: usize = 64;

fn validate_bank(project: &Project, bank: &SampleBank) -> Result<(), ModelError> {
    for (name, samples) in &bank.sounds {
        let ok = !name.is_empty()
            && name.len() <= MAX_SOUND_NAME
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '#'));
        if !ok {
            return Err(ModelError::InvalidValue(format!(
                "bank sound names use letters, digits, _ - . or # and are at most \
                 {MAX_SOUND_NAME} long, got {name:?}"
            )));
        }
        if samples.is_empty() {
            return Err(ModelError::InvalidValue(format!(
                "bank sound {name:?} has no samples"
            )));
        }
        for s in samples {
            project.pool_entry(s.audio)?;
            if s.root.is_some_and(|r| r > 127) {
                return Err(ModelError::InvalidValue(format!(
                    "bank sound {name:?} has a root note above 127"
                )));
            }
        }
    }
    if let Some(p) = &bank.pitched
        && !bank.sounds.contains_key(p)
    {
        return Err(ModelError::InvalidValue(format!(
            "the bank has no sound {p:?} to play notes with"
        )));
    }
    Ok(())
}

fn validate_effect(slot: &EffectSlot) -> Result<(), ModelError> {
    for (name, value) in &slot.params {
        validate_param(slot.kind.params(), slot.kind.name(), name, *value)?;
    }
    Ok(())
}

fn id_in_use(project: &Project, uuid: uuid::Uuid) -> bool {
    project.tracks.iter().any(|t| {
        t.id.0 == uuid
            || t.clips.iter().any(|c| c.id.0 == uuid)
            || t.channel.effects.iter().any(|e| e.id.0 == uuid)
    }) || project.master.effects.iter().any(|e| e.id.0 == uuid)
}

fn ensure_unique(project: &Project, uuid: uuid::Uuid) -> Result<(), ModelError> {
    if id_in_use(project, uuid) {
        Err(ModelError::DuplicateId(uuid.to_string()))
    } else {
        Ok(())
    }
}

fn clip_track_mut(project: &mut Project, id: ClipId) -> Result<(usize, usize), ModelError> {
    for (ti, t) in project.tracks.iter().enumerate() {
        if let Some(ci) = t.clips.iter().position(|c| c.id == id) {
            return Ok((ti, ci));
        }
    }
    Err(ModelError::ClipNotFound(id))
}

fn midi_notes_mut(project: &mut Project, clip: ClipId) -> Result<&mut Vec<Note>, ModelError> {
    let c = project.clip_mut(clip)?;
    let actual = c.kind_name();
    match &mut c.content {
        ClipContent::Midi { notes } => Ok(notes),
        _ => Err(ModelError::WrongClipType {
            id: clip,
            expected: "midi",
            actual,
        }),
    }
}

fn sort_notes(notes: &mut [Note]) {
    notes.sort_by_key(|n| (n.start, n.pitch, n.id));
}

/// The most tags a game asset can have, and the longest tag.
const MAX_TAGS: usize = 16;
const MAX_TAG: usize = 64;

fn validate_game_asset(project: &Project, asset: &GameAsset) -> Result<(), ModelError> {
    if !is_asset_name(&asset.name) {
        return Err(ModelError::InvalidValue(format!(
            "game asset names use lowercase letters, digits and single underscores, at most \
             {MAX_ASSET_NAME} long, such as footstep_grass; got {:?}",
            asset.name
        )));
    }
    if let Some(other) = project
        .game
        .assets
        .iter()
        .find(|a| a.name == asset.name && a.id != asset.id)
    {
        return Err(ModelError::InvalidValue(format!(
            "another game asset ({}) is already called {:?}",
            other.id, asset.name
        )));
    }
    if asset.tags.len() > MAX_TAGS
        || asset
            .tags
            .iter()
            .any(|t| t.trim().is_empty() || t.len() > MAX_TAG)
    {
        return Err(ModelError::InvalidValue(format!(
            "a game asset has at most {MAX_TAGS} tags, each 1 to {MAX_TAG} characters"
        )));
    }
    match &asset.kind {
        GameAssetKind::Loop { range, intro_start } => {
            TimeRange::new(range.start, range.end)?;
            if let Some(intro) = intro_start
                && (*intro < 0 || *intro >= range.start)
            {
                return Err(ModelError::InvalidValue(format!(
                    "the intro must start at or after 0 and before the loop starts at tick {}, got {intro}",
                    range.start
                )));
            }
        }
        GameAssetKind::Sfx {
            variations,
            pitch_semitones,
            gain_db,
            tail_seconds,
            ..
        } => {
            if variations.is_empty() || variations.len() > MAX_SFX_VARIATIONS {
                return Err(ModelError::InvalidValue(format!(
                    "an SFX set needs 1 to {MAX_SFX_VARIATIONS} variations, got {}",
                    variations.len()
                )));
            }
            for v in variations {
                TimeRange::new(v.start, v.end)?;
            }
            let pitch = MAX_SFX_PITCH_SEMITONES;
            check_range("random pitch", *pitch_semitones, -pitch, pitch, "semitones")?;
            check_range("random gain", *gain_db, SFX_GAIN_DB.0, SFX_GAIN_DB.1, "dB")?;
            if !(tail_seconds.is_finite() && (0.0..=MAX_SFX_TAIL_SECONDS).contains(tail_seconds)) {
                return Err(ModelError::InvalidValue(format!(
                    "an SFX tail must be 0 to {MAX_SFX_TAIL_SECONDS} seconds, got {tail_seconds}"
                )));
            }
        }
    }
    Ok(())
}

fn check_range(
    what: &str,
    range: RandomRange,
    lo: f32,
    hi: f32,
    unit: &str,
) -> Result<(), ModelError> {
    let ok = |v: f32| v.is_finite() && (lo..=hi).contains(&v);
    if ok(range.min) && ok(range.max) && range.min <= range.max {
        Ok(())
    } else {
        Err(ModelError::InvalidValue(format!(
            "{what} must be a range from min to max within {lo} to {hi} {unit}, got {} to {}",
            range.min, range.max
        )))
    }
}

fn game_asset_index(project: &Project, id: AssetId) -> Result<usize, ModelError> {
    project
        .game
        .assets
        .iter()
        .position(|a| a.id == id)
        .ok_or(ModelError::GameAssetNotFound(id))
}

impl Command {
    /// A short description for history lists.
    pub fn describe(&self) -> &'static str {
        match self {
            Command::SetProjectName { .. } => "Rename project",
            Command::SetTempo { .. } | Command::SetTempoMap { .. } => "Change tempo",
            Command::SetTimeSignature { .. } => "Change time signature",
            Command::SetLoopRegion { .. } => "Set loop region",
            Command::SetLoopEnabled { .. } => "Toggle loop",
            Command::AddMarker { .. } => "Add marker",
            Command::RemoveMarker { .. } => "Remove marker",
            Command::AddTrack { .. } => "Add track",
            Command::RemoveTrack { .. } => "Delete track",
            Command::MoveTrack { .. } => "Move track",
            Command::SetTrackName { .. } => "Rename track",
            Command::SetTrackColor { .. } => "Recolor track",
            Command::SetChannel { .. } => "Change mixer",
            Command::SetInstrument { .. } => "Change instrument",
            Command::SetInstrumentParam { .. } => "Change instrument parameter",
            Command::AddEffect { .. } => "Add effect",
            Command::RemoveEffect { .. } => "Remove effect",
            Command::MoveEffect { .. } => "Move effect",
            Command::SetEffectParam { .. } => "Change effect parameter",
            Command::SetEffectEnabled { .. } => "Toggle effect",
            Command::AddClip { .. } => "Add clip",
            Command::RemoveClip { .. } => "Delete clip",
            Command::MoveClip { .. } => "Move clip",
            Command::SetClipLength { .. } => "Resize clip",
            Command::SetClipName { .. } => "Rename clip",
            Command::SetAudioClip { .. } => "Edit audio clip",
            Command::AddNotes { .. } => "Add notes",
            Command::RemoveNotes { .. } => "Delete notes",
            Command::UpdateNotes { .. } => "Edit notes",
            Command::SetPattern { .. } => "Edit pattern",
            Command::AddPoolAudio { .. } => "Import audio",
            Command::RemovePoolAudio { .. } => "Remove audio",
            Command::SetGameExport { .. } => "Change game export",
            Command::AddGameAsset { .. } => "Add game asset",
            Command::RemoveGameAsset { .. } => "Remove game asset",
            Command::UpdateGameAsset { .. } => "Edit game asset",
        }
    }

    /// Applies the command and returns its inverse. On error the project is unchanged.
    pub fn apply(self, project: &mut Project) -> Result<Command, ModelError> {
        Ok(match self {
            Command::SetProjectName { name } => {
                let old = std::mem::replace(&mut project.name, name);
                Command::SetProjectName { name: old }
            }
            Command::SetTempo { bpm } => {
                validate_bpm(bpm)?;
                let old = std::mem::replace(&mut project.tempo, TempoMap::constant(bpm));
                Command::SetTempoMap { map: old }
            }
            Command::SetTempoMap { map } => {
                let map = TempoMap::from_points(map.points().to_vec())?;
                let old = std::mem::replace(&mut project.tempo, map);
                Command::SetTempoMap { map: old }
            }
            Command::SetTimeSignature {
                numerator,
                denominator,
            } => {
                let ts = TimeSignature::new(numerator, denominator)?;
                let old = std::mem::replace(&mut project.time_signature, ts);
                Command::SetTimeSignature {
                    numerator: old.numerator,
                    denominator: old.denominator,
                }
            }
            Command::SetLoopRegion { start, end } => {
                let range = TimeRange::new(start, end)?;
                let old = std::mem::replace(&mut project.loop_region, range);
                Command::SetLoopRegion {
                    start: old.start,
                    end: old.end,
                }
            }
            Command::SetLoopEnabled { enabled } => {
                let old = std::mem::replace(&mut project.loop_enabled, enabled);
                Command::SetLoopEnabled { enabled: old }
            }
            Command::AddMarker { marker } => {
                if project.markers.iter().any(|m| m.id == marker.id) {
                    return Err(ModelError::DuplicateId(marker.id.to_string()));
                }
                let id = marker.id;
                project.markers.push(marker);
                project.markers.sort_by_key(|m| (m.tick, m.id));
                Command::RemoveMarker { id }
            }
            Command::RemoveMarker { id } => {
                let i = project
                    .markers
                    .iter()
                    .position(|m| m.id == id)
                    .ok_or(ModelError::MarkerNotFound(id))?;
                let marker = project.markers.remove(i);
                Command::AddMarker { marker }
            }
            Command::AddTrack { track, index } => {
                ensure_unique(project, track.id.0)?;
                for c in &track.clips {
                    ensure_unique(project, c.id.0)?;
                    validate_clip(project, &track, c)?;
                }
                if let Some(inst) = &track.instrument {
                    validate_instrument(project, inst)?;
                }
                if track.kind == TrackKind::Audio && track.instrument.is_some() {
                    return Err(ModelError::InvalidValue(
                        "audio tracks cannot hold an instrument".into(),
                    ));
                }
                check_volume(track.channel.volume_db)?;
                check_pan(track.channel.pan)?;
                for e in &track.channel.effects {
                    ensure_unique(project, e.id.0)?;
                    validate_effect(e)?;
                }
                let id = track.id;
                let at = index
                    .unwrap_or(project.tracks.len())
                    .min(project.tracks.len());
                project.tracks.insert(at, *track);
                Command::RemoveTrack { id }
            }
            Command::RemoveTrack { id } => {
                let i = project.track_index(id)?;
                let track = project.tracks.remove(i);
                Command::AddTrack {
                    track: Box::new(track),
                    index: Some(i),
                }
            }
            Command::MoveTrack { id, index } => {
                let i = project.track_index(id)?;
                let track = project.tracks.remove(i);
                let at = index.min(project.tracks.len());
                project.tracks.insert(at, track);
                Command::MoveTrack { id, index: i }
            }
            Command::SetTrackName { id, name } => {
                let t = project.track_mut(id)?;
                let old = std::mem::replace(&mut t.name, name);
                Command::SetTrackName { id, name: old }
            }
            Command::SetTrackColor { id, color } => {
                let t = project.track_mut(id)?;
                let old = std::mem::replace(&mut t.color, color);
                Command::SetTrackColor { id, color: old }
            }
            Command::SetChannel {
                channel,
                volume_db,
                pan,
                mute,
                solo,
            } => {
                if let Some(v) = volume_db {
                    check_volume(v)?;
                }
                if let Some(p) = pan {
                    check_pan(p)?;
                }
                if channel == ChannelRef::Master && solo.is_some() {
                    return Err(ModelError::InvalidValue(
                        "the master channel cannot be soloed".into(),
                    ));
                }
                let ch = channel_mut(project, channel)?;
                let inverse = Command::SetChannel {
                    channel,
                    volume_db: volume_db.map(|_| ch.volume_db),
                    pan: pan.map(|_| ch.pan),
                    mute: mute.map(|_| ch.mute),
                    solo: solo.map(|_| ch.solo),
                };
                if let Some(v) = volume_db {
                    ch.volume_db = v;
                }
                if let Some(p) = pan {
                    ch.pan = p;
                }
                if let Some(m) = mute {
                    ch.mute = m;
                }
                if let Some(s) = solo {
                    ch.solo = s;
                }
                inverse
            }
            Command::SetInstrument { track, instrument } => {
                if let Some(slot) = &instrument {
                    validate_instrument(project, slot)?;
                }
                let t = project.track_mut(track)?;
                if t.kind != TrackKind::Instrument {
                    return Err(ModelError::NotAnInstrumentTrack(track));
                }
                let old = std::mem::replace(&mut t.instrument, instrument);
                Command::SetInstrument {
                    track,
                    instrument: old,
                }
            }
            Command::SetInstrumentParam {
                track,
                param,
                value,
            } => {
                let t = project.track_mut(track)?;
                let slot = t
                    .instrument
                    .as_mut()
                    .ok_or(ModelError::NotAnInstrumentTrack(track))?;
                let old = slot.params.get(&param).copied();
                match value {
                    Some(v) => {
                        let v = validate_param(slot.kind.params(), slot.kind.name(), &param, v)?;
                        slot.params.insert(param.clone(), v);
                    }
                    None => {
                        let specs = slot.kind.params();
                        if sky_dsp::params::find(specs, &param).is_none() {
                            validate_param(specs, slot.kind.name(), &param, 0.0)?;
                        }
                        slot.params.remove(&param);
                    }
                }
                Command::SetInstrumentParam {
                    track,
                    param,
                    value: old,
                }
            }
            Command::AddEffect {
                channel,
                effect,
                index,
            } => {
                ensure_unique(project, effect.id.0)?;
                validate_effect(&effect)?;
                let id = effect.id;
                let ch = channel_mut(project, channel)?;
                let at = index.unwrap_or(ch.effects.len()).min(ch.effects.len());
                ch.effects.insert(at, effect);
                Command::RemoveEffect { channel, id }
            }
            Command::RemoveEffect { channel, id } => {
                let ch = channel_mut(project, channel)?;
                let i = ch
                    .effects
                    .iter()
                    .position(|e| e.id == id)
                    .ok_or(ModelError::EffectNotFound(id))?;
                let effect = ch.effects.remove(i);
                Command::AddEffect {
                    channel,
                    effect,
                    index: Some(i),
                }
            }
            Command::MoveEffect { channel, id, index } => {
                let ch = channel_mut(project, channel)?;
                let i = ch
                    .effects
                    .iter()
                    .position(|e| e.id == id)
                    .ok_or(ModelError::EffectNotFound(id))?;
                let e = ch.effects.remove(i);
                let at = index.min(ch.effects.len());
                ch.effects.insert(at, e);
                Command::MoveEffect {
                    channel,
                    id,
                    index: i,
                }
            }
            Command::SetEffectParam {
                channel,
                effect,
                param,
                value,
            } => {
                let ch = channel_mut(project, channel)?;
                let slot = ch
                    .effects
                    .iter_mut()
                    .find(|e| e.id == effect)
                    .ok_or(ModelError::EffectNotFound(effect))?;
                let specs = slot.kind.params();
                if sky_dsp::params::find(specs, &param).is_none() {
                    validate_param(specs, slot.kind.name(), &param, 0.0)?;
                }
                let old = slot.params.get(&param).copied();
                match value {
                    Some(v) => {
                        let v = validate_param(specs, slot.kind.name(), &param, v)?;
                        slot.params.insert(param.clone(), v);
                    }
                    None => {
                        slot.params.remove(&param);
                    }
                }
                Command::SetEffectParam {
                    channel,
                    effect,
                    param,
                    value: old,
                }
            }
            Command::SetEffectEnabled {
                channel,
                effect,
                enabled,
            } => {
                let ch = channel_mut(project, channel)?;
                let slot = ch
                    .effects
                    .iter_mut()
                    .find(|e| e.id == effect)
                    .ok_or(ModelError::EffectNotFound(effect))?;
                let old = std::mem::replace(&mut slot.enabled, enabled);
                Command::SetEffectEnabled {
                    channel,
                    effect,
                    enabled: old,
                }
            }
            Command::AddClip { track, clip } => {
                ensure_unique(project, clip.id.0)?;
                validate_clip(project, project.track(track)?, &clip)?;
                let id = clip.id;
                let t = project.track_mut(track)?;
                t.clips.push(*clip);
                t.clips.sort_by_key(|c| (c.start, c.id));
                Command::RemoveClip { id }
            }
            Command::RemoveClip { id } => {
                let (ti, ci) = clip_track_mut(project, id)?;
                let t = &mut project.tracks[ti];
                let clip = t.clips.remove(ci);
                Command::AddClip {
                    track: t.id,
                    clip: Box::new(clip),
                }
            }
            Command::MoveClip { id, track, start } => {
                let (ti, ci) = clip_track_mut(project, id)?;
                let mut clip = project.tracks[ti].clips[ci].clone();
                let old_track = project.tracks[ti].id;
                let old_start = clip.start;
                clip.start = start;
                let target = project.track(track)?;
                validate_clip(project, target, &clip)?;
                project.tracks[ti].clips.remove(ci);
                let t = project.track_mut(track)?;
                t.clips.push(clip);
                t.clips.sort_by_key(|c| (c.start, c.id));
                Command::MoveClip {
                    id,
                    track: old_track,
                    start: old_start,
                }
            }
            Command::SetClipLength { id, length } => {
                let c = project.clip_mut(id)?;
                check_clip_bounds(c.start, length)?;
                let old = std::mem::replace(&mut c.length, length);
                Command::SetClipLength { id, length: old }
            }
            Command::SetClipName { id, name } => {
                let c = project.clip_mut(id)?;
                let old = std::mem::replace(&mut c.name, name);
                Command::SetClipName { id, name: old }
            }
            Command::SetAudioClip {
                id,
                gain_db,
                fade_in,
                fade_out,
                offset,
            } => {
                if let Some(g) = gain_db
                    && (!g.is_finite() || !(-60.0..=24.0).contains(&g))
                {
                    return Err(ModelError::InvalidValue(format!(
                        "clip gain must be between -60 and +24 dB, got {g}"
                    )));
                }
                for f in [fade_in, fade_out].into_iter().flatten() {
                    if f < 0 {
                        return Err(ModelError::InvalidValue("fades cannot be negative".into()));
                    }
                }
                if offset.is_some_and(|o| o < 0) {
                    return Err(ModelError::InvalidValue("offset cannot be negative".into()));
                }
                let c = project.clip_mut(id)?;
                let actual = c.kind_name();
                let ClipContent::Audio {
                    gain_db: g,
                    fade_in: fi,
                    fade_out: fo,
                    offset: off,
                    ..
                } = &mut c.content
                else {
                    return Err(ModelError::WrongClipType {
                        id,
                        expected: "audio",
                        actual,
                    });
                };
                let inverse = Command::SetAudioClip {
                    id,
                    gain_db: gain_db.map(|_| *g),
                    fade_in: fade_in.map(|_| *fi),
                    fade_out: fade_out.map(|_| *fo),
                    offset: offset.map(|_| *off),
                };
                if let Some(v) = gain_db {
                    *g = v;
                }
                if let Some(v) = fade_in {
                    *fi = v;
                }
                if let Some(v) = fade_out {
                    *fo = v;
                }
                if let Some(v) = offset {
                    *off = v;
                }
                inverse
            }
            Command::AddNotes { clip, notes } => {
                for n in &notes {
                    n.validate()?;
                }
                let existing = midi_notes_mut(project, clip)?;
                for n in &notes {
                    if existing.iter().any(|e| e.id == n.id)
                        || notes.iter().filter(|m| m.id == n.id).count() > 1
                    {
                        return Err(ModelError::DuplicateId(n.id.to_string()));
                    }
                }
                let ids = notes.iter().map(|n| n.id).collect();
                existing.extend(notes);
                sort_notes(existing);
                Command::RemoveNotes { clip, ids }
            }
            Command::RemoveNotes { clip, ids } => {
                let existing = midi_notes_mut(project, clip)?;
                if let Some(missing) = ids.iter().find(|id| !existing.iter().any(|n| n.id == **id))
                {
                    return Err(ModelError::NoteNotFound(*missing));
                }
                let mut removed = Vec::new();
                existing.retain(|n| {
                    if ids.contains(&n.id) {
                        removed.push(*n);
                        false
                    } else {
                        true
                    }
                });
                Command::AddNotes {
                    clip,
                    notes: removed,
                }
            }
            Command::UpdateNotes { clip, notes } => {
                for n in &notes {
                    n.validate()?;
                }
                let existing = midi_notes_mut(project, clip)?;
                if let Some(missing) = notes
                    .iter()
                    .find(|n| !existing.iter().any(|e| e.id == n.id))
                {
                    return Err(ModelError::NoteNotFound(missing.id));
                }
                let mut old = Vec::with_capacity(notes.len());
                for n in notes {
                    if let Some(e) = existing.iter_mut().find(|e| e.id == n.id) {
                        old.push(std::mem::replace(e, n));
                    }
                }
                sort_notes(existing);
                Command::UpdateNotes { clip, notes: old }
            }
            Command::SetPattern {
                clip,
                source,
                seed,
                cycle,
            } => {
                validate_pattern(&source, cycle)?;
                let c = project.clip_mut(clip)?;
                let actual = c.kind_name();
                let ClipContent::Pattern {
                    source: s,
                    seed: sd,
                    cycle: cy,
                } = &mut c.content
                else {
                    return Err(ModelError::WrongClipType {
                        id: clip,
                        expected: "pattern",
                        actual,
                    });
                };
                Command::SetPattern {
                    clip,
                    source: std::mem::replace(s, source),
                    seed: std::mem::replace(sd, seed),
                    cycle: std::mem::replace(cy, cycle),
                }
            }
            Command::AddPoolAudio { entry, index } => {
                if project.audio_pool.iter().any(|e| e.id == entry.id) {
                    return Err(ModelError::DuplicateId(entry.id.to_string()));
                }
                let id = entry.id;
                let at = index
                    .unwrap_or(project.audio_pool.len())
                    .min(project.audio_pool.len());
                project.audio_pool.insert(at, entry);
                Command::RemovePoolAudio { id }
            }
            Command::RemovePoolAudio { id } => {
                let used = project.tracks.iter().any(|t| {
                    t.instrument.as_ref().is_some_and(|i| {
                        i.sample == Some(id)
                            || i.bank
                                .as_ref()
                                .is_some_and(|b| b.audio_ids().any(|a| a == id))
                    }) || t.clips.iter().any(
                        |c| matches!(c.content, ClipContent::Audio { source, .. } if source == id),
                    )
                });
                if used {
                    return Err(ModelError::InvalidValue(format!(
                        "audio {id} is still used by a clip, sampler or sample bank; delete those first"
                    )));
                }
                let i = project
                    .audio_pool
                    .iter()
                    .position(|e| e.id == id)
                    .ok_or(ModelError::AudioNotFound(id))?;
                let entry = project.audio_pool.remove(i);
                Command::AddPoolAudio {
                    entry,
                    index: Some(i),
                }
            }
            Command::SetGameExport { export_dir, bits } => {
                if !matches!(bits, 16 | 24 | 32) {
                    return Err(ModelError::InvalidValue(format!(
                        "game exports use 16, 24 or 32-bit (float) WAV files, got {bits}"
                    )));
                }
                let export_dir = export_dir
                    .map(|d| d.trim().to_string())
                    .filter(|d| !d.is_empty());
                let game = &mut project.game;
                let old_dir = std::mem::replace(&mut game.export_dir, export_dir);
                let old_bits = std::mem::replace(&mut game.bits, bits);
                Command::SetGameExport {
                    export_dir: old_dir,
                    bits: old_bits,
                }
            }
            Command::AddGameAsset { asset, index } => {
                if project.game.asset(asset.id).is_some() {
                    return Err(ModelError::DuplicateId(asset.id.to_string()));
                }
                validate_game_asset(project, &asset)?;
                let id = asset.id;
                let assets = &mut project.game.assets;
                let at = index.unwrap_or(assets.len()).min(assets.len());
                assets.insert(at, *asset);
                Command::RemoveGameAsset { id }
            }
            Command::RemoveGameAsset { id } => {
                let i = game_asset_index(project, id)?;
                let asset = project.game.assets.remove(i);
                Command::AddGameAsset {
                    asset: Box::new(asset),
                    index: Some(i),
                }
            }
            Command::UpdateGameAsset { asset } => {
                let i = game_asset_index(project, asset.id)?;
                validate_game_asset(project, &asset)?;
                let old = std::mem::replace(&mut project.game.assets[i], *asset);
                Command::UpdateGameAsset {
                    asset: Box::new(old),
                }
            }
        })
    }
}
