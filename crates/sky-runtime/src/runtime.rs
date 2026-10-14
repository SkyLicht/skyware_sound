// SPDX-License-Identifier: MIT OR Apache-2.0
//! The runtime: loaded assets, the voice pool and the mixer.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sky_core::manifest::{Asset, AudioFile, MANIFEST_FILE_NAME, Manifest, RandomRange};

use crate::RuntimeError;
use crate::decode::{AudioData, decode_wav};
use crate::rng::Rng;
use crate::select::Selector;
use crate::voice::{LoopRegion, Ramp, Voice, VoiceState, pan_gains};

/// The number of voices [`Runtime::new`] allocates.
pub const DEFAULT_MAX_VOICES: usize = 64;

/// The seed a new runtime starts with, so output is reproducible even
/// without calling [`Runtime::set_seed`].
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
pub struct AssetHandle(u32);

/// One triggered sound. Ids are generation-checked: once the voice ends, its
/// id stops matching, so calls with a stale id do nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VoiceId {
    slot: u32,
    generation: u32,
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

/// Plays Skyware Sound exports: seamless loops and randomized SFX containers.
///
/// Create one per output stream, load one or more manifests, then call
/// [`play`](Self::play) from game code and [`render`](Self::render) from the
/// audio callback. The runtime is `Send`; if game code and the audio callback
/// run on different threads, share it behind your own lock or message queue.
pub struct Runtime {
    sample_rate: u32,
    assets: Vec<LoadedAsset>,
    names: HashMap<String, AssetHandle>,
    voices: Box<[Voice]>,
    next_start: u64,
    rng: Rng,
    master: Ramp,
    warnings: Vec<LoadWarning>,
}

impl fmt::Debug for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runtime")
            .field("sample_rate", &self.sample_rate)
            .field("assets", &self.assets.len())
            .field("max_voices", &self.voices.len())
            .field("active_voices", &self.active_voices())
            .finish_non_exhaustive()
    }
}

impl Runtime {
    /// A runtime that renders at `sample_rate` with 64 voices.
    pub fn new(sample_rate: u32) -> Self {
        Self::with_max_voices(sample_rate, DEFAULT_MAX_VOICES)
    }

    /// A runtime with a fixed pool of `max_voices` voices, all allocated now.
    /// A sample rate of 0 is treated as 1.
    pub fn with_max_voices(sample_rate: u32, max_voices: usize) -> Self {
        Self {
            sample_rate: sample_rate.max(1),
            assets: Vec::new(),
            names: HashMap::new(),
            voices: (0..max_voices).map(|_| Voice::new()).collect(),
            next_start: 0,
            rng: Rng::new(DEFAULT_SEED),
            master: Ramp::at(1.0),
            warnings: Vec::new(),
        }
    }

    /// The output sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The size of the voice pool.
    pub fn max_voices(&self) -> usize {
        self.voices.len()
    }

