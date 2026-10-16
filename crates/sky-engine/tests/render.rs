// SPDX-License-Identifier: GPL-3.0-or-later
//! Offline render behaviour: timing, clips, loops and tail folding.

use std::sync::Arc;

use sky_core::{AudioId, ClipId, PPQ};
use sky_dsp::{AudioData, InstrumentType, analysis};
use sky_engine::{
    AudioCache, EngineMsg, compile, create_engine, render, render_intro_loop, render_loop,
    render_one_shot,
};
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
        muted: false,
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
        source: None,
    });
    let mut t = Track::new_audio("Audio");
    t.clips.push(Clip {
        id: ClipId::new(),
        name: "ramp".into(),
        start: PPQ,
        length: PPQ * 4,
        muted: false,
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
fn intro_loops_keep_the_intro_and_a_folded_body() {
    let p = demo_project();
    let bar = p.tempo.tick_to_sample(p.ticks_per_bar(), SR);
    // Bar 1 is the intro; bars 2 to 4 repeat.
    let (intro, start, end) = (0, bar, bar * 4);
    let out = render_intro_loop(compile(&p, &AudioCache::new(), SR, 1), intro, start, end);
    assert_eq!(out.frames(), end as usize);
    // The intro is the arrangement played from its start, up to the short
    // fade into the body.
    let plain = render(
        compile(&p, &AudioCache::new(), SR, 1),
        0,
        bar as usize,
        false,
    );
    let fade = (SR / 50) as usize;
    let before = bar as usize - fade;
    assert_eq!(&out.left()[..before], &plain.left()[..before]);
    // The body is the second time through, so the pad's reverb from the end
    // of the loop already rings at its start.
    let from_silence = render(
        compile(&p, &AudioCache::new(), SR, 1),
        start,
        (end - start) as usize,
        false,
    );
    let head = |a: &[f32]| a[..2_000].iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(head(&out.left()[bar as usize..]) > head(from_silence.left()));
    let seam = analysis::seam_check(&out, bar as usize, out.frames());
    assert_eq!(seam.verdict, "seamless", "{seam:?}");
}

#[test]
fn notes_held_into_the_body_do_not_cut_off_at_the_join() {
    // A pad note from beat 3 of the intro bar to beat 2 of the body.
    let mut p = Project::new("t");
    p.loop_enabled = false;
    let mut t = Track::new_instrument("Pad", InstrumentType::Synth);
    t.clips.push(Clip {
        id: ClipId::new(),
        name: "c".into(),
        start: 0,
        length: PPQ * 16,
        muted: false,
        content: ClipContent::Midi {
            notes: vec![Note::new(PPQ * 2, PPQ * 3, 57, 110)],
        },
    });
    p.tracks.push(t);
    let bar = p.tempo.tick_to_sample(p.ticks_per_bar(), SR);
    let out = render_intro_loop(compile(&p, &AudioCache::new(), SR, 1), 0, bar, bar * 2);
    let b = bar as usize;
    let fade = (SR / 50) as usize;
    let peak = |a: &[f32]| a.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    for ch in [out.left(), out.right()] {
        // The pad sounds up to the fade, and enters the body as smoothly
        // as the body's own samples follow each other.
        assert!(peak(&ch[b - fade - 480..b - fade]) > 0.01);
        let typical = ch[b..b + 64]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        let jump = (ch[b] - ch[b - 1]).abs();
        assert!(
            jump <= typical * 2.0 + 1e-4,
            "jump {jump}, typical {typical}"
        );
    }
    // The body still repeats seamlessly.
    let seam = analysis::seam_check(&out, b, out.frames());
    assert_eq!(seam.verdict, "seamless", "{seam:?}");
}

#[test]
fn one_shots_leave_out_what_starts_after_them() {
    // A kick on beat 1 and a snare on beat 2.
    let p = drum_project(vec![
        Note::new(0, PPQ / 4, 36, 110),
        Note::new(PPQ, PPQ / 4, 38, 110),
    ]);
    let beat = p.tempo.tick_to_sample(PPQ, SR);
    let frames = (beat + i64::from(SR)) as usize;
    let plain = render(compile(&p, &AudioCache::new(), SR, 1), 0, frames, false);
    let shot = render_one_shot(compile(&p, &AudioCache::new(), SR, 1), 0, beat, frames);
    let b = beat as usize;
    // The same up to the end of the range; then only the kick's decay.
    assert_eq!(&shot.left()[..b], &plain.left()[..b]);
    let peak = |a: &[f32]| a.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let kick = peak(&shot.left()[..4_800]);
    assert!(peak(&plain.left()[b..b + 4_800]) > 0.5 * kick);
    assert!(peak(&shot.left()[b..]) < 0.1 * kick);
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
fn muted_clips_play_nothing() {
    let mut p = drum_project(vec![Note::new(0, PPQ / 4, 36, 127)]);
    p.tracks[0].clips[0].muted = true;
    let out = render(compile(&p, &AudioCache::new(), SR, 1), 0, 48_000, false);
    assert_eq!(out.peak(), 0.0);
    p.tracks[0].clips[0].muted = false;
    let out = render(compile(&p, &AudioCache::new(), SR, 1), 0, 48_000, false);
    assert!(out.peak() > 0.0);
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
        muted: false,
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
            muted: false,
            content: ClipContent::pattern("~ bd", 0, PPQ * 4),
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

/// A pool file of constant `level`, `frames` long, in `audio` and `p`.
fn dc_audio(p: &mut Project, audio: &mut AudioCache, level: f32, frames: usize) -> AudioId {
    let id = AudioId::new();
    audio.insert(
        id,
        Arc::new(AudioData {
            sample_rate: SR,
            channels: vec![vec![level; frames]],
        }),
    );
    p.audio_pool.push(PoolEntry {
        id,
        name: format!("dc {level}"),
        file: format!("audio/{id}.wav"),
        channels: 1,
        sample_rate: SR,
        frames: frames as u64,
        source: None,
    });
    id
}

fn bank_project(clip: ClipContent, pitched: Option<&str>) -> (Project, AudioCache) {
    let mut p = Project::new("bank");
    p.loop_enabled = false;
    let mut audio = AudioCache::new();
    let mut sound = |p: &mut Project, levels: &[(f32, Option<u8>)]| -> Vec<BankSample> {
        levels
            .iter()
            .map(|&(level, root)| BankSample {
                audio: dc_audio(p, &mut audio, level, 4_800),
                root,
            })
            .collect()
    };
    let mut sounds = std::collections::BTreeMap::new();
    sounds.insert("bd".to_string(), sound(&mut p, &[(0.1, None)]));
    sounds.insert("sn".to_string(), sound(&mut p, &[(0.2, None), (0.3, None)]));
    sounds.insert(
        "piano".to_string(),
        sound(&mut p, &[(0.4, Some(60)), (0.5, Some(72))]),
    );
    let mut t = Track::new_instrument("Bank", InstrumentType::Bank);
    t.instrument.as_mut().unwrap().bank = Some(SampleBank {
        name: "Test".into(),
        sounds,
        pitched: pitched.map(str::to_string),
    });
    t.clips.push(Clip {
        id: ClipId::new(),
        name: "c".into(),
        start: 0,
        length: PPQ * 4,
        muted: false,
        content: clip,
    });
    p.tracks.push(t);
    (p, audio)
}

/// The level a sound settles at, a little after `onset`.
fn level_at(buf: &[f32], onset: usize) -> f32 {
    buf[onset + 200]
}

#[test]
fn bank_patterns_play_named_sounds_and_variations() {
    // One bar of four steps, a quarter note (24 000 samples) each.
    let (p, audio) = bank_project(ClipContent::pattern("bd sd:1 zz sn:2", 0, PPQ * 4), None);
    let out = render(compile(&p, &audio, SR, 1), 0, SR as usize * 2, false);
    let l = out.left();
    let base = level_at(l, 0);
    assert!(base > 0.01, "bd is silent");
    // `sd` finds the bank's `sn`; `:1` is its second variation.
    assert!((level_at(l, 24_000) / base - 3.0).abs() < 0.01);
    // An unknown sound plays nothing.
    assert!(l[48_000..72_000].iter().all(|s| s.abs() < 1e-6));
    // Index 2 of two variations wraps to the first.
    assert!((level_at(l, 72_000) / base - 2.0).abs() < 0.01);
    // Each sound plays its 4800-frame sample once, right on its step.
    assert!(first_sound(l).is_some_and(|i| i <= 1));
    assert!(l[5_000..24_000].iter().all(|s| s.abs() < 1e-6));
}

#[test]
fn bank_notes_play_drums_by_number_or_the_pitched_sound() {
    let notes = vec![Note::new(0, PPQ / 4, 38, 100)];
    let (p, audio) = bank_project(
        ClipContent::Midi {
            notes: notes.clone(),
        },
        None,
    );
    let drums = render(compile(&p, &audio, SR, 1), 0, SR as usize / 2, false);
    let (p2, audio2) = bank_project(ClipContent::pattern("bd", 0, PPQ * 4), None);
    let bd = render(compile(&p2, &audio2, SR, 1), 0, SR as usize / 2, false);
    // Note 38 is the snare: the first `sn`, twice the level of `bd`.
    assert!((level_at(drums.left(), 0) / level_at(bd.left(), 0) - 2.0).abs() < 0.01);

    // With `piano` pitched, note 62 plays the C4 sample two semitones up,
    // so the 4800 frames go by faster.
    let (p, audio) = bank_project(
        ClipContent::Midi {
            notes: vec![Note::new(0, PPQ, 62, 100)],
        },
        Some("piano"),
    );
    let out = render(compile(&p, &audio, SR, 1), 0, SR as usize / 2, false);
    let sounding = out.left().iter().filter(|s| s.abs() > 1e-4).count();
    let expected = 4_800.0 / 2f64.powf(2.0 / 12.0);
    assert!(
        (sounding as f64 - expected).abs() < 4.0,
        "{sounding} frames"
    );
    assert!((level_at(out.left(), 0) / level_at(bd.left(), 0) - 4.0).abs() < 0.01);
}

#[test]
fn the_master_loudness_meter_agrees_with_the_offline_analysis() {
    let p = demo_project();
    let (mut handle, mut engine) = create_engine(compile(&p, &AudioCache::new(), SR, 1));
    handle.send(EngineMsg::Play);
    let frames = SR as usize * 4;
    let mut left = vec![0.0; frames];
    let mut right = vec![0.0; frames];
    for (l, r) in left.chunks_mut(480).zip(right.chunks_mut(480)) {
        engine.process_planar(l, r);
    }
    let shared = handle.shared().clone();
    let mut seen = 0;
    let mut loud = sky_dsp::loudness::Loudness::default();
    let mut blocks = 0;
    shared.loudness.read(&mut seen, |b| {
        assert!(b.playing);
        loud.push(b.power, b.playing);
        blocks += 1;
    });
    assert_eq!(blocks, 40);
    let analysis = analysis::analyze(&AudioData {
        sample_rate: SR,
        channels: vec![left, right],
    });
    let integrated = loud.integrated().unwrap();
    let expected = analysis.integrated_lufs.unwrap();
    assert!(
        (integrated - expected).abs() < 0.15,
        "{integrated} vs {expected}"
    );
    let (tl, tr) = shared.master_true_peak.take();
    let tp = sky_dsp::util::gain_to_db(tl.max(tr));
    let expected = analysis.true_peak_dbfs.unwrap();
    assert!((tp - expected).abs() < 0.1, "{tp} vs {expected}");
    // Stopped, the blocks keep coming but say so.
    handle.send(EngineMsg::Stop);
    let mut l = vec![0.0; 4800];
    let mut r = vec![0.0; 4800];
    engine.process_planar(&mut l, &mut r);
    let mut stopped = 0;
    shared.loudness.read(&mut seen, |b| {
        assert!(!b.playing);
        stopped += 1;
    });
    assert_eq!(stopped, 1);
}
