// SPDX-License-Identifier: MIT OR Apache-2.0
//! The split runtime (a controller for game code and a renderer for the
//! audio callback), category gains, live pan and pitch, and loading from
//! memory.

mod common;

use std::collections::HashMap;
use std::path::Path;

use common::*;
use sky_core::manifest::{Category, MANIFEST_FILE_NAME, RandomRange, Selection};
use sky_runtime::{Controller, PlayParams, Renderer, Runtime, RuntimeError};

/// A music loop, a stereo SFX set with randomization, and constant-level
/// files whose gains are easy to read.
fn fixture(dir: &Path) {
    let music: Vec<f32> = (0..9_000).map(|i| marker(i, 0) * 0.5).collect();
    let mut hits = Vec::new();
    for k in 0..3 {
        let samples: Vec<f32> = (0..2_000 + k * 700).map(|i| marker(i, k)).collect();
        hits.push(write_f32(
            dir,
            &format!("hit_{k}.wav"),
            44_100,
            &stereo(samples),
        ));
    }
    let mut level = sfx_asset(
        "level",
        vec![write_f32(dir, "level.wav", 48_000, &[vec![0.5; 48_000]])],
        Selection::Random,
        RandomRange::ZERO,
        RandomRange::ZERO,
    );
    if let sky_core::manifest::Asset::Sfx(s) = &mut level {
        s.category = Category::Ui;
    }
    write_manifest(
        dir,
        vec![
            loop_asset(
                "music",
                write_f32(dir, "music.wav", 48_000, &stereo(music)),
                1_000,
                9_000,
            ),
            loop_asset(
                "hum",
                write_f32(dir, "hum.wav", 48_000, &stereo(vec![0.25; 600])),
                0,
                600,
            ),
            sfx_asset(
                "hit",
                hits,
                Selection::RandomNoRepeat,
                RandomRange::new(-3.0, 3.0),
                RandomRange::new(-6.0, 0.0),
            ),
            level,
            sfx_asset(
                "blip",
                vec![write_f32(dir, "blip.wav", 48_000, &[vec![0.5; 1_000]])],
                Selection::Random,
                RandomRange::ZERO,
                RandomRange::ZERO,
            ),
        ],
    );
}

fn loaded(dir: &Path, voices: usize) -> Runtime {
    let mut rt = Runtime::with_max_voices(48_000, voices);
    rt.load_manifest(dir).unwrap();
    assert!(rt.warnings().is_empty(), "{:?}", rt.warnings());
    rt
}

fn render_split(renderer: &mut Renderer, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; frames * 2];
    for chunk in out.chunks_mut(512) {
        renderer.render(chunk);
    }
    out
}

/// One scripted session of game calls, run against either kind of runtime.
trait Game {
    fn play(&mut self, name: &str, params: PlayParams) -> sky_runtime::VoiceId;
    fn stop(&mut self, voice: sky_runtime::VoiceId, ms: f32);
    fn gain(&mut self, voice: sky_runtime::VoiceId, db: f32, ms: f32);
    fn pan(&mut self, voice: sky_runtime::VoiceId, pan: f32, ms: f32);
    fn pitch(&mut self, voice: sky_runtime::VoiceId, semitones: f32);
    fn category(&mut self, category: Category, db: f32, ms: f32);
    fn render(&mut self, frames: usize) -> Vec<f32>;
}

impl Game for Runtime {
    fn play(&mut self, name: &str, params: PlayParams) -> sky_runtime::VoiceId {
        self.play_by_name(name, params).unwrap()
    }
    fn stop(&mut self, voice: sky_runtime::VoiceId, ms: f32) {
        Runtime::stop(self, voice, ms);
    }
    fn gain(&mut self, voice: sky_runtime::VoiceId, db: f32, ms: f32) {
        self.set_voice_gain(voice, db, ms);
    }
    fn pan(&mut self, voice: sky_runtime::VoiceId, pan: f32, ms: f32) {
        self.set_voice_pan(voice, pan, ms);
    }
    fn pitch(&mut self, voice: sky_runtime::VoiceId, semitones: f32) {
        self.set_voice_pitch(voice, semitones);
    }
    fn category(&mut self, category: Category, db: f32, ms: f32) {
        self.set_category_gain(category, db, ms);
    }
    fn render(&mut self, frames: usize) -> Vec<f32> {
        render_blocks(self, frames, 512)
    }
}

impl Game for (Controller, Renderer) {
    fn play(&mut self, name: &str, params: PlayParams) -> sky_runtime::VoiceId {
        self.0.play_by_name(name, params).unwrap()
    }
    fn stop(&mut self, voice: sky_runtime::VoiceId, ms: f32) {
        self.0.stop(voice, ms);
    }
    fn gain(&mut self, voice: sky_runtime::VoiceId, db: f32, ms: f32) {
        self.0.set_voice_gain(voice, db, ms);
    }
    fn pan(&mut self, voice: sky_runtime::VoiceId, pan: f32, ms: f32) {
        self.0.set_voice_pan(voice, pan, ms);
    }
    fn pitch(&mut self, voice: sky_runtime::VoiceId, semitones: f32) {
        self.0.set_voice_pitch(voice, semitones);
    }
    fn category(&mut self, category: Category, db: f32, ms: f32) {
        self.0.set_category_gain(category, db, ms);
    }
    fn render(&mut self, frames: usize) -> Vec<f32> {
        render_split(&mut self.1, frames)
    }
}

