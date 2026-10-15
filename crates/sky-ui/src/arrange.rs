// SPDX-License-Identifier: GPL-3.0-or-later
//! The arrangement: bar ruler with the loop region and playhead, and one
//! lane of clips per track next to its header.

use std::collections::HashMap;

use egui::{
    Align, Align2, Color32, Context, CornerRadius, CursorIcon, Frame, Id, Key, Layout, Margin,
    Painter, Rect, RichText, ScrollArea, Sense, Shape, Stroke, StrokeKind, Ui, UiBuilder, pos2,
    vec2,
};
use sky_core::{ClipId, NoteId, PPQ, Ticks, TimeSignature, TrackId};
use sky_model::{Clip, ClipContent, Command, Note, Project, Track, TrackKind, Transaction};
use sky_pattern::{Pattern, Time};

use crate::actions::{self, NewTrack};
use crate::app::{Action, App, Tab};
use crate::theme;

/// Width of the track headers left of the lanes.
pub const HEADER_W: f32 = 236.0;
pub const ROW_H: f32 = 64.0;
pub const RULER_H: f32 = 30.0;
const LOOP_BAND: f32 = 11.0;
const HBAR_H: f32 = 12.0;
const MIN_PX_PER_TICK: f32 = 4.0 / 3840.0;
const MAX_PX_PER_TICK: f32 = 1.0;
const DEFAULT_PX_PER_TICK: f32 = 150.0 / 3840.0;
const CLIP_HEAD: f32 = 15.0;

/// What clip starts and ends snap to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Snap {
    Off,
    Bar,
    #[default]
    Beat,
    Eighth,
    Sixteenth,
}

impl Snap {
    pub const ALL: [Snap; 5] = [
        Snap::Bar,
        Snap::Beat,
        Snap::Eighth,
        Snap::Sixteenth,
        Snap::Off,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Snap::Off => "Off",
            Snap::Bar => "Bar",
            Snap::Beat => "Beat",
            Snap::Eighth => "1/8",
            Snap::Sixteenth => "1/16",
        }
    }

    pub fn grid(self, ts: TimeSignature) -> Option<Ticks> {
        match self {
            Snap::Off => None,
            Snap::Bar => Some(ts.ticks_per_bar()),
            Snap::Beat => Some(ts.ticks_per_beat()),
            Snap::Eighth => Some(PPQ / 2),
            Snap::Sixteenth => Some(PPQ / 4),
        }
    }
}

/// Rounds to the nearest grid line.
pub fn snap_round(tick: f64, grid: Option<Ticks>) -> Ticks {
    match grid {
        Some(g) if g > 0 => (tick / g as f64).round() as Ticks * g,
        _ => tick.round() as Ticks,
    }
}

