// SPDX-License-Identifier: GPL-3.0-or-later
//! Skyware Sound: the app, its headless MCP server and command-line tools.

mod cli;
mod config;
mod crash;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use clap::Parser;
use sky_mcp::{Permission, Policy, SkywareMcp};
use sky_model::templates::demo_project;
use sky_session::{LoopExportOptions, Session};

use crate::cli::{Cli, Cmd, McpArgs, OutputArgs};

// In debug builds the audio callback runs under `assert_no_alloc`, which
// needs this allocator to catch allocations on the audio thread.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    init_logging();
    crash::install_panic_hook();
    match run(cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // Logs go to stderr so stdout stays free for the stdio MCP protocol.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        None => run_app(
            cli.project.as_deref(),
            cli.mcp,
            cli.output,
            cli.no_mcp,
            cli.stress_ui,
        ),
        Some(Cmd::Mcp { project, mcp }) => run_mcp_stdio(project.as_deref(), &mcp),
        Some(Cmd::Serve {
            project,
            mcp,
            output,
            no_audio,
        }) => run_headless(project.as_deref(), &mcp, &output, no_audio),
        Some(Cmd::Render {
            project,
            out,
            start_bar,
            end_bar,
            tail,
            bits,
        }) => {
            let session = open_session(Some(&project), None)?;
            let p = session.project();
            let range = bar_range(&p, start_bar, end_bar)?;
            let format = wav_format(bits)?;
            let audio = session.export_wav(&out, range, tail, format)?;
            println!(
                "Wrote {} ({:.2} s)",
                out.display(),
                audio.duration_seconds()
            );
            Ok(())
        }
        Some(Cmd::ExportLoop {
            project,
            name,
            dir,
            start_bar,
            end_bar,
            bits,
        }) => {
            let session = open_session(Some(&project), None)?;
            let p = session.project();
            let out = session.export_loop(&LoopExportOptions {
                name,
                dir,
                range: bar_range(&p, start_bar, end_bar)?,
                format: wav_format(bits)?,
                ..Default::default()
            })?;
            println!("Wrote {}", out.exported.file.display());
            println!("Manifest {}", out.exported.manifest.display());
            println!(
                "{} bars, seam: {}{}",
                out.bars,
                out.seam.verdict,
                out.seam.note.map(|n| format!(" ({n})")).unwrap_or_default()
            );
            Ok(())
        }
        Some(Cmd::Analyze { file }) => {
            let audio = sky_io::read_audio(&file)?;
            let report = sky_dsp::analysis::analyze(&audio);
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Some(Cmd::Devices) => {
            let devices = sky_io::device::list_output_devices();
            if devices.is_empty() {
                println!("No output devices found.");
            }
            for d in devices {
                println!("{d}");
            }
            Ok(())
        }
        Some(Cmd::Token { reset }) => {
            let token = if reset {
                config::reset_token()?
            } else {
                config::token()?
            };
            println!("{token}");
            eprintln!(
                "Connect an MCP client to http://127.0.0.1:{}/mcp with the header \
                 \"Authorization: Bearer {token}\".",
                sky_mcp::DEFAULT_PORT
            );
            Ok(())
        }
        Some(Cmd::LatencyTest {
            output,
            minutes,
            project,
        }) => latency_test(&output, minutes, project.as_deref()),
    }
}

/// Opens a project folder, or starts from the demo project.
fn open_session(project: Option<&Path>, engine_rate: Option<u32>) -> anyhow::Result<Session> {
    let (session, _) = Session::new(demo_project(), engine_rate);
    if let Some(dir) = project {
        let replayed = session
            .open(dir)
            .with_context(|| format!("cannot open {}", dir.display()))?;
        if replayed > 0 {
            tracing::warn!("recovered {replayed} unsaved change(s) from the crash journal");
        }
    }
    Ok(session)
}

