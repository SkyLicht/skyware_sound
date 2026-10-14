// SPDX-License-Identifier: GPL-3.0-or-later
//! The system check: what a build has to show on real hardware before it
//! is fit for game audio work.
//!
//! - [`check_device`] plays a project through the audio device at a buffer
//!   size (128 frames by default) and counts everything that would be heard
//!   as a dropout, and whether the device really used that buffer size.
//! - [`check_loop`] exports a loop for the game (the 16-bar
//!   [`check_project`] by default) and verifies what landed on disk: its
//!   exact length, the manifest, the seam after quantizing, and that
//!   `sky-runtime` plays it across the loop point without a gap or click.
//!
//! `skyware check` runs both from the command line, and the `system_check`
//! MCP tool runs them for an assistant. Both report the same [`Report`].
//!
//! [`check_game`] does the same for a whole game export: it loads it into
//! `sky-runtime` the way a game does, checks every loop and SFX set, and
//! plays them through the device (`skyware play-game`).

mod device;
mod game;
mod loops;

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

pub use device::{DeviceCheck, DeviceOptions, DeviceProgress, check_device};
pub use game::{GameCheck, GameOptions, GamePlayback, check_game};
pub use loops::{
    Expected, LoopCheck, LoopOptions, MAX_BARS, check_loop, check_project, verify_loop_export,
};
pub use sky_io::device::{OutputConfig, OutputMode};

/// One verified property, with what was measured.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub passed: bool,
    pub detail: String,
}

impl Check {
    pub fn new(name: &'static str, passed: bool, detail: impl Into<String>) -> Self {
        Self {
            name,
            passed,
            detail: detail.into(),
        }
    }
}

/// Both halves of the system check. A half that was not run is `None`; a
/// half that could not run at all has an error instead.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_error: Option<String>,
    #[serde(rename = "loop", skip_serializing_if = "Option::is_none")]
    pub loop_export: Option<LoopCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_error: Option<String>,
}

impl Report {
    pub fn new(
        device: Option<Result<DeviceCheck, String>>,
        loop_export: Option<Result<LoopCheck, String>>,
    ) -> Self {
        let mut r = Report::default();
        match device {
            Some(Ok(d)) => r.device = Some(d),
            Some(Err(e)) => r.device_error = Some(e),
            None => {}
        }
        match loop_export {
            Some(Ok(l)) => r.loop_export = Some(l),
            Some(Err(e)) => r.loop_error = Some(e),
            None => {}
        }
        let ran = r.device.is_some() || r.loop_export.is_some();
        r.passed = ran
            && r.device_error.is_none()
            && r.loop_error.is_none()
            && r.device.as_ref().is_none_or(|d| d.passed)
            && r.loop_export.as_ref().is_none_or(|l| l.passed);
        r
    }

    /// The report as lines for a terminal.
    pub fn lines(&self) -> Vec<String> {
        let mark = |ok: bool| if ok { "PASS" } else { "FAIL" };
        let mut out = Vec::new();
        if let Some(d) = &self.device {
            out.push(format!(
                "{}  device: {} ({}{}), {} Hz, {:.1} s",
                mark(d.passed),
                d.device,
                d.host,
                d.mode
                    .as_deref()
                    .map(|m| format!(", {m} mode"))
                    .unwrap_or_default(),
                d.sample_rate,
                d.seconds
            ));
            if let Some(n) = &d.note {
                out.push(format!("      note: {n}"));
            }
            for c in &d.checks {
                out.push(format!("      {} {}: {}", mark(c.passed), c.name, c.detail));
            }
        }
        if let Some(e) = &self.device_error {
            out.push(format!("FAIL  device: {e}"));
        }
        if let Some(l) = &self.loop_export {
            out.push(format!(
                "{}  loop: \"{}\", {} bars, {:.3} s, {}{}",
                mark(l.passed),
                l.asset,
                l.bars,
                l.seconds,
                l.file.display(),
                if l.kept { "" } else { " (deleted)" }
            ));
            for c in &l.checks {
                out.push(format!("      {} {}: {}", mark(c.passed), c.name, c.detail));
            }
        }
        if let Some(e) = &self.loop_error {
            out.push(format!("FAIL  loop: {e}"));
        }
        out.push(if self.passed {
            "PASS: all checks passed".into()
        } else {
            "FAIL: see above".into()
        });
        out
    }
}

/// A folder in the system temp folder, deleted when dropped.
pub(crate) struct TempFolder(PathBuf);

impl TempFolder {
    pub(crate) fn new(what: &str) -> Result<Self, String> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "skyware-check-{what}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path)
            .map_err(|e| format!("cannot make {}: {e}", path.display()))?;
        Ok(Self(path))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFolder {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            tracing::warn!("cannot remove {}: {e}", self.0.display());
        }
    }
}
