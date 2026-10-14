// SPDX-License-Identifier: GPL-3.0-or-later
//! The game check: load a game export the way a game does, through
//! `sky-runtime` split into a game-thread [`Controller`] and an audio-thread
//! [`Renderer`]; verify that every loop repeats exactly and every SFX set
//! plays and ends; then play them through the audio device, with the
//! renderer in the device callback and SFX fired from the game thread.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{ErrorKind, FromSample, SampleFormat, SizedSample};
use serde::Serialize;
use sky_core::manifest::{Asset, LoopAsset, Selection, SfxAsset};
use sky_io::device::{OutputConfig, StreamStats, cpal_output};
use sky_runtime::{Controller, PlayParams, Renderer, Runtime, VoiceId};

use crate::Check;

/// Options for [`check_game`].
#[derive(Clone, Debug)]
pub struct GameOptions {
    /// The export folder, holding `skyware-manifest.json`.
    pub dir: PathBuf,
    /// Names of the assets to check and play; empty means all of them.
    pub assets: Vec<String>,
    /// Seconds to play through the device; 0 skips the device.
    pub seconds: f64,
    /// Milliseconds between SFX triggers on the device.
    pub every_ms: u32,
    pub output: OutputConfig,
}

impl Default for GameOptions {
    fn default() -> Self {
        Self {
            dir: PathBuf::new(),
            assets: Vec::new(),
            seconds: 10.0,
            every_ms: 500,
            output: OutputConfig::default(),
        }
    }
}

/// The result of [`check_game`].
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameCheck {
    pub passed: bool,
    pub dir: PathBuf,
    pub loops: usize,
    pub sfx: usize,
    /// Loading, then one check per asset.
    pub checks: Vec<Check>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<GamePlayback>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_error: Option<String>,
}

/// What playing the export through the device showed.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GamePlayback {
    pub passed: bool,
    pub host: String,
    pub device: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_format: String,
    pub seconds: f64,
    pub loops: Vec<String>,
    pub sfx_triggers: u64,
    pub callbacks: u64,
    pub callback_frames_max: u32,
    /// Errors the host reported plus hand-offs after the device ran dry.
    pub dropouts: u64,
    /// The longest render as a share of the time its buffer lasts.
    pub load_max: f32,
    /// True when the device got silence while the runtime ran.
    pub silent: bool,
    pub checks: Vec<Check>,
}

impl GameCheck {
    /// The report as lines for a terminal.
    pub fn lines(&self) -> Vec<String> {
        let mark = |ok: bool| if ok { "PASS" } else { "FAIL" };
        let offline = self.checks.iter().all(|c| c.passed);
        let mut out = vec![format!(
            "{}  game export: {} loop(s) and {} SFX set(s) in {}",
            mark(offline),
            self.loops,
            self.sfx,
            self.dir.display()
        )];
        for c in &self.checks {
            out.push(format!("      {} {}: {}", mark(c.passed), c.name, c.detail));
        }
        if let Some(d) = &self.device {
            out.push(format!(
                "{}  device: {} ({}), {} Hz, {:.1} s{}",
                mark(d.passed),
                d.device,
                d.host,
                d.sample_rate,
                d.seconds,
                if d.silent { ", silent" } else { "" }
            ));
            for c in &d.checks {
                out.push(format!("      {} {}: {}", mark(c.passed), c.name, c.detail));
            }
        }
        if let Some(e) = &self.device_error {
            out.push(format!("FAIL  device: {e}"));
        }
        out.push(if self.passed {
            "PASS: all checks passed".into()
        } else {
            "FAIL: see above".into()
        });
        out
    }
}

