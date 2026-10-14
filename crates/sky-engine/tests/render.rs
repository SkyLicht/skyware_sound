// SPDX-License-Identifier: GPL-3.0-or-later
//! Offline render behaviour: timing, clips, loops and tail folding.

use std::sync::Arc;

use sky_core::{AudioId, ClipId, PPQ};
use sky_dsp::{AudioData, InstrumentType, analysis};
use sky_engine::{AudioCache, EngineMsg, compile, create_engine, render, render_loop};
use sky_model::templates::demo_project;
use sky_model::*;

const SR: u32 = 48_000;

fn first_sound(buf: &[f32]) -> Option<usize> {
    buf.iter().position(|s| s.abs() > 1e-6)
}

fn drum_project(notes: Vec<Note>) -> Project {
    let mut p = Project::new("t");
    p.loop_enabled = false;
    let mut t = Track::new_instrument("Drums", InstrumentType::Drums);
    t.clips.push(Clip {
        id: ClipId::new(),
        name: "c".into(),
        start: 0,
        length: PPQ * 16,
        content: ClipContent::Midi { notes },
    });
    p.tracks.push(t);
    p
}

#[test]
fn notes_start_on_the_exact_sample() {
    // 120 BPM at 48 kHz: one quarter note is 24 000 samples.
    let p = drum_project(vec![Note::new(PPQ, PPQ / 4, 36, 127)]);
    let snap = compile(&p, &AudioCache::new(), SR, 1);
    let out = render(snap, 0, SR as usize, false);
    let start = first_sound(out.left()).unwrap();
    assert!((24_000..=24_001).contains(&start), "first sound at {start}");
}

#[test]
fn renders_are_deterministic_and_finite() {
    let p = demo_project();
    let a = render(
        compile(&p, &AudioCache::new(), SR, 1),
        0,
        SR as usize * 2,
        false,
    );
    let b = render(
        compile(&p, &AudioCache::new(), SR, 1),
        0,
        SR as usize * 2,
        false,
    );
    assert_eq!(a, b);
    assert!(a.channels.iter().flatten().all(|s| s.is_finite()));
    assert!(a.peak() > 0.05, "demo is too quiet: {}", a.peak());
    assert!(a.peak() <= 0.892, "master limiter let {} through", a.peak());
}

#[test]
fn audio_clips_play_sample_exact() {
    let mut p = Project::new("audio");
    p.loop_enabled = false;
    let id = AudioId::new();
    let source: Vec<f32> = (0..10_000).map(|i| (i as f32 / 10_000.0) - 0.5).collect();
    let data = Arc::new(AudioData {
        sample_rate: SR,
        channels: vec![source.clone()],
    });
    p.audio_pool.push(PoolEntry {
        id,
        name: "ramp".into(),
        file: "audio/ramp.wav".into(),
        channels: 1,
        sample_rate: SR,
        frames: 10_000,
    });
    let mut t = Track::new_audio("Audio");
    t.clips.push(Clip {
        id: ClipId::new(),
        name: "ramp".into(),
        start: PPQ,
        length: PPQ * 4,
        content: ClipContent::Audio {
            source: id,
            offset: 100,
            gain_db: 0.0,
            fade_in: 0,
            fade_out: 0,
        },
    });
    p.tracks.push(t);
    let mut cache = AudioCache::new();
    cache.insert(id, data);
    let out = render(compile(&p, &cache, SR, 1), 0, 40_000, false);
    for i in 0..9_000 {
        let expected = source[100 + i];
        let got = out.left()[24_000 + i];
        assert!(
            (got - expected).abs() < 1e-6,
            "sample {i}: {got} vs {expected}"
        );
        assert_eq!(out.right()[24_000 + i], got);
    }
    assert!(out.left()[..24_000].iter().all(|s| *s == 0.0));
}

#[test]
fn transport_loop_wraps_on_the_loop_end() {
    let mut p = drum_project(vec![Note::new(0, PPQ / 4, 36, 127)]);
    p.loop_enabled = true;
    p.loop_region = TimeRange::new(0, PPQ * 4).unwrap();
    let bar = 96_000;
    let out = render(compile(&p, &AudioCache::new(), SR, 1), 0, bar * 3, true);
    for pass in 0..3 {
        let seg = &out.left()[pass * bar..(pass + 1) * bar];
        assert!(first_sound(seg).unwrap() <= 1, "pass {pass} starts late");
    }
}

