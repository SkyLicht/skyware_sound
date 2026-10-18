// SPDX-License-Identifier: GPL-3.0-or-later
//! The audio engine.
//!
//! The control thread compiles the project into an immutable
//! [`EngineSnapshot`] and sends it to the audio thread over a lock-free
//! queue. The audio thread swaps it in at a block boundary, handing running
//! state (voices, delay lines) from the old snapshot to the new one, and
//! returns the old snapshot through a second queue so it is freed elsewhere.
//! Meters and the playhead flow back through atomics.
//!
//! Auditioning goes through the same queue: keys played live on a track's
//! instrument, and a preview player that mixes audio in after the master
//! channel, both whether or not the transport runs.
//!
//! The offline renderer runs the very same [`Engine`] without a device, so
//! what you hear is what you export.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]

pub mod compile;
pub mod engine;
pub mod offline;
pub mod preview;
pub mod shared;

pub use compile::{AudioCache, LibraryCache, compile, compile_with};
pub use engine::{Engine, EngineHandle, EngineMsg, create_engine};
pub use offline::{render, render_intro_loop, render_loop, render_one_shot};
pub use preview::Preview;
pub use shared::{
    EngineShared, KeyTap, LoudnessBlock, LoudnessTap, MAX_METERS, REDUCTION_SLOTS, Reduction,
};

pub use sky_dsp::MAX_BLOCK;

/// Sets flush-to-zero and denormals-are-zero on the calling thread, so
/// decaying filters and reverbs never fall into slow denormal arithmetic.
pub fn enable_flush_to_zero() {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: setting the FTZ (bit 15) and DAZ (bit 6) flags only changes
    // how this thread rounds tiny floats; no memory is touched.
    #[allow(deprecated)]
    unsafe {
        use std::arch::x86_64::{_mm_getcsr, _mm_setcsr};
        _mm_setcsr(_mm_getcsr() | 0x8040);
    }
}
