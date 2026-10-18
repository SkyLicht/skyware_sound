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
use crate::picture::Picture;
use crate::steps::StepClip;

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
    /// The key the song is in, which the piano roll highlights. Nothing
    /// plays differently with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<Scale>,
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

/// A key: a root note and the scale built on it, such as A minor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scale {
    /// The root as a pitch class, 0 for C up to 11 for B.
    pub root: u8,
    pub mode: ScaleMode,
}

impl Scale {
    pub fn new(root: u8, mode: ScaleMode) -> Result<Self, ModelError> {
        if root > 11 {
            return Err(ModelError::InvalidValue(format!(
                "a scale's root is a pitch class from 0 (C) to 11 (B), got {root}"
            )));
        }
        Ok(Self { root, mode })
    }

    /// Whether a MIDI note is in the scale.
    pub fn contains(&self, pitch: u8) -> bool {
        let degree = (pitch % 12 + 12 - self.root % 12) % 12;
        self.mode.intervals().contains(&degree)
    }

    /// Whether a MIDI note is the root.
    pub fn is_root(&self, pitch: u8) -> bool {
        pitch % 12 == self.root % 12
    }

    /// Such as `A minor` or `F# Phrygian`.
    pub fn label(&self) -> String {
        format!(
            "{} {}",
            sky_core::music::pitch_class_name(self.root),
            self.mode.label()
        )
    }
}

/// The scales a song can be in, as steps from the root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScaleMode {
    Major,
    Minor,
    HarmonicMinor,
    MelodicMinor,
    Dorian,
    Phrygian,
    PhrygianDominant,
    Lydian,
    Mixolydian,
    Locrian,
    MajorPentatonic,
    MinorPentatonic,
    Blues,
}

impl ScaleMode {
    pub const ALL: [ScaleMode; 13] = [
        ScaleMode::Major,
        ScaleMode::Minor,
        ScaleMode::HarmonicMinor,
        ScaleMode::MelodicMinor,
        ScaleMode::Dorian,
        ScaleMode::Phrygian,
        ScaleMode::PhrygianDominant,
        ScaleMode::Lydian,
        ScaleMode::Mixolydian,
        ScaleMode::Locrian,
        ScaleMode::MajorPentatonic,
        ScaleMode::MinorPentatonic,
        ScaleMode::Blues,
    ];

    /// Semitones above the root.
    pub fn intervals(self) -> &'static [u8] {
        match self {
            ScaleMode::Major => &[0, 2, 4, 5, 7, 9, 11],
            ScaleMode::Minor => &[0, 2, 3, 5, 7, 8, 10],
            ScaleMode::HarmonicMinor => &[0, 2, 3, 5, 7, 8, 11],
            ScaleMode::MelodicMinor => &[0, 2, 3, 5, 7, 9, 11],
            ScaleMode::Dorian => &[0, 2, 3, 5, 7, 9, 10],
            ScaleMode::Phrygian => &[0, 1, 3, 5, 7, 8, 10],
            ScaleMode::PhrygianDominant => &[0, 1, 4, 5, 7, 8, 10],
            ScaleMode::Lydian => &[0, 2, 4, 6, 7, 9, 11],
            ScaleMode::Mixolydian => &[0, 2, 4, 5, 7, 9, 10],
            ScaleMode::Locrian => &[0, 1, 3, 5, 6, 8, 10],
            ScaleMode::MajorPentatonic => &[0, 2, 4, 7, 9],
            ScaleMode::MinorPentatonic => &[0, 3, 5, 7, 10],
            ScaleMode::Blues => &[0, 3, 5, 6, 7, 10],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ScaleMode::Major => "major",
            ScaleMode::Minor => "minor",
            ScaleMode::HarmonicMinor => "harmonic minor",
            ScaleMode::MelodicMinor => "melodic minor",
            ScaleMode::Dorian => "Dorian",
            ScaleMode::Phrygian => "Phrygian",
            ScaleMode::PhrygianDominant => "Phrygian dominant",
            ScaleMode::Lydian => "Lydian",
            ScaleMode::Mixolydian => "Mixolydian",
            ScaleMode::Locrian => "Locrian",
            ScaleMode::MajorPentatonic => "major pentatonic",
            ScaleMode::MinorPentatonic => "minor pentatonic",
            ScaleMode::Blues => "blues",
        }
    }
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
    /// The picture in the track's header and mixer strip. Without one a
    /// track shows a symbol for what it plays, or its sample pack's picture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture: Option<Picture>,
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
            picture: None,
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
                InstrumentType::Acid => [196, 214, 70],
                InstrumentType::Piano => [92, 150, 214],
            },
            picture: None,
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
    /// The instrument library a piano plays. Without one, the piano plays
    /// the small Upright Piano KW built into the program.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<LibraryRef>,
    /// The other side of an A/B comparison, once one is started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compare: Option<AbCompare>,
}

impl InstrumentSlot {
    pub fn new(kind: InstrumentType) -> Self {
        Self {
            kind,
            params: BTreeMap::new(),
            sample: None,
            bank: None,
            library: None,
            compare: None,
        }
    }