/// Rounds down to a grid line.
pub fn snap_floor(tick: f64, grid: Option<Ticks>) -> Ticks {
    match grid {
        Some(g) if g > 0 => (tick / g as f64).floor() as Ticks * g,
        _ => tick.floor() as Ticks,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipDragKind {
    Move,
    Resize,
}

/// A clip being moved or resized. Shown as a preview and applied once, when
/// the button is released, so the gesture is one undo step.
#[derive(Clone, Debug)]
pub struct ClipDrag {
    pub clip: ClipId,
    pub kind: ClipDragKind,
    pub from_track: TrackId,
    pub grab_tick: f64,
    pub orig_start: Ticks,
    pub orig_len: Ticks,
    pub start: Ticks,
    pub length: Ticks,
    pub track: TrackId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopDragKind {
    Move,
    Start,
    End,
    New,
}

#[derive(Clone, Copy, Debug)]
pub struct LoopDrag {
    pub kind: LoopDragKind,
    pub grab_tick: f64,
    pub orig: (Ticks, Ticks),
    pub start: Ticks,
    pub end: Ticks,
}

/// Parsed patterns of pattern clips, for drawing their events.
#[derive(Default)]
pub struct PatternCache {
    map: HashMap<ClipId, (String, Option<Pattern>)>,
}

impl PatternCache {
    pub fn get(&mut self, id: ClipId, source: &str) -> Option<&Pattern> {
        if self.map.len() > 4096 {
            self.map.clear();
        }
        let entry = self
            .map
            .entry(id)
            .or_insert_with(|| (source.to_string(), sky_pattern::parse(source).ok()));
        if entry.0 != source {
            *entry = (source.to_string(), sky_pattern::parse(source).ok());
        }
        entry.1.as_ref()
    }
}

pub struct ArrangeState {
    pub px_per_tick: f32,
    /// The tick at the left edge of the lanes.
    pub scroll: f64,
    pub snap: Snap,
    pub follow: bool,
    pub drag: Option<ClipDrag>,
    pub loop_drag: Option<LoopDrag>,
    pub scrubbing: bool,
    /// Lane rects drawn this frame and the last, for finding drop targets.
    pub lanes: Vec<(TrackId, Rect)>,
    pub prev_lanes: Vec<(TrackId, Rect)>,
    pub lane_left: f32,
    pub lane_width: f32,
    /// Where the lane context menu was opened.
    pub context_tick: f64,
    /// A track header whose name is being edited.
    pub renaming: Option<(TrackId, String)>,
    pub rename_focus: bool,
    pub patterns: PatternCache,
}

impl Default for ArrangeState {
    fn default() -> Self {
        Self {
            px_per_tick: DEFAULT_PX_PER_TICK,
            scroll: 0.0,
            snap: Snap::default(),
            follow: true,
            drag: None,
            loop_drag: None,
            scrubbing: false,
            lanes: Vec::new(),
            prev_lanes: Vec::new(),
            lane_left: HEADER_W,
            lane_width: 800.0,
            context_tick: 0.0,
            renaming: None,
            rename_focus: false,
            patterns: PatternCache::default(),
        }
    }
}

impl ArrangeState {
    pub fn x_of(&self, tick: f64) -> f32 {
        self.lane_left + ((tick - self.scroll) * self.px_per_tick as f64) as f32
    }

    pub fn tick_at(&self, x: f32) -> f64 {
        self.scroll + ((x - self.lane_left) / self.px_per_tick) as f64
    }

    pub fn visible_ticks(&self) -> f64 {
        (self.lane_width / self.px_per_tick) as f64
    }

    pub fn reset_for_new_project(&mut self) {
        self.scroll = 0.0;
        self.drag = None;
        self.loop_drag = None;
        self.scrubbing = false;
        self.renaming = None;
        self.patterns = PatternCache::default();
    }

    /// Zooms so that `ticks` fill the visible lanes.
    pub fn fit(&mut self, ticks: Ticks) {
        let ticks = ticks.max(PPQ * 4) as f32;
        self.px_per_tick = (self.lane_width * 0.95 / ticks).clamp(MIN_PX_PER_TICK, MAX_PX_PER_TICK);
        self.scroll = 0.0;
    }

    fn zoom_at(&mut self, factor: f32, x: f32) {
        let anchor = self.tick_at(x);
        self.px_per_tick = (self.px_per_tick * factor).clamp(MIN_PX_PER_TICK, MAX_PX_PER_TICK);
        self.scroll = (anchor - ((x - self.lane_left) / self.px_per_tick) as f64).max(0.0);
    }
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let view = app.view.clone();
    let project = &view.project;
    toolbar(app, ui, project);

    let full = ui.available_rect_before_wrap();
    if full.width() < HEADER_W + 60.0 || full.height() < RULER_H + 30.0 {
        return;
    }
    let lane_left = full.left() + HEADER_W;
    let ruler = Rect::from_min_max(
        pos2(lane_left, full.top()),
        pos2(full.right(), full.top() + RULER_H),
    );
    let corner = Rect::from_min_max(full.min, pos2(lane_left, ruler.bottom()));
    let hbar = Rect::from_min_max(pos2(lane_left, full.bottom() - HBAR_H), full.max);
    let body = Rect::from_min_max(
        pos2(full.left(), ruler.bottom()),
        pos2(full.right(), hbar.top()),
    );
    {
        let st = &mut app.arrange;
        st.lane_left = lane_left;
        st.lane_width = ruler.width().max(1.0);
        st.prev_lanes = std::mem::take(&mut st.lanes);
    }

    timeline_input(app, ui, Rect::from_min_max(ruler.min, body.max));
    follow_playhead(app);
    update_clip_drag(app, ui.ctx(), project);
    update_loop_drag(app, ui.ctx(), project);

    corner_ui(app, ui, corner, project);
    ruler_ui(app, ui, ruler, project);
    rows(app, ui, body, project);
    crate::browser::arrange_drop(app, ui, body, project);
    hscrollbar(app, ui, hbar, project);
    ui.advance_cursor_after_rect(full);
}

fn toolbar(app: &mut App, ui: &mut Ui, project: &Project) {
    Frame::new()
        .fill(theme::PANEL)
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.toggle_value(&mut app.browser.open, "Browser")
                    .on_hover_text("Show or hide the browser of audio and sample packs (Ctrl+B)");
                ui.separator();
                ui.label(RichText::new("Add track").color(theme::TEXT_WEAK));
                for kind in NewTrack::ALL {
                    let tip = match kind {
                        NewTrack::Instrument(t) => t.description(),
                        NewTrack::Audio => "Plays audio clips from the project's audio pool",
                    };
                    if ui
                        .button(format!("+ {}", kind.label()))
                        .on_hover_text(tip)
                        .clicked()
                    {
                        let ctx = ui.ctx().clone();
                        app.run_action(&ctx, Action::AddTrack(kind));
                    }
                }
                ui.separator();
                ui.label(RichText::new("Snap").color(theme::TEXT_WEAK));
                egui::ComboBox::from_id_salt("snap")
                    .width(64.0)
                    .selected_text(app.arrange.snap.label())
                    .show_ui(ui, |ui| {
                        for s in Snap::ALL {
                            ui.selectable_value(&mut app.arrange.snap, s, s.label());
                        }
                    })
                    .response
                    .on_hover_text("Grid for moving and resizing clips. Hold Alt while dragging to place freely.");
                ui.toggle_value(&mut app.arrange.follow, "Follow")
                    .on_hover_text("Scroll with the playhead while playing");
                ui.separator();
                let center = app.arrange.lane_left + app.arrange.lane_width * 0.5;
                if ui.button("−").on_hover_text("Zoom out (Ctrl+wheel)").clicked() {
                    app.arrange.zoom_at(1.0 / 1.4, center);
                }
                if ui.button("+").on_hover_text("Zoom in (Ctrl+wheel)").clicked() {
                    app.arrange.zoom_at(1.4, center);
                }
                if ui.button("Fit").on_hover_text("Show the whole song").clicked() {
                    app.arrange.fit(project.content_end());
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.available_width() < 420.0 {
                        return;
                    }
                    ui.label(
                        RichText::new(
                            "Double-click a lane for a clip · Shift+wheel scrolls · Ctrl+wheel zooms",
                        )
                        .small()
                        .color(theme::TEXT_DIM),
                    );
                });
            });
        });
}

fn timeline_input(app: &mut App, ui: &mut Ui, rect: Rect) {
    if !ui.rect_contains_pointer(rect) {
        return;
    }
    let (pos, zoom, dx) = ui.input(|i| {
        (
            i.pointer.hover_pos(),
            i.zoom_delta(),
            i.smooth_scroll_delta.x,
        )
    });
    let Some(pos) = pos else {
        return;
    };
    let st = &mut app.arrange;
    if zoom != 1.0 {
        st.zoom_at(zoom, pos.x.max(st.lane_left));
    }
    if dx != 0.0 {
        st.scroll = (st.scroll - (dx / st.px_per_tick) as f64).max(0.0);
        ui.input_mut(|i| i.smooth_scroll_delta.x = 0.0);
    }
}

