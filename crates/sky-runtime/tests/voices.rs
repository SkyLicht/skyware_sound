// SPDX-License-Identifier: MIT OR Apache-2.0
//! Voices: fades, stopping, stale ids, gain, pan and voice stealing.

mod common;

use std::path::Path;

use common::*;
use sky_core::manifest::{RandomRange, Selection};
use sky_runtime::{PlayParams, Runtime, RuntimeError};

/// A loop and a few SFX made of constant levels, so gains are easy to read.
fn fixture(dir: &Path) {
    let assets = vec![
        loop_asset(
            "pad",
            write_f32(dir, "pad.wav", 48_000, &stereo(vec![0.5; 1000])),
            0,
            1000,
        ),
        loop_asset(
            "drone",
            write_f32(dir, "drone.wav", 48_000, &stereo(vec![0.125; 500])),
            0,
            500,
        ),
        sfx_asset(
            "long",
            vec![write_f32(
                dir,
                "long.wav",
                48_000,
                &stereo(vec![0.25; 48_000]),
            )],
            Selection::Random,
            RandomRange::ZERO,
            RandomRange::ZERO,
        ),
        sfx_asset(
            "short",
            vec![write_f32(dir, "short.wav", 48_000, &stereo(vec![0.25; 10]))],
            Selection::Random,
            RandomRange::ZERO,
            RandomRange::ZERO,
        ),
        sfx_asset(
            "mono",
            vec![write_f32(dir, "mono.wav", 48_000, &[vec![0.5; 100]])],
            Selection::Random,
            RandomRange::ZERO,
            RandomRange::ZERO,
        ),
        sfx_asset(
            "wide",
            vec![write_f32(
                dir,
                "wide.wav",
                48_000,
                &[vec![0.5; 100], vec![-0.25; 100]],
            )],
            Selection::Random,
            RandomRange::ZERO,
            RandomRange::ZERO,
        ),
        sfx_asset(
            "rr",
            vec![
                write_f32(dir, "rr0.wav", 48_000, &stereo(vec![0.1; 10])),
                write_f32(dir, "rr1.wav", 48_000, &stereo(vec![0.2; 10])),
            ],
            Selection::RoundRobin,
            RandomRange::ZERO,
            RandomRange::ZERO,
        ),
    ];
    write_manifest(dir, assets);
}

fn runtime(dir: &Path, max_voices: usize) -> Runtime {
    fixture(dir);
    let mut rt = Runtime::with_max_voices(48_000, max_voices);
    rt.load_manifest(dir).unwrap();
    rt
}

fn play(rt: &mut Runtime, name: &str) -> sky_runtime::VoiceId {
    rt.play_by_name(name, PlayParams::default()).unwrap()
}

fn close(a: f32, b: f32, tolerance: f32) -> bool {
    (a - b).abs() <= tolerance
}

#[test]
fn stop_with_fade_frees_the_voice_after_the_fade() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = runtime(dir.path(), 8);
    let voice = play(&mut rt, "pad");
    assert!(render(&mut rt, 10).iter().all(|&s| s == 0.5));

    rt.stop(voice, 10.0); // 480 frames at 48 kHz
    let out = left(&render(&mut rt, 479));
    assert!(rt.is_playing(voice), "still fading out");
    for (k, &value) in out.iter().enumerate() {
        let expected = 0.5 * (1.0 - (k + 1) as f32 / 480.0);
        assert!(
            close(value, expected, 1e-5),
            "frame {k}: {value} vs {expected}"
        );
    }
    assert!(out.windows(2).all(|w| w[1] < w[0]));

    let last = render(&mut rt, 1);
    assert!(close(last[0], 0.0, 1e-6));
    assert!(!rt.is_playing(voice));
    assert_eq!(rt.active_voices(), 0);
    assert!(render(&mut rt, 64).iter().all(|&s| s == 0.0));
}

#[test]
fn stop_without_fade_is_immediate() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = runtime(dir.path(), 8);
    let voice = play(&mut rt, "pad");
    render(&mut rt, 5);
    rt.stop(voice, 0.0);
    assert!(!rt.is_playing(voice));
    assert_eq!(rt.active_voices(), 0);
    assert!(render(&mut rt, 16).iter().all(|&s| s == 0.0));
}

