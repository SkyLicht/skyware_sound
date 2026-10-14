// SPDX-License-Identifier: GPL-3.0-or-later
//! Our own WASAPI output, for the two modes cpal does not have: exclusive
//! mode, and shared mode at a period shorter than the Windows mixer's
//! default (`IAudioClient3`, Windows 10 and later). Both are event driven:
//! Windows signals an event each period, and a thread of ours, registered
//! with MMCSS as "Pro Audio", renders the engine into the device buffer.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Instant;

use sky_engine::Engine;
use windows::Win32::Foundation::{CloseHandle, HANDLE, S_OK, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED, AUDCLNT_E_DEVICE_IN_USE,
    AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED, AUDCLNT_E_UNSUPPORTED_FORMAT,
    AUDCLNT_SHAREMODE_EXCLUSIVE, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, IAudioClient, IAudioClient3,
    IAudioRenderClient, IMMDevice, WAVE_FORMAT_PCM, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    WAVEFORMATEXTENSIBLE_0,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, SetEvent,
    WaitForMultipleObjects,
};
use windows::core::{GUID, Interface, w};

use super::{Callback, OutputMode, Pcm, StreamStats, frames_to_hns};
use crate::codec::IoError;
use crate::periods::DevicePeriods;

const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const SUBTYPE_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
const SUBTYPE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

/// How long without a callback before the device counts as gone.
const SILENCE_MS: u32 = 2_000;

/// What to open.
pub(super) struct Request {
    pub device: Option<String>,
    /// [`OutputMode::LowLatency`] or [`OutputMode::Exclusive`].
    pub mode: OutputMode,
    pub sample_rate: u32,
    pub frames: Option<u32>,
    pub silent: bool,
}

/// A started stream and what it runs at.
pub(super) struct Opened {
    pub stream: WasapiStream,
    pub name: String,
    pub channels: u16,
    pub period_frames: u32,
    pub sample_format: String,
    pub stats: Arc<StreamStats>,
}

/// A failed start, with the engine handed back so the caller can try
/// another mode. `None` only if the audio thread could not be started.
pub(super) type Failed = (IoError, Option<Box<Engine>>);

/// A running stream; dropping it stops the audio thread.
pub(super) struct WasapiStream {
    thread: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    wake: Handle,
}

impl Drop for WasapiStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // SAFETY: the event lives until it is closed below, after the
        // thread that waits on it has ended.
        unsafe {
            let _ = SetEvent(self.wake.0);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // SAFETY: made by CreateEventW in `start` and closed only here.
        unsafe {
            let _ = CloseHandle(self.wake.0);
        }
    }
}

/// An event handle that may cross threads, as Windows handles can.
struct Handle(HANDLE);

// SAFETY: a kernel event handle is valid on any thread of the process.
unsafe impl Send for Handle {}

fn device_error(context: &str, e: windows::core::Error) -> IoError {
    let code = e.code();
    let why = if code == AUDCLNT_E_DEVICE_IN_USE {
        "another program is using the device in exclusive mode".to_string()
    } else if code == AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED {
        "Windows does not allow exclusive mode on this device (Sound settings, the \
         device's properties, Advanced: \"Allow applications to take exclusive control\")"
            .to_string()
    } else if code == AUDCLNT_E_UNSUPPORTED_FORMAT {
        "the device does not take that format".to_string()
    } else {
        e.message().trim().to_string()
    };
    IoError::Device(format!("{context}: {why} ({:#010x})", code.0 as u32))
}

