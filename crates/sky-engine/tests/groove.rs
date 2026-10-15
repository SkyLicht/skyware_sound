// SPDX-License-Identifier: GPL-3.0-or-later
//! Pattern controls, swing, sidechain ducking and tempo-synced effects,
//! rendered.

use std::collections::BTreeMap;

use sky_core::{ClipId, PPQ, TempoMap};
use sky_dsp::{EffectType, InstrumentType};
use sky_engine::{AudioCache, compile, render, render_loop};
use sky_model::templates::techno_project;
use sky_model::*;

const SR: u32 = 48_000;
/// A beat at 120 BPM.
const BEAT: usize = 24_000;

fn pattern(source: &str, cycle: i64, controls: &[(&str, &str)], swing: Swing) -> ClipContent {
    ClipContent::Pattern {
        source: source.into(),
        seed: 0,
        cycle,
        controls: controls
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
        swing,
    }
}

fn clip(start: i64, length: i64, content: ClipContent) -> Clip {
    Clip {
        id: ClipId::new(),
        name: "c".into(),
        start,
        length,
        content,
    }
}

fn project(tracks: Vec<Track>) -> Project {
    let mut p = Project::new("t");
    p.loop_enabled = false;
    p.tracks = tracks;
    p
}

fn drums(clips: Vec<Clip>) -> Track {
    let mut t = Track::new_instrument("Drums", InstrumentType::Drums);
    t.clips = clips;
    t
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

/// The peak of each beat.
fn beat_peaks(buf: &[f32]) -> Vec<f32> {
    buf.chunks(BEAT).map(peak).collect()
}

fn first_sound(buf: &[f32]) -> Option<usize> {
    buf.iter().position(|s| s.abs() > 1e-6)
}

#[test]
fn velocity_patterns_shape_each_step() {
    // Closed hats, short enough that each beat holds only its own hit.
    let p = project(vec![drums(vec![clip(
        0,
        PPQ * 4,
        pattern(
            "hh*4",
            PPQ * 4,
            &[("velocity", "127 64 0 ~")],
            Swing::default(),
        ),
    )])]);
    let peaks = beat_peaks(&play(&p, BEAT * 4));
    // The kit's level follows velocity squared: 64 is about a quarter of
    // 127, a rest plays the usual 100 (about 0.62), and 0 is no note.
    let ratio = |i: usize| peaks[i] / peaks[0];
    assert!((0.15..0.4).contains(&ratio(1)), "{peaks:?}");
    assert!(ratio(2) < 0.01, "{peaks:?}");
    assert!((0.45..0.8).contains(&ratio(3)), "{peaks:?}");
}

#[test]
fn parameter_patterns_set_each_note_and_other_notes_get_the_usual_value() {
    let bar = PPQ * 4;
    let p = project(vec![drums(vec![
        clip(
            0,
            bar,
            pattern(
                "hh*4",
                bar,
                &[("hat_level", "0 -60 0 -60")],
                Swing::default(),
            ),
        ),
        // A MIDI hat after the pattern, whose last note muted the hats.
        clip(
            bar,
            bar,
            ClipContent::Midi {
                notes: vec![Note::new(0, PPQ / 4, 42, 100)],
            },
        ),
    ])]);
    let peaks = beat_peaks(&play(&p, BEAT * 5));
    assert!(peaks[0] > 0.01, "{peaks:?}");
    assert!(peaks[1] < 1e-4 && peaks[3] < 1e-4, "{peaks:?}");
    assert!(peaks[2] > 0.01, "{peaks:?}");
    assert!(peaks[4] > 0.01, "the MIDI note stayed muted: {peaks:?}");
}

#[test]
fn swing_delays_every_second_step() {
    // "~ hh" over an eighth note puts the hat on the second sixteenth,
    // 6000 frames in at 120 BPM; 75% swing moves it to 9000.
    let at = |swing: Swing| {
        let p = project(vec![drums(vec![clip(
            0,
            PPQ / 2,
            pattern("~ hh", PPQ / 2, &[], swing),
        )])]);
        first_sound(&play(&p, BEAT)).unwrap()
    };
    assert!((6_000..=6_001).contains(&at(Swing::default())));
    assert!((9_000..=9_001).contains(&at(Swing::new(75, 16))));
    // The offbeat eighth only moves when eighths swing.
    let offbeat = |swing: Swing| {
        let p = project(vec![drums(vec![clip(
            0,
            PPQ,
            pattern("~ ~ hh ~", PPQ, &[], swing),
        )])]);
        first_sound(&play(&p, BEAT)).unwrap()
    };
    assert!((12_000..=12_001).contains(&offbeat(Swing::new(75, 16))));
    assert!((18_000..=18_001).contains(&offbeat(Swing::new(75, 8))));
}

/// A muted kick track driving a duck on a held synth note.
fn ducked(kick_source: &str, duck: Option<&[(&str, f32)]>) -> Vec<f32> {
    let bar = PPQ * 4;
    let mut kick = drums(vec![clip(
        0,
        bar,
        pattern(kick_source, bar, &[], Swing::default()),
    )]);
    kick.channel.mute = true;
    let mut bass = Track::new_instrument("Bass", InstrumentType::Synth);
    let slot = bass.instrument.as_mut().unwrap();
    for (k, v) in [
        ("attack", 0.5),
        ("sustain", 1.0),
        ("env_amount", 0.0),
        ("detune", 0.0),
    ] {
        slot.params.insert(k.into(), v);
    }
    bass.clips.push(clip(
        0,
        bar,
        ClipContent::Midi {
            notes: vec![Note::new(0, bar, 57, 100)],
        },
    ));
    if let Some(params) = duck {
        bass.channel.effects.push(
            EffectSlot::new(EffectType::Duck)
                .with_params(params)
                .with_sidechain(kick.id),
        );
    }
    play(&project(vec![kick, bass]), BEAT * 4)
}

#[test]
fn a_duck_dips_its_track_after_each_note_on_another() {
    let params: &[(&str, f32)] = &[("depth", 1.0), ("attack", 0.0), ("length", 0.5)];
    let plain = ducked("bd*4", None);
    let duck = ducked("bd*4", Some(params));
    let ratio = |from: usize, to: usize| rms(&duck[from..to]) / rms(&plain[from..to]);
    // Right after the second kick the bass is nearly gone; half a beat
    // (12000 frames) later it is back.
    assert!(
        ratio(BEAT + 20, BEAT + 1_000) < 0.05,
        "{}",
        ratio(BEAT + 20, BEAT + 1_000)
    );
    let back = ratio(BEAT + 13_000, BEAT + 14_000);
    assert!((0.98..1.02).contains(&back), "{back}");
}

#[test]
fn a_kick_only_duck_ignores_the_other_drums() {
    let params: &[(&str, f32)] = &[
        ("depth", 1.0),
        ("attack", 0.0),
        ("length", 0.5),
        ("trigger", 1.0),
    ];
    let plain = ducked("bd hh bd hh", None);
    let duck = ducked("bd hh bd hh", Some(params));
    let ratio = |from: usize| rms(&duck[from..from + 1_000]) / rms(&plain[from..from + 1_000]);
    assert!(
        ratio(BEAT + 20) > 0.98,
        "the hat ducked: {}",
        ratio(BEAT + 20)
    );
    assert!(
        ratio(2 * BEAT + 20) < 0.05,
        "the kick did not: {}",
        ratio(2 * BEAT + 20)
    );
}

#[test]
fn synced_effects_follow_the_project_tempo() {
    // At 150 BPM a synced quarter-note delay repeats after 400 ms.
    let mut p = project(vec![drums(vec![clip(
        0,
        PPQ,
        ClipContent::Midi {
            notes: vec![Note::new(0, PPQ / 4, 36, 127)],
        },
    )])]);
    p.tempo = TempoMap::constant(150.0);
    p.tracks[0]
        .channel
        .effects
        .push(EffectSlot::new(EffectType::Delay).with_params(&[
            ("sync", 9.0),
            ("mix", 1.0),
            ("feedback", 0.0),
        ]));
    let out = play(&p, SR as usize);
    let echo = first_sound(&out).unwrap();
    assert!((19_200..=19_201).contains(&echo), "echo at {echo}");
}

#[test]
fn the_techno_template_plays_and_repeats_cleanly() {
    let p = techno_project();
    let snap = || compile(&p, &AudioCache::new(), SR, 1);
    let end = p.tempo.tick_to_sample(p.loop_region.end, SR);
    // Four bars at 128 BPM are a whole number of samples.
    assert_eq!(end, 360_000);
    let out = render_loop(snap(), 0, end, 3);
    let left = out.left();
    assert!(left.iter().all(|s| s.is_finite()));
    assert!(peak(left) > 0.1 && peak(left) <= 1.0, "{}", peak(left));
    let seam = sky_dsp::analysis::seam_check(&out, 0, out.frames());
    assert_eq!(seam.verdict, "seamless", "{seam:?}");
}
