// SPDX-License-Identifier: GPL-3.0-or-later
//! Decoding audio files and writing WAV.

use std::fs::File;
use std::path::Path;

use sky_dsp::AudioData;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IoError {
    #[error("cannot open {path}: {message}")]
    Open { path: String, message: String },
    #[error("cannot decode {path}: {message}")]
    Decode { path: String, message: String },
    #[error("cannot write {path}: {message}")]
    Write { path: String, message: String },
    #[error("audio device: {0}")]
    Device(String),
}

/// Reads WAV, FLAC, Ogg Vorbis or MP3 into planar f32.
pub fn read_audio(path: &Path) -> Result<AudioData, IoError> {
    let is_wav = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("wav"));
    if is_wav && let Ok(audio) = read_wav(path) {
        return Ok(audio);
    }
    read_with_symphonia(path)
}

fn read_wav(path: &Path) -> Result<AudioData, IoError> {
    let err = |e: hound::Error| IoError::Decode {
        path: path.display().to_string(),
        message: e.to_string(),
    };
    let reader = hound::WavReader::open(path).map_err(err)?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(err)?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32;
            reader
                .into_samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()
                .map_err(err)?
        }
    };
    Ok(AudioData::from_interleaved(
        &samples,
        channels,
        spec.sample_rate,
    ))
}

fn read_with_symphonia(path: &Path) -> Result<AudioData, IoError> {
    let display = path.display().to_string();
    let file = File::open(path).map_err(|e| IoError::Open {
        path: display.clone(),
        message: e.to_string(),
    })?;
    let derr = |m: String| IoError::Decode {
        path: display.clone(),
        message: m,
    };
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| derr(e.to_string()))?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| derr("no audio track".into()))?;
    let track_id = track.id;
    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| derr("unknown sample rate".into()))?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| derr(e.to_string()))?;
    let mut interleaved: Vec<f32> = Vec::new();
    let mut channels = 0usize;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(derr(e.to_string())),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                channels = spec.channels.count();
                let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                buf.copy_interleaved_ref(decoded);
                interleaved.extend_from_slice(buf.samples());
            }
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(derr(e.to_string())),
        }
    }
    if channels == 0 {
        return Err(derr("the file holds no audio".into()));
    }
    Ok(AudioData::from_interleaved(
        &interleaved,
        channels,
        sample_rate,
    ))
}

/// WAV sample formats for export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WavFormat {
    Pcm16,
    Pcm24,
    Float32,
}

impl WavFormat {
    pub fn from_bits(bits: u16) -> Option<Self> {
        match bits {
            16 => Some(WavFormat::Pcm16),
            24 => Some(WavFormat::Pcm24),
            32 => Some(WavFormat::Float32),
            _ => None,
        }
    }
}

/// Writes planar audio as an interleaved WAV file. Integer formats get
/// triangular dither when `dither` is set, and samples are clipped to full scale.
pub fn write_wav(
    path: &Path,
    audio: &AudioData,
    format: WavFormat,
    dither: bool,
) -> Result<(), IoError> {
    let werr = |e: hound::Error| IoError::Write {
        path: path.display().to_string(),
        message: e.to_string(),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| IoError::Write {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
    }
    let (bits, sample_format) = match format {
        WavFormat::Pcm16 => (16, hound::SampleFormat::Int),
        WavFormat::Pcm24 => (24, hound::SampleFormat::Int),
        WavFormat::Float32 => (32, hound::SampleFormat::Float),
    };
    let spec = hound::WavSpec {
        channels: audio.channel_count() as u16,
        sample_rate: audio.sample_rate,
        bits_per_sample: bits,
        sample_format,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(werr)?;
    let mut rng = sky_dsp::util::Rng::new(0xD1);
    let frames = audio.frames();
    for i in 0..frames {
        for ch in &audio.channels {
            let s = ch.get(i).copied().unwrap_or(0.0);
            match format {
                WavFormat::Float32 => writer.write_sample(s).map_err(werr)?,
                WavFormat::Pcm16 | WavFormat::Pcm24 => {
                    let max = ((1i64 << (bits - 1)) - 1) as f32;
                    let tpdf = if dither {
                        (rng.bipolar() + rng.bipolar()) * 0.5
                    } else {
                        0.0
                    };
                    let v = (s * max + tpdf).round().clamp(-max - 1.0, max) as i32;
                    writer.write_sample(v).map_err(werr)?;
                }
            }
        }
    }
    writer.finalize().map_err(werr)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone() -> AudioData {
        let n = 4_800;
        AudioData {
            sample_rate: 48_000,
            channels: vec![
                (0..n).map(|i| (i as f32 * 0.05).sin() * 0.5).collect(),
                (0..n).map(|i| (i as f32 * 0.03).cos() * 0.25).collect(),
            ],
        }
    }

    #[test]
    fn wav_round_trips_in_every_format() {
        let dir = tempfile::tempdir().unwrap();
        let audio = tone();
        for (format, tolerance) in [
            (WavFormat::Float32, 0.0),
            (WavFormat::Pcm24, 1e-6),
            (WavFormat::Pcm16, 1e-4),
        ] {
            let path = dir.path().join(format!("{format:?}.wav"));
            write_wav(&path, &audio, format, false).unwrap();
            let back = read_audio(&path).unwrap();
            assert_eq!(back.frames(), audio.frames());
            assert_eq!(back.channel_count(), 2);
            for (a, b) in audio
                .channels
                .iter()
                .flatten()
                .zip(back.channels.iter().flatten())
            {
                assert!((a - b).abs() <= tolerance, "{format:?}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn missing_files_report_the_path() {
        let e = read_audio(Path::new("/no/such/file.flac"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("/no/such/file.flac"), "{e}");
    }
}