fn script(game: &mut impl Game) -> Vec<f32> {
    let mut out = Vec::new();
    let music = game.play(
        "music",
        PlayParams {
            fade_in_ms: 20.0,
            ..PlayParams::default()
        },
    );
    out.extend(game.render(3_000));
    for k in 0..12 {
        let hit = game.play(
            "hit",
            PlayParams {
                pan: k as f32 / 6.0 - 1.0,
                ..PlayParams::default()
            },
        );
        if k % 3 == 0 {
            game.pan(hit, -0.5, 5.0);
            game.pitch(hit, 2.0);
        }
        out.extend(game.render(700));
    }
    game.gain(music, -6.0, 30.0);
    game.category(Category::Music, -3.0, 50.0);
    game.category(Category::Sfx, -12.0, 0.0);
    game.play("blip", PlayParams::default());
    out.extend(game.render(4_000));
    game.stop(music, 40.0);
    out.extend(game.render(4_000));
    out
}

#[test]
fn a_split_runtime_renders_exactly_like_a_single_one() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut single = loaded(dir.path(), 8);
    let mut split = loaded(dir.path(), 8).split(64);
    let a = script(&mut single);
    let b = script(&mut split);
    assert_eq!(a.len(), b.len());
    assert!(a.iter().any(|s| s.abs() > 0.05));
    assert_eq!(a, b, "the split runtime must render bit-identically");
}

#[test]
fn ended_voices_are_reported_back_to_the_controller() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let (mut ctl, mut rnd) = loaded(dir.path(), 4).split(32);
    let hum = ctl.play_by_name("hum", PlayParams::default()).unwrap();
    let blip = ctl.play_by_name("blip", PlayParams::default()).unwrap();
    // Nothing has rendered yet, but both count as started.
    assert_eq!(ctl.active_voices(), 2);
    render_split(&mut rnd, 2_000);
    assert!(ctl.is_playing(hum));
    assert!(!ctl.is_playing(blip), "the 1000-frame blip ended");
    ctl.stop(hum, 0.0);
    // Until the renderer applies the stop, the hum still counts.
    assert!(ctl.is_playing(hum));
    render_split(&mut rnd, 512);
    assert!(!ctl.is_playing(hum));
    assert_eq!(ctl.active_voices(), 0);
    // Stale ids do nothing.
    ctl.stop(hum, 10.0);
    ctl.set_voice_gain(blip, -3.0, 0.0);
    let out = render_split(&mut rnd, 512);
    assert!(out.iter().all(|s| *s == 0.0));
}

#[test]
fn calls_beyond_the_queue_wait_and_are_not_lost() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    // A queue of 16 commands and 40 loops started before any render.
    let (mut ctl, mut rnd) = loaded(dir.path(), 64).split(16);
    let voices: Vec<_> = (0..40)
        .map(|_| ctl.play_by_name("hum", PlayParams::default()).unwrap())
        .collect();
    let mut out = vec![0.0f32; 2];
    rnd.render(&mut out);
    // The renderer took what fit; the rest goes with the next calls.
    let mut blocks = 1;
    while blocks < 10 {
        ctl.update();
        rnd.render(&mut out);
        blocks += 1;
    }
    // 40 stereo loops at 0.25, which play at unity when centered.
    assert!((out[0] - 40.0 * 0.25).abs() < 1e-4, "{}", out[0]);
    for v in voices {
        assert!(ctl.is_playing(v));
    }
}

#[test]
fn only_loops_left_means_no_free_voice() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let (mut ctl, _rnd) = loaded(dir.path(), 2).split(16);
    ctl.play_by_name("hum", PlayParams::default()).unwrap();
    ctl.play_by_name("music", PlayParams::default()).unwrap();
    assert!(matches!(
        ctl.play_by_name("blip", PlayParams::default()),
        Err(RuntimeError::NoFreeVoice)
    ));
    assert!(matches!(
        ctl.play_by_name("nope", PlayParams::default()),
        Err(RuntimeError::UnknownAsset(_))
    ));
}

