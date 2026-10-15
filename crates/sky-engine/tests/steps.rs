// SPDX-License-Identifier: GPL-3.0-or-later
//! Step clips and the Acid bass, rendered.

use sky_core::{ClipId, PPQ};
use sky_dsp::InstrumentType;
use sky_engine::compile::SourceNode;
use sky_engine::{AudioCache, compile, render, render_loop};
use sky_model::steps::parse_locks;
use sky_model::templates::acid_project;
use sky_model::*;

const SR: u32 = 48_000;
/// A beat and a sixteenth at 120 BPM.
const BEAT: usize = 24_000;
const SIXTEENTH: usize = BEAT / 4;

fn steps_clip(bars: i64, steps: StepClip) -> Clip {
    Clip {
        id: ClipId::new(),
        name: "s".into(),
        start: 0,
        length: PPQ * 4 * bars,
        content: ClipContent::Steps(steps),
    }
}

fn track(kind: InstrumentType, lanes: Vec<StepLane>) -> Track {
    let mut t = Track::new_instrument("t", kind);
    t.clips.push(steps_clip(1, StepClip::new(16, lanes)));
    t
}

fn project(tracks: Vec<Track>) -> Project {
    let mut p = Project::new("t");
    p.loop_enabled = false;
    p.tracks = tracks;
    p
}

fn play(p: &Project, frames: usize) -> Vec<f32> {
    let out = render(compile(p, &AudioCache::new(), SR, 1), 0, frames, false);
    out.left().to_vec()
}

fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0, |m, s| m.max(s.abs()))
}

fn rms(buf: &[f32]) -> f32 {
    (buf.iter().map(|s| s * s).sum::<f32>() / buf.len().max(1) as f32).sqrt()
}

fn onsets(buf: &[f32], step: usize) -> Vec<usize> {
    buf.chunks(step)
        .enumerate()
        .filter(|(_, c)| peak(c) > 0.01)
        .map(|(i, _)| i)
        .collect()
}

#[test]
fn drum_lanes_play_on_the_grid() {
    let p = project(vec![track(
        InstrumentType::Drums,
        vec![
            StepLane::parse(36, "x . . .").unwrap(),
            StepLane::parse(39, ". . . . x . . .").unwrap(),
        ],
    )]);
    let out = play(&p, BEAT * 4);
    let first = out.iter().position(|s| s.abs() > 1e-6).unwrap();
    assert!(first <= 1, "{first}");
    // The kick lane, four steps long, repeats every beat; the clap lane,
    // eight steps long, plays on beats 2 and 4.
    let snap = compile(&p, &AudioCache::new(), SR, 1);
    let SourceNode::Instrument { notes, .. } = &snap.tracks[0].source else {
        panic!("an instrument track");
    };
    let times = |pitch: u8| -> Vec<i64> {
        notes
            .iter()
            .filter(|n| n.pitch == pitch)
            .map(|n| n.time)
            .collect()
    };
    let beat = BEAT as i64;
    assert_eq!(times(36), [0, beat, 2 * beat, 3 * beat]);
    assert_eq!(times(39), [beat, 3 * beat]);
}

#[test]
fn locks_set_each_steps_sound() {
    let mut lane = StepLane::parse(42, "x . . . x . . . x . . . x . . .").unwrap();
    parse_locks(
        &mut lane.steps,
        "hat_level",
        ". . . . -60 . . . . . . . -60 . . .",
    )
    .unwrap();
    let p = project(vec![track(InstrumentType::Drums, vec![lane])]);
    let beats: Vec<f32> = play(&p, BEAT * 4).chunks(BEAT).map(peak).collect();
    assert!(beats[0] > 0.01 && beats[2] > 0.01, "{beats:?}");
    assert!(beats[1] < 1e-4 && beats[3] < 1e-4, "{beats:?}");
}

