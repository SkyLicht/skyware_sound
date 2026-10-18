// SPDX-License-Identifier: GPL-3.0-or-later
//! The sustain pedal on a piano track: stretches in MIDI clips and a
//! pattern's `sustain` control hold notes after they end, live and in an
//! offline render, and let go of them when the pedal comes up.

use std::sync::Arc;

use sky_core::{ClipId, PPQ, Ticks};
use sky_dsp::instruments::{SampleData, SampleLibrary, Zone};
use sky_dsp::{AudioData, InstrumentType, SUSTAIN_PEDAL};
use sky_engine::compile::SourceNode;
use sky_engine::{AudioCache, EngineMsg, LibraryCache, compile_with, create_engine, render};
use sky_model::*;

const SR: u32 = 48_000;
/// A quarter note at 120 BPM.
const BEAT: usize = 24_000;

/// A piano that plays a four-second sine on every key, with a 1 ms
/// release, as the built-in piano.
fn libraries() -> LibraryCache {
    let tone: Vec<f32> = (0..SR as usize * 4)
        .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / SR as f32).sin() * 0.5)
        .collect();
    let audio = AudioData {
        sample_rate: SR,
        channels: vec![tone],
    };
    let library = Arc::new(SampleLibrary::new(
        "tone",
        vec![SampleData::from_audio(&audio)],
        vec![Zone::new(0, 69)],
    ));
    [(String::new(), library)].into()
}

fn piano_project(content: ClipContent, length: Ticks) -> Project {
    let mut p = Project::new("pedal");
    p.loop_enabled = false;
    let mut t = Track::new_instrument("Piano", InstrumentType::Piano);
    t.clips.push(Clip {
        id: ClipId::new(),
        name: "c".into(),
        start: 0,
        length,
        muted: false,
        content,
    });
    p.tracks.push(t);
    p
}

/// A one-beat chord, with the pedal down for the first three beats when
/// `pedal` is set.
fn chord(pedal: bool) -> Project {
    let notes = [60u8, 64, 67]
        .iter()
        .map(|&k| Note::new(0, PPQ, k, 100))
        .collect();
    let pedal = if pedal {
        vec![Pedal::new(0, PPQ * 3)]
    } else {
        Vec::new()
    };
    piano_project(ClipContent::Midi { notes, pedal }, PPQ * 8)
}

fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

/// The loudest sample of each beat.
fn beats(buf: &[f32], n: usize) -> Vec<f32> {
    (0..n)
        .map(|b| peak(buf.get(b * BEAT..(b + 1) * BEAT).unwrap_or_default()))
        .collect()
}

fn pedal_spans(p: &Project) -> Vec<(i64, i64)> {
    let snap = compile_with(p, &AudioCache::new(), &libraries(), SR, 1);
    match &snap.tracks[0].source {
        SourceNode::Instrument { pedal, .. } => pedal.clone(),
        SourceNode::Audio(_) => Vec::new(),
    }
}

/// Plays `p` live from `from`, in blocks of 512, for `frames` frames.
fn play_live(p: &Project, from: i64, frames: usize) -> Vec<f32> {
    let (mut handle, mut engine) =
        create_engine(compile_with(p, &AudioCache::new(), &libraries(), SR, 1));
    handle.send(EngineMsg::Seek(from));
    handle.send(EngineMsg::Play);
    let mut out = Vec::with_capacity(frames);
    let (mut l, mut r) = (vec![0.0; 512], vec![0.0; 512]);
    while out.len() < frames {
        engine.process_planar(&mut l, &mut r);
        out.extend_from_slice(&l);
    }
    out.truncate(frames);
    out
}

