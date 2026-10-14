// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio files and audio devices.

pub mod codec;
#[cfg(feature = "device")]
pub mod device;

pub use codec::{IoError, WavFormat, read_audio, write_wav};