fn follow_playhead(app: &mut App) {
    if !(app.transport.playing && app.arrange.follow) || app.arrange.drag.is_some() {
        return;
    }
    let pos = app.transport.position_ticks;
    let st = &mut app.arrange;
    let vis = st.visible_ticks();
    if pos > st.scroll + vis * 0.92 || pos < st.scroll {
        st.scroll = (pos - vis * 0.08).max(0.0);
    }
}

fn clip_fits(track: &Track, clip: &Clip) -> bool {
    match clip.content {
        ClipContent::Audio { .. } => track.kind == TrackKind::Audio,
        ClipContent::Midi { .. } | ClipContent::Pattern { .. } | ClipContent::Steps(_) => {
            track.kind == TrackKind::Instrument
        }
    }
}

fn update_clip_drag(app: &mut App, ctx: &Context, project: &Project) {
    let Some(mut drag) = app.arrange.drag.clone() else {
        return;
    };
    let Ok((_, clip)) = project.find_clip(drag.clip) else {
        app.arrange.drag = None;
        return;
    };
    let (pos, down, alt, escape) = ctx.input(|i| {
        (
            i.pointer.latest_pos(),
            i.pointer.primary_down(),
            i.modifiers.alt,
            i.key_pressed(Key::Escape),
        )
    });
    if escape {
        app.arrange.drag = None;
        return;
    }
    let grid = if alt {
        None
    } else {
        app.arrange.snap.grid(project.time_signature)
    };
    if let Some(pos) = pos {
        let tick = app.arrange.tick_at(pos.x);
        match drag.kind {
            ClipDragKind::Move => {
                let raw = drag.orig_start as f64 + (tick - drag.grab_tick);
                drag.start = snap_round(raw, grid).max(0);
                let target = app
                    .arrange
                    .prev_lanes
                    .iter()
                    .find(|(_, r)| r.y_range().contains(pos.y))
                    .and_then(|(id, _)| project.track(*id).ok());
                if let Some(t) = target
                    && clip_fits(t, clip)
                {
                    drag.track = t.id;
                }
            }
            ClipDragKind::Resize => {
                let min_len = grid.unwrap_or(PPQ / 16).max(1);
                let end = drag.orig_start as f64 + drag.orig_len as f64 + (tick - drag.grab_tick);
                let end = snap_round(end, grid);
                drag.length = (end - drag.orig_start).max(min_len);
            }
        }
        ctx.set_cursor_icon(match drag.kind {
            ClipDragKind::Move => CursorIcon::Grabbing,
            ClipDragKind::Resize => CursorIcon::ResizeHorizontal,
        });
    }
    if down {
        app.arrange.drag = Some(drag);
        return;
    }
    app.arrange.drag = None;
    commit_clip_drag(app, &drag);
}

/// Applies a finished clip drag as one change.
pub fn commit_clip_drag(app: &mut App, drag: &ClipDrag) {
    match drag.kind {
        ClipDragKind::Move if drag.start != drag.orig_start || drag.track != drag.from_track => {
            let r = actions::move_clip(&app.session, drag.clip, drag.track, drag.start);
            if app.report(r).is_some() {
                app.sel.select_clip(drag.track, drag.clip);
            }
        }
        ClipDragKind::Resize if drag.length != drag.orig_len => {
            let r = actions::resize_clip(&app.session, drag.clip, drag.length);
            app.report(r);
        }
        _ => {}
    }
    app.refresh();
}

fn update_loop_drag(app: &mut App, ctx: &Context, project: &Project) {
    let Some(mut d) = app.arrange.loop_drag else {
        return;
    };
    let (pos, down, alt) = ctx.input(|i| {
        (
            i.pointer.latest_pos(),
            i.pointer.primary_down(),
            i.modifiers.alt,
        )
    });
    let grid = if alt {
        None
    } else {
        app.arrange.snap.grid(project.time_signature)
    };
    let min_len = grid.unwrap_or(PPQ / 4).max(1);
    if let Some(pos) = pos {
        let t = app.arrange.tick_at(pos.x);
        let (s0, e0) = d.orig;
        match d.kind {
            LoopDragKind::Move => {
                let s = snap_round(s0 as f64 + (t - d.grab_tick), grid).max(0);
                d.start = s;
                d.end = s + (e0 - s0);
            }
            LoopDragKind::Start => {
                d.start = snap_round(t, grid).clamp(0, (e0 - min_len).max(0));
                d.end = e0;
            }
            LoopDragKind::End => {
                d.start = s0;
                d.end = snap_round(t, grid).max(s0 + min_len);
            }
            LoopDragKind::New => {
                let a = snap_round(d.grab_tick, grid).max(0);
                let b = snap_round(t, grid).max(0);
                d.start = a.min(b);
                d.end = a.max(b).max(d.start + min_len);
            }
        }
    }
    if down {
        app.arrange.loop_drag = Some(d);
        return;
    }
    app.arrange.loop_drag = None;
    if (d.start, d.end) != d.orig && d.end > d.start {
        let r = actions::set_loop_region(&app.session, d.start, d.end);
        app.report(r);
        app.refresh();
    }
}

/// Bars between labelled ruler lines at the current zoom.
fn bar_step(project: &Project, px_per_tick: f32) -> i64 {
    let px_bar = project.ticks_per_bar() as f32 * px_per_tick;
    [1, 2, 4, 8, 16, 32, 64, 128, 256]
        .into_iter()
        .find(|n| px_bar * *n as f32 >= 44.0)
        .unwrap_or(512)
}

