// SPDX-License-Identifier: GPL-3.0-or-later
//! The device check: play through the real output for a while at a buffer
//! size and count everything that would be heard as a dropout.

use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

use serde::Serialize;
use sky_io::device::{OutputConfig, StreamStatsSnapshot, probe_output, start_output};
use sky_session::Session;

use crate::Check;

/// Options for [`check_device`].
#[derive(Clone, Debug)]
pub struct DeviceOptions {
    pub output: OutputConfig,
    /// How long to measure, after the stream has settled.
    pub duration: Duration,
    /// Ignored at the start while the device and the thread priorities settle.
    pub settle: Duration,
    /// How often `progress` is called.
    pub progress_every: Duration,
}

impl Default for DeviceOptions {
    fn default() -> Self {
        Self {
            output: OutputConfig {
                buffer_frames: Some(128),
                ..OutputConfig::default()
            },
            duration: Duration::from_secs(20),
            settle: Duration::from_secs(1),
            progress_every: Duration::from_secs(10),
        }
    }
}

/// Counters while the check runs.
#[derive(Clone, Copy, Debug)]
pub struct DeviceProgress {
    pub elapsed: Duration,
    pub callbacks: u64,
    pub dropouts: u64,
    pub overruns: u64,
    pub load_avg: f32,
    pub load_max: f32,
}

/// The result of [`check_device`].
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCheck {
    pub passed: bool,
    pub host: String,
    /// How the stream shared the device: shared, low-latency or exclusive.
    pub mode: Option<String>,
    /// The mode asked for, where it differs from the one used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_mode: Option<String>,
    /// Why the stream ran differently from what was asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub device: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_format: String,
    /// The buffer size asked for, if any.
    pub requested_buffer_frames: Option<u32>,
    /// What the device really asked for in each callback.
    pub callback_frames_min: u32,
    pub callback_frames_max: u32,
    pub callback_frames_avg: f64,
    /// The largest callback in milliseconds: the real buffer latency.
    pub buffer_ms: f64,
    /// Whether every callback was at most the requested size.
    pub buffer_honored: Option<bool>,
    /// The shortest period any mode can run the device at, where Windows
    /// tells. A request below it passes when the device ran at it.
    pub device_min_frames: Option<u32>,
    /// The device's whole buffer, where the host shows it (WASAPI).
    pub device_buffer_frames: Option<u32>,
    pub device_buffer_ms: Option<f64>,
    /// The smallest periods Windows says the device supports in shared
    /// mode (with `IAudioClient3`) and in exclusive mode.
    pub shared_min_frames: Option<u32>,
    pub exclusive_min_frames: Option<u32>,
    /// The longest time between the starts of two callbacks.
    pub max_callback_gap_ms: f64,
    /// The longest time between two hand-offs of audio to the device, and
    /// how long the device can play on one: past that it runs dry.
    pub max_handoff_ms: f64,
    pub handoff_budget_ms: f64,
    pub seconds: f64,
    pub callbacks: u64,
    /// Everything heard as a gap: errors the host reported plus hand-offs
    /// that came after the device had run dry.
    pub dropouts: u64,
    pub host_xruns: u64,
    pub late_handoffs: u64,
    /// Callbacks where the engine took longer than the buffer lasts.
    pub overruns: u64,
    /// Engine time as a share of the audio time, on average and at worst.
    pub load_avg: f32,
    pub load_max: f32,
    /// True when the device got silence while the engine ran.
    pub silent: bool,
    pub checks: Vec<Check>,
}

/// How a stream's callbacks compare with the buffer size asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Buffer {
    /// Every callback was at most the request.
    Honored,
    /// The request is below the device's shortest period in any mode, and
    /// the stream ran at that period.
    AtFloor(u32),
    NotHonored,
}

fn buffer_verdict(requested: u32, callbacks: u64, max_frames: u32, floor: Option<u32>) -> Buffer {
    if callbacks == 0 {
        return Buffer::NotHonored;
    }
    match floor {
        _ if max_frames <= requested => Buffer::Honored,
        Some(f) if requested < f && max_frames <= f => Buffer::AtFloor(f),
        _ => Buffer::NotHonored,
    }
}

fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 100_000.0).round() / 100.0
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Plays the session's project through the output device for
/// `opts.duration` (looping, from the start) and reports what the device
/// did. The session must not already drive a device: the check attaches
/// its own engine and detaches it afterwards.
pub fn check_device(
    session: &Session,
    opts: &DeviceOptions,
    progress: &mut dyn FnMut(&DeviceProgress),
) -> Result<DeviceCheck, String> {
    let info = probe_output(&opts.output).map_err(|e| format!("no audio output: {e}"))?;
    let engine = session.attach_engine(info.sample_rate);
    let result = run(session, opts, engine, progress);
    session.detach_engine();
    result
}