/// Loads the export in `opts.dir` into `sky-runtime`, checks every loop
/// and SFX set offline, then plays them through the device for
/// `opts.seconds`. Fails only when the manifest cannot be read or names
/// an asset that is not in it; everything else is a failed check.
pub fn check_game(opts: &GameOptions) -> Result<GameCheck, String> {
    let dir = &opts.dir;
    let manifest = sky_game::read_manifest(dir)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "{} was not found; export the game assets first",
                sky_game::manifest_path(dir).display()
            )
        })?;
    for name in &opts.assets {
        if manifest.find(name).is_none() {
            return Err(format!(
                "the manifest has no asset {name:?}; it has {}",
                names(&manifest.assets)
            ));
        }
    }
    let selected: Vec<&Asset> = manifest
        .assets
        .iter()
        .filter(|a| opts.assets.is_empty() || opts.assets.iter().any(|n| n == a.name()))
        .collect();
    if selected.is_empty() {
        return Err(format!(
            "{} lists no assets",
            sky_game::manifest_path(dir).display()
        ));
    }
    let loops = selected
        .iter()
        .filter(|a| matches!(a, Asset::Loop(_)))
        .count();
    let mut report = GameCheck {
        passed: false,
        dir: dir.clone(),
        loops,
        sfx: selected.len() - loops,
        checks: offline_checks(dir, &selected),
        device: None,
        device_error: None,
    };
    if opts.seconds > 0.0 {
        match play_on_device(opts, &selected) {
            Ok(d) => report.device = Some(d),
            Err(e) => report.device_error = Some(e),
        }
    }
    report.passed = report.checks.iter().all(|c| c.passed)
        && report.device_error.is_none()
        && report.device.as_ref().is_none_or(|d| d.passed);
    Ok(report)
}

