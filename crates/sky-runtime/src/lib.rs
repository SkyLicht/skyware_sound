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
//! # Real-time safety
//!
//! [`Runtime::render`] never allocates, frees memory, locks or panics, so it
//! is safe to call from an audio callback. All memory (decoded audio, the
//! voice pool, shuffle tables) is allocated by
//! [`Runtime::with_max_voices`] and the load functions.
//!
//! The runtime is [`Send`] but holds no locks itself. If game code and the
//! audio callback run on different threads, wrap it in your own mutex or
//! drive it from the audio thread through a message queue.
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
//! - The random generator starts from a fixed seed, so a runtime given the
//!   same calls always renders the same output. Use [`Runtime::set_seed`]
//!   for a different (but still reproducible) sequence.

mod decode;
mod error;
mod rng;
mod runtime;
mod select;
mod voice;

pub use error::RuntimeError;
pub use runtime::{
    AssetHandle, DEFAULT_MAX_VOICES, DEFAULT_SEED, LoadWarning, PlayParams, Runtime, VoiceId,
};
/// The manifest types this crate reads, re-exported from `sky-core`.
pub use sky_core::manifest;

// The runtime must be movable to the audio thread.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<Runtime>();
};
