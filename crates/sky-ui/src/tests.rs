// SPDX-License-Identifier: GPL-3.0-or-later
//! Headless tests: the whole interface runs frame by frame in an egui
//! context, with no window and no audio device.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use egui::{
    Context, Event, FontId, Id, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, pos2, vec2,
};
use sky_core::{AudioId, ClipId, PPQ};
use sky_dsp::{AudioData, InstrumentType};
use sky_model::templates::{demo_project, empty_project};
use sky_model::{
    BankSample, Clip, ClipContent, Command, License, Note, Origin, Project, SampleBank, Track,
    Transaction,
};
use sky_session::Session;

use crate::actions::{self, NewTrack};
use crate::app::{Action, App, Tab};
use crate::audition::{ButtonState, PreviewKey};
use crate::dialogs::PathPurpose;
use crate::waveform::Peaks;
use crate::widgets::Gestures;
use crate::{UiConfig, arrange, game, palette, pattern_editor, sounds, theme};

const TABS: [Tab; 4] = [Tab::Editor, Tab::Mixer, Tab::Instrument, Tab::Ai];

/// The app in a bare egui context, fed with synthetic input.
struct Harness {
    ctx: Context,
    app: App,
    time: f64,
    events: Vec<Event>,
    modifiers: Modifiers,
    screen: Rect,
    /// The audio-thread end of the engine, run by hand, when there is one.
    engine: Option<sky_engine::Engine>,
    /// Files dropped on the window in the next frame.
    dropped: Vec<egui::DroppedFileHandle>,
    /// Files held over the window.
    hovered: Vec<egui::HoveredFile>,
}

impl Harness {
    fn with_config(project: Project, config: UiConfig) -> Self {
        Self::build(project, config, None)
    }

    /// With an engine at 48 kHz that [`heard`](Self::heard) runs.
    fn with_engine(project: Project) -> Self {
        Self::build(project, UiConfig::default(), Some(48_000))
    }

    fn build(project: Project, config: UiConfig, engine_rate: Option<u32>) -> Self {
        let (session, engine) = Session::new(project, engine_rate);
        let ctx = Context::default();
        theme::apply(&ctx);
        let app = App::new(session, config, false);
        let mut h = Self {
            ctx,
            app,
            time: 0.0,
            events: Vec::new(),
            modifiers: Modifiers::NONE,
            screen: Rect::from_min_size(Pos2::ZERO, vec2(1440.0, 900.0)),
            engine,
            dropped: Vec::new(),
            hovered: Vec::new(),
        };
        h.frames(3);
        h
    }

    /// Runs the engine for 50 ms and returns the loudest sample.
    fn heard(&mut self) -> f32 {
        let engine = self.engine.as_mut().expect("an engine");
        let mut l = vec![0.0f32; 2_400];
        let mut r = vec![0.0f32; 2_400];
        engine.process_planar(&mut l, &mut r);
        l.iter().chain(&r).fold(0.0, |m, s| m.max(s.abs()))
    }

    /// Runs the engine until what it plays dies away.
    fn quiet(&mut self) {
        for _ in 0..40 {
            self.heard();
        }
    }

    /// Where a widget with this id was last laid out.
    fn rect_of(&self, id: Id) -> Rect {
        self.ctx
            .read_response(id)
            .unwrap_or_else(|| panic!("no widget {id:?}"))
            .rect
    }

    fn with_project(project: Project) -> Self {
        Self::with_config(project, UiConfig::default())
    }

    fn demo() -> Self {
        Self::with_project(demo_project())
    }

    fn frame(&mut self) {
        let input = RawInput {
            screen_rect: Some(self.screen),
            time: Some(self.time),
            events: std::mem::take(&mut self.events),
            dropped_files: std::mem::take(&mut self.dropped),
            hovered_files: self.hovered.clone(),
            ..RawInput::default()
        };
        let app = &mut self.app;
        let mut out = self.ctx.run_ui(input, |ui| app.frame(ui));
        // No renderer here to take the font atlas.
        out.textures_delta.clear();
        self.time += 1.0 / 60.0;
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.frame();
        }
    }

    fn ctx(&self) -> Context {
        self.ctx.clone()
    }

    fn project(&self) -> Arc<Project> {
        self.app.session.project()
    }

    fn history_len(&self) -> usize {
        self.app.session.view().history.len()
    }

    fn set_modifiers(&mut self, modifiers: Modifiers) {
        self.modifiers = modifiers;
        self.events.push(Event::ModifiersChanged(modifiers));
    }

    fn pointer(&mut self, pos: Pos2, pressed: bool) {
        self.events.push(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: self.modifiers,
        });
    }

    fn move_to(&mut self, pos: Pos2) {
        self.events.push(Event::PointerMoved(pos));
        self.frame();
    }

    fn click(&mut self, pos: Pos2) {
        self.move_to(pos);
        self.pointer(pos, true);
        self.frame();
        self.pointer(pos, false);
        self.frame();
    }

    fn double_click(&mut self, pos: Pos2) {
        self.move_to(pos);
        for _ in 0..2 {
            self.pointer(pos, true);
            self.frame();
            self.pointer(pos, false);
            self.frame();
        }
        self.frames(2);
    }

    fn drag(&mut self, from: Pos2, to: Pos2) {
        self.move_to(from);
        self.pointer(from, true);
        self.frame();
        for step in 1..=10 {
            self.move_to(from.lerp(to, step as f32 / 10.0));
        }
        self.pointer(to, false);
        self.frame();
        self.frames(2);
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) {
        for pressed in [true, false] {
            self.events.push(Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            });
            self.frame();
        }
    }

    fn type_text(&mut self, text: &str) {
        self.events.push(Event::Text(text.to_string()));
        self.frame();
    }

    /// Marks the known sample packs as unreadable, so a search in the
    /// browser does not go to the network for them.
    fn offline_packs(&mut self) {
        for info in sky_library::PACKS {
            self.app
                .browser_pack_read(info.map.to_string(), Err("not read in tests".into()));
        }
    }

    /// Waits for background jobs and hands their results to the app.
    fn wait_jobs(&mut self) {
        let ctx = self.ctx();
        for out in self.app.jobs.wait_all(Duration::from_secs(120)) {
            self.app.handle_job(&ctx, out);
        }
        self.frames(2);
    }

    fn run(&mut self, action: Action) {
        let ctx = self.ctx();
        self.app.run_action(&ctx, action);
        self.frames(2);
    }

    /// Renders every bottom tab once.
    fn every_tab(&mut self) {
        for tab in TABS {
            self.app.tab = tab;
            self.frames(2);
        }
    }

    fn track_named(&self, name: &str) -> Track {
        self.project()
            .tracks
            .iter()
            .find(|t| t.name == name)
            .cloned()
            .unwrap_or_else(|| panic!("no track named {name}"))
    }

    fn clip(&self, id: ClipId) -> Clip {
        self.project()
            .find_clip(id)
            .map(|(_, c)| c.clone())
            .expect("clip exists")
    }

    /// Lays out so that `bars` bars fill the lanes.
    fn show_bars(&mut self, bars: i64) {
        let bar = self.project().ticks_per_bar();
        self.app.arrange.fit(bar * bars);
        self.frames(2);
    }
}

const COMMAND: Modifiers = Modifiers {
    alt: false,
    ctrl: true,
    shift: false,
    mac_cmd: false,
    command: true,
};

fn cmd_shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..COMMAND
    }
}

#[test]
fn demo_project_renders_everywhere() {
    let mut h = Harness::demo();
    h.every_tab();
    // With a clip of each kind selected.
    for t in h.project().tracks.clone() {
        h.app.sel.select_track(t.id);
        h.every_tab();
        for c in &t.clips {
            h.app.sel.select_clip(t.id, c.id);
            h.every_tab();
        }
    }
    // Every dialog and the palette open at once.
    h.app.palette.show_palette();
    h.app.open_export_loop();
    h.app.open_export_wav();
    h.app.open_game_assets();
    h.frames(3);
    assert!(h.app.palette.open);
    assert!(h.app.dialogs.game.is_some());
    assert!(h.app.dialogs.export_loop.is_some());
    // Escape closes the palette.
    h.key(Key::Escape, Modifiers::NONE);
    h.frames(1);
    assert!(!h.app.palette.open);
}

#[test]
fn smallest_window_still_lays_out() {
    let mut h = Harness::demo();
    h.screen = Rect::from_min_size(Pos2::ZERO, vec2(960.0, 600.0));
    h.frames(2);
    for t in h.project().tracks.clone() {
        if let Some(c) = t.clips.first() {
            h.app.sel.select_clip(t.id, c.id);
        }
        h.every_tab();
    }
    // Lanes still get drawn.
    assert!(!h.app.arrange.lanes.is_empty());
    // And a tiny window does not panic either.
    h.screen = Rect::from_min_size(Pos2::ZERO, vec2(200.0, 120.0));
    h.every_tab();
}

