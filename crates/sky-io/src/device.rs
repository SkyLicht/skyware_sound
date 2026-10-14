// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio output through cpal: WASAPI on Windows (ASIO with the `asio`
//! feature), ALSA or PipeWire on Linux, Core Audio on macOS.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, FromSample, SampleFormat, SizedSample, StreamConfig};
use sky_engine::{Engine, EngineShared};

use crate::codec::IoError;

/// What the caller asks for. `None` means the device's default.
#[derive(Clone, Debug, Default)]
pub struct OutputConfig {
    /// Part of a device name to look for; the default device otherwise.
    pub device: Option<String>,
    pub sample_rate: Option<u32>,
    /// Requested buffer size in frames, such as 128.
    pub buffer_frames: Option<u32>,
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
    pub buffer_frames: Option<u32>,
    pub sample_format: String,
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
    })
}

/// A running output stream. Dropping it stops playback.
pub struct AudioStream {
    _stream: cpal::Stream,
    pub info: DeviceInfo,
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
pub fn start_output(config: &OutputConfig, mut engine: Engine) -> Result<AudioStream, IoError> {
    let (device, supported, host) = pick(config)?;
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
        _stream: stream,
        info: DeviceInfo {
            host,
            name: device_name(&device),
            sample_rate,
            channels: channels as u16,
            buffer_frames: config.buffer_frames,
            sample_format: format!("{format:?}"),
        },
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
