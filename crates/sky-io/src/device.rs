// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio output through cpal: WASAPI on Windows (ASIO with the `asio`
//! feature), ALSA or PipeWire on Linux, Core Audio on macOS. On Windows a
//! WASAPI output of our own adds what cpal lacks: exclusive mode and
//! low-latency shared mode (see [`OutputMode`]).

#[cfg(windows)]
mod wasapi;

use std::cell::Cell;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, FromSample, SampleFormat, SizedSample, StreamConfig};
use sky_engine::{Engine, EngineShared};

use crate::codec::IoError;
use crate::periods::DevicePeriods;

/// How the output shares a WASAPI device with other programs. Other hosts
/// (ASIO, Linux, macOS) have one way and take only `Auto` or `Shared`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OutputMode {
    /// Shared mode, unless the requested buffer is shorter than shared mode
    /// can go: then low-latency shared mode if the driver allows the
    /// buffer, otherwise exclusive mode. Falls back to shared mode if the
    /// device refuses.
    #[default]
    Auto,
    /// Through the Windows mixer at its usual period, 10 ms on most devices.
    Shared,
    /// Through the Windows mixer at a shorter period, where the driver
    /// allows one (Windows 10 and later). Other programs keep playing.
    LowLatency,
    /// The app takes the device for itself: the shortest buffers the
    /// hardware can do, but other programs go silent while it plays.
    Exclusive,
}

impl OutputMode {
    pub const ALL: [OutputMode; 4] = [
        OutputMode::Auto,
        OutputMode::Shared,
        OutputMode::LowLatency,
        OutputMode::Exclusive,
    ];

    /// The name used on the command line and in reports.
    pub fn name(self) -> &'static str {
        match self {
            OutputMode::Auto => "auto",
            OutputMode::Shared => "shared",
            OutputMode::LowLatency => "low-latency",
            OutputMode::Exclusive => "exclusive",
        }
    }
}

impl fmt::Display for OutputMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for OutputMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        OutputMode::ALL
            .into_iter()
            .find(|m| m.name() == s.trim().to_lowercase().replace([' ', '_'], "-"))
            .ok_or_else(|| {
                format!("the mode must be auto, shared, low-latency or exclusive, not {s:?}")
            })
    }
}

/// The mode [`OutputMode::Auto`] picks for a buffer of `frames` at
/// `sample_rate` on a device with `periods`: shared mode when it can run
/// that buffer, else low-latency shared mode when that can, else whichever
/// of exclusive and low-latency shared mode goes lower.
pub fn auto_mode(
    frames: Option<u32>,
    sample_rate: u32,
    periods: Option<&DevicePeriods>,
) -> OutputMode {
    let (Some(frames), Some(p)) = (frames, periods) else {
        return OutputMode::Shared;
    };
    // The periods are counted at the device's mix rate.
    let wanted = (frames as u64 * p.sample_rate as u64 / sample_rate.max(1) as u64) as u32;
    if wanted >= p.shared_default {
        return OutputMode::Shared;
    }
    // Low-latency shared mode runs at the mix rate only.
    let low =
        (p.has_low_latency() && sample_rate == p.sample_rate).then(|| p.low_latency_period(wanted));
    match low {
        Some(period) if period <= wanted => OutputMode::LowLatency,
        Some(period) if p.exclusive_min >= period => OutputMode::LowLatency,
        _ if p.exclusive_min > 0 && p.exclusive_min < p.shared_default => OutputMode::Exclusive,
        _ => OutputMode::Shared,
    }
}

/// What the caller asks for. `None` means the device's default.
#[derive(Clone, Debug, Default)]
pub struct OutputConfig {
    /// Part of a device name to look for; the default device otherwise.
    pub device: Option<String>,
    pub sample_rate: Option<u32>,
    /// Requested buffer size in frames, such as 128. Without one, exclusive
    /// mode runs at the device's default period and low-latency shared mode
    /// at the shortest the driver allows.
    pub buffer_frames: Option<u32>,
    /// How to share the device with other programs.
    pub mode: OutputMode,
    /// Use the ASIO host instead of WASAPI (Windows, `asio` feature).
    pub asio: bool,
    /// Run the engine as usual but send silence to the device, so a test
    /// can measure real playback without making a sound.
    pub silent: bool,
}