#[test]
fn stop_all_fades_every_voice() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = runtime(dir.path(), 8);
    let a = play(&mut rt, "pad");
    let b = play(&mut rt, "long");
    rt.stop_all(1.0); // 48 frames
    render(&mut rt, 47);
    assert!(rt.is_playing(a) && rt.is_playing(b));
    render(&mut rt, 1);
    assert_eq!(rt.active_voices(), 0);
}

#[test]
fn stale_voice_ids_are_harmless() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = runtime(dir.path(), 1);
    let old = play(&mut rt, "short");
    render(&mut rt, 20); // "short" is 10 frames long
    assert!(!rt.is_playing(old));

    // The new voice reuses the only slot.
    let new = play(&mut rt, "pad");
    assert_ne!(old, new);
    assert!(!rt.is_playing(old));
    rt.stop(old, 0.0);
    rt.set_voice_gain(old, f32::NEG_INFINITY, 0.0);
    assert!(rt.is_playing(new));
    assert!(render(&mut rt, 32).iter().all(|&s| s == 0.5));

    // Stopping twice is fine too.
    rt.stop(new, 0.0);
    rt.stop(new, 0.0);
    assert_eq!(rt.active_voices(), 0);
}

#[test]
fn fade_in_ramps_linearly() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = runtime(dir.path(), 8);
    let params = PlayParams {
        fade_in_ms: 1.0, // 48 frames
        ..PlayParams::default()
    };
    rt.play_by_name("pad", params).unwrap();
    let out = left(&render(&mut rt, 100));
    for (k, &value) in out.iter().enumerate() {
        let expected = 0.5 * ((k + 1) as f32 / 48.0).min(1.0);
        assert!(
            close(value, expected, 1e-5),
            "frame {k}: {value} vs {expected}"
        );
    }
}

#[test]
fn voice_gain_ramps_to_the_new_level() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = runtime(dir.path(), 8);
    let voice = play(&mut rt, "pad");
    render(&mut rt, 4);
    rt.set_voice_gain(voice, -6.0206, 1.0);
    let out = left(&render(&mut rt, 96));
    assert!(close(out[0], 0.5 - 0.25 / 48.0, 1e-4), "{}", out[0]);
    assert!(close(out[23], 0.375, 1e-4), "{}", out[23]);
    assert!(out[47..].iter().all(|&s| close(s, 0.25, 1e-4)));

    // Immediate changes work too.
    rt.set_voice_gain(voice, 0.0, 0.0);
    assert!(render(&mut rt, 8).iter().all(|&s| s == 0.5));
}

#[test]
fn master_gain_is_smoothed_and_applied_last() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = runtime(dir.path(), 8);
    play(&mut rt, "pad");
    play(&mut rt, "drone");
    // Voices are summed.
    assert!(render(&mut rt, 8).iter().all(|&s| s == 0.625));

    rt.set_master_gain(-6.0206);
    let out = left(&render(&mut rt, 960));
    assert!(
        out[0] < 0.625 && out[0] > 0.6,
        "the change is ramped: {}",
        out[0]
    );
    assert!(out[480..].iter().all(|&s| close(s, 0.3125, 1e-4)));
}

#[test]
fn mono_sources_use_equal_power_pan() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = runtime(dir.path(), 8);
    let at = |rt: &mut Runtime, pan: f32| {
        let params = PlayParams {
            pan,
            ..PlayParams::default()
        };
        let voice = rt.play_by_name("mono", params).unwrap();
        let mut frame = [0.0f32; 2];
        rt.render(&mut frame);
        rt.stop(voice, 0.0);
        (frame[0], frame[1])
    };
    let (l, r) = at(&mut rt, 0.0);
    assert!(close(l, 0.5 * std::f32::consts::FRAC_1_SQRT_2, 1e-6) && l == r);
    assert_eq!(at(&mut rt, -1.0), (0.5, 0.0));
    assert_eq!(at(&mut rt, 1.0), (0.0, 0.5));
    let (l, r) = at(&mut rt, 0.3);
    assert!(close(l * l + r * r, 0.25, 1e-6), "constant power");
    assert!(r > l);
}

