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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Category {
    #[default]
    Music,
    Sfx,
    Ui,
    Voice,
    Ambience,
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
    }
}
