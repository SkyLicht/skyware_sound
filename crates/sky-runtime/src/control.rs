// SPDX-License-Identifier: MIT OR Apache-2.0
//! The game-thread half of the runtime: loaded assets, variation picking,
//! the random generator, and which voice slots are in use. It turns calls
//! such as `play` into [`MixCommand`]s for the mixer.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sky_core::manifest::{Asset, AudioFile, Category, MANIFEST_FILE_NAME, Manifest, RandomRange};

use crate::RuntimeError;
use crate::decode::{AudioData, decode_wav, decode_wav_from};
use crate::mixer::{MixCommand, VoiceStart};
use crate::rng::Rng;
use crate::select::Selector;
use crate::voice::{LoopRegion, pan_gains};

/// The number of voices [`Runtime::new`](crate::Runtime::new) allocates.
pub const DEFAULT_MAX_VOICES: usize = 64;

/// The seed a new runtime starts with, so output is reproducible even
/// without calling [`Runtime::set_seed`](crate::Runtime::set_seed).
pub const DEFAULT_SEED: u64 = 0x5EED_5EED_5EED_5EED;

/// Pitch is clamped to this many semitones either way (four octaves).
const MAX_PITCH_SEMITONES: f32 = 48.0;
/// Gains above this are clamped, so a typo cannot blow up the mix.
const MAX_GAIN_DB: f32 = 48.0;
/// How long `set_master_gain` takes to reach its target, to avoid clicks.
const MASTER_RAMP_MS: f32 = 10.0;

/// A loaded asset. Handles stay valid for the life of the runtime; reloading
/// an asset with the same name keeps its handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AssetHandle(pub(crate) u32);

/// One triggered sound. Ids are generation-checked: once the voice ends, its
/// id stops matching, so calls with a stale id do nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VoiceId {
    pub(crate) slot: u32,
    pub(crate) generation: u32,
}

/// Per-trigger settings. The default is unity gain, original pitch, center
/// pan and no fade-in.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayParams {
    /// Gain in decibels. SFX containers add their random gain offset to it.
    pub gain_db: f32,
    /// Pitch shift in semitones. SFX containers add their random offset to it.
    pub pitch_semitones: f32,
    /// Stereo position from -1 (left) to 1 (right).
    pub pan: f32,
    /// Linear fade-in time in milliseconds; 0 starts at full volume.
    pub fade_in_ms: f32,
}

/// Something suspicious found while loading that did not stop the load,
/// such as a manifest whose channel count disagrees with the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadWarning {
    /// The asset the warning is about.
    pub asset: String,
    /// The audio file involved, if any.
    pub path: Option<PathBuf>,
    pub message: String,
}

impl fmt::Display for LoadWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.path {
            Some(path) => write!(f, "{} ({}): {}", self.asset, path.display(), self.message),
            None => write!(f, "{}: {}", self.asset, self.message),
        }
    }
}

struct LoadedAsset {
    info: Asset,
    kind: LoadedKind,
}

enum LoadedKind {
    Loop {
        audio: Arc<AudioData>,
        region: LoopRegion,
    },
    Sfx {
        variations: Box<[Arc<AudioData>]>,
        selector: Selector,
        pitch: RandomRange,
        gain: RandomRange,
    },
}

/// What the game thread knows about a voice slot.
#[derive(Clone, Copy, Debug)]
struct Slot {
    generation: u32,
    active: bool,
    is_loop: bool,
    /// Start order, used to steal the oldest SFX voice.
    started: u64,
    stereo: bool,
    /// File rate over output rate.
    base_rate: f64,
    /// Random offsets picked at trigger time, kept across later changes.
    random_pitch: f32,
    random_gain_db: f32,
}

impl Slot {
    const FREE: Slot = Slot {
        generation: 0,
        active: false,
        is_loop: false,
        started: 0,
        stereo: false,
        base_rate: 1.0,
        random_pitch: 0.0,
        random_gain_db: 0.0,
    };
}

pub(crate) struct Control {
    sample_rate: u32,
    assets: Vec<LoadedAsset>,
    names: HashMap<String, AssetHandle>,
    slots: Box<[Slot]>,
    next_start: u64,
    rng: Rng,
    warnings: Vec<LoadWarning>,
}

/// Reads one audio file of a manifest, by its path in the manifest, and
/// returns its full path (for messages) and its audio.
type OpenFile<'a> = dyn FnMut(&str) -> Result<(PathBuf, AudioData), RuntimeError> + 'a;

