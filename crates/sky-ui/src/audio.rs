// SPDX-License-Identifier: GPL-3.0-or-later
//! The audio device: start, restart after a change or a lost device, and
//! what to show about it.

use sky_io::device::{AudioStream, OutputConfig, OutputMode, probe_output, start_output};
use sky_session::Session;

/// Buffer sizes offered in the settings window; `None` is the device default.
pub const BUFFER_SIZES: [Option<u32>; 7] = [
    None,
    Some(64),
    Some(128),
    Some(256),
    Some(512),
    Some(1024),
    Some(2048),
];

/// Sample rates offered in the settings window; `None` is the device default.
pub const SAMPLE_RATES: [Option<u32>; 4] = [None, Some(44_100), Some(48_000), Some(96_000)];

#[derive(Default)]
pub struct AudioState {
    pub config: OutputConfig,
    /// Kept alive for as long as audio should play; dropping it stops sound.
    pub stream: Option<AudioStream>,
    /// Why there is no stream, when there is none.
    pub error: Option<String>,
    /// A note about the running stream, such as a buffer size that was refused.
    pub note: Option<String>,
}

impl AudioState {
    pub fn new(config: OutputConfig) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }

    /// Opens the configured device and connects a fresh engine to it. On
    /// failure the session runs without an engine and `error` says why.
    pub fn start(&mut self, session: &Session) {
        // Stop the old stream first so the device is free.
        self.stream = None;
        self.note = None;
        match open(session, &self.config) {
            Ok(stream) => {
                tracing::info!(
                    "audio: {} at {} Hz",
                    stream.info.name,
                    stream.info.sample_rate
                );
                self.note = stream.note.clone();
                self.stream = Some(stream);
                self.error = None;
            }
            Err(first)
                if self.config.buffer_frames.is_some()
                    || !matches!(self.config.mode, OutputMode::Auto | OutputMode::Shared) =>
            {
                // Some devices refuse fixed buffer sizes or a mode; try the
                // default in shared mode.
                let mut fallback = self.config.clone();
                fallback.buffer_frames = None;
                fallback.mode = OutputMode::Shared;
                match open(session, &fallback) {
                    Ok(stream) => {
                        self.note = Some(format!(
                            "the device refused {}, so it uses its default in shared mode ({first})",
                            match self.config.buffer_frames {
                                Some(b) =>
                                    format!("a {b}-frame buffer in {} mode", self.config.mode),
                                None => format!("{} mode", self.config.mode),
                            }
                        ));
                        self.stream = Some(stream);
                        self.error = None;
                    }
                    Err(e) => self.fail(session, e),
                }
            }
            Err(e) => self.fail(session, e),
        }
    }

    fn fail(&mut self, session: &Session, error: String) {
        tracing::warn!("no audio output: {error}");
        session.detach_engine();
        self.error = Some(error);
    }

    /// Stops audio at shutdown.
    pub fn stop(&mut self, session: &Session) {
        self.stream = None;
        session.detach_engine();
    }

    pub fn is_lost(&self) -> bool {
        self.stream.as_ref().is_some_and(AudioStream::is_lost)
    }

    /// One line for the status bar.
    pub fn describe(&self) -> String {
        match &self.stream {
            Some(s) => {
                let buffer = s
                    .info
                    .period_frames
                    .or(s.info.buffer_frames)
                    .map_or_else(|| "default buffer".to_string(), |b| format!("{b} frames"));
                let mode = match s.info.mode {
                    Some(OutputMode::LowLatency) => " · low-latency",
                    Some(OutputMode::Exclusive) => " · exclusive",
                    _ => "",
                };
                format!(
                    "{} · {} Hz · {buffer}{mode}",
                    s.info.name, s.info.sample_rate
                )
            }
            None => match &self.error {
                Some(e) => format!("No audio: {e}"),
                None => "No audio device".to_string(),
            },
        }
    }
}

fn open(session: &Session, config: &OutputConfig) -> Result<AudioStream, String> {
    let info = probe_output(config).map_err(|e| e.to_string())?;
    let engine = session.attach_engine(info.sample_rate);
    start_output(config, engine).map_err(|e| e.to_string())
}

pub fn buffer_label(b: Option<u32>) -> String {
    b.map_or_else(|| "Device default".to_string(), |b| format!("{b} frames"))
}

pub fn mode_label(m: OutputMode) -> &'static str {
    match m {
        OutputMode::Auto => "Automatic",
        OutputMode::Shared => "Shared",
        OutputMode::LowLatency => "Low-latency shared",
        OutputMode::Exclusive => "Exclusive",
    }
}

pub fn mode_tip(m: OutputMode) -> &'static str {
    match m {
        OutputMode::Auto => {
            "Shared mode, or a lower-latency mode when shared mode cannot run the buffer size"
        }
        OutputMode::Shared => "Through the Windows mixer; other programs keep playing",
        OutputMode::LowLatency => {
            "Through the Windows mixer at a shorter period, where the driver allows it"
        }
        OutputMode::Exclusive => {
            "This app alone uses the device: the smallest buffers, but other programs go silent"
        }
    }
}

pub fn rate_label(r: Option<u32>) -> String {
    r.map_or_else(|| "Device default".to_string(), |r| format!("{r} Hz"))
}
