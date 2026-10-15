// SPDX-License-Identifier: GPL-3.0-or-later
//! Project data.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sky_core::manifest::{Category, RandomRange, Selection};
use sky_core::{
    AssetId, AudioId, ClipId, EffectId, MarkerId, NoteId, ProjectId, SampleTime, TempoMap, Ticks,
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
    /// The loops and SFX sets this project exports for a game, and where to.
    #[serde(default, skip_serializing_if = "GameSettings::is_empty")]
    pub game: GameSettings,
}

/// What a project exports for a game, and where to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameSettings {
    /// The folder game exports go to, such as the game's `assets/audio`.
    /// A relative path is relative to the project folder. Without one,
    /// exports go to `renders/export` inside the project folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_dir: Option<String>,
    /// Bits per sample of exported WAV files: 16, 24 or 32 (float).
    #[serde(default = "default_game_bits")]
    pub bits: u16,
    #[serde(default)]
    pub assets: Vec<GameAsset>,
}

fn default_game_bits() -> u16 {
    24
}

impl Default for GameSettings {
    fn default() -> Self {
        Self {
            export_dir: None,
            bits: default_game_bits(),
            assets: Vec::new(),
        }
    }
}

impl GameSettings {
    /// True when nothing differs from a new project's settings.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn asset(&self, id: AssetId) -> Option<&GameAsset> {
        self.assets.iter().find(|a| a.id == id)
    }

    pub fn find(&self, name: &str) -> Option<&GameAsset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

/// One asset the game plays: a loop or a set of SFX variations, rendered
/// from the arrangement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameAsset {
    pub id: AssetId,
    /// The name the game plays it by, such as `footstep_grass`: lowercase
    /// letters, digits and underscores. File names come from it too.
    pub name: String,
    #[serde(default)]
    pub category: Category,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(flatten)]
    pub kind: GameAssetKind,
}

/// The longest tail an SFX variation can ring out for, in seconds.
pub const MAX_SFX_TAIL_SECONDS: f32 = 10.0;
/// The most variations one SFX set can have.
pub const MAX_SFX_VARIATIONS: usize = 64;
/// The widest random pitch offset of an SFX set, in semitones either way.
pub const MAX_SFX_PITCH_SEMITONES: f32 = 24.0;
/// The range a random gain offset of an SFX set may cover, in dB.
pub const SFX_GAIN_DB: (f32, f32) = (-60.0, 12.0);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum GameAssetKind {
    /// A seamless loop. The body repeats forever; tails from its end are
    /// folded into its start. An intro, if there is one, plays once first.
    #[serde(rename_all = "camelCase")]
    Loop {
        /// The repeating body.
        range: TimeRange,
        /// Where an intro that plays once starts; before `range.start`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intro_start: Option<Ticks>,
    },
    /// A set of variations of one sound. The game picks one per trigger and
    /// adds a random pitch and gain offset.
    #[serde(rename_all = "camelCase")]
    Sfx {
        /// One time range per variation, each rendered to its own file.
        variations: Vec<TimeRange>,
        #[serde(default)]
        selection: Selection,
        /// Random pitch offset per trigger, in semitones.
        #[serde(default)]
        pitch_semitones: RandomRange,
        /// Random gain offset per trigger, in decibels.
        #[serde(default)]
        gain_db: RandomRange,
        /// Seconds rendered after each variation so releases and reverb
        /// ring out. Trailing silence is trimmed.
        #[serde(default = "default_sfx_tail")]
        tail_seconds: f32,
        /// Mix the variations down to mono, for sounds the game positions
        /// itself.
        #[serde(default)]
        mono: bool,
    },
}

fn default_sfx_tail() -> f32 {
    1.0
}

impl GameAssetKind {
    /// An SFX set with the default selection, no randomization and a
    /// one-second tail.
    pub fn sfx(variations: Vec<TimeRange>) -> Self {
        GameAssetKind::Sfx {
            variations,
            selection: Selection::default(),
            pitch_semitones: RandomRange::ZERO,
            gain_db: RandomRange::ZERO,
            tail_seconds: default_sfx_tail(),
            mono: false,
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            GameAssetKind::Loop { .. } => "loop",
            GameAssetKind::Sfx { .. } => "sfx",
        }
    }
}

impl TimeRange {
    /// Splits the range into `count` equal parts, such as one variation per
    /// beat. The last part takes any remainder.
    pub fn split(&self, count: usize) -> Vec<TimeRange> {
        let count = (count as Ticks).clamp(1, self.length().max(1));
        let step = self.length() / count;
        (0..count)
            .map(|i| {
                let start = self.start + i * step;
                let end = if i + 1 == count {
                    self.end
                } else {
                    start + step
                };
                TimeRange { start, end }
            })
            .collect()
    }

    /// From the earliest start to the latest end of `ranges`, in any order;
    /// `None` when there are none.
    pub fn covering(ranges: &[TimeRange]) -> Option<TimeRange> {
        let start = ranges.iter().map(|r| r.start).min()?;
        let end = ranges.iter().map(|r| r.end).max()?;
        Some(TimeRange { start, end })
    }
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
                InstrumentType::Bank => [214, 176, 86],
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
    /// The sounds a sample bank plays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bank: Option<SampleBank>,
}

impl InstrumentSlot {
    pub fn new(kind: InstrumentType) -> Self {
        Self {
            kind,
            params: BTreeMap::new(),
            sample: None,
            bank: None,
        }
    }

