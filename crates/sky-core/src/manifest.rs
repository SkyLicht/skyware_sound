// SPDX-License-Identifier: MIT OR Apache-2.0
//! The export manifest.
//!
//! Every export writes a versioned JSON manifest next to the audio files. It
//! is the contract between Skyware Sound and the game: `sky-runtime` reads it
//! to play loops and SFX containers without any hand edits.

use serde::{Deserialize, Serialize};

use crate::CoreError;
use crate::ids::AssetId;

/// The newest manifest schema this build reads and writes.
pub const MANIFEST_VERSION: u32 = 1;

/// The manifest's file name inside an export folder.
pub const MANIFEST_FILE_NAME: &str = "skyware-manifest.json";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub schema_version: u32,
    /// The program that wrote the manifest, such as `Skyware Sound 0.1.0`.
    pub generator: String,
    /// The name of the project the assets came from.
    pub project: String,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

/// What kind of sound an asset is, used for loudness targets and grouping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Category {
    #[default]
    Music,
    Sfx,
    Ui,
    Voice,
    Ambience,
}

impl Category {
    /// Every category, in the order of [`index`](Self::index).
    pub const ALL: [Category; 5] = [
        Category::Music,
        Category::Sfx,
        Category::Ui,
        Category::Voice,
        Category::Ambience,
    ];

    /// The name used in manifests and folder names, such as `music`.
    pub fn name(self) -> &'static str {
        match self {
            Category::Music => "music",
            Category::Sfx => "sfx",
            Category::Ui => "ui",
            Category::Voice => "voice",
            Category::Ambience => "ambience",
        }
    }

    /// Reads a name such as `music` or `SFX`.
    pub fn parse(name: &str) -> Option<Category> {
        Category::ALL
            .into_iter()
            .find(|c| c.name().eq_ignore_ascii_case(name.trim()))
    }

    /// The position in [`ALL`](Self::ALL), from 0 to 4.
    pub fn index(self) -> usize {
        self as usize
    }
}

/// One exported asset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Asset {
    /// A file that plays once and then repeats `[loop_start, loop_end)` forever.
    Loop(LoopAsset),
    /// A set of variations, one of which is picked and randomized per trigger.
    Sfx(SfxAsset),
}

impl Asset {
    pub fn id(&self) -> AssetId {
        match self {
            Asset::Loop(a) => a.id,
            Asset::Sfx(a) => a.id,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Asset::Loop(a) => &a.name,
            Asset::Sfx(a) => &a.name,
        }
    }

    pub fn category(&self) -> Category {
        match self {
            Asset::Loop(a) => a.category,
            Asset::Sfx(a) => a.category,
        }
    }

    /// The project that manages this asset; see [`LoopAsset::project_id`].
    pub fn project_id(&self) -> Option<&str> {
        match self {
            Asset::Loop(a) => a.project_id.as_deref(),
            Asset::Sfx(a) => a.project_id.as_deref(),
        }
    }

    /// Every audio file the asset uses.
    pub fn files(&self) -> impl Iterator<Item = &AudioFile> {
        let (single, many): (Option<&AudioFile>, &[AudioFile]) = match self {
            Asset::Loop(a) => (Some(&a.file), &[]),
            Asset::Sfx(a) => (None, &a.variations),
        };
        single.into_iter().chain(many)
    }
}

/// Turns a display name into an asset name: lowercase ASCII letters, digits
/// and single underscores, such as `battle_loop_2` for `Battle Loop #2`, cut
/// to [`MAX_ASSET_NAME`] bytes. Asset names double as file names, so they are
/// safe on every platform.
pub fn asset_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    out.truncate(MAX_ASSET_NAME);
    let out = out.trim_end_matches('_').to_string();
    if out.is_empty() {
        "untitled".into()
    } else {
        out
    }
}

/// The longest asset name, in bytes.
pub const MAX_ASSET_NAME: usize = 64;

/// Whether `name` is already in the form [`asset_name`] produces and at most
/// [`MAX_ASSET_NAME`] long.
pub fn is_asset_name(name: &str) -> bool {
    name.len() <= MAX_ASSET_NAME && asset_name(name) == name
}

