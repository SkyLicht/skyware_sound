// SPDX-License-Identifier: GPL-3.0-or-later
//! Game-audio exports.
//!
//! An export writes audio files into an export folder and records each one
//! in the folder's `skyware-manifest.json`, which `sky-runtime` loads in the
//! game. Exporting again under the same name replaces the file and keeps the
//! asset's ID, so game code that refers to it keeps working.
//!
//! [`export_assets`] writes a set of loops and SFX sets in one go, the way a
//! project's game assets are exported: every file first, then the manifest,
//! then the files that the new manifest no longer uses are deleted.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use sky_core::AssetId;
use sky_core::manifest::{
    Asset, AudioFile, Category, LoopAsset, MANIFEST_FILE_NAME, Manifest, RandomRange, Selection,
    SfxAsset, asset_name, is_asset_name,
};
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
/// underscores. The same rule as [`sky_core::manifest::asset_name`].
pub fn file_stem(name: &str) -> String {
    asset_name(name)
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

/// The rendered audio of one asset.
pub enum AssetAudio<'a> {
    /// A file that plays once from the start and then repeats
    /// `[loop_start, loop_end)` forever.
    Loop {
        audio: &'a AudioData,
        /// Frames of intro before the repeating body; 0 for none.
        loop_start: u64,
        /// One past the body's last frame, normally the file's length.
        loop_end: u64,
        tempo_bpm: Option<f64>,
        time_signature: Option<[u32; 2]>,
        /// Bars in the repeating body.
        bars: Option<f64>,
    },
    /// Variations of one sound, each written to its own numbered file.
    Sfx {
        variations: Vec<&'a AudioData>,
        selection: Selection,
        pitch_semitones: RandomRange,
        gain_db: RandomRange,
    },
}

/// One asset to export.
pub struct AssetExport<'a> {
    /// The name the game plays it by; see [`sky_core::manifest::asset_name`].
    pub name: String,
    /// The ID written into the manifest.
    pub id: AssetId,
    pub category: Category,
    pub tags: Vec<String>,
    /// Credit lines for the manifest, from the licenses of the sounds used.
    pub credits: Vec<String>,
    pub audio: AssetAudio<'a>,
}

/// How [`export_assets`] writes a set.
#[derive(Clone, Debug)]
pub struct ExportSet {
    /// The name of the project the assets came from.
    pub project: String,
    /// Marks the assets as managed by this project (see
    /// [`LoopAsset::project_id`]). One-off exports leave it out.
    pub project_id: Option<String>,
    /// Remove the project's assets that are not in this set, with their
    /// files. Needs `project_id`; use it when the set is all of the
    /// project's game assets.
    pub prune: bool,
    pub format: WavFormat,
}

/// What [`export_assets`] did.
#[derive(Clone, Debug)]
pub struct SetExported {
    /// The manifest entries written, in the order given.
    pub assets: Vec<Asset>,
    /// Every audio file written.
    pub files: Vec<PathBuf>,
    /// Names of the assets removed from the manifest by pruning.
    pub removed: Vec<String>,
    /// Old audio files the manifest no longer uses, now deleted.
    pub deleted: Vec<PathBuf>,
    pub manifest: PathBuf,
}

