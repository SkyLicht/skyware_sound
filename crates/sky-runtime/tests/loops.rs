// SPDX-License-Identifier: MIT OR Apache-2.0
//! Loop playback: seams, intros and resampling.

mod common;

use common::*;
use sky_runtime::{PlayParams, Runtime};

fn markers(len: usize, channel: usize) -> Vec<f32> {
    (0..len).map(|i| marker(i, channel)).collect()
}

fn same_bits(a: f32, b: f32) -> bool {
    a.to_bits() == b.to_bits()
}

#[test]
fn loop_seam_is_sample_exact() {
    let dir = tempfile::tempdir().unwrap();
    let len = 1000;
    let (l, r) = (markers(len, 0), markers(len, 1));
    let file = write_f32(
        dir.path(),
        "music/loop.wav",
        48_000,
        &[l.clone(), r.clone()],
    );
    write_manifest(dir.path(), vec![loop_asset("music", file, 0, len as u64)]);

    let mut rt = Runtime::new(48_000);
    rt.load_manifest(dir.path()).unwrap();
    let voice = rt.play_by_name("music", PlayParams::default()).unwrap();
    // An odd block size so seams fall in the middle of render calls.
    let frames = len * 3 + 517;
    let out = render_blocks(&mut rt, frames, 37);

    for k in 0..frames {
        assert!(
            same_bits(out[2 * k], l[k % len]),
            "left differs at frame {k}"
        );
        assert!(
            same_bits(out[2 * k + 1], r[k % len]),
            "right differs at frame {k}"
        );
    }
    assert!(rt.is_playing(voice), "loops never end by themselves");
}

#[test]
fn mono_ramp_loop_repeats_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let len = 640usize;
    let ramp: Vec<i32> = (0..len as i32).map(|i| i * 50 - 16_000).collect();
    let file = write_int(
        dir.path(),
        "ramp.wav",
        48_000,
        16,
        std::slice::from_ref(&ramp),
    );
    write_manifest(dir.path(), vec![loop_asset("ramp", file, 0, len as u64)]);

    let mut rt = Runtime::new(48_000);
    rt.load_manifest(dir.path()).unwrap();
    rt.play_by_name("ramp", PlayParams::default()).unwrap();
    let out = render_blocks(&mut rt, len * 4, 100);

    // Mono at center is -3 dB on both sides.
    let center = std::f32::consts::FRAC_1_SQRT_2;
    for k in 0..len * 4 {
        let expected = ramp[k % len] as f32 / 32_768.0 * center;
        assert!((out[2 * k] - expected).abs() < 1e-6, "frame {k}");
        assert_eq!(out[2 * k], out[2 * k + 1]);
    }
}

#[test]
fn intro_plays_once_then_the_body_repeats() {
    let dir = tempfile::tempdir().unwrap();
    // 300 frames of intro, a 700-frame body and a 200-frame tail that the
    // loop never reaches.
    let (len, start, end) = (1200usize, 300usize, 1000usize);
    let (l, r) = (markers(len, 0), markers(len, 1));
    let file = write_f32(
        dir.path(),
        "intro_loop.wav",
        48_000,
        &[l.clone(), r.clone()],
    );
    write_manifest(
        dir.path(),
        vec![loop_asset("intro_loop", file, start as u64, end as u64)],
    );

    let mut rt = Runtime::new(48_000);
    rt.load_manifest(dir.path()).unwrap();
    rt.play_by_name("intro_loop", PlayParams::default())
        .unwrap();
    let frames = end + (end - start) * 4 + 123;
    let out = render_blocks(&mut rt, frames, 64);

    for k in 0..frames {
        let src = if k < end {
            k
        } else {
            start + (k - end) % (end - start)
        };
        assert!(same_bits(out[2 * k], l[src]), "left differs at frame {k}");
        assert!(
            same_bits(out[2 * k + 1], r[src]),
            "right differs at frame {k}"
        );
    }
}

#[test]
fn octave_up_and_down_stay_on_the_sample_grid() {
    let dir = tempfile::tempdir().unwrap();
    let len = 500;
    let l = markers(len, 0);
    let file = write_f32(dir.path(), "loop.wav", 48_000, &stereo(l.clone()));
    write_manifest(dir.path(), vec![loop_asset("loop", file, 0, len as u64)]);

    let mut rt = Runtime::new(48_000);
    rt.load_manifest(dir.path()).unwrap();
    let up = PlayParams {
        pitch_semitones: 12.0,
        ..PlayParams::default()
    };
    let voice = rt.play_by_name("loop", up).unwrap();
    let out = left(&render(&mut rt, len * 3));
    for (k, &value) in out.iter().enumerate() {
        assert!(same_bits(value, l[(2 * k) % len]), "octave up, frame {k}");
    }
    rt.stop(voice, 0.0);

    let down = PlayParams {
        pitch_semitones: -12.0,
        ..PlayParams::default()
    };
    rt.play_by_name("loop", down).unwrap();
    let out = left(&render(&mut rt, len * 3));
    for k in (0..out.len()).step_by(2) {
        assert!(
            same_bits(out[k], l[(k / 2) % len]),
            "octave down, frame {k}"
        );
    }
}

