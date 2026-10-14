// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio output through cpal: WASAPI on Windows (ASIO with the `asio`
//! feature), ALSA or PipeWire on Linux, Core Audio on macOS.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

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
}

impl AudioStream {
    /// True once the device has gone away (unplugged, format changed); the
    /// caller should open a new stream.
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }
}

thread_local! {
    static AUDIO_THREAD_READY: Cell<bool> = const { Cell::new(false) };
}

/// Runs the engine for one device buffer, with real-time checks in debug builds.
#[inline]
fn run_engine(
    engine: &mut Engine,
    out: &mut [f32],
    channels: usize,
    frames: usize,
    sample_rate: u32,
) {
    if !AUDIO_THREAD_READY.with(Cell::get) {
        sky_engine::enable_flush_to_zero();
        AUDIO_THREAD_READY.with(|c| c.set(true));
    }
    let started = Instant::now();
    #[cfg(debug_assertions)]
    assert_no_alloc::assert_no_alloc(|| engine.process_interleaved(out, channels));
    #[cfg(not(debug_assertions))]
    engine.process_interleaved(out, channels);
    let budget = frames as f32 / sample_rate.max(1) as f32;
    let load = started.elapsed().as_secs_f32() / budget.max(1e-9);
    let shared = engine.shared();
    shared.load.store(load.to_bits(), Ordering::Relaxed);
    shared.callbacks.fetch_add(1, Ordering::Relaxed);
    if load > 1.0 {
        shared.overruns.fetch_add(1, Ordering::Relaxed);
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
                let frames = out.len() / channels.max(1);
                run_engine(&mut engine, out, channels, frames, sample_rate);
            },
            on_error,
            None,
        ),
        SampleFormat::I16 => build_converting::<i16>(
            &device,
            &stream_config,
            engine,
            channels,
            sample_rate,
            on_error,
        ),
        SampleFormat::I32 => build_converting::<i32>(
            &device,
            &stream_config,
            engine,
            channels,
            sample_rate,
            on_error,
        ),
        SampleFormat::U16 => build_converting::<u16>(
            &device,
            &stream_config,
            engine,
            channels,
            sample_rate,
            on_error,
        ),
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
    })
}

/// Largest device buffer converted in one go; longer buffers are split.
const SCRATCH_FRAMES: usize = 8_192;

fn build_converting<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut engine: Engine,
    channels: usize,
    sample_rate: u32,
    on_error: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let mut scratch = vec![0.0f32; SCRATCH_FRAMES * channels.max(1)];
    device.build_output_stream::<T, _, _>(
        *config,
        move |out: &mut [T], _| {
            for chunk in out.chunks_mut(scratch.len().max(1)) {
                let n = chunk.len();
                if let Some(tmp) = scratch.get_mut(..n) {
                    run_engine(&mut engine, tmp, channels, n / channels.max(1), sample_rate);
                    for (o, s) in chunk.iter_mut().zip(tmp.iter()) {
                        *o = T::from_sample(*s);
                    }
                }
            }
        },
        on_error,
        None,
    )
}