impl Control {
    pub(crate) fn new(sample_rate: u32, max_voices: usize) -> Self {
        Self {
            sample_rate: sample_rate.max(1),
            assets: Vec::new(),
            names: HashMap::new(),
            slots: vec![Slot::FREE; max_voices].into_boxed_slice(),
            next_start: 0,
            rng: Rng::new(DEFAULT_SEED),
            warnings: Vec::new(),
        }
    }

    pub(crate) fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub(crate) fn asset_count(&self) -> usize {
        self.assets.len()
    }

    pub(crate) fn load_manifest(&mut self, path: &Path) -> Result<Vec<AssetHandle>, RuntimeError> {
        let manifest_path = if path.is_dir() {
            path.join(MANIFEST_FILE_NAME)
        } else {
            path.to_path_buf()
        };
        let text = std::fs::read_to_string(&manifest_path).map_err(|source| RuntimeError::Io {
            path: manifest_path.clone(),
            source,
        })?;
        let manifest = Manifest::from_json(&text)?;
        let base_dir = manifest_path.parent().unwrap_or(Path::new(""));
        self.load_manifest_from(&manifest, base_dir)
    }

    pub(crate) fn load_manifest_from(
        &mut self,
        manifest: &Manifest,
        base_dir: &Path,
    ) -> Result<Vec<AssetHandle>, RuntimeError> {
        self.load(manifest, &mut |rel| {
            let path = resolve(base_dir, rel);
            let audio = decode_wav(&path)?;
            Ok((path, audio))
        })
    }

    pub(crate) fn load_manifest_with(
        &mut self,
        manifest_json: &str,
        read: &mut dyn FnMut(&str) -> std::io::Result<Vec<u8>>,
    ) -> Result<Vec<AssetHandle>, RuntimeError> {
        let manifest = Manifest::from_json(manifest_json)?;
        self.load(&manifest, &mut |rel| {
            let path = PathBuf::from(rel);
            let bytes = read(rel).map_err(|source| RuntimeError::Io {
                path: path.clone(),
                source,
            })?;
            let audio = decode_wav_from(std::io::Cursor::new(bytes), &path)?;
            Ok((path, audio))
        })
    }

    /// Loads every asset of `manifest`, reading each audio file once through
    /// `open`, which returns the file's path (for messages) and its audio.
    fn load(
        &mut self,
        manifest: &Manifest,
        open: &mut OpenFile<'_>,
    ) -> Result<Vec<AssetHandle>, RuntimeError> {
        let mut cache: HashMap<String, (PathBuf, Arc<AudioData>)> = HashMap::new();
        let mut warnings = Vec::new();
        let mut prepared = Vec::with_capacity(manifest.assets.len());
        let mut load_file = |file: &AudioFile,
                             asset: &str,
                             warnings: &mut Vec<LoadWarning>|
         -> Result<Arc<AudioData>, RuntimeError> {
            let key = cache_key(&file.path);
            let (path, audio) = match cache.get(&key) {
                Some((path, audio)) => (path.clone(), Arc::clone(audio)),
                None => {
                    let (path, audio) = open(&file.path)?;
                    let audio = Arc::new(audio);
                    cache.insert(key, (path.clone(), Arc::clone(&audio)));
                    (path, audio)
                }
            };
            check_file(file, &audio, asset, &path, warnings);
            Ok(audio)
        };

        for asset in &manifest.assets {
            let kind = match asset {
                Asset::Loop(l) => {
                    let audio = load_file(&l.file, &l.name, &mut warnings)?;
                    let region =
                        loop_region(&l.name, l.loop_start, l.loop_end, &audio, &mut warnings)?;
                    LoadedKind::Loop { audio, region }
                }
                Asset::Sfx(s) => {
                    let variations = s
                        .variations
                        .iter()
                        .map(|file| load_file(file, &s.name, &mut warnings))
                        .collect::<Result<Box<[_]>, _>>()?;
                    LoadedKind::Sfx {
                        selector: Selector::new(s.selection, variations.len()),
                        variations,
                        pitch: sanitize_range(s.pitch_semitones),
                        gain: sanitize_range(s.gain_db),
                    }
                }
            };
            prepared.push(LoadedAsset {
                info: asset.clone(),
                kind,
            });
        }

        let mut handles = Vec::with_capacity(prepared.len());
        for asset in prepared {
            let name = asset.info.name().to_string();
            let handle = match self.names.get(&name) {
                Some(&handle) => {
                    self.assets[handle.0 as usize] = asset;
                    handle
                }
                None => {
                    let handle = AssetHandle(self.assets.len() as u32);
                    self.assets.push(asset);
                    self.names.insert(name, handle);
                    handle
                }
            };
            handles.push(handle);
        }
        self.warnings.extend(warnings);
        Ok(handles)
    }