/// The device a stream will open, as resolved from an [`OutputConfig`].
#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub host: String,
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
    /// The buffer size asked for.
    pub buffer_frames: Option<u32>,
    pub sample_format: String,
    /// The mode a running stream uses; `None` before it starts and on ASIO.
    pub mode: Option<OutputMode>,
    /// The period the device really runs at, where the host tells: in
    /// low-latency shared and exclusive mode.
    pub period_frames: Option<u32>,
}

fn host(asio: bool) -> Result<cpal::Host, IoError> {
    #[cfg(all(windows, feature = "asio"))]
    if asio {
        return cpal::host_from_id(cpal::HostId::Asio).map_err(|e| IoError::Device(e.to_string()));
    }
    if asio {
        return Err(IoError::Device(
            "this build has no ASIO support; rebuild with the asio feature".into(),
        ));
    }
    Ok(cpal::default_host())
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "Unknown device".into())
}

/// Names of the output devices on the default host.
pub fn list_output_devices() -> Vec<String> {
    let Ok(host) = host(false) else {
        return Vec::new();
    };
    host.output_devices()
        .map(|devs| devs.map(|d| device_name(&d)).collect())
        .unwrap_or_default()
}

fn pick(
    config: &OutputConfig,
) -> Result<(cpal::Device, cpal::SupportedStreamConfig, String), IoError> {
    let host = host(config.asio)?;
    let host_name = format!("{:?}", host.id());
    let device = match &config.device {
        Some(wanted) => host
            .output_devices()
            .map_err(|e| IoError::Device(e.to_string()))?
            .find(|d| {
                device_name(d)
                    .to_lowercase()
                    .contains(&wanted.to_lowercase())
            })
            .ok_or_else(|| IoError::Device(format!("no output device matching {wanted:?}")))?,
        None => host
            .default_output_device()
            .ok_or_else(|| IoError::Device("no default output device".into()))?,
    };
    let default = device
        .default_output_config()
        .map_err(|e| IoError::Device(e.to_string()))?;
    let supported = match config.sample_rate {
        Some(rate) if rate != default.sample_rate() => device
            .supported_output_configs()
            .map_err(|e| IoError::Device(e.to_string()))?
            .find(|c| {
                c.min_sample_rate() <= rate && rate <= c.max_sample_rate() && c.channels() >= 2
            })
            .map(|c| c.with_sample_rate(rate))
            .ok_or_else(|| IoError::Device(format!("the device does not support {rate} Hz")))?,
        _ => default,
    };
    Ok((device, supported, host_name))
}

/// Resolves the device and format a stream would use, without opening it.
pub fn probe_output(config: &OutputConfig) -> Result<DeviceInfo, IoError> {
    let (device, supported, host) = pick(config)?;
    Ok(DeviceInfo {
        host,
        name: device_name(&device),
        sample_rate: supported.sample_rate(),
        channels: supported.channels(),
        buffer_frames: config.buffer_frames,
        sample_format: format!("{:?}", supported.sample_format()),
        mode: None,
        period_frames: None,
    })
}

/// What plays a stream, held only to be dropped when the stream stops.
#[allow(dead_code)]
enum Backend {
    Cpal(cpal::Stream),
    #[cfg(windows)]
    Wasapi(wasapi::WasapiStream),
}

/// A running output stream. Dropping it stops playback.
pub struct AudioStream {
    _stream: Backend,
    pub info: DeviceInfo,
    /// Why the stream runs differently from what was asked, such as a mode
    /// the device refused.
    pub note: Option<String>,
    lost: Arc<AtomicBool>,
    stats: Arc<StreamStats>,
}

