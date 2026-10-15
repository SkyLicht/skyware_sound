// SPDX-License-Identifier: GPL-3.0-or-later
//! Skyware Sound: the app, its headless MCP server and command-line tools.

mod autosave;
mod cli;
mod config;
mod crash;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::Parser;
use sky_mcp::{Permission, Policy, SkywareMcp};
use sky_model::templates::demo_project;
use sky_session::{GameExportOptions, LoopExportOptions, Session};

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
            print_licenses(&session.license_report(), "Credit");
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
                out.seam
                    .note
                    .map(|n| format!(". Note: {n}"))
                    .unwrap_or_default()
            );
            print_licenses(&out.licenses, "Credit in the manifest");
            Ok(())
        }
        Some(Cmd::ExportGame { project, dir, only }) => {
            if project.is_none() && dir.is_none() {
                bail!("give a project folder, or --dir to export the demo project's game assets");
            }
            let session = open_session(project.as_deref(), None)?;
            let out = session.export_game(&GameExportOptions { dir, only })?;
            for a in &out.assets {
                let files: Vec<_> = a.asset.files().map(|f| f.path.as_str()).collect();
                let seam = a
                    .seam
                    .as_ref()
                    .map(|s| {
                        format!(
                            ", seam: {}{}",
                            s.verdict,
                            s.note
                                .as_ref()
                                .map(|n| format!(". Note: {n}"))
                                .unwrap_or_default()
                        )
                    })
                    .unwrap_or_default();
                let kind = match a.asset {
                    sky_core::manifest::Asset::Loop(_) => "Loop",
                    sky_core::manifest::Asset::Sfx(_) => "SFX",
                };
                println!("{kind} {}: {}{seam}", a.asset.name(), files.join(", "));
            }
            for name in &out.removed {
                println!("Removed {name} from the manifest");
            }
            for file in &out.deleted {
                println!("Deleted {}", file.display());
            }
            println!("Manifest {}", out.manifest.display());
            print_licenses(&out.licenses, "Credit in the manifest");
            Ok(())
        }
        Some(Cmd::PlayGame {
            dir,
            assets,
            seconds,
            every,
            device,
            buffer,
            sample_rate,
            report,
        }) => {
            let opts = sky_check::GameOptions {
                dir,
                assets,
                seconds: seconds.max(0.0),
                every_ms: every,
                output: sky_io::device::OutputConfig {
                    device,
                    sample_rate,
                    buffer_frames: buffer,
                    silent: report.silent,
                    ..Default::default()
                },
            };
            if opts.seconds > 0.0 {
                eprintln!("Playing for {:.0} s...", opts.seconds);
            }
            let check = sky_check::check_game(&opts).map_err(anyhow::Error::msg)?;
            if report.json {
                println!("{}", serde_json::to_string_pretty(&check)?);
            } else {
                for line in check.lines() {
                    println!("{line}");
                }
            }
            if check.passed {
                Ok(())
            } else {
                bail!("the check failed")
            }
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
            report,
        }) => system_check(&CheckPlan {
            output,
            minutes: minutes.max(1.0 / 60.0),
            bars: None,
            project,
            out: None,
            silent: report.silent,
            json: report.json,
        }),
        Some(Cmd::Check {
            mut output,
            minutes,
            bars,
            project,
            out,
            report,
        }) => {
            // The release gate is 128 frames.
            output.buffer = output.buffer.or(Some(128));
            system_check(&CheckPlan {
                output,
                minutes,
                bars: (bars > 0).then_some(bars),
                project,
                out,
                silent: report.silent,
                json: report.json,
            })
        }
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
    autosave::spawn(session.clone());
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
        mode: output.mode.into(),
        asio: output.asio,
        silent: false,
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
    autosave::spawn(session.clone());
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
                "audio: {} on {}{}, {} Hz, buffer {}",
                stream.info.name,
                stream.info.host,
                stream
                    .info
                    .mode
                    .map(|m| format!(" ({m} mode)"))
                    .unwrap_or_default(),
                stream.info.sample_rate,
                stream
                    .info
                    .period_frames
                    .or(stream.info.buffer_frames)
                    .map_or("default".to_string(), |b| b.to_string())
            );
            if let Some(note) = &stream.note {
                tracing::warn!("{note}");
            }
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
    autosave::spawn(session.clone());
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
        browser_store: config::browser_folders_file(),
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

/// Prints the credits an export's sounds ask for, and warnings about them.
fn print_licenses(report: &sky_model::LicenseReport, credit: &str) {
    for c in &report.credits {
        println!("{credit}: {c}");
    }
    for w in &report.warnings {
        eprintln!("warning: {w}");
    }
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

/// What `skyware check` and `skyware latency-test` run.
struct CheckPlan {
    output: OutputArgs,
    /// Minutes through the device; 0 skips the device check.
    minutes: f64,
    /// Bars of the generated loop to export; `None` skips the loop check.
    bars: Option<u32>,
    project: Option<std::path::PathBuf>,
    out: Option<std::path::PathBuf>,
    silent: bool,
    json: bool,
}

/// Plays through the device and counts dropouts, so a buffer size can be
/// checked on real hardware, then exports a loop and verifies it. Plays and
/// exports `plan.project`, or the generated check loop.
fn system_check(plan: &CheckPlan) -> anyhow::Result<()> {
    let bars = plan.bars.unwrap_or(16).clamp(1, sky_check::MAX_BARS);
    let session = match &plan.project {
        Some(dir) => open_session(Some(dir), None)?,
        None => Session::new(sky_check::check_project(bars), None).0,
    };
    let say = |line: String| {
        if !plan.json {
            println!("{line}");
        }
    };

    let device = (plan.minutes > 0.0).then(|| {
        let mut output = output_config(&plan.output);
        output.silent = plan.silent;
        let opts = sky_check::DeviceOptions {
            output,
            duration: Duration::from_secs_f64(plan.minutes * 60.0),
            ..Default::default()
        };
        say(format!(
            "Playing \"{}\" through the {} for {:.1} min at {}, {} mode{}...",
            session.project().name,
            plan.output
                .device
                .as_deref()
                .map_or("default output".to_string(), |d| format!("output {d:?}")),
            plan.minutes,
            opts.output
                .buffer_frames
                .map_or("the default buffer".to_string(), |b| format!("{b} frames")),
            opts.output.mode,
            if plan.silent { ", silently" } else { "" }
        ));
        sky_check::check_device(&session, &opts, &mut |p| {
            say(format!(
                "{:>5.0} s  callbacks {}  dropouts {}  overruns {}  load avg {:.1}% max {:.1}%",
                p.elapsed.as_secs_f64(),
                p.callbacks,
                p.dropouts,
                p.overruns,
                p.load_avg * 100.0,
                p.load_max * 100.0
            ));
        })
    });

    let loop_export = plan.bars.map(|_| {
        let name = match &plan.project {
            Some(_) => session.project().name.clone(),
            None => sky_check::LoopOptions::default().name,
        };
        say(format!("Exporting and checking the loop \"{name}\"..."));
        sky_check::check_loop(
            &session,
            &sky_check::LoopOptions {
                dir: plan.out.clone(),
                name,
                ..Default::default()
            },
        )
    });

    let report = sky_check::Report::new(device, loop_export);
    if plan.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for line in report.lines() {
            println!("{line}");
        }
    }
    if report.passed {
        Ok(())
    } else {
        bail!("the check failed")
    }
}