    /// The current value of a parameter, falling back to its default.
    pub fn param(&self, name: &str) -> Option<f32> {
        let spec = sky_dsp::params::find(self.kind.params(), name)?;
        Some(self.params.get(name).copied().unwrap_or(spec.default))
    }
}

/// The sounds a sample-bank instrument plays, by name, as in Strudel: the
/// pattern `bd sd:2` plays the first `bd` and the third `sd`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleBank {
    /// What the bank is called, such as `RolandTR808` or `piano`.
    pub name: String,
    /// Each sound's variations in order. A pattern index past the end wraps
    /// around, so `bd:5` with two variations plays the second.
    pub sounds: BTreeMap<String, Vec<BankSample>>,
    /// The sound that MIDI notes and note names play, pitched across the
    /// keyboard. Without one, notes play the drum sounds on their General
    /// MIDI notes (36 `bd`, 38 `sd`, 42 `hh`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitched: Option<String>,
}

/// One variation of a bank sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BankSample {
    pub audio: AudioId,
    /// The MIDI note the sample was recorded at, for pitched sounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<u8>,
}

impl SampleBank {
    /// The sound a pattern name plays and its variations: the name itself,
    /// then the name in another case, then another name for the same drum,
    /// so `sd` finds a bank's `sn`.
    pub fn resolve(&self, name: &str) -> Option<(&str, &[BankSample])> {
        let found = |key: &str| {
            self.sounds
                .get_key_value(key)
                .or_else(|| {
                    self.sounds
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case(key))
                })
                .filter(|(_, v)| !v.is_empty())
                .map(|(k, v)| (k.as_str(), v.as_slice()))
        };
        found(name).or_else(|| {
            let note = sky_pattern::drum_note(name)?;
            sky_pattern::drum_names(note).find_map(found)
        })
    }

    /// Every pool file the bank uses.
    pub fn audio_ids(&self) -> impl Iterator<Item = AudioId> + '_ {
        self.sounds.values().flatten().map(|s| s.audio)
    }

    /// How many samples the bank holds.
    pub fn sample_count(&self) -> usize {
        self.sounds.values().map(Vec::len).sum()
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
    /// Where the file came from, for files loaded from a sound pack. Files
    /// imported from disk have none: they are the user's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<AudioSource>,
}

/// The pack a pool file was loaded from, and its license.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioSource {
    /// The pack's name, such as `Salamander Grand Piano`.
    pub pack: String,
    /// The address the file was loaded from: an http(s) URL or a path.
    pub url: String,
    pub license: License,
    /// The credit line the license asks for, such as `Salamander Grand
    /// Piano by Alexander Holm (CC BY 3.0)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit: Option<String>,
}

/// The license of downloaded audio. Serialized as SPDX identifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum License {
    /// Public domain dedication; no conditions.
    #[serde(rename = "CC0-1.0")]
    Cc0,
    #[serde(rename = "public-domain")]
    PublicDomain,
    /// Keep the copyright notice.
    #[serde(rename = "MIT")]
    Mit,
    /// Credit the author.
    #[serde(rename = "CC-BY-3.0")]
    CcBy3,
    #[serde(rename = "CC-BY-4.0")]
    CcBy4,
    /// Credit the author, and share what is made with it under the same terms.
    #[serde(rename = "CC-BY-SA-4.0")]
    CcBySa4,
    /// The pack does not say. Nothing is known about what is allowed.
    #[serde(rename = "unknown")]
    Unknown,
}

impl License {
    pub const ALL: [License; 7] = [
        License::Cc0,
        License::PublicDomain,
        License::Mit,
        License::CcBy3,
        License::CcBy4,
        License::CcBySa4,
        License::Unknown,
    ];

    /// The SPDX identifier, or `public-domain` or `unknown`.
    pub fn id(self) -> &'static str {
        match self {
            License::Cc0 => "CC0-1.0",
            License::PublicDomain => "public-domain",
            License::Mit => "MIT",
            License::CcBy3 => "CC-BY-3.0",
            License::CcBy4 => "CC-BY-4.0",
            License::CcBySa4 => "CC-BY-SA-4.0",
            License::Unknown => "unknown",
        }
    }

    /// A short name for people, such as `CC BY 3.0`.
    pub fn label(self) -> &'static str {
        match self {
            License::Cc0 => "CC0",
            License::PublicDomain => "Public domain",
            License::Mit => "MIT",
            License::CcBy3 => "CC BY 3.0",
            License::CcBy4 => "CC BY 4.0",
            License::CcBySa4 => "CC BY-SA 4.0",
            License::Unknown => "License unknown",
        }
    }

    /// Reads an identifier or label, ignoring case: `CC0-1.0`, `cc0`,
    /// `CC BY 3.0`, `unknown`.
    pub fn parse(text: &str) -> Option<License> {
        let norm = |s: &str| {
            s.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase()
        };
        let want = norm(text);
        License::ALL
            .into_iter()
            .find(|l| norm(l.id()) == want || norm(l.label()) == want)
    }

    /// True when shipping the audio means crediting its author.
    pub fn needs_credit(self) -> bool {
        matches!(
            self,
            License::Mit | License::CcBy3 | License::CcBy4 | License::CcBySa4
        )
    }

    /// True when work made with the audio must use the same license.
    pub fn share_alike(self) -> bool {
        self == License::CcBySa4
    }
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
            game: GameSettings::default(),
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
