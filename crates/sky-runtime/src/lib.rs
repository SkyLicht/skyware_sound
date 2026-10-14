// SPDX-License-Identifier: MIT OR Apache-2.0
//! Plays Skyware Sound exports inside games.
//!
//! Skyware Sound exports WAV files plus a `skyware-manifest.json` that
//! describes them: **loops**, which play an optional intro and then repeat
//! their body sample-exactly forever, and **SFX containers**, sets of
//! variations of which one is picked per trigger, with a random pitch and
//! gain offset. This crate loads that manifest and mixes the assets into an
//! interleaved stereo `f32` buffer that your engine sends to the speakers.
//!
//! It is licensed MIT OR Apache-2.0 (unlike the GPL editor) so any game can
//! embed it, and it depends only on permissively licensed crates.
//!
//! # Usage
//!
//! ```no_run
//! use sky_runtime::{PlayParams, Runtime};
//!
//! # fn main() -> Result<(), sky_runtime::RuntimeError> {
//! let mut runtime = Runtime::new(48_000);
//! // The folder Skyware Sound exported to, holding skyware-manifest.json.
//! runtime.load_manifest("assets/audio")?;
//!
//! // Game code: start the music and trigger a footstep.
//! let music = runtime.play_by_name("battle_loop", PlayParams::default())?;
//! runtime.play_by_name(
//!     "footstep",
//!     PlayParams { gain_db: -3.0, pan: -0.4, ..PlayParams::default() },
//! )?;
//!
//! // Audio callback: fill the device buffer (interleaved left, right).
//! let mut buffer = vec![0.0f32; 2 * 512];
//! runtime.render(&mut buffer);
//!
//! // Later: fade the music out over two seconds.
//! runtime.stop(music, 2_000.0);
//! # Ok(())
//! # }
//! ```
//!
//! # Game thread and audio thread
//!
//! Most engines run game code on one thread and the audio callback on
//! another. [`Runtime::split`] turns a runtime into a [`Controller`] for the
//! game thread and a [`Renderer`] for the audio callback, connected by
//! lock-free queues: the controller loads, plays, stops and changes voices
//! without ever waiting for the audio thread, and the renderer applies
//! those changes at the start of its next block. Audio that voices no
//! longer need goes back to the controller, so it is freed on the game
//! thread. A split runtime renders exactly the samples the same calls on
//! one [`Runtime`] would.
//!
//! ```rust,ignore
//! // With cpal; any audio API that asks for f32 samples works the same way.
//! let mut runtime = sky_runtime::Runtime::new(sample_rate);
//! runtime.load_manifest("assets/audio")?;
//! let (mut audio, mut renderer) = runtime.split(256);
//!
//! let stream = device.build_output_stream(
//!     stream_config, // two channels, at `sample_rate`
//!     move |out: &mut [f32], _| renderer.render(out),
//!     |err| eprintln!("audio: {err}"),
//!     None,
//! )?;
//! stream.play()?;
//!
//! // Game thread:
//! let music = audio.play_by_name("battle_loop", PlayParams::default())?;
//! audio.set_category_gain(Category::Music, -6.0, 500.0); // the music slider
//! // Once a frame, so finished voices are freed promptly:
//! audio.update();
//! ```
//!
//! # Real-time safety
//!
//! [`Runtime::render`] and [`Renderer::render`] never allocate, free
//! memory, lock or panic, so they are safe to call from an audio callback.
//! All memory (decoded audio, the voice pool, shuffle tables, the queues)
//! is allocated by [`Runtime::with_max_voices`], [`Runtime::split`] and the
//! load functions.
//!
//! [`Runtime`], [`Controller`] and [`Renderer`] are [`Send`] and hold no
//! locks.
//!
//! # Loading
//!
//! [`Runtime::load_manifest`] reads an export folder from disk.
//! [`Runtime::load_manifest_with`] takes the manifest's text and a function
//! that returns each audio file's bytes, for games that keep their assets
//! in a pack or archive. Loading again after Skyware Sound exported again
//! replaces the assets in place and keeps their handles.
//!
//! # Playback details
//!
//! - Audio files are decoded to `f32` once at load time (8, 16, 24 and
//!   32-bit integer or 32-bit float WAV; only the first two channels of
//!   multichannel files are used). A file whose sample rate differs from
//!   the output rate is resampled while playing with 4-point Hermite
//!   interpolation. At the same rate and pitch, playback is bit-exact.
//! - Pitch shifts by resampling: the playback rate is
//!   `2^(semitones / 12) * file_rate / output_rate`, clamped to four octaves
//!   either way.
//! - Pan is equal-power: mono sources are -3 dB per side at center; stereo
//!   sources use a balance control that is unity on both sides at center.
//! - Fade-ins, fade-outs and gain changes are linear ramps; a fade of 0 ms is
//!   immediate. A stopped voice frees itself when its fade reaches zero.
//! - When all voices are busy, `play` steals the oldest SFX voice. Loops are
//!   never stolen.
//! - Every asset has a [`Category`] from the manifest (music, sfx, ui,
//!   voice, ambience); [`Runtime::set_category_gain`] sets a category's
//!   volume with a ramp, for a game's volume settings or ducking.
//! - [`Runtime::set_voice_pan`] and [`Runtime::set_voice_pitch`] move and
//!   bend a playing voice, for sounds that follow something on screen.
//! - The random generator starts from a fixed seed, so a runtime given the
//!   same calls always renders the same output. Use [`Runtime::set_seed`]
//!   for a different (but still reproducible) sequence.

mod control;
mod decode;
mod error;
mod mixer;
mod rng;
mod runtime;
mod select;
mod threaded;
mod voice;

pub use control::{
    AssetHandle, DEFAULT_MAX_VOICES, DEFAULT_SEED, LoadWarning, PlayParams, VoiceId,
};
pub use error::RuntimeError;
pub use runtime::Runtime;
/// The manifest types this crate reads, re-exported from `sky-core`.
pub use sky_core::manifest;
pub use sky_core::manifest::Category;
pub use threaded::{Controller, Renderer};

// The runtime, and each half of a split one, must be movable to the thread
// that uses it.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<Runtime>();
    assert_send::<Controller>();
    assert_send::<Renderer>();
};