fn policy(mcp: &McpArgs, project: Option<&Path>) -> anyhow::Result<Policy> {
    let permission = Permission::from_name(&mcp.permission).with_context(|| {
        format!(
            "--permission must be read, edit or files, not {:?}",
            mcp.permission
        )
    })?;
    let mut roots = mcp.allow_dir.clone();
    if let Some(docs) = config::documents_dir() {
        roots.push(docs);
    }
    if let Some(parent) = project.and_then(Path::parent) {
        roots.push(parent.to_path_buf());
    }
    Ok(Policy::new(permission, roots))
}

fn run_mcp_stdio(project: Option<&Path>, mcp: &McpArgs) -> anyhow::Result<()> {
    let session = open_session(project, None)?;
    let server = SkywareMcp::new(session, policy(mcp, project)?);
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(sky_mcp::serve_stdio(server))
}

fn mcp_addr(mcp: &McpArgs) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, mcp.port))
}

fn output_config(output: &OutputArgs) -> sky_io::device::OutputConfig {
    sky_io::device::OutputConfig {
        device: output.device.clone(),
        sample_rate: output.sample_rate,
        buffer_frames: output.buffer,
        asio: output.asio,
    }
}

/// The MCP HTTP server and audio output without a window.
fn run_headless(
    project: Option<&Path>,
    mcp: &McpArgs,
    output: &OutputArgs,
    no_audio: bool,
) -> anyhow::Result<()> {
    let session = open_session(project, None)?;
    let _stream = if no_audio {
        None
    } else {
        start_audio(&session, &output_config(output))
    };
    let policy = policy(mcp, project)?;
    let token = config::token()?;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let s = session.clone();
        let server = sky_mcp::serve_http(
            move || SkywareMcp::new(s.clone(), policy.clone()),
            mcp_addr(mcp),
            token,
        )
        .await?;
        eprintln!(
            "Skyware Sound MCP server at {} (Ctrl+C to stop)",
            server.url()
        );
        eprintln!("Run `skyware token` for the access token.");
        tokio::signal::ctrl_c().await?;
        server.shutdown();
        anyhow::Ok(())
    })
}

fn start_audio(
    session: &Session,
    cfg: &sky_io::device::OutputConfig,
) -> Option<sky_io::device::AudioStream> {
    let info = match sky_io::device::probe_output(cfg) {
        Ok(info) => info,
        Err(e) => {
            tracing::warn!("no audio output: {e}");
            return None;
        }
    };
    let engine = session.attach_engine(info.sample_rate);
    match sky_io::device::start_output(cfg, engine) {
        Ok(stream) => {
            tracing::info!(
                "audio: {} on {}, {} Hz, buffer {}",
                stream.info.name,
                stream.info.host,
                stream.info.sample_rate,
                stream
                    .info
                    .buffer_frames
                    .map_or("default".to_string(), |b| b.to_string())
            );
            Some(stream)
        }
        Err(e) => {
            tracing::warn!("cannot start audio: {e}");
            session.detach_engine();
            None
        }
    }
}

#[cfg(feature = "gui")]
fn run_app(
    project: Option<&Path>,
    mcp: McpArgs,
    output: OutputArgs,
    no_mcp: bool,
    stress_ui: Option<usize>,
) -> anyhow::Result<()> {
    let session = open_session(project, None)?;
    // The MCP server runs on a background runtime for as long as the window is open.
    let rt = tokio::runtime::Runtime::new()?;
    let mut server = None;
    if !no_mcp {
        let policy = policy(&mcp, project)?;
        let token = config::token()?;
        let s = session.clone();
        match rt.block_on(sky_mcp::serve_http(
            move || SkywareMcp::new(s.clone(), policy.clone()),
            mcp_addr(&mcp),
            token.clone(),
        )) {
            Ok(srv) => {
                let info = sky_ui::McpInfo {
                    url: srv.url(),
                    token,
                };
                server = Some((srv, info));
            }
            Err(e) => tracing::warn!("MCP server not started: {e:#}"),
        }
    }
    let config = sky_ui::UiConfig {
        output: output_config(&output),
        mcp: server.as_ref().map(|(_, info)| sky_ui::McpInfo {
            url: info.url.clone(),
            token: info.token.clone(),
        }),
        demo_tracks: stress_ui,
    };
    let result = sky_ui::run(session, config);
    if let Some((srv, _)) = server {
        srv.shutdown();
    }
    rt.shutdown_timeout(Duration::from_secs(2));
    result
}

