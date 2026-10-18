// SPDX-License-Identifier: GPL-3.0-or-later
//! The piano end to end: its built-in library loads with the project, every
//! key sounds in a render, velocity sets the level, notes ring on through
//! changes to the project, and switching libraries is one undo step.

use sky_core::{ClipId, PPQ, TrackId};
use sky_dsp::{AudioData, InstrumentType};
use sky_model::templates::empty_project;
use sky_model::{Clip, ClipContent, Command, Note, Origin, Project, Track, Transaction};
use sky_session::Session;

/// A project with one piano track whose clip plays `notes`.
fn piano_project(notes: Vec<Note>) -> (Project, TrackId) {
    let mut project = empty_project("Piano");
    let mut track = Track::new_instrument("Piano", InstrumentType::Piano);
    let id = track.id;
    let length = notes
        .iter()
        .map(|n| n.start + n.length)
        .max()
        .unwrap_or(PPQ);
    track.clips.push(Clip {
        id: ClipId::new(),
        name: "Keys".into(),
        start: 0,
        length,
        muted: false,
        content: ClipContent::Midi { notes },
    });
    project.tracks.push(track);
    (project, id)
}

/// The loudest sample of both channels in `[from, to)` seconds.
fn peak(audio: &AudioData, from: f64, to: f64) -> f32 {
    let sr = audio.sample_rate as f64;
    let (a, b) = ((from * sr) as usize, (to * sr) as usize);
    audio
        .channels
        .iter()
        .flat_map(|c| c.get(a.min(c.len())..b.min(c.len())).unwrap_or_default())
        .fold(0.0f32, |m, s| m.max(s.abs()))
}

fn engine_peak(engine: &mut sky_engine::Engine, frames: usize) -> f32 {
    let mut l = vec![0.0f32; frames];
    let mut r = vec![0.0f32; frames];
    engine.process_planar(&mut l, &mut r);
    l.iter().chain(&r).fold(0.0f32, |m, s| m.max(s.abs()))
}

#[test]
fn every_key_sounds_and_stops() {
    // A key a second, held for a quarter of it: the release is gone well
    // before the next key.
    let step = 2 * PPQ;
    let notes = (21u8..=108)
        .enumerate()
        .map(|(i, key)| Note::new(i as i64 * step, step / 4, key, 100))
        .collect();
    let (project, _) = piano_project(notes);
    let seconds = project.tempo.tick_to_sample(step, 48_000) as f64 / 48_000.0;
    assert!((seconds - 1.0).abs() < 1e-6, "120 BPM");
    let session = Session::new(project, None).0;
    let audio = session.render_range(0, 88 * step, 0.0).unwrap();
    let mut quietest = (f32::MAX, 0u8);
    for (i, key) in (21u8..=108).enumerate() {
        let at = i as f64;
        let sounding = peak(&audio, at, at + 0.2);
        assert!(sounding > 0.01, "key {key} is silent: {sounding}");
        let tail = peak(&audio, at + 0.95, at + 1.0);
        assert!(
            tail < sounding * 0.05,
            "key {key} rings on: {tail} after {sounding}"
        );
        if sounding < quietest.0 {
            quietest = (sounding, key);
        }
    }
    // Nothing clips at a normal velocity.
    assert!(peak(&audio, 0.0, 88.0) < 1.0);
    println!("quietest key {} at {}", quietest.1, quietest.0);
}

#[test]
fn velocity_sets_the_level() {
    let (project, _) = piano_project(vec![
        Note::new(0, PPQ, 60, 30),
        Note::new(2 * PPQ, PPQ, 60, 120),
    ]);
    let session = Session::new(project, None).0;
    let audio = session.render_range(0, 4 * PPQ, 0.0).unwrap();
    let soft = peak(&audio, 0.0, 0.5);
    let loud = peak(&audio, 1.0, 1.5);
    // The default dynamics span 30 dB; 30 and 120 are about 20 dB apart.
    let db = 20.0 * (loud / soft).log10();
    assert!((15.0..25.0).contains(&db), "{db} dB from {soft} to {loud}");
}

