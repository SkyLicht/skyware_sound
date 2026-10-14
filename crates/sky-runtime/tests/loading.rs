// SPDX-License-Identifier: MIT OR Apache-2.0
//! Loading manifests and decoding every supported WAV format.

mod common;

use common::*;
use sky_core::manifest::{Asset, AudioFile, Manifest, RandomRange, Selection};
use sky_runtime::{PlayParams, Runtime};

/// Renders the first `frames` frames of a loop asset made from `file`.
fn decode_through_runtime(dir: &std::path::Path, file: AudioFile, frames: usize) -> Vec<f32> {
    let length = file.length_samples;
    write_manifest(dir, vec![loop_asset("probe", file, 0, length)]);
    let mut rt = Runtime::new(48_000);
    rt.load_manifest(dir).unwrap();
    rt.play_by_name("probe", PlayParams::default()).unwrap();
    render(&mut rt, frames)
}

#[test]
fn every_integer_bit_depth_decodes() {
    for bits in [8u16, 16, 24, 32] {
        let dir = tempfile::tempdir().unwrap();
        let max = if bits == 32 {
            i32::MAX
        } else {
            (1i32 << (bits - 1)) - 1
        };
        let min = if bits == 32 { i32::MIN } else { -max - 1 };
        let l = vec![min, -1, 0, 1, max, max / 2];
        let r = vec![max, 0, min, min / 3, 2, -2];
        let file = write_int(
            dir.path(),
            "probe.wav",
            48_000,
            bits,
            &[l.clone(), r.clone()],
        );
        let out = decode_through_runtime(dir.path(), file, l.len());

        let full_scale = 2f64.powi(i32::from(bits) - 1);
        for k in 0..l.len() {
            let expected_l = f64::from(l[k]) / full_scale;
            let expected_r = f64::from(r[k]) / full_scale;
            let tolerance = if bits == 32 { 1e-7 } else { 0.0 };
            assert!(
                (f64::from(out[2 * k]) - expected_l).abs() <= tolerance,
                "{bits}-bit left frame {k}: {} vs {expected_l}",
                out[2 * k]
            );
            assert!(
                (f64::from(out[2 * k + 1]) - expected_r).abs() <= tolerance,
                "{bits}-bit right frame {k}: {} vs {expected_r}",
                out[2 * k + 1]
            );
        }
    }
}

#[test]
fn float_files_decode_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let l = vec![-1.0f32, -0.123_456_7, 0.0, 0.5, 1.0, 1.5];
    let r = vec![0.25f32, 0.75, -0.75, 1e-7, -1e-3, 0.0];
    let file = write_f32(dir.path(), "probe.wav", 48_000, &[l.clone(), r.clone()]);
    let out = decode_through_runtime(dir.path(), file, l.len());
    assert_eq!(left(&out), l);
    assert_eq!(right(&out), r);
}

#[test]
fn only_the_first_two_channels_of_multichannel_files_play() {
    let dir = tempfile::tempdir().unwrap();
    let channels = vec![vec![0.1f32; 8], vec![0.2; 8], vec![0.3; 8], vec![0.4; 8]];
    let file = write_f32(dir.path(), "quad.wav", 48_000, &channels);
    write_manifest(dir.path(), vec![loop_asset("quad", file, 0, 8)]);
    let mut rt = Runtime::new(48_000);
    rt.load_manifest(dir.path()).unwrap();
    assert!(
        rt.warnings()
            .iter()
            .any(|w| w.message.contains("only the first two")),
        "{:?}",
        rt.warnings()
    );
    rt.play_by_name("quad", PlayParams::default()).unwrap();
    let out = render(&mut rt, 16);
    assert!(left(&out).iter().all(|&s| s == 0.1));
    assert!(right(&out).iter().all(|&s| s == 0.2));
}

#[test]
fn manifest_mismatches_become_warnings_and_the_file_wins() {
    let dir = tempfile::tempdir().unwrap();
    let mut file = write_f32(dir.path(), "a.wav", 44_100, &stereo(vec![0.5; 500]));
    file.channels = 1;
    file.sample_rate = 48_000;
    file.length_samples = 800;
    write_manifest(
        dir.path(),
        vec![sfx_asset(
            "a",
            vec![file],
            Selection::Random,
            RandomRange::ZERO,
            RandomRange::ZERO,
        )],
    );
    let mut rt = Runtime::new(44_100);
    rt.load_manifest(dir.path()).unwrap();
    let messages: Vec<String> = rt.warnings().iter().map(|w| w.to_string()).collect();
    assert_eq!(messages.len(), 3, "{messages:?}");
    assert!(
        messages.iter().all(|m| m.starts_with("a (")),
        "{messages:?}"
    );

    // The decoded data is trusted: stereo, 500 frames at 44.1 kHz.
    let voice = rt.play_by_name("a", PlayParams::default()).unwrap();
    let out = render(&mut rt, 499);
    assert!(out.iter().all(|&s| s == 0.5));
    render(&mut rt, 1);
    assert!(!rt.is_playing(voice));

    rt.clear_warnings();
    assert!(rt.warnings().is_empty());
}

