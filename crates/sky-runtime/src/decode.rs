// SPDX-License-Identifier: MIT OR Apache-2.0
//! WAV decoding into planar `f32` buffers.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use hound::{SampleFormat, WavReader};

use crate::RuntimeError;

/// Decoded audio: one or two channels of `f32` samples at the file's rate.
#[derive(Debug)]
pub(crate) struct AudioData {
    pub left: Box<[f32]>,
    /// `None` for mono files.
    pub right: Option<Box<[f32]>>,
    /// Frames per channel.
    pub frames: usize,
    /// The file's sample rate.
    pub sample_rate: u32,
    /// The number of channels in the file, before dropping any beyond two.
    pub source_channels: u16,
}

/// Decodes a WAV file: 8, 16, 24 or 32-bit integer PCM or 32-bit float, with
/// any number of channels. Only the first two channels are kept.
pub(crate) fn decode_wav(path: &Path) -> Result<AudioData, RuntimeError> {
    let file = File::open(path).map_err(|source| RuntimeError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader = WavReader::new(BufReader::new(file)).map_err(|e| map_hound(path, e))?;
    let spec = reader.spec();
    if spec.channels == 0 {
        return Err(wav_error(path, "the file declares zero channels"));
    }
    if spec.sample_rate == 0 {
        return Err(wav_error(path, "the file declares a sample rate of zero"));
    }

    let source_channels = usize::from(spec.channels);
    let kept = source_channels.min(2);
    let frames = reader.duration() as usize;
    let mut planes: Vec<Vec<f32>> = (0..kept).map(|_| Vec::with_capacity(frames)).collect();

    match spec.sample_format {
        SampleFormat::Float => {
            if spec.bits_per_sample != 32 {
                return Err(wav_error(
                    path,
                    &format!(
                        "{}-bit float samples are not supported; export 32-bit float or integer PCM",
                        spec.bits_per_sample
                    ),
                ));
            }
            for (i, sample) in reader.samples::<f32>().enumerate() {
                let sample = sample.map_err(|e| map_hound(path, e))?;
                if let Some(plane) = planes.get_mut(i % source_channels) {
                    // NaN or infinite samples would poison the whole mix.
                    plane.push(if sample.is_finite() { sample } else { 0.0 });
                }
            }
        }
        SampleFormat::Int => {
            let bits = spec.bits_per_sample;
            if bits == 0 || bits > 32 {
                return Err(wav_error(
                    path,
                    &format!("{bits}-bit integer samples are not supported"),
                ));
            }
            // A power of two, so the scaling is exact for up to 24 bits.
            let scale = (0.5f64).powi(i32::from(bits) - 1) as f32;
            for (i, sample) in reader.samples::<i32>().enumerate() {
                let sample = sample.map_err(|e| map_hound(path, e))?;
                if let Some(plane) = planes.get_mut(i % source_channels) {
                    plane.push(sample as f32 * scale);
                }
            }
        }
    }

    // hound guarantees whole frames, but keep the planes equal regardless.
    let frames = planes.iter().map(Vec::len).min().unwrap_or(0);
    let mut planes = planes.into_iter().map(|mut p| {
        p.truncate(frames);
        p.into_boxed_slice()
    });
    let left = planes.next().unwrap_or_default();
    let right = planes.next();

    Ok(AudioData {
        left,
        right,
        frames,
        sample_rate: spec.sample_rate,
        source_channels: spec.channels,
    })
}

fn wav_error(path: &Path, message: &str) -> RuntimeError {
    RuntimeError::Wav {
        path: path.to_path_buf(),
        message: message.to_string(),
    }
}

fn map_hound(path: &Path, error: hound::Error) -> RuntimeError {
    match error {
        // hound reports a short read as "Failed to read enough bytes."
        hound::Error::IoError(source)
            if source.kind() == std::io::ErrorKind::UnexpectedEof
                || source.to_string().contains("enough bytes") =>
        {
            wav_error(path, "the file is truncated")
        }
        hound::Error::IoError(source) => RuntimeError::Io {
            path: path.to_path_buf(),
            source,
        },
        other => wav_error(path, &other.to_string()),
    }
}