/// An audio file referenced by the manifest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioFile {
    /// Path relative to the manifest, with forward slashes.
    pub path: String,
    pub channels: u16,
    pub sample_rate: u32,
    pub length_samples: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopAsset {
    pub id: AssetId,
    pub name: String,
    #[serde(default)]
    pub category: Category,
    pub file: AudioFile,
    /// First sample of the repeating body. Zero unless the file starts with an intro.
    pub loop_start: u64,
    /// One past the last sample of the body; playback jumps back to `loop_start` here.
    pub loop_end: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tempo_bpm: Option<f64>,
    /// Numerator and denominator, such as `[4, 4]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_signature: Option<[u32; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bars: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loudness_lufs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_dbfs: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Credit lines the licenses of the sounds in it ask for, such as
    /// `Salamander Grand Piano by Alexander Holm (CC BY 3.0)`. A game shows
    /// them in its credits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credits: Vec<String>,
    /// The project whose game assets list this asset, when it was exported
    /// as part of one. Exporting that project's assets again removes the
    /// ones it no longer lists. One-off exports have none and stay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

/// How an SFX container picks the next variation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Selection {
    /// Any variation, possibly the same one twice.
    Random,
    /// Any variation except the one played last.
    #[default]
    RandomNoRepeat,
    /// Each variation in order, then start over.
    RoundRobin,
    /// Every variation once in a random order, then reshuffle.
    Shuffle,
}

/// A closed range that a value is picked from uniformly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RandomRange {
    pub min: f32,
    pub max: f32,
}

impl RandomRange {
    pub const ZERO: RandomRange = RandomRange { min: 0.0, max: 0.0 };

    pub fn new(min: f32, max: f32) -> Self {
        if min <= max {
            Self { min, max }
        } else {
            Self { min: max, max: min }
        }
    }

    /// The value at `t` in `[0, 1]`.
    pub fn lerp(&self, t: f32) -> f32 {
        self.min + (self.max - self.min) * t
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SfxAsset {
    pub id: AssetId,
    pub name: String,
    #[serde(default = "sfx_category")]
    pub category: Category,
    pub variations: Vec<AudioFile>,
    #[serde(default)]
    pub selection: Selection,
    /// Random pitch offset per trigger, in semitones.
    #[serde(default)]
    pub pitch_semitones: RandomRange,
    /// Random gain offset per trigger, in decibels.
    #[serde(default)]
    pub gain_db: RandomRange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loudness_lufs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_dbfs: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Credit lines the licenses of the sounds in it ask for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credits: Vec<String>,
    /// The project that manages this asset; see [`LoopAsset::project_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

fn sfx_category() -> Category {
    Category::Sfx
}

impl Manifest {
    pub fn new(generator: impl Into<String>, project: impl Into<String>) -> Self {
        Self {
            schema_version: MANIFEST_VERSION,
            generator: generator.into(),
            project: project.into(),
            assets: Vec::new(),
        }
    }

    /// Parses a manifest, refusing schema versions newer than this build knows.
    pub fn from_json(text: &str) -> Result<Self, CoreError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| CoreError::ManifestJson(e.to_string()))?;
        let found = value
            .get("schemaVersion")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| CoreError::ManifestJson("missing schemaVersion".into()))?
            as u32;
        if found > MANIFEST_VERSION || found == 0 {
            return Err(CoreError::UnsupportedManifestVersion {
                found,
                supported: MANIFEST_VERSION,
            });
        }
        serde_json::from_value(value).map_err(|e| CoreError::ManifestJson(e.to_string()))
    }

    pub fn to_json_pretty(&self) -> String {
        // Serializing plain data with string keys cannot fail.
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Adds an asset, replacing any existing asset with the same ID or name.
    pub fn upsert(&mut self, asset: Asset) {
        let id = asset.id();
        let name = asset.name().to_string();
        self.assets.retain(|a| a.id() != id && a.name() != name);
        self.assets.push(asset);
        self.assets.sort_by(|a, b| a.name().cmp(b.name()));
    }

    pub fn find(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name() == name)
    }