impl AudioStream {
    /// True once the device has gone away (unplugged, format changed); the
    /// caller should open a new stream.
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }

    /// How the device has been calling back so far.
    pub fn stats(&self) -> &Arc<StreamStats> {
        &self.stats
    }
}

/// How a stream's callbacks behave, measured on the audio thread: the
/// buffer sizes the device really asks for, how evenly the calls come, how
/// long the engine took, and whether audio reached the device in time. A
/// requested buffer size is only a request; this is what the device did.
///
/// A dropout cannot be seen from inside one callback: a late callback still
/// gets a normal-looking buffer. What gives it away is the time between two
/// hand-offs (the ends of two callbacks). After a hand-off the device holds
/// at most its buffer; if the next hand-off comes later than that buffer
/// lasts, the device ran dry in between and played silence.
pub struct StreamStats {
    epoch: Instant,
    sample_rate: u32,
    /// Whether the host hands each callback all the free space in its
    /// buffer, as WASAPI does. The first callback then fills the whole,
    /// empty buffer, which shows its size.
    fills_free_space: bool,
    first_frames: AtomicU32,
    callbacks: AtomicU64,
    frames: AtomicU64,
    min_frames: AtomicU32,
    max_frames: AtomicU32,
    busy_ns: AtomicU64,
    /// Highest load as f32 bits; non-negative floats order like their bits.
    max_load: AtomicU32,
    last_start_ns: AtomicU64,
    max_gap_ns: AtomicU64,
    last_end_ns: AtomicU64,
    max_handoff_ns: AtomicU64,
    late: AtomicU64,
}

/// [`StreamStats`] at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StreamStatsSnapshot {
    pub callbacks: u64,
    pub frames: u64,
    /// Smallest and largest callback in frames; 0 before the first one.
    pub min_frames: u32,
    pub max_frames: u32,
    /// The device's whole buffer, on hosts where the first callback fills
    /// it (WASAPI).
    pub device_buffer_frames: Option<u32>,
    /// Time the engine spent rendering.
    pub busy: Duration,
    /// The largest share of a buffer's duration the engine used.
    pub max_load: f32,
    /// The longest time between the starts of two callbacks.
    pub max_gap: Duration,
    /// The longest time between the ends of two callbacks, when audio is
    /// handed to the device.
    pub max_handoff: Duration,
    /// How long the device can play on what it holds after a hand-off.
    pub handoff_budget: Duration,
    /// Hand-offs that came after the device must have run dry: dropouts.
    pub late: u64,
}

const UNKNOWN: u64 = u64::MAX;

impl StreamStats {
    fn new(fills_free_space: bool, sample_rate: u32) -> Self {
        Self {
            epoch: Instant::now(),
            sample_rate: sample_rate.max(1),
            fills_free_space,
            first_frames: AtomicU32::new(0),
            callbacks: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            min_frames: AtomicU32::new(u32::MAX),
            max_frames: AtomicU32::new(0),
            busy_ns: AtomicU64::new(0),
            max_load: AtomicU32::new(0),
            last_start_ns: AtomicU64::new(UNKNOWN),
            max_gap_ns: AtomicU64::new(0),
            last_end_ns: AtomicU64::new(UNKNOWN),
            max_handoff_ns: AtomicU64::new(0),
            late: AtomicU64::new(0),
        }
    }

    /// Starts counting afresh, for example once the stream has settled. The
    /// buffer size the first callback showed is kept.
    pub fn reset(&self) {
        let r = Ordering::Relaxed;
        self.callbacks.store(0, r);
        self.frames.store(0, r);
        self.min_frames.store(u32::MAX, r);
        self.max_frames.store(0, r);
        self.busy_ns.store(0, r);
        self.max_load.store(0, r);
        self.last_start_ns.store(UNKNOWN, r);
        self.max_gap_ns.store(0, r);
        self.last_end_ns.store(UNKNOWN, r);
        self.max_handoff_ns.store(0, r);
        self.late.store(0, r);
    }