/// Opens the device in `request.mode` on a new audio thread and starts it.
pub(super) fn start(
    request: Request,
    engine: Engine,
    lost: Arc<AtomicBool>,
) -> Result<Opened, Failed> {
    // SAFETY: a plain auto-reset event, closed by `WasapiStream`'s drop or
    // below if the thread does not start.
    let wake = match unsafe { CreateEventW(None, false, false, None) } {
        Ok(h) => Handle(h),
        Err(e) => {
            return Err((
                device_error("cannot make an event", e),
                Some(Box::new(engine)),
            ));
        }
    };
    let wake_thread = Handle(wake.0);
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let (tx, rx) = mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("skyware-audio".into())
        .spawn(move || {
            let wake = wake_thread;
            // SAFETY: COM is set up and torn down on this thread, and every
            // pointer used comes from COM or WASAPI.
            unsafe { run(request, engine, wake.0, &stop_thread, &lost, &tx) }
        });
    let thread = match spawned {
        Ok(t) => t,
        Err(e) => {
            // SAFETY: the event was made above and nothing else holds it.
            unsafe {
                let _ = CloseHandle(wake.0);
            }
            return Err((
                IoError::Device(format!("cannot start the audio thread: {e}")),
                None,
            ));
        }
    };
    let stream = WasapiStream {
        thread: Some(thread),
        stop,
        wake,
    };
    match rx.recv() {
        Ok(Ok(started)) => Ok(Opened {
            stream,
            name: started.name,
            channels: started.channels,
            period_frames: started.period_frames,
            sample_format: started.sample_format,
            stats: started.stats,
        }),
        Ok(Err(failed)) => Err(failed),
        Err(_) => Err((
            IoError::Device("the audio thread stopped while starting".into()),
            None,
        )),
    }
}

/// What the audio thread reports once the device plays.
struct Started {
    name: String,
    channels: u16,
    period_frames: u32,
    sample_format: String,
    stats: Arc<StreamStats>,
}

/// An initialized device client, ready to start.
struct Device {
    client: IAudioClient,
    render: IAudioRenderClient,
    event: HANDLE,
    name: String,
    exclusive: bool,
    /// Frames in the device buffer; in exclusive mode also the period.
    buffer_frames: u32,
    period_frames: u32,
    channels: u16,
    pcm: Pcm,
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: stopping a client twice is harmless, and the event was
        // made for this device only.
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

/// The audio thread: opens the device, reports back, then renders until
/// told to stop or the device goes away.
unsafe fn run(
    request: Request,
    mut engine: Engine,
    wake: HANDLE,
    stop: &AtomicBool,
    lost: &AtomicBool,
    tx: &mpsc::SyncSender<Result<Started, Failed>>,
) {
    unsafe {
        if let Err(e) = CoInitializeEx(None, COINIT_MULTITHREADED).ok() {
            let _ = tx.send(Err((
                device_error("cannot set up COM", e),
                Some(Box::new(engine)),
            )));
            return;
        }
        let mut task = 0u32;
        let mmcss = AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task).ok();
        match open(&request).and_then(|d| d.start().map(|()| d)) {
            Err(e) => {
                let _ = tx.send(Err((e, Some(Box::new(engine)))));
            }
            Ok(device) => {
                let stats = Arc::new(StreamStats::new(!device.exclusive, request.sample_rate));
                let cb = Callback {
                    channels: device.channels as usize,
                    sample_rate: request.sample_rate,
                    silent: request.silent,
                    stats: stats.clone(),
                };
                let started = Started {
                    name: device.name.clone(),
                    channels: device.channels,
                    period_frames: device.period_frames,
                    sample_format: device.pcm.name().to_string(),
                    stats,
                };
                if tx.send(Ok(started)).is_ok() {
                    render(&device, &mut engine, &cb, wake, stop, lost);
                }
                drop(device);
            }
        }
        if let Some(h) = mmcss {
            let _ = AvRevertMmThreadCharacteristics(h);
        }
        CoUninitialize();
    }
}

