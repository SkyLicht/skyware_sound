// SPDX-License-Identifier: MIT OR Apache-2.0
//! SFX containers: selection rules, randomization and reproducibility.

mod common;

use std::path::Path;

use common::*;
use sky_core::manifest::{RandomRange, Selection};
use sky_runtime::{PlayParams, Runtime};

/// The constant level variation `k` is made of, so a render reveals which
/// variation played.
fn level(k: usize) -> f32 {
    (k + 1) as f32 / 16.0
}

fn variations_fixture(dir: &Path, selection: Selection, count: usize) -> Runtime {
    let files = (0..count)
        .map(|k| {
            write_f32(
                dir,
                &format!("sfx/hit_{k:02}.wav"),
                48_000,
                &stereo(vec![level(k); 64]),
            )
        })
        .collect();
    write_manifest(
        dir,
        vec![sfx_asset(
            "hit",
            files,
            selection,
            RandomRange::ZERO,
            RandomRange::ZERO,
        )],
    );
    let mut rt = Runtime::new(48_000);
    rt.load_manifest(dir).unwrap();
    rt
}

/// Triggers "hit" and returns the index of the variation that played.
fn trigger(rt: &mut Runtime) -> usize {
    let voice = rt.play_by_name("hit", PlayParams::default()).unwrap();
    let mut frame = [0.0f32; 2];
    rt.render(&mut frame);
    rt.stop(voice, 0.0);
    assert_eq!(frame[0], frame[1]);
    let k = (frame[0] * 16.0).round() as usize;
    assert!(k >= 1, "nothing played");
    k - 1
}

fn picks(rt: &mut Runtime, n: usize) -> Vec<usize> {
    (0..n).map(|_| trigger(rt)).collect()
}

fn counts(picks: &[usize], count: usize) -> Vec<usize> {
    let mut c = vec![0; count];
    for &p in picks {
        c[p] += 1;
    }
    c
}

#[test]
fn random_is_roughly_uniform_and_may_repeat() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = variations_fixture(dir.path(), Selection::Random, 3);
    let p = picks(&mut rt, 3000);
    let c = counts(&p, 3);
    assert!(c.iter().all(|&n| (800..1200).contains(&n)), "{c:?}");
    assert!(
        p.windows(2).any(|w| w[0] == w[1]),
        "random should sometimes repeat"
    );
}

#[test]
fn random_no_repeat_never_repeats() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = variations_fixture(dir.path(), Selection::RandomNoRepeat, 4);
    let p = picks(&mut rt, 2000);
    assert!(p.windows(2).all(|w| w[0] != w[1]));
    let c = counts(&p, 4);
    assert!(c.iter().all(|&n| (400..600).contains(&n)), "{c:?}");
}

#[test]
fn random_no_repeat_with_two_alternates_and_with_one_repeats() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = variations_fixture(dir.path(), Selection::RandomNoRepeat, 2);
    let p = picks(&mut rt, 100);
    assert!(p.windows(2).all(|w| w[0] != w[1]), "{p:?}");

    let dir = tempfile::tempdir().unwrap();
    let mut rt = variations_fixture(dir.path(), Selection::RandomNoRepeat, 1);
    assert!(picks(&mut rt, 20).iter().all(|&p| p == 0));
}

#[test]
fn round_robin_plays_in_order_and_wraps() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = variations_fixture(dir.path(), Selection::RoundRobin, 3);
    let p = picks(&mut rt, 30);
    let expected: Vec<usize> = (0..30).map(|i| i % 3).collect();
    assert_eq!(p, expected);
}

#[test]
fn shuffle_plays_each_variation_once_per_block() {
    let dir = tempfile::tempdir().unwrap();
    let n = 5;
    let mut rt = variations_fixture(dir.path(), Selection::Shuffle, n);
    let p = picks(&mut rt, n * 200);
    for block in p.chunks(n) {
        let mut sorted = block.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..n).collect::<Vec<_>>(), "block {block:?}");
    }
    // Never twice in a row, including across reshuffles.
    assert!(p.windows(2).all(|w| w[0] != w[1]));
    // The order actually changes between blocks.
    assert!(p.chunks(n).any(|b| b != &p[..n]));
}