    fn device_buffer(&self) -> Option<u32> {
        let first = self.first_frames.load(Ordering::Relaxed);
        (self.fills_free_space && first > 0).then_some(first)
    }

    /// How long the device can play after a hand-off of `frames`: its whole
    /// buffer where that is known, otherwise two callbacks' worth (the
    /// buffer just handed over and the one playing).
    fn budget_ns(&self, frames: u32) -> u64 {
        let held = self.device_buffer().unwrap_or(frames.saturating_mul(2));
        held as u64 * 1_000_000_000 / self.sample_rate as u64
    }

    pub fn snapshot(&self) -> StreamStatsSnapshot {
        let r = Ordering::Relaxed;
        let callbacks = self.callbacks.load(r);
        let max_frames = self.max_frames.load(r);
        StreamStatsSnapshot {
            callbacks,
            frames: self.frames.load(r),
            min_frames: if callbacks == 0 {
                0
            } else {
                self.min_frames.load(r)
            },
            max_frames,
            device_buffer_frames: self.device_buffer(),
            busy: Duration::from_nanos(self.busy_ns.load(r)),
            max_load: f32::from_bits(self.max_load.load(r)),
            max_gap: Duration::from_nanos(self.max_gap_ns.load(r)),
            max_handoff: Duration::from_nanos(self.max_handoff_ns.load(r)),
            handoff_budget: Duration::from_nanos(self.budget_ns(max_frames.max(1))),
            late: self.late.load(r),
        }
    }

    /// Records one device callback that started at `started` and is handing
    /// its audio over now.
    #[inline]
    fn record(&self, frames: usize, started: Instant, busy: Duration, load: f32) {
        let r = Ordering::Relaxed;
        let ns = |t: Instant| t.saturating_duration_since(self.epoch).as_nanos() as u64;
        let (start, end) = (ns(started), ns(Instant::now()));
        let frames = frames as u32;
        let _ = self.first_frames.compare_exchange(0, frames, r, r);
        let last_start = self.last_start_ns.swap(start, r);
        if last_start != UNKNOWN {
            self.max_gap_ns
                .fetch_max(start.saturating_sub(last_start), r);
        }
        let last_end = self.last_end_ns.swap(end, r);
        if last_end != UNKNOWN {
            let handoff = end.saturating_sub(last_end);
            self.max_handoff_ns.fetch_max(handoff, r);
            if handoff > self.budget_ns(frames) {
                self.late.fetch_add(1, r);
            }
        }
        self.callbacks.fetch_add(1, r);
        self.frames.fetch_add(frames as u64, r);
        self.min_frames.fetch_min(frames, r);
        self.max_frames.fetch_max(frames, r);
        self.busy_ns.fetch_add(busy.as_nanos() as u64, r);
        self.max_load.fetch_max(load.max(0.0).to_bits(), r);
    }
}

thread_local! {
    static AUDIO_THREAD_READY: Cell<bool> = const { Cell::new(false) };
}

/// What the audio callback needs besides the engine.
struct Callback {
    channels: usize,
    sample_rate: u32,
    silent: bool,
    stats: Arc<StreamStats>,
}

impl Callback {
    /// Runs the engine for `out`, all or part of a device buffer, with
    /// real-time checks in debug builds. Returns the time it took.
    #[inline]
    fn render(&self, engine: &mut Engine, out: &mut [f32]) -> Duration {
        if !AUDIO_THREAD_READY.with(Cell::get) {
            sky_engine::enable_flush_to_zero();
            AUDIO_THREAD_READY.with(|c| c.set(true));
        }
        let started = Instant::now();
        #[cfg(debug_assertions)]
        assert_no_alloc::assert_no_alloc(|| engine.process_interleaved(out, self.channels));
        #[cfg(not(debug_assertions))]
        engine.process_interleaved(out, self.channels);
        if self.silent {
            out.fill(0.0);
        }
        started.elapsed()
    }