fn draw_grid(painter: &Painter, st: &ArrangeState, rect: Rect, project: &Project) {
    let tpb = project.ticks_per_bar().max(1);
    let beat = project.time_signature.ticks_per_beat().max(1);
    let from = st.tick_at(rect.left()).max(0.0) as i64;
    let to = st.tick_at(rect.right()).max(0.0) as i64 + 1;
    let ppt = st.px_per_tick;
    let mut lines = Vec::new();
    if beat as f32 * ppt >= 12.0 {
        let sub = if (PPQ / 4) as f32 * ppt >= 12.0 {
            PPQ / 4
        } else {
            beat
        };
        let mut t = from / sub * sub;
        let mut n = 0;
        while t <= to && n < 4000 {
            if t % tpb != 0 {
                let c = if t % beat == 0 {
                    theme::GRID_BEAT
                } else {
                    theme::GRID_FAINT
                };
                let x = st.x_of(t as f64);
                lines.push(Shape::vline(x, rect.y_range(), Stroke::new(1.0, c)));
            }
            t += sub;
            n += 1;
        }
    }
    let step = bar_step(project, ppt) * tpb;
    let mut t = from / step * step;
    let mut n = 0;
    while t <= to && n < 2000 {
        let x = st.x_of(t as f64);
        lines.push(Shape::vline(
            x,
            rect.y_range(),
            Stroke::new(1.0, theme::GRID_BAR),
        ));
        t += step;
        n += 1;
    }
    painter.extend(lines);
}

fn loop_range(app: &App, project: &Project) -> (Ticks, Ticks) {
    match &app.arrange.loop_drag {
        Some(d) => (d.start, d.end),
        None => (project.loop_region.start, project.loop_region.end),
    }
}

fn corner_ui(app: &mut App, ui: &mut Ui, rect: Rect, project: &Project) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, theme::HEADER);
    painter.text(
        pos2(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        format!(
            "{} track{}",
            project.tracks.len(),
            if project.tracks.len() == 1 { "" } else { "s" }
        ),
        theme::small(),
        theme::TEXT_WEAK,
    );
    let pos = project
        .time_signature
        .format_position(app.transport.position_ticks.max(0.0) as Ticks);
    painter.text(
        pos2(rect.right() - 10.0, rect.center().y),
        Align2::RIGHT_CENTER,
        pos,
        theme::mono(12.0),
        theme::TEXT_DIM,
    );
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, theme::BG),
    );
}

fn ruler_ui(app: &mut App, ui: &mut Ui, rect: Rect, project: &Project) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, theme::HEADER);
    let band = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + LOOP_BAND));
    let st = &app.arrange;

    // Loop region.
    let (ls, le) = loop_range(app, project);
    let (lx0, lx1) = (st.x_of(ls as f64), st.x_of(le as f64));
    let loop_rect = Rect::from_min_max(pos2(lx0, band.top() + 2.0), pos2(lx1, band.bottom()));
    let loop_color = if project.loop_enabled {
        theme::ACCENT.gamma_multiply(0.7)
    } else {
        theme::GRID_BAR
    };
    painter.rect_filled(loop_rect, CornerRadius::same(2), loop_color);

    // Bar numbers and beat ticks.
    let tpb = project.ticks_per_bar().max(1);
    let beat = project.time_signature.ticks_per_beat().max(1);
    let from = st.tick_at(rect.left()).max(0.0) as i64;
    let to = st.tick_at(rect.right()).max(0.0) as i64 + 1;
    if beat as f32 * st.px_per_tick >= 8.0 {
        let mut t = from / beat * beat;
        let mut n = 0;
        while t <= to && n < 4000 {
            if t % tpb != 0 {
                let x = st.x_of(t as f64);
                painter.vline(
                    x,
                    (rect.bottom() - 5.0)..=rect.bottom(),
                    Stroke::new(1.0, theme::TEXT_DIM),
                );
            }
            t += beat;
            n += 1;
        }
    }
    let step = bar_step(project, st.px_per_tick) * tpb;
    let mut t = from / step * step;
    let mut n = 0;
    while t <= to && n < 2000 {
        let x = st.x_of(t as f64);
        painter.vline(
            x,
            band.bottom()..=rect.bottom(),
            Stroke::new(1.0, theme::GRID_BAR),
        );
        painter.text(
            pos2(x + 4.0, band.bottom() + 1.0),
            Align2::LEFT_TOP,
            format!("{}", t / tpb + 1),
            theme::small(),
            theme::TEXT_WEAK,
        );
        t += step;
        n += 1;
    }

    // Playhead.
    let px = st.x_of(app.transport.position_ticks);
    if rect.x_range().contains(px) {
        painter.vline(px, rect.y_range(), Stroke::new(1.5, theme::ACCENT));
        painter.add(Shape::convex_polygon(
            vec![
                pos2(px - 5.0, rect.bottom() - 7.0),
                pos2(px + 5.0, rect.bottom() - 7.0),
                pos2(px, rect.bottom()),
            ],
            theme::ACCENT,
            Stroke::NONE,
        ));
    }
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, theme::BG),
    );

    // Interaction: the top band edits the loop, the rest moves the playhead.
    let resp = ui.interact(rect, Id::new("ruler"), Sense::click_and_drag());
    let near = |x: f32, edge: f32| (x - edge).abs() <= 5.0;
    if let Some(h) = resp.hover_pos()
        && app.arrange.loop_drag.is_none()
    {
        if h.y <= band.bottom() + 1.0 {
            if near(h.x, lx0) || near(h.x, lx1) {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            } else if h.x > lx0 && h.x < lx1 {
                ui.ctx().set_cursor_icon(CursorIcon::Grab);
            }
        } else {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
    }
    let alt = ui.input(|i| i.modifiers.alt);
    let grid = if alt {
        None
    } else {
        app.arrange.snap.grid(project.time_signature)
    };
    if resp.drag_started()
        && let Some(origin) = ui.input(|i| i.pointer.press_origin())
    {
        let grab = app.arrange.tick_at(origin.x);
        if origin.y <= band.bottom() + 1.0 {
            let kind = if near(origin.x, lx0) {
                LoopDragKind::Start
            } else if near(origin.x, lx1) {
                LoopDragKind::End
            } else if origin.x > lx0 && origin.x < lx1 {
                LoopDragKind::Move
            } else {
                LoopDragKind::New
            };
            app.arrange.loop_drag = Some(LoopDrag {
                kind,
                grab_tick: grab,
                orig: (ls, le),
                start: ls,
                end: le,
            });
        } else {
            app.arrange.scrubbing = true;
        }
    }
    if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        if p.y > band.bottom() + 1.0 {
            let tick = snap_round(app.arrange.tick_at(p.x), grid).max(0);
            seek(app, tick);
        } else if resp.double_clicked() {
            let r = actions::set_loop_enabled(&app.session, !project.loop_enabled);
            app.report(r);
        }
    }
    if app.arrange.scrubbing {
        if resp.dragged() {
            if let Some(p) = ui.input(|i| i.pointer.latest_pos()) {
                let tick = app.arrange.tick_at(p.x).max(0.0) as Ticks;
                seek(app, tick);
            }
        } else {
            app.arrange.scrubbing = false;
        }
    }
}