#[cfg(not(feature = "gui"))]
fn run_app(
    _project: Option<&Path>,
    _mcp: McpArgs,
    _output: OutputArgs,
    _no_mcp: bool,
    _stress_ui: Option<usize>,
) -> anyhow::Result<()> {
    bail!("this build has no window; use a subcommand such as `skyware serve` (see --help)")
}

fn bar_range(
    project: &sky_model::Project,
    start: Option<f64>,
    end: Option<f64>,
) -> anyhow::Result<Option<(i64, i64)>> {
    match (start, end) {
        (None, None) => Ok(None),
        (Some(s), Some(e)) if s >= 1.0 && e > s => {
            Ok(Some((project.bar_to_ticks(s), project.bar_to_ticks(e))))
        }
        (Some(_), Some(_)) => bail!("bars start at 1 and --end-bar must be after --start-bar"),
        _ => bail!("give both --start-bar and --end-bar, or neither"),
    }
}

fn wav_format(bits: u16) -> anyhow::Result<sky_io::WavFormat> {
    sky_io::WavFormat::from_bits(bits).context("--bits must be 16, 24 or 32")
}

/// Plays a project through the device for a while and counts dropouts, so
/// the buffer size can be checked on real hardware.
fn latency_test(output: &OutputArgs, minutes: f64, project: Option<&Path>) -> anyhow::Result<()> {
    let session = open_session(project, None)?;
    let cfg = output_config(output);
    let Some(stream) = start_audio(&session, &cfg) else {
        bail!("no audio output could be started");
    };
    let shared = session
        .engine_shared()
        .context("the engine did not start")?;
    let sr = stream.info.sample_rate as f64;
    if let Some(frames) = stream.info.buffer_frames {
        println!(
            "Device {} ({}), {} Hz, buffer {frames} frames = {:.2} ms",
            stream.info.name,
            stream.info.host,
            stream.info.sample_rate,
            frames as f64 / sr * 1000.0
        );
    } else {
        println!(
            "Device {} ({}), {} Hz, default buffer",
            stream.info.name, stream.info.host, stream.info.sample_rate
        );
    }
    session.play()?;
    let total = Duration::from_secs_f64((minutes * 60.0).max(1.0));
    let start = Instant::now();
    let mut max_load = 0.0f32;
    let mut sum_load = 0.0f64;
    let mut samples = 0u64;
    let mut last_report = Instant::now();
    use std::sync::atomic::Ordering::Relaxed;
    while start.elapsed() < total {
        std::thread::sleep(Duration::from_millis(50));
        let load = shared.load();
        max_load = max_load.max(load);
        sum_load += load as f64;
        samples += 1;
        if stream.is_lost() {
            bail!("the device went away during the test");
        }
        if last_report.elapsed() >= Duration::from_secs(10) {
            last_report = Instant::now();
            println!(
                "{:>5.0} s  dropouts {}  overruns {}  load avg {:.0}% max {:.0}%",
                start.elapsed().as_secs_f64(),
                shared.dropouts.load(Relaxed),
                shared.overruns.load(Relaxed),
                sum_load / samples.max(1) as f64 * 100.0,
                max_load * 100.0
            );
        }
    }
    session.stop()?;
    let dropouts = shared.dropouts.load(Relaxed);
    let overruns = shared.overruns.load(Relaxed);
    println!(
        "Done: {:.1} min, {} callbacks, dropouts {dropouts}, overruns {overruns}, load avg {:.0}% max {:.0}%",
        start.elapsed().as_secs_f64() / 60.0,
        shared.callbacks.load(Relaxed),
        sum_load / samples.max(1) as f64 * 100.0,
        max_load * 100.0
    );
    if dropouts == 0 && overruns == 0 {
        println!("PASS: no dropouts");
        Ok(())
    } else {
        bail!("FAIL: {dropouts} dropout(s), {overruns} overrun(s)")
    }
}