#[test]
fn swing_and_micro_timing_move_steps() {
    let at = |text: &str, swing: Swing| {
        let mut steps = StepClip::new(16, vec![StepLane::parse(42, text).unwrap()]);
        steps.swing = swing;
        let mut t = Track::new_instrument("t", InstrumentType::Drums);
        t.clips.push(steps_clip(1, steps));
        let out = play(&project(vec![t]), BEAT);
        out.iter().position(|s| s.abs() > 1e-6).unwrap()
    };
    assert!((6_000..=6_001).contains(&at(". x", Swing::default())));
    assert!((9_000..=9_001).contains(&at(". x", Swing::new(75, 16))));
    // 20% of a step late.
    assert!((7_200..=7_201).contains(&at(". x>20", Swing::default())));
}

#[test]
fn probability_plays_the_same_every_render() {
    let lane = StepLane::parse(42, &vec!["x?50"; 16].join(" ")).unwrap();
    let p = project(vec![track(InstrumentType::Drums, vec![lane])]);
    let a = onsets(&play(&p, BEAT * 4), SIXTEENTH);
    assert_eq!(a, onsets(&play(&p, BEAT * 4), SIXTEENTH));
    assert!((3..13).contains(&a.len()), "{a:?}");
}

/// The Acid on a line, with a clean filter so levels are easy to read.
fn acid_line(text: &str) -> Vec<f32> {
    let mut t = track(
        InstrumentType::Acid,
        vec![StepLane::parse(36, text).unwrap()],
    );
    let slot = t.instrument.as_mut().unwrap();
    for (k, v) in [("resonance", 0.0), ("env_mod", 0.0), ("cutoff", 4_000.0)] {
        slot.params.insert(k.into(), v);
    }
    play(&project(vec![t]), BEAT)
}

#[test]
fn a_slide_holds_the_acid_into_the_next_note() {
    // Without a slide the first note ends at half a step and dies away
    // before the second; with one it sounds right up to it.
    let window = SIXTEENTH * 3 / 4..SIXTEENTH - 50;
    let plain = acid_line("c2 g2 . .");
    let slide = acid_line("c2/ g2 . .");
    assert!(
        rms(&plain[window.clone()]) < 0.01,
        "{}",
        rms(&plain[window.clone()])
    );
    assert!(
        rms(&slide[window.clone()]) > 0.05,
        "{}",
        rms(&slide[window])
    );
    // The compiled notes overlap, which is what makes the Acid glide.
    let mut t = track(
        InstrumentType::Acid,
        vec![StepLane::parse(36, "c2/ g2 . .").unwrap()],
    );
    t.clips[0].length = PPQ;
    let snap = compile(&project(vec![t]), &AudioCache::new(), SR, 1);
    let SourceNode::Instrument { notes, .. } = &snap.tracks[0].source else {
        panic!("an instrument track");
    };
    assert_eq!(notes.len(), 2);
    assert!(notes[0].end > notes[1].time, "{notes:?}");
}

#[test]
fn accented_steps_play_the_acid_louder() {
    let plain = acid_line("c2 . . .");
    let accent = acid_line("X . . .");
    let head = 200..SIXTEENTH / 2;
    assert!(peak(&accent[head.clone()]) > 1.2 * peak(&plain[head]));
}

#[test]
fn the_acid_template_drifts_and_repeats_cleanly() {
    let p = acid_project();
    let end = p.tempo.tick_to_sample(p.loop_region.end, SR);
    assert_eq!(end, 360_000);
    // The bass line is 12 steps over a 16-step beat.
    let lens: Vec<Vec<usize>> = p
        .tracks
        .iter()
        .map(|t| match &t.clips[0].content {
            ClipContent::Steps(s) => s.lanes.iter().map(StepLane::len).collect(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(lens, [vec![16, 16, 16, 16], vec![12]]);
    let out = render_loop(compile(&p, &AudioCache::new(), SR, 1), 0, end, 3);
    let left = out.left();
    assert!(left.iter().all(|s| s.is_finite()));
    assert!(peak(left) > 0.1 && peak(left) <= 1.0, "{}", peak(left));
    let seam = sky_dsp::analysis::seam_check(&out, 0, out.frames());
    assert_eq!(seam.verdict, "seamless", "{seam:?}");
}
