// SPDX-License-Identifier: GPL-3.0-or-later
//! Project data.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sky_core::{
    AudioId, ClipId, EffectId, MarkerId, NoteId, ProjectId, SampleTime, TempoMap, Ticks,
    TimeSignature, TrackId,
};
use sky_dsp::{EffectType, InstrumentType};

use crate::error::ModelError;

/// The project file format version this build writes.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub schema_version: u32,
    pub id: ProjectId,
    pub name: String,
    /// The sample rate for renders and exports.
    pub sample_rate: u32,
    pub tempo: TempoMap,
    pub time_signature: TimeSignature,
    /// The transport loop, which is also the default export region.
    pub loop_region: TimeRange,
    pub loop_enabled: bool,
    #[serde(default)]
    pub markers: Vec<Marker>,
    #[serde(default)]
    pub tracks: Vec<Track>,
    pub master: Channel,
    #[serde(default)]
    pub audio_pool: Vec<PoolEntry>,
}

/// A span of musical time, `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeRange {
    pub start: Ticks,
    pub end: Ticks,
}

impl TimeRange {
    pub fn new(start: Ticks, end: Ticks) -> Result<Self, ModelError> {
        if start < 0 || end <= start {
            return Err(ModelError::InvalidValue(format!(
                "a time range needs 0 <= start < end, got {start}..{end} ticks"
            )));
        }
        Ok(Self { start, end })
    }