/// Fills the device buffer each time Windows asks, until `stop`.
unsafe fn render(
    device: &Device,
    engine: &mut Engine,
    cb: &Callback,
    wake: HANDLE,
    stop: &AtomicBool,
    lost: &AtomicBool,
) {
    let channels = device.channels as usize;
    let bytes_per_frame = channels * device.pcm.bytes();
    let mut scratch = vec![0.0f32; device.buffer_frames as usize * channels];
    let handles = [device.event, wake];
    let fail = |what: &str, e: Option<windows::core::Error>| {
        lost.store(true, Ordering::Relaxed);
        match e {
            Some(e) => tracing::warn!("audio device lost: {what}: {}", e.message()),
            None => tracing::warn!("audio device lost: {what}"),
        }
    };
    loop {
        // SAFETY: both handles stay open until this loop has ended.
        let woke = unsafe { WaitForMultipleObjects(&handles, false, SILENCE_MS) };
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if woke == WAIT_TIMEOUT {
            fail("the device stopped asking for audio", None);
            break;
        }
        if woke != WAIT_OBJECT_0 {
            continue;
        }
        let started = Instant::now();
        // Exclusive mode swaps whole buffers; shared mode tops up the room.
        let frames = if device.exclusive {
            device.buffer_frames
        } else {
            // SAFETY: a started client.
            match unsafe { device.client.GetCurrentPadding() } {
                Ok(queued) => device.buffer_frames.saturating_sub(queued),
                Err(e) => {
                    fail("cannot read the buffer", Some(e));
                    break;
                }
            }
        };
        if frames == 0 {
            continue;
        }
        // SAFETY: at most the buffer's size, released below.
        let data = match unsafe { device.render.GetBuffer(frames) } {
            Ok(p) => p,
            Err(e) => {
                fail("cannot get the buffer", Some(e));
                break;
            }
        };
        let Some(out) = scratch.get_mut(..frames as usize * channels) else {
            break;
        };
        let busy = cb.render(engine, out);
        // SAFETY: GetBuffer returned room for `frames` frames of this format.
        let bytes =
            unsafe { std::slice::from_raw_parts_mut(data, frames as usize * bytes_per_frame) };
        device.pcm.write(out, bytes);
        // SAFETY: releases exactly what GetBuffer returned.
        if let Err(e) = unsafe { device.render.ReleaseBuffer(frames, 0) } {
            fail("cannot hand over the buffer", Some(e));
            break;
        }
        cb.finish(engine, frames as usize, started, busy);
    }
}

impl Device {
    /// Starts playback. Exclusive mode first gets a buffer of silence, so
    /// the device has something to play while the first one renders.
    unsafe fn start(&self) -> Result<(), IoError> {
        unsafe {
            if self.exclusive {
                self.render
                    .GetBuffer(self.buffer_frames)
                    .and_then(|_| {
                        self.render
                            .ReleaseBuffer(self.buffer_frames, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)
                    })
                    .map_err(|e| device_error("cannot fill the first buffer", e))?;
            }
            self.client
                .Start()
                .map_err(|e| device_error("cannot start the device", e))
        }
    }
}

/// A format WASAPI allocated, freed when dropped.
struct CoFormat(*mut WAVEFORMATEX);

impl Drop for CoFormat {
    fn drop(&mut self) {
        // SAFETY: allocated by WASAPI with CoTaskMemAlloc.
        unsafe { CoTaskMemFree(Some(self.0 as *const _)) }
    }
}

/// The parts of a WASAPI format the output needs.
struct FormatInfo {
    channels: u16,
    sample_rate: u32,
    channel_mask: u32,
    pcm: Option<Pcm>,
}

/// Reads a format WASAPI handed out (the mixer's format).
unsafe fn format_info(format: *const WAVEFORMATEX) -> FormatInfo {
    unsafe {
        let f = format.read_unaligned();
        let ext = (f.wFormatTag == WAVE_FORMAT_EXTENSIBLE && f.cbSize >= 22)
            .then(|| (format as *const WAVEFORMATEXTENSIBLE).read_unaligned());
        let (float, mask) = match ext {
            Some(x) => {
                let sub = x.SubFormat;
                let mask = x.dwChannelMask;
                if sub == SUBTYPE_FLOAT {
                    (Some(true), mask)
                } else if sub == SUBTYPE_PCM {
                    (Some(false), mask)
                } else {
                    (None, mask)
                }
            }
            None if f.wFormatTag == WAVE_FORMAT_IEEE_FLOAT => (Some(true), 0),
            None if u32::from(f.wFormatTag) == WAVE_FORMAT_PCM => (Some(false), 0),
            None => (None, 0),
        };
        let pcm = match (float, f.wBitsPerSample) {
            (Some(true), 32) => Some(Pcm::F32),
            (Some(false), 32) => Some(Pcm::I32),
            (Some(false), 24) => Some(Pcm::I24),
            (Some(false), 16) => Some(Pcm::I16),
            _ => None,
        };
        FormatInfo {
            channels: f.nChannels,
            sample_rate: f.nSamplesPerSec,
            channel_mask: mask,
            pcm,
        }
    }
}

