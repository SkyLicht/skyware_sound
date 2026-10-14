// SPDX-License-Identifier: MIT OR Apache-2.0
//! Error reporting while loading and triggering.

mod common;

use common::*;
use sky_core::CoreError;
use sky_core::manifest::{AudioFile, MANIFEST_FILE_NAME, RandomRange, Selection};
use sky_runtime::{PlayParams, Runtime, RuntimeError};

#[test]
fn missing_audio_file_fails_the_whole_load() {
    let dir = tempfile::tempdir().unwrap();
    let good = write_f32(dir.path(), "good.wav", 48_000, &stereo(vec![0.1; 10]));
    let missing = AudioFile {
        path: "sfx/missing.wav".into(),
        channels: 1,
        sample_rate: 48_000,
        length_samples: 10,
    };
    write_manifest(
        dir.path(),
        vec![
            loop_asset("a_good", good, 0, 10),
            sfx_asset(
                "b_bad",
                vec![missing],
                Selection::Random,
                RandomRange::ZERO,
                RandomRange::ZERO,
            ),
        ],
    );

    let mut rt = Runtime::new(48_000);
    match rt.load_manifest(dir.path()) {
        Err(RuntimeError::Io { path, source }) => {
            assert!(path.ends_with("sfx/missing.wav"), "{}", path.display());
            assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
        }
        other => panic!("expected an Io error, got {other:?}"),
    }
    // All or nothing: the good asset listed before it was not added either.
    assert!(rt.asset("a_good").is_none());
}

#[test]
fn missing_manifest_reports_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = Runtime::new(48_000);
    match rt.load_manifest(dir.path()) {
        Err(RuntimeError::Io { path, .. }) => assert!(path.ends_with(MANIFEST_FILE_NAME)),
        other => panic!("expected an Io error, got {other:?}"),
    }
    let err = rt.load_manifest(dir.path()).unwrap_err();
    assert!(err.to_string().contains(MANIFEST_FILE_NAME), "{err}");
}

#[test]
fn unknown_asset_names_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(dir.path(), vec![]);
    let mut rt = Runtime::new(48_000);
    assert!(rt.load_manifest(dir.path()).unwrap().is_empty());
    match rt.play_by_name("nope", PlayParams::default()) {
        Err(RuntimeError::UnknownAsset(name)) => assert_eq!(name, "nope"),
        other => panic!("expected UnknownAsset, got {other:?}"),
    }
    assert!(rt.asset("nope").is_none());
}

#[test]
fn newer_manifest_versions_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(MANIFEST_FILE_NAME),
        r#"{"schemaVersion": 2, "generator": "Skyware Sound 9.0", "project": "Future", "assets": []}"#,
    )
    .unwrap();
    let mut rt = Runtime::new(48_000);
    match rt.load_manifest(dir.path()) {
        Err(RuntimeError::Manifest(CoreError::UnsupportedManifestVersion { found, supported })) => {
            assert_eq!(found, 2);
            assert_eq!(supported, 1);
        }
        other => panic!("expected an unsupported version error, got {other:?}"),
    }
}

#[test]
fn invalid_json_is_a_manifest_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(MANIFEST_FILE_NAME), "{ not json").unwrap();
    let mut rt = Runtime::new(48_000);
    assert!(matches!(
        rt.load_manifest(dir.path()),
        Err(RuntimeError::Manifest(CoreError::ManifestJson(_)))
    ));
}

#[test]
fn files_that_are_not_wav_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("noise.wav"), b"definitely not a RIFF file").unwrap();
    let file = AudioFile {
        path: "noise.wav".into(),
        channels: 2,
        sample_rate: 48_000,
        length_samples: 100,
    };
    write_manifest(dir.path(), vec![loop_asset("noise", file, 0, 100)]);
    let mut rt = Runtime::new(48_000);
    match rt.load_manifest(dir.path()) {
        Err(RuntimeError::Wav { path, .. }) => assert!(path.ends_with("noise.wav")),
        other => panic!("expected a Wav error, got {other:?}"),
    }
}

#[test]
fn truncated_wav_files_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_f32(dir.path(), "cut.wav", 48_000, &stereo(vec![0.5; 1000]));
    let path = dir.path().join("cut.wav");
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    write_manifest(dir.path(), vec![loop_asset("cut", file, 0, 1000)]);
    let mut rt = Runtime::new(48_000);
    match rt.load_manifest(dir.path()) {
        Err(RuntimeError::Wav { message, .. }) => {
            assert!(message.contains("truncated"), "{message}")
        }
        other => panic!("expected a Wav error, got {other:?}"),
    }
}

#[test]
fn empty_sfx_containers_cannot_be_played() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(
        dir.path(),
        vec![sfx_asset(
            "silence",
            vec![],
            Selection::Shuffle,
            RandomRange::ZERO,
            RandomRange::ZERO,
        )],
    );
    let mut rt = Runtime::new(48_000);
    rt.load_manifest(dir.path()).unwrap();
    assert!(rt.asset_info(rt.asset("silence").unwrap()).is_some());
    match rt.play_by_name("silence", PlayParams::default()) {
        Err(RuntimeError::EmptySfx(name)) => assert_eq!(name, "silence"),
        other => panic!("expected EmptySfx, got {other:?}"),
    }
    assert_eq!(rt.active_voices(), 0);
}

#[test]
fn empty_loop_files_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_f32(dir.path(), "empty.wav", 48_000, &stereo(vec![]));
    write_manifest(dir.path(), vec![loop_asset("empty", file, 0, 0)]);
    let mut rt = Runtime::new(48_000);
    assert!(matches!(
        rt.load_manifest(dir.path()),
        Err(RuntimeError::InvalidLoop { name, .. }) if name == "empty"
    ));
}

#[test]
fn handles_from_another_runtime_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_f32(dir.path(), "a.wav", 48_000, &stereo(vec![0.1; 10]));
    let b = write_f32(dir.path(), "b.wav", 48_000, &stereo(vec![0.1; 10]));
    write_manifest(
        dir.path(),
        vec![loop_asset("a", a, 0, 10), loop_asset("b", b, 0, 10)],
    );
    let mut big = Runtime::new(48_000);
    let handles = big.load_manifest(dir.path()).unwrap();

    let mut small = Runtime::new(48_000);
    assert!(small.asset_info(handles[1]).is_none());
    assert!(matches!(
        small.play(handles[1], PlayParams::default()),
        Err(RuntimeError::InvalidHandle(_))
    ));
}
