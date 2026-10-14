// SPDX-License-Identifier: GPL-3.0-or-later
use sky_core::{AudioId, ClipId, CoreError, EffectId, MarkerId, NoteId, TrackId};
use thiserror::Error;

/// Why a command or file operation failed. Messages are written for people
/// and for AI clients, so they say what was wrong and what is allowed.
#[derive(Debug, Error, PartialEq)]
pub enum ModelError {
    #[error("no track with id {0}")]
    TrackNotFound(TrackId),
    #[error("no clip with id {0}")]
    ClipNotFound(ClipId),
    #[error("no note with id {0} in this clip")]
    NoteNotFound(NoteId),
    #[error("no effect with id {0} on this channel")]
    EffectNotFound(EffectId),
    #[error("no audio with id {0} in the project's audio pool")]
    AudioNotFound(AudioId),
    #[error("no marker with id {0}")]
    MarkerNotFound(MarkerId),
    #[error("an object with id {0} already exists")]
    DuplicateId(String),
    #[error("track {0} is an audio track and has no instrument")]
    NotAnInstrumentTrack(TrackId),
    #[error("clip {id} is a {actual} clip, but this needs a {expected} clip")]
    WrongClipType {
        id: ClipId,
        expected: &'static str,
        actual: &'static str,
    },
    #[error("{0}")]
    InvalidValue(String),
    #[error("invalid parameter: {0}")]
    InvalidParam(String),
    #[error("invalid pattern: {0}")]
    Pattern(String),
    #[error("invalid note list: {0}")]
    NoteList(String),
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("project file error: {0}")]
    File(String),
    #[error(
        "this project was saved by a newer version of Skyware Sound (schema {found}, this build reads up to {supported})"
    )]
    NewerSchema { found: u32, supported: u32 },
    #[error("nothing to {0}")]
    NothingTo(&'static str),
}

impl From<std::io::Error> for ModelError {
    fn from(e: std::io::Error) -> Self {
        ModelError::File(e.to_string())
    }
}