#[test]
fn selection_state_persists_per_asset() {
    let dir = tempfile::tempdir().unwrap();
    let mut files = Vec::new();
    for k in 0..3 {
        files.push(write_f32(
            dir.path(),
            &format!("v{k}.wav"),
            48_000,
            &stereo(vec![level(k); 16]),
        ));
    }
    write_manifest(
        dir.path(),
        vec![
            sfx_asset(
                "hit",
                files.clone(),
                Selection::RoundRobin,
                RandomRange::ZERO,
                RandomRange::ZERO,
            ),
            sfx_asset(
                "other",
                files,
                Selection::RoundRobin,
                RandomRange::ZERO,
                RandomRange::ZERO,
            ),
        ],
    );
    let mut rt = Runtime::new(48_000);
    rt.load_manifest(dir.path()).unwrap();
    assert_eq!(trigger(&mut rt), 0);
    // Playing another asset does not advance this one.
    for _ in 0..2 {
        let v = rt.play_by_name("other", PlayParams::default()).unwrap();
        rt.stop(v, 0.0);
    }
    assert_eq!(trigger(&mut rt), 1);
    assert_eq!(trigger(&mut rt), 2);
}

#[test]
fn pitch_randomization_stays_in_range() {
    let dir = tempfile::tempdir().unwrap();
    let len = 4096;
    let ramp: Vec<f32> = (0..len).map(|i| i as f32 / len as f32).collect();
    let file = write_f32(dir.path(), "ramp.wav", 48_000, &stereo(ramp));
    write_manifest(
        dir.path(),
        vec![sfx_asset(
            "whoosh",
            vec![file],
            Selection::Random,
            RandomRange::new(-2.0, 3.0),
            RandomRange::ZERO,
        )],
    );
    let mut rt = Runtime::new(48_000);
    rt.load_manifest(dir.path()).unwrap();

    // On a ramp, frame k reads position k * rate, so rate = value / (k / len).
    let measure = |rt: &mut Runtime, base: f32| {
        let params = PlayParams {
            pitch_semitones: base,
            ..PlayParams::default()
        };
        let voice = rt.play_by_name("whoosh", params).unwrap();
        let out = left(&render(rt, 12));
        rt.stop(voice, 0.0);
        let rate = f64::from(out[10]) / (10.0 / len as f64);
        12.0 * rate.log2()
    };

    let semis: Vec<f64> = (0..400).map(|_| measure(&mut rt, 0.0)).collect();
    for &s in &semis {
        assert!((-2.0 - 1e-3..=3.0 + 1e-3).contains(&s), "{s}");
    }
    let (lo, hi) = semis
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, b), &s| (a.min(s), b.max(s)));
    assert!(lo < -1.8 && hi > 2.8, "range not covered: {lo}..{hi}");

    // The random offset is added to the requested pitch.
    for _ in 0..50 {
        let s = measure(&mut rt, 12.0);
        assert!((10.0 - 1e-3..=15.0 + 1e-3).contains(&s), "{s}");
    }
}

#[test]
fn gain_randomization_stays_in_range() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_f32(dir.path(), "tick.wav", 48_000, &stereo(vec![0.5; 32]));
    write_manifest(
        dir.path(),
        vec![sfx_asset(
            "tick",
            vec![file],
            Selection::Random,
            RandomRange::ZERO,
            RandomRange::new(-12.0, -3.0),
        )],
    );
    let mut rt = Runtime::new(48_000);
    rt.load_manifest(dir.path()).unwrap();

    let measure = |rt: &mut Runtime, base: f32| {
        let params = PlayParams {
            gain_db: base,
            ..PlayParams::default()
        };
        let voice = rt.play_by_name("tick", params).unwrap();
        let mut frame = [0.0f32; 2];
        rt.render(&mut frame);
        rt.stop(voice, 0.0);
        20.0 * (frame[0] / 0.5).log10()
    };

    let dbs: Vec<f32> = (0..400).map(|_| measure(&mut rt, 0.0)).collect();
    for &db in &dbs {
        assert!((-12.001..=-2.999).contains(&db), "{db}");
    }
    let (lo, hi) = dbs
        .iter()
        .fold((f32::MAX, f32::MIN), |(a, b), &d| (a.min(d), b.max(d)));
    assert!(lo < -11.5 && hi > -3.5, "range not covered: {lo}..{hi}");
    assert!(dbs.windows(2).any(|w| w[0] != w[1]));

    for _ in 0..50 {
        let db = measure(&mut rt, -6.0);
        assert!((-18.001..=-8.999).contains(&db), "{db}");
    }
}