/// Moves the playhead.
pub fn seek(app: &mut App, tick: Ticks) {
    if app.session.seek(tick.max(0)).is_err() {
        app.error("No audio device is running, so the playhead cannot move");
    }
}

fn rows(app: &mut App, ui: &mut Ui, body: Rect, project: &Project) {
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(body)
            .layout(Layout::top_down(Align::Min)),
    );
    child.spacing_mut().item_spacing.y = 0.0;
    if project.tracks.is_empty() {
        empty_state(app, &mut child, body);
        return;
    }
    let count = project.tracks.len();
    ScrollArea::vertical()
        .id_salt("arrange_rows")
        .auto_shrink([false, false])
        .show_rows(&mut child, ROW_H, count, |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for i in range {
                if let Some(track) = project.tracks.get(i) {
                    row(app, ui, project, track, i);
                }
            }
        });
}

fn empty_state(app: &mut App, ui: &mut Ui, body: Rect) {
    ui.painter()
        .rect_filled(body, CornerRadius::ZERO, theme::LANE);
    let inner = Rect::from_center_size(body.center(), vec2(420.0, 120.0));
    ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("No tracks yet").heading().color(theme::TEXT));
            ui.label(
                RichText::new("Add an instrument track to write MIDI or patterns, or an audio track for recordings.")
                    .color(theme::TEXT_WEAK),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for kind in NewTrack::ALL {
                    if ui.button(format!("+ {}", kind.label())).clicked() {
                        let ctx = ui.ctx().clone();
                        app.run_action(&ctx, Action::AddTrack(kind));
                    }
                }
            });
        });
    });
}

fn row(app: &mut App, ui: &mut Ui, project: &Project, track: &Track, index: usize) {
    let (_, rect) = ui.allocate_space(vec2(ui.available_width(), ROW_H));
    let header = Rect::from_min_size(rect.min, vec2(HEADER_W, ROW_H));
    let lane = Rect::from_min_max(pos2(rect.left() + HEADER_W, rect.top()), rect.max);
    crate::tracks::header(app, ui, header, project, track, index);
    lane_ui(app, ui, lane, project, track, index);
    app.arrange.lanes.push((track.id, lane));
}

fn clip_rect(st: &ArrangeState, lane: Rect, start: Ticks, length: Ticks) -> Rect {
    let x0 = st.x_of(start as f64);
    let x1 = st.x_of((start + length.max(0)) as f64).max(x0 + 3.0);
    Rect::from_min_max(pos2(x0, lane.top() + 3.0), pos2(x1, lane.bottom() - 3.0))
}

fn lane_ui(app: &mut App, ui: &mut Ui, rect: Rect, project: &Project, track: &Track, index: usize) {
    let painter = ui.painter_at(rect);
    let selected = app.sel.track == Some(track.id);
    let bg = if selected {
        theme::lighten(theme::LANE, 0.035)
    } else if index.is_multiple_of(2) {
        theme::LANE
    } else {
        theme::LANE_ALT
    };
    painter.rect_filled(rect, CornerRadius::ZERO, bg);
    let (ls, le) = loop_range(app, project);
    if project.loop_enabled {
        let lr = Rect::from_min_max(
            pos2(app.arrange.x_of(ls as f64), rect.top()),
            pos2(app.arrange.x_of(le as f64), rect.bottom()),
        );
        painter.rect_filled(lr, CornerRadius::ZERO, theme::LOOP_FILL);
    }
    draw_grid(&painter, &app.arrange, rect, project);
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, theme::BG),
    );

    let resp = ui.interact(rect, Id::new(("lane", track.id)), Sense::click());
    if resp.clicked() {
        app.sel.select_track(track.id);
    }
    if resp.double_clicked()
        && track.kind == TrackKind::Instrument
        && let Some(p) = resp.interact_pointer_pos()
    {
        let tick = app.arrange.tick_at(p.x);
        create_clip_at(app, track.id, tick);
    }
    if resp.secondary_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        app.arrange.context_tick = app.arrange.tick_at(p.x).max(0.0);
        app.sel.select_track(track.id);
    }
    resp.context_menu(|ui| lane_menu(app, ui, project, track));

    let drag = app.arrange.drag.clone();
    for clip in &track.clips {
        if drag.as_ref().is_some_and(|d| d.clip == clip.id) {
            continue;
        }
        clip_ui(app, ui, &painter, rect, project, track, clip);
    }
    if let Some(d) = drag.as_ref().filter(|d| d.track == track.id)
        && let Ok((_, clip)) = project.find_clip(d.clip)
    {
        let mut ghost = clip.clone();
        ghost.start = d.start;
        ghost.length = d.length;
        let r = clip_rect(&app.arrange, rect, ghost.start, ghost.length);
        let ctx = ui.ctx().clone();
        draw_clip(app, &ctx, &painter, r, &ghost, track, true, project);
    }

    let px = app.arrange.x_of(app.transport.position_ticks);
    if rect.x_range().contains(px) {
        painter.vline(px, rect.y_range(), Stroke::new(1.0, theme::ACCENT));
    }
}