#[test]
fn empty_and_odd_projects_do_not_panic() {
    let mut h = Harness::with_project(empty_project("Nothing here"));
    h.every_tab();
    h.show_bars(8);
    h.click(pos2(700.0, 300.0));
    h.key(Key::Delete, Modifiers::NONE);
    h.key(Key::Z, COMMAND);
    h.key(Key::Space, Modifiers::NONE);
    h.every_tab();

    // Clips the UI must survive: zero length, missing audio, a broken
    // pattern, a zero-length note.
    let mut p = demo_project();
    let mut synth = Track::new_instrument("Odd synth", InstrumentType::Synth);
    synth.clips.push(Clip {
        id: ClipId::new(),
        name: "Empty".into(),
        start: 0,
        length: 0,
        content: ClipContent::Midi {
            notes: vec![Note::new(0, 0, 60, 100)],
        },
    });
    synth.clips.push(Clip {
        id: ClipId::new(),
        name: "Broken".into(),
        start: 3840,
        length: 3840,
        content: ClipContent::pattern("c4 [e4", 0, 0),
    });
    let mut audio = Track::new_audio("Odd audio");
    audio.clips.push(Clip {
        id: ClipId::new(),
        name: "Missing file".into(),
        start: 0,
        length: 3840,
        content: ClipContent::Audio {
            source: AudioId::new(),
            offset: 0,
            gain_db: 0.0,
            fade_in: 0,
            fade_out: 0,
        },
    });
    p.tracks.push(synth);
    p.tracks.push(audio);
    let mut h = Harness::with_project(p);
    for t in h.project().tracks.clone() {
        for c in &t.clips {
            h.app.sel.select_clip(t.id, c.id);
            h.every_tab();
        }
    }
    // Zoomed all the way in and out.
    for _ in 0..30 {
        h.app.arrange.px_per_tick *= 2.0;
        h.frame();
    }
    for _ in 0..60 {
        h.app.arrange.px_per_tick /= 2.0;
        h.frame();
    }
    h.every_tab();
}

#[test]
fn actions_change_the_project_and_undo_restores_it() {
    let mut h = Harness::demo();
    let before = h.project();
    let tracks = before.tracks.len();

    // Add a track, as the toolbar button does.
    h.run(Action::AddTrack(NewTrack::Instrument(
        InstrumentType::Synth,
    )));
    let p = h.project();
    assert_eq!(p.tracks.len(), tracks + 1);
    let track = h.app.sel.track.expect("new track selected");
    assert_eq!(h.app.tab, Tab::Instrument);
    let new = p.track(track).expect("track exists");
    assert_eq!(new.kind, sky_model::TrackKind::Instrument);
    assert!(new.clips.is_empty());

    // Add a clip, as a double-click does.
    let clip = arrange::create_clip_at(&mut h.app, track, 0.0).expect("clip added");
    h.frames(2);
    let bar = h.project().ticks_per_bar();
    assert_eq!(h.clip(clip).length, bar);
    assert_eq!(h.app.sel.clip, Some(clip));

    // Move it two bars right.
    let r = actions::move_clip(&h.app.session, clip, track, bar * 2);
    assert!(r.is_ok(), "{r:?}");
    h.frames(2);
    assert_eq!(h.clip(clip).start, bar * 2);

    // Set a parameter.
    let r = actions::set_instrument_param(&h.app.session, track, "cutoff", Some(1234.0), None);
    assert!(r.is_ok(), "{r:?}");
    h.frames(2);
    let p = h.project();
    let inst = p
        .track(track)
        .ok()
        .and_then(|t| t.instrument.clone())
        .expect("synth");
    assert_eq!(inst.params.get("cutoff").copied(), Some(1234.0));
    assert!(h.app.view.dirty);

    // Four undos with the shortcut take it all back.
    for _ in 0..4 {
        h.key(Key::Z, COMMAND);
    }
    h.frames(2);
    assert_eq!(*h.project(), *before);
    assert!(h.app.sel.track.is_none_or(|t| h.project().track(t).is_ok()));
    assert!(h.app.sel.clip.is_none());

    // Redo brings the track back.
    h.key(Key::Z, cmd_shift());
    assert_eq!(h.project().tracks.len(), tracks + 1);
    h.key(Key::Y, COMMAND);
    h.frames(1);
    assert!(h.project().find_clip(clip).is_ok());
}

#[test]
fn dragging_a_clip_moves_it_in_one_undo_step() {
    let mut h = Harness::demo();
    h.show_bars(10);
    let bass = h.track_named("Bass");
    let clip = bass.clips[0].id;
    let start = h.clip(clip).start;
    let r = arrange::clip_screen_rect(&h.app, clip).expect("clip visible");
    let bar = h.project().ticks_per_bar();
    let bar_px = h.app.arrange.px_per_tick * bar as f32;
    let history = h.history_len();

    let from = pos2(r.left() + 40.0, r.center().y);
    h.drag(from, from + vec2(bar_px, 0.0));
    assert_eq!(h.clip(clip).start, start + bar);
    assert_eq!(h.history_len(), history + 1, "one drag is one undo step");
    assert_eq!(h.app.sel.clip, Some(clip));
    assert!(h.app.arrange.drag.is_none());

    // Snapping: a drag of a beat and a bit lands on a beat.
    let r = arrange::clip_screen_rect(&h.app, clip).expect("clip visible");
    let from = pos2(r.left() + 40.0, r.center().y);
    let beat_px = h.app.arrange.px_per_tick * PPQ as f32;
    h.drag(from, from + vec2(beat_px * 1.3, 0.0));
    assert_eq!(h.clip(clip).start, start + bar + PPQ);

    // Alt places freely.
    let r = arrange::clip_screen_rect(&h.app, clip).expect("clip visible");
    let from = pos2(r.left() + 40.0, r.center().y);
    h.set_modifiers(Modifiers::ALT);
    h.drag(from, from + vec2(beat_px * 0.37, 0.0));
    h.set_modifiers(Modifiers::NONE);
    h.frame();
    let free = h.clip(clip).start;
    assert_ne!(free % PPQ, 0, "an Alt drag does not snap: {free}");

    // Undo goes back one drag at a time.
    h.run(Action::Undo);
    assert_eq!(h.clip(clip).start, start + bar + PPQ);
    h.run(Action::Undo);
    h.run(Action::Undo);
    assert_eq!(h.clip(clip).start, start);
}

#[test]
fn dragging_a_clip_to_another_lane_and_resizing_it() {
    let mut h = Harness::demo();
    h.show_bars(10);
    let bass = h.track_named("Bass");
    let pad = h.track_named("Pad");
    let clip = bass.clips[0].id;
    let length = h.clip(clip).length;
    let bar = h.project().ticks_per_bar();
    let bar_px = h.app.arrange.px_per_tick * bar as f32;

    // Onto the pad lane, two bars later so it does not sit on the pad's clip.
    let r = arrange::clip_screen_rect(&h.app, clip).expect("clip visible");
    let pad_lane = h
        .app
        .arrange
        .lanes
        .iter()
        .find(|(id, _)| *id == pad.id)
        .map(|(_, r)| *r)
        .expect("pad lane visible");
    let from = pos2(r.left() + 40.0, r.center().y);
    let to = pos2(from.x + bar_px * 4.0, pad_lane.center().y);
    h.drag(from, to);
    let p = h.project();
    let (track, moved) = p.find_clip(clip).expect("clip");
    assert_eq!(track.id, pad.id);
    assert_eq!(moved.start, bar * 4);

    // Resize from the right edge by one bar.
    let r = arrange::clip_screen_rect(&h.app, clip).expect("clip visible");
    let from = pos2(r.right() - 2.0, r.center().y);
    let history = h.history_len();
    h.drag(from, from + vec2(bar_px, 0.0));
    assert_eq!(h.clip(clip).length, length + bar);
    assert_eq!(h.history_len(), history + 1);
}

#[test]
fn double_click_on_an_empty_lane_adds_a_bar_long_clip() {
    let mut h = Harness::demo();
    h.show_bars(10);
    let pad = h.track_named("Pad");
    let clips = pad.clips.len();
    let lane = h
        .app
        .arrange
        .lanes
        .iter()
        .find(|(id, _)| *id == pad.id)
        .map(|(_, r)| *r)
        .expect("pad lane visible");
    let bar = h.project().ticks_per_bar();
    let x = h.app.arrange.x_of((bar * 6 + PPQ / 2) as f64);
    h.double_click(pos2(x, lane.center().y));
    let pad = h.track_named("Pad");
    assert_eq!(pad.clips.len(), clips + 1);
    let new = pad
        .clips
        .iter()
        .find(|c| c.start == bar * 6 && c.length == bar);
    let new = new.expect("a one-bar clip at bar 7");
    assert!(matches!(new.content, ClipContent::Midi { .. }));
    assert_eq!(h.app.sel.clip, Some(new.id));
    assert_eq!(h.app.tab, Tab::Editor);
    // The piano roll shows it.
    h.frames(3);

    // Delete removes the selection; Ctrl+Z brings it back.
    h.key(Key::Delete, Modifiers::NONE);
    assert_eq!(h.track_named("Pad").clips.len(), clips);
    h.key(Key::Z, COMMAND);
    assert_eq!(h.track_named("Pad").clips.len(), clips + 1);
}

#[test]
fn drags_coalesce_into_one_undo_step() {
    let h = Harness::demo();
    let s = &h.app.session;
    let bass = h.track_named("Bass");
    let history = h.history_len();
    let tempo = h.project().tempo.initial_bpm();
    let cutoff = bass
        .instrument
        .as_ref()
        .and_then(|i| i.params.get("cutoff").copied());

    // One tempo drag: many updates under one key.
    for bpm in [121.0, 124.5, 130.0] {
        assert!(actions::set_tempo(s, bpm, Some(77)).is_ok());
    }
    assert_eq!(h.history_len(), history + 1);
    assert_eq!(h.project().tempo.initial_bpm(), 130.0);

    // One parameter slider drag.
    for v in [800.0, 900.0, 1000.0, 1100.0] {
        let r = actions::set_instrument_param(s, bass.id, "cutoff", Some(v), Some(78));
        assert!(r.is_ok(), "{r:?}");
    }
    assert_eq!(h.history_len(), history + 2);

    // One fader drag.
    for db in [-3.0, -6.0, -9.0] {
        let change = actions::ChannelChange {
            volume_db: Some(db),
            ..Default::default()
        };
        let r = actions::set_channel(s, sky_model::ChannelRef::Track(bass.id), change, Some(79));
        assert!(r.is_ok(), "{r:?}");
    }
    assert_eq!(h.history_len(), history + 3);

    assert!(s.undo().is_ok());
    assert!(s.undo().is_ok());
    let p = h.project();
    let back = p
        .track(bass.id)
        .ok()
        .and_then(|t| t.instrument.as_ref())
        .and_then(|i| i.params.get("cutoff").copied());
    assert_eq!(back, cutoff);
    assert!(s.undo().is_ok());
    assert_eq!(h.project().tempo.initial_bpm(), tempo);
}