    /// Publishes the load and timing of one whole device buffer.
    #[inline]
    fn finish(&self, engine: &Engine, frames: usize, started: Instant, busy: Duration) {
        let budget = frames as f32 / self.sample_rate.max(1) as f32;
        let load = busy.as_secs_f32() / budget.max(1e-9);
        let shared = engine.shared();
        shared.load.store(load.to_bits(), Ordering::Relaxed);
        shared.callbacks.fetch_add(1, Ordering::Relaxed);
        if load > 1.0 {
            shared.overruns.fetch_add(1, Ordering::Relaxed);
        }
        self.stats.record(frames, started, busy, load);
    }
}

/// Opens the device and starts pulling audio from `engine`. The engine must
/// have been created for `info.sample_rate` (see [`probe_output`]).
pub fn start_output(config: &OutputConfig, engine: Engine) -> Result<AudioStream, IoError> {
    let (device, supported, host) = pick(config)?;
    if host != "Wasapi" {
        return match config.mode {
            OutputMode::Auto | OutputMode::Shared => {
                let mode = (host != "Asio").then_some(OutputMode::Shared);
                start_cpal(config, device, supported, host, mode, engine)
            }
            other => Err(IoError::Device(format!(
                "{other} mode is for WASAPI on Windows; {host} has no such mode"
            ))),
        };
    }
    #[cfg(windows)]
    {
        start_wasapi(config, device, supported, host, engine)
    }
    #[cfg(not(windows))]
    {
        start_cpal(
            config,
            device,
            supported,
            host,
            Some(OutputMode::Shared),
            engine,
        )
    }
}

/// WASAPI in the mode asked for, through cpal for shared mode and our own
/// output for the others. In auto mode a device that refuses the chosen
/// mode gets the next best one, down to shared mode.
#[cfg(windows)]
fn start_wasapi(
    config: &OutputConfig,
    device: cpal::Device,
    supported: cpal::SupportedStreamConfig,
    host: String,
    mut engine: Engine,
) -> Result<AudioStream, IoError> {
    let sample_rate = supported.sample_rate();
    let auto = config.mode == OutputMode::Auto;
    let periods = (auto && config.buffer_frames.is_some())
        .then(|| crate::periods::device_periods(config.device.as_deref()))
        .flatten();
    let first = if auto {
        auto_mode(config.buffer_frames, sample_rate, periods.as_ref())
    } else {
        config.mode
    };
    // The modes to try before shared mode, best first.
    let mut tries = Vec::new();
    if first != OutputMode::Shared {
        tries.push(first);
    }
    if auto
        && first == OutputMode::Exclusive
        && periods.as_ref().is_some_and(|p| p.has_low_latency())
    {
        tries.push(OutputMode::LowLatency);
    }
    let mut refused: Vec<String> = Vec::new();
    for mode in tries {
        let lost = Arc::new(AtomicBool::new(false));
        let request = wasapi::Request {
            device: config.device.clone(),
            mode,
            sample_rate,
            frames: config.buffer_frames,
            silent: config.silent,
        };
        match wasapi::start(request, engine, lost.clone()) {
            Ok(opened) => {
                let note = (!refused.is_empty())
                    .then(|| format!("{}, so audio runs in {mode} mode", refused.join("; ")));
                return Ok(AudioStream {
                    info: DeviceInfo {
                        host,
                        name: opened.name,
                        sample_rate,
                        channels: opened.channels,
                        buffer_frames: config.buffer_frames,
                        sample_format: opened.sample_format,
                        mode: Some(mode),
                        period_frames: Some(opened.period_frames),
                    },
                    _stream: Backend::Wasapi(opened.stream),
                    note,
                    lost,
                    stats: opened.stats,
                });
            }
            Err((e, Some(back))) if auto => {
                tracing::warn!("{mode} mode failed: {e}");
                refused.push(format!("{mode} mode failed ({e})"));
                engine = *back;
            }
            Err((e, _)) => return Err(e),
        }
    }
    // Shared mode, as asked or as the last resort in auto mode.
    let mut stream = start_cpal(
        config,
        device,
        supported,
        host,
        Some(OutputMode::Shared),
        engine,
    )?;
    if !refused.is_empty() {
        stream.note = Some(format!(
            "{}, so audio runs in shared mode",
            refused.join("; ")
        ));
    }
    Ok(stream)
}