fn names(assets: &[Asset]) -> String {
    assets
        .iter()
        .map(Asset::name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Loads the export at the rate of its first file and runs one check per
/// asset, all through a split runtime driven from this thread.
fn offline_checks(dir: &Path, selected: &[&Asset]) -> Vec<Check> {
    let rate = selected
        .iter()
        .flat_map(|a| a.files())
        .map(|f| f.sample_rate)
        .next()
        .unwrap_or(48_000);
    let mut rt = Runtime::new(rate);
    let handles = match rt.load_manifest(dir) {
        Ok(h) => h,
        Err(e) => {
            return vec![Check::new(
                "load",
                false,
                format!("sky-runtime cannot load the export: {e}"),
            )];
        }
    };
    let warnings: Vec<String> = rt.warnings().iter().map(|w| w.to_string()).collect();
    let mut checks = vec![Check::new(
        "load",
        true,
        format!(
            "sky-runtime loaded {} asset(s) at {rate} Hz{}",
            handles.len(),
            if warnings.is_empty() {
                String::new()
            } else {
                format!("; warnings: {}", warnings.join("; "))
            }
        ),
    )];
    let (mut ctl, mut ren) = rt.split(64);
    for asset in selected {
        checks.push(match asset {
            Asset::Loop(a) => check_loop_asset(&mut ctl, &mut ren, dir, a),
            Asset::Sfx(a) => check_sfx_asset(&mut ctl, &mut ren, a),
        });
    }
    checks
}

/// Frames rendered per call, as a device would ask for.
const BLOCK: usize = 512;

/// Plays a loop past its loop point and compares the output with the file:
/// the intro once, then the body repeated. At the file's own rate, unity
/// gain and center pan the runtime reproduces it sample for sample, so any
/// gap, overlap or click at the wrap shows up as a difference.
fn check_loop_asset(
    ctl: &mut Controller,
    ren: &mut Renderer,
    dir: &Path,
    asset: &LoopAsset,
) -> Check {
    let name = &asset.name;
    let fail = |detail: String| Check::new("loop", false, format!("{name}: {detail}"));
    let audio = match sky_io::read_audio(&dir.join(&asset.file.path)) {
        Ok(a) => a,
        Err(e) => return fail(e.to_string()),
    };
    let frames = audio.frames();
    let (start, end) = (asset.loop_start as usize, asset.loop_end as usize);
    if end <= start || end > frames {
        return fail(format!(
            "loop points {start}..{end} do not fit the file's {frames} frames"
        ));
    }
    let voice = match ctl.play_by_name(name, PlayParams::default()) {
        Ok(v) => v,
        Err(e) => return fail(format!("sky-runtime cannot play it: {e}")),
    };
    let sr = ren.sample_rate() as usize;
    let exact = audio.sample_rate == ren.sample_rate();
    let body = end - start;
    // Through the loop point and up to five seconds of the repeat.
    let total = end + body.min(5 * sr);
    let (left, right) = (audio.left(), audio.right());
    // sky-runtime plays mono files centered with an equal-power pan.
    let center = if audio.channel_count() == 1 {
        std::f32::consts::FRAC_1_SQRT_2
    } else {
        1.0
    };
    let mut buf = vec![0.0f32; BLOCK * 2];
    let (mut worst, mut worst_at, mut peak) = (0.0f32, 0usize, 0.0f32);
    let mut done = 0;
    while done < total {
        let n = BLOCK.min(total - done);
        ctl.update();
        ren.render(&mut buf[..n * 2]);
        for (k, frame) in buf[..n * 2].as_chunks::<2>().0.iter().enumerate() {
            let i = done + k;
            let j = if i < end {
                i
            } else {
                start + (i - start) % body
            };
            peak = peak.max(frame[0].abs()).max(frame[1].abs());
            let d = (frame[0] - left[j] * center)
                .abs()
                .max((frame[1] - right[j] * center).abs());
            if d > worst {
                worst = d;
                worst_at = i;
            }
        }
        done += n;
    }
    let playing = ctl.is_playing(voice);
    stop(ctl, ren, voice);
    if !playing {
        return fail("the voice stopped instead of looping".into());
    }
    let secs = |f: usize| f as f64 / sr as f64;
    let intro = if start > 0 {
        format!("a {:.2} s intro, then ", secs(start))
    } else {
        String::new()
    };
    if !exact {
        // Resampled: only check that it keeps sounding.
        return Check::new(
            "loop",
            peak > 1e-4,
            format!(
                "{name}: {intro}a {:.2} s body; the file is at {} Hz, so it was resampled \
                 to {sr} Hz and kept playing past the loop point",
                secs(body),
                audio.sample_rate
            ),
        );
    }
    // The runtime mixes in f32 at unity gain (or the center pan for mono);
    // anything above rounding noise is a real difference.
    let passed = worst <= 1e-5;
    Check::new(
        "loop",
        passed,
        if passed {
            format!(
                "{name}: {intro}a {:.2} s body; sky-runtime played {:.2} s through the loop \
                 point identical to the file (largest difference {worst:e})",
                secs(body),
                secs(total)
            )
        } else {
            format!(
                "{name}: sky-runtime output differs from the file by {worst} at {:.4} s \
                 (the loop point is at {:.4} s)",
                secs(worst_at),
                secs(end)
            )
        },
    )
}

/// Triggers an SFX set several times, each to the end, and checks that
/// every trigger sounds and ends when its longest variation would.
fn check_sfx_asset(ctl: &mut Controller, ren: &mut Renderer, asset: &SfxAsset) -> Check {
    let name = &asset.name;
    let fail = |detail: String| Check::new("sfx", false, format!("{name}: {detail}"));
    if asset.variations.is_empty() {
        return fail("it has no variations".into());
    }
    let out_rate = f64::from(ren.sample_rate());
    // The slowest playback: the lowest random pitch.
    let slowest = 2f64.powf(f64::from(asset.pitch_semitones.min.min(0.0)) / 12.0);
    let longest = asset
        .variations
        .iter()
        .map(|v| v.length_samples as f64 * out_rate / f64::from(v.sample_rate.max(1)))
        .fold(0.0, f64::max)
        / slowest;
    let limit = (longest * 1.05) as usize + 2 * BLOCK;
    let triggers = (asset.variations.len() * 3).clamp(3, 48);
    let mut buf = vec![0.0f32; BLOCK * 2];
    let (mut shortest, mut longest_heard) = (usize::MAX, 0usize);
    let (mut silent, mut hung) = (0usize, 0usize);
    for _ in 0..triggers {
        let voice = match ctl.play_by_name(name, PlayParams::default()) {
            Ok(v) => v,
            Err(e) => return fail(format!("sky-runtime cannot play it: {e}")),
        };
        let (mut frames, mut peak) = (0usize, 0.0f32);
        while ctl.is_playing(voice) && frames <= limit {
            ctl.update();
            ren.render(&mut buf);
            peak = buf.iter().fold(peak, |p, s| p.max(s.abs()));
            frames += BLOCK;
        }
        if ctl.is_playing(voice) {
            hung += 1;
            stop(ctl, ren, voice);
        }
        if peak <= 1e-4 {
            silent += 1;
        }
        shortest = shortest.min(frames);
        longest_heard = longest_heard.max(frames);
    }
    let secs = |f: usize| f as f64 / out_rate;
    let passed = silent == 0 && hung == 0;
    Check::new(
        "sfx",
        passed,
        if passed {
            format!(
                "{name}: {} variation(s), {}; triggered {triggers} times, each sounded and \
                 ended after {:.2} to {:.2} s",
                asset.variations.len(),
                selection_name(asset.selection),
                secs(shortest),
                secs(longest_heard)
            )
        } else {
            format!(
                "{name}: of {triggers} triggers, {silent} were silent and {hung} did not \
                 end within {:.2} s",
                secs(limit)
            )
        },
    )
}

fn selection_name(selection: Selection) -> &'static str {
    match selection {
        Selection::Random => "picked at random",
        Selection::RandomNoRepeat => "picked at random without repeats",
        Selection::RoundRobin => "played in turn",
        Selection::Shuffle => "shuffled",
    }
}

/// Stops a voice at once and renders until the runtime has let it go.
fn stop(ctl: &mut Controller, ren: &mut Renderer, voice: VoiceId) {
    ctl.stop(voice, 0.0);
    let mut buf = [0.0f32; 64];
    for _ in 0..4 {
        ctl.update();
        ren.render(&mut buf);
    }
    ctl.update();
}

/// Largest device buffer rendered in one go; longer buffers are split.
const SCRATCH_FRAMES: usize = 4_096;

/// The audio callback: the runtime's stereo mix, spread over the device's
/// channels.
struct Feed {
    renderer: Renderer,
    scratch: Vec<f32>,
    channels: usize,
    sample_rate: u32,
    silent: bool,
    stats: Arc<StreamStats>,
}

impl Feed {
    #[inline]
    fn fill<T: SizedSample + FromSample<f32>>(&mut self, out: &mut [T]) {
        let started = Instant::now();
        let ch = self.channels.max(1);
        let mut busy = Duration::ZERO;
        for chunk in out.chunks_mut(SCRATCH_FRAMES * ch) {
            let n = chunk.len() / ch;
            let stereo = &mut self.scratch[..n * 2];
            let t = Instant::now();
            #[cfg(debug_assertions)]
            assert_no_alloc::assert_no_alloc(|| self.renderer.render(stereo));
            #[cfg(not(debug_assertions))]
            self.renderer.render(stereo);
            busy += t.elapsed();
            if self.silent {
                stereo.fill(0.0);
            }
            for (o, s) in chunk.chunks_exact_mut(ch).zip(stereo.as_chunks::<2>().0) {
                if let [mono] = o {
                    *mono = T::from_sample(0.5 * (s[0] + s[1]));
                    continue;
                }
                o[0] = T::from_sample(s[0]);
                o[1] = T::from_sample(s[1]);
                for x in &mut o[2..] {
                    *x = T::EQUILIBRIUM;
                }
            }
        }
        let frames = out.len() / ch;
        let budget = frames as f32 / self.sample_rate.max(1) as f32;
        let load = busy.as_secs_f32() / budget.max(1e-9);
        self.stats.record(frames, started, busy, load);
    }
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut feed: Feed,
    xruns: Arc<AtomicU64>,
    lost: Arc<AtomicBool>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    device.build_output_stream::<T, _, _>(
        config,
        move |out: &mut [T], _| feed.fill(out),
        move |e: cpal::Error| match e.kind() {
            ErrorKind::Xrun => {
                xruns.fetch_add(1, Relaxed);
            }
            ErrorKind::DeviceNotAvailable
            | ErrorKind::StreamInvalidated
            | ErrorKind::DeviceChanged => lost.store(true, Relaxed),
            _ => tracing::warn!("audio stream error: {e}"),
        },
        None,
    )
}

/// Plays the selected assets through the device the way a game would:
/// loops run the whole time and SFX sets fire in turn every
/// `opts.every_ms`, panned left, center and right, from this thread while
/// the renderer runs in the device callback.
fn play_on_device(opts: &GameOptions, selected: &[&Asset]) -> Result<GamePlayback, String> {
    let out = cpal_output(&opts.output).map_err(|e| format!("no audio output: {e}"))?;
    let rate = out.config.sample_rate();
    let channels = out.config.channels();
    let format = out.config.sample_format();
    let mut rt = Runtime::new(rate);
    rt.load_manifest(&opts.dir)
        .map_err(|e| format!("sky-runtime cannot load the export: {e}"))?;
    // Loops and SFX together can add up past full scale.
    rt.set_master_gain(-6.0);
    let (mut ctl, renderer) = rt.split(256);

    let stats = Arc::new(StreamStats::new(out.host == "Wasapi", rate));
    let xruns = Arc::new(AtomicU64::new(0));
    let lost = Arc::new(AtomicBool::new(false));
    let feed = Feed {
        renderer,
        scratch: vec![0.0; SCRATCH_FRAMES * 2],
        channels: usize::from(channels),
        sample_rate: rate,
        silent: opts.output.silent,
        stats: stats.clone(),
    };
    let mut config = out.config.config();
    if let Some(frames) = opts.output.buffer_frames {
        config.buffer_size = cpal::BufferSize::Fixed(frames);
    }
    let (d, x, l) = (&out.device, xruns.clone(), lost.clone());
    let stream = match format {
        SampleFormat::F32 => build::<f32>(d, config, feed, x, l),
        SampleFormat::I16 => build::<i16>(d, config, feed, x, l),
        SampleFormat::I32 => build::<i32>(d, config, feed, x, l),
        SampleFormat::U16 => build::<u16>(d, config, feed, x, l),
        other => return Err(format!("unsupported sample format {other:?}")),
    }
    .map_err(|e| format!("cannot open {}: {e}", out.name))?;
    stream
        .play()
        .map_err(|e| format!("cannot start {}: {e}", out.name))?;

    // Let the stream and thread priorities settle, then count afresh.
    let settle = Instant::now();
    while settle.elapsed() < Duration::from_millis(500) {
        std::thread::sleep(Duration::from_millis(20));
        ctl.update();
    }
    if stats.snapshot().callbacks == 0 {
        return Err(format!(
            "{} on {} opened but never asked for audio",
            out.name, out.host
        ));
    }
    stats.reset();
    let xruns_before = xruns.load(Relaxed);

    let mut loops = Vec::new();
    let mut sfx = Vec::new();
    for asset in selected {
        let Some(handle) = ctl.asset(asset.name()) else {
            continue;
        };
        match asset {
            Asset::Loop(a) => {
                let voice = ctl
                    .play(handle, PlayParams::default())
                    .map_err(|e| format!("cannot start {}: {e}", a.name))?;
                loops.push((a.name.clone(), voice));
            }
            Asset::Sfx(_) => sfx.push(handle),
        }
    }
    let every = Duration::from_millis(u64::from(opts.every_ms.max(10)));
    let duration = Duration::from_secs_f64(opts.seconds);
    let start = Instant::now();
    let mut next = start;
    let mut triggers = 0u64;
    let mut failed_triggers = 0u64;
    while start.elapsed() < duration {
        if lost.load(Relaxed) {
            return Err("the device went away during the check".into());
        }
        if !sfx.is_empty() && Instant::now() >= next {
            let handle = sfx[triggers as usize % sfx.len()];
            let pan = [-0.6, 0.0, 0.6][triggers as usize % 3];
            if ctl
                .play(
                    handle,
                    PlayParams {
                        pan,
                        ..PlayParams::default()
                    },
                )
                .is_err()
            {
                failed_triggers += 1;
            }
            triggers += 1;
            next += every;
        }
        ctl.update();
        std::thread::sleep(Duration::from_millis(5));
    }
    let elapsed = start.elapsed();
    let stopped: Vec<&str> = loops
        .iter()
        .filter(|(_, v)| !ctl.is_playing(*v))
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>();
    let s = stats.snapshot();
    let host_xruns = xruns.load(Relaxed) - xruns_before;
    ctl.stop_all(100.0);
    std::thread::sleep(Duration::from_millis(200));
    ctl.update();
    drop(stream);

    let sr = f64::from(rate.max(1));
    let audio_seconds = s.frames as f64 / sr;
    let dropouts = host_xruns + s.late;
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let mut checks = vec![Check::new(
        "callbacks",
        s.callbacks > 0 && audio_seconds >= elapsed.as_secs_f64() * 0.9,
        format!(
            "{} callbacks of {} to {} frames gave {audio_seconds:.2} s of audio in {:.2} s",
            s.callbacks,
            s.min_frames,
            s.max_frames,
            elapsed.as_secs_f64()
        ),
    )];
    if !loops.is_empty() {
        checks.push(Check::new(
            "loops",
            stopped.is_empty(),
            if stopped.is_empty() {
                format!(
                    "{} played the whole time",
                    loops
                        .iter()
                        .map(|(n, _)| n.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            } else {
                format!("{} stopped before the end", stopped.join(", "))
            },
        ));
    }
    if !sfx.is_empty() {
        checks.push(Check::new(
            "sfx",
            triggers > 0 && failed_triggers == 0,
            format!(
                "{triggers} trigger(s) from the game thread every {} ms, {failed_triggers} \
                 refused",
                every.as_millis()
            ),
        ));
    }
    checks.push(Check::new(
        "dropouts",
        dropouts == 0,
        format!(
            "{host_xruns} reported by the host, {} late hand-off(s); audio reached the device \
             at most {:.2} ms apart, and it can play {:.2} ms on one",
            s.late,
            ms(s.max_handoff),
            ms(s.handoff_budget)
        ),
    ));
    checks.push(Check::new(
        "render time",
        s.max_load < 1.0,
        format!(
            "rendering took at most {:.1}% of the time its buffer lasts",
            s.max_load * 100.0
        ),
    ));
    Ok(GamePlayback {
        passed: checks.iter().all(|c| c.passed),
        host: out.host,
        device: out.name,
        sample_rate: rate,
        channels,
        sample_format: format!("{format:?}"),
        seconds: (elapsed.as_secs_f64() * 100.0).round() / 100.0,
        loops: loops.into_iter().map(|(n, _)| n).collect(),
        sfx_triggers: triggers,
        callbacks: s.callbacks,
        callback_frames_max: s.max_frames,
        dropouts,
        load_max: (s.max_load * 1000.0).round() / 1000.0,
        silent: opts.output.silent,
        checks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sky_model::templates::demo_project;
    use sky_session::{GameExportOptions, Session};

    #[test]
    fn the_demo_export_passes_offline() {
        let tmp = tempfile::tempdir().unwrap();
        let (session, _) = Session::new(demo_project(), None);
        session
            .export_game(&GameExportOptions {
                dir: Some(tmp.path().to_path_buf()),
                ..Default::default()
            })
            .unwrap();
        let report = check_game(&GameOptions {
            dir: tmp.path().to_path_buf(),
            seconds: 0.0,
            ..Default::default()
        })
        .unwrap();
        assert!(report.passed, "{}", report.lines().join("\n"));
        assert_eq!((report.loops, report.sfx), (1, 1));
        assert_eq!(report.checks.len(), 3);

        let only = check_game(&GameOptions {
            dir: tmp.path().to_path_buf(),
            assets: vec!["demo_hits".into()],
            seconds: 0.0,
            ..Default::default()
        })
        .unwrap();
        assert_eq!((only.loops, only.sfx), (0, 1));
        let e = check_game(&GameOptions {
            dir: tmp.path().to_path_buf(),
            assets: vec!["nope".into()],
            seconds: 0.0,
            ..Default::default()
        })
        .unwrap_err();
        assert!(e.contains("demo_music"), "{e}");
    }

    #[test]
    fn a_broken_file_fails_the_load_check() {
        let tmp = tempfile::tempdir().unwrap();
        let (session, _) = Session::new(demo_project(), None);
        let out = session
            .export_game(&GameExportOptions {
                dir: Some(tmp.path().to_path_buf()),
                ..Default::default()
            })
            .unwrap();
        std::fs::write(&out.files[0], b"not a wav file").unwrap();
        let report = check_game(&GameOptions {
            dir: tmp.path().to_path_buf(),
            seconds: 0.0,
            ..Default::default()
        })
        .unwrap();
        assert!(!report.passed);
        assert_eq!(report.checks.len(), 1);
        assert_eq!(report.checks[0].name, "load");
    }

    #[test]
    fn mono_loops_match_at_the_center_pan() {
        let tmp = tempfile::tempdir().unwrap();
        let audio = sky_dsp::AudioData {
            sample_rate: 48_000,
            channels: vec![(0..24_000).map(|i| (i as f32 * 0.02).sin() * 0.5).collect()],
        };
        let out = sky_game::export_loop(
            tmp.path(),
            &sky_game::LoopExport {
                name: "drone",
                project: "Hand made",
                audio: &audio,
                loop_start: 0,
                loop_end: 24_000,
                tempo_bpm: None,
                time_signature: None,
                bars: None,
                category: sky_core::manifest::Category::Ambience,
                tags: vec![],
                credits: vec![],
                format: sky_io::WavFormat::Float32,
            },
        )
        .unwrap();
        assert_eq!(out.asset.files().next().unwrap().channels, 1);
        let report = check_game(&GameOptions {
            dir: tmp.path().to_path_buf(),
            seconds: 0.0,
            ..Default::default()
        })
        .unwrap();
        assert!(report.passed, "{}", report.lines().join("\n"));
        assert_eq!(report.loops, 1);
    }

    #[test]
    fn a_missing_manifest_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let e = check_game(&GameOptions {
            dir: tmp.path().to_path_buf(),
            ..Default::default()
        })
        .unwrap_err();
        assert!(e.contains("export the game assets first"), "{e}");
    }
}