#[test]
fn loop_render_folds_tails_into_the_start() {
    let p = demo_project();
    let (start, end) = (0, p.tempo.tick_to_sample(p.loop_region.end, SR));
    let snap = compile(&p, &AudioCache::new(), SR, 1);
    let folded = render_loop(snap, start, end, 2);
    assert_eq!(folded.frames(), end as usize);
    let plain = render(
        compile(&p, &AudioCache::new(), SR, 1),
        0,
        end as usize,
        false,
    );
    // The pad's reverb tail from the end of the loop rings into the start of
    // the folded render, but not into a plain render from silence.
    let head = |a: &AudioData| a.left()[..2_000].iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(head(&folded) > head(&plain));
    let seam = analysis::seam_check(&folded, 0, folded.frames());
    assert_eq!(seam.verdict, "seamless", "{seam:?}");
}

#[test]
fn mute_and_solo() {
    let mut p = demo_project();
    for t in &mut p.tracks {
        t.channel.mute = true;
    }
    p.master.effects.clear();
    let silent = render(compile(&p, &AudioCache::new(), SR, 1), 0, 48_000, false);
    // Instruments still ring through effects, but muted channels pass nothing.
    assert_eq!(silent.peak(), 0.0);
    let mut p = demo_project();
    p.tracks[1].channel.solo = true;
    let soloed = render(compile(&p, &AudioCache::new(), SR, 1), 0, 48_000, false);
    assert!(soloed.peak() > 0.0);
}

#[test]
fn swaps_keep_held_notes_sounding() {
    let mut p = Project::new("held");
    p.loop_enabled = false;
    let mut t = Track::new_instrument("Pad", InstrumentType::Synth);
    t.clips.push(Clip {
        id: ClipId::new(),
        name: "long".into(),
        start: 0,
        length: PPQ * 16,
        content: ClipContent::Midi {
            notes: vec![Note::new(0, PPQ * 16, 60, 100)],
        },
    });
    p.tracks.push(t);
    let (mut handle, mut engine) = create_engine(compile(&p, &AudioCache::new(), SR, 1));
    handle.send(EngineMsg::Play);
    let mut l = vec![0.0; 4_800];
    let mut r = vec![0.0; 4_800];
    engine.process_planar(&mut l, &mut r);
    // An unrelated edit recompiles the snapshot mid-note.
    p.tracks[0].channel.volume_db = -1.0;
    handle.send(EngineMsg::Swap(Box::new(compile(
        &p,
        &AudioCache::new(),
        SR,
        2,
    ))));
    engine.process_planar(&mut l, &mut r);
    let peak = l.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.01, "note was cut by the swap");
    assert_eq!(handle.collect_garbage(), 1);
}

#[test]
fn pattern_clips_play_on_the_grid() {
    // "~ bd" over a one-bar cycle at 120 BPM: the kick lands half way
    // through each bar, at 48 000 and 144 000 samples.
    let project = |bars: i64| {
        let mut p = Project::new("t");
        p.loop_enabled = false;
        let mut t = Track::new_instrument("Drums", InstrumentType::Drums);
        t.clips.push(Clip {
            id: ClipId::new(),
            name: "p".into(),
            start: 0,
            length: PPQ * 4 * bars,
            content: ClipContent::Pattern {
                source: "~ bd".into(),
                seed: 0,
                cycle: PPQ * 4,
            },
        });
        p.tracks.push(t);
        p
    };
    let frames = SR as usize * 4;
    let one = render(
        compile(&project(1), &AudioCache::new(), SR, 1),
        0,
        frames,
        false,
    );
    let two = render(
        compile(&project(2), &AudioCache::new(), SR, 1),
        0,
        frames,
        false,
    );
    let first = first_sound(one.left()).unwrap();
    assert!((48_000..=48_001).contains(&first), "first kick at {first}");
    // The second bar's kick is what the two-bar render adds.
    let diff: Vec<f32> = two
        .left()
        .iter()
        .zip(one.left())
        .map(|(a, b)| a - b)
        .collect();
    let second = first_sound(&diff).unwrap();
    assert!(
        (144_000..=144_001).contains(&second),
        "second kick at {second}"
    );
}