#[test]
fn dragging_a_header_volume_slider_is_one_undo_step() {
    let mut h = Harness::demo();
    let bass = h.track_named("Bass");
    let lane = h
        .app
        .arrange
        .lanes
        .iter()
        .find(|(id, _)| *id == bass.id)
        .map(|(_, r)| *r)
        .expect("bass lane visible");
    let left = lane.left() - arrange::HEADER_W;
    let history = h.history_len();
    // The volume slider sits on the header's second row.
    let y = lane.top() + 44.0;
    h.drag(pos2(left + 150.0, y), pos2(left + 90.0, y));
    let volume = h.track_named("Bass").channel.volume_db;
    assert!(volume < bass.channel.volume_db - 1.0, "volume is {volume}");
    assert_eq!(h.history_len(), history + 1, "one drag is one undo step");
    h.run(Action::Undo);
    assert_eq!(
        h.track_named("Bass").channel.volume_db,
        bass.channel.volume_db
    );
}

#[test]
fn changing_audio_settings_restarts_audio() {
    let mut h = Harness::demo();
    h.run(Action::OpenSettings);
    assert!(h.app.dialogs.settings.is_some());
    h.wait_jobs();
    assert!(
        h.app
            .dialogs
            .settings
            .as_ref()
            .is_some_and(|s| s.devices.is_some())
    );
    if let Some(s) = h.app.dialogs.settings.as_mut() {
        s.buffer = Some(256);
        s.mode = sky_io::device::OutputMode::Exclusive;
    }
    h.frames(2);
    assert_eq!(h.app.audio.config.buffer_frames, Some(256));
    assert_eq!(
        h.app.audio.config.mode,
        sky_io::device::OutputMode::Exclusive
    );
    // There is no device here, so the restart reports why.
    if h.app.audio.stream.is_none() {
        assert!(h.app.audio.error.is_some());
        assert!(h.app.notice.as_ref().is_some_and(|n| n.error));
    }
    h.app.audio.stop(&h.app.session);
}

#[test]
fn gesture_keys_last_while_the_button_is_held() {
    let mut h = Harness::demo();
    let mut g = Gestures::default();
    let a = Id::new("a");
    let b = Id::new("b");
    let ka = g.key(a);
    assert_eq!(g.key(a), ka);
    assert_ne!(g.key(b), ka);
    // Pointer down: keys survive the next frame.
    h.move_to(pos2(5.0, 5.0));
    h.pointer(pos2(5.0, 5.0), true);
    h.frame();
    g.begin_frame(&h.ctx);
    assert_eq!(g.key(a), ka);
    // Released: the next drag is a new step.
    h.pointer(pos2(5.0, 5.0), false);
    h.frame();
    g.begin_frame(&h.ctx);
    assert_ne!(g.key(a), ka);
}

#[test]
fn space_plays_but_not_while_typing() {
    let mut h = Harness::demo();
    // There is no engine here, so play reports that it cannot.
    h.key(Key::Space, Modifiers::NONE);
    let notice = h
        .app
        .notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default();
    assert!(notice.starts_with("Cannot play"), "{notice}");

    h.app.notice = None;
    let track = h.track_named("Bass");
    crate::tracks::start_rename(&mut h.app, &track);
    h.frames(3);
    assert!(h.ctx.text_edit_focused(), "the rename box has focus");
    h.key(Key::Space, Modifiers::NONE);
    assert!(h.app.notice.is_none(), "space went to the text box");
}

#[test]
fn renaming_a_track_in_its_header() {
    let mut h = Harness::demo();
    let track = h.track_named("Bass");
    crate::tracks::start_rename(&mut h.app, &track);
    h.frames(2);
    // Select all, type a new name, press Enter.
    h.key(Key::A, COMMAND);
    h.type_text("Sub bass");
    h.key(Key::Enter, Modifiers::NONE);
    h.frames(2);
    let p = h.project();
    assert_eq!(
        p.track(track.id).map(|t| t.name.as_str()).ok(),
        Some("Sub bass")
    );
}

#[test]
fn palette_runs_commands() {
    let mut h = Harness::demo();
    let entries = palette::entries(&h.app);
    let reverb = palette::filter(entries, "reverb master");
    assert!(
        reverb
            .first()
            .is_some_and(|e| e.label == "Add Reverb to master"),
        "{:?}",
        reverb.iter().map(|e| &e.label).collect::<Vec<_>>()
    );
    assert_eq!(palette::tempo_in("tempo 128"), Some(128.0));
    assert_eq!(palette::tempo_in("95 bpm"), Some(95.0));
    assert_eq!(palette::tempo_in("tempo"), None);
    assert_eq!(palette::tempo_in("add synth"), None);

    // Ctrl+K, type, Enter.
    h.key(Key::K, COMMAND);
    assert!(h.app.palette.open);
    h.frames(1);
    h.type_text("tempo 99");
    assert_eq!(h.app.palette.query, "tempo 99");
    h.key(Key::Enter, Modifiers::NONE);
    assert!(!h.app.palette.open);
    assert_eq!(h.project().tempo.initial_bpm(), 99.0);

    // And an effect on the master.
    h.key(Key::K, COMMAND);
    h.frames(1);
    h.type_text("add delay to master");
    h.key(Key::Enter, Modifiers::NONE);
    let fx = &h.project().master.effects;
    assert!(fx.iter().any(|e| e.kind == sky_dsp::EffectType::Delay));
    assert!(h.app.mixer.open_effect.is_some(), "the new effect opens");
    h.frames(3);
}

#[test]
fn pattern_status_and_mcp_command() {
    match pattern_editor::parse_status("bd*4, hh*8", 0) {
        pattern_editor::ParseStatus::Ok { events } => assert_eq!(events, 12),
        other => panic!("{other:?}"),
    }
    match pattern_editor::parse_status("c4 [e4", 0) {
        pattern_editor::ParseStatus::Error { column, .. } => assert!(column > 0),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        crate::ai_panel::mcp_command("http://127.0.0.1:7777/mcp", "abc"),
        "claude mcp add --transport http skyware http://127.0.0.1:7777/mcp --header \"Authorization: Bearer abc\""
    );
}

#[test]
fn pattern_edits_apply_on_enter() {
    let mut h = Harness::demo();
    let drums = h.track_named("Drums");
    let clip = drums.clips[0].id;
    let original = match &h.clip(clip).content {
        ClipContent::Pattern { source, .. } => source.clone(),
        other => panic!("{other:?}"),
    };
    h.app.sel.select_clip(drums.id, clip);
    h.app.tab = Tab::Editor;
    h.frames(3);
    let text_id = Id::new(("pattern_text", clip));
    h.ctx.memory_mut(|m| m.request_focus(text_id));
    h.frames(2);
    // A typo is not applied.
    h.key(Key::A, COMMAND);
    h.type_text("bd*2 [hh");
    assert_eq!(h.app.pattern.text, "bd*2 [hh");
    h.key(Key::Enter, Modifiers::NONE);
    let notice = h
        .app
        .notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default();
    assert!(notice.starts_with("Pattern not applied"), "{notice}");
    match &h.clip(clip).content {
        ClipContent::Pattern { source, .. } => assert_eq!(*source, original),
        other => panic!("{other:?}"),
    }
    // A good pattern is applied on Enter.
    h.ctx.memory_mut(|m| m.request_focus(text_id));
    h.frames(2);
    h.key(Key::A, COMMAND);
    h.type_text("bd*2, hh*4");
    h.key(Key::Enter, Modifiers::NONE);
    match &h.clip(clip).content {
        ClipContent::Pattern { source, .. } => assert_eq!(source, "bd*2, hh*4"),
        other => panic!("{other:?}"),
    }
    h.run(Action::Undo);
    match &h.clip(clip).content {
        ClipContent::Pattern { source, .. } => assert_eq!(*source, original),
        other => panic!("{other:?}"),
    }
    assert_eq!(h.app.pattern.text, original, "the box follows the undo");
}

