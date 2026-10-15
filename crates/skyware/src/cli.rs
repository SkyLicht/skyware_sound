// SPDX-License-Identifier: GPL-3.0-or-later
//! Command-line arguments.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "skyware",
    version,
    about = "Skyware Sound: a music and game-audio workstation that AI assistants can drive over MCP",
    long_about = "Skyware Sound: a music and game-audio workstation that AI assistants can drive over MCP.\n\n\
                  With no subcommand it opens the app, with the MCP server on http://127.0.0.1:47800/mcp."
)]
pub struct Cli {
    /// A .skyproj project folder to open. Without one, the demo project opens.
    pub project: Option<PathBuf>,
    /// Do not start the MCP server.
    #[arg(long)]
    pub no_mcp: bool,
    /// Open a project with this many tracks, to test UI speed.
    #[arg(long, value_name = "TRACKS")]
    pub stress_ui: Option<usize>,
    #[command(flatten)]
    pub mcp: McpArgs,
    #[command(flatten)]
    pub output: OutputArgs,
    #[command(subcommand)]
    pub command: Option<Cmd>,
}

#[derive(Args, Clone, Debug)]
pub struct McpArgs {
    /// Port of the local MCP HTTP server.
    #[arg(long = "mcp-port", default_value_t = sky_mcp::DEFAULT_PORT)]
    pub port: u16,
    /// What MCP clients may do: read, edit, or files (edit plus open, save, import and export).
    #[arg(long = "permission", default_value = "files")]
    pub permission: String,
    /// A folder MCP clients may read and write files in. Repeatable. The
    /// "Skyware Sound" folder in Documents and the project's folder are always allowed.
    #[arg(long = "allow-dir", value_name = "DIR")]
    pub allow_dir: Vec<PathBuf>,
}

#[derive(Args, Clone, Debug)]
pub struct OutputArgs {
    /// Output device (part of its name). Default: the system default.
    #[arg(long)]
    pub device: Option<String>,
    /// Buffer size in frames, such as 128.
    #[arg(long)]
    pub buffer: Option<u32>,
    /// Sample rate, such as 48000. Default: the device's.
    #[arg(long)]
    pub sample_rate: Option<u32>,
    /// Use ASIO instead of WASAPI (Windows builds with the asio feature).
    #[arg(long)]
    pub asio: bool,
    /// How to share the device with other programs (WASAPI). auto picks
    /// low-latency shared or exclusive mode when shared mode cannot run
    /// the --buffer asked for.
    #[arg(long, value_enum, default_value_t = Mode::Auto)]
    pub mode: Mode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Shared mode, or a lower-latency mode when --buffer needs one.
    Auto,
    /// Through the Windows mixer at its usual period (10 ms on most devices).
    Shared,
    /// Through the Windows mixer at a shorter period, where the driver allows it.
    LowLatency,
    /// Take the device for this app alone: the shortest buffers, but other
    /// programs go silent.
    Exclusive,
}