fn lane_menu(app: &mut App, ui: &mut Ui, project: &Project, track: &Track) {
    let tick = app.arrange.context_tick;
    let grid = app.arrange.snap.grid(project.time_signature);
    let start = snap_floor(tick, grid).max(0);
    let bar = project.ticks_per_bar();
    match track.kind {
        TrackKind::Instrument => {
            if ui.button("Add MIDI clip here").clicked() {
                create_clip_at(app, track.id, tick);
            }
            if ui.button("Add pattern clip here").clicked() {
                let is_drums = track
                    .instrument
                    .as_ref()
                    .is_some_and(|i| i.kind == sky_dsp::InstrumentType::Drums);
                let source = if is_drums {
                    "bd*4, ~ sd ~ sd, hh*8"
                } else {
                    "c4 e4 g4 e4"
                };
                let r = actions::add_pattern_clip(&app.session, track.id, start, bar * 4, source);
                if let Some(id) = app.report(r) {
                    app.refresh();
                    app.sel.select_clip(track.id, id);
                    app.tab = Tab::Editor;
                }
            }
            if ui.button("Add step clip here").clicked() {
                let steps = actions::starter_steps(track);
                let r = actions::add_step_clip(&app.session, track.id, start, bar * 4, steps);
                if let Some(id) = app.report(r) {
                    app.refresh();
                    app.sel.select_clip(track.id, id);
                    app.tab = Tab::Editor;
                }
            }
        }
        TrackKind::Audio => {
            if ui.button("Import audio…").clicked() {
                app.sel.select_track(track.id);
                let ctx = ui.ctx().clone();
                app.run_action(&ctx, Action::ImportAudio);
            }
            let pool: Vec<_> = project
                .audio_pool
                .iter()
                .map(|e| (e.id, e.name.clone()))
                .collect();
            if !pool.is_empty() {
                ui.menu_button("Add clip from pool", |ui| {
                    for (id, name) in pool {
                        if ui.button(name).clicked() {
                            let r = actions::add_audio_clip(&app.session, track.id, start, id);
                            if let Some(clip) = app.report(r) {
                                app.refresh();
                                app.sel.select_clip(track.id, clip);
                            }
                        }
                    }
                });
            }
        }
    }
}

/// Creates a one-bar MIDI clip at `tick` on an instrument track and opens it.
pub fn create_clip_at(app: &mut App, track: TrackId, tick: f64) -> Option<ClipId> {
    let project = app.view.project.clone();
    let grid = app.arrange.snap.grid(project.time_signature);
    let start = snap_floor(tick, grid).max(0);
    let r = actions::add_midi_clip(&app.session, track, start, project.ticks_per_bar());
    let id = app.report(r)?;
    app.refresh();
    app.sel.select_clip(track, id);
    app.tab = Tab::Editor;
    Some(id)
}

/// Starts dragging a clip, as if the pointer had pressed at `grab_x`.
pub fn begin_clip_drag(app: &mut App, track: &Track, clip: &Clip, kind: ClipDragKind, grab_x: f32) {
    app.sel.select_clip(track.id, clip.id);
    app.arrange.drag = Some(ClipDrag {
        clip: clip.id,
        kind,
        from_track: track.id,
        grab_tick: app.arrange.tick_at(grab_x),
        orig_start: clip.start,
        orig_len: clip.length,
        start: clip.start,
        length: clip.length,
        track: track.id,
    });
}

fn clip_ui(
    app: &mut App,
    ui: &mut Ui,
    painter: &Painter,
    lane: Rect,
    project: &Project,
    track: &Track,
    clip: &Clip,
) {
    let r = clip_rect(&app.arrange, lane, clip.start, clip.length);
    if r.right() < lane.left() || r.left() > lane.right() {
        return;
    }
    let id = Id::new(("clip", clip.id));
    let body = ui.interact(r.intersect(lane), id, Sense::click_and_drag());
    let edge_rect = Rect::from_min_max(
        pos2((r.right() - 6.0).max(r.left() + 2.0), r.top()),
        pos2(r.right() + 2.0, r.bottom()),
    )
    .intersect(lane);
    let edge = ui.interact(edge_rect, id.with("edge"), Sense::drag());
    let selected = app.sel.clip == Some(clip.id);
    let ctx = ui.ctx().clone();
    draw_clip(app, &ctx, painter, r, clip, track, selected, project);

    if edge.hovered() {
        ctx.set_cursor_icon(CursorIcon::ResizeHorizontal);
    } else if body.hovered() {
        ctx.set_cursor_icon(CursorIcon::Grab);
    }
    if body.clicked() {
        app.sel.select_clip(track.id, clip.id);
    }
    if body.double_clicked() {
        app.sel.select_clip(track.id, clip.id);
        app.tab = Tab::Editor;
    }
    let origin = ui.input(|i| i.pointer.press_origin());
    if edge.drag_started() {
        let x = origin.map_or(r.right(), |p| p.x);
        begin_clip_drag(app, track, clip, ClipDragKind::Resize, x);
    } else if body.drag_started() {
        let x = origin.map_or(r.left(), |p| p.x);
        begin_clip_drag(app, track, clip, ClipDragKind::Move, x);
    }
    body.context_menu(|ui| {
        if ui.button("Open in editor").clicked() {
            app.sel.select_clip(track.id, clip.id);
            app.tab = Tab::Editor;
        }
        if ui.button("Duplicate").clicked() {
            duplicate_clip(app, track, clip);
        }
        ui.separator();
        if ui
            .button(RichText::new("Delete clip").color(theme::ERROR))
            .clicked()
        {
            let r = actions::delete_clip(&app.session, clip.id);
            app.report(r);
        }
    });
}

