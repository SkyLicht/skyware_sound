// SPDX-License-Identifier: GPL-3.0-or-later
//! The desktop interface for Skyware Sound, built with egui.
//!
//! The UI holds a [`Session`] like the MCP server does and changes the
//! project only through [`Session::apply`], so every edit made here is one
//! undo step and reaches the engine as a new snapshot. Each frame reads the
//! latest [`sky_session::ProjectView`] without locking, so changes an AI makes
//! show up on the next repaint.
//!
//! All drawing code takes an `&mut egui::Ui`, which lets the tests drive the
//! whole interface headless through [`egui::Context::run_ui`].

mod actions;
mod ai_panel;
mod app;
mod arrange;
mod audio;
mod audition;
mod dialogs;
mod game;
mod jobs;
mod mixer;
mod palette;
mod params;
mod pattern_editor;
mod piano_roll;
mod sounds;
mod theme;
mod tracks;
mod transport;
mod waveform;
mod widgets;

#[cfg(test)]
mod tests;

use sky_session::Session;

/// How an AI client reaches the MCP server, shown in the AI panel.
#[derive(Clone)]
pub struct McpInfo {
    pub url: String,
    pub token: String,
}

/// Start-up options for [`run`].
#[derive(Clone, Default)]
pub struct UiConfig {
    pub output: sky_io::device::OutputConfig,
    /// Shown in the AI panel with copy buttons.
    pub mcp: Option<McpInfo>,
    /// `--stress-ui`: replace the project with one that has this many tracks.
    pub demo_tracks: Option<usize>,
}

/// Opens the main window and runs until it is closed.
///
/// Audio starts on the configured output device. If no device works the UI
/// keeps running without sound and says so in the status bar.
pub fn run(session: Session, config: UiConfig) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(app::WINDOW_TITLE)
            .with_app_id("skyware-sound")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0]),
        renderer: eframe::Renderer::Glow,
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        app::WINDOW_TITLE,
        options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            let mut app = app::App::new(session, config, true);
            app.start_audio();
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("cannot open the Skyware Sound window: {e}"))
}