/// A WAVE_FORMAT_EXTENSIBLE format for exclusive mode.
fn extensible(
    pcm: Pcm,
    valid_bits: u16,
    channels: u16,
    rate: u32,
    mask: u32,
) -> WAVEFORMATEXTENSIBLE {
    let block = channels * pcm.bytes() as u16;
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE,
            nChannels: channels,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * u32::from(block),
            nBlockAlign: block,
            wBitsPerSample: pcm.bytes() as u16 * 8,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: valid_bits,
        },
        dwChannelMask: mask,
        SubFormat: if pcm == Pcm::F32 {
            SUBTYPE_FLOAT
        } else {
            SUBTYPE_PCM
        },
    }
}

/// The usual speaker layout for a channel count.
fn default_mask(channels: u16) -> u32 {
    match channels {
        1 => 0x4,
        2 => 0x3,
        4 => 0x33,
        6 => 0x3F,
        8 => 0x63F,
        _ => 0,
    }
}

unsafe fn open(request: &Request) -> Result<Device, IoError> {
    unsafe {
        let wanted = request.device.as_deref().map(str::to_lowercase);
        let device = crate::periods::wasapi::find(wanted.as_deref()).ok_or_else(|| {
            IoError::Device(match &request.device {
                Some(d) => format!("no output device matching {d:?}"),
                None => "no default output device".into(),
            })
        })?;
        let name = crate::periods::wasapi::friendly_name(&device)
            .unwrap_or_else(|| "Unknown device".into());
        let mut opened = match request.mode {
            OutputMode::Exclusive => open_exclusive(&device, request)?,
            _ => open_low_latency(&device, request)?,
        };
        opened.name = name;
        Ok(opened)
    }
}

/// Shared mode at a short period with `IAudioClient3`. It runs in the
/// mixer's format and at its rate.
unsafe fn open_low_latency(device: &IMMDevice, request: &Request) -> Result<Device, IoError> {
    unsafe {
        let client: IAudioClient3 = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| device_error("low-latency shared mode needs Windows 10 or later", e))?;
        let mix = CoFormat(
            client
                .GetMixFormat()
                .map_err(|e| device_error("cannot read the mixer format", e))?,
        );
        let info = format_info(mix.0);
        let pcm = info.pcm.ok_or_else(|| {
            IoError::Device("the mixer uses a sample format Skyware Sound cannot write".into())
        })?;
        if info.sample_rate != request.sample_rate {
            return Err(IoError::Device(format!(
                "low-latency shared mode runs at the mixer's {} Hz, not {} Hz",
                info.sample_rate, request.sample_rate
            )));
        }
        let (mut default, mut step, mut min, mut max) = (0, 0, 0, 0);
        client
            .GetSharedModeEnginePeriod(mix.0, &mut default, &mut step, &mut min, &mut max)
            .map_err(|e| device_error("cannot read the mixer's periods", e))?;
        let periods = DevicePeriods {
            sample_rate: info.sample_rate,
            shared_default: default,
            shared_min: min,
            shared_max: max,
            shared_step: step,
            exclusive_min: 0,
        };
        if !periods.has_low_latency() {
            return Err(IoError::Device(format!(
                "the driver runs shared mode at {default} frames only"
            )));
        }
        let period = periods.low_latency_period(request.frames.unwrap_or(min));
        client
            .InitializeSharedAudioStream(AUDCLNT_STREAMFLAGS_EVENTCALLBACK, period, mix.0, None)
            .map_err(|e| device_error("cannot open low-latency shared mode", e))?;
        let client: IAudioClient = client
            .cast()
            .map_err(|e| device_error("cannot open low-latency shared mode", e))?;
        finish(client, false, period, info.channels, pcm)
    }
}