    pub fn length(&self) -> Ticks {
        self.end - self.start
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Marker {
    pub id: MarkerId,
    pub tick: Ticks,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TrackKind {
    /// Plays audio clips from the pool.
    Audio,
    /// Plays MIDI and pattern clips through an instrument.
    Instrument,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    pub color: [u8; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument: Option<InstrumentSlot>,
    pub channel: Channel,
    #[serde(default)]
    pub clips: Vec<Clip>,
}

impl Track {
    pub fn new_audio(name: impl Into<String>) -> Self {
        Self {
            id: TrackId::new(),
            name: name.into(),
            kind: TrackKind::Audio,
            color: [86, 156, 214],
            instrument: None,
            channel: Channel::default(),
            clips: Vec::new(),
        }
    }

    pub fn new_instrument(name: impl Into<String>, instrument: InstrumentType) -> Self {
        Self {
            id: TrackId::new(),
            name: name.into(),
            kind: TrackKind::Instrument,
            color: match instrument {
                InstrumentType::Drums => [214, 120, 86],
                InstrumentType::Synth => [140, 110, 214],
                InstrumentType::Sampler => [86, 190, 140],
            },
            instrument: Some(InstrumentSlot::new(instrument)),
            channel: Channel::default(),
            clips: Vec::new(),
        }
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentSlot {
    pub kind: InstrumentType,
    /// Values that differ from the defaults.
    #[serde(default)]
    pub params: BTreeMap<String, f32>,
    /// The pool audio a sampler plays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<AudioId>,
}

impl InstrumentSlot {
    pub fn new(kind: InstrumentType) -> Self {
        Self {
            kind,
            params: BTreeMap::new(),
            sample: None,
        }
    }

    /// The current value of a parameter, falling back to its default.
    pub fn param(&self, name: &str) -> Option<f32> {
        let spec = sky_dsp::params::find(self.kind.params(), name)?;
        Some(self.params.get(name).copied().unwrap_or(spec.default))
    }
}

/// A mixer channel: the track's or the master's fader, pan and inserts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub volume_db: f32,
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
    #[serde(default)]
    pub effects: Vec<EffectSlot>,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            volume_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            effects: Vec::new(),
        }
    }
}

impl Channel {
    pub fn effect(&self, id: EffectId) -> Option<&EffectSlot> {
        self.effects.iter().find(|e| e.id == id)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectSlot {
    pub id: EffectId,
    pub kind: EffectType,
    pub enabled: bool,
    /// Values that differ from the defaults.
    #[serde(default)]
    pub params: BTreeMap<String, f32>,
}

impl EffectSlot {
    pub fn new(kind: EffectType) -> Self {
        Self {
            id: EffectId::new(),
            kind,
            enabled: true,
            params: BTreeMap::new(),
        }
    }

    pub fn with_params(mut self, params: &[(&str, f32)]) -> Self {
        for (k, v) in params {
            self.params.insert((*k).to_string(), *v);
        }
        self
    }

    pub fn param(&self, name: &str) -> Option<f32> {
        let spec = sky_dsp::params::find(self.kind.params(), name)?;
        Some(self.params.get(name).copied().unwrap_or(spec.default))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    pub id: ClipId,
    pub name: String,
    pub start: Ticks,
    pub length: Ticks,
    pub content: ClipContent,
}

impl Clip {
    pub fn end(&self) -> Ticks {
        self.start + self.length
    }

    pub fn kind_name(&self) -> &'static str {
        self.content.kind_name()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ClipContent {
    #[serde(rename_all = "camelCase")]
    Audio {
        source: AudioId,
        /// Where in the source file the clip starts, in source samples.
        #[serde(default)]
        offset: SampleTime,
        #[serde(default)]
        gain_db: f32,
        #[serde(default)]
        fade_in: Ticks,
        #[serde(default)]
        fade_out: Ticks,
    },
    #[serde(rename_all = "camelCase")]
    Midi {
        #[serde(default)]
        notes: Vec<Note>,
    },
    /// A short pattern in mini-notation that generates notes for any span.
    #[serde(rename_all = "camelCase")]
    Pattern {
        source: String,
        #[serde(default)]
        seed: u64,
        /// The length of one pattern cycle, normally one bar.
        cycle: Ticks,
    },
}

impl ClipContent {
    pub fn kind_name(&self) -> &'static str {
        match self {
            ClipContent::Audio { .. } => "audio",
            ClipContent::Midi { .. } => "midi",
            ClipContent::Pattern { .. } => "pattern",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub id: NoteId,
    /// Start relative to the clip start.
    pub start: Ticks,
    pub length: Ticks,
    pub pitch: u8,
    pub velocity: u8,
}

impl Note {
    pub fn new(start: Ticks, length: Ticks, pitch: u8, velocity: u8) -> Self {
        Self {
            id: NoteId::new(),
            start,
            length,
            pitch,
            velocity,
        }
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        if self.start < 0 || self.length <= 0 {
            return Err(ModelError::InvalidValue(format!(
                "a note needs start >= 0 and length > 0, got start {} length {}",
                self.start, self.length
            )));
        }
        if self.pitch > 127 || self.velocity == 0 || self.velocity > 127 {
            return Err(ModelError::InvalidValue(format!(
                "a note needs pitch 0-127 and velocity 1-127, got pitch {} velocity {}",
                self.pitch, self.velocity
            )));
        }
        Ok(())
    }
}

/// A file in the project's audio pool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolEntry {
    pub id: AudioId,
    pub name: String,
    /// Path inside the project folder, such as `audio/<id>.wav`.
    pub file: String,
    pub channels: u16,
    pub sample_rate: u32,
    pub frames: u64,
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        let ts = TimeSignature::default();
        Self {
            schema_version: SCHEMA_VERSION,
            id: ProjectId::new(),
            name: name.into(),
            sample_rate: 48_000,
            tempo: TempoMap::constant(120.0),
            time_signature: ts,
            loop_region: TimeRange {
                start: 0,
                end: ts.ticks_per_bar() * 4,
            },
            loop_enabled: true,
            markers: Vec::new(),
            tracks: Vec::new(),
            master: Channel::default(),
            audio_pool: Vec::new(),
        }
    }

    pub fn track(&self, id: TrackId) -> Result<&Track, ModelError> {
        self.tracks
            .iter()
            .find(|t| t.id == id)
            .ok_or(ModelError::TrackNotFound(id))
    }

    pub fn track_mut(&mut self, id: TrackId) -> Result<&mut Track, ModelError> {
        self.tracks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(ModelError::TrackNotFound(id))
    }

    pub fn track_index(&self, id: TrackId) -> Result<usize, ModelError> {
        self.tracks
            .iter()
            .position(|t| t.id == id)
            .ok_or(ModelError::TrackNotFound(id))
    }

    /// Finds a clip on any track.
    pub fn find_clip(&self, id: ClipId) -> Result<(&Track, &Clip), ModelError> {
        self.tracks
            .iter()
            .find_map(|t| t.clip(id).map(|c| (t, c)))
            .ok_or(ModelError::ClipNotFound(id))
    }

    pub fn clip_mut(&mut self, id: ClipId) -> Result<&mut Clip, ModelError> {
        self.tracks
            .iter_mut()
            .flat_map(|t| t.clips.iter_mut())
            .find(|c| c.id == id)
            .ok_or(ModelError::ClipNotFound(id))
    }

    pub fn pool_entry(&self, id: AudioId) -> Result<&PoolEntry, ModelError> {
        self.audio_pool
            .iter()
            .find(|e| e.id == id)
            .ok_or(ModelError::AudioNotFound(id))
    }

    /// The end of the last clip, or the loop end if that is later.
    pub fn content_end(&self) -> Ticks {
        self.tracks
            .iter()
            .flat_map(|t| t.clips.iter().map(Clip::end))
            .max()
            .unwrap_or(0)
            .max(self.loop_region.end)
    }

    pub fn ticks_per_bar(&self) -> Ticks {
        self.time_signature.ticks_per_bar()
    }

    /// Ticks at a 1-based bar number (bar 1 is the start of the song).
    pub fn bar_to_ticks(&self, bar: f64) -> Ticks {
        ((bar - 1.0) * self.ticks_per_bar() as f64).round() as Ticks
    }

    /// The 1-based bar number at `tick`.
    pub fn ticks_to_bar(&self, tick: Ticks) -> f64 {
        tick as f64 / self.ticks_per_bar() as f64 + 1.0
    }

    pub fn any_solo(&self) -> bool {
        self.tracks.iter().any(|t| t.channel.solo)
    }
}
