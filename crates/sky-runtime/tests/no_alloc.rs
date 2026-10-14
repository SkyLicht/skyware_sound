// SPDX-License-Identifier: MIT OR Apache-2.0
//! `render` must never touch the allocator.
//!
//! This test binary installs `assert_no_alloc`'s allocator, which aborts the
//! process if anything allocates or frees memory inside `assert_no_alloc`.
//! The check runs in debug and release test builds.

mod common;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use common::*;
use sky_core::manifest::{RandomRange, Selection};
use sky_runtime::{PlayParams, Runtime};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn render_does_not_allocate() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path();
    // A loop that needs resampling, plus mono and stereo SFX of various rates.
    let music: Vec<f32> = (0..20_000).map(|i| marker(i, 0) * 0.3).collect();
    let music = write_f32(dir, "music.wav", 44_100, &stereo(music));
    let mut hits = Vec::new();
    for k in 0..4 {
        let samples: Vec<f32> = (0..3000 + k * 500).map(|i| marker(i, k)).collect();
        let channels = if k % 2 == 0 {
            vec![samples]
        } else {
            stereo(samples)
        };
        hits.push(write_f32(
            dir,
            &format!("hit_{k}.wav"),
            22_050 * (k as u32 % 2 + 1),
            &channels,
        ));
    }
    write_manifest(
        dir,
        vec![
            loop_asset("music", music, 2000, 18_000),
            sfx_asset(
                "hit",
                hits,
                Selection::Shuffle,
                RandomRange::new(-5.0, 5.0),
                RandomRange::new(-6.0, 0.0),
            ),
        ],
    );

    // Few voices, so stealing happens too.
    let mut rt = Runtime::with_max_voices(48_000, 6);
    rt.load_manifest(dir).unwrap();
    let hit = rt.asset("hit").unwrap();
    let music = rt
        .play_by_name(
            "music",
            PlayParams {
                fade_in_ms: 50.0,
                ..PlayParams::default()
            },
        )
        .unwrap();
    let mut buffer = vec![0.0f32; 2 * 256];

    for block in 0..400 {
        if block % 3 == 0 {
            let params = PlayParams {
                pan: (block % 7) as f32 / 3.0 - 1.0,
                fade_in_ms: (block % 4) as f32,
                ..PlayParams::default()
            };
            rt.play(hit, params).unwrap();
        }
        match block {
            100 => rt.set_voice_gain(music, -12.0, 200.0),
            150 => rt.set_master_gain(-3.0),
            300 => rt.stop(music, 300.0),
            350 => rt.stop_all(20.0),
            _ => {}
        }
        assert_no_alloc(|| rt.render(&mut buffer));
    }
    assert!(buffer.iter().all(|s| s.is_finite()));

    // Triggering and controlling voices by handle is allocation-free as well.
    assert_no_alloc(|| {
        let voice = rt.play(hit, PlayParams::default()).unwrap();
        rt.set_voice_gain(voice, -3.0, 5.0);
        rt.render(&mut buffer);
        rt.stop(voice, 5.0);
        rt.render(&mut buffer);
        rt.set_master_gain(0.0);
        rt.render(&mut buffer);
    });
}

#[test]
fn the_split_renderer_does_not_allocate_or_free() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path();
    let music: Vec<f32> = (0..20_000).map(|i| marker(i, 0) * 0.3).collect();
    let music = write_f32(dir, "music.wav", 44_100, &stereo(music));
    let hits: Vec<_> = (0..3)
        .map(|k| {
            let samples: Vec<f32> = (0..800 + k * 300).map(|i| marker(i, k)).collect();
            write_f32(dir, &format!("hit_{k}.wav"), 48_000, &[samples])
        })
        .collect();
    write_manifest(
        dir,
        vec![
            loop_asset("music", music, 2000, 18_000),
            sfx_asset(
                "hit",
                hits,
                Selection::RoundRobin,
                RandomRange::new(-2.0, 2.0),
                RandomRange::new(-3.0, 0.0),
            ),
        ],
    );
    let mut rt = Runtime::with_max_voices(48_000, 5);
    rt.load_manifest(dir).unwrap();
    // A small queue, so calls also wait for room.
    let (mut ctl, mut rnd) = rt.split(16);
    let music = ctl.play_by_name("music", PlayParams::default()).unwrap();
    let mut buffer = vec![0.0f32; 2 * 256];
    for block in 0..300 {
        // Several hits per block: voices end, get stolen and come back.
        let hits = if block < 250 { block % 4 } else { 0 };
        for k in 0..hits {
            let hit = ctl.play_by_name("hit", PlayParams::default()).unwrap();
            if k == 1 {
                ctl.set_voice_pan(hit, 0.5, 2.0);
                ctl.set_voice_pitch(hit, -1.0);
            }
            if k == 2 {
                ctl.stop(hit, 0.0);
            }
        }
        match block {
            50 => ctl.set_voice_gain(music, -6.0, 50.0),
            80 => ctl.set_category_gain(sky_core::manifest::Category::Sfx, -9.0, 20.0),
            // Reloading replaces the assets while the old audio still plays.
            120 => {
                ctl.load_manifest(dir).unwrap();
            }
            200 => ctl.set_master_gain(-2.0),
            250 => ctl.stop_all(5.0),
            _ => {}
        }
        assert_no_alloc(|| rnd.render(&mut buffer));
    }
    assert!(buffer.iter().all(|s| s.is_finite()));
    assert_eq!(ctl.active_voices(), 0);
}