#[test]
fn pattern_controls_apply_on_enter_and_follow_undo() {
    let mut h = Harness::demo();
    let drums = h.track_named("Drums");
    let clip = drums.clips[0].id;
    let controls = |h: &Harness| match &h.clip(clip).content {
        ClipContent::Pattern { controls, .. } => controls.clone(),
        other => panic!("{other:?}"),
    };
    h.app.sel.select_clip(drums.id, clip);
    h.app.tab = Tab::Editor;
    h.frames(3);
    h.app
        .report(actions::set_pattern_control(
            &h.app.session,
            clip,
            "velocity",
            Some("100"),
        ))
        .unwrap();
    h.frames(2);
    assert_eq!(h.app.pattern.controls.get("velocity").unwrap(), "100");
    let text_id = Id::new(("control_text", clip, "velocity"));
    // A name where numbers go is not applied.
    h.ctx.memory_mut(|m| m.request_focus(text_id));
    h.frames(2);
    h.key(Key::A, COMMAND);
    h.type_text("127 bd");
    h.key(Key::Enter, Modifiers::NONE);
    let notice = h
        .app
        .notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default();
    assert!(notice.starts_with("velocity not applied"), "{notice}");
    assert_eq!(controls(&h)["velocity"], "100");
    // Numbers are.
    h.ctx.memory_mut(|m| m.request_focus(text_id));
    h.frames(2);
    h.key(Key::A, COMMAND);
    h.type_text("127 0 90 60");
    h.key(Key::Enter, Modifiers::NONE);
    assert_eq!(controls(&h)["velocity"], "127 0 90 60");
    h.run(Action::Undo);
    assert_eq!(controls(&h)["velocity"], "100");
    assert_eq!(
        h.app.pattern.controls["velocity"], "100",
        "the box follows the undo"
    );
    // Swing and removal go through the same command.
    h.app
        .report(actions::set_swing(
            &h.app.session,
            clip,
            sky_model::Swing::new(62, 16),
            None,
        ))
        .unwrap();
    h.app
        .report(actions::set_pattern_control(
            &h.app.session,
            clip,
            "velocity",
            None,
        ))
        .unwrap();
    h.frames(2);
    match &h.clip(clip).content {
        ClipContent::Pattern {
            controls, swing, ..
        } => {
            assert!(controls.is_empty());
            assert_eq!(swing.amount, 62);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_new_duck_follows_the_drums() {
    let mut h = Harness::demo();
    let drums = h.track_named("Drums");
    let bass = h.track_named("Bass");
    let id = h
        .app
        .report(actions::add_effect(
            &h.app.session,
            sky_model::ChannelRef::Track(bass.id),
            sky_dsp::EffectType::Duck,
        ))
        .unwrap();
    let bass = h.track_named("Bass");
    assert_eq!(bass.channel.effect(id).unwrap().sidechain, Some(drums.id));
    // The window shows who it follows and survives the drums being deleted.
    h.app.mixer.open_effect = Some((sky_model::ChannelRef::Track(bass.id), id));
    h.app.tab = Tab::Mixer;
    h.frames(3);
    h.app
        .report(actions::apply(
            &h.app.session,
            Transaction::single(Command::RemoveTrack { id: drums.id }),
            None,
        ))
        .unwrap();
    h.frames(3);
    assert!(h.app.mixer.open_effect.is_some());
}

#[test]
fn the_techno_template_renders_everywhere() {
    let mut h = Harness::with_project(sky_model::templates::techno_project());
    h.every_tab();
    for t in h.project().tracks.clone() {
        for c in &t.clips {
            h.app.sel.select_clip(t.id, c.id);
            h.every_tab();
        }
        for e in &t.channel.effects {
            h.app.mixer.open_effect = Some((sky_model::ChannelRef::Track(t.id), e.id));
            h.frames(2);
        }
    }
}

#[test]
fn ai_changes_flash_the_indicator_and_can_be_undone() {
    let mut h = Harness::with_config(
        demo_project(),
        UiConfig {
            mcp: Some(crate::McpInfo {
                url: "http://127.0.0.1:7777/mcp".into(),
                token: "secret-token".into(),
            }),
            ..UiConfig::default()
        },
    );
    assert!(!h.app.ai.flashing(h.app.now));
    let tracks = h.project().tracks.len();
    let mcp = Origin::Mcp {
        client: "test-client".into(),
        tool: "add_track".into(),
    };
    let track = Track::new_instrument("From AI", InstrumentType::Synth);
    let tx = Transaction::new(
        "Add track From AI",
        mcp,
        vec![Command::AddTrack {
            track: Box::new(track),
            index: None,
        }],
    );
    assert!(h.app.session.apply(tx).is_ok());
    h.frames(2);
    assert_eq!(h.project().tracks.len(), tracks + 1);
    assert!(h.app.ai.flashing(h.app.now));
    assert!(
        h.app
            .ai
            .latest
            .as_deref()
            .is_some_and(|l| l.contains("test-client"))
    );
    h.app.tab = Tab::Ai;
    h.frames(3);
    let seq = h.app.view.activity.last().map(|a| a.seq).expect("activity");
    assert!(h.app.session.undo_to(seq).is_ok());
    h.frames(2);
    assert_eq!(h.project().tracks.len(), tracks);
    assert!(h.app.view.activity.last().is_some_and(|a| a.undone));
    // The flash fades.
    h.time += 5.0;
    h.frames(1);
    assert!(!h.app.ai.flashing(h.app.now));
}

#[test]
fn stress_option_builds_many_tracks() {
    let mut h = Harness::with_config(
        demo_project(),
        UiConfig {
            demo_tracks: Some(64),
            ..UiConfig::default()
        },
    );
    assert_eq!(h.project().tracks.len(), 64);
    h.every_tab();
    // Scroll down through the rows.
    for _ in 0..10 {
        h.events.push(Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -400.0),
            phase: egui::TouchPhase::Move,
            modifiers: Modifiers::NONE,
        });
        h.move_to(pos2(700.0, 300.0));
    }
    h.frames(3);
}

fn wait_for_file(path: &Path) {
    assert!(path.exists(), "{} was not written", path.display());
}

#[test]
fn save_reopen_and_ask_before_discarding() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut h = Harness::demo();
    let tracks = h.project().tracks.len();
    let ctx = h.ctx();

    // Not saved yet: Save asks for a path (typed, as there is no native dialog).
    h.run(Action::Save);
    let prompt = h.app.dialogs.path_prompt.as_ref().expect("path prompt");
    assert_eq!(prompt.purpose, PathPurpose::SaveAs);
    assert!(prompt.text.ends_with(".skyproj"), "{}", prompt.text);
    h.app.dialogs.path_prompt = None;
    let target = dir.path().join("My song");
    assert_eq!(h.app.path_chosen(&ctx, PathPurpose::SaveAs, target), None);
    assert!(h.app.jobs.is_busy(), "saving runs on a thread");
    h.wait_jobs();
    let saved = dir.path().join("My song.skyproj");
    wait_for_file(&saved.join(sky_model::file::PROJECT_FILE));
    assert_eq!(h.app.view.path.as_deref(), Some(saved.as_path()));
    assert!(!h.app.view.dirty);

    // A change, then New: the app asks first.
    h.run(Action::AddTrack(NewTrack::Audio));
    assert!(h.app.view.dirty);
    h.run(Action::NewEmpty);
    assert!(h.app.dialogs.confirm.is_some());
    assert_eq!(h.project().tracks.len(), tracks + 1);
    h.app.dialogs.confirm = None;

    // Saving to the known path needs no prompt.
    h.key(Key::S, COMMAND);
    assert!(h.app.dialogs.path_prompt.is_none());
    h.wait_jobs();
    assert!(!h.app.view.dirty);
    h.run(Action::NewEmpty);
    assert!(h.app.dialogs.confirm.is_none());
    assert!(h.project().tracks.is_empty());

    // Open it again.
    assert_eq!(
        h.app
            .path_chosen(&ctx, PathPurpose::OpenProject, saved.clone()),
        None
    );
    h.wait_jobs();
    assert_eq!(h.project().tracks.len(), tracks + 1);

    // A folder that is not a project is refused with a message.
    let err = h
        .app
        .path_chosen(&ctx, PathPurpose::OpenProject, dir.path().to_path_buf());
    assert!(err.is_some());
}

#[test]
fn exports_run_on_threads() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut h = Harness::demo();
    let ctx = h.ctx();

    // Loop for a game.
    h.app.open_export_loop();
    if let Some(d) = h.app.dialogs.export_loop.as_mut() {
        d.folder = dir.path().join("game").display().to_string();
        d.name = "Demo loop".into();
        d.bits = 16;
    }
    h.app.start_loop_export(&ctx);
    assert!(
        h.app
            .dialogs
            .export_loop
            .as_ref()
            .is_some_and(|d| d.running)
    );
    h.frames(2);
    h.wait_jobs();
    let d = h
        .app
        .dialogs
        .export_loop
        .as_ref()
        .expect("dialog stays open");
    assert!(!d.running);
    match &d.result {
        Some(Ok(s)) => {
            wait_for_file(&s.file);
            wait_for_file(&s.manifest);
            assert!(!s.verdict.is_empty());
            assert!(s.bars > 0.0);
        }
        other => panic!(
            "loop export failed: {:?}",
            other.as_ref().map(|r| r.as_ref().err())
        ),
    }
    h.frames(2);
    h.app.dialogs.export_loop = None;

    // Plain WAV, then import it back as audio.
    h.app.open_export_wav();
    let wav = dir.path().join("song.wav");
    if let Some(d) = h.app.dialogs.export_wav.as_mut() {
        d.path = dir.path().join("song").display().to_string();
        d.tail = 0.5;
    }
    h.app.start_wav_export(&ctx);
    h.wait_jobs();
    let d = h
        .app
        .dialogs
        .export_wav
        .as_ref()
        .expect("dialog stays open");
    match &d.result {
        Some(Ok(s)) => {
            assert_eq!(s.path, wav);
            assert!(s.seconds > 1.0);
        }
        other => panic!(
            "wav export failed: {:?}",
            other.as_ref().map(|r| r.as_ref().err())
        ),
    }
    wait_for_file(&wav);
    h.frames(2);
    h.app.dialogs.export_wav = None;

    let tracks = h.project().tracks.len();
    assert_eq!(h.app.path_chosen(&ctx, PathPurpose::ImportAudio, wav), None);
    h.wait_jobs();
    let p = h.project();
    assert_eq!(p.tracks.len(), tracks + 1, "a new audio track for the file");
    let clip = h.app.sel.clip.expect("the new clip is selected");
    assert!(matches!(h.clip(clip).content, ClipContent::Audio { .. }));
    // The waveform is computed in the background and then drawn.
    h.app.tab = Tab::Editor;
    h.frames(2);
    h.wait_jobs();
    h.frames(2);
}

