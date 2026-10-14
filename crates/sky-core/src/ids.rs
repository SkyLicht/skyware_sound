// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stable object IDs.
//!
//! Every object in a project has a UUID v7 that never changes. Undo, MCP
//! references and the file format all rely on it.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// A new, time-ordered ID.
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// An ID built from a fixed number, for tests and fixtures.
            pub const fn from_u128(value: u128) -> Self {
                Self(Uuid::from_u128(value))
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s.trim()).map(Self)
            }
        }
    };
}

define_id!(
    /// A project.
    ProjectId
);
define_id!(
    /// A track.
    TrackId
);
define_id!(
    /// A clip on a track.
    ClipId
);
define_id!(
    /// A note inside a MIDI clip.
    NoteId
);
define_id!(
    /// An effect in a channel's insert chain.
    EffectId
);
define_id!(
    /// A file in the project's audio pool.
    AudioId
);
define_id!(
    /// A timeline marker.
    MarkerId
);
define_id!(
    /// An exported game asset.
    AssetId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_through_strings_and_json() {
        let id = TrackId::new();
        let parsed: TrackId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(serde_json::from_str::<TrackId>(&json).unwrap(), id);
    }

    #[test]
    fn new_ids_are_unique_and_time_ordered() {
        let a = ClipId::new();
        let b = ClipId::new();
        assert_ne!(a, b);
        assert!(a < b);
    }
}
