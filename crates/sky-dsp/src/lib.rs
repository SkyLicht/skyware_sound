// SPDX-License-Identifier: GPL-3.0-or-later
//! Built-in instruments, effects and analysis.
//!
//! Every processor here follows the real-time rules: after construction,
//! `process` and `render` never allocate, lock or touch the disk. State that
//! must survive a graph rebuild (delay lines, voices, envelopes) moves from
//! the old instance to the new one with `adopt`, which swaps buffers rather
//! than copying or allocating.

pub mod analysis;
pub mod audio;
pub mod curves;
pub mod effects;
pub mod filters;
pub mod instruments;
pub mod loudness;
pub mod params;
pub mod presets;
pub mod util;

pub use audio::AudioData;
pub use effects::{Effect, EffectType};
pub use instruments::{Instrument, InstrumentAudio, InstrumentType, SUSTAIN_PEDAL};
pub use params::{ParamKind, ParamSpec};
pub use presets::Preset;

/// The largest block any processor is asked to handle at once. Callers split
/// longer device buffers into chunks of at most this size.
pub const MAX_BLOCK: usize = 512;