#[test]
fn stereo_sources_use_balance_pan() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = runtime(dir.path(), 8);
    let at = |rt: &mut Runtime, pan: f32| {
        let params = PlayParams {
            pan,
            ..PlayParams::default()
        };
        let voice = rt.play_by_name("wide", params).unwrap();
        let mut frame = [0.0f32; 2];
        rt.render(&mut frame);
        rt.stop(voice, 0.0);
        (frame[0], frame[1])
    };
    assert_eq!(at(&mut rt, 0.0), (0.5, -0.25));
    assert_eq!(at(&mut rt, 1.0), (0.0, -0.25));
    assert_eq!(at(&mut rt, -1.0), (0.5, 0.0));
    let (l, r) = at(&mut rt, 0.5);
    assert!(close(l, 0.5 * std::f32::consts::FRAC_1_SQRT_2, 1e-6));
    assert_eq!(r, -0.25);
}

#[test]
fn the_oldest_sfx_voice_is_stolen() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = runtime(dir.path(), 3);
    let music = play(&mut rt, "pad");
    let a = play(&mut rt, "long");
    let b = play(&mut rt, "long");
    assert_eq!(rt.active_voices(), 3);

    let c = play(&mut rt, "long");
    assert!(!rt.is_playing(a), "the oldest SFX voice is stolen");
    assert!(rt.is_playing(music) && rt.is_playing(b) && rt.is_playing(c));

    let d = play(&mut rt, "long");
    assert!(!rt.is_playing(b));
    assert!(rt.is_playing(music) && rt.is_playing(c) && rt.is_playing(d));
    assert_eq!(rt.active_voices(), 3);

    // A stolen voice's id is stale and harmless.
    rt.stop(a, 0.0);
    assert_eq!(rt.active_voices(), 3);

    // Loops may steal SFX voices too.
    let e = play(&mut rt, "drone");
    assert!(!rt.is_playing(c) && rt.is_playing(e));
}

#[test]
fn loops_are_never_stolen() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = runtime(dir.path(), 2);
    let a = play(&mut rt, "pad");
    let b = play(&mut rt, "drone");
    assert!(matches!(
        rt.play_by_name("long", PlayParams::default()),
        Err(RuntimeError::NoFreeVoice)
    ));
    assert!(matches!(
        rt.play_by_name("pad", PlayParams::default()),
        Err(RuntimeError::NoFreeVoice)
    ));
    assert!(rt.is_playing(a) && rt.is_playing(b));

    rt.stop(a, 0.0);
    assert!(rt.play_by_name("long", PlayParams::default()).is_ok());
}

#[test]
fn a_failed_play_does_not_advance_selection() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = runtime(dir.path(), 1);
    let music = play(&mut rt, "pad");
    for _ in 0..3 {
        assert!(rt.play_by_name("rr", PlayParams::default()).is_err());
    }
    rt.stop(music, 0.0);
    play(&mut rt, "rr");
    let mut frame = [0.0f32; 2];
    rt.render(&mut frame);
    assert_eq!(
        frame[0], 0.1,
        "round robin still starts at the first variation"
    );
}

#[test]
fn a_runtime_without_voices_refuses_to_play() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = runtime(dir.path(), 0);
    assert!(matches!(
        rt.play_by_name("short", PlayParams::default()),
        Err(RuntimeError::NoFreeVoice)
    ));
    let mut out = [1.0f32; 8];
    rt.render(&mut out);
    assert_eq!(out, [0.0; 8]);
}

#[test]
fn render_overwrites_and_tolerates_odd_lengths() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = runtime(dir.path(), 4);
    play(&mut rt, "pad");
    let mut out = [9.0f32; 7];
    rt.render(&mut out);
    assert_eq!(out, [0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.0]);
    rt.render(&mut []);
}

#[test]
fn hostile_parameters_do_not_poison_the_mix() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = runtime(dir.path(), 4);
    let params = PlayParams {
        gain_db: f32::NAN,
        pitch_semitones: f32::INFINITY,
        pan: f32::NAN,
        fade_in_ms: f32::NAN,
    };
    rt.play_by_name("pad", params).unwrap();
    let huge = PlayParams {
        gain_db: f32::INFINITY,
        ..PlayParams::default()
    };
    rt.play_by_name("drone", huge).unwrap();
    let hostile = render(&mut rt, 256);
    assert!(hostile.iter().all(|s| s.is_finite()));
    // Values that are not numbers count as the default, 0 dB and no pitch.
    let mut plain = runtime(dir.path(), 4);
    plain
        .play_by_name(
            "pad",
            PlayParams {
                pitch_semitones: 0.0,
                ..PlayParams::default()
            },
        )
        .unwrap();
    plain.play_by_name("drone", PlayParams::default()).unwrap();
    assert_eq!(hostile, render(&mut plain, 256));
}