#[test]
fn resampled_loop_has_the_expected_length_and_no_drift() {
    let dir = tempfile::tempdir().unwrap();
    let len = 1000usize;
    // A sawtooth: each iteration starts with a sharp drop.
    let saw: Vec<f32> = (0..len).map(|i| i as f32 / len as f32).collect();
    let file = write_f32(dir.path(), "saw.wav", 44_100, &stereo(saw));
    write_manifest(dir.path(), vec![loop_asset("saw", file, 0, len as u64)]);

    let mut rt = Runtime::new(48_000);
    rt.load_manifest(dir.path()).unwrap();
    rt.play_by_name("saw", PlayParams::default()).unwrap();
    let laps = 12;
    let frames_per_lap = len as f64 * 48_000.0 / 44_100.0; // 1088.435...
    let out = left(&render(
        &mut rt,
        (frames_per_lap * (laps as f64 + 0.5)) as usize,
    ));

    // Frames where the signal enters the bottom of the sawtooth.
    let wraps: Vec<usize> = (1..out.len())
        .filter(|&k| out[k] < 0.1 && out[k - 1] >= 0.1)
        .collect();
    assert_eq!(wraps.len(), laps, "{wraps:?}");
    for (m, &k) in wraps.iter().enumerate() {
        let expected = ((m + 1) as f64 * frames_per_lap).ceil();
        assert!(
            (k as f64 - expected).abs() <= 1.0,
            "iteration {} wrapped at frame {k}, expected about {expected}",
            m + 1
        );
    }
}

#[test]
fn resampled_sine_loop_is_seamless() {
    let dir = tempfile::tempdir().unwrap();
    let len = 1000usize;
    let cycles = 5.0;
    let tau = std::f64::consts::TAU;
    let sine: Vec<f32> = (0..len)
        .map(|i| (tau * cycles * i as f64 / len as f64).sin() as f32 * 0.8)
        .collect();
    let file = write_f32(dir.path(), "sine.wav", 44_100, &stereo(sine));
    write_manifest(dir.path(), vec![loop_asset("sine", file, 0, len as u64)]);

    let mut rt = Runtime::new(48_000);
    rt.load_manifest(dir.path()).unwrap();
    rt.play_by_name("sine", PlayParams::default()).unwrap();
    let out = left(&render(&mut rt, 48_000 / 4));

    // Ten iterations with no click at any seam and no phase drift.
    let ratio = 44_100.0 / 48_000.0;
    for (k, &value) in out.iter().enumerate().skip(2) {
        let expected = (tau * cycles * k as f64 * ratio / len as f64).sin() * 0.8;
        assert!(
            (f64::from(value) - expected).abs() < 1e-3,
            "frame {k}: {value} vs {expected}"
        );
    }
}

#[test]
fn tiny_loops_at_high_pitch_are_safe() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_f32(dir.path(), "dc.wav", 48_000, &stereo(vec![0.25]));
    write_manifest(dir.path(), vec![loop_asset("dc", file, 0, 1)]);

    let mut rt = Runtime::new(48_000);
    rt.load_manifest(dir.path()).unwrap();
    let params = PlayParams {
        pitch_semitones: 1000.0, // clamped
        ..PlayParams::default()
    };
    let voice = rt.play_by_name("dc", params).unwrap();
    let out = render(&mut rt, 1000);
    assert!(out.iter().all(|&s| s == 0.25));
    assert!(rt.is_playing(voice));
}

#[test]
fn loop_points_are_clamped_to_the_decoded_audio() {
    let dir = tempfile::tempdir().unwrap();
    let len = 300;
    let l = markers(len, 0);
    let file = write_f32(dir.path(), "short.wav", 48_000, &stereo(l.clone()));
    // The manifest claims a longer body than the file holds.
    write_manifest(dir.path(), vec![loop_asset("short", file, 100, 5000)]);

    let mut rt = Runtime::new(48_000);
    rt.load_manifest(dir.path()).unwrap();
    assert!(
        rt.warnings().iter().any(|w| w.message.contains("loop end")),
        "{:?}",
        rt.warnings()
    );
    rt.play_by_name("short", PlayParams::default()).unwrap();
    let out = left(&render(&mut rt, 1000));
    for (k, &value) in out.iter().enumerate() {
        let src = if k < len {
            k
        } else {
            100 + (k - len) % (len - 100)
        };
        assert!(same_bits(value, l[src]), "frame {k}");
    }
}