/// Exclusive mode at the requested period, or the device's shortest if
/// the request is below that; at the device's default period without one.
unsafe fn open_exclusive(device: &IMMDevice, request: &Request) -> Result<Device, IoError> {
    unsafe {
        let activate = || -> Result<IAudioClient, IoError> {
            device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| device_error("cannot open the device", e))
        };
        let client = activate()?;
        let mix = CoFormat(
            client
                .GetMixFormat()
                .map_err(|e| device_error("cannot read the mixer format", e))?,
        );
        let info = format_info(mix.0);
        drop(mix);
        let channels = info.channels;
        let mask = if info.channel_mask != 0 {
            info.channel_mask
        } else {
            default_mask(channels)
        };
        let rate = request.sample_rate;
        // Float first, so the engine's samples go through unchanged, then
        // the widest integers.
        let candidates = [
            (Pcm::F32, 32),
            (Pcm::I32, 32),
            (Pcm::I32, 24),
            (Pcm::I24, 24),
            (Pcm::I16, 16),
        ];
        let (pcm, format) = candidates
            .iter()
            .map(|&(pcm, bits)| (pcm, extensible(pcm, bits, channels, rate, mask)))
            .find(|(_, f)| {
                client.IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, &f.Format, None) == S_OK
            })
            .ok_or_else(|| {
                IoError::Device(format!(
                    "the device takes no {channels}-channel format at {rate} Hz in exclusive mode \
                     (tried 32-bit float and 32-, 24- and 16-bit integers)"
                ))
            })?;
        let (mut default_hns, mut min_hns) = (0i64, 0i64);
        client
            .GetDevicePeriod(Some(&mut default_hns), Some(&mut min_hns))
            .map_err(|e| device_error("cannot read the device period", e))?;
        let mut hns = request
            .frames
            .map_or(default_hns, |f| frames_to_hns(f, rate).max(min_hns));
        let init = |client: &IAudioClient, hns: i64| {
            client.Initialize(
                AUDCLNT_SHAREMODE_EXCLUSIVE,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                hns,
                hns,
                &format.Format,
                None,
            )
        };
        let client = match init(&client, hns) {
            Ok(()) => client,
            Err(e) if e.code() == AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED => {
                // The device wants a period it can split evenly: ask again
                // with the size it rounded to, on a fresh client.
                let frames = client
                    .GetBufferSize()
                    .map_err(|e| device_error("cannot read the buffer size", e))?;
                hns = frames_to_hns(frames, rate);
                drop(client);
                let client = activate()?;
                init(&client, hns).map_err(|e| device_error("cannot open exclusive mode", e))?;
                client
            }
            Err(e) => return Err(device_error("cannot open exclusive mode", e)),
        };
        let frames = client
            .GetBufferSize()
            .map_err(|e| device_error("cannot read the buffer size", e))?;
        finish(client, true, frames, channels, pcm)
    }
}

/// Connects the event and the render client to an initialized client.
unsafe fn finish(
    client: IAudioClient,
    exclusive: bool,
    period_frames: u32,
    channels: u16,
    pcm: Pcm,
) -> Result<Device, IoError> {
    unsafe {
        let event = CreateEventW(None, false, false, None)
            .map_err(|e| device_error("cannot make an event", e))?;
        let ready = client
            .SetEventHandle(event)
            .and_then(|()| client.GetBufferSize())
            .and_then(|size| Ok((size, client.GetService::<IAudioRenderClient>()?)));
        match ready {
            Ok((buffer_frames, render)) => Ok(Device {
                client,
                render,
                event,
                name: String::new(),
                exclusive,
                buffer_frames,
                period_frames,
                channels,
                pcm,
            }),
            Err(e) => {
                let _ = CloseHandle(event);
                Err(device_error("cannot connect to the device", e))
            }
        }
    }
}
