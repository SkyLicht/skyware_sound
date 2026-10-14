// SPDX-License-Identifier: MIT OR Apache-2.0
//! The runtime split across two threads: a [`Controller`] that game code
//! calls and a [`Renderer`] that the audio callback calls, joined by two
//! lock-free queues. Calls become commands the renderer applies at the start
//! of its next block; ended voices and the audio they held come back the
//! other way, so memory is only ever freed on the controller's thread.

use std::collections::VecDeque;
use std::fmt;
use std::path::Path;

use sky_core::manifest::{Asset, Category, Manifest};

use crate::RuntimeError;
use crate::control::{AssetHandle, Control, LoadWarning, PlayParams, VoiceId};
use crate::mixer::{Ended, MixCommand, Mixer};

/// The smallest command queue [`Runtime::split`](crate::Runtime::split) makes.
const MIN_COMMANDS: usize = 16;

pub(crate) fn split(
    control: Control,
    mixer: Mixer,
    command_capacity: usize,
) -> (Controller, Renderer) {
    let command_capacity = command_capacity.max(MIN_COMMANDS);
    // Each command starts at most one voice, and each start and each voice
    // reports back at most once, so the return queue never fills up while
    // the controller keeps draining it.
    let ended_capacity = 2 * (command_capacity + 1) + mixer.max_voices();
    let (command_tx, command_rx) = rtrb::RingBuffer::new(command_capacity);
    let (ended_tx, ended_rx) = rtrb::RingBuffer::new(ended_capacity);
    let sample_rate = control.sample_rate();
    let max_voices = mixer.max_voices();
    (
        Controller {
            control,
            commands: command_tx,
            waiting: VecDeque::new(),
            ended: ended_rx,
            max_voices,
        },
        Renderer {
            mixer: mixer.with_returns(ended_tx),
            commands: command_rx,
            sample_rate,
        },
    )
}

/// The game side of a split runtime: loads assets and starts, stops and
/// changes voices. Every call is cheap and never waits for the audio
/// thread; changes take effect at the start of the renderer's next block.
///
/// Created by [`Runtime::split`](crate::Runtime::split). Voice state
/// ([`is_playing`](Self::is_playing), [`active_voices`](Self::active_voices))
/// is what the renderer last reported.
pub struct Controller {
    control: Control,
    commands: rtrb::Producer<MixCommand>,
    /// Commands that did not fit in the queue yet, oldest first.
    waiting: VecDeque<MixCommand>,
    ended: rtrb::Consumer<Ended>,
    max_voices: usize,
}

impl fmt::Debug for Controller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Controller")
            .field("sample_rate", &self.control.sample_rate())
            .field("assets", &self.control.asset_count())
            .field("max_voices", &self.max_voices)
            .field("waiting_commands", &self.waiting.len())
            .finish_non_exhaustive()
    }
}