#[test]
fn small_length_differences_are_tolerated() {
    let dir = tempfile::tempdir().unwrap();
    let mut file = write_f32(dir.path(), "a.wav", 48_000, &stereo(vec![0.5; 5000]));
    file.length_samples = 5004; // within 0.1%
    write_manifest(dir.path(), vec![loop_asset("a", file, 0, 5000)]);
    let mut rt = Runtime::new(48_000);
    rt.load_manifest(dir.path()).unwrap();
    assert!(rt.warnings().is_empty(), "{:?}", rt.warnings());
}

#[test]
fn manifests_load_from_a_folder_a_file_or_memory() {
    let dir = tempfile::tempdir().unwrap();
    let music = write_f32(
        dir.path(),
        "music/theme.wav",
        48_000,
        &stereo(vec![0.5; 100]),
    );
    let hit = write_f32(dir.path(), "sfx/hit.wav", 48_000, &[vec![0.5; 10]]);
    let assets = vec![
        loop_asset("theme", music, 0, 100),
        sfx_asset(
            "hit",
            vec![hit],
            Selection::RoundRobin,
            RandomRange::ZERO,
            RandomRange::ZERO,
        ),
    ];
    write_manifest(dir.path(), assets.clone());

    let mut rt = Runtime::new(48_000);
    let handles = rt.load_manifest(dir.path()).unwrap();
    assert_eq!(handles.len(), 2);
    let theme = rt.asset("theme").unwrap();
    let hit = rt.asset("hit").unwrap();
    assert!(handles.contains(&theme) && handles.contains(&hit));
    assert!(matches!(rt.asset_info(theme), Some(Asset::Loop(l)) if l.loop_end == 100));
    assert!(matches!(rt.asset_info(hit), Some(Asset::Sfx(s)) if s.variations.len() == 1));
    assert!(rt.play(theme, PlayParams::default()).is_ok());

    let mut by_file = Runtime::new(48_000);
    let manifest_path = dir.path().join(sky_core::manifest::MANIFEST_FILE_NAME);
    assert_eq!(by_file.load_manifest(&manifest_path).unwrap().len(), 2);

    let mut manifest = Manifest::new("test", "memory");
    for asset in assets {
        manifest.upsert(asset);
    }
    let mut from_memory = Runtime::new(48_000);
    let handles = from_memory
        .load_manifest_from(&manifest, dir.path())
        .unwrap();
    assert_eq!(handles.len(), 2);
    assert!(
        from_memory
            .play_by_name("hit", PlayParams::default())
            .is_ok()
    );
}

#[test]
fn reloading_replaces_assets_and_keeps_handles() {
    let dir = tempfile::tempdir().unwrap();
    let v1 = write_f32(dir.path(), "v1.wav", 48_000, &stereo(vec![0.25; 100]));
    write_manifest(dir.path(), vec![loop_asset("theme", v1, 0, 100)]);
    let mut rt = Runtime::new(48_000);
    let first = rt.load_manifest(dir.path()).unwrap()[0];
    let old_voice = rt.play(first, PlayParams::default()).unwrap();

    let v2 = write_f32(dir.path(), "v2.wav", 48_000, &stereo(vec![0.5; 50]));
    let other = write_f32(dir.path(), "other.wav", 48_000, &stereo(vec![0.1; 50]));
    write_manifest(
        dir.path(),
        vec![
            loop_asset("theme", v2, 0, 50),
            loop_asset("other", other, 0, 50),
        ],
    );
    let second = rt.load_manifest(dir.path()).unwrap();
    assert!(second.contains(&first), "same name, same handle");
    assert_ne!(rt.asset("other"), Some(first));

    // The playing voice keeps the old audio; new voices get the new audio.
    assert!(render(&mut rt, 10).iter().all(|&s| s == 0.25));
    rt.stop(old_voice, 0.0);
    rt.play(first, PlayParams::default()).unwrap();
    assert!(render(&mut rt, 10).iter().all(|&s| s == 0.5));
}