#[test]
fn sfx_voice_ends_with_its_variation() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = variations_fixture(dir.path(), Selection::Random, 1);
    let voice = rt.play_by_name("hit", PlayParams::default()).unwrap();
    let out = render(&mut rt, 63);
    assert!(out.iter().all(|&s| s == level(0)));
    assert!(rt.is_playing(voice));
    let out = render(&mut rt, 1);
    assert_eq!(out, vec![level(0); 2]);
    assert!(
        !rt.is_playing(voice),
        "the voice ends right after its last frame"
    );
    assert_eq!(rt.active_voices(), 0);
    assert!(render(&mut rt, 100).iter().all(|&s| s == 0.0));
}

/// Plays a scripted session of overlapping, randomized SFX over a resampled
/// loop and returns everything it rendered.
fn session(dir: &Path, seed: Option<u64>, block: usize) -> Vec<f32> {
    let mut rt = Runtime::new(48_000);
    rt.load_manifest(dir).unwrap();
    if let Some(seed) = seed {
        rt.set_seed(seed);
    }
    let mut out = Vec::new();
    rt.play_by_name("bed", PlayParams::default()).unwrap();
    for i in 0..40 {
        let params = PlayParams {
            pan: (i % 5) as f32 / 2.0 - 1.0,
            fade_in_ms: (i % 3) as f32,
            ..PlayParams::default()
        };
        rt.play_by_name("impact", params).unwrap();
        out.extend(render_blocks(&mut rt, 300, block));
    }
    out
}

fn session_fixture(dir: &Path) {
    let mut variations = Vec::new();
    for k in 0..3 {
        let samples: Vec<f32> = (0..2000).map(|i| marker(i * (k + 2), k)).collect();
        variations.push(write_f32(
            dir,
            &format!("impact_{k}.wav"),
            44_100,
            &[samples],
        ));
    }
    let bed: Vec<f32> = (0..3000).map(|i| marker(i, 3) * 0.2).collect();
    let bed = write_f32(dir, "bed.wav", 32_000, &stereo(bed));
    write_manifest(
        dir,
        vec![
            loop_asset("bed", bed, 0, 3000),
            sfx_asset(
                "impact",
                variations,
                Selection::RandomNoRepeat,
                RandomRange::new(-3.0, 3.0),
                RandomRange::new(-6.0, 0.0),
            ),
        ],
    );
}

#[test]
fn same_seed_renders_identical_output() {
    let dir = tempfile::tempdir().unwrap();
    session_fixture(dir.path());
    let a = session(dir.path(), Some(1234), 300);
    let b = session(dir.path(), Some(1234), 300);
    assert!(a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()));
    assert_eq!(a.len(), b.len());

    let c = session(dir.path(), Some(4321), 300);
    assert_ne!(a, c, "a different seed should change the output");

    // Without set_seed the runtime is still reproducible.
    assert_eq!(
        session(dir.path(), None, 300),
        session(dir.path(), None, 300)
    );
}

#[test]
fn output_does_not_depend_on_block_size() {
    let dir = tempfile::tempdir().unwrap();
    session_fixture(dir.path());
    let big = session(dir.path(), Some(7), 300);
    let small = session(dir.path(), Some(7), 1);
    let odd = session(dir.path(), Some(7), 77);
    assert_eq!(big, small);
    assert_eq!(big, odd);
}

#[test]
fn set_seed_resets_selection_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut rt = variations_fixture(dir.path(), Selection::Shuffle, 4);
    rt.set_seed(99);
    let first = picks(&mut rt, 23);
    rt.set_seed(99);
    let second = picks(&mut rt, 23);
    assert_eq!(first, second);

    let dir = tempfile::tempdir().unwrap();
    let mut rt = variations_fixture(dir.path(), Selection::RoundRobin, 3);
    picks(&mut rt, 2);
    rt.set_seed(1);
    assert_eq!(picks(&mut rt, 3), vec![0, 1, 2]);
}