/// A pack folder with a bank `Kit` of three drums and a pitched `keys`.
fn write_pack(dir: &Path) {
    std::fs::create_dir_all(dir).expect("pack folder");
    for (i, name) in ["bd0", "bd1", "sd", "hh", "c4"].into_iter().enumerate() {
        let audio = AudioData {
            sample_rate: 48_000,
            channels: vec![vec![0.1 * (i + 1) as f32; 2_400]],
        };
        sky_io::write_wav(
            &dir.join(format!("{name}.wav")),
            &audio,
            sky_io::WavFormat::Pcm16,
            false,
        )
        .expect("write wav");
    }
    std::fs::write(
        dir.join("strudel.json"),
        r#"{"Kit_bd": ["bd0.wav", "bd1.wav"], "Kit_sd": "sd.wav", "Kit_hh": "hh.wav",
            "keys": {"c4": "c4.wav"}}"#,
    )
    .expect("write map");
}

#[test]
fn sample_banks_load_from_a_pack_and_exports_credit_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let folder = dir.path().join("my kit");
    write_pack(&folder);
    let mut h = Harness::with_project(empty_project("Banks"));
    let ctx = h.ctx();

    // Load sounds with nothing selected loads into a new track.
    h.app.run_action(&ctx, Action::LoadSounds);
    let d = h
        .app
        .dialogs
        .load_sounds
        .as_mut()
        .expect("the dialog opens");
    assert_eq!(d.track, None);
    assert!(d.pack.is_some(), "a known pack is chosen");
    // Another pack: first an address that does not work.
    d.pack = None;
    d.address = dir.path().join("nowhere").display().to_string();
    h.app.read_pack(&ctx);
    h.wait_jobs();
    let d = h.app.dialogs.load_sounds.as_mut().expect("dialog");
    let listing = d.listing.as_ref().expect("read");
    assert!(listing.pack.is_err());
    assert!(d.offered().is_empty());

    // Then the folder, said to be CC BY 4.0.
    d.license = License::CcBy4;
    assert_eq!(
        h.app
            .path_chosen(&ctx, PathPurpose::SoundPack, folder.clone()),
        None
    );
    h.wait_jobs();
    let d = h.app.dialogs.load_sounds.as_mut().expect("dialog");
    assert_eq!(d.bank, None, "the pack has a sound in no bank");
    let offered: Vec<String> = d.offered().into_iter().map(|(n, _)| n).collect();
    assert_eq!(offered, ["keys"]);
    d.bank = Some("Kit".into());
    assert_eq!(
        d.offered(),
        [("bd".to_string(), 2), ("hh".into(), 1), ("sd".into(), 1)]
    );
    d.chosen.extend(["bd".to_string(), "sd".to_string()]);
    h.frames(3);
    assert!(h.app.dialogs.blocks_input());

    h.app.start_bank_load(&ctx);
    assert!(
        h.app
            .dialogs
            .load_sounds
            .as_ref()
            .is_some_and(|d| d.running)
    );
    h.wait_jobs();
    let kit = h.track_named("Kit");
    assert_eq!(h.app.sel.track, Some(kit.id));
    assert_eq!(h.app.tab, Tab::Instrument);
    let slot = kit.instrument.clone().expect("instrument");
    assert_eq!(slot.kind, InstrumentType::Bank);
    let bank = slot.bank.clone().expect("bank");
    assert_eq!(bank.sounds.keys().collect::<Vec<_>>(), ["bd", "sd"]);
    assert_eq!(bank.sounds["bd"].len(), 2);
    let p = h.project();
    let source = p
        .pool_entry(bank.sounds["sd"][0].audio)
        .ok()
        .and_then(|e| e.source.clone())
        .expect("pack audio keeps its source");
    assert_eq!(
        (source.pack.as_str(), source.license),
        ("my kit", License::CcBy4)
    );
    let d = h
        .app
        .dialogs
        .load_sounds
        .as_mut()
        .expect("dialog stays open");
    assert_eq!(
        d.track,
        Some(kit.id),
        "the next load goes to the same track"
    );
    match &d.result {
        Some(Ok(b)) => assert_eq!(b.new_files, 3),
        other => panic!(
            "load failed: {:?}",
            other.as_ref().map(|r| r.as_ref().err())
        ),
    }
    h.frames(3);

    // Adding hh keeps bd and sd.
    let d = h.app.dialogs.load_sounds.as_mut().expect("dialog");
    d.chosen = ["hh".to_string()].into();
    d.add = true;
    h.app.start_bank_load(&ctx);
    h.wait_jobs();
    let bank = h
        .track_named("Kit")
        .instrument
        .and_then(|i| i.bank)
        .expect("bank");
    assert_eq!(bank.sounds.keys().collect::<Vec<_>>(), ["bd", "hh", "sd"]);
    assert_eq!(bank.name, "Kit");

    // The one plain sound, pitched, into another new track.
    let d = h.app.dialogs.load_sounds.as_mut().expect("dialog");
    d.track = None;
    d.bank = None;
    d.chosen.clear();
    d.add = false;
    h.app.start_bank_load(&ctx);
    h.wait_jobs();
    let keys = h.track_named("keys");
    let bank = keys.instrument.and_then(|i| i.bank).expect("bank");
    assert_eq!(bank.pitched.as_deref(), Some("keys"));
    h.app.dialogs.load_sounds = None;

    // The bank panel draws, and its pattern clip plays the drums.
    h.app.sel.select_track(kit.id);
    h.app.tab = Tab::Instrument;
    h.frames(3);
    h.app.add_bank_pattern(kit.id);
    let clip = h.app.sel.clip.expect("the pattern clip is selected");
    match &h.clip(clip).content {
        ClipContent::Pattern { source, .. } => assert_eq!(source, "bd*4, ~ sd ~ sd, hh*8"),
        other => panic!("not a pattern: {other:?}"),
    }
    h.every_tab();

    // Exports credit the pack.
    h.app.open_export_loop();
    let d = h.app.dialogs.export_loop.as_mut().expect("export dialog");
    assert_eq!(d.licenses.credits, ["my kit (CC BY 4.0)"]);
    d.folder = dir.path().join("game").display().to_string();
    h.frames(2);
    h.app.start_loop_export(&ctx);
    h.wait_jobs();
    match &h
        .app
        .dialogs
        .export_loop
        .as_ref()
        .and_then(|d| d.result.as_ref())
    {
        Some(Ok(s)) => {
            assert_eq!(s.licenses.credits, ["my kit (CC BY 4.0)"]);
            wait_for_file(&s.manifest);
            let manifest = std::fs::read_to_string(&s.manifest).expect("manifest");
            assert!(manifest.contains("my kit (CC BY 4.0)"), "{manifest}");
        }
        other => panic!("export failed: {:?}", other.map(|r| r.as_ref().err())),
    }
    h.frames(2);
}

#[test]
fn starter_patterns_use_what_the_bank_has() {
    let bank = |names: &[&str], pitched: Option<&str>| SampleBank {
        name: "b".into(),
        sounds: names
            .iter()
            .map(|n| {
                (
                    n.to_string(),
                    vec![BankSample {
                        audio: AudioId::new(),
                        root: None,
                    }],
                )
            })
            .collect(),
        pitched: pitched.map(str::to_string),
    };
    assert_eq!(
        sounds::starter_pattern(&bank(&["bd", "sn", "hh"], None)),
        "bd*4, ~ sd ~ sd, hh*8"
    );
    assert_eq!(
        sounds::starter_pattern(&bank(&["casio", "e", "crow", "my.sound"], None)),
        "casio crow"
    );
    assert_eq!(
        sounds::starter_pattern(&bank(&["piano"], Some("piano"))),
        "c3 e3 g3 e3"
    );
}

#[test]
fn waveform_peaks_cover_the_audio() {
    let mut audio = AudioData::silent(2, 1000, 48_000);
    audio.channels[0][10] = 0.5;
    audio.channels[1][900] = -0.75;
    let peaks = Peaks::compute(&audio);
    assert_eq!(peaks.frames, 1000);
    assert_eq!(peaks.range(0.0, 1000.0), Some((-0.75, 0.5)));
    assert_eq!(peaks.range(0.0, 100.0), Some((0.0, 0.5)));
    assert_eq!(peaks.range(2000.0, 3000.0), None);
    let empty = Peaks::compute(&AudioData::silent(1, 0, 48_000));
    assert_eq!(empty.range(0.0, 10.0), None);
}

#[test]
fn every_symbol_in_the_ui_has_a_glyph() {
    let h = Harness::demo();
    // Every non-ASCII character in the sources, comments included.
    let sources = [
        include_str!("actions.rs"),
        include_str!("ai_panel.rs"),
        include_str!("app.rs"),
        include_str!("arrange.rs"),
        include_str!("audio.rs"),
        include_str!("audition.rs"),
        include_str!("browser.rs"),
        include_str!("dialogs.rs"),
        include_str!("game.rs"),
        include_str!("jobs.rs"),
        include_str!("lib.rs"),
        include_str!("mixer.rs"),
        include_str!("palette.rs"),
        include_str!("params.rs"),
        include_str!("pattern_editor.rs"),
        include_str!("piano_roll.rs"),
        include_str!("sounds.rs"),
        include_str!("theme.rs"),
        include_str!("tracks.rs"),
        include_str!("transport.rs"),
        include_str!("waveform.rs"),
        include_str!("widgets.rs"),
    ];
    let mut symbols: Vec<char> = sources
        .iter()
        .flat_map(|s| s.chars())
        .filter(|c| !c.is_ascii())
        .collect();
    symbols.sort_unstable();
    symbols.dedup();
    assert!(symbols.contains(&'…'));
    let font = FontId::proportional(13.5);
    let missing: String = symbols
        .iter()
        .copied()
        .filter(|c| !h.ctx.fonts_mut(|f| f.has_glyph(&font, *c)))
        .collect();
    assert!(missing.is_empty(), "missing glyphs: {missing}");
    // `has_glyph` reports false negatives for characters that live in the
    // face holding the replacement glyph, which for monospace is the face
    // that has all of these. Monospace falls back to every proportional
    // face, so whatever passed above also renders there.
    let defs = egui::FontDefinitions::default();
    let mono = &defs.families[&egui::FontFamily::Monospace];
    let prop = &defs.families[&egui::FontFamily::Proportional];
    assert!(prop.iter().all(|f| mono.contains(f)));
}