    /// The current value of a parameter, falling back to its default.
    pub fn param(&self, name: &str) -> Option<f32> {
        let spec = sky_dsp::params::find(self.kind.params(), name)?;
        Some(self.params.get(name).copied().unwrap_or(spec.default))
    }

    /// The key a piano's loaded library is shared under: its id and
    /// version, or an empty key for the built-in piano it plays without one.
    pub fn library_key(&self) -> String {
        self.library
            .as_ref()
            .map(LibraryRef::key)
            .unwrap_or_default()
    }
}

/// What a piano without a library plays: the small CC0 upright built into
/// Skyware Sound.
pub const BUILT_IN_PIANO: &str = "Upright Piano KW (small)";

/// An instrument library a project uses. Libraries are hundreds of MB, so
/// they live in a shared folder outside projects; the project names the
/// library and keeps its license for export credits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryRef {
    /// A stable name such as `upright-piano-kw`.
    pub id: String,
    /// What people see, such as `Upright Piano KW`.
    pub name: String,
    /// The library's release, such as `2022-02-21`.
    pub version: String,
    /// SHA-256 of the archive the library was installed from, in hex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    pub license: License,
    /// The credit line the license asks for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit: Option<String>,
}

impl LibraryRef {
    /// `id@version`, the key a loaded copy is shared under.
    pub fn key(&self) -> String {
        format!("{}@{}", self.id, self.version)
    }
}

/// Two versions of a device's settings to switch between while listening.
/// The slot's `params` are the side playing now; this holds the other.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbCompare {
    /// Whether the side playing now is B.
    #[serde(default)]
    pub on_b: bool,
    /// The other side's values that differ from the defaults.
    #[serde(default)]
    pub other: BTreeMap<String, f32>,
}

impl AbCompare {
    /// The comparison after switching sides, when `params` were playing.
    pub fn switched(&self, params: &BTreeMap<String, f32>) -> Self {
        Self {
            on_b: !self.on_b,
            other: params.clone(),
        }
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
    /// Packs linked to the bank, as Strudel's
    /// `samples('github:tidalcycles/dirt-samples')` links one: a sound a
    /// pattern names that the bank lacks loads from the first that has it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packs: Vec<PackLink>,
}

/// A sound pack linked to a sample bank.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackLink {
    /// The pack's name, such as `dirt-samples`.
    pub name: String,
    /// Its sample map as it was written: a `github:user/repo` shortcut, a
    /// URL or a folder.
    pub map: String,
    pub license: License,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit: Option<String>,
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
    /// The track whose notes drive the effect, for one that
    /// [takes a sidechain](EffectType::takes_sidechain).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidechain: Option<TrackId>,
    /// The other side of an A/B comparison, once one is started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compare: Option<AbCompare>,
}

impl EffectSlot {
    pub fn new(kind: EffectType) -> Self {
        Self {
            id: EffectId::new(),
            kind,
            enabled: true,
            params: BTreeMap::new(),
            sidechain: None,
            compare: None,
        }
    }

    pub fn with_sidechain(mut self, track: TrackId) -> Self {
        self.sidechain = Some(track);
        self
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
    /// A muted clip stays on its track but plays nothing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub muted: bool,
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
        /// Where the sustain pedal is down, sorted and apart.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pedal: Vec<Pedal>,
    },
    /// A short pattern in mini-notation that generates notes for any span.
    #[serde(rename_all = "camelCase")]
    Pattern {
        source: String,
        #[serde(default)]
        seed: u64,
        /// The length of one pattern cycle, normally one bar.
        cycle: Ticks,
        /// Number patterns by parameter name, read at the start of each
        /// note: `velocity` sets how hard the note plays (0 skips it), and
        /// an instrument parameter's name sets it for that note. A note
        /// that falls on a rest gets the usual value.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        controls: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Swing::is_straight")]
        swing: Swing,
    },
    /// Lanes of steps on a grid, as on a drum machine or an acid bass.
    Steps(StepClip),
}

/// Swing for a pattern: every second step of `grid` comes late.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Swing {
    /// Where the second step of each pair starts, as a percentage of the
    /// pair: 50 is straight, about 66 a triplet feel, 75 the most.
    pub amount: u8,
    /// The step that swings as a note value: 8 swings eighths, 16
    /// sixteenths.
    pub grid: u8,
}

impl Swing {
    pub const STRAIGHT: u8 = 50;
    pub const MAX: u8 = 75;
    pub const GRIDS: [u8; 4] = [4, 8, 16, 32];

    pub fn new(amount: u8, grid: u8) -> Self {
        Self { amount, grid }
    }

    pub fn is_straight(&self) -> bool {
        self.amount <= Self::STRAIGHT
    }

    /// Moves a time on the timeline to where swing plays it. Times on the
    /// grid's pairs stay put and everything between stretches, so notes
    /// keep their order and never overlap.
    pub fn apply(&self, tick: Ticks) -> Ticks {
        if self.is_straight() || self.grid == 0 {
            return tick;
        }
        let step = sky_core::PPQ * 4 / Ticks::from(self.grid);
        let pair = 2 * step;
        let base = tick.div_euclid(pair) * pair;
        let x = tick - base;
        let late = pair * Ticks::from(self.amount.min(Self::MAX)) / 100;
        let y = if x < step {
            x * late / step
        } else {
            late + (x - step) * (pair - late) / step
        };
        base + y
    }
}