impl From<Mode> for sky_io::device::OutputMode {
    fn from(m: Mode) -> Self {
        match m {
            Mode::Auto => Self::Auto,
            Mode::Shared => Self::Shared,
            Mode::LowLatency => Self::LowLatency,
            Mode::Exclusive => Self::Exclusive,
        }
    }
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Serve MCP over stdin/stdout with its own headless project (for clients that start servers themselves).
    Mcp {
        /// A .skyproj project folder to open.
        project: Option<PathBuf>,
        #[command(flatten)]
        mcp: McpArgs,
    },
    /// Run headless: the MCP HTTP server and audio output, without a window.
    Serve {
        project: Option<PathBuf>,
        #[command(flatten)]
        mcp: McpArgs,
        #[command(flatten)]
        output: OutputArgs,
        /// Do not open an audio device.
        #[arg(long)]
        no_audio: bool,
    },
    /// Render a project (or some bars of it) to a WAV file.
    Render {
        project: PathBuf,
        #[arg(long, short)]
        out: PathBuf,
        #[arg(long)]
        start_bar: Option<f64>,
        #[arg(long)]
        end_bar: Option<f64>,
        /// Seconds rendered after the end so tails ring out.
        #[arg(long, default_value_t = 2.0)]
        tail: f64,
        /// 16, 24 or 32 (float).
        #[arg(long, default_value_t = 24)]
        bits: u16,
    },
    /// Export the loop region as a seamless game loop with a manifest for sky-runtime.
    ExportLoop {
        project: PathBuf,
        /// Asset name. Default: the project name.
        #[arg(long)]
        name: Option<String>,
        /// Export folder. Default: renders/export inside the project.
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        start_bar: Option<f64>,
        #[arg(long)]
        end_bar: Option<f64>,
        #[arg(long, default_value_t = 24)]
        bits: u16,
    },
    /// Export the project's game assets (loops and SFX sets) with a manifest
    /// for sky-runtime, to the project's game export folder.
    ExportGame {
        /// A .skyproj project folder. Without one, the demo project's game
        /// assets are exported (give --dir).
        project: Option<PathBuf>,
        /// Export folder. Default: the project's game export folder.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Export only this asset. Repeatable. Without it, every asset is
        /// exported and, for a saved project, assets it dropped are removed.
        #[arg(long, value_name = "NAME")]
        only: Vec<String>,
    },
    /// Load a game export into sky-runtime the way a game does, check every
    /// loop and SFX set, then play them through the audio device: loops
    /// throughout, SFX fired in turn from a game thread.
    PlayGame {
        /// The export folder, holding skyware-manifest.json.
        dir: PathBuf,
        /// Check and play only this asset. Repeatable.
        #[arg(long = "asset", value_name = "NAME")]
        assets: Vec<String>,
        /// Seconds to play through the device; 0 only checks offline.
        #[arg(long, default_value_t = 10.0)]
        seconds: f64,
        /// Milliseconds between SFX triggers.
        #[arg(long, default_value_t = 500)]
        every: u32,
        /// Output device (part of its name). Default: the system default.
        #[arg(long)]
        device: Option<String>,
        /// Buffer size in frames, such as 256.
        #[arg(long)]
        buffer: Option<u32>,
        /// Sample rate, such as 48000. Default: the device's.
        #[arg(long)]
        sample_rate: Option<u32>,
        #[command(flatten)]
        report: ReportArgs,
    },
    /// Print loudness, peaks and other measurements of an audio file as JSON.
    Analyze { file: PathBuf },
    /// List audio output devices.
    Devices,
    /// Print the MCP access token.
    Token {
        /// Make a new token; clients using the old one stop working.
        #[arg(long)]
        reset: bool,
    },
    /// Play the demo (or a project) through the device and count dropouts.
    LatencyTest {
        #[command(flatten)]
        output: OutputArgs,
        /// How long to play.
        #[arg(long, default_value_t = 10.0)]
        minutes: f64,
        #[arg(long)]
        project: Option<PathBuf>,
        #[command(flatten)]
        report: ReportArgs,
    },
    /// The system check: play through the audio device at a buffer size and
    /// count dropouts, then export a loop for the game and verify the export.
    Check {
        #[command(flatten)]
        output: OutputArgs,
        /// Minutes to play through the device; 0 skips the device check.
        #[arg(long, default_value_t = 0.5)]
        minutes: f64,
        /// Bars in the generated test loop; 0 skips the loop check.
        #[arg(long, default_value_t = 16)]
        bars: u32,
        /// Check this project instead of the generated loop: it plays, and
        /// its loop region is exported.
        #[arg(long)]
        project: Option<PathBuf>,
        /// Keep the exported loop in this folder. Default: a temporary
        /// folder that is deleted afterwards.
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
        #[command(flatten)]
        report: ReportArgs,
    },
}

#[derive(Args, Clone, Debug)]
pub struct ReportArgs {
    /// Run the engine but send silence to the device, for unattended runs.
    #[arg(long)]
    pub silent: bool,
    /// Print the report as JSON.
    #[arg(long)]
    pub json: bool,
}