#[test]
fn unique_names_and_new_tracks() {
    let p = demo_project();
    assert_eq!(actions::unique_track_name(&p, "Bass"), "Bass 2");
    assert_eq!(actions::unique_track_name(&p, "Lead"), "Lead");
    let empty = empty_project("x");
    for kind in NewTrack::ALL {
        let t = actions::new_track(&empty, kind);
        assert!(!t.name.is_empty());
    }
}

#[test]
fn opening_game_assets_changes_nothing() {
    use sky_core::manifest::RandomRange;
    use sky_model::GameAssetKind;

    let mut h = Harness::demo();
    // Ranges the model takes but the window shows only roughly.
    let mut hits = h.project().game.find("demo_hits").expect("hits").clone();
    if let GameAssetKind::Sfx {
        pitch_semitones,
        gain_db,
        ..
    } = &mut hits.kind
    {
        *pitch_semitones = RandomRange::new(-24.0, 24.0);
        *gain_db = RandomRange::new(1.0, 3.0);
    }
    assert!(game::update(&h.app.session, hits.clone(), None).is_ok());
    let revision = h.app.session.view().revision;
    let ctx = h.ctx();
    h.app.run_action(&ctx, Action::GameAssets);
    h.frames(3);
    assert!(h.app.dialogs.game.is_some());
    assert_eq!(h.app.session.view().revision, revision);
    assert_eq!(h.project().game.find("demo_hits"), Some(&hits));
}

#[test]
fn game_assets_are_added_edited_and_exported() {
    use sky_model::GameAssetKind;

    let dir = tempfile::tempdir().expect("temp dir");
    let mut h = Harness::demo();
    let ctx = h.ctx();
    h.app.run_action(&ctx, Action::GameAssets);
    h.frames(2);
    assert!(h.app.dialogs.game.is_some());

    // The demo has a loop and a set of hits; add one more of each from
    // a one-bar loop region: the SFX set gets one variation per beat.
    let r = actions::set_loop_region(&h.app.session, 0, 4 * PPQ);
    assert!(r.is_ok(), "{r:?}");
    let sfx = game::add_sfx(&h.app.session).expect("add sfx");
    let music = game::add_loop(&h.app.session).expect("add loop");
    h.frames(2);
    let p = h.project();
    assert_eq!(p.game.assets.len(), 4);
    let added = p.game.asset(sfx).expect("sfx");
    assert_eq!(added.name, "demo_loop_sfx");
    match &added.kind {
        GameAssetKind::Sfx { variations, .. } => assert_eq!(variations.len(), 4),
        other => panic!("{other:?}"),
    }
    assert_eq!(p.game.asset(music).expect("loop").name, "demo_loop_music");
    // A second one gets a free name.
    assert_eq!(
        game::unique_asset_name(&p, "Demo loop SFX"),
        "demo_loop_sfx_2"
    );
    // Long project names are cut to fit, suffix and all.
    let long = "A very long project name ".repeat(4);
    let name = game::unique_asset_name(&p, &long);
    assert!(sky_core::manifest::is_asset_name(&name), "{name}");
    let mut taken = (*p).clone();
    let mut asset = added.clone();
    asset.id = sky_core::AssetId::new();
    asset.name = name.clone();
    taken.game.assets.push(asset);
    let second = game::unique_asset_name(&taken, &long);
    assert!(sky_core::manifest::is_asset_name(&second), "{second}");
    assert!(second.ends_with("_2") && second != name, "{second}");

    // A bad name is refused and the asset keeps its own.
    let mut bad = p.game.asset(sfx).expect("sfx").clone();
    bad.name = "Has spaces".into();
    assert!(game::update(&h.app.session, bad, None).is_err());
    assert!(game::remove(&h.app.session, music).is_ok());
    h.frames(2);
    assert_eq!(h.project().game.assets.len(), 3);

    // Export into a folder of our own; the setting is saved in the project.
    if let Some(d) = h.app.dialogs.game.as_mut() {
        d.folder = dir.path().join("game").display().to_string();
        d.bits = 16;
    }
    h.app.start_game_export(&ctx);
    assert!(h.app.dialogs.game.as_ref().is_some_and(|d| d.running));
    h.frames(2);
    h.wait_jobs();
    let d = h.app.dialogs.game.as_ref().expect("window stays open");
    assert!(!d.running);
    match &d.result {
        Some(Ok(s)) => {
            wait_for_file(&s.manifest);
            assert!(s.passed, "{:?}", s.checks);
            assert_eq!(s.assets.len(), 3);
        }
        Some(Err(e)) => panic!("game export failed: {e}"),
        None => panic!("no result"),
    }
    assert!(dir.path().join("game/sfx/demo_loop_sfx_04.wav").exists());
    let p = h.project();
    assert_eq!(p.game.bits, 16);
    assert!(
        p.game
            .export_dir
            .as_deref()
            .is_some_and(|d| d.ends_with("game"))
    );
    h.frames(2);

    // Closing the window keeps the project's settings.
    h.app.dialogs.game = None;
    h.frames(1);
    assert_eq!(h.project().game.assets.len(), 3);
}

#[test]
fn piano_keys_and_placed_notes_are_heard() {
    let mut h = Harness::with_engine(demo_project());
    let bass = h.track_named("Bass");
    let clip = bass.clips[0].id;
    h.app.sel.select_clip(bass.id, clip);
    h.app.tab = Tab::Editor;
    h.frames(3);
    let history = h.history_len();
    assert_eq!(h.heard(), 0.0, "nothing plays while stopped");

    // Pressing a key holds it; letting go releases it.
    let keys = h.rect_of(Id::new(("piano_keys", clip)));
    let key = keys.center();
    h.move_to(key);
    h.pointer(key, true);
    h.frame();
    let held = h.app.audition.held.expect("a key is held");
    assert_eq!(held.0, bass.id);
    assert!(h.heard() > 0.01, "the key is heard");
    // Dragging along the keys plays the next one.
    h.move_to(key + vec2(0.0, -40.0));
    let next = h.app.audition.held.expect("still held");
    assert!(next.1 > held.1, "{next:?} after {held:?}");
    h.pointer(key, false);
    h.frame();
    assert_eq!(h.app.audition.held, None);
    h.quiet();
    assert!(h.heard() < 1e-3, "the key was let go");

    // A note added on the grid sounds once, and only the note is a change.
    let grid = h.rect_of(Id::new(("piano_bg", clip)));
    let notes = |h: &Harness| match &h.clip(clip).content {
        ClipContent::Midi { notes } => notes.len(),
        _ => 0,
    };
    let before = notes(&h);
    h.click(grid.left_top() + vec2(4.0, 4.0));
    assert_eq!(notes(&h), before + 1);
    assert_eq!(h.history_len(), history + 1);
    assert!(h.heard() > 0.01, "the new note is heard");
    h.quiet();
    assert!(h.heard() < 1e-3, "and it ends by itself");

    // With hearing notes off, adding one is silent.
    h.app.audition.hear_notes = false;
    h.click(grid.left_top() + vec2(4.0, 30.0));
    assert_eq!(notes(&h), before + 2);
    assert_eq!(h.heard(), 0.0);
}

#[test]
fn the_instrument_keyboard_plays_the_track() {
    let mut h = Harness::with_engine(demo_project());
    let pad = h.track_named("Pad");
    h.app.sel.select_track(pad.id);
    h.app.tab = Tab::Instrument;
    h.frames(3);
    let keys = h.rect_of(crate::audition::keyboard_id(pad.id));
    let at = keys.left_bottom() + vec2(keys.width() / 2.0, -4.0);
    h.move_to(at);
    h.pointer(at, true);
    h.frame();
    assert!(h.app.audition.held.is_some_and(|(t, _)| t == pad.id));
    assert!(h.heard() > 0.01);
    h.pointer(at, false);
    h.frame();
    assert_eq!(h.app.audition.held, None);
    // Only a project with no device stays silent, without complaint.
    let mut off = Harness::demo();
    let pad = off.track_named("Pad");
    off.app.sel.select_track(pad.id);
    off.app.tab = Tab::Instrument;
    off.frames(3);
    let keys = off.rect_of(crate::audition::keyboard_id(pad.id));
    off.click(keys.center());
    assert_eq!(off.app.audition.held, None);
    assert!(off.app.notice.is_none());
}

