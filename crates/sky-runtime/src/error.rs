// SPDX-License-Identifier: MIT OR Apache-2.0
//! Errors returned by the runtime.

use std::path::PathBuf;

use sky_core::CoreError;
use thiserror::Error;

use crate::AssetHandle;

/// Everything that can go wrong while loading or triggering assets.
///
/// [`Runtime::render`](crate::Runtime::render) never returns an error.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RuntimeError {
    /// A manifest or audio file could not be opened or read.
    #[error("could not read {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The manifest is not valid JSON, does not match the schema, or was
    /// written by a newer version of Skyware Sound.
    #[error("invalid manifest: {0}")]
    Manifest(#[from] CoreError),
    /// An audio file is not a WAV file this runtime can decode.
    #[error("could not decode WAV file {}: {message}", path.display())]
    Wav { path: PathBuf, message: String },
    /// A loop asset cannot be played, for example because its file is empty.
    #[error("loop asset {name:?} cannot be played: {message}")]
    InvalidLoop { name: String, message: String },
    /// No loaded asset has this name.
    #[error("no asset named {0:?} is loaded")]
    UnknownAsset(String),
    /// The handle does not belong to an asset of this runtime.
    #[error("{0:?} does not refer to an asset loaded in this runtime")]
    InvalidHandle(AssetHandle),
    /// Every voice is busy with a loop, so nothing could be stolen.
    #[error("no free voice: every voice is playing a loop")]
    NoFreeVoice,
    /// The SFX asset has no variations to pick from.
    #[error("SFX asset {0:?} has no variations")]
    EmptySfx(String),
}