/// A stream through cpal: shared mode on WASAPI, or another host.
fn start_cpal(
    config: &OutputConfig,
    device: cpal::Device,
    supported: cpal::SupportedStreamConfig,
    host: String,
    mode: Option<OutputMode>,
    mut engine: Engine,
) -> Result<AudioStream, IoError> {
    let sample_rate = supported.sample_rate();
    let channels = supported.channels() as usize;
    let mut stream_config: StreamConfig = supported.config();
    if let Some(frames) = config.buffer_frames {
        stream_config.buffer_size = cpal::BufferSize::Fixed(frames);
    }
    let shared: Arc<EngineShared> = engine.shared().clone();
    let stats = Arc::new(StreamStats::new(host == "Wasapi", sample_rate));
    let cb = Callback {
        channels,
        sample_rate,
        silent: config.silent,
        stats: stats.clone(),
    };
    let lost = Arc::new(AtomicBool::new(false));
    let lost_cb = lost.clone();
    let err_shared = shared.clone();
    let on_error = move |e: cpal::Error| match e.kind() {
        ErrorKind::Xrun => {
            err_shared.dropouts.fetch_add(1, Ordering::Relaxed);
        }
        ErrorKind::DeviceNotAvailable | ErrorKind::StreamInvalidated | ErrorKind::DeviceChanged => {
            lost_cb.store(true, Ordering::Relaxed);
            tracing::warn!("audio device lost: {e}");
        }
        _ => tracing::warn!("audio stream error: {e}"),
    };
    let format = supported.sample_format();
    let stream = match format {
        SampleFormat::F32 => device.build_output_stream::<f32, _, _>(
            stream_config,
            move |out: &mut [f32], _| {
                let started = Instant::now();
                let frames = out.len() / channels.max(1);
                let busy = cb.render(&mut engine, out);
                cb.finish(&engine, frames, started, busy);
            },
            on_error,
            None,
        ),
        SampleFormat::I16 => build_converting::<i16>(&device, &stream_config, engine, cb, on_error),
        SampleFormat::I32 => build_converting::<i32>(&device, &stream_config, engine, cb, on_error),
        SampleFormat::U16 => build_converting::<u16>(&device, &stream_config, engine, cb, on_error),
        other => {
            return Err(IoError::Device(format!(
                "unsupported sample format {other:?}"
            )));
        }
    }
    .map_err(|e| IoError::Device(e.to_string()))?;
    stream.play().map_err(|e| IoError::Device(e.to_string()))?;
    Ok(AudioStream {
        _stream: Backend::Cpal(stream),
        info: DeviceInfo {
            host,
            name: device_name(&device),
            sample_rate,
            channels: channels as u16,
            buffer_frames: config.buffer_frames,
            sample_format: format!("{format:?}"),
            mode,
            period_frames: None,
        },
        note: None,
        lost,
        stats,
    })
}

/// Largest device buffer converted in one go; longer buffers are split.
const SCRATCH_FRAMES: usize = 8_192;

fn build_converting<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut engine: Engine,
    cb: Callback,
    on_error: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let mut scratch = vec![0.0f32; SCRATCH_FRAMES * cb.channels.max(1)];
    device.build_output_stream::<T, _, _>(
        *config,
        move |out: &mut [T], _| {
            let started = Instant::now();
            let mut busy = Duration::ZERO;
            for chunk in out.chunks_mut(scratch.len().max(1)) {
                let n = chunk.len();
                if let Some(tmp) = scratch.get_mut(..n) {
                    busy += cb.render(&mut engine, tmp);
                    for (o, s) in chunk.iter_mut().zip(tmp.iter()) {
                        *o = T::from_sample(*s);
                    }
                }
            }
            cb.finish(&engine, out.len() / cb.channels.max(1), started, busy);
        },
        on_error,
        None,
    )
}