#[test]
fn category_gains_apply_to_their_category_only() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut rt = loaded(dir.path(), 8);
    // "level" is a UI sound at 0.5 mono, "hum" music at 0.25 stereo.
    rt.play_by_name("level", PlayParams::default()).unwrap();
    rt.play_by_name("hum", PlayParams::default()).unwrap();
    let center = std::f32::consts::FRAC_1_SQRT_2;
    let both = render(&mut rt, 1);
    assert!((both[0] - (0.5 * center + 0.25)).abs() < 1e-6);
    rt.set_category_gain(Category::Music, f32::NEG_INFINITY, 0.0);
    let ui_only = render(&mut rt, 1);
    assert!((ui_only[0] - 0.5 * center).abs() < 1e-6, "{}", ui_only[0]);
    // A ramp goes linearly from the old gain to the new one.
    rt.set_category_gain(Category::Ui, -6.0206, 100.0);
    let ramp = left(&render(&mut rt, 4_800));
    assert!(ramp[0] < 0.5 * center && ramp[0] > 0.49 * center);
    assert!(
        (ramp[4_799] - 0.25 * center).abs() < 1e-4,
        "{}",
        ramp[4_799]
    );
    assert!(
        (ramp[2_400] - 0.375 * center).abs() < 1e-3,
        "{}",
        ramp[2_400]
    );
    // Turning music back up brings back the loop that kept playing.
    rt.set_category_gain(Category::Music, 0.0, 0.0);
    let back = render(&mut rt, 1);
    assert!(
        (back[0] - (0.25 * center + 0.25)).abs() < 1e-4,
        "{}",
        back[0]
    );
}

#[test]
fn voices_can_be_panned_and_pitched_while_playing() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut rt = loaded(dir.path(), 8);
    let blip = rt.play_by_name("blip", PlayParams::default()).unwrap();
    let first = render(&mut rt, 1);
    assert_eq!(first[0], first[1]);
    rt.set_voice_pan(blip, -1.0, 0.0);
    let hard_left = render(&mut rt, 1);
    assert!((hard_left[0] - 0.5).abs() < 1e-6);
    assert_eq!(hard_left[1], 0.0);
    // An octave up plays the remaining 998 frames in 499.
    rt.set_voice_pitch(blip, 12.0);
    render(&mut rt, 498);
    assert!(rt.is_playing(blip));
    render(&mut rt, 2);
    assert!(!rt.is_playing(blip));
    // A ramped pan moves smoothly.
    let blip = rt.play_by_name("blip", PlayParams::default()).unwrap();
    rt.set_voice_pan(blip, 1.0, 10.0);
    let l = left(&render(&mut rt, 480));
    assert!(
        l.windows(2).all(|w| w[1] <= w[0] + 1e-7),
        "left falls steadily"
    );
    assert!(l[479].abs() < 1e-6);
}

#[test]
fn manifests_load_from_memory() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let json = std::fs::read_to_string(dir.path().join(MANIFEST_FILE_NAME)).unwrap();
    // A game's pack file: paths from the manifest to bytes.
    let mut pack: HashMap<String, Vec<u8>> = HashMap::new();
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "wav") {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            pack.insert(name, std::fs::read(&path).unwrap());
        }
    }
    let mut from_memory = Runtime::new(48_000);
    let mut reads = 0;
    let handles = from_memory
        .load_manifest_with(&json, |path| {
            reads += 1;
            pack.get(path)
                .cloned()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, path.to_string()))
        })
        .unwrap();
    assert_eq!(handles.len(), 5);
    assert_eq!(reads, 7, "each file is read once");
    let mut from_disk = loaded(dir.path(), 64);
    for rt in [&mut from_memory, &mut from_disk] {
        rt.play_by_name("music", PlayParams::default()).unwrap();
        rt.play_by_name("hit", PlayParams::default()).unwrap();
    }
    assert_eq!(
        render(&mut from_memory, 5_000),
        render(&mut from_disk, 5_000)
    );

    // A missing file fails the whole load with its manifest path.
    pack.remove("hit_1.wav");
    let mut rt = Runtime::new(48_000);
    let err = rt
        .load_manifest_with(&json, |path| {
            pack.get(path)
                .cloned()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, path.to_string()))
        })
        .unwrap_err();
    assert!(err.to_string().contains("hit_1.wav"), "{err}");
    assert!(rt.asset("music").is_none());
}

#[test]
fn the_halves_run_on_their_own_threads() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let (mut ctl, mut rnd) = loaded(dir.path(), 16).split(64);
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let audio = {
        let done = done.clone();
        std::thread::spawn(move || {
            let mut block = vec![0.0f32; 2 * 128];
            let mut loudest = 0.0f32;
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                rnd.render(&mut block);
                loudest = block.iter().fold(loudest, |m, s| m.max(s.abs()));
                std::thread::yield_now();
            }
            (rnd, loudest)
        })
    };
    let music = ctl.play_by_name("music", PlayParams::default()).unwrap();
    for _ in 0..200 {
        ctl.play_by_name("hit", PlayParams::default()).unwrap();
        ctl.set_category_gain(Category::Sfx, -3.0, 5.0);
        std::thread::yield_now();
    }
    ctl.stop_all(0.0);
    done.store(true, std::sync::atomic::Ordering::Release);
    let (mut rnd, loudest) = audio.join().unwrap();
    assert!(loudest > 0.01);
    // Calls that waited for room in the queue still arrive, in order.
    let mut block = vec![0.0f32; 2 * 128];
    for _ in 0..100 {
        if ctl.active_voices() == 0 {
            break;
        }
        rnd.render(&mut block);
    }
    assert!(!ctl.is_playing(music));
    assert_eq!(ctl.active_voices(), 0);
}