/// Writes every asset's audio into `dir`, then records them in the
/// manifest. Files from earlier exports that the manifest no longer refers
/// to (a renamed asset, variations that went away, pruned assets) are
/// deleted afterwards.
///
/// Nothing is written unless every asset is valid; a failure while writing
/// audio leaves the manifest as it was.
pub fn export_assets(
    dir: &Path,
    set: &ExportSet,
    assets: &[AssetExport],
) -> Result<SetExported, GameError> {
    if set.prune && set.project_id.is_none() {
        return Err(GameError::Invalid(
            "pruning needs the project's ID, so only its own assets are removed".into(),
        ));
    }
    let mut seen = BTreeSet::new();
    for a in assets {
        validate(a)?;
        if !seen.insert(a.name.as_str()) {
            return Err(GameError::Invalid(format!(
                "two assets are called {:?}",
                a.name
            )));
        }
    }

    // Write the audio under temporary names, then move it into place, so a
    // failure part way leaves the old files and manifest in step.
    let mut entries = Vec::with_capacity(assets.len());
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
    let result = (|| {
        for a in assets {
            let (entry, files) = render_entry(a, set)?;
            for (rel, audio) in files {
                let target = dir.join(&rel);
                let tmp = tmp_path(&target);
                write_wav(&tmp, audio, set.format, set.format != WavFormat::Float32)?;
                staged.push((tmp, target));
            }
            entries.push(entry);
        }
        Ok::<_, GameError>(())
    })();
    if let Err(e) = result {
        for (tmp, _) in &staged {
            let _ = fs::remove_file(tmp);
        }
        return Err(e);
    }

    let mut manifest =
        read_manifest(dir)?.unwrap_or_else(|| Manifest::new(generator(), set.project.as_str()));
    manifest.generator = generator();
    manifest.project = set.project.clone();
    let before: Vec<Asset> = manifest.assets.clone();

    let mut files = Vec::with_capacity(staged.len());
    for (tmp, target) in staged {
        fs::rename(&tmp, &target).map_err(|e| {
            GameError::Invalid(format!("cannot move {} into place: {e}", target.display()))
        })?;
        files.push(target);
    }

    let mut removed = Vec::new();
    if set.prune {
        let keep: BTreeSet<AssetId> = entries.iter().map(Asset::id).collect();
        let gone: Vec<AssetId> = manifest
            .assets
            .iter()
            .filter(|a| a.project_id().is_some() && a.project_id() == set.project_id.as_deref())
            .map(Asset::id)
            .filter(|id| !keep.contains(id))
            .collect();
        for id in gone {
            if let Some(a) = manifest.remove(id) {
                removed.push(a.name().to_string());
            }
        }
    }
    for entry in &entries {
        manifest.upsert(entry.clone());
    }
    write_manifest(dir, &manifest)?;

    let deleted = delete_unused(dir, &before, &manifest);
    Ok(SetExported {
        assets: entries,
        files,
        removed,
        deleted,
        manifest: manifest_path(dir),
    })
}

fn validate(a: &AssetExport) -> Result<(), GameError> {
    if !is_asset_name(&a.name) {
        return Err(GameError::Invalid(format!(
            "{:?} is not an asset name; use lowercase letters, digits and underscores",
            a.name
        )));
    }
    match &a.audio {
        AssetAudio::Loop {
            audio,
            loop_start,
            loop_end,
            ..
        } => {
            let frames = audio.frames() as u64;
            if frames == 0 {
                return Err(GameError::Invalid(format!(
                    "the loop {:?} is empty",
                    a.name
                )));
            }
            if *loop_end > frames || loop_start >= loop_end {
                return Err(GameError::Invalid(format!(
                    "the loop points {loop_start}..{loop_end} of {:?} do not fit a file of \
                     {frames} samples",
                    a.name
                )));
            }
        }
        AssetAudio::Sfx { variations, .. } => {
            if variations.is_empty() {
                return Err(GameError::Invalid(format!(
                    "the SFX set {:?} has no variations",
                    a.name
                )));
            }
            if variations.iter().any(|v| v.frames() == 0) {
                return Err(GameError::Invalid(format!(
                    "a variation of {:?} is empty",
                    a.name
                )));
            }
        }
    }
    Ok(())
}

fn category_dir(category: Category) -> &'static str {
    category.name()
}

fn audio_file(path: String, audio: &AudioData) -> AudioFile {
    AudioFile {
        path,
        channels: audio.channel_count() as u16,
        sample_rate: audio.sample_rate,
        length_samples: audio.frames() as u64,
    }
}

/// Audio files to write: each path relative to the export folder, and its
/// audio.
type Files<'a> = Vec<(String, &'a AudioData)>;

