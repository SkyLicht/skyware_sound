// SPDX-License-Identifier: GPL-3.0-or-later
//! Headless tests: the whole interface runs frame by frame in an egui
//! context, with no window and no audio device.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use egui::accesskit::Role;
use egui::{
    Context, Event, FontId, Id, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, pos2, vec2,
};
use sky_core::{AudioId, ClipId, EffectId, MarkerId, PPQ, TrackId};
use sky_dsp::{AudioData, InstrumentType};
use sky_model::templates::{demo_project, empty_project};
use sky_model::{
    BankSample, Clip, ClipContent, Command, License, Note, Origin, PoolEntry, Project, SampleBank,
    Scale, ScaleMode, Track, Transaction,
};
use sky_session::Session;

use crate::actions::{self, NewTrack};
use crate::app::{Action, App, Focus, Tab};
use crate::audition::{ButtonState, PreviewKey};
use crate::clip_tools::{EditKind, Tool};
use crate::dialogs::PathPurpose;
use crate::edit::Copied;
use crate::loop_inspector::Target;
use crate::piano_roll::Ghosts;
use crate::ruler_lanes::Lane;
use crate::waveform::Peaks;
use crate::widgets::Gestures;
use crate::workspace::Workspace;
use crate::{
    UiConfig, arrange, asset_lane, fx_view, game, inst_view, keys, menu, mixer, palette,
    pattern_editor, sounds, theme, velocity_lane,
};

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
    /// Labelled widgets and where they were drawn last frame, once
    /// [`accessible`](Self::accessible) is on.
    access: Vec<(String, Rect, Role)>,
    /// The text last put on the system clipboard.
    copied: Option<String>,
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
            access: Vec::new(),
            copied: None,
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
        for command in &out.platform_output.commands {
            if let egui::OutputCommand::CopyText(text) = command {
                self.copied = Some(text.clone());
            }
        }
        if let Some(tree) = out.platform_output.accesskit_update.take() {
            self.access = tree
                .nodes
                .iter()
                .filter_map(|(_, node)| {
                    let b = node.bounds()?;
                    let rect = Rect::from_min_max(
                        pos2(b.x0 as f32, b.y0 as f32),
                        pos2(b.x1 as f32, b.y1 as f32),
                    );
                    // Plain text carries its words as a value.
                    let label = node.label().or(node.value())?;
                    Some((label.to_string(), rect, node.role()))
                })
                .collect();
        }
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

    fn right_click(&mut self, pos: Pos2) {
        self.move_to(pos);
        for pressed in [true, false] {
            self.events.push(Event::PointerButton {
                pos,
                button: PointerButton::Secondary,
                pressed,
                modifiers: self.modifiers,
            });
            self.frame();
        }
        self.frames(2);
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

    /// Reads widget labels from the accessibility tree from now on, for
    /// [`find`](Self::find) and [`click_label`](Self::click_label).
    fn accessible(&mut self) {
        self.ctx.enable_accesskit();
        self.frames(2);
    }

    /// Where the widget with this label, or this label and a shortcut, was
    /// drawn last frame; the topmost one if there are several.
    fn find(&self, label: &str) -> Option<Rect> {
        self.find_role(label, None)
    }

    /// Like [`find`](Self::find), for one kind of widget only.
    fn find_role(&self, label: &str, role: Option<Role>) -> Option<Rect> {
        let with_shortcut = format!("{label} ");
        self.access
            .iter()
            .rev()
            .filter(|(_, _, r)| role.is_none_or(|role| *r == role))
            .find(|(l, _, _)| l == label || l.starts_with(&with_shortcut))
            .map(|(_, r, _)| *r)
    }

    /// Clicks the widget with this label inside a panel, for labels that
    /// other panels use too.
    fn click_in(&mut self, tab: Tab, label: &str) {
        let area = self.panel_rect(tab);
        let rect = self
            .access
            .iter()
            .find(|(l, r, _)| l == label && area.contains(r.center()))
            .map(|(_, r, _)| *r)
            .unwrap_or_else(|| panic!("no {label:?} in {tab:?}"));
        self.click(rect.center());
    }

    fn click_label(&mut self, label: &str) {
        self.click_role(label, None);
    }

    fn click_role(&mut self, label: &str, role: Option<Role>) {
        let rect = self.find_role(label, role).unwrap_or_else(|| {
            let labels: Vec<&str> = self.access.iter().map(|(l, _, _)| l.as_str()).collect();
            panic!("no widget labelled {label:?} among {labels:?}")
        });
        self.click(rect.center());
    }

    /// The rectangle of the tab group a panel is in.
    fn panel_rect(&self, tab: Tab) -> Rect {
        let dock = self.app.workspace.dock();
        let path = dock.find_tab(&tab).expect("panel is open");
        dock.leaf(path.node_path()).expect("a tab group").rect
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

    /// Brings every panel to the front once.
    fn every_tab(&mut self) {
        for tab in Tab::ALL {
            self.app.workspace.show(tab);
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
        muted: false,
        content: ClipContent::Midi {
            notes: vec![Note::new(0, 0, 60, 100)],
        },
    });
    synth.clips.push(Clip {
        id: ClipId::new(),
        name: "Broken".into(),
        start: 3840,
        length: 3840,
        muted: false,
        content: ClipContent::pattern("c4 [e4", 0, 0),
    });
    let mut audio = Track::new_audio("Odd audio");
    audio.clips.push(Clip {
        id: ClipId::new(),
        name: "Missing file".into(),
        start: 0,
        length: 3840,
        muted: false,
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
    assert!(h.app.workspace.is_front(Tab::Instrument));
    let new = p.track(track).expect("track exists");
    assert_eq!(new.kind, sky_model::TrackKind::Instrument);
    assert!(new.clips.is_empty());

    // Add a clip, as a double-click does.
    let clip = arrange::create_clip_at(&mut h.app, track, 0.0).expect("clip added");
    h.frames(2);
    let bar = h.project().ticks_per_bar();
    assert_eq!(h.clip(clip).length, bar);
    assert_eq!(h.app.sel.clip, Some(clip));

    // Move it two bars right, as Shift+Right twice does.
    h.run(Action::NudgeBars(2));
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
    assert!(h.app.workspace.is_front(Tab::Editor));
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
    assert_eq!(h.app.mixer.open_effects.len(), 1, "the new effect opens");
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
fn pattern_edits_apply_on_ctrl_enter_and_span_lines() {
    let mut h = Harness::demo();
    let drums = h.track_named("Drums");
    let clip = drums.clips[0].id;
    let source = |h: &Harness| match &h.clip(clip).content {
        ClipContent::Pattern { source, .. } => source.clone(),
        other => panic!("{other:?}"),
    };
    let original = source(&h);
    let notice = |h: &Harness| {
        h.app
            .notice
            .as_ref()
            .map(|n| n.text.clone())
            .unwrap_or_default()
    };
    h.app.sel.select_clip(drums.id, clip);
    h.app.workspace.show(Tab::Editor);
    h.frames(3);
    let text_id = Id::new(("pattern_text", clip));
    h.ctx.memory_mut(|m| m.request_focus(text_id));
    h.frames(2);
    // A typo is not applied, and the notice says where it is.
    h.key(Key::A, COMMAND);
    h.type_text("bd*2 [hh");
    assert_eq!(h.app.pattern.text, "bd*2 [hh");
    h.key(Key::Enter, COMMAND);
    assert!(
        notice(&h).starts_with("Pattern not applied") && notice(&h).contains("at column"),
        "{}",
        notice(&h)
    );
    assert_eq!(source(&h), original);

    // Enter starts a new line; Ctrl+Enter applies the lines as layers.
    h.ctx.memory_mut(|m| m.request_focus(text_id));
    h.frames(2);
    h.key(Key::A, COMMAND);
    h.type_text("bd*2,");
    h.key(Key::Enter, Modifiers::NONE);
    h.type_text("hh*4");
    assert_eq!(h.app.pattern.text, "bd*2,\nhh*4");
    assert_eq!(source(&h), original, "Enter alone does not apply");
    h.key(Key::Enter, COMMAND);
    assert_eq!(source(&h), "bd*2,\nhh*4");
    assert!(
        h.ctx.memory(|m| m.has_focus(text_id)),
        "the box keeps the focus"
    );
    match pattern_editor::parse_status(&source(&h), 0) {
        pattern_editor::ParseStatus::Ok { events } => assert_eq!(events, 6),
        other => panic!("{other:?}"),
    }

    // An error on the second line is placed by line and column.
    h.type_text(" [");
    h.key(Key::Enter, COMMAND);
    assert!(notice(&h).contains("at line 2, column"), "{}", notice(&h));
    assert_eq!(source(&h), "bd*2,\nhh*4");
    h.key(Key::A, COMMAND);
    h.type_text("bd*4");
    h.key(Key::Enter, COMMAND);
    assert_eq!(source(&h), "bd*4");
    h.run(Action::Undo);
    assert_eq!(source(&h), "bd*2,\nhh*4");
    h.run(Action::Undo);
    h.ctx.memory_mut(|m| m.surrender_focus(text_id));
    h.frames(2);
    assert_eq!(source(&h), original);
    assert_eq!(h.app.pattern.text, original, "the box follows the undo");
}

#[test]
fn sound_names_complete_in_the_pattern_box() {
    let mut h = Harness::demo();
    let drums = h.track_named("Drums");
    let clip = drums.clips[0].id;
    let source = |h: &Harness| match &h.clip(clip).content {
        ClipContent::Pattern { source, .. } => source.clone(),
        other => panic!("{other:?}"),
    };
    let original = source(&h);
    let offered = |h: &Harness| {
        let (names, pick) = h.app.pattern.offered();
        (names.to_vec(), pick)
    };
    h.accessible();
    h.app.sel.select_clip(drums.id, clip);
    h.app.workspace.show(Tab::Editor);
    h.frames(3);
    let text_id = Id::new(("pattern_text", clip));
    h.ctx.memory_mut(|m| m.request_focus(text_id));
    h.frames(2);
    h.key(Key::A, COMMAND);
    h.type_text("bd h");
    h.frames(1);
    assert_eq!(offered(&h), (vec!["hh".into(), "ht".into()], 0));
    assert!(h.find("Tab completes · Esc hides").is_some());

    // The arrows move through the names, Tab takes one and the caret goes
    // after it.
    h.key(Key::ArrowDown, Modifiers::NONE);
    assert_eq!(offered(&h).1, 1);
    h.key(Key::ArrowUp, Modifiers::NONE);
    assert_eq!(offered(&h).1, 0);
    h.key(Key::Tab, Modifiers::NONE);
    assert_eq!(h.app.pattern.text, "bd hh");
    assert!(offered(&h).0.is_empty());
    assert!(
        h.ctx.memory(|m| m.has_focus(text_id)),
        "Tab stays in the box"
    );
    h.type_text("*2");
    assert_eq!(h.app.pattern.text, "bd hh*2");

    // Escape hides the names without leaving the box or applying.
    h.type_text(" s");
    h.frames(1);
    assert_eq!(
        offered(&h).0,
        ["sd", "sh", "sn", "snare", "shaker"].map(String::from)
    );
    h.key(Key::Escape, Modifiers::NONE);
    h.frames(1);
    assert!(offered(&h).0.is_empty());
    assert!(h.ctx.memory(|m| m.has_focus(text_id)));
    assert_eq!(source(&h), original);
    // The next letter offers them again.
    h.type_text("n");
    h.frames(1);
    assert_eq!(offered(&h).0, ["snare"].map(String::from));

    // A click on a name takes it, and the edit goes on.
    h.click_label("snare");
    h.frames(2);
    assert_eq!(h.app.pattern.text, "bd hh*2 snare");
    assert_eq!(source(&h), original, "the click did not apply the text");
    assert!(h.ctx.memory(|m| m.has_focus(text_id)));
    h.key(Key::Enter, COMMAND);
    assert_eq!(source(&h), "bd hh*2 snare");

    // A synth has no sound names to offer.
    let bass = h.track_named("Bass");
    h.app.sel.select_clip(bass.id, bass.clips[0].id);
    h.frames(3);
    assert!(offered(&h).0.is_empty());
}

#[test]
fn the_step_grid_writes_the_pattern() {
    let mut h = Harness::demo();
    let drums = h.track_named("Drums");
    let clip = drums.clips[0].id;
    let source = |h: &Harness| match &h.clip(clip).content {
        ClipContent::Pattern { source, .. } => source.clone(),
        other => panic!("{other:?}"),
    };
    let original = source(&h);
    let cell = |h: &Harness, row: usize, step: usize| {
        h.rect_of(Id::new(("grid_cell", clip, row, step))).center()
    };
    h.accessible();
    h.app.sel.select_clip(drums.id, clip);
    h.app.workspace.show(Tab::Editor);
    h.frames(3);
    assert!(
        h.ctx
            .read_response(Id::new(("grid_cell", clip, 0usize, 0usize)))
            .is_none(),
        "the grid is off at first"
    );
    h.click_in(Tab::Editor, "Step grid");
    assert!(h.app.workspace.pattern_grid);
    h.frames(2);
    // bd*4, ~ sd ~ sd, hh*8 is three rows of 16 steps.
    for (row, name) in ["bd", "sd", "hh"].iter().enumerate() {
        assert!(h.find(name).is_some(), "no {name} row");
        let _ = cell(&h, row, 15);
    }

    // A click on a step in the middle of a kick splits it.
    let before = h.history_len();
    h.click(cell(&h, 0, 2));
    h.frames(2);
    assert_eq!(h.history_len(), before + 1, "one undo step");
    assert_eq!(source(&h), "bd bd bd _ bd _ bd _,\n~ sd ~ sd,\nhh*8");
    assert_eq!(h.app.pattern.text, source(&h), "the box follows the grid");
    // A click on an onset clears the hit.
    h.frames(40);
    h.click(cell(&h, 1, 4));
    h.frames(2);
    assert_eq!(source(&h), "bd bd bd _ bd _ bd _,\n~ ~ ~ sd,\nhh*8");
    h.run(Action::Undo);
    h.run(Action::Undo);
    h.frames(2);
    assert_eq!(source(&h), original);

    // A new row starts empty, and stays after the undo of its first hit.
    pick(&mut h, "Add row…", "cp");
    assert!(h.find("cp").is_some());
    assert_eq!(source(&h), original);
    h.frames(40);
    h.click(cell(&h, 3, 4));
    h.frames(2);
    assert_eq!(
        source(&h),
        "bd*4,\n~ sd ~ sd,\nhh*8,\n~ ~ ~ ~ cp ~ ~ ~ ~ ~ ~ ~ ~ ~ ~ ~"
    );
    h.run(Action::Undo);
    h.frames(2);
    assert_eq!(source(&h), original);
    assert!(h.find("cp").is_some(), "the empty row stays");
    // Its × removes it, and the kick's × removes the kick.
    let remove = |h: &Harness, row: usize| {
        let y = cell(h, row, 0).y;
        h.access
            .iter()
            .find(|(l, r, _)| l == "×" && (r.center().y - y).abs() < 4.0)
            .map(|(_, r, _)| r.center())
            .expect("a × on the row")
    };
    h.frames(40);
    let before = h.history_len();
    h.click(remove(&h, 3));
    h.frames(2);
    assert!(h.find("cp").is_none());
    assert_eq!(h.history_len(), before, "an empty row is not in the text");
    h.frames(40);
    h.click(remove(&h, 0));
    h.frames(2);
    assert_eq!(source(&h), "~ sd ~ sd,\nhh*8");
    h.run(Action::Undo);
    h.frames(2);
    assert_eq!(source(&h), original);

    // Finer steps are offered when every hit stays on one.
    pick(&mut h, "16", "32");
    assert_eq!(h.app.pattern.grid_steps, Some(32));
    assert_eq!(source(&h), original, "the step count alone changes nothing");
    let _ = cell(&h, 0, 31);

    // Typing hides the grid until the text is applied.
    let text_id = Id::new(("pattern_text", clip));
    h.ctx.memory_mut(|m| m.request_focus(text_id));
    h.frames(2);
    h.key(Key::A, COMMAND);
    h.type_text("<bd sd>");
    h.frames(1);
    assert!(
        h.find("Apply or revert what you typed to use the grid")
            .is_some()
    );
    // A pattern that changes from cycle to cycle has no grid.
    h.key(Key::Enter, COMMAND);
    h.frames(2);
    assert!(
        h.find("This pattern changes from cycle to cycle, so one grid can't show it")
            .is_some()
    );

    // The grid is still on next run.
    let mut storage = MemoryStorage::default();
    eframe::App::save(&mut h.app, &mut storage);
    let mut next = Harness::demo();
    next.app.restore(&storage);
    assert!(next.app.workspace.pattern_grid);
}

#[test]
fn the_playing_step_is_lit_while_playing() {
    let mut h = Harness::with_engine(demo_project());
    let drums = h.track_named("Drums");
    let clip = drums.clips[0].id;
    h.app.sel.select_clip(drums.id, clip);
    h.app.workspace.show(Tab::Editor);
    h.app.workspace.pattern_grid = true;
    h.frames(3);
    assert!(pattern_editor::lit_atoms(&h.app, clip).is_empty());
    let bar = h.project().ticks_per_bar();
    let lit = |h: &mut Harness, at: i64| {
        arrange::seek(&mut h.app, at);
        h.heard();
        h.frames(2);
        let pos = h.app.transport.position_ticks;
        assert!(
            pos >= at as f64 && pos < (at + bar / 8) as f64,
            "{pos} is not just after {at}"
        );
        let mut starts: Vec<usize> = pattern_editor::lit_atoms(&h.app, clip)
            .iter()
            .map(|s| s.start)
            .collect();
        starts.sort_unstable();
        starts
    };
    h.run(Action::PlayStop);
    h.heard();
    h.frames(2);
    assert!(h.app.transport.playing);
    // In "bd*4, ~ sd ~ sd, hh*8" three quarters into a bar the kick, the
    // second snare and a hat sound; a quarter into the next bar, the first
    // snare.
    assert_eq!(lit(&mut h, bar * 3 / 4), [0, 13, 17]);
    // Nothing is lit while the box holds other text.
    h.app.pattern.text.push(' ');
    assert!(pattern_editor::lit_atoms(&h.app, clip).is_empty());
    h.app.pattern.text.pop();
    assert_eq!(lit(&mut h, bar + bar / 4), [0, 8, 17]);
    h.run(Action::PlayStop);
    h.heard();
    h.frames(2);
    assert!(pattern_editor::lit_atoms(&h.app, clip).is_empty());
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
    h.app.workspace.show(Tab::Editor);
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
    h.app
        .mixer
        .open_effect((sky_model::ChannelRef::Track(bass.id), id));
    h.app.workspace.show(Tab::Mixer);
    h.frames(3);
    h.app
        .report(actions::apply(
            &h.app.session,
            Transaction::single(Command::RemoveTrack { id: drums.id }),
            None,
        ))
        .unwrap();
    h.frames(3);
    assert_eq!(h.app.mixer.open_effects.len(), 1);
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
            h.app
                .mixer
                .open_effect((sky_model::ChannelRef::Track(t.id), e.id));
            h.frames(2);
        }
    }
}

#[test]
fn the_acid_template_renders_everywhere() {
    let mut h = Harness::with_project(sky_model::templates::acid_project());
    h.every_tab();
    for t in h.project().tracks.clone() {
        for c in &t.clips {
            h.app.sel.select_clip(t.id, c.id);
            h.every_tab();
        }
    }
    h.run(Action::NewEmpty);
    h.run(Action::NewAcid);
    assert_eq!(h.project().name, "Acid loop");
}

#[test]
fn step_clips_are_edited_by_clicking_steps() {
    let mut h = Harness::demo();
    let drums = h.track_named("Drums");
    let steps_of = |h: &Harness, id: ClipId| match h.clip(id).content {
        ClipContent::Steps(s) => s,
        other => panic!("{other:?}"),
    };
    // A new step clip on drums starts from a beat.
    let start = h.project().ticks_per_bar() * 8;
    let id = h
        .app
        .report(actions::add_step_clip(
            &h.app.session,
            drums.id,
            start,
            start / 2,
            actions::starter_steps(&drums),
        ))
        .unwrap();
    assert_eq!(steps_of(&h, id).lanes.len(), 3);
    h.app.sel.select_clip(drums.id, id);
    h.app.workspace.show(Tab::Editor);
    h.frames(3);
    let cell = |h: &Harness, lane: usize, step: usize| {
        h.app
            .steps
            .cells
            .iter()
            .find(|(at, _)| *at == (lane, step))
            .map(|(_, r)| r.center())
            .expect("the step is drawn")
    };
    // A click turns a step on and selects it; the inspector shows.
    let before = h.history_len();
    h.click(cell(&h, 0, 1));
    assert!(steps_of(&h, id).lanes[0].steps[1].is_note());
    assert_eq!(h.app.steps.selected, Some((id, 0, 1)));
    assert_eq!(h.history_len(), before + 1);
    h.frames(2);
    // Shift accents, Ctrl ties, and a second click turns it off.
    h.set_modifiers(Modifiers::SHIFT);
    h.click(cell(&h, 0, 1));
    assert!(steps_of(&h, id).lanes[0].steps[1].accent);
    h.set_modifiers(Modifiers::CTRL);
    h.click(cell(&h, 0, 2));
    assert_eq!(
        steps_of(&h, id).lanes[0].steps[2].kind,
        sky_model::StepKind::Tie
    );
    h.set_modifiers(Modifiers::NONE);
    h.click(cell(&h, 0, 1));
    assert!(!steps_of(&h, id).lanes[0].steps[1].is_note());
    h.run(Action::Undo);
    assert!(steps_of(&h, id).lanes[0].steps[1].accent);
    // Settings and locks go through the same command.
    h.app
        .report(actions::edit_steps(&h.app.session, id, None, |s| {
            s.lanes[2].steps[2].locks.insert("hat_decay".into(), 2.0);
            s.lanes[2].resize(12);
            s.swing = sky_model::Swing::new(60, 16);
        }))
        .unwrap();
    h.app.steps.selected = Some((id, 2, 2));
    h.frames(3);
    let steps = steps_of(&h, id);
    assert_eq!(steps.lanes[2].len(), 12);
    assert_eq!(steps.locked_params(), ["hat_decay"]);
    // The arrange view draws the clip, and an Acid track starts from a line.
    h.app.workspace.show(Tab::Editor);
    h.show_bars(16);
    let acid = Track::new_instrument("Acid", sky_dsp::InstrumentType::Acid);
    assert!(
        actions::starter_steps(&acid).lanes[0]
            .steps
            .iter()
            .any(|s| s.slide)
    );
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
    h.app.workspace.show(Tab::Ai);
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
fn ai_changes_light_what_they_touched_and_show_it() {
    use sky_model::ChannelRef;
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
    let drums = h.track_named("Drums");
    let bass = h.track_named("Bass");
    let pad = h.track_named("Pad");
    let beat = drums.clips[0].id;
    let reverb = pad.channel.effects[0].id;
    let bass_level = |db: f32| Command::SetChannel {
        channel: ChannelRef::Track(bass.id),
        volume_db: Some(db),
        pan: None,
        mute: None,
        solo: None,
    };
    // A change made here lights nothing.
    assert!(
        h.app
            .session
            .apply(Transaction::single(bass_level(-2.0)))
            .is_ok()
    );
    h.frames(2);
    assert!(h.app.ai.lit().is_empty());
    // An assistant's changes light the clip, the strip and the effect,
    // and the headers of the tracks they are on.
    let changes = [
        (
            "Mute clip Beat",
            "clip_set",
            Command::SetClipMuted {
                id: beat,
                muted: true,
            },
        ),
        ("Change Bass", "track_set", bass_level(-4.0)),
        (
            "Change Reverb on Pad",
            "effect_set",
            Command::SetEffectParam {
                channel: ChannelRef::Track(pad.id),
                effect: reverb,
                param: "mix".into(),
                value: Some(0.5),
            },
        ),
    ];
    for (label, tool, command) in changes {
        let origin = Origin::Mcp {
            client: "test-client".into(),
            tool: tool.into(),
        };
        let tx = Transaction::new(label, origin, vec![command]);
        assert!(h.app.session.apply(tx).is_ok());
    }
    h.frames(2);
    let now = h.app.now;
    let ai = &h.app.ai;
    assert_eq!(ai.lit().len(), 3);
    assert_eq!(ai.clip_glow(beat, now), 1.0);
    assert_eq!(ai.channel_glow(ChannelRef::Track(bass.id), now), 1.0);
    assert_eq!(ai.effect_glow(ChannelRef::Track(pad.id), reverb, now), 1.0);
    for track in [&drums, &bass, &pad] {
        assert_eq!(ai.track_glow(track, now), 1.0, "{}", track.name);
    }
    // Only the reverb changed on the pad's strip, and nothing on the master.
    assert_eq!(ai.channel_glow(ChannelRef::Track(pad.id), now), 0.0);
    assert_eq!(ai.channel_glow(ChannelRef::Master, now), 0.0);
    h.every_tab();

    // Show brings each change into view.
    h.app.workspace.show(Tab::Ai);
    h.accessible();
    let show = |h: &mut Harness, label: &str| {
        let row = h.find(label).unwrap_or_else(|| panic!("no row {label:?}"));
        let button = h
            .access
            .iter()
            .filter(|(l, r, _)| {
                l == "Show" && (r.center().y - row.center().y).abs() < 8.0 && r.left() > row.right()
            })
            .map(|(_, r, _)| *r)
            .min_by(|a, b| a.left().total_cmp(&b.left()))
            .unwrap_or_else(|| panic!("no Show beside {label:?}"));
        h.click(button.center());
        h.frames(2);
    };
    show(&mut h, "Change Reverb on Pad");
    assert!(h.app.workspace.is_front(Tab::Mixer));
    assert_eq!(
        h.app.mixer.open_effects,
        [(ChannelRef::Track(pad.id), reverb)]
    );
    assert_eq!(h.app.sel.track, Some(pad.id));
    h.app.workspace.show(Tab::Ai);
    h.frames(3);
    show(&mut h, "Mute clip Beat");
    assert!(h.app.workspace.is_front(Tab::Editor));
    assert_eq!(h.app.sel.clip, Some(beat));
    assert_eq!(h.app.sel.clips, [beat]);

    // An undone change goes dark at once; the others fade out.
    let seq = h
        .app
        .view
        .activity
        .iter()
        .find(|a| a.label == "Change Reverb on Pad")
        .map(|a| a.seq)
        .expect("the reverb change");
    assert!(h.app.session.undo_to(seq).is_ok());
    h.frames(2);
    let now = h.app.now;
    assert_eq!(
        h.app.ai.effect_glow(ChannelRef::Track(pad.id), reverb, now),
        0.0
    );
    assert_eq!(h.app.ai.clip_glow(beat, now), 1.0);
    h.time += 5.0;
    h.frames(1);
    let fading = h.app.ai.clip_glow(beat, h.app.now);
    assert!(fading > 0.0 && fading < 1.0, "{fading}");
    h.time += 4.0;
    h.frames(1);
    assert!(h.app.ai.lit().is_empty());
    assert!(!h.app.ai.flashing(h.app.now));
}

#[test]
fn mixer_strips_line_up_and_fill_the_panel() {
    let mut h = Harness::demo();
    h.app.workspace.show(Tab::Mixer);
    h.accessible();
    // The level under each fader, and the insert slots, by strip.
    let in_mixer = |h: &Harness, label: &str| -> Vec<Rect> {
        let panel = h.panel_rect(Tab::Mixer);
        let mut found: Vec<Rect> = h
            .access
            .iter()
            .filter(|(l, r, role)| {
                *role != Role::TextRun && panel.contains(r.center()) && l == label
            })
            .map(|(_, r, _)| *r)
            .collect();
        found.sort_by(|a, b| a.left().total_cmp(&b.left()));
        found
    };
    let levels = |h: &Harness| -> Vec<Rect> {
        let panel = h.panel_rect(Tab::Mixer);
        h.access
            .iter()
            .filter(|(l, r, role)| {
                *role == Role::Label && panel.contains(r.center()) && l.ends_with(" dB")
            })
            .map(|(_, r, _)| *r)
            .collect()
    };
    let same_row = |rects: &[Rect]| {
        rects
            .iter()
            .all(|r| (r.center().y - rects[0].center().y).abs() < 1.5)
    };
    assert_eq!(mixer::slot_count(&h.project()), 2);
    let before = levels(&h);
    assert_eq!(before.len(), 4, "a level on every strip");
    assert!(same_row(&before), "{before:?}");
    let panel = h.panel_rect(Tab::Mixer);
    assert!(
        panel.bottom() - before[0].bottom() < 30.0,
        "the faders fill the panel"
    );
    // Drums and Bass have no effects, Pad a reverb and the master a
    // limiter: the first slot lines up across them.
    let add = in_mixer(&h, "+ Effect");
    assert_eq!(add.len(), 4);
    let reverb = in_mixer(&h, "Reverb")[0];
    let limiter = in_mixer(&h, "Limiter")[0];
    assert!(
        same_row(&[add[0], add[1], reverb, limiter]),
        "{add:?} {reverb:?} {limiter:?}"
    );
    assert!(same_row(&[add[2], add[3]]));
    assert!(add[2].top() > reverb.bottom());

    // A third effect on one strip adds a slot to all of them; the levels
    // stay put and the faders get shorter.
    let drums = h.track_named("Drums").id;
    for kind in [sky_dsp::EffectType::Delay, sky_dsp::EffectType::Reverb] {
        h.app
            .report(actions::add_effect(
                &h.app.session,
                sky_model::ChannelRef::Track(drums),
                kind,
            ))
            .unwrap();
    }
    h.frames(3);
    assert_eq!(mixer::slot_count(&h.project()), 3);
    let after = levels(&h);
    assert!(same_row(&after), "{after:?}");
    assert_eq!(after[0].top(), before[0].top());
    // Drums now has its "+ Effect" in the third slot, Bass in the first,
    // Pad and the master in the second.
    let add = in_mixer(&h, "+ Effect");
    assert!(same_row(&add[2..]), "{add:?}");
    assert!(add[0].top() > add[2].top() && add[2].top() > add[1].top());

    // A taller window gives the faders the room.
    h.screen = Rect::from_min_size(Pos2::ZERO, vec2(1440.0, 1200.0));
    h.frames(3);
    let taller = levels(&h);
    assert!(same_row(&taller));
    assert!(taller[0].top() > after[0].top() + 100.0);
    let panel = h.panel_rect(Tab::Mixer);
    assert!(panel.bottom() - taller[0].bottom() < 30.0);
}

#[test]
fn the_master_measures_loudness_while_playing() {
    let mut h = Harness::with_engine(demo_project());
    h.app.workspace.show(Tab::Mixer);
    h.accessible();
    let st = |h: &Harness| {
        let l = &h.app.mixer.loudness;
        (l.meter.momentary(), l.meter.integrated(), l.true_peak)
    };
    assert_eq!(st(&h), (None, None, 0.0));
    assert!(h.find("LUFS").is_some() && h.find("dBTP").is_some());
    h.run(Action::PlayStop);
    // Two seconds of the song.
    for _ in 0..40 {
        h.heard();
    }
    h.frames(2);
    let (momentary, integrated, true_peak) = st(&h);
    assert!(momentary.is_some());
    let integrated = integrated.expect("integrated loudness");
    assert!((-30.0..-8.0).contains(&integrated), "{integrated}");
    assert!(true_peak > 0.25, "{true_peak}");
    assert!(h.find(&format!("{integrated:.1}")).is_some(), "the readout");
    // Stopped, the integrated loudness and the peak stay.
    h.run(Action::PlayStop);
    for _ in 0..40 {
        h.heard();
    }
    h.frames(2);
    let (momentary, still, peak) = st(&h);
    assert!(
        momentary.is_none_or(|m| m < -40.0),
        "only the reverb's tail: {momentary:?}"
    );
    assert_eq!(still, Some(integrated));
    assert_eq!(peak, true_peak);
    // Reset starts them again.
    h.click_in(Tab::Mixer, "Reset");
    let (_, integrated, peak) = st(&h);
    assert_eq!((integrated, peak), (None, 0.0));
    // So does pressing play.
    h.app.mixer.loudness.true_peak = 4.0;
    h.run(Action::PlayStop);
    for _ in 0..10 {
        h.heard();
    }
    h.frames(2);
    let (_, again, peak) = st(&h);
    assert!(again.is_some());
    assert!(peak > 0.0 && peak < 4.0, "{peak}");
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
    h.app.workspace.show(Tab::Editor);
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
    assert!(h.app.workspace.is_front(Tab::Instrument));
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
    h.app.workspace.show(Tab::Instrument);
    h.frames(3);
    h.app.add_bank_pattern(kit.id);
    let clip = h.app.sel.clip.expect("the pattern clip is selected");
    match &h.clip(clip).content {
        ClipContent::Pattern { source, .. } => assert_eq!(source, "bd*4, ~ sd ~ sd, hh*8"),
        other => panic!("not a pattern: {other:?}"),
    }
    // Its pattern box offers the bank's sound names.
    h.app.workspace.show(Tab::Editor);
    h.frames(3);
    let text_id = Id::new(("pattern_text", clip));
    h.ctx.memory_mut(|m| m.request_focus(text_id));
    h.frames(2);
    h.key(Key::End, COMMAND);
    h.type_text(", s");
    h.frames(1);
    assert_eq!(h.app.pattern.offered().0, ["sd"]);
    h.key(Key::Tab, Modifiers::NONE);
    h.key(Key::Enter, COMMAND);
    match &h.clip(clip).content {
        ClipContent::Pattern { source, .. } => {
            assert_eq!(source, "bd*4, ~ sd ~ sd, hh*8, sd")
        }
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
        include_str!("asset_lane.rs"),
        include_str!("audio.rs"),
        include_str!("audition.rs"),
        include_str!("browser.rs"),
        include_str!("clip_tools.rs"),
        include_str!("dialogs.rs"),
        include_str!("edit.rs"),
        include_str!("game.rs"),
        include_str!("jobs.rs"),
        include_str!("keys.rs"),
        include_str!("lib.rs"),
        include_str!("live_pattern.rs"),
        include_str!("loop_inspector.rs"),
        include_str!("menu.rs"),
        include_str!("mixer.rs"),
        include_str!("palette.rs"),
        include_str!("params.rs"),
        include_str!("pattern_editor.rs"),
        include_str!("piano_roll.rs"),
        include_str!("ruler_lanes.rs"),
        include_str!("sounds.rs"),
        include_str!("theme.rs"),
        include_str!("tracks.rs"),
        include_str!("transport.rs"),
        include_str!("velocity_lane.rs"),
        include_str!("waveform.rs"),
        include_str!("widgets.rs"),
        include_str!("workspace.rs"),
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
    h.app.workspace.show(Tab::Editor);
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
    h.app.workspace.show(Tab::Instrument);
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
    off.app.workspace.show(Tab::Instrument);
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
    assert!(h.app.workspace.is_front(Tab::Instrument));

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

#[test]
fn panels_split_float_close_and_reset() {
    let mut h = Harness::demo();
    h.accessible();

    // A first run: the browser on the left, the arrangement above the
    // editor, and the mixer, instrument and AI tabs behind the editor.
    for tab in Tab::ALL {
        assert!(h.app.workspace.is_open(tab), "{tab:?} is open");
    }
    for tab in [Tab::Arrange, Tab::Browser, Tab::Editor] {
        assert!(h.app.workspace.is_front(tab), "{tab:?} is in front");
    }
    assert!(!h.app.workspace.is_front(Tab::Mixer));
    let browser = h.panel_rect(Tab::Browser);
    let arrange = h.panel_rect(Tab::Arrange);
    let lower = h.panel_rect(Tab::Editor);
    assert!(browser.left() < 1.0, "{browser:?}");
    assert!((0.15..0.21).contains(&(browser.width() / h.screen.width())));
    assert!(arrange.left() >= browser.right() - 1.0);
    assert!(arrange.bottom() <= lower.top() + 1.0);
    assert!(arrange.height() > lower.height());
    assert_eq!(h.panel_rect(Tab::Mixer), lower);

    // Dragging the mixer's tab onto the right-hand drop button puts it
    // beside the editor, and both show.
    let tab = h.find("Mixer").expect("the mixer's tab");
    let overlay = egui_dock::Style::from_egui(&h.ctx.global_style()).overlay;
    let gap = overlay.button_spacing;
    let area = lower.shrink(gap);
    let side = ((area.width() - 2.0 * gap) / 3.0)
        .min((area.height() - 2.0 * gap) / 3.0)
        .min(overlay.max_button_size);
    let right = area.center() + vec2(side + gap, 0.0);
    h.move_to(tab.center());
    h.pointer(tab.center(), true);
    h.frame();
    for step in 1..=12 {
        h.move_to(tab.center().lerp(right, step as f32 / 12.0));
    }
    h.frames(3);
    h.pointer(right, false);
    h.frames(3);
    assert!(h.app.workspace.is_front(Tab::Mixer));
    assert!(h.app.workspace.is_front(Tab::Editor));
    let editor = h.panel_rect(Tab::Editor);
    let mixer = h.panel_rect(Tab::Mixer);
    assert!(mixer.left() >= editor.right() - 3.0, "{editor:?} {mixer:?}");
    assert_eq!(h.panel_rect(Tab::Instrument), editor);

    // Ctrl+B hides the browser and brings it back on the left.
    h.key(Key::B, COMMAND);
    assert!(!h.app.workspace.is_open(Tab::Browser));
    assert!(!h.app.browser.open);
    assert!(h.panel_rect(Tab::Arrange).left() < 1.0);
    h.key(Key::B, COMMAND);
    assert!(h.app.browser.open);
    assert!(h.panel_rect(Tab::Browser).left() < 1.0);

    // View > Instrument closes it; again, it comes back beside the editor.
    h.click_label("View");
    h.click_role("Instrument", Some(Role::CheckBox));
    assert!(!h.app.workspace.is_open(Tab::Instrument));
    h.click_label("View");
    h.click_role("Instrument", Some(Role::CheckBox));
    assert!(h.app.workspace.is_front(Tab::Instrument));
    assert_eq!(h.panel_rect(Tab::Instrument), h.panel_rect(Tab::Editor));

    // The arrangement cannot be closed.
    h.run(Action::TogglePanel(Tab::Arrange));
    assert!(h.app.workspace.is_open(Tab::Arrange));

    // A floating panel still draws, and comes forward when asked.
    let path = h.app.workspace.dock().find_tab(&Tab::Ai).expect("AI tab");
    let window = Rect::from_min_size(pos2(500.0, 200.0), vec2(420.0, 300.0));
    h.app.workspace.dock_mut().detach_tab(path, window);
    h.frames(2);
    let path = h.app.workspace.dock().find_tab(&Tab::Ai).expect("AI tab");
    assert!(!path.surface.is_main());
    h.every_tab();
    assert!(h.app.workspace.is_front(Tab::Ai));

    // Reset puts every panel back where a first run has it.
    h.click_label("View");
    h.click_label("Reset the panel layout");
    h.frames(1);
    assert_eq!(h.panel_rect(Tab::Mixer), h.panel_rect(Tab::Editor));
    assert!(
        h.app
            .workspace
            .dock()
            .find_tab(&Tab::Ai)
            .is_some_and(|p| p.surface.is_main())
    );
    assert!(h.app.workspace.is_front(Tab::Editor));
}

/// eframe's storage, in memory.
#[derive(Default)]
struct MemoryStorage(std::collections::HashMap<String, String>);

impl eframe::Storage for MemoryStorage {
    fn get_string(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }

    fn set_string(&mut self, key: &str, value: String) {
        self.0.insert(key.to_string(), value);
    }

    fn remove_string(&mut self, key: &str) {
        self.0.remove(key);
    }

    fn flush(&mut self) {}
}

/// The items of a submenu, by label.
fn submenu<'a>(entries: &'a [menu::Entry], label: &str) -> &'a [menu::Entry] {
    entries
        .iter()
        .find_map(|e| match e {
            menu::Entry::Sub(l, list) if l == label => Some(list.as_slice()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no submenu {label}"))
}

fn menu_entries<'a>(menus: &'a [menu::Menu], title: &str) -> &'a [menu::Entry] {
    &menus
        .iter()
        .find(|m| m.title == title)
        .unwrap_or_else(|| panic!("no menu {title}"))
        .entries
}

fn item_labels(entries: &[menu::Entry]) -> Vec<&str> {
    entries
        .iter()
        .filter_map(|e| match e {
            menu::Entry::Item(it) => Some(it.label.as_str()),
            menu::Entry::Note(text) => Some(*text),
            _ => None,
        })
        .collect()
}

#[test]
fn the_layout_and_recent_projects_come_back_next_run() {
    use crate::workspace::STORAGE_KEY;
    use eframe::Storage as _;

    let dir = tempfile::tempdir().expect("temp dir");
    let mut h = Harness::demo();

    // Saving a project, then a second one, lists both, newest first.
    let first = dir.path().join("First.skyproj");
    let second = dir.path().join("Second.skyproj");
    h.app.session.save(Some(&first)).expect("saved");
    h.frames(2);
    h.app.session.new_project(empty_project("Second"));
    h.app.session.save(Some(&second)).expect("saved");
    h.frames(2);
    assert_eq!(h.app.workspace.recent, [second.clone(), first.clone()]);

    // Hide the browser and AI activity, bring the mixer forward, and quit.
    h.run(Action::TogglePanel(Tab::Browser));
    h.run(Action::TogglePanel(Tab::Ai));
    h.app.workspace.show(Tab::Mixer);
    h.frames(2);
    let mut storage = MemoryStorage::default();
    eframe::App::save(&mut h.app, &mut storage);

    // The next run starts as this one ended.
    let mut next = Harness::demo();
    next.app.restore(&storage);
    next.frames(2);
    assert!(!next.app.workspace.is_open(Tab::Browser));
    assert!(!next.app.browser.open);
    assert!(!next.app.workspace.is_open(Tab::Ai));
    assert!(next.app.workspace.is_front(Tab::Mixer));
    assert_eq!(next.app.workspace.recent, [second.clone(), first.clone()]);

    // File > Open recent lists them and opens one.
    let menus = menu::menus(&next.app);
    let recent = submenu(menu_entries(&menus, "File"), "Open recent");
    assert_eq!(
        item_labels(recent),
        ["Second.skyproj", "First.skyproj", "Clear the list"]
    );
    next.run(Action::OpenRecent(first.clone()));
    next.wait_jobs();
    assert_eq!(next.app.view.path.as_deref(), Some(first.as_path()));
    assert_eq!(next.app.workspace.recent, [first.clone(), second.clone()]);

    // One that has gone says so and leaves the list.
    let gone = dir.path().join("Gone.skyproj");
    next.app.workspace.remember(&gone);
    next.run(Action::OpenRecent(gone.clone()));
    assert!(next.app.notice.as_ref().is_some_and(|n| n.error));
    assert!(!next.app.workspace.recent.contains(&gone));
    assert_eq!(next.app.view.path.as_deref(), Some(first.as_path()));

    next.run(Action::ClearRecent);
    let menus = menu::menus(&next.app);
    let recent = submenu(menu_entries(&menus, "File"), "Open recent");
    assert_eq!(item_labels(recent), ["No recent projects"]);

    // A layout this version cannot use leaves the first-run layout in
    // place, and keeps the recent projects when it can read them.
    let no_arrange = egui_dock::DockState::new(vec![Tab::Mixer]);
    let twice = egui_dock::DockState::new(vec![Tab::Arrange, Tab::Mixer, Tab::Mixer]);
    for (saved, recent) in [
        ("not json".to_string(), 0),
        (r#"{"dock": 5, "recent": ["a.skyproj"]}"#.to_string(), 1),
        (
            serde_json::json!({ "dock": no_arrange, "recent": ["a.skyproj"] }).to_string(),
            1,
        ),
        (
            serde_json::json!({ "dock": twice, "recent": ["a.skyproj"] }).to_string(),
            1,
        ),
    ] {
        let mut storage = MemoryStorage::default();
        storage.set_string(STORAGE_KEY, saved.clone());
        let mut h = Harness::demo();
        h.app.restore(&storage);
        h.frames(2);
        assert!(
            Tab::ALL.iter().all(|t| h.app.workspace.is_open(*t)),
            "{saved}"
        );
        assert_eq!(h.app.workspace.recent.len(), recent, "{saved}");
    }
}

#[test]
fn the_menu_bar_opens_and_runs_what_it_lists() {
    let mut h = Harness::demo();
    h.accessible();

    // Each menu opens on a click.
    for (title, first) in [
        ("File", "New empty project"),
        ("Edit", "Delete"),
        ("Track", "Add synth track"),
        ("Transport", "Set tempo…"),
        ("View", "Reset the panel layout"),
        ("Help", "Command palette…"),
    ] {
        h.click_label(title);
        assert!(h.find(first).is_some(), "{title} lists {first}");
        h.key(Key::Escape, Modifiers::NONE);
        assert!(h.find(first).is_none(), "{title} closes");
    }

    // A submenu: File > Open recent.
    h.click_label("File");
    h.click_label("Open recent");
    assert!(h.find("No recent projects").is_some());
    h.key(Key::Escape, Modifiers::NONE);

    // View > Mixer closes the mixer.
    h.click_label("View");
    h.click_role("Mixer", Some(Role::CheckBox));
    assert!(!h.app.workspace.is_open(Tab::Mixer));

    // Help > About.
    h.click_label("Help");
    h.click_label("About Skyware Sound");
    assert!(h.app.about);
    assert!(
        h.find(&format!("Version {}", env!("CARGO_PKG_VERSION")))
            .is_some()
    );

    // Edit names what Undo would undo, and is greyed out with nothing to undo.
    let undo = |h: &Harness| {
        let menus = menu::menus(&h.app);
        match &menu_entries(&menus, "Edit")[0] {
            menu::Entry::Item(it) => (it.label.clone(), it.enabled),
            _ => panic!("Undo first"),
        }
    };
    assert_eq!(undo(&h), ("Undo".to_string(), false));
    h.run(Action::AddTrack(NewTrack::Audio));
    let (label, enabled) = undo(&h);
    assert!(enabled && label.starts_with("Undo “"), "{label}");

    // Track offers effects for the selected track, and Transport shows
    // whether the loop is on.
    let menus = menu::menus(&h.app);
    let track = h.app.sel.track.expect("the new track is selected");
    let name = h.project().track(track).expect("track").name.clone();
    submenu(
        menu_entries(&menus, "Track"),
        &format!("Add effect to {name}"),
    );
    let looping = menu_entries(&menus, "Transport").iter().any(|e| {
        matches!(e, menu::Entry::Item(it) if it.label == "Loop"
            && it.checked == Some(h.project().loop_enabled))
    });
    assert!(looping);

    // Every item runs without trouble.
    fn actions(entries: &[menu::Entry], out: &mut Vec<Action>) {
        for e in entries {
            match e {
                menu::Entry::Item(it) => out.push(it.action.clone()),
                menu::Entry::Sub(_, list) => actions(list, out),
                _ => {}
            }
        }
    }
    let mut all = Vec::new();
    for m in menu::menus(&h.app) {
        actions(&m.entries, &mut all);
    }
    assert!(all.len() > 40, "{} items", all.len());
    for action in all {
        // Loading sounds would read sample packs over the network.
        if action == Action::LoadSounds {
            continue;
        }
        h.run(action);
        h.wait_jobs();
        h.app.dialogs = Default::default();
        h.app.palette.close();
        h.app.about = false;
    }
    h.ctx.set_zoom_factor(1.0);
    h.every_tab();
}

fn tick_x(h: &Harness, tick: i64) -> f32 {
    h.app.arrange.x_of(tick as f64)
}

fn lane_rect(h: &Harness, track: sky_core::TrackId) -> Rect {
    h.app
        .arrange
        .lanes
        .iter()
        .find(|(id, _)| *id == track)
        .map(|(_, r)| *r)
        .expect("lane visible")
}

#[test]
fn a_marquee_selects_clips_that_then_move_and_delete_together() {
    let mut h = Harness::demo();
    h.show_bars(12);
    let bar = h.project().ticks_per_bar();
    let bar_px = h.app.arrange.px_per_tick * bar as f32;
    let drums = h.track_named("Drums").clips[0].id;
    let bass = h.track_named("Bass");
    let pad = h.track_named("Pad");
    let (b, p) = (bass.clips[0].id, pad.clips[0].id);

    // From empty space at bar 7 on the pad lane, up and left into the bass
    // clip: both clips are picked, the drums above are not.
    let from = pos2(tick_x(&h, bar * 6), lane_rect(&h, pad.id).center().y);
    let to = pos2(tick_x(&h, bar * 2), lane_rect(&h, bass.id).center().y);
    h.drag(from, to);
    assert!(h.app.arrange.marquee.is_none());
    assert_eq!(h.app.sel.clips.len(), 2);
    assert!(h.app.sel.clips.contains(&b) && h.app.sel.clips.contains(&p));
    assert!(h.app.sel.clip.is_some_and(|c| c == b || c == p));

    // Dragging one moves both, as one undo step.
    let history = h.history_len();
    let r = arrange::clip_screen_rect(&h.app, b).expect("clip visible");
    let from = pos2(r.left() + 40.0, r.center().y);
    h.drag(from, from + vec2(bar_px, 0.0));
    assert_eq!(h.clip(b).start, bar);
    assert_eq!(h.clip(p).start, bar);
    assert_eq!(h.clip(drums).start, 0);
    assert_eq!(h.history_len(), history + 1);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Move 2 clips"));

    // Shift-click takes one out, and Ctrl-click puts it back.
    let r = arrange::clip_screen_rect(&h.app, p).expect("clip visible");
    h.set_modifiers(Modifiers::SHIFT);
    h.click(r.center());
    assert_eq!(h.app.sel.clips, [b]);
    h.set_modifiers(COMMAND);
    h.click(r.center());
    h.set_modifiers(Modifiers::NONE);
    h.frame();
    assert_eq!(h.app.sel.clips.len(), 2);

    // Ctrl+A takes every clip; Delete removes them in one step.
    h.key(Key::A, COMMAND);
    assert_eq!(h.app.sel.clips.len(), 3);
    h.key(Key::Delete, Modifiers::NONE);
    assert!(h.project().tracks.iter().all(|t| t.clips.is_empty()));
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Delete 3 clips"));
    h.key(Key::Z, COMMAND);
    assert_eq!(h.project().content_end(), bar * 5);

    // Escape selects nothing, and so does a click on an empty lane.
    h.key(Key::A, COMMAND);
    h.key(Key::Escape, Modifiers::NONE);
    assert!(h.app.sel.clips.is_empty());
    h.key(Key::A, COMMAND);
    h.click(pos2(tick_x(&h, bar * 8), lane_rect(&h, pad.id).center().y));
    assert!(h.app.sel.clips.is_empty());

    // Escape during a marquee puts the selection back as it was.
    let r = arrange::clip_screen_rect(&h.app, b).expect("clip visible");
    h.click(r.center());
    let from = pos2(tick_x(&h, bar * 7), lane_rect(&h, pad.id).center().y);
    h.move_to(from);
    h.pointer(from, true);
    h.frame();
    for step in 1..=5 {
        h.move_to(from.lerp(
            pos2(tick_x(&h, 0), lane_rect(&h, bass.id).top()),
            step as f32 / 5.0,
        ));
    }
    assert_eq!(h.app.sel.clips.len(), 2, "the marquee picks as it goes");
    h.key(Key::Escape, Modifiers::NONE);
    assert_eq!(h.app.sel.clips, [b]);
    h.pointer(from, false);
    h.frames(2);
    assert_eq!(h.app.sel.clips, [b]);
}

#[test]
fn clips_are_duplicated_copied_cut_and_pasted_at_the_playhead() {
    let mut h = Harness::with_engine(demo_project());
    h.show_bars(24);
    let bar = h.project().ticks_per_bar();
    let bass = h.track_named("Bass");
    let clip = bass.clips[0].id;
    let r = arrange::clip_screen_rect(&h.app, clip).expect("clip visible");
    h.click(r.center());
    assert_eq!(h.app.sel.clips, [clip]);

    // Ctrl+D puts a copy right after, with notes of its own, and selects it.
    h.key(Key::D, COMMAND);
    let bass = h.track_named("Bass");
    assert_eq!(bass.clips.len(), 2);
    let dup = bass.clips.iter().find(|c| c.id != clip).expect("a copy");
    assert_eq!(dup.start, bar * 4);
    assert_eq!(h.app.sel.clips, [dup.id]);
    let (ClipContent::Midi { notes: a }, ClipContent::Midi { notes: b }) =
        (&h.clip(clip).content, &dup.content)
    else {
        panic!("MIDI clips");
    };
    assert_eq!(a.len(), b.len());
    assert!(a.iter().all(|n| b.iter().all(|m| m.id != n.id)));
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Duplicate clip"));

    // Copy puts JSON on the system clipboard as well.
    h.key(Key::C, COMMAND);
    let text = h.copied.clone().expect("copied to the system clipboard");
    let Some(Copied::Clips { clips, .. }) = Copied::from_json(&text) else {
        panic!("clips in {text}");
    };
    assert_eq!(clips.len(), 1);

    // Paste goes at the playhead; again without moving it, right after.
    arrange::seek(&mut h.app, bar * 8);
    h.heard();
    h.frames(2);
    assert_eq!(h.app.transport.position_ticks.round() as i64, bar * 8);
    h.key(Key::V, COMMAND);
    h.key(Key::V, COMMAND);
    let starts: Vec<i64> = h
        .track_named("Bass")
        .clips
        .iter()
        .map(|c| c.start)
        .collect();
    assert_eq!(starts, [0, bar * 4, bar * 8, bar * 12]);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Paste clip"));

    // The system's Cut, as Ctrl+X arrives from the window, removes it.
    h.events.push(Event::Cut);
    h.frame();
    let starts: Vec<i64> = h
        .track_named("Bass")
        .clips
        .iter()
        .map(|c| c.start)
        .collect();
    assert_eq!(starts, [0, bar * 4, bar * 8]);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Cut clip"));

    // Clipboard text from another window pastes too, onto the selected
    // track; an audio track cannot take a MIDI clip, so it goes back to
    // the bass.
    let pad = h.track_named("Pad");
    h.app.sel.select_track(pad.id);
    arrange::seek(&mut h.app, bar * 16);
    h.heard();
    h.frames(2);
    h.app.edit.clipboard = None;
    h.events.push(Event::Paste(text.clone()));
    h.frame();
    assert!(
        h.track_named("Pad")
            .clips
            .iter()
            .any(|c| c.start == bar * 16)
    );
    h.run(Action::AddTrack(NewTrack::Audio));
    arrange::seek(&mut h.app, bar * 20);
    h.heard();
    h.frames(2);
    h.events.push(Event::Paste(text));
    h.frame();
    assert!(
        h.track_named("Bass")
            .clips
            .iter()
            .any(|c| c.start == bar * 20)
    );

    // Other text pastes the app's own clipboard, which is empty here.
    h.events.push(Event::Paste("hello".into()));
    h.frame();
    let notice = h.app.notice.as_ref().map(|n| n.text.clone());
    assert_eq!(
        notice.as_deref(),
        Some("Nothing to paste yet: copy clips or notes first")
    );

    // Arrows nudge by the grid or a bar, and move clips between tracks.
    let r = arrange::clip_screen_rect(&h.app, clip).expect("clip visible");
    h.click(r.center());
    h.key(Key::ArrowRight, Modifiers::NONE);
    assert_eq!(h.clip(clip).start, PPQ);
    h.key(Key::ArrowRight, Modifiers::SHIFT);
    assert_eq!(h.clip(clip).start, PPQ + bar);
    h.key(Key::ArrowLeft, Modifiers::SHIFT);
    h.key(Key::ArrowLeft, Modifiers::SHIFT);
    assert_eq!(h.clip(clip).start, 0, "not before the start");
    h.key(Key::ArrowDown, Modifiers::NONE);
    assert_eq!(h.project().find_clip(clip).expect("clip").0.id, pad.id);
    h.key(Key::ArrowDown, Modifiers::NONE);
    assert_eq!(h.project().find_clip(clip).expect("clip").0.id, pad.id);
    let notice = h
        .app
        .notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default();
    assert!(notice.contains("cannot hold"), "{notice}");
    h.key(Key::ArrowUp, Modifiers::NONE);
    assert_eq!(h.project().find_clip(clip).expect("clip").0.id, bass.id);
}

#[test]
fn notes_are_selected_nudged_transposed_and_copied_in_the_piano_roll() {
    let mut h = Harness::demo();
    let pad = h.track_named("Pad");
    let clip = pad.clips[0].id;
    let bar = h.project().ticks_per_bar();
    h.app.sel.select_clip(pad.id, clip);
    h.app.workspace.show(Tab::Editor);
    // Room for the rows the chord is on.
    h.app.workspace.piano.velocity_lane = false;
    h.frames(3);
    h.app.piano.top_pitch = 72.0;
    h.frames(2);
    let notes = |h: &Harness| match &h.clip(clip).content {
        ClipContent::Midi { notes } => notes.clone(),
        _ => panic!("a MIDI clip"),
    };
    let first: Vec<Note> = notes(&h).into_iter().filter(|n| n.start == 0).collect();
    assert_eq!(first.len(), 3);
    let total = notes(&h).len();
    let rect = |h: &Harness, n: &Note| h.rect_of(Id::new(("note", clip, n.id)));

    // A marquee from empty space above the notes takes the first chord.
    let top = first
        .iter()
        .map(|n| rect(&h, n).top())
        .fold(f32::MAX, f32::min);
    let bottom = first
        .iter()
        .map(|n| rect(&h, n).bottom())
        .fold(0.0, f32::max);
    let right = rect(&h, &first[0]).right();
    let from = pos2(right - 20.0, top - 30.0);
    h.drag(from, pos2(right - 60.0, bottom + 2.0));
    let mut chosen = h.app.sel.notes.clone();
    chosen.sort();
    let mut want: Vec<_> = first.iter().map(|n| n.id).collect();
    want.sort();
    assert_eq!(chosen, want);
    assert_eq!(h.app.edit_focus(), crate::app::Focus::Editor);

    let chord = |h: &Harness| -> Vec<Note> {
        notes(h)
            .into_iter()
            .filter(|n| want.contains(&n.id))
            .collect()
    };
    // Right nudges by the grid, Left brings it back, not past the start.
    h.key(Key::ArrowRight, Modifiers::NONE);
    assert!(chord(&h).iter().all(|n| n.start == PPQ / 4));
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Nudge notes"));
    h.key(Key::ArrowLeft, Modifiers::NONE);
    h.key(Key::ArrowLeft, Modifiers::NONE);
    assert!(chord(&h).iter().all(|n| n.start == 0));

    // Up and Down transpose; Shift moves by octaves.
    let pitches = |h: &Harness| -> Vec<u8> {
        let mut p: Vec<u8> = chord(h).iter().map(|n| n.pitch).collect();
        p.sort_unstable();
        p
    };
    let before = pitches(&h);
    h.key(Key::ArrowUp, Modifiers::NONE);
    h.key(Key::ArrowUp, Modifiers::SHIFT);
    h.key(Key::ArrowDown, Modifiers::NONE);
    let up: Vec<u8> = before.iter().map(|p| p + 12).collect();
    assert_eq!(pitches(&h), up);
    h.key(Key::ArrowDown, Modifiers::SHIFT);
    assert_eq!(pitches(&h), before);

    // Ctrl+D copies the chord to just after itself and selects the copy.
    h.key(Key::D, COMMAND);
    assert_eq!(notes(&h).len(), total + 3);
    let copies: Vec<Note> = notes(&h)
        .into_iter()
        .filter(|n| h.app.sel.notes.contains(&n.id))
        .collect();
    assert_eq!(copies.len(), 3);
    let length = first[0].length;
    assert!(copies.iter().all(|n| n.start == length));
    h.key(Key::Z, COMMAND);

    // Ctrl+A and Escape.
    h.key(Key::A, COMMAND);
    assert_eq!(h.app.sel.notes.len(), total);
    h.key(Key::Escape, Modifiers::NONE);
    assert!(h.app.sel.notes.is_empty());

    // Copy the chord and paste it at the playhead (the start here), then
    // again right after.
    h.app.sel.notes = want.clone();
    h.key(Key::C, COMMAND);
    h.key(Key::V, COMMAND);
    h.key(Key::V, COMMAND);
    let all = notes(&h);
    assert_eq!(all.len(), total + 6);
    assert_eq!(all.iter().filter(|n| n.start == 0).count(), 6);
    assert_eq!(all.iter().filter(|n| n.start == length).count(), 3 + 3);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Paste 3 notes"));

    // Cut takes them out; Delete is one step too.
    h.key(Key::X, COMMAND);
    assert_eq!(notes(&h).len(), total + 3);
    h.key(Key::A, COMMAND);
    h.key(Key::Delete, Modifiers::NONE);
    assert!(notes(&h).is_empty());
    h.key(Key::Z, COMMAND);
    assert_eq!(notes(&h).len(), total + 3);

    // Notes paste into another MIDI clip too, and the arrangement keys
    // act on clips once it is clicked again.
    let bass = h.track_named("Bass");
    h.app.sel.select_clip(bass.id, bass.clips[0].id);
    h.app.sel.focus = crate::app::Focus::Editor;
    h.frames(2);
    let before = bass.clips[0].clone();
    h.key(Key::V, COMMAND);
    let ClipContent::Midi { notes: after } = h.clip(before.id).content else {
        panic!("MIDI");
    };
    let ClipContent::Midi { notes: was } = &before.content else {
        panic!("MIDI");
    };
    assert_eq!(after.len(), was.len() + 3);
    assert!(bar > 0);
}

#[test]
fn one_key_command_table_drives_shortcuts_menus_palette_and_the_printed_list() {
    use crate::keys::Scope;

    // No key does two things in the same place.
    let table = keys::table();
    for (i, a) in table.iter().enumerate() {
        for b in &table[i + 1..] {
            let shared = a.keys.iter().any(|k| b.keys.contains(k));
            let apart = matches!(
                (a.scope, b.scope),
                (Scope::Arrange, Scope::Editor) | (Scope::Editor, Scope::Arrange)
            );
            assert!(!shared || apart, "{} and {} share a key", a.label, b.label);
        }
        assert!(keys::GROUPS.contains(&a.group), "{}", a.group);
    }

    // Menus and the palette show the table's keys, and every key command
    // can be found in one of them.
    let h = Harness::demo();
    let menus = menu::menus(&h.app);
    let edit = menu_entries(&menus, "Edit");
    let shortcut = |entries: &[menu::Entry], label: &str| {
        entries
            .iter()
            .find_map(|e| match e {
                menu::Entry::Item(it) if it.label == label => Some(it.shortcut.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no item {label}"))
    };
    assert_eq!(shortcut(edit, "Duplicate"), "Ctrl+D");
    assert_eq!(shortcut(edit, "Redo"), "Ctrl+Shift+Z");
    assert_eq!(
        shortcut(submenu(edit, "Move the selection"), "Notes up an octave"),
        "Shift+Up"
    );
    assert_eq!(shortcut(menu_entries(&menus, "View"), "Browser"), "Ctrl+B");
    let palette = palette::entries(&h.app);
    let copy = palette.iter().find(|e| e.label == "Copy").expect("Copy");
    assert_eq!(copy.shortcut, "Ctrl+C");
    fn actions(entries: &[menu::Entry], out: &mut Vec<Action>) {
        for e in entries {
            match e {
                menu::Entry::Item(it) => out.push(it.action.clone()),
                menu::Entry::Sub(_, list) => actions(list, out),
                _ => {}
            }
        }
    }
    let mut offered: Vec<Action> = palette.into_iter().map(|e| e.action).collect();
    for m in &menus {
        actions(&m.entries, &mut offered);
    }
    for c in &table {
        assert!(offered.contains(&c.action), "{} is in no menu", c.label);
    }

    // docs/keyboard-shortcuts.md is the table, printed.
    let doc = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/keyboard-shortcuts.md");
    if std::env::var_os("UPDATE_SHORTCUTS").is_some() {
        std::fs::write(&doc, keys::markdown()).expect("write the shortcuts");
    }
    let written = std::fs::read_to_string(&doc).unwrap_or_default();
    assert_eq!(
        written.replace("\r\n", "\n"),
        keys::markdown(),
        "docs/keyboard-shortcuts.md is out of date; run UPDATE_SHORTCUTS=1 cargo test -p sky-ui"
    );

    // Keys run what the table says: Ctrl+L loops, Plus zooms the timeline,
    // Ctrl+Plus makes everything bigger, Ctrl+/ lists the shortcuts.
    let mut h = Harness::demo();
    let looping = h.project().loop_enabled;
    h.key(Key::L, COMMAND);
    assert_ne!(h.project().loop_enabled, looping);
    let ppt = h.app.arrange.px_per_tick;
    h.key(Key::Plus, Modifiers::NONE);
    assert!(h.app.arrange.px_per_tick > ppt);
    h.key(Key::F, Modifiers::NONE);
    h.key(Key::Plus, COMMAND);
    assert!(h.ctx.zoom_factor() > 1.0);
    h.key(Key::Num0, COMMAND);
    assert_eq!(h.ctx.zoom_factor(), 1.0);
    h.accessible();
    h.key(Key::Slash, COMMAND);
    assert!(h.app.shortcuts_open);
    h.frames(2);
    assert!(h.find("Duplicate right after").is_some());
    assert!(h.find("Ctrl+Shift+Z or Ctrl+Y").is_some());
    h.click_label("Copy the list");
    assert_eq!(h.copied.as_deref(), Some(keys::markdown().as_str()));

    // Saved as a web page to print.
    h.click_label("Save as a web page…");
    assert!(h.app.dialogs.path_prompt.is_some());
    h.app.dialogs.path_prompt = None;
    let dir = tempfile::tempdir().expect("temp dir");
    let ctx = h.ctx();
    let failed = h
        .app
        .path_chosen(&ctx, PathPurpose::ShortcutsPage, dir.path().join("keys"));
    assert!(failed.is_none(), "{failed:?}");
    let page = std::fs::read_to_string(dir.path().join("keys.html")).expect("page saved");
    assert!(page.contains("<td>Ctrl+D</td><td>Duplicate right after</td>"));

    // Arrows leave a focused widget alone.
    h.app.shortcuts_open = false;
    h.click(pos2(5.0, 5.0));
    let bass = h.track_named("Bass");
    h.app.sel.select_clip(bass.id, bass.clips[0].id);
    h.ctx
        .memory_mut(|m| m.request_focus(Id::new("some widget")));
    h.key(Key::ArrowRight, Modifiers::NONE);
    assert_eq!(h.clip(bass.clips[0].id).start, 0);
}

/// The demo project's loop and its SFX set.
fn demo_assets(h: &Harness) -> (sky_core::AssetId, sky_core::AssetId) {
    let p = h.project();
    let id = |name: &str| p.game.find(name).expect("demo asset").id;
    (id("demo_music"), id("demo_hits"))
}

fn asset(h: &Harness, id: sky_core::AssetId) -> sky_model::GameAsset {
    h.project().game.asset(id).cloned().expect("asset")
}

#[test]
fn game_assets_are_regions_under_the_ruler_that_drag_to_edit() {
    use sky_model::{GameAssetKind, TimeRange};
    let mut h = Harness::demo();
    h.accessible();
    let (music, hits) = demo_assets(&h);
    let bar = h.project().ticks_per_bar();
    let beat = bar / 4;

    // The loop and the hits overlap, so they take a row each, and the
    // tracks start below them. A project without assets has no lane.
    assert_eq!(asset_lane::rows(&h.project()), vec![(music, 0), (hits, 1)]);
    assert_eq!(asset_lane::height(&empty_project("None")), 0.0);
    let loop_rect = h.rect_of(asset_lane::region_id(music));
    let hits_rect = h.rect_of(asset_lane::region_id(hits));
    assert!(hits_rect.top() >= loop_rect.bottom());
    let drums = h.track_named("Drums");
    assert!(lane_rect(&h, drums.id).top() >= hits_rect.bottom());
    assert!((loop_rect.right() - tick_x(&h, 4 * bar)).abs() < 1.0);

    // Dragging the loop's right edge, its seam, shortens the body by a bar
    // in one undo step.
    let before = h.history_len();
    let y = loop_rect.center().y;
    h.drag(
        pos2(loop_rect.right() - 2.0, y),
        pos2(tick_x(&h, 3 * bar), y),
    );
    let range = |h: &Harness| match asset(h, music).kind {
        GameAssetKind::Loop { range, intro_start } => (range, intro_start),
        GameAssetKind::Sfx { .. } => unreachable!(),
    };
    assert_eq!(
        range(&h).0,
        TimeRange {
            start: 0,
            end: 3 * bar
        }
    );
    assert_eq!(h.history_len(), before + 1);
    assert_eq!(h.app.arrange.asset, Some(music), "it is selected");
    h.run(Action::Undo);
    assert_eq!(range(&h).0.end, 4 * bar);

    // Dragging from the middle moves it, snapped to the grid.
    let loop_rect = h.rect_of(asset_lane::region_id(music));
    let from = pos2(tick_x(&h, 2 * bar), y);
    h.drag(from, pos2(tick_x(&h, 3 * bar) + 8.0, y));
    assert_eq!(
        range(&h).0,
        TimeRange {
            start: bar,
            end: 5 * bar
        }
    );
    assert!(h.rect_of(asset_lane::region_id(music)).left() > loop_rect.left());

    // Now it can have an intro, from its menu; dragging the intro's start
    // onto the body takes it away again.
    h.right_click(pos2(tick_x(&h, 2 * bar), y));
    h.click_label("Add a bar of intro");
    assert_eq!(range(&h).1, Some(0));
    h.drag(pos2(tick_x(&h, 0) + 2.0, y), pos2(tick_x(&h, bar) + 3.0, y));
    assert_eq!(
        range(&h),
        (
            TimeRange {
                start: bar,
                end: 5 * bar
            },
            None
        )
    );

    // The edge between two hits moves both; the others stay.
    h.app.arrange.snap = arrange::Snap::Sixteenth;
    let hy = h.rect_of(asset_lane::region_id(hits)).center().y;
    h.drag(
        pos2(tick_x(&h, beat), hy),
        pos2(tick_x(&h, beat + beat / 2), hy),
    );
    let variations = |h: &Harness| match asset(h, hits).kind {
        GameAssetKind::Sfx { variations, .. } => variations,
        GameAssetKind::Loop { .. } => unreachable!(),
    };
    let v = variations(&h);
    assert_eq!(
        v[0],
        TimeRange {
            start: 0,
            end: beat + beat / 2
        }
    );
    assert_eq!(
        v[1],
        TimeRange {
            start: beat + beat / 2,
            end: 2 * beat
        }
    );
    assert_eq!(
        v[2],
        TimeRange {
            start: 2 * beat,
            end: 3 * beat
        }
    );

    // Escape drops a drag; nothing changes.
    let before = h.history_len();
    let start = pos2(tick_x(&h, 3 * bar), y);
    h.move_to(start);
    h.pointer(start, true);
    h.frame();
    for step in 1..=5 {
        h.move_to(start + vec2(step as f32 * 20.0, 0.0));
    }
    assert!(h.app.arrange.asset_drag.is_some());
    h.key(Key::Escape, Modifiers::NONE);
    h.pointer(start + vec2(100.0, 0.0), false);
    h.frames(2);
    assert!(h.app.arrange.asset_drag.is_none());
    assert_eq!(h.history_len(), before);
    assert_eq!(range(&h).0.start, bar);

    // The corner opens the Game assets window; double-clicking an SFX set
    // does too.
    h.click_label("Game assets…");
    assert!(h.app.dialogs.game.is_some());
    h.app.dialogs.game = None;
    h.frames(2);
    // Long enough after the last click that egui does not count a triple.
    h.frames(40);
    h.double_click(pos2(tick_x(&h, 2 * beat) + 10.0, hy));
    assert!(h.app.dialogs.game.is_some());
}

#[test]
fn the_loop_inspector_shows_the_seam_and_plays_it() {
    let mut h = Harness::with_engine(demo_project());
    h.accessible();
    let (music, _) = demo_assets(&h);
    let bar = h.project().ticks_per_bar();
    assert!(
        !h.app.workspace.is_front(Tab::Loop),
        "behind the editor at first"
    );
    assert!(
        !h.app.inspector.is_rendering(),
        "nothing renders out of view"
    );
    let history = h.history_len();

    // Double-clicking a loop on the timeline shows it in the inspector,
    // which renders it as it exports and checks its seam.
    let y = h.rect_of(asset_lane::region_id(music)).center().y;
    h.double_click(pos2(tick_x(&h, bar), y));
    assert!(h.app.workspace.is_front(Tab::Loop));
    assert_eq!(h.app.inspected(), Target::Asset(music));
    h.frames(2);
    h.wait_jobs();
    let checked = h.app.inspector.checked.clone().expect("rendered");
    assert_eq!(checked.target, Target::Asset(music));
    assert_eq!(checked.revision, h.app.view.revision);
    let rate = f64::from(checked.audio.sample_rate);
    let seconds = checked.audio.frames() as f64 / rate;
    assert!(
        (seconds - 8.0).abs() < 0.1,
        "four bars at 120 BPM: {seconds}"
    );
    assert!(
        h.find("Seamless").is_some() || h.find("A click at the seam").is_some(),
        "the verdict shows"
    );
    assert!(h.find("End of the loop").is_none(), "painted, not a widget");

    // Play loops it; Join starts just before the seam.
    h.click_in(Tab::Loop, "Play");
    h.heard();
    let play = PreviewKey::Inspected { join: false };
    assert_eq!(h.app.preview_state(&play), ButtonState::Playing);
    assert!(h.heard() > 0.001);
    h.click_in(Tab::Loop, "Join");
    h.heard();
    assert_eq!(
        h.app.preview_state(&PreviewKey::Inspected { join: true }),
        ButtonState::Playing
    );
    assert_eq!(h.history_len(), history, "inspecting changes nothing");

    // Shortening the loop on the timeline renders it again.
    let right = h.rect_of(asset_lane::region_id(music)).right();
    h.drag(pos2(right - 2.0, y), pos2(tick_x(&h, 2 * bar), y));
    h.frames(2);
    h.wait_jobs();
    let checked = h.app.inspector.checked.clone().expect("rendered");
    assert_eq!(checked.revision, h.app.view.revision);
    let seconds = checked.audio.frames() as f64 / rate;
    assert!((seconds - 4.0).abs() < 0.1, "{seconds}");

    // The loop region can be inspected too; it has no intro.
    h.app.inspector.target = Some(Target::Region);
    h.frames(2);
    h.wait_jobs();
    let checked = h.app.inspector.checked.clone().expect("rendered");
    assert_eq!(checked.target, Target::Region);
    assert_eq!(checked.loop_start, 0);

    // Putting another tab in front stops what the inspector played.
    h.click_in(Tab::Loop, "Play");
    h.heard();
    assert!(h.app.session.previewing().is_some());
    h.app.workspace.show(Tab::Editor);
    h.frames(2);
    h.heard();
    h.heard();
    assert_eq!(h.app.session.previewing(), None);

    // The Game assets window sends a loop to the inspector.
    h.run(Action::GameAssets);
    h.frames(2);
    h.click_label("Inspect");
    h.frames(2);
    assert!(h.app.workspace.is_front(Tab::Loop));
    assert_eq!(h.app.inspected(), Target::Asset(music));
}

/// The demo with an audio track holding one clip of a four-second, 48 kHz
/// file, from bar 2 for two bars. The file is not on disk, which only
/// leaves the clip without a waveform.
fn with_audio_clip() -> (Project, ClipId) {
    let mut p = demo_project();
    let entry = PoolEntry {
        id: AudioId::new(),
        name: "loop".into(),
        file: "audio/loop.wav".into(),
        channels: 1,
        sample_rate: 48_000,
        frames: 192_000,
        source: None,
    };
    let bar = p.ticks_per_bar();
    let mut track = Track::new_audio("Audio");
    let clip = Clip {
        id: ClipId::new(),
        name: "loop".into(),
        start: bar,
        length: bar * 2,
        muted: false,
        content: ClipContent::Audio {
            source: entry.id,
            offset: 0,
            gain_db: 0.0,
            fade_in: 0,
            fade_out: 0,
        },
    };
    let id = clip.id;
    track.clips.push(clip);
    p.audio_pool.push(entry);
    p.tracks.push(track);
    (p, id)
}

/// An audio clip's offset into its file, gain and fades.
fn audio_of(clip: &Clip) -> (i64, f32, i64, i64) {
    match clip.content {
        ClipContent::Audio {
            offset,
            gain_db,
            fade_in,
            fade_out,
            ..
        } => (offset, gain_db, fade_in, fade_out),
        _ => panic!("an audio clip"),
    }
}

fn notes_of(clip: &Clip) -> Vec<Note> {
    match &clip.content {
        ClipContent::Midi { notes } => notes.clone(),
        _ => panic!("a MIDI clip"),
    }
}

/// A point on a clip's left edge, where it trims.
fn left_edge(h: &Harness, clip: ClipId) -> Pos2 {
    let r = arrange::clip_screen_rect(&h.app, clip).expect("clip visible");
    pos2(r.left() + 2.0, r.center().y)
}

#[test]
fn trimming_a_clip_start_keeps_its_audio_and_notes_in_place() {
    let (p, audio) = with_audio_clip();
    let mut h = Harness::with_project(p);
    h.show_bars(8);
    let bar = h.project().ticks_per_bar();
    let beat_px = h.app.arrange.px_per_tick * PPQ as f32;

    // A beat later, the clip starts half a second into its file at 120 BPM
    // and still ends where it did.
    let from = left_edge(&h, audio);
    h.drag(from, from + vec2(beat_px, 0.0));
    let c = h.clip(audio);
    assert_eq!((c.start, c.length), (bar + PPQ, bar * 2 - PPQ));
    assert_eq!(audio_of(&c).0, 24_000);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Trim clip"));

    // Back past where the file begins, it stops there.
    let from = left_edge(&h, audio);
    h.drag(from, from - vec2(beat_px * 3.0, 0.0));
    let c = h.clip(audio);
    assert_eq!((c.start, c.length), (bar, bar * 2));
    assert_eq!(audio_of(&c).0, 0);

    // A MIDI clip trimmed by a bar drops the notes before the cut, and the
    // rest play when they did.
    let bass = h.track_named("Bass").clips[0].clone();
    let sounding = |start: i64, notes: &[Note]| -> Vec<(i64, u8)> {
        notes.iter().map(|n| (start + n.start, n.pitch)).collect()
    };
    let kept: Vec<(i64, u8)> = sounding(bass.start, &notes_of(&bass))
        .into_iter()
        .filter(|(t, _)| *t >= bar)
        .collect();
    let from = left_edge(&h, bass.id);
    h.drag(from, from + vec2(beat_px * 4.0, 0.0));
    let c = h.clip(bass.id);
    assert_eq!((c.start, c.length), (bar, bar * 3));
    assert_eq!(sounding(c.start, &notes_of(&c)), kept);

    // One undo puts it all back.
    h.key(Key::Z, COMMAND);
    assert_eq!(h.clip(bass.id), bass);
}

#[test]
fn slipping_moves_what_a_clip_plays_and_not_the_clip() {
    let (p, audio) = with_audio_clip();
    let mut h = Harness::with_project(p);
    h.show_bars(8);
    let beat_px = h.app.arrange.px_per_tick * PPQ as f32;
    let before = h.clip(audio);

    // The Slip tool slides the audio a beat earlier: half a second further
    // into the file, with the clip where it was.
    h.key(Key::Num4, Modifiers::NONE);
    assert_eq!(h.app.arrange.tool, Tool::Slip);
    let r = arrange::clip_screen_rect(&h.app, audio).expect("clip visible");
    h.drag(r.center(), r.center() - vec2(beat_px, 0.0));
    let c = h.clip(audio);
    assert_eq!((c.start, c.length), (before.start, before.length));
    assert_eq!(audio_of(&c).0, 24_000);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Slip clip"));

    // Later than the file's start, it stops there.
    h.drag(r.center(), r.center() + vec2(beat_px * 3.0, 0.0));
    assert_eq!(audio_of(&h.clip(audio)).0, 0);

    // A pattern clip has nothing to slip, and says so.
    let beat = h.track_named("Drums").clips[0].clone();
    let r = arrange::clip_screen_rect(&h.app, beat.id).expect("clip visible");
    h.drag(r.center(), r.center() + vec2(beat_px, 0.0));
    assert_eq!(h.clip(beat.id), beat);
    assert!(
        h.app
            .notice
            .as_ref()
            .is_some_and(|n| !n.error && n.text.contains("nothing to slip"))
    );

    // With the Select tool, Ctrl+Alt slips: notes a beat later, then two
    // earlier, which stops with the first note at the clip's start.
    h.key(Key::Num1, Modifiers::NONE);
    let bass = h.track_named("Bass").clips[0].clone();
    let r = arrange::clip_screen_rect(&h.app, bass.id).expect("clip visible");
    let from = pos2(r.left() + 60.0, r.center().y);
    h.set_modifiers(Modifiers {
        alt: true,
        ..COMMAND
    });
    h.drag(from, from + vec2(beat_px, 0.0));
    let c = h.clip(bass.id);
    assert_eq!((c.start, c.length), (bass.start, bass.length));
    let moved: Vec<i64> = notes_of(&c).iter().map(|n| n.start).collect();
    let orig: Vec<i64> = notes_of(&bass).iter().map(|n| n.start + PPQ).collect();
    assert_eq!(moved, orig);
    h.drag(from, from - vec2(beat_px * 2.0, 0.0));
    h.set_modifiers(Modifiers::NONE);
    h.frame();
    assert_eq!(h.clip(bass.id), bass);
}

#[test]
fn fade_and_gain_handles_edit_audio_clips() {
    let (p, audio) = with_audio_clip();
    let mut h = Harness::with_project(p);
    h.show_bars(8);
    let bar = h.project().ticks_per_bar();
    let handle = |h: &Harness, kind: EditKind| {
        h.rect_of(Id::new(("clip_handle", audio, kind as u8)))
            .center()
    };

    // The handles show on a selected clip.
    let r = arrange::clip_screen_rect(&h.app, audio).expect("clip visible");
    h.click(pos2(r.left() + r.width() * 0.3, r.bottom() - 6.0));
    assert_eq!(h.app.sel.clips, [audio]);

    // Up 20 pixels is 5 dB, as one undo step; with Shift, 1 dB.
    let history = h.history_len();
    let from = handle(&h, EditKind::Gain);
    h.drag(from, from - vec2(0.0, 20.0));
    assert_eq!(audio_of(&h.clip(audio)).1, 5.0);
    assert_eq!(h.history_len(), history + 1);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Change clip gain"));
    h.set_modifiers(Modifiers::SHIFT);
    h.drag(from, from - vec2(0.0, 20.0));
    h.set_modifiers(Modifiers::NONE);
    assert_eq!(audio_of(&h.clip(audio)).1, 6.0);

    // Escape during the drag leaves it as it was.
    h.move_to(from);
    h.pointer(from, true);
    h.frame();
    for step in 1..=5 {
        h.move_to(from - vec2(0.0, 8.0 * step as f32));
    }
    h.key(Key::Escape, Modifiers::NONE);
    h.pointer(from - vec2(0.0, 40.0), false);
    h.frames(2);
    assert_eq!(audio_of(&h.clip(audio)).1, 6.0);

    // A double-click puts it back to 0 dB.
    h.frames(40);
    h.double_click(handle(&h, EditKind::Gain));
    assert_eq!(audio_of(&h.clip(audio)).1, 0.0);

    // The fades follow the pointer, and the fade in stops at the fade out.
    let y = handle(&h, EditKind::FadeIn).y;
    h.drag(handle(&h, EditKind::FadeIn), pos2(tick_x(&h, bar + PPQ), y));
    assert_eq!(audio_of(&h.clip(audio)).2, PPQ);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Change fade in"));
    h.drag(
        handle(&h, EditKind::FadeOut),
        pos2(tick_x(&h, bar * 3 - PPQ * 2), y),
    );
    assert_eq!(audio_of(&h.clip(audio)).3, PPQ * 2);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Change fade out"));
    h.drag(handle(&h, EditKind::FadeIn), pos2(tick_x(&h, bar * 3), y));
    assert_eq!(audio_of(&h.clip(audio)).2, bar * 2 - PPQ * 2);
    assert_eq!(h.clip(audio).start, bar, "the clip did not move");
}

#[test]
fn the_split_tool_and_ctrl_e_cut_clips_in_two() {
    let mut h = Harness::with_engine(demo_project());
    h.show_bars(8);
    h.accessible();
    let bar = h.project().ticks_per_bar();
    let bass = h.track_named("Bass");
    let notes = notes_of(&bass.clips[0]);

    // A click with the Split tool cuts on the grid line under it; a note
    // across the cut would be cut too, and these play as before.
    h.click_in(Tab::Arrange, "Split");
    assert_eq!(h.app.arrange.tool, Tool::Split);
    h.click(pos2(
        tick_x(&h, bar * 2) + 3.0,
        lane_rect(&h, bass.id).center().y,
    ));
    let mut halves = h.track_named("Bass").clips;
    halves.sort_by_key(|c| c.start);
    let spans: Vec<(i64, i64)> = halves.iter().map(|c| (c.start, c.length)).collect();
    assert_eq!(spans, [(0, bar * 2), (bar * 2, bar * 2)]);
    let mut heard: Vec<(i64, u8)> = halves
        .iter()
        .flat_map(|c| {
            notes_of(c)
                .into_iter()
                .map(|n| (c.start + n.start, n.pitch))
        })
        .collect();
    heard.sort_unstable();
    let mut was: Vec<(i64, u8)> = notes.iter().map(|n| (n.start, n.pitch)).collect();
    was.sort_unstable();
    assert_eq!(heard, was);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Split clip"));

    // Ctrl+E cuts the selected clips under the playhead, and the new
    // halves are selected with them.
    h.key(Key::Num1, Modifiers::NONE);
    h.key(Key::A, COMMAND);
    assert_eq!(h.app.sel.clips.len(), 4);
    arrange::seek(&mut h.app, bar * 3);
    h.heard();
    h.frames(2);
    h.key(Key::E, COMMAND);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Split 3 clips"));
    let count = |h: &Harness| -> usize { h.project().tracks.iter().map(|t| t.clips.len()).sum() };
    assert_eq!(count(&h), 7);
    assert_eq!(h.app.sel.clips.len(), 7);

    // With nothing there to cut, it says how to use it.
    h.key(Key::Escape, Modifiers::NONE);
    arrange::seek(&mut h.app, bar * 6);
    h.heard();
    h.frames(2);
    h.key(Key::E, COMMAND);
    assert_eq!(count(&h), 7);
    assert!(
        h.app
            .notice
            .as_ref()
            .is_some_and(|n| n.text.starts_with("Put the playhead"))
    );
}

#[test]
fn the_mute_and_erase_tools_act_on_clicks_and_sweeps() {
    let mut h = Harness::demo();
    h.show_bars(8);
    let bar = h.project().ticks_per_bar();
    let drums = h.track_named("Drums").clips[0].id;
    let chords = h.track_named("Pad").clips[0].id;
    let at = |h: &Harness, clip: ClipId| {
        let r = arrange::clip_screen_rect(&h.app, clip).expect("clip visible");
        pos2(tick_x(h, bar), r.center().y)
    };
    let muted = |h: &Harness| -> Vec<bool> {
        h.project()
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .map(|c| c.muted)
            .collect()
    };

    // A click mutes a clip, and another unmutes it.
    h.key(Key::Num5, Modifiers::NONE);
    assert_eq!(h.app.arrange.tool, Tool::Mute);
    h.click(at(&h, chords));
    assert!(h.clip(chords).muted);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Mute clip"));
    h.frames(40);
    h.click(at(&h, chords));
    assert!(!h.clip(chords).muted);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Unmute clip"));

    // A drag down across all three mutes them in one step.
    let history = h.history_len();
    h.drag(at(&h, drums), at(&h, chords));
    assert_eq!(muted(&h), [true, true, true]);
    assert_eq!(h.history_len(), history + 1);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Mute 3 clips"));

    // Shift+M unmutes the selection when every clip in it is muted.
    h.key(Key::Num1, Modifiers::NONE);
    h.key(Key::A, COMMAND);
    h.key(Key::M, Modifiers::SHIFT);
    assert_eq!(muted(&h), [false, false, false]);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Unmute 3 clips"));
    h.key(Key::M, Modifiers::SHIFT);
    assert_eq!(muted(&h), [true, true, true]);

    // The Erase tool: Escape during a sweep keeps everything; a sweep
    // deletes what it crossed, and a click deletes one clip.
    h.key(Key::Num6, Modifiers::NONE);
    let from = at(&h, drums);
    h.move_to(from);
    h.pointer(from, true);
    h.frame();
    for step in 1..=5 {
        h.move_to(from.lerp(at(&h, chords), step as f32 / 5.0));
    }
    assert!(h.app.arrange.sweep.is_some());
    h.key(Key::Escape, Modifiers::NONE);
    h.pointer(at(&h, chords), false);
    h.frames(2);
    assert_eq!(muted(&h).len(), 3);
    h.drag(at(&h, drums), at(&h, chords));
    assert!(h.project().tracks.iter().all(|t| t.clips.is_empty()));
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Delete 3 clips"));
    h.key(Key::Z, COMMAND);
    h.frames(40);
    h.click(at(&h, chords));
    assert!(h.track_named("Pad").clips.is_empty());
    assert_eq!(muted(&h).len(), 2);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Delete clip"));
}

#[test]
fn the_draw_tool_paints_clips_and_number_keys_pick_tools() {
    let (p, _) = with_audio_clip();
    let mut h = Harness::with_project(p);
    h.show_bars(8);
    h.accessible();
    let bar = h.project().ticks_per_bar();
    let bass = h.track_named("Bass").id;
    let pad = h.track_named("Pad").id;
    let audio = h.track_named("Audio").id;

    for tool in Tool::ALL {
        h.key(tool.key(), Modifiers::NONE);
        assert_eq!(h.app.arrange.tool, tool);
    }
    h.click_in(Tab::Arrange, "Draw");
    assert_eq!(h.app.arrange.tool, Tool::Draw);

    // A drag paints from the grid line before it to the one nearest its
    // end, then opens the new clip in the editor.
    let y = lane_rect(&h, bass).center().y;
    h.drag(
        pos2(tick_x(&h, bar * 5) + 3.0, y),
        pos2(tick_x(&h, bar * 6 + PPQ * 2) - 3.0, y),
    );
    let new = h
        .track_named("Bass")
        .clips
        .into_iter()
        .find(|c| c.start == bar * 5)
        .expect("a painted clip");
    assert_eq!(new.length, bar + PPQ * 2);
    assert!(matches!(new.content, ClipContent::Midi { .. }));
    assert_eq!(h.app.sel.clip, Some(new.id));
    assert!(h.app.workspace.is_front(Tab::Editor));

    // A click paints a bar.
    h.frames(40);
    h.click(pos2(
        tick_x(&h, bar * 5 + PPQ) + 10.0,
        lane_rect(&h, pad).center().y,
    ));
    let spans: Vec<(i64, i64)> = h
        .track_named("Pad")
        .clips
        .iter()
        .map(|c| (c.start, c.length))
        .collect();
    assert_eq!(spans, [(0, bar * 4), (bar * 5 + PPQ, bar)]);

    // Audio tracks take no painted clips.
    h.frames(40);
    h.click(pos2(tick_x(&h, bar * 6), lane_rect(&h, audio).center().y));
    assert_eq!(h.track_named("Audio").clips.len(), 1);
    assert!(
        h.app
            .notice
            .as_ref()
            .is_some_and(|n| n.text.contains("instrument tracks"))
    );
}

#[test]
fn markers_are_added_moved_renamed_and_deleted() {
    let mut h = Harness::with_engine(demo_project());
    h.show_bars(8);
    h.accessible();
    let bar = h.project().ticks_per_bar();
    let lane = h.rect_of(Id::new("marker_lane"));
    let flag = |h: &Harness, id: MarkerId| h.rect_of(Id::new(("marker", id))).center();

    // A double-click on the lane adds one on the grid.
    h.double_click(pos2(tick_x(&h, bar * 2) + 4.0, lane.center().y));
    let p = h.project();
    assert_eq!(p.markers.len(), 1);
    assert_eq!(
        (p.markers[0].tick, p.markers[0].name.as_str()),
        (bar * 2, "Marker 1")
    );
    let first = p.markers[0].id;
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Add marker"));

    // M adds one at the playhead, or picks the one already there.
    arrange::seek(&mut h.app, bar * 3);
    h.heard();
    h.frames(2);
    h.key(Key::M, Modifiers::NONE);
    let second = h
        .project()
        .markers
        .iter()
        .find(|m| m.tick == bar * 3)
        .map(|m| m.id)
        .expect("a marker at the playhead");
    h.key(Key::M, Modifiers::NONE);
    assert_eq!(h.project().markers.len(), 2);
    assert_eq!(h.app.arrange.ruler.marker, Some(second));

    // A click on one goes there.
    arrange::seek(&mut h.app, bar * 6);
    h.heard();
    h.click(flag(&h, first));
    h.heard();
    h.frames(2);
    assert_eq!(h.app.transport.position_ticks.round() as i64, bar * 2);

    // Dragged a bar later, as one step.
    let history = h.history_len();
    let from = flag(&h, second);
    h.drag(
        from,
        from + vec2(h.app.arrange.px_per_tick * bar as f32, 0.0),
    );
    let tick_of = |h: &Harness, id: MarkerId| {
        h.project()
            .markers
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.tick)
    };
    assert_eq!(tick_of(&h, second), Some(bar * 4));
    assert_eq!(h.history_len(), history + 1);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Move marker"));

    // A double-click renames it; Enter keeps the name.
    h.frames(40);
    h.double_click(flag(&h, first));
    assert!(h.app.arrange.ruler.renaming.is_some());
    h.key(Key::A, COMMAND);
    h.type_text("Drop");
    h.key(Key::Enter, Modifiers::NONE);
    h.frames(2);
    assert!(h.app.arrange.ruler.renaming.is_none());
    let name = |h: &Harness, id: MarkerId| {
        h.project()
            .markers
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.name.clone())
    };
    assert_eq!(name(&h, first).as_deref(), Some("Drop"));
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Rename marker"));

    // Its menu loops to the next marker and deletes it.
    h.right_click(flag(&h, first));
    h.click_label("Loop to the next marker");
    let r = h.project().loop_region;
    assert_eq!((r.start, r.end), (bar * 2, bar * 4));
    h.frames(40);
    h.right_click(flag(&h, second));
    h.click_label("Delete");
    assert_eq!(tick_of(&h, second), None);
    assert_eq!(h.project().markers.len(), 1);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Delete marker"));
}

#[test]
fn the_tempo_lane_adds_moves_and_deletes_tempo_changes() {
    let mut h = Harness::demo();
    h.show_bars(8);
    h.accessible();
    let bar = h.project().ticks_per_bar();
    let lane = h.rect_of(Id::new("tempo_lane"));
    let point = |h: &Harness, i: usize| h.rect_of(Id::new(("tempo_point", i))).center();
    let points = |h: &Harness| -> Vec<(i64, f64)> {
        h.project()
            .tempo
            .points()
            .iter()
            .map(|p| (p.tick, p.bpm))
            .collect()
    };

    // The lane shows 112 to 128 BPM around a steady 120; a double-click at
    // its top sets 128 from there on.
    let top = lane.top() + 6.0;
    h.double_click(pos2(tick_x(&h, bar * 2) + 4.0, top));
    assert_eq!(points(&h), [(0, 120.0), (bar * 2, 128.0)]);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Change tempo"));

    // Down 11 pixels is 12 BPM now that the lane shows 112 to 136; along a
    // bar moves it a bar.
    let from = point(&h, 1);
    h.drag(from, from + vec2(0.0, 11.0));
    assert_eq!(points(&h), [(0, 120.0), (bar * 2, 116.0)]);
    let from = point(&h, 1);
    let bar_px = h.app.arrange.px_per_tick * bar as f32;
    h.drag(from, from + vec2(bar_px, 0.0));
    assert_eq!(points(&h), [(0, 120.0), (bar * 3, 116.0)]);

    // The first point stays at the start.
    let history = h.history_len();
    let from = point(&h, 0);
    h.drag(from, from + vec2(bar_px, 0.0));
    assert_eq!(points(&h)[0].0, 0);
    assert_eq!(h.history_len(), history);

    // The transport's tempo changes the start and keeps later changes.
    let r = actions::set_tempo(&h.app.session, 100.0, None);
    h.app.report(r);
    h.frames(2);
    assert_eq!(points(&h), [(0, 100.0), (bar * 3, 116.0)]);

    // A point's menu deletes it; the lane's menu flattens the map.
    h.right_click(point(&h, 1));
    h.click_label("Delete");
    assert_eq!(points(&h), [(0, 100.0)]);
    h.key(Key::Z, COMMAND);
    assert_eq!(points(&h).len(), 2);
    h.frames(40);
    h.right_click(pos2(tick_x(&h, bar * 6), lane.center().y));
    h.click_label("Keep the starting tempo throughout");
    assert_eq!(points(&h), [(0, 100.0)]);
}

#[test]
fn the_ruler_lanes_hide_and_come_back_next_run() {
    let mut h = Harness::with_engine(demo_project());
    h.frames(2);
    assert!(h.ctx.read_response(Id::new("marker_lane")).is_some());
    assert!(h.ctx.read_response(Id::new("tempo_lane")).is_some());

    // View > Marker lane hides it.
    let lane_item = |h: &Harness, lane: Lane| -> Option<bool> {
        let menus = menu::menus(&h.app);
        menu_entries(&menus, "View").iter().find_map(|e| match e {
            menu::Entry::Item(it) if it.label == lane.label() => it.checked,
            _ => None,
        })
    };
    assert_eq!(lane_item(&h, Lane::Markers), Some(true));
    h.run(Action::ToggleLane(Lane::Markers));
    assert_eq!(lane_item(&h, Lane::Markers), Some(false));
    assert!(h.ctx.read_response(Id::new("marker_lane")).is_none());
    assert!(h.ctx.read_response(Id::new("tempo_lane")).is_some());

    // The next run starts without it.
    let mut storage = MemoryStorage::default();
    eframe::App::save(&mut h.app, &mut storage);
    let mut next = Harness::with_engine(demo_project());
    next.app.restore(&storage);
    next.frames(2);
    assert!(!next.app.workspace.lanes.markers);
    assert!(next.app.workspace.lanes.tempo);
    assert!(next.ctx.read_response(Id::new("marker_lane")).is_none());

    // Adding a marker shows it again.
    next.key(Key::M, Modifiers::NONE);
    assert!(next.app.workspace.lanes.markers);
    assert_eq!(next.project().markers.len(), 1);
    assert!(next.ctx.read_response(Id::new("marker_lane")).is_some());
}

/// Opens the piano roll's drop-down showing `shown` and picks `item`, once
/// the list has scrolled to what is chosen.
fn pick(h: &mut Harness, shown: &str, item: &str) {
    h.frames(40);
    h.click_in(Tab::Editor, shown);
    h.frames(30);
    // Exactly this label: "Phrygian" is not "Phrygian dominant".
    let rect = h
        .access
        .iter()
        .rev()
        .find(|(l, _, _)| l == item)
        .map(|(_, r, _)| *r)
        .unwrap_or_else(|| panic!("no {item:?} in the list"));
    h.click(rect.center());
    h.frames(2);
}

/// The demo's Pad chords open in the piano roll.
fn pad_in_piano_roll() -> (Harness, ClipId) {
    let mut h = Harness::demo();
    let pad = h.track_named("Pad");
    let clip = pad.clips[0].id;
    h.app.sel.select_clip(pad.id, clip);
    h.app.workspace.show(Tab::Editor);
    h.frames(3);
    (h, clip)
}

/// The Pad's velocities, one per chord, from the velocities of its notes.
fn chord_velocities(h: &Harness, clip: ClipId) -> Vec<(i64, u8)> {
    let mut v: Vec<(i64, u8)> = notes_of(&h.clip(clip))
        .iter()
        .map(|n| (n.start, n.velocity))
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

#[test]
fn velocities_are_drawn_in_the_lane_under_the_piano_roll() {
    let (mut h, clip) = pad_in_piano_roll();
    let lane = h.rect_of(Id::new(("velocity_lane", clip)));
    let bar = h.project().ticks_per_bar();
    let x = |h: &Harness, tick: i64| {
        lane.left()
            + ((tick as f64 - h.app.piano.scroll) * f64::from(h.app.piano.px_per_tick)) as f32
    };
    let y = |v: u8| velocity_lane::y_of(lane, v);
    let chords = [0, bar, bar * 2, bar * 3];
    assert_eq!(
        chord_velocities(&h, clip),
        chords.map(|t| (t, 80)),
        "one velocity per chord"
    );
    let history = h.history_len();

    // Dragging level across the lane sets every stem, as one undo step.
    h.drag(
        pos2(x(&h, 0) + 1.0, y(120)),
        pos2(lane.right() - 2.0, y(120)),
    );
    assert_eq!(chord_velocities(&h, clip), chords.map(|t| (t, 120)));
    assert_eq!(h.history_len(), history + 1);
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Change velocity"));
    assert_eq!(h.app.edit_focus(), Focus::Editor);
    h.key(Key::Z, COMMAND);
    assert_eq!(chord_velocities(&h, clip), chords.map(|t| (t, 80)));

    // With notes selected, only they change.
    let first: Vec<_> = notes_of(&h.clip(clip))
        .iter()
        .filter(|n| n.start == 0)
        .map(|n| n.id)
        .collect();
    h.app.sel.notes = first.clone();
    h.drag(pos2(x(&h, 0) + 1.0, y(30)), pos2(lane.right() - 2.0, y(30)));
    assert_eq!(
        chord_velocities(&h, clip),
        [(0, 30), (bar, 80), (bar * 2, 80), (bar * 3, 80)]
    );
    h.key(Key::Z, COMMAND);

    // Escape drops a drag without changing anything, and keeps the
    // selection.
    let history = h.history_len();
    let from = pos2(x(&h, 0) + 1.0, y(10));
    h.move_to(from);
    h.pointer(from, true);
    h.frame();
    for i in 1..=5 {
        h.move_to(from + vec2(i as f32 * 20.0, 0.0));
    }
    h.key(Key::Escape, Modifiers::NONE);
    h.move_to(from + vec2(150.0, 0.0));
    h.pointer(from + vec2(150.0, 0.0), false);
    h.frames(2);
    assert_eq!(chord_velocities(&h, clip), chords.map(|t| (t, 80)));
    assert_eq!(h.history_len(), history);
    assert_eq!(h.app.sel.notes.len(), 3);
    h.app.sel.notes.clear();

    // Shift draws a straight ramp, here from the second chord to the last.
    h.set_modifiers(Modifiers::SHIFT);
    h.drag(
        pos2(x(&h, bar) - 1.0, y(20)),
        pos2(x(&h, bar * 3) + 1.0, y(120)),
    );
    h.set_modifiers(Modifiers::NONE);
    let v: Vec<u8> = chord_velocities(&h, clip).iter().map(|(_, v)| *v).collect();
    assert_eq!(v.len(), 4, "{v:?}");
    assert_eq!(v[0], 80, "{v:?}");
    assert!(v[1] < v[2] && v[2] < v[3], "{v:?}");
    assert!(v[1] <= 25 && v[3] >= 115, "{v:?}");

    // A click sets the stems under the pointer.
    h.frames(40);
    h.click(pos2(x(&h, bar * 2), y(64)));
    assert_eq!(chord_velocities(&h, clip)[2], (bar * 2, 64));
    assert_eq!(chord_velocities(&h, clip)[1].1, v[1]);

    // The toolbar hides the lane.
    h.accessible();
    h.click_in(Tab::Editor, "Velocity lane");
    h.frames(2);
    assert!(!h.app.workspace.piano.velocity_lane);
    assert!(
        h.ctx
            .read_response(Id::new(("velocity_lane", clip)))
            .is_none()
    );
}

#[test]
fn piano_roll_rows_zoom_and_its_look_is_kept_next_run() {
    let (mut h, clip) = pad_in_piano_roll();
    h.accessible();
    assert_eq!(h.app.workspace.piano.row_h, 13.0);
    h.click_in(Tab::Editor, "+");
    assert_eq!(h.app.workspace.piano.row_h, 15.0);
    h.frames(40);
    h.click_in(Tab::Editor, "\u{2212}");
    h.frames(40);
    h.click_in(Tab::Editor, "\u{2212}");
    assert_eq!(h.app.workspace.piano.row_h, 11.0);

    // Alt and the wheel make the rows taller around the pointer.
    let grid = h.rect_of(Id::new(("piano_bg", clip)));
    let at = pos2(grid.center().x, grid.top() + 60.0);
    let pitch =
        |h: &Harness| h.app.piano.top_pitch - (at.y - grid.top()) / h.app.workspace.piano.row_h;
    let before = pitch(&h);
    h.move_to(at);
    h.set_modifiers(Modifiers::ALT);
    h.events.push(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, 30.0),
        phase: egui::TouchPhase::Move,
        modifiers: Modifiers::ALT,
    });
    h.frames(20);
    h.set_modifiers(Modifiers::NONE);
    h.frames(2);
    let row_h = h.app.workspace.piano.row_h;
    assert!(row_h > 11.5 && row_h <= 32.0, "{row_h}");
    assert!(
        (pitch(&h) - before).abs() < 0.05,
        "{} vs {before}",
        pitch(&h)
    );

    // Ghost notes can be turned off.
    assert_eq!(h.app.workspace.piano.ghosts, Ghosts::All);
    pick(&mut h, "All tracks", "Off");
    assert_eq!(h.app.workspace.piano.ghosts, Ghosts::Off);

    // The row height, lane and ghosts come back next run.
    h.app.workspace.piano.velocity_lane = false;
    let look = h.app.workspace.piano;
    let mut storage = MemoryStorage::default();
    eframe::App::save(&mut h.app, &mut storage);
    let mut next = Harness::demo();
    next.app.restore(&storage);
    assert_eq!(next.app.workspace.piano, look);

    // A saved height out of range is brought back into it.
    let mut w = Workspace::default();
    w.restore(r#"{"piano":{"rowH":500,"ghosts":"track"}}"#);
    assert_eq!(w.piano.row_h, 32.0);
    assert_eq!(w.piano.ghosts, Ghosts::Track);
    assert!(w.piano.velocity_lane);
}

#[test]
fn the_song_key_is_picked_in_the_piano_roll_and_undoes() {
    let (mut h, _) = pad_in_piano_roll();
    h.accessible();
    let scale = |h: &Harness| h.project().scale;
    assert_eq!(scale(&h), Scale::new(9, ScaleMode::Minor).ok());

    pick(&mut h, "minor", "Phrygian");
    assert_eq!(scale(&h), Scale::new(9, ScaleMode::Phrygian).ok());
    assert_eq!(h.app.view.undo_label.as_deref(), Some("Change scale"));
    pick(&mut h, "A", "D");
    assert_eq!(scale(&h), Scale::new(2, ScaleMode::Phrygian).ok());

    // No scale, then a root alone gives its major scale.
    pick(&mut h, "Phrygian", "No scale");
    assert_eq!(scale(&h), None);
    pick(&mut h, "\u{2013}", "E");
    assert_eq!(scale(&h), Scale::new(4, ScaleMode::Major).ok());

    h.key(Key::Z, COMMAND);
    assert_eq!(scale(&h), None);
    h.key(Key::Z, COMMAND);
    assert_eq!(scale(&h), Scale::new(2, ScaleMode::Phrygian).ok());
}

#[test]
fn notes_quantize_to_the_grid() {
    let (mut h, clip) = pad_in_piano_roll();
    let step = h.app.piano.grid.ticks();
    let written = notes_of(&h.clip(clip));
    // Played a little late, and let go a little early.
    let late: Vec<Note> = written
        .iter()
        .map(|n| Note {
            start: n.start + step / 3,
            length: n.length - step / 5,
            ..*n
        })
        .collect();
    let r = actions::update_notes(&h.app.session, clip, late.clone(), "Play late", None);
    h.app.report(r);
    h.frames(2);
    let spans = |h: &Harness| -> Vec<(i64, i64)> {
        let mut v: Vec<_> = notes_of(&h.clip(clip))
            .iter()
            .map(|n| (n.start, n.start + n.length))
            .collect();
        v.sort_unstable();
        v
    };
    // Each note back on its grid line, as long as it was played.
    let mut starts_on_grid: Vec<(i64, i64)> = late
        .iter()
        .map(|n| (n.start - step / 3, n.start - step / 3 + n.length))
        .collect();
    starts_on_grid.sort_unstable();

    // Q moves every note's start to the grid, and keeps its length.
    h.app.sel.focus = Focus::Editor;
    h.key(Key::Q, Modifiers::NONE);
    assert_eq!(spans(&h), starts_on_grid);
    let label = format!("Quantize {} notes", late.len());
    assert_eq!(h.app.view.undo_label.as_deref(), Some(label.as_str()));
    h.key(Key::Z, COMMAND);

    // Shift+Q moves the ends too, of the selected notes only.
    let first: Vec<_> = late
        .iter()
        .filter(|n| n.start == step / 3)
        .map(|n| n.id)
        .collect();
    assert_eq!(first.len(), 3);
    h.app.sel.notes = first.clone();
    h.key(Key::Q, Modifiers::SHIFT);
    let mut expected: Vec<(i64, i64)> = written
        .iter()
        .zip(&late)
        .map(|(w, l)| {
            let n = if first.contains(&l.id) { w } else { l };
            (n.start, n.start + n.length)
        })
        .collect();
    expected.sort_unstable();
    assert_eq!(spans(&h), expected);
    h.key(Key::Z, COMMAND);
    h.app.sel.notes.clear();

    // The toolbar button does what Q does; then nothing is left to move.
    h.accessible();
    h.click_in(Tab::Editor, "Quantize");
    assert_eq!(spans(&h), starts_on_grid);
    let history = h.history_len();
    h.frames(40);
    h.click_in(Tab::Editor, "Quantize");
    assert_eq!(h.history_len(), history);

    // Shift+Q then puts the ends back where they were written.
    h.key(Key::Q, Modifiers::SHIFT);
    let mut on_grid: Vec<_> = written
        .iter()
        .map(|n| (n.start, n.start + n.length))
        .collect();
    on_grid.sort_unstable();
    assert_eq!(spans(&h), on_grid);
}

/// Adds an effect to a track and opens its window in the mixer.
fn open_effect(h: &mut Harness, track: &str, kind: sky_dsp::EffectType) -> (TrackId, EffectId) {
    let t = h.track_named(track);
    let channel = sky_model::ChannelRef::Track(t.id);
    let id = h
        .app
        .report(actions::add_effect(&h.app.session, channel, kind))
        .expect("effect added");
    h.app.mixer.open_effect((channel, id));
    h.app.workspace.show(Tab::Mixer);
    h.frames(3);
    (t.id, id)
}

fn effect_param(h: &Harness, track: TrackId, effect: EffectId, name: &str) -> f32 {
    h.project()
        .track(track)
        .ok()
        .and_then(|t| t.channel.effect(effect))
        .and_then(|e| e.param(name))
        .expect("parameter")
}

fn instrument_param(h: &Harness, track: TrackId, name: &str) -> f32 {
    h.project()
        .track(track)
        .ok()
        .and_then(|t| t.instrument.as_ref())
        .and_then(|i| i.param(name))
        .expect("parameter")
}

#[test]
fn dragging_an_eq_band_moves_it_in_one_undo_step() {
    let mut h = Harness::demo();
    let (bass, eq) = open_effect(&mut h, "Bass", sky_dsp::EffectType::Eq);
    let handle = h.rect_of(fx_view::handle_id(eq, "mid_freq")).center();
    let history = h.history_len();
    h.drag(handle, handle + vec2(60.0, -40.0));
    let freq = effect_param(&h, bass, eq, "mid_freq");
    let gain = effect_param(&h, bass, eq, "mid_gain");
    assert!(freq > 1_500.0, "{freq}");
    assert!(gain > 5.0, "{gain}");
    assert_eq!(h.history_len(), history + 1, "one drag is one undo step");

    // The wheel over it makes it narrower, steps close together one step.
    let handle = h.rect_of(fx_view::handle_id(eq, "mid_freq")).center();
    h.move_to(handle);
    for _ in 0..3 {
        h.events.push(Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, 40.0),
            phase: egui::TouchPhase::Move,
            modifiers: Modifiers::NONE,
        });
        h.frames(4);
    }
    h.frames(20);
    let q = effect_param(&h, bass, eq, "mid_q");
    assert!(q > 1.0, "{q}");
    assert_eq!(h.history_len(), history + 2);

    // A double-click sets the gain back to flat.
    let handle = h.rect_of(fx_view::handle_id(eq, "mid_freq")).center();
    h.double_click(handle);
    assert_eq!(effect_param(&h, bass, eq, "mid_gain"), 0.0);
    h.run(Action::Undo);
    h.run(Action::Undo);
    h.run(Action::Undo);
    assert_eq!(effect_param(&h, bass, eq, "mid_freq"), 1_000.0);
}

#[test]
fn the_filter_and_compressor_handles_set_their_parameters() {
    let mut h = Harness::demo();
    let (bass, filter) = open_effect(&mut h, "Bass", sky_dsp::EffectType::Filter);
    let handle = h.rect_of(fx_view::handle_id(filter, "cutoff")).center();
    h.drag(handle, handle + vec2(-50.0, -50.0));
    assert!(effect_param(&h, bass, filter, "cutoff") < 700.0);
    assert!(effect_param(&h, bass, filter, "resonance") > 0.5);

    let (drums, comp) = open_effect(&mut h, "Drums", sky_dsp::EffectType::Compressor);
    let handle = h.rect_of(fx_view::handle_id(comp, "threshold")).center();
    h.drag(handle, handle + vec2(-30.0, 0.0));
    let threshold = effect_param(&h, drums, comp, "threshold");
    assert!(threshold < -25.0, "{threshold}");
    // Down at the right edge compresses harder.
    let handle = h.rect_of(fx_view::handle_id(comp, "ratio")).center();
    h.drag(handle, handle + vec2(0.0, 20.0));
    let ratio = effect_param(&h, drums, comp, "ratio");
    assert!(ratio > 5.0, "{ratio}");
}

#[test]
fn compressors_show_how_much_they_turn_down() {
    let mut h = Harness::with_engine(demo_project());
    let (drums, comp) = open_effect(&mut h, "Drums", sky_dsp::EffectType::Compressor);
    let channel = sky_model::ChannelRef::Track(drums);
    for (name, v) in [("threshold", -40.0), ("ratio", 20.0)] {
        h.app
            .report(actions::set_effect_param(
                &h.app.session,
                channel,
                comp,
                name,
                Some(v),
                None,
            ))
            .unwrap();
    }
    assert!(!h.app.mixer.reductions[&(channel, comp)].is_active());
    h.run(Action::PlayStop);
    for _ in 0..10 {
        h.heard();
        h.frame();
    }
    let level = &h.app.mixer.reductions[&(channel, comp)];
    assert!(level.db > 10.0 && level.hold >= level.db, "{}", level.db);
    assert!(level.history.iter().any(|d| *d > 10.0));
    assert!(h.app.mixer.any_active());
    // Effects that never turn the signal down have no meter.
    let (_, eq) = open_effect(&mut h, "Bass", sky_dsp::EffectType::Eq);
    let bass = sky_model::ChannelRef::Track(h.track_named("Bass").id);
    assert!(!h.app.mixer.reductions.contains_key(&(bass, eq)));
    h.run(Action::PlayStop);
}

#[test]
fn the_envelope_graph_drags_attack_and_sustain() {
    let mut h = Harness::demo();
    let bass = h.track_named("Bass");
    h.app.sel.select_track(bass.id);
    h.app.workspace.show(Tab::Instrument);
    h.frames(3);
    let attack = instrument_param(&h, bass.id, "attack");
    let history = h.history_len();
    let handle = h.rect_of(inst_view::handle_id(bass.id, "attack")).center();
    h.drag(handle, handle + vec2(30.0, 0.0));
    assert!(instrument_param(&h, bass.id, "attack") > attack * 2.0);
    assert_eq!(h.history_len(), history + 1, "one drag is one undo step");

    // The decay's handle sets the sustain too, in the same step.
    let sustain = instrument_param(&h, bass.id, "sustain");
    let handle = h.rect_of(inst_view::handle_id(bass.id, "decay")).center();
    h.drag(handle, handle + vec2(20.0, 30.0));
    let now = instrument_param(&h, bass.id, "sustain");
    assert!(now < sustain - 0.1, "{now}");
    assert_eq!(h.history_len(), history + 2);

    // The filter sweep's handles set its depth and cutoff.
    let amount = instrument_param(&h, bass.id, "env_amount");
    let handle = h
        .rect_of(inst_view::handle_id(bass.id, "env_amount"))
        .center();
    h.drag(handle, handle + vec2(0.0, 20.0));
    assert!(instrument_param(&h, bass.id, "env_amount") < amount);
}

#[test]
fn the_sampler_markers_set_where_it_plays() {
    let mut p = demo_project();
    let entry = PoolEntry {
        id: AudioId::new(),
        name: "hit".into(),
        file: "audio/hit.wav".into(),
        channels: 1,
        sample_rate: 48_000,
        frames: 48_000,
        source: None,
    };
    let mut track = Track::new_instrument("Hit", InstrumentType::Sampler);
    if let Some(i) = track.instrument.as_mut() {
        i.sample = Some(entry.id);
    }
    let track_id = track.id;
    p.audio_pool.push(entry.clone());
    p.tracks.push(track);
    let mut h = Harness::with_project(p);
    let mut audio = AudioData::silent(1, 48_000, 48_000);
    if let Some(s) = audio.channels.first_mut().and_then(|c| c.get_mut(100)) {
        *s = 0.9;
    }
    h.app
        .waveforms
        .insert(entry.id, Some(Arc::new(Peaks::compute(&audio))));
    h.app.sel.select_track(track_id);
    h.app.workspace.show(Tab::Instrument);
    h.frames(3);
    let history = h.history_len();
    let start = h.rect_of(inst_view::handle_id(track_id, "start"));
    let end = h.rect_of(inst_view::handle_id(track_id, "end"));
    let width = end.center().x - start.center().x;
    h.drag(start.center(), start.center() + vec2(width * 0.25, 0.0));
    let at = instrument_param(&h, track_id, "start");
    assert!((at - 0.25).abs() < 0.02, "{at}");
    assert_eq!(h.history_len(), history + 1, "one drag is one undo step");
    // The end can't pass the start.
    h.drag(end.center(), start.center() - vec2(40.0, 0.0));
    let end_at = instrument_param(&h, track_id, "end");
    assert!(end_at > at && end_at < at + 0.01, "{end_at}");
}

#[test]
fn drum_pads_play_and_pick_their_sound() {
    let mut h = Harness::with_engine(demo_project());
    let drums = h.track_named("Drums");
    h.app.sel.select_track(drums.id);
    h.app.workspace.show(Tab::Instrument);
    h.accessible();
    assert!(h.find("Kick tune").is_some(), "the kick is picked first");
    assert!(h.find("Snare snap").is_none());
    let snare = h.rect_of(inst_view::pad_id(drums.id, 2));
    assert_eq!(h.find_role("Snare", Some(Role::Button)), Some(snare));
    h.move_to(snare.center());
    h.pointer(snare.center(), true);
    h.frame();
    assert_eq!(h.app.audition.held, Some((drums.id, 38)));
    assert!(h.heard() > 0.01);
    h.pointer(snare.center(), false);
    h.frames(3);
    assert_eq!(h.app.audition.held, None);
    assert_eq!(h.app.devices.pads.get(&drums.id), Some(&2));
    assert!(h.find("Snare snap").is_some(), "its settings show");
    assert!(h.find("Kick tune").is_none());
    // The kit's own settings stay.
    assert!(h.find("Style").is_some());
}

#[test]
fn sliders_leave_values_set_elsewhere_alone() {
    let mut h = Harness::demo();
    let bass = h.track_named("Bass");
    h.app.sel.select_track(bass.id);
    h.app.workspace.show(Tab::Instrument);
    h.frames(3);
    // An AI sets a value finer than its slider shows.
    let r = actions::set_instrument_param(&h.app.session, bass.id, "attack", Some(20.6537), None);
    h.app.report(r).unwrap();
    let history = h.history_len();
    h.frames(5);
    assert_eq!(instrument_param(&h, bass.id, "attack"), 20.6537);
    assert_eq!(
        h.history_len(),
        history,
        "the slider adds no step of its own"
    );
}

/// Opens the preset menu showing `shown` and picks `item` from it.
fn pick_preset(h: &mut Harness, shown: &str, item: &str) {
    h.frames(40);
    h.click_label(shown);
    h.frames(30);
    let rect = h
        .access
        .iter()
        .rev()
        .find(|(l, _, _)| l == item)
        .map(|(_, r, _)| *r)
        .unwrap_or_else(|| panic!("no {item:?} in the presets"));
    h.click(rect.center());
    h.frames(2);
}

fn effect_slot(h: &Harness, track: TrackId, effect: EffectId) -> sky_model::EffectSlot {
    h.project()
        .track(track)
        .ok()
        .and_then(|t| t.channel.effect(effect))
        .cloned()
        .expect("effect")
}

#[test]
fn presets_load_in_one_undo_step_and_save_as_your_own() {
    let mut h = Harness::demo();
    let bass = h.track_named("Bass");
    let before = bass.instrument.clone().unwrap();
    h.app.sel.select_track(bass.id);
    h.app.workspace.show(Tab::Instrument);
    h.accessible();
    let history = h.history_len();
    pick_preset(&mut h, "Custom", "Pluck");
    let pluck = InstrumentType::Synth
        .presets()
        .iter()
        .find(|p| p.name == "Pluck")
        .unwrap();
    for spec in InstrumentType::Synth.params() {
        assert_eq!(
            instrument_param(&h, bass.id, spec.name),
            pluck.value(spec.name).unwrap_or(spec.default),
            "{}",
            spec.name
        );
    }
    assert_eq!(h.history_len(), history + 1, "one undo step");
    assert!(h.find("Pluck").is_some(), "the menu shows it");
    h.run(Action::Undo);
    h.frames(3);
    assert_eq!(h.track_named("Bass").instrument, Some(before.clone()));

    // Saved under a name of your own, then deleted again.
    h.click_label("Save…");
    h.frames(2);
    h.type_text("My bass");
    h.key(Key::Enter, Modifiers::NONE);
    h.frames(3);
    let kind = crate::presets::Kind::Instrument(InstrumentType::Synth);
    let names: Vec<String> = h
        .app
        .presets
        .store
        .of(kind)
        .map(|p| p.name.clone())
        .collect();
    assert_eq!(names, ["My bass"]);
    assert_eq!(
        h.history_len(),
        history,
        "saving changes nothing in the song"
    );
    assert!(h.find("My bass").is_some(), "the menu shows the new name");
    h.click_label("Delete");
    h.frames(3);
    assert_eq!(h.app.presets.store.of(kind).count(), 0);
    assert!(h.find("Custom").is_some());
    assert_eq!(h.track_named("Bass").instrument, Some(before));
}

#[test]
fn a_and_b_keep_two_versions_to_switch_between() {
    let mut h = Harness::demo();
    let (bass, eq) = open_effect(&mut h, "Bass", sky_dsp::EffectType::Eq);
    let channel = sky_model::ChannelRef::Track(bass);
    let r = actions::set_effect_param(&h.app.session, channel, eq, "low_gain", Some(6.0), None);
    h.app.report(r).unwrap();
    h.accessible();
    let history = h.history_len();
    let a: std::collections::BTreeMap<String, f32> = [("low_gain".to_string(), 6.0)].into();

    // B starts as a copy of A.
    h.click_label("B");
    h.frames(2);
    let fx = effect_slot(&h, bass, eq);
    assert_eq!(fx.params, a);
    assert_eq!(
        fx.compare,
        Some(sky_model::AbCompare {
            on_b: true,
            other: a.clone(),
        })
    );
    pick_preset(&mut h, "Custom", "Bright air");
    let bright = effect_slot(&h, bass, eq).params;
    assert_eq!(effect_param(&h, bass, eq, "high_gain"), 4.0);

    // Back to A, then B again.
    h.click_label("A");
    h.frames(2);
    let fx = effect_slot(&h, bass, eq);
    assert_eq!(fx.params, a);
    assert_eq!(
        fx.compare.as_ref().map(|c| (c.on_b, &c.other)),
        Some((false, &bright))
    );
    h.click_label("B");
    h.frames(2);
    assert_eq!(effect_slot(&h, bass, eq).params, bright);
    assert_eq!(h.history_len(), history + 4, "each switch is one undo step");

    // Copying makes both sides the same.
    h.click_label("Copy B to A");
    h.frames(2);
    let fx = effect_slot(&h, bass, eq);
    assert_eq!(fx.compare.as_ref().map(|c| &c.other), Some(&bright));
    h.run(Action::Undo);
    h.run(Action::Undo);
    h.frames(2);
    let fx = effect_slot(&h, bass, eq);
    assert_eq!(fx.params, a, "undo goes back to A");
    assert_eq!(fx.compare.map(|c| (c.on_b, c.other)), Some((false, bright)));
}

#[test]
fn several_effect_windows_stay_open() {
    let mut h = Harness::demo();
    let (bass, eq) = open_effect(&mut h, "Bass", sky_dsp::EffectType::Eq);
    let (drums, comp) = open_effect(&mut h, "Drums", sky_dsp::EffectType::Compressor);
    h.accessible();
    assert!(h.find("Bass · 3-band EQ").is_some());
    assert!(h.find("Drums · Compressor").is_some());
    let eq_key = (sky_model::ChannelRef::Track(bass), eq);
    let comp_key = (sky_model::ChannelRef::Track(drums), comp);
    assert_eq!(h.app.mixer.open_effects, [eq_key, comp_key]);
    // Each has its own handles.
    assert!(h.rect_of(fx_view::handle_id(eq, "mid_freq")).is_positive());
    assert!(
        h.rect_of(fx_view::handle_id(comp, "threshold"))
            .is_positive()
    );

    // The slot in the strip closes its window and opens it again.
    h.click_in(Tab::Mixer, "3-band EQ");
    h.frames(2);
    assert_eq!(h.app.mixer.open_effects, [comp_key]);
    assert!(h.find("Bass · 3-band EQ").is_none());
    h.click_in(Tab::Mixer, "3-band EQ");
    h.frames(2);
    assert_eq!(h.app.mixer.open_effects, [comp_key, eq_key]);
}

#[test]
fn inserts_drag_to_a_new_place() {
    let mut h = Harness::demo();
    let bass = h.track_named("Bass");
    let drums = h.track_named("Drums");
    let channel = sky_model::ChannelRef::Track(bass.id);
    let ids: Vec<EffectId> = [
        sky_dsp::EffectType::Eq,
        sky_dsp::EffectType::Compressor,
        sky_dsp::EffectType::Delay,
    ]
    .into_iter()
    .map(|k| {
        h.app
            .report(actions::add_effect(&h.app.session, channel, k))
            .unwrap()
    })
    .collect();
    h.app.workspace.show(Tab::Mixer);
    h.accessible();
    let order = |h: &Harness, t: TrackId| -> Vec<EffectId> {
        h.project()
            .track(t)
            .unwrap()
            .channel
            .effects
            .iter()
            .map(|e| e.id)
            .collect()
    };
    let slot = |h: &Harness, label: &str| -> Rect {
        let area = h.panel_rect(Tab::Mixer);
        h.access
            .iter()
            .find(|(l, r, _)| l == label && area.contains(r.center()))
            .map(|(_, r, _)| *r)
            .unwrap_or_else(|| panic!("no {label:?} in the mixer"))
    };
    let history = h.history_len();
    let delay = slot(&h, "Delay");
    let eq = slot(&h, "3-band EQ");
    h.drag(delay.center(), eq.center() - vec2(0.0, 8.0));
    assert_eq!(order(&h, bass.id), [ids[2], ids[0], ids[1]]);
    assert_eq!(h.history_len(), history + 1, "one undo step");
    assert!(
        h.app.mixer.open_effects.is_empty(),
        "a drag opens no window"
    );

    // Dropped where it was, nothing changes.
    let eq = slot(&h, "3-band EQ");
    h.drag(eq.center(), eq.center() + vec2(0.0, 6.0));
    assert_eq!(h.history_len(), history + 1);

    // Onto another strip, it moves there.
    let comp = slot(&h, "Compressor");
    let to = h
        .access
        .iter()
        .filter(|(l, _, _)| l == "+ Effect")
        .map(|(_, r, _)| *r)
        .min_by(|a, b| a.left().total_cmp(&b.left()))
        .expect("the drums strip");
    h.drag(comp.center(), to.center() - vec2(0.0, 6.0));
    assert_eq!(order(&h, drums.id), [ids[1]]);
    assert_eq!(order(&h, bass.id), [ids[2], ids[0]]);
    h.run(Action::Undo);
    h.frames(2);
    assert_eq!(order(&h, bass.id), [ids[2], ids[0], ids[1]]);
    assert!(order(&h, drums.id).is_empty());
}

/// A small opaque PNG file.
fn picture_file(path: &Path, rgb: [u8; 3]) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).expect("folder");
    }
    let image = image::RgbImage::from_pixel(40, 24, image::Rgb(rgb));
    image.save(path).expect("write png");
}

fn picture_id(place: &str, target: &crate::pictures::Target) -> Id {
    crate::pictures::button_id(place, target)
}

#[test]
fn a_tracks_picture_changes_from_its_header_and_undoes() {
    use crate::pictures::{Shown, Target};
    use sky_model::{Picture, Symbol};

    let mut h = Harness::demo();
    h.accessible();
    let track = h.project().tracks[0].clone();
    let target = Target::Track(track.id);
    assert_eq!(track.picture, None);
    let shown = h.app.shown_of(&target).expect("shown");
    assert!(
        matches!(shown, Shown::Symbol { symbol, .. } if symbol == crate::pictures::track_symbol(&track)),
        "a track shows what it plays: {shown:?}"
    );

    // Clicking the picture in the header opens the editor for it.
    h.click(h.rect_of(picture_id("header", &target)).center());
    assert_eq!(
        h.app.pictures.editor.as_ref().map(|e| e.target.clone()),
        Some(target.clone())
    );
    h.frames(2);

    // A symbol is one undo step, and follows the track's color.
    let history = h.history_len();
    h.click_label("drum machine");
    let p = h.project();
    assert_eq!(
        p.track(track.id).expect("track").picture,
        Some(Picture::symbol(Symbol::DrumMachine))
    );
    assert_eq!(h.history_len(), history + 1);

    // A swatch sets the track's color.
    h.click_label("#e25c54");
    assert_eq!(
        h.project().track(track.id).expect("track").color,
        [226, 92, 84]
    );
    assert_eq!(h.history_len(), history + 2);

    // Undo takes both back.
    h.key(Key::Z, Modifiers::COMMAND);
    h.key(Key::Z, Modifiers::COMMAND);

    // A change made by a drag shows at once and is one undo step when the
    // drag ends.
    let history = h.history_len();
    let image = sky_model::PictureImage::new(vec![1, 2, 3], "x.png");
    h.pointer(pos2(700.0, 700.0), true);
    h.frame();
    for zoom in [1.5, 2.0, 2.5] {
        let mut zoomed = image.clone();
        zoomed.zoom = zoom;
        h.app
            .set_picture(&target, Some(Picture::Image(zoomed)), true);
        h.frame();
    }
    assert_eq!(
        h.history_len(),
        history,
        "nothing yet while the drag goes on"
    );
    assert!(
        matches!(h.app.shown_of(&target), Some(Shown::Image { image, .. }) if image.zoom == 2.5)
    );
    h.pointer(pos2(700.0, 700.0), false);
    h.frame();
    assert_eq!(h.history_len(), history + 1);
    assert!(matches!(
        &h.project().track(track.id).expect("track").picture,
        Some(Picture::Image(i)) if i.zoom == 2.5
    ));
    h.key(Key::Z, Modifiers::COMMAND);
    let back = h.project().track(track.id).expect("track").clone();
    assert_eq!((back.picture, back.color), (None, track.color));

    // Done closes it, and what was picked is offered again.
    h.click_label("drum machine");
    h.click_label("Done");
    assert!(h.app.pictures.editor.is_none());
    assert_eq!(
        h.app.pictures.store.recent(),
        [Picture::symbol(Symbol::DrumMachine)]
    );

    // The mixer strip shows it too, and opens the same editor.
    h.app.workspace.show(Tab::Mixer);
    h.frames(3);
    h.click(h.rect_of(picture_id("mixer", &target)).center());
    assert!(h.app.pictures.editor.is_some());
    h.frames(2);
    h.click_label("Reset to default");
    assert_eq!(h.project().track(track.id).expect("track").picture, None);
}

#[test]
fn pack_pictures_are_kept_on_this_computer() {
    use crate::browser::{pack_key, pack_target, row_id};
    use crate::pictures::PictureStore;
    use sky_library::PACKS;
    use sky_model::{Picture, Symbol};

    let dir = tempfile::tempdir().expect("temp dir");
    let store = dir.path().join("pictures.json");
    let mut h = Harness::with_config(
        empty_project("Pictures"),
        UiConfig {
            picture_store: Some(store.clone()),
            ..UiConfig::default()
        },
    );
    h.offline_packs();
    h.accessible();
    let (vcsl, piano) = (&PACKS[0], &PACKS[1]);
    let target = pack_target(vcsl);

    // The pack's row shows its picture; clicking it opens the editor
    // rather than the pack.
    h.click(h.rect_of(picture_id("browser", &target)).center());
    assert!(!h.app.browser.is_expanded(&pack_key(vcsl)));
    assert!(h.app.pictures.editor.is_some());
    h.frames(2);
    h.click_label("piano");
    let mine = Picture::Symbol {
        symbol: Symbol::Piano,
        color: Some([200, 150, 60]),
    };
    assert_eq!(h.app.pictures.store.get(&pack_key(vcsl)), Some(&mine));
    // No undo step: it is not part of the project.
    assert_eq!(h.history_len(), 0);
    h.click_label("Done");
    h.frames(2);
    assert_eq!(
        PictureStore::load(Some(store.clone())).get(&pack_key(vcsl)),
        Some(&mine)
    );

    // Copy picture and Paste picture from the rows' menus.
    h.right_click(h.rect_of(row_id(&pack_key(vcsl))).center());
    h.click_label("Copy picture");
    h.right_click(h.rect_of(row_id(&pack_key(piano))).center());
    h.click_label("Paste picture");
    assert_eq!(h.app.pictures.store.get(&pack_key(piano)), Some(&mine));
    h.right_click(h.rect_of(row_id(&pack_key(vcsl))).center());
    h.click_label("Reset picture");
    assert_eq!(h.app.pictures.store.get(&pack_key(vcsl)), None);
    h.frames(2);
    let again = PictureStore::load(Some(store));
    assert_eq!(again.get(&pack_key(vcsl)), None);
    assert_eq!(again.get(&pack_key(piano)), Some(&mine));
}

#[test]
fn dropped_images_become_pictures() {
    use crate::pictures::Target;
    use sky_model::Picture;

    let dir = tempfile::tempdir().expect("temp dir");
    let red = dir.path().join("red.png");
    let blue = dir.path().join("blue.png");
    picture_file(&red, [220, 30, 30]);
    picture_file(&blue, [30, 30, 220]);
    let mut h = Harness::demo();
    let track = h.project().tracks[1].id;
    let target = Target::Track(track);
    let drop = |h: &mut Harness, path: &Path| {
        h.dropped = vec![Arc::new(TestFile(path.to_path_buf())) as egui::DroppedFileHandle];
        h.frame();
        h.wait_jobs();
        h.frames(2);
    };

    // Into the open picture editor.
    h.click(h.rect_of(picture_id("header", &target)).center());
    drop(&mut h, &red);
    let picture = h.project().track(track).expect("track").picture.clone();
    let Some(Picture::Image(image)) = picture else {
        panic!("an image: {picture:?}");
    };
    assert_eq!(image.name, "red.png");
    assert_eq!(&image.data[..3], &[0xff, 0xd8, 0xff], "kept as a JPEG");
    assert!(
        h.app
            .pictures
            .images
            .texture(&h.ctx(), &image.data)
            .is_some()
    );
    h.app.close_picture_editor();

    // Dropped where no picture is known, it waits for a click on one.
    h.move_to(pos2(900.0, 880.0));
    drop(&mut h, &blue);
    assert_eq!(h.app.pictures.waiting.as_deref(), Some(blue.as_path()));
    h.click(h.rect_of(picture_id("header", &target)).center());
    h.wait_jobs();
    h.frames(2);
    let picture = h.project().track(track).expect("track").picture.clone();
    assert!(
        matches!(&picture, Some(Picture::Image(i)) if i.name == "blue.png"),
        "{picture:?}"
    );
    assert!(h.app.pictures.editor.is_none(), "the click only placed it");

    // Esc lets a waiting image go.
    h.move_to(pos2(900.0, 880.0));
    drop(&mut h, &red);
    assert!(h.app.pictures.waiting.is_some());
    h.key(Key::Escape, Modifiers::NONE);
    assert!(h.app.pictures.waiting.is_none());

    // A file that is not an image says so in the editor.
    let fake = dir.path().join("fake.png");
    std::fs::write(&fake, "not an image").expect("fake");
    h.click(h.rect_of(picture_id("header", &target)).center());
    drop(&mut h, &fake);
    let error = h.app.pictures.editor.as_ref().and_then(|e| e.error.clone());
    assert!(error.is_some_and(|e| e.contains("fake.png")));
}

#[test]
fn packs_show_as_tiles_with_a_card() {
    use crate::browser::{pack_key, row_id};
    use sky_library::PACKS;

    let mut h = Harness::with_project(empty_project("Tiles"));
    h.offline_packs();
    h.accessible();
    h.click_label("Tiles view");
    assert!(h.app.browser.tiles);
    h.frames(2);
    let tile = row_id(&format!("tile:{}", PACKS[2].id));
    assert!(h.ctx.read_response(row_id(&pack_key(&PACKS[2]))).is_none());

    // Clicking a tile shows the pack's card at the bottom.
    h.click(h.rect_of(tile).center() + vec2(0.0, 10.0));
    assert_eq!(h.app.browser.card, Some(PACKS[2].id));
    h.frames(2);
    assert!(h.find("Close the pack card").is_some());

    // Show sounds goes to the list with the pack open.
    h.click_label("Show sounds");
    h.frames(2);
    assert!(!h.app.browser.tiles);
    assert!(h.app.browser.is_expanded(&pack_key(&PACKS[2])));
    assert!(h.ctx.read_response(row_id(&pack_key(&PACKS[2]))).is_some());
    h.click_label("Close the pack card");
    assert_eq!(h.app.browser.card, None);
}

#[test]
fn folders_and_packs_bring_their_own_covers() {
    use crate::browser::folder_key;
    use crate::pictures::Shown;

    let dir = tempfile::tempdir().expect("temp dir");
    let samples = dir.path().join("Kicks");
    tone_file(&samples.join("kick.wav"), 0.1);
    picture_file(&samples.join("cover.png"), [10, 200, 120]);
    let mut h = Harness::with_project(empty_project("Covers"));
    h.offline_packs();
    h.app.browser.add_folder(samples.clone());
    h.frames(2);
    h.wait_jobs();
    h.frames(2);
    let key = folder_key(&samples);
    let shown = h
        .app
        .pictures
        .browser_shown(&key, &crate::pictures::folder_default(&samples));
    assert!(
        matches!(&shown, Shown::Image { image, .. } if image.name == "cover.png"),
        "{shown:?}"
    );
    // A picture someone picks wins over the cover.
    h.app.pictures.store.set(
        &key,
        Some(sky_model::Picture::symbol(sky_model::Symbol::Drums)),
    );
    let shown = h
        .app
        .pictures
        .browser_shown(&key, &crate::pictures::folder_default(&samples));
    assert!(matches!(shown, Shown::Symbol { .. }));
}
