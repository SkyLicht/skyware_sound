// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared building blocks for Skyware Sound.
//!
//! This crate is permissively licensed (MIT OR Apache-2.0) because the
//! `sky-runtime` playback library, which ships inside games, depends on it.
//! It must never depend on GPL or AGPL code.

pub mod error;
pub mod ids;
pub mod manifest;
pub mod music;
pub mod time;

pub use error::CoreError;
pub use ids::*;
pub use time::{PPQ, SampleTime, TempoMap, TempoPoint, Ticks, TimeSignature};