    pub(crate) fn asset(&self, name: &str) -> Option<AssetHandle> {
        self.names.get(name).copied()
    }

    pub(crate) fn asset_info(&self, handle: AssetHandle) -> Option<&Asset> {
        self.assets.get(handle.0 as usize).map(|a| &a.info)
    }

    pub(crate) fn warnings(&self) -> &[LoadWarning] {
        &self.warnings
    }

    pub(crate) fn clear_warnings(&mut self) {
        self.warnings.clear();
    }

    /// Marks slots free that the mixer says are no longer sounding.
    pub(crate) fn sync(&mut self, active: impl Fn(usize) -> bool) {
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.active && !active(i) {
                slot.active = false;
            }
        }
    }

    /// The mixer reported that a voice ended.
    pub(crate) fn ended(&mut self, slot: u32, generation: u32) {
        if let Some(s) = self.slots.get_mut(slot as usize)
            && s.generation == generation
        {
            s.active = false;
        }
    }

    /// Starts an asset; see [`Runtime::play`](crate::Runtime::play).
    pub(crate) fn play(
        &mut self,
        asset: AssetHandle,
        params: PlayParams,
    ) -> Result<(VoiceId, MixCommand), RuntimeError> {
        let output_rate = f64::from(self.sample_rate);
        let loaded = self
            .assets
            .get_mut(asset.0 as usize)
            .ok_or(RuntimeError::InvalidHandle(asset))?;
        if let LoadedKind::Sfx { variations, .. } = &loaded.kind
            && variations.is_empty()
        {
            return Err(RuntimeError::EmptySfx(loaded.info.name().to_string()));
        }
        let slot = find_slot(&self.slots).ok_or(RuntimeError::NoFreeVoice)?;

        let (audio, region, pitch_offset, gain_offset) = match &mut loaded.kind {
            LoadedKind::Loop { audio, region } => (Arc::clone(audio), Some(*region), 0.0, 0.0),
            LoadedKind::Sfx {
                variations,
                selector,
                pitch,
                gain,
            } => {
                let index = selector.next(&mut self.rng);
                let pitch_offset = pitch.lerp(self.rng.next_f32());
                let gain_offset = gain.lerp(self.rng.next_f32());
                let audio = variations.get(index).or(variations.first()).cloned();
                match audio {
                    Some(audio) => (audio, None, pitch_offset, gain_offset),
                    None => return Err(RuntimeError::EmptySfx(loaded.info.name().to_string())),
                }
            }
        };
        let category = loaded.info.category().index();

        let base_rate = f64::from(audio.sample_rate) / output_rate;
        let stereo = audio.right.is_some();
        let rate = pitch_rate(
            finite_or_zero(params.pitch_semitones) + pitch_offset,
            base_rate,
        );
        let pan = pan_gains(finite_or_zero(params.pan), stereo);
        let fade_in_frames = ms_to_frames(params.fade_in_ms, self.sample_rate);

        let started = self.next_start;
        self.next_start += 1;
        let s = &mut self.slots[slot];
        let generation = s.generation.wrapping_add(1);
        *s = Slot {
            generation,
            active: true,
            is_loop: region.is_some(),
            started,
            stereo,
            base_rate,
            random_pitch: pitch_offset,
            random_gain_db: gain_offset,
        };
        let cmd = MixCommand::Start {
            slot: slot as u32,
            start: VoiceStart {
                generation,
                audio,
                region,
                rate,
                gain: db_to_amp(finite_or_zero(params.gain_db) + gain_offset),
                fade_in_frames,
                pan,
                category,
            },
        };
        let id = VoiceId {
            slot: slot as u32,
            generation,
        };
        Ok((id, cmd))
    }

    pub(crate) fn play_by_name(
        &mut self,
        name: &str,
        params: PlayParams,
    ) -> Result<(VoiceId, MixCommand), RuntimeError> {
        let handle = self
            .asset(name)
            .ok_or_else(|| RuntimeError::UnknownAsset(name.to_string()))?;
        self.play(handle, params)
    }

    /// The slot of a voice that is still sounding, as far as this side knows.
    fn live(&self, voice: VoiceId) -> Option<&Slot> {
        self.slots
            .get(voice.slot as usize)
            .filter(|s| s.active && s.generation == voice.generation)
    }

    /// A stop without a fade frees the slot right away, as the mixer does,
    /// so the next voice can take it before the mixer reports back.
    pub(crate) fn stop(&mut self, voice: VoiceId, fade_out_ms: f32) -> Option<MixCommand> {
        self.live(voice)?;
        let frames = ms_to_frames(fade_out_ms, self.sample_rate);
        if frames == 0 {
            self.slots[voice.slot as usize].active = false;
        }
        Some(MixCommand::Stop {
            slot: voice.slot,
            generation: voice.generation,
            frames,
        })
    }

    pub(crate) fn stop_all(&mut self, fade_out_ms: f32) -> MixCommand {
        let frames = ms_to_frames(fade_out_ms, self.sample_rate);
        if frames == 0 {
            for slot in &mut self.slots {
                slot.active = false;
            }
        }
        MixCommand::StopAll { frames }
    }

    pub(crate) fn set_voice_gain(
        &self,
        voice: VoiceId,
        gain_db: f32,
        ramp_ms: f32,
    ) -> Option<MixCommand> {
        self.live(voice).map(|s| MixCommand::Gain {
            slot: voice.slot,
            generation: voice.generation,
            amp: db_to_amp(finite_or_zero(gain_db) + s.random_gain_db),
            frames: ms_to_frames(ramp_ms, self.sample_rate),
        })
    }

    pub(crate) fn set_voice_pan(
        &self,
        voice: VoiceId,
        pan: f32,
        ramp_ms: f32,
    ) -> Option<MixCommand> {
        self.live(voice).map(|s| {
            let (left, right) = pan_gains(finite_or_zero(pan), s.stereo);
            MixCommand::Pan {
                slot: voice.slot,
                generation: voice.generation,
                left,
                right,
                frames: ms_to_frames(ramp_ms, self.sample_rate),
            }
        })
    }

    pub(crate) fn set_voice_pitch(&self, voice: VoiceId, semitones: f32) -> Option<MixCommand> {
        self.live(voice).map(|s| MixCommand::Rate {
            slot: voice.slot,
            generation: voice.generation,
            rate: pitch_rate(finite_or_zero(semitones) + s.random_pitch, s.base_rate),
        })
    }

    pub(crate) fn set_master_gain(&self, gain_db: f32) -> MixCommand {
        MixCommand::Master {
            amp: db_to_amp(finite_or_zero(gain_db)),
            frames: ms_to_frames(MASTER_RAMP_MS, self.sample_rate),
        }
    }

    pub(crate) fn set_category_gain(
        &self,
        category: Category,
        gain_db: f32,
        ramp_ms: f32,
    ) -> MixCommand {
        MixCommand::Category {
            index: category.index(),
            amp: db_to_amp(gain_db),
            frames: ms_to_frames(ramp_ms, self.sample_rate),
        }
    }

    pub(crate) fn is_playing(&self, voice: VoiceId) -> bool {
        self.live(voice).is_some()
    }

    pub(crate) fn active_voices(&self) -> usize {
        self.slots.iter().filter(|s| s.active).count()
    }

    pub(crate) fn set_seed(&mut self, seed: u64) {
        self.rng = Rng::new(seed);
        for asset in &mut self.assets {
            if let LoadedKind::Sfx { selector, .. } = &mut asset.kind {
                selector.reset();
            }
        }
    }
}

