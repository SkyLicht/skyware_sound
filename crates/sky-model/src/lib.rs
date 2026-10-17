// SPDX-License-Identifier: GPL-3.0-or-later
//! The project model.
//!
//! A [`Project`] is plain data that changes only through [`Command`]s.
//! Applying a command returns its inverse; a [`History`] groups commands into
//! transactions (one UI gesture or one MCP tool call) and gives undo and redo.
//! Projects live on disk as a `.skyproj` folder holding `project.json`.

pub mod command;
pub mod error;
pub mod file;
pub mod history;
pub mod licenses;
pub mod notes;
pub mod picture;
pub mod project;
pub mod steps;
pub mod summary;
pub mod templates;

pub use command::{ChannelRef, Command, Target};
pub use error::ModelError;
pub use history::{Applied, History, Origin, Transaction};
pub use licenses::{LicenseReport, license_report};
pub use picture::{Picture, PictureFit, PictureImage, Symbol};
pub use project::*;
pub use steps::{Condition, Step, StepClip, StepKind, StepLane};