/// Sample formats a device buffer can take, in the order exclusive mode
/// tries them. Windows only, but kept here so it is tested everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pcm {
    F32,
    /// 32-bit integers, of which the device may use only the top 24 bits.
    I32,
    /// Packed 24-bit integers, three bytes each.
    I24,
    I16,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Pcm {
    pub(crate) fn bytes(self) -> usize {
        match self {
            Pcm::F32 | Pcm::I32 => 4,
            Pcm::I24 => 3,
            Pcm::I16 => 2,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Pcm::F32 => "F32",
            Pcm::I32 => "I32",
            Pcm::I24 => "I24",
            Pcm::I16 => "I16",
        }
    }

    /// Writes `src` to `dst` as little-endian samples of this format,
    /// clipping at full scale. `dst` holds `bytes()` per sample.
    #[inline]
    pub(crate) fn write(self, src: &[f32], dst: &mut [u8]) {
        let full = |s: f32, max: f64| (f64::from(s.clamp(-1.0, 1.0)) * max).round();
        match self {
            Pcm::F32 => {
                for (s, d) in src.iter().zip(dst.as_chunks_mut::<4>().0) {
                    *d = s.to_le_bytes();
                }
            }
            Pcm::I32 => {
                for (s, d) in src.iter().zip(dst.as_chunks_mut::<4>().0) {
                    *d = (full(*s, 2_147_483_647.0) as i32).to_le_bytes();
                }
            }
            Pcm::I24 => {
                for (s, d) in src.iter().zip(dst.as_chunks_mut::<3>().0) {
                    let v = (full(*s, 8_388_607.0) as i32).to_le_bytes();
                    d.copy_from_slice(&v[..3]);
                }
            }
            Pcm::I16 => {
                for (s, d) in src.iter().zip(dst.as_chunks_mut::<2>().0) {
                    *d = (full(*s, 32_767.0) as i16).to_le_bytes();
                }
            }
        }
    }
}