#[test]
fn a_pedalled_chord_rings_after_its_note_offs_and_stops_on_pedal_up() {
    let offline = |p: &Project| {
        let snap = compile_with(p, &AudioCache::new(), &libraries(), SR, 1);
        render(snap, 0, BEAT * 5, false).left().to_vec()
    };
    for (how, out) in [
        ("offline", offline(&chord(true))),
        ("live", play_live(&chord(true), 0, BEAT * 5)),
    ] {
        let b = beats(&out, 5);
        // Notes end after beat 1; the pedal holds them through beat 3.
        assert!(b[0] > 0.05, "{how}: {b:?}");
        assert!(b[1] > b[0] * 0.8 && b[2] > b[0] * 0.8, "{how}: {b:?}");
        // Pedal up at beat 3: gone within the 1 ms release.
        let after = peak(&out[BEAT * 3 + 480..BEAT * 4]);
        assert!(after < 1e-3, "{how}: {after} after pedal up");
    }
    // Without the pedal the chord stops when it ends.
    let dry = offline(&chord(false));
    assert!(peak(&dry[BEAT + 480..BEAT * 2]) < 1e-3);
}

#[test]
fn starting_inside_a_stretch_puts_the_pedal_down() {
    // A short note in the middle of a stretch, played from just before it.
    let p = piano_project(
        ClipContent::Midi {
            notes: vec![Note::new(PPQ * 2, PPQ / 4, 60, 100)],
            pedal: vec![Pedal::new(0, PPQ * 4)],
        },
        PPQ * 8,
    );
    let out = play_live(&p, 2 * BEAT as i64 - 100, BEAT * 2);
    // The note ends a sixteenth after it starts but rings to the pedal.
    let ringing = peak(&out[BEAT..BEAT * 2 - 1_000]);
    assert!(ringing > 0.05, "{ringing}");
}

#[test]
fn the_pedal_stays_down_through_a_graph_swap() {
    let p = chord(true);
    let libs = libraries();
    let (mut handle, mut engine) =
        create_engine(compile_with(&p, &AudioCache::new(), &libs, SR, 1));
    handle.send(EngineMsg::Play);
    let (mut l, mut r) = (vec![0.0; BEAT], vec![0.0; BEAT]);
    // Past the note-offs, with the pedal still down.
    engine.process_planar(&mut l, &mut r);
    engine.process_planar(&mut l, &mut r);
    handle.send(EngineMsg::Swap(Box::new(compile_with(
        &p,
        &AudioCache::new(),
        &libs,
        SR,
        2,
    ))));
    engine.process_planar(&mut l, &mut r);
    assert!(peak(&l) > 0.05, "the swap let go of the pedal");
    engine.process_planar(&mut l, &mut r);
    assert!(peak(&l[480..]) < 1e-3, "the pedal never came up");
}

#[test]
fn stopping_lets_go_of_the_pedal() {
    let p = chord(true);
    let (mut handle, mut engine) =
        create_engine(compile_with(&p, &AudioCache::new(), &libraries(), SR, 1));
    handle.send(EngineMsg::Play);
    let (mut l, mut r) = (vec![0.0; BEAT * 2], vec![0.0; BEAT * 2]);
    engine.process_planar(&mut l, &mut r);
    handle.send(EngineMsg::Stop);
    let (mut l, mut r) = (vec![0.0; 4_800], vec![0.0; 4_800]);
    engine.process_planar(&mut l, &mut r);
    assert!(peak(&l[480..]) < 1e-3);
}

#[test]
fn stretches_end_with_their_clip_and_only_pianos_take_them() {
    let p = piano_project(
        ClipContent::Midi {
            notes: Vec::new(),
            pedal: vec![Pedal::new(0, PPQ * 2), Pedal::new(PPQ * 3, PPQ * 4)],
        },
        PPQ * 4,
    );
    let beat = BEAT as i64;
    assert_eq!(pedal_spans(&p), [(0, 2 * beat), (3 * beat, 4 * beat)]);
    let mut synth = p.clone();
    synth.tracks[0].instrument = Some(InstrumentSlot::new(InstrumentType::Synth));
    assert!(pedal_spans(&synth).is_empty());
}