/// A free slot, or else the oldest SFX voice to steal.
fn find_slot(slots: &[Slot]) -> Option<usize> {
    if let Some(free) = slots.iter().position(|s| !s.active) {
        return Some(free);
    }
    slots
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.is_loop)
        .min_by_key(|(_, s)| s.started)
        .map(|(i, _)| i)
}

/// The playback rate for a pitch shift, clamped to four octaves either way.
fn pitch_rate(semitones: f32, base_rate: f64) -> f64 {
    let semitones = finite_or_zero(semitones).clamp(-MAX_PITCH_SEMITONES, MAX_PITCH_SEMITONES);
    2f64.powf(f64::from(semitones) / 12.0) * base_rate
}

/// Warns when a file does not match what the manifest says about it.
fn check_file(
    file: &AudioFile,
    audio: &AudioData,
    asset: &str,
    path: &Path,
    warnings: &mut Vec<LoadWarning>,
) {
    let mut warn = |message: String| {
        warnings.push(LoadWarning {
            asset: asset.to_string(),
            path: Some(path.to_path_buf()),
            message,
        });
    };
    if file.channels != audio.source_channels {
        warn(format!(
            "the manifest says {} channel(s) but the file has {}",
            file.channels, audio.source_channels
        ));
    }
    if audio.source_channels > 2 {
        warn(format!(
            "the file has {} channels; only the first two are played",
            audio.source_channels
        ));
    }
    if file.sample_rate != audio.sample_rate {
        warn(format!(
            "the manifest says {} Hz but the file is {} Hz",
            file.sample_rate, audio.sample_rate
        ));
    }
    let decoded = audio.frames as u64;
    let tolerance = (file.length_samples / 1000).max(1);
    if file.length_samples.abs_diff(decoded) > tolerance {
        warn(format!(
            "the manifest says {} samples but the file has {decoded}",
            file.length_samples
        ));
    }
}