/// The manifest entry for an asset and the files it needs.
fn render_entry<'a>(a: &'a AssetExport, set: &ExportSet) -> Result<(Asset, Files<'a>), GameError> {
    let dir = category_dir(a.category);
    Ok(match &a.audio {
        AssetAudio::Loop {
            audio,
            loop_start,
            loop_end,
            tempo_bpm,
            time_signature,
            bars,
        } => {
            let rel = format!("{dir}/{}.wav", a.name);
            let stats = analysis::analyze(audio);
            let entry = Asset::Loop(LoopAsset {
                id: a.id,
                name: a.name.clone(),
                category: a.category,
                file: audio_file(rel.clone(), audio),
                loop_start: *loop_start,
                loop_end: *loop_end,
                tempo_bpm: *tempo_bpm,
                time_signature: *time_signature,
                bars: *bars,
                key: None,
                loudness_lufs: stats.integrated_lufs,
                peak_dbfs: Some(stats.peak_dbfs as f64),
                tags: a.tags.clone(),
                credits: a.credits.clone(),
                project_id: set.project_id.clone(),
            });
            (entry, vec![(rel, *audio)])
        }
        AssetAudio::Sfx {
            variations,
            selection,
            pitch_semitones,
            gain_db,
        } => {
            let files: Files = variations
                .iter()
                .enumerate()
                .map(|(i, v)| (format!("{dir}/{}_{:02}.wav", a.name, i + 1), *v))
                .collect();
            // Loudness over all variations played back to back; the peak of
            // the loudest one.
            let joined = join(variations);
            let stats = analysis::analyze(&joined);
            let entry = Asset::Sfx(SfxAsset {
                id: a.id,
                name: a.name.clone(),
                category: a.category,
                variations: files
                    .iter()
                    .map(|(rel, audio)| audio_file(rel.clone(), audio))
                    .collect(),
                selection: *selection,
                pitch_semitones: *pitch_semitones,
                gain_db: *gain_db,
                loudness_lufs: stats.integrated_lufs,
                peak_dbfs: Some(stats.peak_dbfs as f64),
                tags: a.tags.clone(),
                credits: a.credits.clone(),
                project_id: set.project_id.clone(),
            });
            (entry, files)
        }
    })
}

/// The variations one after another, in stereo, for loudness analysis.
fn join(variations: &[&AudioData]) -> AudioData {
    let sample_rate = variations.first().map_or(48_000, |v| v.sample_rate);
    let mut left = Vec::new();
    let mut right = Vec::new();
    for v in variations {
        left.extend_from_slice(v.left());
        right.extend_from_slice(v.right());
    }
    AudioData {
        sample_rate,
        channels: vec![left, right],
    }
}

fn tmp_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    target.with_file_name(name)
}

/// Deletes the audio files that `before` used and `after` does not, when
/// they are plain relative paths inside `dir`. Returns what was deleted.
fn delete_unused(dir: &Path, before: &[Asset], after: &Manifest) -> Vec<PathBuf> {
    let used: BTreeSet<&str> = after
        .assets
        .iter()
        .flat_map(|a| a.files().map(|f| f.path.as_str()))
        .collect();
    let old: BTreeSet<&str> = before
        .iter()
        .flat_map(|a| a.files().map(|f| f.path.as_str()))
        .collect();
    let mut deleted = Vec::new();
    for rel in old.difference(&used) {
        let Some(path) = inside(dir, rel) else {
            continue;
        };
        if path.is_file() && fs::remove_file(&path).is_ok() {
            deleted.push(path);
        }
    }
    deleted
}

/// `rel` joined onto `dir` if it is a relative path to a WAV file that
/// cannot leave `dir`.
fn inside(dir: &Path, rel: &str) -> Option<PathBuf> {
    let rel = Path::new(rel);
    let plain = rel.components().all(|c| matches!(c, Component::Normal(_)));
    let wav = rel
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wav"));
    (plain && wav).then(|| dir.join(rel))
}