impl Controller {
    /// The output sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.control.sample_rate()
    }

    /// The size of the voice pool.
    pub fn max_voices(&self) -> usize {
        self.max_voices
    }

    /// Picks up what the renderer reported (ended voices, audio to free) and
    /// sends commands that were waiting for room in the queue. Every other
    /// call does this too; call it once a frame when nothing else happens,
    /// so finished voices are freed promptly.
    pub fn update(&mut self) {
        while let Ok(ended) = self.ended.pop() {
            self.control.ended(ended.slot, ended.generation);
            // The audio, if this held the last reference, is freed here.
            drop(ended.audio);
        }
        while let Some(cmd) = self.waiting.pop_front() {
            if let Err(rtrb::PushError::Full(cmd)) = self.commands.push(cmd) {
                self.waiting.push_front(cmd);
                break;
            }
        }
    }

    fn send(&mut self, cmd: MixCommand) {
        if !self.waiting.is_empty() {
            self.waiting.push_back(cmd);
            return;
        }
        if let Err(rtrb::PushError::Full(cmd)) = self.commands.push(cmd) {
            self.waiting.push_back(cmd);
        }
    }

    /// Loads a manifest and its audio on this thread. See
    /// [`Runtime::load_manifest`](crate::Runtime::load_manifest).
    pub fn load_manifest(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<Vec<AssetHandle>, RuntimeError> {
        self.update();
        self.control.load_manifest(path.as_ref())
    }

    /// See [`Runtime::load_manifest_from`](crate::Runtime::load_manifest_from).
    pub fn load_manifest_from(
        &mut self,
        manifest: &Manifest,
        base_dir: &Path,
    ) -> Result<Vec<AssetHandle>, RuntimeError> {
        self.update();
        self.control.load_manifest_from(manifest, base_dir)
    }

    /// See [`Runtime::load_manifest_with`](crate::Runtime::load_manifest_with).
    pub fn load_manifest_with(
        &mut self,
        manifest_json: &str,
        mut read: impl FnMut(&str) -> std::io::Result<Vec<u8>>,
    ) -> Result<Vec<AssetHandle>, RuntimeError> {
        self.update();
        self.control.load_manifest_with(manifest_json, &mut read)
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

    /// Starts an asset. See [`Runtime::play`](crate::Runtime::play).
    pub fn play(
        &mut self,
        asset: AssetHandle,
        params: PlayParams,
    ) -> Result<VoiceId, RuntimeError> {
        self.update();
        let (voice, cmd) = self.control.play(asset, params)?;
        self.send(cmd);
        Ok(voice)
    }

    /// Starts the asset with this name. See [`Runtime::play`](crate::Runtime::play).
    pub fn play_by_name(
        &mut self,
        name: &str,
        params: PlayParams,
    ) -> Result<VoiceId, RuntimeError> {
        self.update();
        let (voice, cmd) = self.control.play_by_name(name, params)?;
        self.send(cmd);
        Ok(voice)
    }

    /// See [`Runtime::stop`](crate::Runtime::stop).
    pub fn stop(&mut self, voice: VoiceId, fade_out_ms: f32) {
        self.update();
        if let Some(cmd) = self.control.stop(voice, fade_out_ms) {
            self.send(cmd);
        }
    }

    /// See [`Runtime::stop_all`](crate::Runtime::stop_all).
    pub fn stop_all(&mut self, fade_out_ms: f32) {
        self.update();
        let cmd = self.control.stop_all(fade_out_ms);
        self.send(cmd);
    }

    /// See [`Runtime::set_voice_gain`](crate::Runtime::set_voice_gain).
    pub fn set_voice_gain(&mut self, voice: VoiceId, gain_db: f32, ramp_ms: f32) {
        self.update();
        if let Some(cmd) = self.control.set_voice_gain(voice, gain_db, ramp_ms) {
            self.send(cmd);
        }
    }

    /// See [`Runtime::set_voice_pan`](crate::Runtime::set_voice_pan).
    pub fn set_voice_pan(&mut self, voice: VoiceId, pan: f32, ramp_ms: f32) {
        self.update();
        if let Some(cmd) = self.control.set_voice_pan(voice, pan, ramp_ms) {
            self.send(cmd);
        }
    }

    /// See [`Runtime::set_voice_pitch`](crate::Runtime::set_voice_pitch).
    pub fn set_voice_pitch(&mut self, voice: VoiceId, semitones: f32) {
        self.update();
        if let Some(cmd) = self.control.set_voice_pitch(voice, semitones) {
            self.send(cmd);
        }
    }

    /// See [`Runtime::set_master_gain`](crate::Runtime::set_master_gain).
    pub fn set_master_gain(&mut self, gain_db: f32) {
        self.update();
        let cmd = self.control.set_master_gain(gain_db);
        self.send(cmd);
    }

    /// See [`Runtime::set_category_gain`](crate::Runtime::set_category_gain).
    pub fn set_category_gain(&mut self, category: Category, gain_db: f32, ramp_ms: f32) {
        self.update();
        let cmd = self.control.set_category_gain(category, gain_db, ramp_ms);
        self.send(cmd);
    }

    /// Whether the voice is still sounding, as of the renderer's last block.
    pub fn is_playing(&mut self, voice: VoiceId) -> bool {
        self.update();
        self.control.is_playing(voice)
    }

    /// The number of voices sounding, as of the renderer's last block.
    pub fn active_voices(&mut self) -> usize {
        self.update();
        self.control.active_voices()
    }

    /// See [`Runtime::set_seed`](crate::Runtime::set_seed).
    pub fn set_seed(&mut self, seed: u64) {
        self.control.set_seed(seed);
    }
}

/// The audio side of a split runtime. Call [`render`](Self::render) from
/// the audio callback; it never allocates, frees, locks or panics.
///
/// Drop it off the audio thread (for example by closing the audio stream
/// from the game thread): dropping it frees the voices' audio.
pub struct Renderer {
    mixer: Mixer,
    commands: rtrb::Consumer<MixCommand>,
    sample_rate: u32,
}

impl fmt::Debug for Renderer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Renderer")
            .field("sample_rate", &self.sample_rate)
            .field("max_voices", &self.mixer.max_voices())
            .finish_non_exhaustive()
    }
}

impl Renderer {
    /// The output sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Applies the controller's calls since the last block, then mixes
    /// every voice into `out`, interleaved stereo, overwriting it. See
    /// [`Runtime::render`](crate::Runtime::render).
    pub fn render(&mut self, out: &mut [f32]) {
        while let Ok(cmd) = self.commands.pop() {
            self.mixer.apply(cmd);
        }
        self.mixer.render(out);
    }
}