/// Clamps loop points to the decoded audio.
fn loop_region(
    name: &str,
    loop_start: u64,
    loop_end: u64,
    audio: &AudioData,
    warnings: &mut Vec<LoadWarning>,
) -> Result<LoopRegion, RuntimeError> {
    let frames = audio.frames as u64;
    if frames == 0 {
        return Err(RuntimeError::InvalidLoop {
            name: name.to_string(),
            message: "its audio file has no samples".into(),
        });
    }
    let mut warn = |message: String| {
        warnings.push(LoadWarning {
            asset: name.to_string(),
            path: None,
            message,
        });
    };
    let mut end = loop_end;
    if end == 0 || end > frames {
        warn(format!(
            "loop end {loop_end} is outside the file's {frames} samples; using {frames}"
        ));
        end = frames;
    }
    let mut start = loop_start;
    if start >= end {
        warn(format!(
            "loop start {loop_start} is not before loop end {end}; using 0"
        ));
        start = 0;
    }
    Ok(LoopRegion {
        start: start as usize,
        end: end as usize,
    })
}

/// Joins a manifest path (forward slashes, relative) onto `base_dir`.
fn resolve(base_dir: &Path, relative: &str) -> PathBuf {
    if Path::new(relative).is_absolute() {
        return PathBuf::from(relative);
    }
    let mut path = base_dir.to_path_buf();
    for part in relative.split(['/', '\\']) {
        if !part.is_empty() && part != "." {
            path.push(part);
        }
    }
    path
}

/// A manifest path spelled one way, so files listed as `sfx/hit.wav` and
/// `./sfx\hit.wav` are read once.
fn cache_key(relative: &str) -> String {
    if Path::new(relative).is_absolute() {
        return relative.to_string();
    }
    relative
        .split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/")
}

fn sanitize_range(range: RandomRange) -> RandomRange {
    RandomRange::new(finite_or_zero(range.min), finite_or_zero(range.max))
}

fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

/// Decibels to a linear gain: `-inf` is silence, `NaN` counts as 0 dB and
/// anything above +48 dB is clamped.
fn db_to_amp(db: f32) -> f32 {
    if db.is_nan() {
        return 1.0;
    }
    10f32.powf(db.min(MAX_GAIN_DB) / 20.0)
}

fn ms_to_frames(ms: f32, sample_rate: u32) -> u32 {
    if ms.is_nan() || ms <= 0.0 {
        return 0;
    }
    (f64::from(ms) * f64::from(sample_rate) / 1000.0)
        .round()
        .min(f64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decibels() {
        assert_eq!(db_to_amp(0.0), 1.0);
        assert!((db_to_amp(-6.0206) - 0.5).abs() < 1e-4);
        assert_eq!(db_to_amp(f32::NEG_INFINITY), 0.0);
        assert_eq!(db_to_amp(f32::NAN), 1.0);
        assert!(db_to_amp(1000.0).is_finite());
    }

    #[test]
    fn milliseconds() {
        assert_eq!(ms_to_frames(10.0, 48_000), 480);
        assert_eq!(ms_to_frames(0.0, 48_000), 0);
        assert_eq!(ms_to_frames(-5.0, 48_000), 0);
        assert_eq!(ms_to_frames(f32::NAN, 48_000), 0);
    }

    #[test]
    fn pitch_rates() {
        assert_eq!(pitch_rate(0.0, 1.0), 1.0);
        assert!((pitch_rate(12.0, 0.5) - 1.0).abs() < 1e-12);
        assert_eq!(pitch_rate(f32::NAN, 2.0), 2.0);
        assert!((pitch_rate(1000.0, 1.0) - 16.0).abs() < 1e-9);
    }

    #[test]
    fn paths_spelled_differently_share_a_cache_key() {
        assert_eq!(cache_key("./sfx\\hit.wav"), cache_key("sfx//hit.wav"));
        assert_eq!(cache_key("sfx/hit.wav"), "sfx/hit.wav");
        assert_ne!(cache_key("sfx/Hit.wav"), cache_key("sfx/hit.wav"));
    }

    #[test]
    fn resolves_forward_slash_paths() {
        let p = resolve(Path::new("export"), "sfx/./hit_01.wav");
        assert_eq!(p, Path::new("export").join("sfx").join("hit_01.wav"));
    }
}