impl Default for Swing {
    fn default() -> Self {
        Self {
            amount: Self::STRAIGHT,
            grid: 16,
        }
    }
}

impl ClipContent {
    /// A pattern with no controls or swing.
    pub fn pattern(source: impl Into<String>, seed: u64, cycle: Ticks) -> Self {
        ClipContent::Pattern {
            source: source.into(),
            seed,
            cycle,
            controls: BTreeMap::new(),
            swing: Swing::default(),
        }
    }

    /// MIDI content with these notes and the pedal up throughout.
    pub fn midi(notes: Vec<Note>) -> Self {
        ClipContent::Midi {
            notes,
            pedal: Vec::new(),
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            ClipContent::Audio { .. } => "audio",
            ClipContent::Midi { .. } => "midi",
            ClipContent::Pattern { .. } => "pattern",
            ClipContent::Steps(_) => "steps",
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

/// A stretch of a MIDI clip with the sustain pedal down.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pedal {
    /// Start relative to the clip start.
    pub start: Ticks,
    pub length: Ticks,
}

impl Pedal {
    pub fn new(start: Ticks, length: Ticks) -> Self {
        Self { start, length }
    }

    pub fn end(&self) -> Ticks {
        self.start + self.length
    }

    /// The stretch moved `by` ticks and cut where the clip starts, or none
    /// when it ends before then.
    pub fn shifted(&self, by: Ticks) -> Option<Pedal> {
        let start = (self.start + by).max(0);
        let end = self.end() + by;
        (end > start).then(|| Pedal::new(start, end - start))
    }

    /// The stretch cut where a clip `length` long ends, or none when it
    /// starts after then.
    pub fn cut(&self, length: Ticks) -> Option<Pedal> {
        (self.start < length).then(|| Pedal::new(self.start, self.length.min(length - self.start)))
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        if self.start < 0 || self.length <= 0 {
            return Err(ModelError::InvalidValue(format!(
                "a pedal stretch needs start >= 0 and length > 0, got start {} length {}",
                self.start, self.length
            )));
        }
        Ok(())
    }

    /// Sorts stretches and joins the ones that touch or overlap, so the
    /// pedal goes down and up once per stretch.
    pub fn tidy(mut pedal: Vec<Pedal>) -> Vec<Pedal> {
        pedal.sort_by_key(|p| p.start);
        let mut out: Vec<Pedal> = Vec::with_capacity(pedal.len());
        for p in pedal {
            match out.last_mut() {
                Some(last) if p.start <= last.end() => {
                    last.length = last.length.max(p.end() - last.start);
                }
                _ => out.push(p),
            }
        }
        out
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
            scale: None,
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

    /// `base`, or `base 2`, `base 3` and so on: the first that no track
    /// is called yet.
    pub fn unique_track_name(&self, base: &str) -> String {
        let taken = |name: &str| self.tracks.iter().any(|t| t.name == name);
        if !taken(base) {
            return base.to_string();
        }
        (2..)
            .map(|n| format!("{base} {n}"))
            .find(|n| !taken(n))
            .unwrap_or_else(|| base.to_string())
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

    /// The track a new duck on `channel`'s track (or the master, for
    /// `None`) should follow: the first other drum track, else the first
    /// other instrument track.
    pub fn default_sidechain(&self, channel: Option<TrackId>) -> Option<TrackId> {
        let others = || {
            self.tracks
                .iter()
                .filter(move |t| t.kind == TrackKind::Instrument && Some(t.id) != channel)
        };
        others()
            .find(|t| {
                t.instrument
                    .as_ref()
                    .is_some_and(|i| i.kind == InstrumentType::Drums)
            })
            .or_else(|| others().next())
            .map(|t| t.id)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_know_their_notes() {
        let a_minor = Scale::new(9, ScaleMode::Minor).unwrap();
        assert_eq!(a_minor.label(), "A minor");
        // A B C D E F G, in any octave.
        let in_scale: Vec<u8> = (57..69).filter(|p| a_minor.contains(*p)).collect();
        assert_eq!(in_scale, [57, 59, 60, 62, 64, 65, 67]);
        assert!(a_minor.is_root(45) && a_minor.is_root(69) && !a_minor.is_root(60));
        let f_sharp = Scale::new(6, ScaleMode::Phrygian).unwrap();
        assert_eq!(f_sharp.label(), "F# Phrygian");
        assert!(f_sharp.contains(7) && !f_sharp.contains(8));
        assert!(Scale::new(12, ScaleMode::Major).is_err());
        for mode in ScaleMode::ALL {
            let steps = mode.intervals();
            assert_eq!(steps[0], 0, "{mode:?}");
            assert!(
                steps.windows(2).all(|w| w[0] < w[1] && w[1] < 12),
                "{mode:?}"
            );
        }
    }
}
