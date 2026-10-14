// SPDX-License-Identifier: GPL-3.0-or-later
//! Game-audio exports.
//!
//! An export writes audio files into an export folder and records each one
//! in the folder's `skyware-manifest.json`, which `sky-runtime` loads in the
//! game. Exporting again under the same name replaces the file and keeps the
//! asset's ID, so game code that refers to it keeps working.

use std::fs;
use std::path::{Path, PathBuf};

use sky_core::AssetId;
use sky_core::manifest::{Asset, AudioFile, Category, LoopAsset, MANIFEST_FILE_NAME, Manifest};
use sky_dsp::{AudioData, analysis};
use sky_io::{WavFormat, write_wav};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GameError {
    #[error("{0}")]
    Io(#[from] sky_io::IoError),
    #[error("manifest: {0}")]
    Manifest(String),
    #[error("{0}")]
    Invalid(String),
}

/// The generator string written into manifests.
pub fn generator() -> String {
    format!("Skyware Sound {}", env!("CARGO_PKG_VERSION"))
}

/// Turns a display name into a safe file stem: lowercase letters, digits and
/// underscores.
pub fn file_stem(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    let out = out.trim_end_matches('_').to_string();
    if out.is_empty() {
        "untitled".into()
    } else {
        out
    }
}

fn category_dir(category: Category) -> &'static str {
    match category {
        Category::Music => "music",
        Category::Sfx => "sfx",
        Category::Ui => "ui",
        Category::Voice => "voice",
        Category::Ambience => "ambience",
    }
}

pub fn manifest_path(dir: &Path) -> PathBuf {
    dir.join(MANIFEST_FILE_NAME)
}

/// Reads the folder's manifest, or `None` if there is none yet.
pub fn read_manifest(dir: &Path) -> Result<Option<Manifest>, GameError> {
    let path = manifest_path(dir);
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|e| GameError::Manifest(e.to_string()))?;
    Manifest::from_json(&text)
        .map(Some)
        .map_err(|e| GameError::Manifest(e.to_string()))
}

pub fn write_manifest(dir: &Path, manifest: &Manifest) -> Result<(), GameError> {
    fs::create_dir_all(dir).map_err(|e| GameError::Manifest(e.to_string()))?;
    let tmp = dir.join(format!("{MANIFEST_FILE_NAME}.tmp"));
    fs::write(&tmp, manifest.to_json_pretty() + "\n")
        .map_err(|e| GameError::Manifest(e.to_string()))?;
    fs::rename(&tmp, manifest_path(dir)).map_err(|e| GameError::Manifest(e.to_string()))
}

/// A rendered loop ready to export.
pub struct LoopExport<'a> {
    pub name: &'a str,
    pub project: &'a str,
    pub audio: &'a AudioData,
    pub loop_start: u64,
    pub loop_end: u64,
    pub tempo_bpm: Option<f64>,
    pub time_signature: Option<[u32; 2]>,
    pub bars: Option<f64>,
    pub category: Category,
    pub tags: Vec<String>,
    /// Credit lines for the manifest, from the licenses of the sounds used.
    pub credits: Vec<String>,
    pub format: WavFormat,
}

/// The result of an export: the manifest entry and where the file went.
pub struct Exported {
    pub asset: Asset,
    pub file: PathBuf,
    pub manifest: PathBuf,
}

/// Writes a loop's WAV file into `dir` and records it in the manifest.
pub fn export_loop(dir: &Path, export: &LoopExport) -> Result<Exported, GameError> {
    let frames = export.audio.frames() as u64;
    if frames == 0 {
        return Err(GameError::Invalid("the loop is empty".into()));
    }
    if export.loop_end > frames || export.loop_start >= export.loop_end {
        return Err(GameError::Invalid(format!(
            "loop points {}..{} do not fit a file of {frames} samples",
            export.loop_start, export.loop_end
        )));
    }
    let stem = file_stem(export.name);
    let rel = format!("{}/{stem}.wav", category_dir(export.category));
    let file = dir.join(&rel);
    write_wav(
        &file,
        export.audio,
        export.format,
        export.format != WavFormat::Float32,
    )?;
    let stats = analysis::analyze(export.audio);
    let mut manifest =
        read_manifest(dir)?.unwrap_or_else(|| Manifest::new(generator(), export.project));
    manifest.generator = generator();
    let id = manifest.find(&stem).map_or_else(AssetId::new, Asset::id);
    let asset = Asset::Loop(LoopAsset {
        id,
        name: stem,
        category: export.category,
        file: AudioFile {
            path: rel,
            channels: export.audio.channel_count() as u16,
            sample_rate: export.audio.sample_rate,
            length_samples: frames,
        },
        loop_start: export.loop_start,
        loop_end: export.loop_end,
        tempo_bpm: export.tempo_bpm,
        time_signature: export.time_signature,
        bars: export.bars,
        key: None,
        loudness_lufs: stats.integrated_lufs,
        peak_dbfs: Some(stats.peak_dbfs as f64),
        tags: export.tags.clone(),
        credits: export.credits.clone(),
    });
    manifest.upsert(asset.clone());
    write_manifest(dir, &manifest)?;
    Ok(Exported {
        asset,
        file,
        manifest: manifest_path(dir),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize) -> AudioData {
        AudioData {
            sample_rate: 48_000,
            channels: vec![(0..frames).map(|i| (i as f32 * 0.01).sin() * 0.3).collect(); 2],
        }
    }

    #[test]
    fn file_stems_are_safe() {
        assert_eq!(file_stem("Battle Loop #2"), "battle_loop_2");
        assert_eq!(file_stem("  ...  "), "untitled");
        assert_eq!(file_stem("Ünïcode/../path"), "n_code_path");
    }

    #[test]
    fn exports_write_files_and_keep_ids() {
        let dir = tempfile::tempdir().unwrap();
        let audio = tone(48_000);
        let export = LoopExport {
            name: "Battle Loop",
            project: "Demo",
            audio: &audio,
            loop_start: 0,
            loop_end: 48_000,
            tempo_bpm: Some(120.0),
            time_signature: Some([4, 4]),
            bars: Some(0.5),
            category: Category::Music,
            tags: vec!["battle".into()],
            credits: vec!["Piano by Someone (CC BY 3.0)".into()],
            format: WavFormat::Pcm24,
        };
        let first = export_loop(dir.path(), &export).unwrap();
        assert!(dir.path().join("music/battle_loop.wav").exists());
        let second = export_loop(dir.path(), &export).unwrap();
        assert_eq!(first.asset.id(), second.asset.id());
        let manifest = read_manifest(dir.path()).unwrap().unwrap();
        assert_eq!(manifest.assets.len(), 1);
        let Asset::Loop(l) = &manifest.assets[0] else {
            panic!()
        };
        assert_eq!(l.loop_end, 48_000);
        assert!(l.loudness_lufs.is_some());
        assert_eq!(l.credits, ["Piano by Someone (CC BY 3.0)"]);
        let bad = LoopExport {
            loop_end: 99_999,
            ..export
        };
        assert!(export_loop(dir.path(), &bad).is_err());
    }
}