/// `frames` at `sample_rate` in the 100-nanosecond units WASAPI counts in.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn frames_to_hns(frames: u32, sample_rate: u32) -> i64 {
    (f64::from(frames) * 10_000_000.0 / f64::from(sample_rate.max(1))).round() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headset() -> DevicePeriods {
        // What Windows reported for the G522 headset.
        DevicePeriods {
            sample_rate: 48_000,
            shared_default: 480,
            shared_min: 480,
            shared_max: 480,
            shared_step: 480,
            exclusive_min: 144,
        }
    }

    fn onboard() -> DevicePeriods {
        // A typical HD Audio driver with low-latency shared mode.
        DevicePeriods {
            sample_rate: 48_000,
            shared_default: 480,
            shared_min: 128,
            shared_max: 480,
            shared_step: 32,
            exclusive_min: 144,
        }
    }

    #[test]
    fn modes_parse_from_their_names() {
        for m in OutputMode::ALL {
            assert_eq!(m.name().parse::<OutputMode>(), Ok(m));
            assert_eq!(m.to_string(), m.name());
        }
        assert_eq!("Low Latency".parse(), Ok(OutputMode::LowLatency));
        assert_eq!("EXCLUSIVE".parse(), Ok(OutputMode::Exclusive));
        assert!("asio".parse::<OutputMode>().is_err());
    }

    #[test]
    fn auto_stays_shared_when_shared_mode_is_enough() {
        assert_eq!(
            auto_mode(None, 48_000, Some(&headset())),
            OutputMode::Shared
        );
        assert_eq!(
            auto_mode(Some(512), 48_000, Some(&headset())),
            OutputMode::Shared
        );
        assert_eq!(
            auto_mode(Some(480), 48_000, Some(&onboard())),
            OutputMode::Shared
        );
        // Nothing known about the device: shared mode.
        assert_eq!(auto_mode(Some(128), 48_000, None), OutputMode::Shared);
    }

    #[test]
    fn auto_prefers_low_latency_shared_mode_when_it_honors_the_buffer() {
        assert_eq!(
            auto_mode(Some(128), 48_000, Some(&onboard())),
            OutputMode::LowLatency
        );
        assert_eq!(
            auto_mode(Some(256), 48_000, Some(&onboard())),
            OutputMode::LowLatency
        );
    }

    #[test]
    fn auto_takes_exclusive_mode_when_only_it_gets_lower() {
        // The headset's shared mode is stuck at 480; exclusive goes to 144.
        assert_eq!(
            auto_mode(Some(128), 48_000, Some(&headset())),
            OutputMode::Exclusive
        );
        // Low-latency shared mode stops at 256 here, exclusive at 144.
        let p = DevicePeriods {
            shared_min: 256,
            ..onboard()
        };
        assert_eq!(
            auto_mode(Some(128), 48_000, Some(&p)),
            OutputMode::Exclusive
        );
        // At another rate than the mixer's only exclusive mode can help.
        assert_eq!(
            auto_mode(Some(128), 44_100, Some(&onboard())),
            OutputMode::Exclusive
        );
    }

    #[test]
    fn auto_keeps_other_programs_playing_when_exclusive_is_no_better() {
        let p = DevicePeriods {
            exclusive_min: 128,
            ..onboard()
        };
        assert_eq!(
            auto_mode(Some(64), 48_000, Some(&p)),
            OutputMode::LowLatency
        );
        let none = DevicePeriods {
            exclusive_min: 480,
            ..headset()
        };
        assert_eq!(
            auto_mode(Some(128), 48_000, Some(&none)),
            OutputMode::Shared
        );
    }

    #[test]
    fn low_latency_periods_never_exceed_the_request_when_they_can_help_it() {
        let p = onboard();
        assert_eq!(p.low_latency_period(128), 128);
        assert_eq!(p.low_latency_period(200), 192);
        assert_eq!(p.low_latency_period(64), 128);
        assert_eq!(p.low_latency_period(4_096), 480);
        assert!(p.has_low_latency());
        assert!(!headset().has_low_latency());
        assert_eq!(p.shortest(), 128);
        assert_eq!(headset().shortest(), 144);
    }

    #[test]
    fn buffer_durations_round_to_the_nearest_100_ns() {
        assert_eq!(frames_to_hns(480, 48_000), 100_000);
        assert_eq!(frames_to_hns(144, 48_000), 30_000);
        assert_eq!(frames_to_hns(128, 48_000), 26_667);
        assert_eq!(frames_to_hns(441, 44_100), 100_000);
    }

    #[test]
    fn samples_convert_to_each_device_format() {
        let src = [0.0f32, 1.0, -1.0, 0.5, 2.0, f32::NAN];
        let mut out = vec![0u8; src.len() * 4];

        Pcm::I16.write(&src, &mut out[..src.len() * 2]);
        let i16s: Vec<i16> = out[..src.len() * 2]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(i16s, [0, 32_767, -32_767, 16_384, 32_767, 0]);

        Pcm::I24.write(&src, &mut out[..src.len() * 3]);
        let i24s: Vec<i32> = out[..src.len() * 3]
            .as_chunks::<3>()
            .0
            .iter()
            .map(|b| i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8)
            .collect();
        assert_eq!(i24s, [0, 8_388_607, -8_388_607, 4_194_304, 8_388_607, 0]);

        Pcm::I32.write(&src, &mut out);
        let i32s: Vec<i32> = out
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        assert_eq!(i32s, [0, i32::MAX, -i32::MAX, 1_073_741_824, i32::MAX, 0]);

        Pcm::F32.write(&src[..5], &mut out[..20]);
        let f32s: Vec<f32> = out[..20]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        assert_eq!(f32s, src[..5]);
        assert_eq!(Pcm::I24.bytes() * 2, 6);
    }
}
