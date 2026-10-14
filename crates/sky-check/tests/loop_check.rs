// SPDX-License-Identifier: GPL-3.0-or-later
//! The loop half of the system check, headless: a 16-bar loop exported
//! for the game passes every check, and broken exports are caught.

use sky_check::{Expected, LoopOptions, check_loop, check_project, verify_loop_export};
use sky_core::manifest::Category;
use sky_dsp::AudioData;
use sky_game::LoopExport;
use sky_io::WavFormat;
use sky_session::Session;

#[test]
fn a_sixteen_bar_loop_passes_every_check() {
    let dir = tempfile::tempdir().unwrap();
    let (session, _) = Session::new(check_project(16), None);
    let check = check_loop(
        &session,
        &LoopOptions {
            dir: Some(dir.path().to_path_buf()),
            ..LoopOptions::default()
        },
    )
    .unwrap();
    let failed: Vec<_> = check.checks.iter().filter(|c| !c.passed).collect();
    assert!(check.passed, "{failed:#?}");
    assert_eq!(check.bars, 16.0);
    // 16 bars of 4/4 at 120 BPM.
    assert_eq!(check.frames, 32 * 48_000);
    assert!(check.kept);
    assert!(check.file.exists());
    let names: Vec<_> = check.checks.iter().map(|c| c.name).collect();
    assert_eq!(
        names,
        ["length", "manifest", "seam", "clipping", "runtime"],
        "every check ran"
    );
}

#[test]
fn without_a_folder_the_export_is_cleaned_up() {
    let (session, _) = Session::new(check_project(4), None);
    let check = check_loop(&session, &LoopOptions::default()).unwrap();
    assert!(check.passed, "{:#?}", check.checks);
    assert!(!check.kept);
    assert!(!check.file.exists());
}

fn sine(frames: usize, cycles: f64) -> AudioData {
    let s: Vec<f32> = (0..frames)
        .map(|i| (std::f64::consts::TAU * cycles * i as f64 / frames as f64).sin() as f32 * 0.5)
        .collect();
    AudioData {
        sample_rate: 48_000,
        channels: vec![s.clone(), s],
    }
}

fn export(dir: &std::path::Path, audio: &AudioData, loop_end: u64, bars: f64) {
    sky_game::export_loop(
        dir,
        &LoopExport {
            name: "Tone",
            project: "Test",
            audio,
            loop_start: 0,
            loop_end,
            tempo_bpm: Some(120.0),
            time_signature: Some([4, 4]),
            bars: Some(bars),
            category: Category::Music,
            tags: Vec::new(),
            format: WavFormat::Pcm24,
        },
    )
    .unwrap();
}

/// One bar of 4/4 at 120 BPM and 48 kHz is two seconds.
fn one_bar() -> Expected {
    Expected {
        bars: 1.0,
        exact_frames: 96_000.0,
        sample_rate: 48_000,
        tempo_bpm: Some(120.0),
    }
}

#[test]
fn a_click_at_the_loop_point_fails_the_seam_check() {
    let dir = tempfile::tempdir().unwrap();
    // 100.25 cycles: the wave ends on a peak and starts at zero.
    export(dir.path(), &sine(96_000, 100.25), 96_000, 1.0);
    let check = verify_loop_export(dir.path(), "tone", &one_bar()).unwrap();
    let by_name = |n: &str| check.checks.iter().find(|c| c.name == n).unwrap().passed;
    assert!(!check.passed);
    assert!(!by_name("seam"));
    assert!(by_name("length"));
    assert!(by_name("manifest"));
    // The runtime faithfully plays the click; it is the file that is wrong.
    assert!(by_name("runtime"));

    // Whole cycles meet cleanly.
    export(dir.path(), &sine(96_000, 100.0), 96_000, 1.0);
    let check = verify_loop_export(dir.path(), "tone", &one_bar()).unwrap();
    assert!(check.passed, "{:#?}", check.checks);
}

#[test]
fn wrong_lengths_and_loop_points_are_caught() {
    let dir = tempfile::tempdir().unwrap();
    // A sample short of the bar.
    export(dir.path(), &sine(95_999, 100.0), 95_999, 1.0);
    let check = verify_loop_export(dir.path(), "tone", &one_bar()).unwrap();
    assert!(!check.checks[0].passed, "{:?}", check.checks[0]);

    // Loop points that leave out the end of the file.
    export(dir.path(), &sine(96_000, 100.0), 90_000, 1.0);
    let check = verify_loop_export(dir.path(), "tone", &one_bar()).unwrap();
    let manifest = check.checks.iter().find(|c| c.name == "manifest").unwrap();
    assert!(!manifest.passed);
    assert!(manifest.detail.contains("0..96000"), "{}", manifest.detail);

    // A missing asset is an error, not a pass.
    assert!(verify_loop_export(dir.path(), "nope", &one_bar()).is_err());
}