/// Copies a clip to just after itself.
pub fn duplicate_clip(app: &mut App, track: &Track, clip: &Clip) {
    let mut copy = clip.clone();
    copy.id = ClipId::new();
    copy.start = clip.end();
    if let ClipContent::Midi { notes } = &mut copy.content {
        for n in notes.iter_mut() {
            n.id = NoteId::new();
        }
    }
    let id = copy.id;
    let r = actions::apply(
        &app.session,
        Transaction::user(
            "Duplicate clip",
            vec![Command::AddClip {
                track: track.id,
                clip: Box::new(copy),
            }],
        ),
        None,
    );
    if app.report(r).is_some() {
        app.refresh();
        app.sel.select_clip(track.id, id);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_clip(
    app: &mut App,
    ctx: &Context,
    painter: &Painter,
    r: Rect,
    clip: &Clip,
    track: &Track,
    selected: bool,
    project: &Project,
) {
    let vis = r.intersect(painter.clip_rect());
    if vis.width() <= 0.0 || vis.height() <= 0.0 {
        return;
    }
    let p = painter.with_clip_rect(vis.expand(1.0));
    let color = theme::track_color(track.color);
    p.rect_filled(r, CornerRadius::same(3), theme::darken(color, 0.64));
    let head = Rect::from_min_max(
        r.min,
        pos2(r.right(), (r.top() + CLIP_HEAD).min(r.bottom())),
    );
    p.rect_filled(
        head,
        CornerRadius {
            nw: 3,
            ne: 3,
            sw: 0,
            se: 0,
        },
        theme::darken(color, 0.22),
    );
    let text_left = r.left().max(vis.left());
    let name = if clip.name.is_empty() {
        clip.kind_name()
    } else {
        clip.name.as_str()
    };
    p.text(
        pos2(text_left + 4.0, head.center().y),
        Align2::LEFT_CENTER,
        name,
        theme::small(),
        Color32::from_gray(245),
    );
    let body = Rect::from_min_max(
        pos2(r.left(), head.bottom() + 1.0),
        pos2(r.right(), r.bottom() - 2.0),
    );
    if body.height() > 2.0 {
        match &clip.content {
            ClipContent::Midi { notes } => draw_notes(
                &p,
                r,
                body,
                vis,
                clip,
                notes,
                app.arrange.px_per_tick,
                color,
            ),
            ClipContent::Pattern {
                source,
                seed,
                cycle,
                ..
            } => draw_pattern(app, &p, r, body, vis, clip, source, *seed, *cycle, color),
            ClipContent::Steps(steps) => {
                let notes: Vec<Note> = steps
                    .notes(clip.length)
                    .into_iter()
                    .map(|n| Note::new(n.start, n.end - n.start, n.pitch, n.velocity))
                    .collect();
                draw_notes(
                    &p,
                    r,
                    body,
                    vis,
                    clip,
                    &notes,
                    app.arrange.px_per_tick,
                    color,
                );
            }
            ClipContent::Audio {
                source,
                offset,
                gain_db,
                fade_in,
                fade_out,
            } => {
                let (peaks, needs_job) = app.waveforms.get(*source);
                if needs_job {
                    app.spawn_peaks(ctx, *source);
                }
                let fades = (*fade_in, *fade_out);
                draw_wave(
                    &app.arrange,
                    &p,
                    r,
                    body,
                    vis,
                    clip,
                    project,
                    peaks.as_deref(),
                    *offset,
                    *gain_db,
                    fades,
                    color,
                );
            }
        }
    }
    let stroke = if selected {
        Stroke::new(2.0, theme::ACCENT)
    } else {
        Stroke::new(1.0, theme::darken(color, 0.1))
    };
    p.rect_stroke(r, CornerRadius::same(3), stroke, StrokeKind::Inside);
}

#[allow(clippy::too_many_arguments)]
fn draw_notes(
    p: &Painter,
    r: Rect,
    body: Rect,
    vis: Rect,
    clip: &Clip,
    notes: &[Note],
    ppt: f32,
    color: Color32,
) {
    let inside = notes.iter().filter(|n| n.start < clip.length);
    let (lo, hi) = inside.clone().fold((127u8, 0u8), |(lo, hi), n| {
        (lo.min(n.pitch), hi.max(n.pitch))
    });
    if lo > hi {
        return;
    }
    let span = (hi - lo) as f32 + 1.0;
    let pad = ((12.0 - span) / 2.0).max(0.0);
    let rows = span + pad * 2.0;
    let row_h = body.height() / rows;
    let fill = theme::lighten(color, 0.35);
    let mut shapes = Vec::new();
    for n in inside {
        let x0 = r.left() + n.start as f32 * ppt;
        let x1 = r.left() + (n.start + n.length).min(clip.length) as f32 * ppt;
        if x1 < vis.left() || x0 > vis.right() {
            continue;
        }
        let y = body.bottom() - ((n.pitch - lo) as f32 + pad + 1.0) * row_h;
        let h = (row_h - 1.0).clamp(1.5, 6.0);
        shapes.push(Shape::rect_filled(
            Rect::from_min_size(pos2(x0, y), vec2((x1 - x0).max(1.5), h)),
            CornerRadius::same(1),
            fill,
        ));
    }
    p.extend(shapes);
}

fn time_f64(t: Time) -> f64 {
    *t.numer() as f64 / (*t.denom()).max(1) as f64
}

#[allow(clippy::too_many_arguments)]
fn draw_pattern(
    app: &mut App,
    p: &Painter,
    r: Rect,
    body: Rect,
    vis: Rect,
    clip: &Clip,
    source: &str,
    seed: u64,
    cycle: Ticks,
    color: Color32,
) {
    let ppt = app.arrange.px_per_tick;
    let cycle_px = cycle as f32 * ppt;
    if cycle > 0 && cycle_px >= 6.0 {
        let mut x = r.left() + cycle_px;
        let end = r.left() + clip.length as f32 * ppt;
        let mut n = 0;
        while x < end && n < 2000 {
            if x > vis.left() && x < vis.right() {
                p.vline(
                    x,
                    body.y_range(),
                    Stroke::new(1.0, theme::darken(color, 0.4)),
                );
            }
            x += cycle_px;
            n += 1;
        }
    }
    if cycle > 0
        && cycle_px >= 18.0
        && let Some(pattern) = app.arrange.patterns.get(clip.id, source)
    {
        let first = (((vis.left() - r.left()) / cycle_px).floor() as i64).max(0);
        let visible_end = (vis.right() - r.left()).min(clip.length as f32 * ppt);
        let last = (visible_end / cycle_px).ceil() as i64;
        if last > first && last - first <= 64 {
            let fill = theme::lighten(color, 0.3);
            let mut shapes = Vec::new();
            for c in first..last {
                let events = pattern.query(Time::from_integer(c), Time::from_integer(c + 1), seed);
                for e in events.iter().take(512) {
                    let begin = time_f64(e.begin) * cycle as f64;
                    if begin >= clip.length as f64 {
                        continue;
                    }
                    let end = (time_f64(e.end) * cycle as f64).min(clip.length as f64);
                    let x0 = r.left() + (begin as f32) * ppt;
                    let w = (((end - begin) as f32) * ppt - 1.0).max(2.0);
                    let note = e.value.midi_note().unwrap_or(60) as f32;
                    let frac = ((note - 24.0) / 84.0).clamp(0.0, 1.0);
                    let y = body.bottom() - 3.0 - frac * (body.height() - 6.0);
                    shapes.push(Shape::rect_filled(
                        Rect::from_min_size(pos2(x0, y - 1.5), vec2(w, 3.0)),
                        CornerRadius::same(1),
                        fill,
                    ));
                }
            }
            p.extend(shapes);
        }
    }
    let text_left = r.left().max(vis.left());
    p.text(
        pos2(text_left + 4.0, body.top() + 1.0),
        Align2::LEFT_TOP,
        source,
        theme::mono(11.0),
        theme::lighten(color, 0.55),
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_wave(
    st: &ArrangeState,
    p: &Painter,
    r: Rect,
    body: Rect,
    vis: Rect,
    clip: &Clip,
    project: &Project,
    peaks: Option<&crate::waveform::Peaks>,
    offset: i64,
    gain_db: f32,
    fades: (Ticks, Ticks),
    color: Color32,
) {
    let mid = body.center().y;
    let half = (body.height() * 0.5 - 1.0).max(1.0);
    p.hline(
        vis.x_range(),
        mid,
        Stroke::new(1.0, theme::darken(color, 0.35)),
    );
    let Some(pk) = peaks else {
        p.text(
            pos2(r.left().max(vis.left()) + 4.0, mid),
            Align2::LEFT_CENTER,
            "loading waveform…",
            theme::small(),
            theme::TEXT_DIM,
        );
        return;
    };
    let gain = sky_dsp::util::db_to_gain(gain_db).min(4.0);
    let sr = pk.sample_rate.max(1) as f64;
    let tempo = &project.tempo;
    let t0 = tempo.tick_to_seconds(clip.start);
    let clip_end_x = r.right().min(vis.right());
    let frame_at = |x: f32| -> f64 {
        let tick = st.tick_at(x).round() as Ticks;
        (tempo.tick_to_seconds(tick) - t0) * sr + offset as f64
    };
    let fill = theme::lighten(color, 0.25);
    let mut x = vis.left().max(r.left()).floor();
    let mut shapes = Vec::new();
    let mut fa = frame_at(x);
    while x < clip_end_x {
        let fb = frame_at(x + 1.0);
        if let Some((lo, hi)) = pk.range(fa, fb) {
            let top = mid - (hi * gain).clamp(-1.0, 1.0) * half;
            let bottom = mid - (lo * gain).clamp(-1.0, 1.0) * half;
            shapes.push(Shape::line_segment(
                [pos2(x + 0.5, top), pos2(x + 0.5, bottom.max(top + 1.0))],
                Stroke::new(1.0, fill),
            ));
        }
        fa = fb;
        x += 1.0;
    }
    p.extend(shapes);
    let (fade_in, fade_out) = fades;
    let line = Stroke::new(1.0, theme::TEXT_WEAK);
    if fade_in > 0 {
        let fx = st.x_of((clip.start + fade_in) as f64);
        p.line_segment([pos2(r.left(), body.bottom()), pos2(fx, body.top())], line);
    }
    if fade_out > 0 {
        let fx = st.x_of((clip.end() - fade_out) as f64);
        p.line_segment([pos2(fx, body.top()), pos2(r.right(), body.bottom())], line);
    }
}

fn hscrollbar(app: &mut App, ui: &mut Ui, rect: Rect, project: &Project) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, theme::BG);
    let st = &mut app.arrange;
    let vis = st.visible_ticks().max(1.0);
    let content = (project.content_end() + project.ticks_per_bar() * 16) as f64;
    let total = content.max(st.scroll + vis);
    let w = rect.width().max(1.0);
    let thumb_w = ((vis / total) as f32 * w).clamp(24.0, w);
    let thumb_x = rect.left() + (st.scroll / total) as f32 * w;
    let thumb = Rect::from_min_size(
        pos2(thumb_x, rect.top() + 2.0),
        vec2(thumb_w, rect.height() - 4.0),
    );
    let resp = ui.interact(rect, Id::new("arrange_hscroll"), Sense::click_and_drag());
    if resp.dragged() {
        st.scroll = (st.scroll + (resp.drag_delta().x / w) as f64 * total).max(0.0);
    } else if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
        && !thumb.contains(p)
    {
        let frac = ((p.x - rect.left()) / w) as f64;
        st.scroll = (frac * total - vis * 0.5).max(0.0);
    }
    let c = if resp.hovered() || resp.dragged() {
        Color32::from_rgb(92, 100, 116)
    } else {
        Color32::from_rgb(64, 70, 82)
    };
    painter.rect_filled(thumb, CornerRadius::same(4), c);
}

/// The pixel rect of a clip in the lanes drawn last frame, for tests.
#[cfg(test)]
pub fn clip_screen_rect(app: &App, clip: ClipId) -> Option<Rect> {
    let project = app.view.project.clone();
    let (track, c) = project.find_clip(clip).ok()?;
    let lane = app.arrange.lanes.iter().find(|(id, _)| *id == track.id)?.1;
    Some(clip_rect(&app.arrange, lane, c.start, c.length))
}