#[test]
fn game_loops_and_sfx_sets_are_heard_as_they_export() {
    let mut h = Harness::with_engine(demo_project());
    let ctx = h.ctx();
    let music = h.app.report(game::add_loop(&h.app.session)).expect("loop");
    let sfx = h.app.report(game::add_sfx(&h.app.session)).expect("sfx");
    h.run(Action::GameAssets);
    h.frames(2);
    let play = PreviewKey::GameLoop {
        asset: music,
        join: false,
    };

    // Play renders in the background, then loops until stopped.
    h.app.toggle_preview(&ctx, play.clone());
    assert_eq!(h.app.preview_state(&play), ButtonState::Pending);
    h.frames(2);
    h.wait_jobs();
    assert_eq!(h.app.preview_state(&play), ButtonState::Playing);
    assert!(h.heard() > 0.01);
    let id = h.app.session.previewing().expect("the engine plays it");
    // The demo loop is four bars, eight seconds; ten seconds later it
    // still plays.
    for _ in 0..200 {
        h.heard();
    }
    assert_eq!(h.app.session.previewing(), Some(id));
    assert_eq!(h.history_len(), 2, "auditions are not changes");

    // Join starts another render, which replaces it; then Stop stops.
    let join = PreviewKey::GameLoop {
        asset: music,
        join: true,
    };
    h.app.toggle_preview(&ctx, join.clone());
    h.wait_jobs();
    h.heard();
    assert_eq!(h.app.preview_state(&join), ButtonState::Playing);
    assert_eq!(h.app.preview_state(&play), ButtonState::Idle);
    h.app.toggle_preview(&ctx, join.clone());
    h.heard();
    h.frames(20);
    assert_eq!(h.app.preview_state(&join), ButtonState::Idle);
    assert_eq!(h.app.session.previewing(), None);

    // An SFX set renders once, then each trigger plays the next variation.
    h.app.trigger_sfx(&ctx, sfx);
    h.wait_jobs();
    assert!(h.heard() > 0.0);
    let first = h
        .app
        .audition
        .last_trigger
        .get(&sfx)
        .cloned()
        .expect("played");
    assert!(first.starts_with("Variation "), "{first}");
    // The second needs no render.
    h.app.trigger_sfx(&ctx, sfx);
    assert_ne!(
        h.app.preview_state(&PreviewKey::GameSfx(sfx)),
        ButtonState::Pending
    );
    assert!(h.heard() > 0.0);
    h.frames(2);

    // Closing the window stops what it started.
    h.app.toggle_preview(&ctx, play.clone());
    h.wait_jobs();
    h.heard();
    assert!(h.app.session.previewing().is_some());
    h.app.dialogs.game = None;
    h.frames(2);
    h.heard();
    h.heard();
    assert_eq!(h.app.session.previewing(), None);

    // Without a device, Play says why it is silent.
    let mut off = Harness::demo();
    let ctx = off.ctx();
    let music = off
        .app
        .report(game::add_loop(&off.app.session))
        .expect("loop");
    off.run(Action::GameAssets);
    let play = PreviewKey::GameLoop {
        asset: music,
        join: false,
    };
    off.app.toggle_preview(&ctx, play.clone());
    assert_eq!(off.app.preview_state(&play), ButtonState::Idle);
    assert!(off.app.notice.as_ref().is_some_and(|n| n.error));
}

#[test]
fn the_loop_region_and_pack_sounds_are_heard_before_export_or_load() {
    let dir = tempfile::tempdir().expect("temp dir");
    let folder = dir.path().join("kit");
    write_pack(&folder);
    let mut h = Harness::with_engine(empty_project("Banks"));
    let ctx = h.ctx();

    // A pack's sound plays from the Load sounds dialog before loading.
    h.app.run_action(&ctx, Action::LoadSounds);
    let d = h.app.dialogs.load_sounds.as_mut().expect("dialog");
    d.pack = None;
    d.address = folder.display().to_string();
    h.app.read_pack(&ctx);
    h.wait_jobs();
    let d = h.app.dialogs.load_sounds.as_mut().expect("dialog");
    d.bank = Some("Kit".into());
    assert!(d.hear, "on by default");
    h.app
        .start_preview(&ctx, PreviewKey::PackSound("sd".into()));
    h.wait_jobs();
    let p = h.heard();
    // sd.wav is a constant 0.3.
    assert!((p - 0.3).abs() < 0.01, "{p}");
    assert!(h.project().audio_pool.is_empty(), "nothing loaded yet");
    h.app.dialogs.load_sounds = None;
    h.frames(2);

    // Bank sounds play through the track, each click the next variation.
    let track = Track::new_instrument("Kit", InstrumentType::Bank);
    let track_id = track.id;
    h.app
        .session
        .apply(Transaction::single(Command::AddTrack {
            track: Box::new(track),
            index: None,
        }))
        .expect("add track");
    let request = sky_session::BankRequest {
        source: sky_library::PackSource::parse(&folder.to_string_lossy(), Some(License::Cc0)),
        bank: Some("Kit".into()),
        sounds: Vec::new(),
        add: false,
        pitched: None,
    };
    h.app
        .session
        .load_bank(track_id, &request, Origin::User)
        .expect("load");
    h.frames(2);
    h.quiet();
    h.app.hear_bank_sound(track_id, "bd", 2);
    let first = h.heard();
    h.quiet();
    h.app.hear_bank_sound(track_id, "bd", 2);
    let second = h.heard();
    // bd0 and bd1 are 0.1 and 0.2.
    assert!(second > first * 1.5, "{first} then {second}");

    // Export loop's Play renders the loop region and loops it.
    h.app.session.new_project(demo_project());
    h.frames(2);
    h.run(Action::ExportLoop);
    let play = PreviewKey::LoopRegion { join: false };
    h.app.toggle_preview(&ctx, play.clone());
    h.wait_jobs();
    assert!(h.heard() > 0.01);
    assert_eq!(h.app.preview_state(&play), ButtonState::Playing);
    // The transport's Stop stops previews too.
    h.app.stop();
    h.heard();
    h.heard();
    assert_eq!(h.app.session.previewing(), None);
}

#[derive(Debug)]
struct TestFile(std::path::PathBuf);

impl egui::DroppedFile for TestFile {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

fn tone_file(path: &Path, seconds: f32) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).expect("folder");
    }
    let frames = (seconds * 48_000.0) as usize;
    let tone: Vec<f32> = (0..frames).map(|i| (i as f32 * 0.06).sin() * 0.5).collect();
    let audio = AudioData {
        sample_rate: 48_000,
        channels: vec![tone],
    };
    sky_io::write_wav(path, &audio, sky_io::WavFormat::Pcm16, false).expect("write wav");
}

/// A point in a track's lane at `tick`.
fn lane_point(h: &Harness, track: sky_core::TrackId, tick: i64) -> Pos2 {
    let (_, lane) = h
        .app
        .arrange
        .lanes
        .iter()
        .find(|(t, _)| *t == track)
        .expect("the track has a lane");
    pos2(h.app.arrange.x_of(tick as f64), lane.center().y)
}

#[test]
fn the_browser_lists_plays_and_drops_folder_samples() {
    use crate::browser::{Item, item_id, row_id};

    let dir = tempfile::tempdir().expect("temp dir");
    let samples = dir.path().join("samples");
    let kick = samples.join("kicks").join("kick 909.wav");
    let snare = samples.join("snare.wav");
    tone_file(&kick, 0.5);
    tone_file(&snare, 0.25);
    tone_file(&samples.join(".hidden").join("skip.wav"), 0.1);
    std::fs::write(samples.join("readme.txt"), "not audio").expect("text");

    let mut project = empty_project("Browse");
    project.tracks.push(Track::new_audio("Vox"));
    project
        .tracks
        .push(Track::new_instrument("Lead", InstrumentType::Synth));
    let mut h = Harness::with_engine(project);
    h.offline_packs();
    let vox = h.track_named("Vox").id;
    let lead = h.track_named("Lead").id;

    // A folder is looked through in the background, leaving out hidden
    // folders and files that are not audio.
    assert!(h.app.browser.add_folder(samples.clone()));
    assert!(!h.app.browser.add_folder(samples.clone()), "only once");
    h.frames(2);
    h.wait_jobs();
    let index = h.app.browser.folder_index(&samples).expect("indexed");
    // A folder's own files come before its subfolders'.
    assert_eq!(index.all, [snare.clone(), kick.clone()]);
    let kick_item = Item::File(kick.clone());
    let snare_item = Item::File(snare.clone());
    assert!(
        h.ctx.read_response(item_id(&kick_item)).is_none(),
        "the subfolder starts closed"
    );

    // Clicking a sample plays it, and changes nothing.
    let history = h.history_len();
    let snare_row = h.rect_of(item_id(&snare_item)).center();
    h.click(snare_row);
    assert_eq!(h.app.browser.selected.as_ref(), Some(&snare_item));
    h.wait_jobs();
    assert!(h.heard() > 0.1, "the sample plays");
    assert_eq!(h.history_len(), history);
    h.app.stop_preview();
    h.quiet();

    // Opening the subfolder shows its file; a search finds it too.
    let kicks = row_id(&crate::browser::folder_key(&samples.join("kicks")));
    h.click(h.rect_of(kicks).center());
    assert!(h.ctx.read_response(item_id(&kick_item)).is_some());
    h.click(h.rect_of(kicks).center());
    h.app.browser.search = "909".into();
    h.frames(2);
    assert!(h.ctx.read_response(item_id(&kick_item)).is_some());
    assert!(h.ctx.read_response(item_id(&snare_item)).is_none());

    // Dragged onto Vox at bar 3, it lands there as one undo step.
    let bar = h.project().ticks_per_bar();
    let from = h.rect_of(item_id(&kick_item)).center();
    let to = lane_point(&h, vox, bar * 2);
    h.drag(from, to);
    h.wait_jobs();
    let p = h.project();
    let clips = &p.track(vox).expect("vox").clips;
    assert_eq!(clips.len(), 1);
    assert_eq!(clips[0].start, bar * 2);
    assert_eq!(clips[0].name, "kick 909");
    assert_eq!(h.app.sel.clip, Some(clips[0].id));
    assert_eq!(h.history_len(), history + 1);
    assert_eq!(p.audio_pool.len(), 1);

    // A synth takes no audio: nothing changes, and it says why.
    let from = h.rect_of(item_id(&kick_item)).center();
    let to = lane_point(&h, lead, 0);
    h.drag(from, to);
    h.wait_jobs();
    assert_eq!(h.history_len(), history + 1);
    let notice = h.app.notice.as_ref().expect("a notice");
    assert!(
        notice.error && notice.text.contains("Lead plays"),
        "{}",
        notice.text
    );

    // Below the tracks, it goes on a new audio track.
    h.app.browser.search.clear();
    h.frames(2);
    let from = h.rect_of(item_id(&snare_item)).center();
    let below = h
        .app
        .arrange
        .lanes
        .iter()
        .map(|(_, r)| r.bottom())
        .fold(0.0, f32::max)
        + 30.0;
    h.drag(from, pos2(lane_point(&h, vox, 0).x, below));
    h.wait_jobs();
    let p = h.project();
    assert_eq!(p.tracks.len(), 3);
    assert_eq!(p.tracks[2].name, "snare");
    assert_eq!(p.tracks[2].clips[0].start, 0);

    // Project audio is listed too; a double-click adds it where Import
    // audio would: on the selected audio track, at the playhead's bar.
    let pool_item = Item::Pool(p.audio_pool[0].id);
    h.app.sel.select_track(vox);
    h.frames(2);
    h.double_click(h.rect_of(item_id(&pool_item)).center());
    h.wait_jobs();
    let p = h.project();
    assert_eq!(p.track(vox).expect("vox").clips.len(), 2);
    assert_eq!(p.audio_pool.len(), 2, "pool audio is used again");

    // A search looks inside folders that were never opened.
    let more = dir.path().join("more");
    let hat = more.join("open hat.wav");
    tone_file(&hat, 0.1);
    h.app.browser.add_folder(more.clone());
    h.app
        .browser
        .set_expanded(&crate::browser::folder_key(&more), false);
    h.frames(2);
    assert!(h.app.browser.folder_index(&more).is_none());
    h.app.browser.search = "hat".into();
    h.frames(2);
    h.wait_jobs();
    assert!(
        h.ctx
            .read_response(item_id(&Item::File(hat.clone())))
            .is_some()
    );
    h.app.browser.search.clear();

    // Removing the folder forgets it.
    h.app.browser.remove_folder(&samples);
    h.frames(2);
    assert!(h.ctx.read_response(item_id(&snare_item)).is_none());
}