/// A rendered loop ready to export on its own.
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
/// The asset is named from `export.name` and keeps the ID of an asset of
/// that name already in the manifest.
pub fn export_loop(dir: &Path, export: &LoopExport) -> Result<Exported, GameError> {
    let name = asset_name(export.name);
    let id = read_manifest(dir)?
        .and_then(|m| m.find(&name).map(Asset::id))
        .unwrap_or_default();
    let set = ExportSet {
        project: export.project.to_string(),
        project_id: None,
        prune: false,
        format: export.format,
    };
    let mut out = export_assets(
        dir,
        &set,
        &[AssetExport {
            name,
            id,
            category: export.category,
            tags: export.tags.clone(),
            credits: export.credits.clone(),
            audio: AssetAudio::Loop {
                audio: export.audio,
                loop_start: export.loop_start,
                loop_end: export.loop_end,
                tempo_bpm: export.tempo_bpm,
                time_signature: export.time_signature,
                bars: export.bars,
            },
        }],
    )?;
    let asset = out
        .assets
        .pop()
        .ok_or_else(|| GameError::Invalid("nothing was exported".into()))?;
    let file = out
        .files
        .pop()
        .ok_or_else(|| GameError::Invalid("nothing was written".into()))?;
    Ok(Exported {
        asset,
        file,
        manifest: out.manifest,
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
        assert_eq!(l.project_id, None);
        let bad = LoopExport {
            loop_end: 99_999,
            ..export
        };
        assert!(export_loop(dir.path(), &bad).is_err());
        // No temporary files are left behind.
        let leftovers: Vec<_> = fs::read_dir(dir.path().join("music"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n.to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    fn sfx<'a>(name: &str, id: u128, variations: Vec<&'a AudioData>) -> AssetExport<'a> {
        AssetExport {
            name: name.into(),
            id: AssetId::from_u128(id),
            category: Category::Sfx,
            tags: vec![],
            credits: vec![],
            audio: AssetAudio::Sfx {
                variations,
                selection: Selection::Shuffle,
                pitch_semitones: RandomRange::new(-1.0, 1.0),
                gain_db: RandomRange::new(-3.0, 0.0),
            },
        }
    }

    fn intro_loop<'a>(name: &str, id: u128, audio: &'a AudioData) -> AssetExport<'a> {
        AssetExport {
            name: name.into(),
            id: AssetId::from_u128(id),
            category: Category::Music,
            tags: vec!["battle".into()],
            credits: vec![],
            audio: AssetAudio::Loop {
                audio,
                loop_start: 1_000,
                loop_end: audio.frames() as u64,
                tempo_bpm: Some(120.0),
                time_signature: Some([4, 4]),
                bars: Some(1.0),
            },
        }
    }

    fn project_set(prune: bool) -> ExportSet {
        ExportSet {
            project: "Game".into(),
            project_id: Some("p1".into()),
            prune,
            format: WavFormat::Pcm16,
        }
    }

    #[test]
    fn sets_write_loops_and_numbered_variations() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (tone(4_000), tone(5_000), tone(6_000));
        let music = tone(20_000);
        let out = export_assets(
            dir.path(),
            &project_set(true),
            &[
                intro_loop("battle", 1, &music),
                sfx("hit", 2, vec![&a, &b, &c]),
            ],
        )
        .unwrap();
        assert_eq!(out.files.len(), 4);
        for f in [
            "music/battle.wav",
            "sfx/hit_01.wav",
            "sfx/hit_02.wav",
            "sfx/hit_03.wav",
        ] {
            assert!(dir.path().join(f).is_file(), "{f}");
        }
        let m = read_manifest(dir.path()).unwrap().unwrap();
        let Some(Asset::Loop(l)) = m.find("battle") else {
            panic!()
        };
        assert_eq!((l.loop_start, l.loop_end), (1_000, 20_000));
        assert_eq!(l.project_id.as_deref(), Some("p1"));
        let Some(Asset::Sfx(s)) = m.find("hit") else {
            panic!()
        };
        assert_eq!(s.id, AssetId::from_u128(2));
        assert_eq!(s.selection, Selection::Shuffle);
        assert_eq!(s.variations[1].length_samples, 5_000);
        assert!(
            s.peak_dbfs.is_some_and(|p| p < -9.0 && p > -12.0),
            "{:?}",
            s.peak_dbfs
        );

        // Fewer variations and a renamed loop: the stale files go, and the
        // renamed loop keeps its ID.
        let out = export_assets(
            dir.path(),
            &project_set(true),
            &[
                intro_loop("battle_theme", 1, &music),
                sfx("hit", 2, vec![&a, &b]),
            ],
        )
        .unwrap();
        assert!(out.removed.is_empty());
        assert_eq!(out.deleted.len(), 2, "{:?}", out.deleted);
        assert!(!dir.path().join("sfx/hit_03.wav").exists());
        assert!(!dir.path().join("music/battle.wav").exists());
        let m = read_manifest(dir.path()).unwrap().unwrap();
        assert_eq!(m.assets.len(), 2);
        assert_eq!(m.find("battle_theme").unwrap().id(), AssetId::from_u128(1));
    }

    #[test]
    fn pruning_removes_only_this_projects_assets() {
        let dir = tempfile::tempdir().unwrap();
        let a = tone(4_000);
        let music = tone(20_000);
        // A one-off loop export, and another project's SFX.
        export_loop(
            dir.path(),
            &LoopExport {
                name: "menu",
                project: "Menu",
                audio: &music,
                loop_start: 0,
                loop_end: 20_000,
                tempo_bpm: None,
                time_signature: None,
                bars: None,
                category: Category::Music,
                tags: vec![],
                credits: vec![],
                format: WavFormat::Pcm16,
            },
        )
        .unwrap();
        let other = ExportSet {
            project_id: Some("p2".into()),
            ..project_set(true)
        };
        export_assets(dir.path(), &other, &[sfx("door", 9, vec![&a])]).unwrap();
        export_assets(
            dir.path(),
            &project_set(true),
            &[intro_loop("battle", 1, &music), sfx("hit", 2, vec![&a])],
        )
        .unwrap();

        // Exporting only part of the set without pruning keeps the rest.
        export_assets(dir.path(), &project_set(false), &[sfx("hit", 2, vec![&a])]).unwrap();
        assert!(
            read_manifest(dir.path())
                .unwrap()
                .unwrap()
                .find("battle")
                .is_some()
        );

        // The project dropped its loop: pruning removes it and its file.
        let out =
            export_assets(dir.path(), &project_set(true), &[sfx("hit", 2, vec![&a])]).unwrap();
        assert_eq!(out.removed, ["battle"]);
        assert!(!dir.path().join("music/battle.wav").exists());
        let m = read_manifest(dir.path()).unwrap().unwrap();
        let names: Vec<&str> = m.assets.iter().map(Asset::name).collect();
        assert_eq!(names, ["door", "hit", "menu"]);
        assert!(dir.path().join("music/menu.wav").exists());
        assert!(dir.path().join("sfx/door_01.wav").exists());
    }

    #[test]
    fn invalid_sets_write_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let a = tone(4_000);
        let empty = AudioData {
            sample_rate: 48_000,
            channels: vec![vec![], vec![]],
        };
        for assets in [
            vec![sfx("Hit", 1, vec![&a])],
            vec![sfx("hit", 1, vec![])],
            vec![sfx("hit", 1, vec![&a, &empty])],
            vec![sfx("hit", 1, vec![&a]), sfx("hit", 2, vec![&a])],
        ] {
            assert!(export_assets(dir.path(), &project_set(true), &assets).is_err());
        }
        let no_id = ExportSet {
            project_id: None,
            ..project_set(true)
        };
        assert!(export_assets(dir.path(), &no_id, &[sfx("hit", 1, vec![&a])]).is_err());
        assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn only_plain_wav_paths_inside_the_folder_are_deleted() {
        let dir = Path::new("export");
        assert_eq!(
            inside(dir, "sfx/hit_01.wav"),
            Some(dir.join("sfx/hit_01.wav"))
        );
        assert_eq!(inside(dir, "../hit.wav"), None);
        assert_eq!(inside(dir, "/etc/hit.wav"), None);
        assert_eq!(inside(dir, "sfx/./hit.wav"), Some(dir.join("sfx/hit.wav")));
        assert_eq!(inside(dir, "sfx/notes.txt"), None);
    }
}