#[test]
fn the_library_loads_with_the_project_and_notes_ring_through_changes() {
    let (project, track) = piano_project(vec![Note::new(0, PPQ, 60, 100)]);
    let (session, engine) = Session::new(project, Some(48_000));
    let mut engine = engine.unwrap();
    // The built-in piano loads on its own, without blocking.
    let started = std::time::Instant::now();
    while !session.libraries_ready() {
        assert!(started.elapsed().as_secs() < 30, "the piano never loaded");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let lib = session.track_library(track).unwrap();
    assert_eq!(lib.key_range(), Some((21, 108)));
    assert_eq!(session.track_library_error(track), None);

    engine_peak(&mut engine, 480);
    session.note_on(track, 60, 100, Some(3.0)).unwrap();
    let before = engine_peak(&mut engine, 4_800);
    assert!(before > 0.01, "{before}");
    // A change rebuilds the engine's graph; the held note carries over.
    session
        .apply(Transaction::single(Command::SetInstrumentParam {
            track,
            param: "tone".into(),
            value: Some(3.0),
        }))
        .unwrap();
    let after = engine_peak(&mut engine, 4_800);
    assert!(after > before * 0.3, "{before} then {after}");
}

#[test]
fn switching_libraries_is_one_undo_step() {
    let mut project = empty_project("Switch");
    let synth = Track::new_instrument("Keys", InstrumentType::Synth);
    let track = synth.id;
    project.tracks.push(synth);
    let session = Session::new(project, None).0;
    let tmp = tempfile::tempdir().unwrap();
    session.set_instruments(sky_library::Instruments::new(tmp.path().to_path_buf()));

    let e = session
        .set_piano_library(track, "no-such-piano", true, Origin::User)
        .unwrap_err();
    assert!(e.to_string().contains("upright-piano-kw"), "{e}");
    let e = session
        .set_piano_library(track, "upright-piano-kw", false, Origin::User)
        .unwrap_err();
    assert!(e.to_string().contains("not downloaded"), "{e}");
    assert!(session.view().history.is_empty());

    let set = session
        .set_piano_library(track, "Upright Piano KW (small)", false, Origin::User)
        .unwrap();
    assert!(!set.downloaded);
    assert_eq!((set.lowest_key, set.highest_key), (21, 108));
    assert_eq!(set.layers, 1);
    assert!(!set.stereo);
    let slot = session.project().tracks[0].instrument.clone().unwrap();
    assert_eq!(slot.kind, InstrumentType::Piano);
    // The built-in piano is what a slot without a library plays.
    assert_eq!(slot.library, None);
    assert!(session.libraries_ready());
    assert!(session.license_report().is_empty());

    session.undo().unwrap();
    let slot = session.project().tracks[0].instrument.clone().unwrap();
    assert_eq!(slot.kind, InstrumentType::Synth);
}

#[test]
fn chords_stay_below_full_scale() {
    // The workshop line, then sixteen keys at once.
    let mut notes: Vec<Note> = [36u8, 43]
        .iter()
        .map(|&k| Note::new(0, 2 * PPQ, k, 100))
        .chain(
            [52u8, 59, 62, 64]
                .iter()
                .enumerate()
                .map(|(i, &k)| Note::new(i as i64 * PPQ / 2, PPQ / 2, k, 100)),
        )
        .collect();
    notes.extend((0..16).map(|i| Note::new(4 * PPQ, 2 * PPQ, 36 + i * 3, 100)));
    let (project, _) = piano_project(notes);
    let session = Session::new(project, None).0;
    let audio = session.render_range(0, 6 * PPQ, 1.0).unwrap();
    let line = peak(&audio, 0.0, 2.0);
    let chord = peak(&audio, 2.0, 4.0);
    println!("line peak {line}, sixteen-key chord peak {chord}");
    assert!(line > 0.05, "{line}");
    assert!(chord < 1.0, "{chord}");
}
