// SPDX-License-Identifier: MIT OR Apache-2.0
//! The single-threaded runtime: loaded assets, the voice pool and the mixer
//! in one object.

use std::fmt;
use std::path::Path;

use sky_core::manifest::{Asset, Category, Manifest};

use crate::RuntimeError;
use crate::control::{AssetHandle, Control, DEFAULT_MAX_VOICES, LoadWarning, PlayParams, VoiceId};
use crate::mixer::{MixCommand, Mixer};
use crate::threaded::{Controller, Renderer, split};

/// Plays Skyware Sound exports: seamless loops and randomized SFX containers.
///
/// Create one per output stream, load one or more manifests, then call
/// [`play`](Self::play) from game code and [`render`](Self::render) from the
/// audio callback. When game code and the audio callback run on different
/// threads, [`split`](Self::split) the runtime into a [`Controller`] for the
/// game and a [`Renderer`] for the audio callback.
pub struct Runtime {
    control: Control,
    mixer: Mixer,
}

impl fmt::Debug for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runtime")
            .field("sample_rate", &self.control.sample_rate())
            .field("assets", &self.control.asset_count())
            .field("max_voices", &self.mixer.max_voices())
            .field("active_voices", &self.mixer.active_voices())
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
            control: Control::new(sample_rate, max_voices),
            mixer: Mixer::new(max_voices),
        }
    }

    /// Splits the runtime into a [`Controller`], for game code, and a
    /// [`Renderer`], for the audio callback, which talk through lock-free
    /// queues. Loaded assets, voices and settings carry over.
    ///
    /// The controller keeps the whole API except `render`; the renderer only
    /// renders. Up to `command_capacity` calls (plays, stops, gain changes)
    /// can wait for the next render; calls beyond that are held on the
    /// controller's side and sent with later ones, so none are lost.
    pub fn split(mut self, command_capacity: usize) -> (Controller, Renderer) {
        // Voices that ended since the last call are not reported by the
        // renderer, so free their slots now.
        self.sync();
        split(self.control, self.mixer, command_capacity)
    }

    /// The output sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.control.sample_rate()
    }

    /// The size of the voice pool.
    pub fn max_voices(&self) -> usize {
        self.mixer.max_voices()
    }

    /// Loads a manifest and every WAV file it references.
    ///
    /// `path` is either an export folder containing `skyware-manifest.json`
    /// or the path of a manifest file. Audio paths are relative to the
    /// manifest. Returns a handle for every asset, in manifest order.
    ///
    /// Loading is all-or-nothing: if any file fails, nothing is added. An
    /// asset whose name is already loaded is replaced and keeps its handle;
    /// voices already playing it finish with the old audio. Loading again
    /// after Skyware Sound exported again is how a game picks up changes.
    pub fn load_manifest(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<Vec<AssetHandle>, RuntimeError> {
        let handles = self.control.load_manifest(path.as_ref())?;
        self.mixer.release_finished();
        Ok(handles)
    }

    /// Like [`load_manifest`](Self::load_manifest), from an already parsed
    /// manifest whose audio paths are relative to `base_dir`.
    pub fn load_manifest_from(
        &mut self,
        manifest: &Manifest,
        base_dir: &Path,
    ) -> Result<Vec<AssetHandle>, RuntimeError> {
        let handles = self.control.load_manifest_from(manifest, base_dir)?;
        self.mixer.release_finished();
        Ok(handles)
    }

    /// Like [`load_manifest`](Self::load_manifest), for games that keep
    /// their files in a pack or archive: `manifest_json` is the manifest's
    /// text, and `read` returns the bytes of the audio file at a path from
    /// the manifest, such as `sfx/footstep_01.wav`.
    pub fn load_manifest_with(
        &mut self,
        manifest_json: &str,
        mut read: impl FnMut(&str) -> std::io::Result<Vec<u8>>,
    ) -> Result<Vec<AssetHandle>, RuntimeError> {
        let handles = self.control.load_manifest_with(manifest_json, &mut read)?;
        self.mixer.release_finished();
        Ok(handles)
    }

    /// The handle of the asset with this name.
    pub fn asset(&self, name: &str) -> Option<AssetHandle> {
        self.control.asset(name)
    }

    /// The manifest entry of a loaded asset.
    pub fn asset_info(&self, handle: AssetHandle) -> Option<&Asset> {
        self.control.asset_info(handle)
    }

    /// Warnings collected by every load so far.
    pub fn warnings(&self) -> &[LoadWarning] {
        self.control.warnings()
    }

    /// Forgets the collected load warnings.
    pub fn clear_warnings(&mut self) {
        self.control.clear_warnings();
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
        self.sync();
        let (voice, cmd) = self.control.play(asset, params)?;
        self.mixer.apply(cmd);
        Ok(voice)
    }

    /// Starts the asset with this name. See [`play`](Self::play).
    pub fn play_by_name(
        &mut self,
        name: &str,
        params: PlayParams,
    ) -> Result<VoiceId, RuntimeError> {
        self.sync();
        let (voice, cmd) = self.control.play_by_name(name, params)?;
        self.mixer.apply(cmd);
        Ok(voice)
    }

    /// Fades a voice out linearly over `fade_out_ms` and then frees it.
    /// A fade of 0 stops it at once. Stale ids are ignored.
    pub fn stop(&mut self, voice: VoiceId, fade_out_ms: f32) {
        self.sync();
        let cmd = self.control.stop(voice, fade_out_ms);
        self.send(cmd);
    }

    /// Stops every voice. See [`stop`](Self::stop).
    pub fn stop_all(&mut self, fade_out_ms: f32) {
        let cmd = self.control.stop_all(fade_out_ms);
        self.mixer.apply(cmd);
    }

    /// Changes a voice's gain with a linear ramp. For SFX the random gain
    /// offset picked at trigger time is kept. Stale ids are ignored.
    pub fn set_voice_gain(&mut self, voice: VoiceId, gain_db: f32, ramp_ms: f32) {
        self.sync();
        let cmd = self.control.set_voice_gain(voice, gain_db, ramp_ms);
        self.send(cmd);
    }

    /// Moves a voice to `pan`, from -1 (left) to 1 (right), with a linear
    /// ramp, for sounds that follow something on screen. Stale ids are
    /// ignored.
    pub fn set_voice_pan(&mut self, voice: VoiceId, pan: f32, ramp_ms: f32) {
        self.sync();
        let cmd = self.control.set_voice_pan(voice, pan, ramp_ms);
        self.send(cmd);
    }

    /// Changes a voice's pitch shift, in semitones, at once (for Doppler or
    /// engine sounds). For SFX the random pitch offset picked at trigger
    /// time is kept. Stale ids are ignored.
    pub fn set_voice_pitch(&mut self, voice: VoiceId, semitones: f32) {
        self.sync();
        let cmd = self.control.set_voice_pitch(voice, semitones);
        self.send(cmd);
    }

    /// Sets the gain applied to the whole mix, ramped over 10 ms.
    pub fn set_master_gain(&mut self, gain_db: f32) {
        let cmd = self.control.set_master_gain(gain_db);
        self.mixer.apply(cmd);
    }

    /// Sets the gain of every asset in a category (the manifest's
    /// `category`), with a linear ramp: a game's music and effects volume
    /// settings, or ducking music under dialogue.
    pub fn set_category_gain(&mut self, category: Category, gain_db: f32, ramp_ms: f32) {
        let cmd = self.control.set_category_gain(category, gain_db, ramp_ms);
        self.mixer.apply(cmd);
    }

    /// Whether the voice is still sounding, including while it fades out.
    pub fn is_playing(&self, voice: VoiceId) -> bool {
        self.mixer.is_playing(voice.slot, voice.generation)
    }

    /// The number of voices currently sounding.
    pub fn active_voices(&self) -> usize {
        self.mixer.active_voices()
    }

    /// Reseeds the random generator and resets every SFX container's
    /// selection history, so the same calls afterwards give the same output.
    pub fn set_seed(&mut self, seed: u64) {
        self.control.set_seed(seed);
    }

    /// Mixes every voice into `out`, interleaved stereo at
    /// [`sample_rate`](Self::sample_rate), overwriting it.
    ///
    /// Call it from the audio callback: it never allocates, frees, locks or
    /// panics. `out` should have an even length; a trailing odd sample is
    /// set to zero. The mix is not clipped or limited.
    pub fn render(&mut self, out: &mut [f32]) {
        self.mixer.render(out);
    }

    /// Brings the slot bookkeeping up to date with the voices that ended.
    fn sync(&mut self) {
        let mixer = &self.mixer;
        self.control.sync(|slot| mixer.slot_active(slot));
    }

    fn send(&mut self, cmd: Option<MixCommand>) {
        if let Some(cmd) = cmd {
            self.mixer.apply(cmd);
        }
    }
}