#[test]
fn browser_pack_sounds_play_and_drop_into_a_sample_bank() {
    use crate::browser::{Item, PackListing, item_id, pack_key, row_id};
    use sky_library::{Library, PACKS, PackSource};

    let dir = tempfile::tempdir().expect("temp dir");
    let folder = dir.path().join("kit");
    write_pack(&folder);
    let mut project = empty_project("Packs");
    project
        .tracks
        .push(Track::new_instrument("Drums", InstrumentType::Bank));
    let mut h = Harness::with_engine(project);
    let drums = h.track_named("Drums").id;
    let library = Library::new(dir.path().join("cache"));
    h.app.session.set_library(library.clone());
    h.offline_packs();

    // Stand the local pack in for the first known one, as if read.
    let info = &PACKS[0];
    let pack = library
        .open(&PackSource::parse(
            &folder.to_string_lossy(),
            Some(License::Cc0),
        ))
        .expect("pack");
    h.app.browser_pack_read(
        info.map.to_string(),
        Ok(Arc::new(PackListing::new(Arc::new(pack)))),
    );
    h.app.browser.set_expanded(&pack_key(info), true);
    h.app
        .browser
        .set_expanded(&crate::browser::bank_key(info, "Kit"), true);
    h.frames(2);
    assert!(h.ctx.read_response(row_id(&pack_key(info))).is_some());
    let bd = Item::PackSound {
        map: info.map.to_string(),
        bank: Some("Kit".into()),
        sound: "bd".into(),
    };
    let keys = Item::PackSound {
        map: info.map.to_string(),
        bank: None,
        sound: "keys".into(),
    };
    assert!(h.ctx.read_response(item_id(&keys)).is_some());

    h.click(h.rect_of(item_id(&bd)).center());
    h.wait_jobs();
    assert!(h.heard() > 0.01, "the pack sound plays");
    h.app.stop_preview();
    h.quiet();

    // Onto the bank's header: a sound named after it, from the pack.
    let header = h
        .app
        .arrange
        .lanes
        .iter()
        .find(|(t, _)| *t == drums)
        .map(|(_, r)| pos2(r.left() - 40.0, r.center().y))
        .expect("drums lane");
    h.drag(h.rect_of(item_id(&bd)).center(), header);
    h.wait_jobs();
    let p = h.project();
    let bank = p
        .track(drums)
        .expect("drums")
        .instrument
        .as_ref()
        .and_then(|i| i.bank.clone())
        .expect("a bank");
    assert_eq!(bank.sounds["bd"].len(), 1);
    let source = p.audio_pool[0].source.clone().expect("credited");
    assert_eq!(source.license, License::Cc0);
    assert_eq!(h.app.tab, Tab::Instrument);

    // "Load this bank…" opens Load sounds on it, for the selected bank,
    // without reading the pack again.
    let ctx = h.ctx();
    h.app.open_load_sounds_on(&ctx, info, Some("Kit"));
    h.frames(2);
    let d = h
        .app
        .dialogs
        .load_sounds
        .as_ref()
        .expect("Load sounds opens");
    assert_eq!(d.track, Some(drums));
    assert_eq!(d.pack, Some(0));
    assert_eq!(d.bank.as_deref(), Some("Kit"));
    assert_eq!(d.reading, None);
    assert_eq!(d.offered().len(), 3);
    h.app.dialogs.load_sounds = None;

    // Searching finds sounds inside a pack that has been read.
    h.app.browser.search = "kit sd".into();
    h.frames(2);
    let sd = Item::PackSound {
        map: info.map.to_string(),
        bank: Some("Kit".into()),
        sound: "sd".into(),
    };
    assert!(h.ctx.read_response(item_id(&sd)).is_some());
    assert!(h.ctx.read_response(item_id(&keys)).is_none());
}

#[test]
fn files_dropped_on_the_window_come_in() {
    let dir = tempfile::tempdir().expect("temp dir");
    let one = dir.path().join("loop.wav");
    let two = dir.path().join("hit.wav");
    let three = dir.path().join("riser.flac.txt");
    tone_file(&one, 0.5);
    tone_file(&two, 0.1);
    std::fs::write(&three, "text").expect("text");
    let folder = dir.path().join("more");
    std::fs::create_dir_all(&folder).expect("folder");
    let mut h = Harness::with_project(empty_project("Drops"));
    let drop = |h: &mut Harness, paths: &[&Path]| {
        h.dropped = paths
            .iter()
            .map(|p| Arc::new(TestFile(p.to_path_buf())) as egui::DroppedFileHandle)
            .collect();
        h.frame();
        h.wait_jobs();
    };

    // Held over the window: a note says what dropping does.
    h.hovered = vec![egui::HoveredFile::default()];
    h.frames(2);
    h.hovered.clear();

    // One file goes on a new track, as Import audio would put it.
    drop(&mut h, &[&one]);
    let p = h.project();
    assert_eq!(p.tracks.len(), 1);
    assert_eq!(p.tracks[0].name, "loop");
    let notice = h.app.notice.as_ref().expect("a notice");
    assert_eq!(notice.text, "Added loop on a new track");
    let history = h.history_len();

    // Several go into the project's audio as one step; a folder joins the
    // browser; anything else is turned away.
    drop(&mut h, &[&one, &two, &folder, &three]);
    let p = h.project();
    assert_eq!(p.audio_pool.len(), 3);
    assert_eq!(p.tracks.len(), 1);
    assert_eq!(h.history_len(), history + 1);
    assert_eq!(h.app.browser.folders, [folder]);
    let notice = h.app.notice.as_ref().expect("a notice");
    assert!(
        notice.text.starts_with("Imported 2 files"),
        "{}",
        notice.text
    );
    assert!(
        notice.text.contains("Not riser.flac.txt"),
        "{}",
        notice.text
    );
    h.app.session.undo().expect("undo");
    assert_eq!(h.project().audio_pool.len(), 1);

    // A strudel.json opens Load sounds on its pack.
    let pack = dir.path().join("pack");
    write_pack(&pack);
    drop(&mut h, &[&pack.join("strudel.json")]);
    let d = h
        .app
        .dialogs
        .load_sounds
        .as_ref()
        .expect("Load sounds opens");
    assert_eq!(d.pack, None);
    assert!(d.address.ends_with("strudel.json"));
    assert!(d.listing.as_ref().is_some_and(|l| l.pack.is_ok()));
}

#[test]
fn the_browser_keeps_its_folders_between_runs() {
    use crate::browser::BrowserState;

    let dir = tempfile::tempdir().expect("temp dir");
    let store = dir.path().join("config").join("browser-folders.txt");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    let mut first = BrowserState::new(Some(store.clone()));
    assert!(first.folders.is_empty());
    first.add_folder(a.clone());
    first.add_folder(b.clone());
    assert_eq!(
        BrowserState::new(Some(store.clone())).folders,
        [a.clone(), b]
    );
    first.remove_folder(&dir.path().join("b"));
    assert_eq!(BrowserState::new(Some(store)).folders, [a]);
    assert!(BrowserState::new(None).folders.is_empty());
}