#[test]
fn the_sustain_control_moves_the_pedal_in_patterns() {
    let bar = 4 * BEAT as i64;
    // A lane: down for the first bar of every two.
    let mut lane = ClipContent::pattern("c4 e4", 0, PPQ * 4);
    if let ClipContent::Pattern { controls, .. } = &mut lane {
        controls.insert("sustain".into(), "<1 0>".into());
    }
    let p = piano_project(lane.clone(), PPQ * 16);
    assert_eq!(pedal_spans(&p), [(0, bar), (2 * bar, 3 * bar)]);
    // The same from Strudel code, each note moving the pedal.
    let code = ClipContent::pattern("note(\"c4 e4\").sustain(\"<1 0>\")", 0, PPQ * 4);
    let p = piano_project(code, PPQ * 16);
    assert_eq!(pedal_spans(&p), [(0, bar), (2 * bar, 3 * bar)]);
    // Held up to the clip's end, when the last move is down.
    let mut held = ClipContent::pattern("c4", 0, PPQ * 4);
    if let ClipContent::Pattern { controls, .. } = &mut held {
        controls.insert("sustain".into(), "0 1".into());
    }
    let p = piano_project(held, PPQ * 7);
    let beat = BEAT as i64;
    assert_eq!(
        pedal_spans(&p),
        [(2 * beat, 4 * beat), (6 * beat, 7 * beat)]
    );
    // On a synth, `sustain` is still the envelope's sustain level.
    let mut p = piano_project(lane, PPQ * 16);
    p.tracks[0].instrument = Some(InstrumentSlot::new(InstrumentType::Synth));
    assert!(pedal_spans(&p).is_empty());
}

#[test]
fn a_pattern_chord_rings_under_the_pedal() {
    // A short chord each bar; the pedal is down in the first bar only.
    let mut c = ClipContent::pattern("[c4,e4,g4] ~ ~ ~", 0, PPQ * 4);
    if let ClipContent::Pattern { controls, .. } = &mut c {
        controls.insert("sustain".into(), "<1 0>".into());
    }
    let p = piano_project(c, PPQ * 8);
    let snap = compile_with(&p, &AudioCache::new(), &libraries(), SR, 1);
    let out = render(snap, 0, BEAT * 8, false);
    let b = beats(out.left(), 8);
    assert!(b[2] > 0.05, "held in the first bar: {b:?}");
    assert!(b[6] < 1e-3, "let go in the second: {b:?}");
}

#[test]
fn the_editor_sees_the_pedal_and_the_keys_it_holds() {
    let p = chord(true);
    let track = p.tracks[0].id;
    let (mut handle, mut engine) =
        create_engine(compile_with(&p, &AudioCache::new(), &libraries(), SR, 1));
    let shared = handle.shared().clone();
    let seen = || {
        let tap = shared.track_keys(0).expect("a slot");
        (tap.pedal(), tap.keys())
    };
    let mut play = |frames: usize| {
        let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
        engine.process_planar(&mut l, &mut r);
    };
    let chord_keys = 1u128 << 60 | 1 << 64 | 1 << 67;
    handle.send(EngineMsg::Play);
    play(512);
    assert_eq!(seen(), (true, chord_keys));
    // The keys came up after a beat; the pedal holds them to beat 3.
    play(BEAT * 2);
    assert_eq!(seen(), (true, chord_keys));
    play(BEAT);
    assert_eq!(seen(), (false, 0));

    // Stopped, the pedal can go down for keys played to hear them.
    handle.send(EngineMsg::Stop);
    handle.send(EngineMsg::Controller {
        track,
        controller: SUSTAIN_PEDAL,
        value: 127,
    });
    handle.send(EngineMsg::NoteOn {
        track,
        pitch: 72,
        velocity: 100,
        frames: None,
    });
    handle.send(EngineMsg::NoteOff { track, pitch: 72 });
    play(512);
    assert_eq!(seen(), (true, 1 << 72));
    handle.send(EngineMsg::Controller {
        track,
        controller: SUSTAIN_PEDAL,
        value: 0,
    });
    play(512);
    assert_eq!(seen(), (false, 0));
}