    /// Removes and returns the asset with this ID.
    pub fn remove(&mut self, id: AssetId) -> Option<Asset> {
        let i = self.assets.iter().position(|a| a.id() == id)?;
        Some(self.assets.remove(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> Manifest {
        let mut m = Manifest::new("Skyware Sound test", "Demo");
        m.upsert(Asset::Loop(LoopAsset {
            id: AssetId::from_u128(1),
            name: "battle_loop".into(),
            category: Category::Music,
            file: AudioFile {
                path: "music/battle_loop.wav".into(),
                channels: 2,
                sample_rate: 48_000,
                length_samples: 96_000,
            },
            loop_start: 0,
            loop_end: 96_000,
            tempo_bpm: Some(120.0),
            time_signature: Some([4, 4]),
            bars: Some(1.0),
            key: None,
            loudness_lufs: Some(-16.0),
            peak_dbfs: Some(-1.0),
            tags: vec!["battle".into()],
            credits: vec!["Salamander Grand Piano by Alexander Holm (CC BY 3.0)".into()],
            project_id: None,
        }));
        m.upsert(Asset::Sfx(SfxAsset {
            id: AssetId::from_u128(2),
            name: "footstep".into(),
            category: Category::Sfx,
            variations: vec![AudioFile {
                path: "sfx/footstep_01.wav".into(),
                channels: 1,
                sample_rate: 48_000,
                length_samples: 4_800,
            }],
            selection: Selection::RandomNoRepeat,
            pitch_semitones: RandomRange::new(-1.0, 1.0),
            gain_db: RandomRange::new(-2.0, 0.0),
            loudness_lufs: None,
            peak_dbfs: None,
            tags: vec![],
            credits: vec![],
            project_id: Some("project-1".into()),
        }));
        m
    }

    #[test]
    fn manifest_round_trips() {
        let m = sample_manifest();
        let json = m.to_json_pretty();
        assert!(json.contains("\"schemaVersion\": 1"));
        assert!(json.contains("\"kind\": \"loop\""));
        assert!(json.contains("\"loopEnd\": 96000"));
        assert!(json.contains("\"credits\": ["));
        let back = Manifest::from_json(&json).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn newer_versions_are_refused() {
        let json = r#"{"schemaVersion": 99, "generator": "x", "project": "y"}"#;
        assert!(matches!(
            Manifest::from_json(json),
            Err(CoreError::UnsupportedManifestVersion { found: 99, .. })
        ));
    }

    #[test]
    fn asset_names_are_safe_file_names() {
        assert_eq!(asset_name("Battle Loop #2"), "battle_loop_2");
        assert_eq!(asset_name("  ...  "), "untitled");
        assert_eq!(asset_name("Ünïcode/../path"), "n_code_path");
        assert!(is_asset_name("footstep_grass"));
        assert!(!is_asset_name("Footstep"));
        assert!(!is_asset_name("foot__step"));
        assert!(!is_asset_name(""));
        assert!(!is_asset_name(&"a".repeat(MAX_ASSET_NAME + 1)));
        let long = asset_name(&"Long name ".repeat(20));
        assert!(is_asset_name(&long), "{long}");
        assert!(long.len() <= MAX_ASSET_NAME && long.starts_with("long_name_"));
    }

    #[test]
    fn categories_have_names_and_indexes() {
        for (i, c) in Category::ALL.into_iter().enumerate() {
            assert_eq!(c.index(), i);
            assert_eq!(Category::parse(c.name()), Some(c));
        }
        assert_eq!(Category::parse(" SFX "), Some(Category::Sfx));
        assert_eq!(Category::parse("drums"), None);
    }

    #[test]
    fn older_manifests_without_new_fields_load() {
        let json = r#"{"schemaVersion": 1, "generator": "x", "project": "y", "assets": [
            {"kind": "sfx", "id": "00000000-0000-0000-0000-000000000002", "name": "hit",
             "variations": [{"path": "sfx/hit_01.wav", "channels": 1, "sampleRate": 48000, "lengthSamples": 10}]}
        ]}"#;
        let m = Manifest::from_json(json).unwrap();
        let Asset::Sfx(s) = &m.assets[0] else {
            panic!()
        };
        assert_eq!(s.category, Category::Sfx);
        assert!(s.credits.is_empty());
        assert_eq!(m.assets[0].project_id(), None);
        assert_eq!(m.assets[0].files().count(), 1);
    }

    #[test]
    fn upsert_replaces_by_name() {
        let mut m = sample_manifest();
        let mut replacement = m.find("battle_loop").unwrap().clone();
        if let Asset::Loop(l) = &mut replacement {
            l.id = AssetId::from_u128(3);
            l.loop_end = 48_000;
        }
        m.upsert(replacement);
        assert_eq!(m.assets.len(), 2);
        assert_eq!(m.find("battle_loop").unwrap().id(), AssetId::from_u128(3));
        assert!(m.remove(AssetId::from_u128(3)).is_some());
        assert!(m.find("battle_loop").is_none());
        assert_eq!(m.find("footstep").unwrap().project_id(), Some("project-1"));
    }
}