fn run(
    session: &Session,
    opts: &DeviceOptions,
    engine: sky_engine::Engine,
    progress: &mut dyn FnMut(&DeviceProgress),
) -> Result<DeviceCheck, String> {
    let stream =
        start_output(&opts.output, engine).map_err(|e| format!("cannot start audio: {e}"))?;
    let shared = session.engine_shared().ok_or("the engine did not start")?;
    let stats = stream.stats().clone();
    session.play().map_err(|e| e.to_string())?;

    // Let the stream and the thread priorities settle, then count afresh.
    let settle_until = Instant::now() + opts.settle;
    while Instant::now() < settle_until {
        std::thread::sleep(Duration::from_millis(20));
        if stream.is_lost() {
            return Err("the device went away while starting".into());
        }
    }
    if stats.snapshot().callbacks == 0 {
        return Err(format!(
            "{} on {} opened but never asked for audio",
            stream.info.name, stream.info.host
        ));
    }
    stats.reset();
    let xruns_before = shared.dropouts.load(Relaxed);
    let overruns_before = shared.overruns.load(Relaxed);

    let sr = stream.info.sample_rate.max(1) as f64;
    let load_avg = |s: &StreamStatsSnapshot| {
        let audio = s.frames as f64 / sr;
        if audio > 0.0 {
            (s.busy.as_secs_f64() / audio) as f32
        } else {
            0.0
        }
    };
    let start = Instant::now();
    let mut next_report = start + opts.progress_every;
    while start.elapsed() < opts.duration {
        std::thread::sleep(Duration::from_millis(50));
        if stream.is_lost() {
            return Err("the device went away during the check".into());
        }
        if Instant::now() >= next_report {
            next_report += opts.progress_every;
            let s = stats.snapshot();
            progress(&DeviceProgress {
                elapsed: start.elapsed(),
                callbacks: s.callbacks,
                dropouts: shared.dropouts.load(Relaxed) - xruns_before + s.late,
                overruns: shared.overruns.load(Relaxed) - overruns_before,
                load_avg: load_avg(&s),
                load_max: s.max_load,
            });
        }
    }
    let elapsed = start.elapsed();
    let s = stats.snapshot();
    let xruns = shared.dropouts.load(Relaxed) - xruns_before;
    let overruns = shared.overruns.load(Relaxed) - overruns_before;
    let _ = session.stop();
    let dev = stream.info.clone();
    let note = stream.note.clone();
    drop(stream);

    let requested = opts.output.buffer_frames;
    let honored = requested.map(|r| s.callbacks > 0 && s.max_frames <= r);
    let periods = if dev.host == "Wasapi" {
        sky_io::periods::device_periods(opts.output.device.as_deref())
    } else {
        None
    };
    let dropouts = xruns + s.late;
    let buffer_ms = s.max_frames as f64 / sr * 1000.0;
    let mode = dev.mode.map(|m| m.to_string());
    let in_mode = mode
        .as_deref()
        .map(|m| format!(" in {m} mode"))
        .unwrap_or_default();
    let mut checks = vec![
        Check::new(
            "callbacks",
            s.callbacks > 0,
            format!(
                "{} callbacks in {:.1} s{in_mode}, {} to {} frames each, at most {:.2} ms apart{}",
                s.callbacks,
                elapsed.as_secs_f64(),
                s.min_frames,
                s.max_frames,
                ms(s.max_gap),
                s.device_buffer_frames
                    .map(|f| format!("; the device buffer holds {f} frames"))
                    .unwrap_or_default()
            ),
        ),
        Check::new(
            "dropouts",
            dropouts == 0,
            format!(
                "{xruns} reported by the host, {} late hand-off(s); audio reached the \
                 device at most {:.2} ms apart, and it can play {:.2} ms on one",
                s.late,
                ms(s.max_handoff),
                ms(s.handoff_budget)
            ),
        ),
        Check::new(
            "overruns",
            overruns == 0,
            format!(
                "{overruns} callback(s) took longer than their buffer; load {:.1}% on \
                 average, {:.1}% at worst",
                load_avg(&s) * 100.0,
                s.max_load * 100.0
            ),
        ),
    ];
    // The device's floor, counted at the stream's rate.
    let floor = periods.map(|p| {
        (u64::from(p.shortest()) * u64::from(dev.sample_rate))
            .div_ceil(u64::from(p.sample_rate.max(1))) as u32
    });
    if let Some(r) = requested {
        let verdict = buffer_verdict(r, s.callbacks, s.max_frames, floor);
        let asked = format!("asked for {r} frames ({:.2} ms)", r as f64 / sr * 1000.0);
        let detail = match verdict {
            Buffer::Honored => {
                format!(
                    "{asked} and the device used at most {} frames",
                    s.max_frames
                )
            }
            Buffer::AtFloor(f) => format!(
                "{asked}, below the {f} frames ({:.2} ms) this device can do at the least in \
                 any mode, and {}{in_mode} ran at that: up to {} frames",
                f as f64 / sr * 1000.0,
                dev.host,
                s.max_frames
            ),
            Buffer::NotHonored => {
                let mut detail = format!(
                    "{asked} but {}{in_mode} called back with up to {} frames \
                     ({buffer_ms:.2} ms); the request was not honored",
                    dev.host, s.max_frames
                );
                if let Some(n) = &note {
                    detail.push_str(&format!(" ({n})"));
                }
                if let Some(p) = periods {
                    let f = |frames: u32| {
                        format!(
                            "{frames} frames ({:.2} ms)",
                            frames as f64 / p.sample_rate.max(1) as f64 * 1000.0
                        )
                    };
                    detail.push_str(&format!(
                        ". Windows says this device runs shared mode at {} by default and \
                         at {} at the least with a low-latency stream; exclusive mode goes \
                         down to {}",
                        f(p.shared_default),
                        f(p.shared_min),
                        f(p.exclusive_min)
                    ));
                }
                detail
            }
        };
        checks.push(Check::new(
            "buffer size",
            verdict != Buffer::NotHonored,
            detail,
        ));
    }
    let asked = opts.output.mode.to_string();
    Ok(DeviceCheck {
        passed: checks.iter().all(|c| c.passed),
        host: dev.host,
        requested_mode: (mode.as_deref() != Some(asked.as_str())).then_some(asked),
        mode,
        note,
        device: dev.name,
        sample_rate: dev.sample_rate,
        channels: dev.channels,
        sample_format: dev.sample_format,
        requested_buffer_frames: requested,
        callback_frames_min: s.min_frames,
        callback_frames_max: s.max_frames,
        callback_frames_avg: if s.callbacks > 0 {
            round2(s.frames as f64 / s.callbacks as f64)
        } else {
            0.0
        },
        buffer_ms: round2(buffer_ms),
        buffer_honored: honored,
        device_min_frames: floor,
        device_buffer_frames: s.device_buffer_frames,
        device_buffer_ms: s
            .device_buffer_frames
            .map(|f| round2(f as f64 / sr * 1000.0)),
        shared_min_frames: periods.map(|p| p.shared_min),
        exclusive_min_frames: periods.map(|p| p.exclusive_min),
        max_callback_gap_ms: ms(s.max_gap),
        max_handoff_ms: ms(s.max_handoff),
        handoff_budget_ms: ms(s.handoff_budget),
        seconds: round2(elapsed.as_secs_f64()),
        callbacks: s.callbacks,
        dropouts,
        host_xruns: xruns,
        late_handoffs: s.late,
        overruns,
        load_avg: (load_avg(&s) * 1000.0).round() / 1000.0,
        load_max: (s.max_load * 1000.0).round() / 1000.0,
        silent: opts.output.silent,
        checks,
    })
}

#[cfg(test)]
mod tests {
    use super::{Buffer, buffer_verdict};

    #[test]
    fn a_buffer_passes_at_the_request_or_at_the_device_floor() {
        // S/PDIF: 128 asked, 128 used.
        assert_eq!(buffer_verdict(128, 100, 128, Some(128)), Buffer::Honored);
        // The headset in exclusive mode: nothing goes below 144.
        assert_eq!(
            buffer_verdict(128, 100, 144, Some(144)),
            Buffer::AtFloor(144)
        );
        // The headset in shared mode: 480 when 144 was possible.
        assert_eq!(buffer_verdict(128, 100, 480, Some(144)), Buffer::NotHonored);
        // Unknown floor (another host): only the request counts.
        assert_eq!(buffer_verdict(128, 100, 144, None), Buffer::NotHonored);
        // A request the device could meet is not excused by the floor.
        assert_eq!(buffer_verdict(256, 100, 480, Some(144)), Buffer::NotHonored);
        // No callbacks at all never passes.
        assert_eq!(buffer_verdict(128, 0, 0, Some(144)), Buffer::NotHonored);
    }
}
