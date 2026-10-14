// SPDX-License-Identifier: MIT OR Apache-2.0
//! Errors shared across crates.

use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum CoreError {
    #[error(
        "invalid note name {0:?}: expected a letter A-G, optional # or b, and an octave such as C4 or F#3"
    )]
    InvalidNoteName(String),
    #[error("invalid duration {0:?}: expected a fraction of a whole note such as 1/4, 3/8 or 1")]
    InvalidDuration(String),
    #[error("tempo must be between 10 and 999 BPM, got {0}")]
    InvalidTempo(f64),
    #[error(
        "invalid time signature {0}/{1}: the denominator must be 1, 2, 4, 8, 16 or 32 and the numerator 1 to 32"
    )]
    InvalidTimeSignature(u32, u32),
    #[error("unsupported manifest schema version {found}; this build reads up to {supported}")]
    UnsupportedManifestVersion { found: u32, supported: u32 },
    #[error("manifest is not valid JSON: {0}")]
    ManifestJson(String),
}
