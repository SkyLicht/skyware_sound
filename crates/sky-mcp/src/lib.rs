// SPDX-License-Identifier: GPL-3.0-or-later
//! The MCP server. AI assistants use these tools to read and edit the open
//! project, render and analyze it, and export loops for games.
//!
//! The tools drive the same [`sky_session::Session`] as the UI, so a change
//! an assistant makes appears in the app at once and is one undo step.

mod policy;
mod tools;
mod transport;

pub use policy::{Permission, Policy};
pub use tools::SkywareMcp;
pub use transport::{HttpServer, serve_http, serve_stdio};

/// The default port of the HTTP server.
pub const DEFAULT_PORT: u16 = 47_800;
