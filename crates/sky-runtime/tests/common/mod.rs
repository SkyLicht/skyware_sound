// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fixture helpers shared by the integration tests: WAV files written with
//! hound into a temp dir and a manifest next to them.

// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::path::Path;

use sky_core::AssetId;
use sky_core::manifest::{
    Asset, AudioFile, Category, LoopAsset, MANIFEST_FILE_NAME, Manifest, RandomRange, Selection,
    SfxAsset,
};
use sky_runtime::Runtime;

/// Writes planar `f32` channels as a 32-bit float WAV and describes it.
pub fn write_f32(dir: &Path, rel: &str, sample_rate: u32, channels: &[Vec<f32>]) -> AudioFile {
    let spec = hound::WavSpec {
        channels: channels.len() as u16,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut writer = hound::WavWriter::create(&path, spec).unwrap();
    let frames = channels[0].len();
    for i in 0..frames {
        for ch in channels {
            writer.write_sample(ch[i]).unwrap();
        }
    }
    writer.finalize().unwrap();
    AudioFile {
        path: rel.to_string(),
        channels: channels.len() as u16,
        sample_rate,
        length_samples: frames as u64,
    }
}

/// Writes planar integer channels at the given bit depth.
pub fn write_int(
    dir: &Path,
    rel: &str,
    sample_rate: u32,
    bits: u16,
    channels: &[Vec<i32>],
) -> AudioFile {
    let spec = hound::WavSpec {
        channels: channels.len() as u16,
        sample_rate,
        bits_per_sample: bits,
        sample_format: hound::SampleFormat::Int,
    };
    let path = dir.join(rel);
    let mut writer = hound::WavWriter::create(&path, spec).unwrap();
    let frames = channels[0].len();
    for i in 0..frames {
        for ch in channels {
            if bits == 8 {
                writer.write_sample(ch[i] as i8).unwrap();
            } else {
                writer.write_sample(ch[i]).unwrap();
            }
        }
    }
    writer.finalize().unwrap();
    AudioFile {
        path: rel.to_string(),
        channels: channels.len() as u16,
        sample_rate,
        length_samples: frames as u64,
    }
}

/// A stereo file whose two channels are the same.
pub fn stereo(samples: Vec<f32>) -> Vec<Vec<f32>> {
    vec![samples.clone(), samples]
}

/// A distinct, deterministic value per index, different per channel.
pub fn marker(i: usize, channel: usize) -> f32 {
    let x = (i * 7919 + channel * 104_729) % 20_011;
    x as f32 / 20_011.0 - 0.5
}

/// A stable ID derived from the name (FNV-1a), so fixtures never collide.
fn id_for(name: &str) -> AssetId {
    let hash = name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    AssetId::from_u128(u128::from(hash) + 1)
}

pub fn loop_asset(name: &str, file: AudioFile, loop_start: u64, loop_end: u64) -> Asset {
    Asset::Loop(LoopAsset {
        id: id_for(name),
        name: name.to_string(),
        category: Category::Music,
        file,
        loop_start,
        loop_end,
        tempo_bpm: None,
        time_signature: None,
        bars: None,
        key: None,
        loudness_lufs: None,
        peak_dbfs: None,
        tags: Vec::new(),
    })
}

pub fn sfx_asset(
    name: &str,
    variations: Vec<AudioFile>,
    selection: Selection,
    pitch_semitones: RandomRange,
    gain_db: RandomRange,
) -> Asset {
    Asset::Sfx(SfxAsset {
        id: id_for(name),
        name: name.to_string(),
        category: Category::Sfx,
        variations,
        selection,
        pitch_semitones,
        gain_db,
        loudness_lufs: None,
        peak_dbfs: None,
        tags: Vec::new(),
    })
}

/// Writes `skyware-manifest.json` with these assets into `dir`.
pub fn write_manifest(dir: &Path, assets: Vec<Asset>) {
    let mut manifest = Manifest::new("sky-runtime tests", "Fixtures");
    for asset in assets {
        manifest.upsert(asset);
    }
    std::fs::write(dir.join(MANIFEST_FILE_NAME), manifest.to_json_pretty()).unwrap();
}

/// Renders `frames` frames in blocks of `block` frames.
pub fn render_blocks(runtime: &mut Runtime, frames: usize, block: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; frames * 2];
    for chunk in out.chunks_mut(block * 2) {
        runtime.render(chunk);
    }
    out
}

pub fn render(runtime: &mut Runtime, frames: usize) -> Vec<f32> {
    render_blocks(runtime, frames, 256)
}

/// The left channel of an interleaved buffer.
pub fn left(buffer: &[f32]) -> Vec<f32> {
    buffer.iter().step_by(2).copied().collect()
}

/// The right channel of an interleaved buffer.
pub fn right(buffer: &[f32]) -> Vec<f32> {
    buffer.iter().skip(1).step_by(2).copied().collect()
}