    /// Loads a manifest and every WAV file it references.
    ///
    /// `path` is either an export folder containing `skyware-manifest.json`
    /// or the path of a manifest file. Audio paths are relative to the
    /// manifest. Returns a handle for every asset, in manifest order.
    ///
    /// Loading is all-or-nothing: if any file fails, nothing is added. An
    /// asset whose name is already loaded is replaced and keeps its handle;
    /// voices already playing it finish with the old audio.
    pub fn load_manifest(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<Vec<AssetHandle>, RuntimeError> {
        let path = path.as_ref();
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

    /// Like [`load_manifest`](Self::load_manifest), from an already parsed
    /// manifest whose audio paths are relative to `base_dir`.
    pub fn load_manifest_from(
        &mut self,
        manifest: &Manifest,
        base_dir: &Path,
    ) -> Result<Vec<AssetHandle>, RuntimeError> {
        let mut cache: HashMap<PathBuf, Arc<AudioData>> = HashMap::new();
        let mut warnings = Vec::new();
        let mut prepared = Vec::with_capacity(manifest.assets.len());

        for asset in &manifest.assets {
            let kind = match asset {
                Asset::Loop(l) => {
                    let audio = load_file(&mut cache, base_dir, &l.file, &l.name, &mut warnings)?;
                    let region =
                        loop_region(&l.name, l.loop_start, l.loop_end, &audio, &mut warnings)?;
                    LoadedKind::Loop { audio, region }
                }
                Asset::Sfx(s) => {
                    let variations = s
                        .variations
                        .iter()
                        .map(|file| load_file(&mut cache, base_dir, file, &s.name, &mut warnings))
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

        // Release audio that finished voices still hold, so replaced assets
        // are freed here rather than later.
        for voice in self.voices.iter_mut().filter(|v| !v.is_active()) {
            voice.audio = None;
        }
        self.warnings.extend(warnings);
        Ok(handles)
    }

    /// The handle of the asset with this name.
    pub fn asset(&self, name: &str) -> Option<AssetHandle> {
        self.names.get(name).copied()
    }

    /// The manifest entry of a loaded asset.
    pub fn asset_info(&self, handle: AssetHandle) -> Option<&Asset> {
        self.assets.get(handle.0 as usize).map(|a| &a.info)
    }

    /// Warnings collected by every load so far.
    pub fn warnings(&self) -> &[LoadWarning] {
        &self.warnings
    }

    /// Forgets the collected load warnings.
    pub fn clear_warnings(&mut self) {
        self.warnings.clear();
    }

    /// Starts an asset and returns its voice.
    ///
    /// Loops start at their first sample (the intro, if any) and repeat
    /// until stopped. SFX containers pick a variation by their selection
    /// rule, add a random pitch and gain offset, and end with the variation.
    ///
    /// When every voice is busy, the oldest SFX voice is cut off and reused.
    /// Loops are never stolen; if only loops are playing this fails with
    /// [`RuntimeError::NoFreeVoice`] and no selection state changes.
    pub fn play(
        &mut self,
        asset: AssetHandle,
        params: PlayParams,
    ) -> Result<VoiceId, RuntimeError> {
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
        let slot = find_slot(&self.voices).ok_or(RuntimeError::NoFreeVoice)?;

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

        let semitones = (finite_or_zero(params.pitch_semitones) + pitch_offset)
            .clamp(-MAX_PITCH_SEMITONES, MAX_PITCH_SEMITONES);
        let rate =
            2f64.powf(f64::from(semitones) / 12.0) * f64::from(audio.sample_rate) / output_rate;
        let (pan_left, pan_right) = pan_gains(finite_or_zero(params.pan), audio.right.is_some());
        let fade_frames = ms_to_frames(params.fade_in_ms, self.sample_rate);
        let mut fade = Ramp::at(if fade_frames > 0 { 0.0 } else { 1.0 });
        fade.ramp_to(1.0, fade_frames);

        let started = self.next_start;
        self.next_start += 1;
        let voice = &mut self.voices[slot];
        voice.generation = voice.generation.wrapping_add(1);
        voice.state = VoiceState::Playing;
        voice.is_loop = region.is_some();
        voice.started = started;
        voice.audio = Some(audio);
        voice.region = region;
        voice.pos = 0.0;
        voice.rate = rate;
        voice.wrapped = false;
        voice.gain = Ramp::at(db_to_amp(finite_or_zero(params.gain_db) + gain_offset));
        voice.fade = fade;
        voice.random_gain_db = gain_offset;
        voice.pan_left = pan_left;
        voice.pan_right = pan_right;

        Ok(VoiceId {
            slot: slot as u32,
            generation: voice.generation,
        })
    }

    /// Starts the asset with this name. See [`play`](Self::play).
    pub fn play_by_name(
        &mut self,
        name: &str,
        params: PlayParams,
    ) -> Result<VoiceId, RuntimeError> {
        let handle = self
            .asset(name)
            .ok_or_else(|| RuntimeError::UnknownAsset(name.to_string()))?;
        self.play(handle, params)
    }

    /// Fades a voice out linearly over `fade_out_ms` and then frees it.
    /// A fade of 0 stops it at once. Stale ids are ignored.
    pub fn stop(&mut self, voice: VoiceId, fade_out_ms: f32) {
        let frames = ms_to_frames(fade_out_ms, self.sample_rate);
        if let Some(v) = self.voice_mut(voice) {
            stop_voice(v, frames);
        }
    }

    /// Stops every voice. See [`stop`](Self::stop).
    pub fn stop_all(&mut self, fade_out_ms: f32) {
        let frames = ms_to_frames(fade_out_ms, self.sample_rate);
        for v in self.voices.iter_mut().filter(|v| v.is_active()) {
            stop_voice(v, frames);
        }
    }

    /// Changes a voice's gain with a linear ramp. For SFX the random gain
    /// offset picked at trigger time is kept. Stale ids are ignored.
    pub fn set_voice_gain(&mut self, voice: VoiceId, gain_db: f32, ramp_ms: f32) {
        let frames = ms_to_frames(ramp_ms, self.sample_rate);
        if let Some(v) = self.voice_mut(voice) {
            let amp = db_to_amp(finite_or_zero(gain_db) + v.random_gain_db);
            v.gain.ramp_to(amp, frames);
        }
    }

    /// Sets the gain applied to the whole mix, ramped over 10 ms.
    pub fn set_master_gain(&mut self, gain_db: f32) {
        let frames = ms_to_frames(MASTER_RAMP_MS, self.sample_rate);
        self.master
            .ramp_to(db_to_amp(finite_or_zero(gain_db)), frames);
    }

    /// Whether the voice is still sounding, including while it fades out.
    pub fn is_playing(&self, voice: VoiceId) -> bool {
        self.voices
            .get(voice.slot as usize)
            .is_some_and(|v| v.generation == voice.generation && v.is_active())
    }

    /// The number of voices currently sounding.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.is_active()).count()
    }

    /// Reseeds the random generator and resets every SFX container's
    /// selection history, so the same calls afterwards give the same output.
    pub fn set_seed(&mut self, seed: u64) {
        self.rng = Rng::new(seed);
        for asset in &mut self.assets {
            if let LoadedKind::Sfx { selector, .. } = &mut asset.kind {
                selector.reset();
            }
        }
    }

    /// Mixes every voice into `out`, interleaved stereo at
    /// [`sample_rate`](Self::sample_rate), overwriting it.
    ///
    /// Call it from the audio callback: it never allocates, frees, locks or
    /// panics. `out` should have an even length; a trailing odd sample is
    /// set to zero. The mix is not clipped or limited.
    pub fn render(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        let even = out.len() & !1;
        let (out, _) = out.split_at_mut(even);
        for voice in self.voices.iter_mut().filter(|v| v.is_active()) {
            voice.render_add(out);
        }
        if self.master.is_settled() && self.master.value == 1.0 {
            return;
        }
        for frame in out.as_chunks_mut::<2>().0 {
            let g = self.master.next();
            frame[0] *= g;
            frame[1] *= g;
        }
    }

    fn voice_mut(&mut self, voice: VoiceId) -> Option<&mut Voice> {
        self.voices
            .get_mut(voice.slot as usize)
            .filter(|v| v.generation == voice.generation && v.is_active())
    }
}

/// A free slot, or else the oldest SFX voice to steal.
fn find_slot(voices: &[Voice]) -> Option<usize> {
    if let Some(free) = voices.iter().position(|v| !v.is_active()) {
        return Some(free);
    }
    voices
        .iter()
        .enumerate()
        .filter(|(_, v)| !v.is_loop)
        .min_by_key(|(_, v)| v.started)
        .map(|(i, _)| i)
}

fn stop_voice(voice: &mut Voice, frames: u32) {
    if frames == 0 {
        voice.state = VoiceState::Free;
    } else {
        voice.state = VoiceState::Stopping;
        voice.fade.ramp_to(0.0, frames);
    }
}

fn load_file(
    cache: &mut HashMap<PathBuf, Arc<AudioData>>,
    base_dir: &Path,
    file: &AudioFile,
    asset: &str,
    warnings: &mut Vec<LoadWarning>,
) -> Result<Arc<AudioData>, RuntimeError> {
    let path = resolve(base_dir, &file.path);
    let audio = match cache.get(&path) {
        Some(audio) => Arc::clone(audio),
        None => {
            let audio = Arc::new(decode_wav(&path)?);
            cache.insert(path.clone(), Arc::clone(&audio));
            audio
        }
    };

    let mut warn = |message: String| {
        warnings.push(LoadWarning {
            asset: asset.to_string(),
            path: Some(path.clone()),
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
    Ok(audio)
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

fn sanitize_range(range: RandomRange) -> RandomRange {
    RandomRange::new(finite_or_zero(range.min), finite_or_zero(range.max))
}

fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

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
    fn resolves_forward_slash_paths() {
        let p = resolve(Path::new("export"), "sfx/./hit_01.wav");
        assert_eq!(p, Path::new("export").join("sfx").join("hit_01.wav"));
    }
}
