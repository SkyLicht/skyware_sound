// SPDX-License-Identifier: GPL-3.0-or-later
//! The piano roll for MIDI clips.

use egui::{
    Align2, Color32, Context, CornerRadius, CursorIcon, DragValue, Id, Key, Painter, Rect,
    RichText, Sense, Stroke, StrokeKind, Ui, pos2,
};
use sky_core::music::note_name;
use sky_core::{NoteId, PPQ, Ticks};
use sky_model::{Clip, Note, Project, Track};

use crate::app::{App, Focus};
use crate::arrange::{snap_floor, snap_round};
use crate::theme;
use crate::{actions, audition};

const KEYS_W: f32 = 46.0;
const RULER_H: f32 = 16.0;
const EDGE: f32 = 5.0;

/// Note lengths for drawing and snapping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NoteGrid {
    Quarter,
    Eighth,
    #[default]
    Sixteenth,
    ThirtySecond,
    EighthTriplet,
    SixteenthTriplet,
}

impl NoteGrid {
    pub const ALL: [NoteGrid; 6] = [
        NoteGrid::Quarter,
        NoteGrid::Eighth,
        NoteGrid::Sixteenth,
        NoteGrid::ThirtySecond,
        NoteGrid::EighthTriplet,
        NoteGrid::SixteenthTriplet,
    ];

    pub fn ticks(self) -> Ticks {
        match self {
            NoteGrid::Quarter => PPQ,
            NoteGrid::Eighth => PPQ / 2,
            NoteGrid::Sixteenth => PPQ / 4,
            NoteGrid::ThirtySecond => PPQ / 8,
            NoteGrid::EighthTriplet => PPQ / 3,
            NoteGrid::SixteenthTriplet => PPQ / 6,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            NoteGrid::Quarter => "1/4",
            NoteGrid::Eighth => "1/8",
            NoteGrid::Sixteenth => "1/16",
            NoteGrid::ThirtySecond => "1/32",
            NoteGrid::EighthTriplet => "1/8 triplet",
            NoteGrid::SixteenthTriplet => "1/16 triplet",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteDragKind {
    Move,
    Resize,
}

/// Notes being moved or resized; applied once on release.
#[derive(Clone, Debug)]
pub struct NoteDrag {
    pub kind: NoteDragKind,
    /// The note under the pointer when the drag began.
    pub anchor: NoteId,
    pub orig: Vec<Note>,
    pub preview: Vec<Note>,
    pub grab_tick: f64,
    pub grab_pitch: f32,
    /// The pitch offset last heard while moving.
    pub heard: i32,
}

pub struct PianoState {
    /// The clip the view was last fitted to.
    pub clip: Option<sky_core::ClipId>,
    pub px_per_tick: f32,
    pub scroll: f64,
    /// The pitch row at the top edge (fractional while scrolling).
    pub top_pitch: f32,
    pub row_h: f32,
    pub grid: NoteGrid,
    /// Velocity for new notes.
    pub velocity: u8,
    pub drag: Option<NoteDrag>,
}

impl Default for PianoState {
    fn default() -> Self {
        Self {
            clip: None,
            px_per_tick: 0.1,
            scroll: 0.0,
            top_pitch: 84.0,
            row_h: 13.0,
            grid: NoteGrid::default(),
            velocity: 100,
            drag: None,
        }
    }
}

struct View {
    grid: Rect,
    left: f32,
    scroll: f64,
    ppt: f32,
    top_pitch: f32,
    row_h: f32,
}

impl View {
    fn x_of(&self, tick: f64) -> f32 {
        self.left + ((tick - self.scroll) * self.ppt as f64) as f32
    }
    fn tick_at(&self, x: f32) -> f64 {
        self.scroll + ((x - self.left) / self.ppt) as f64
    }
    fn y_of(&self, pitch: u8) -> f32 {
        self.grid.top() + (self.top_pitch - pitch as f32 - 1.0) * self.row_h
    }
    fn pitch_at(&self, y: f32) -> f32 {
        self.top_pitch - (y - self.grid.top()) / self.row_h
    }
    fn note_rect(&self, n: &Note) -> Rect {
        let x0 = self.x_of(n.start as f64);
        let x1 = self.x_of((n.start + n.length) as f64).max(x0 + 3.0);
        let y = self.y_of(n.pitch);
        Rect::from_min_max(pos2(x0, y + 1.0), pos2(x1, y + self.row_h - 1.0))
    }
}

fn is_black(pitch: u8) -> bool {
    matches!(pitch % 12, 1 | 3 | 6 | 8 | 10)
}

fn fit(state: &mut PianoState, clip: &Clip, notes: &[Note], width: f32, rows: f32) {
    let len = clip.length.max(PPQ * 4) as f32;
    state.px_per_tick = (width.max(100.0) * 0.97 / len).clamp(0.005, 4.0);
    state.scroll = 0.0;
    let center = if notes.is_empty() {
        64.0
    } else {
        let (lo, hi) = notes.iter().fold((127u8, 0u8), |(lo, hi), n| {
            (lo.min(n.pitch), hi.max(n.pitch))
        });
        (lo as f32 + hi as f32) / 2.0
    };
    state.top_pitch = (center + rows / 2.0).clamp(rows.min(128.0), 128.0);
}

pub fn show(
    app: &mut App,
    ui: &mut Ui,
    project: &Project,
    track: &Track,
    clip: &Clip,
    notes: &[Note],
) {
    // Toolbar.
    let mut refit = app.piano.clip != Some(clip.id);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Grid").color(theme::TEXT_WEAK));
        egui::ComboBox::from_id_salt("piano_grid")
            .width(96.0)
            .selected_text(app.piano.grid.label())
            .show_ui(ui, |ui| {
                for g in NoteGrid::ALL {
                    ui.selectable_value(&mut app.piano.grid, g, g.label());
                }
            });
        ui.separator();
        let selected: Vec<Note> = notes
            .iter()
            .filter(|n| app.sel.notes.contains(&n.id))
            .copied()
            .collect();
        ui.label(RichText::new("Velocity").color(theme::TEXT_WEAK));
        let mut vel = selected.first().map_or(app.piano.velocity, |n| n.velocity);
        let resp = ui
            .add(DragValue::new(&mut vel).range(1..=127).speed(0.5))
            .on_hover_text("Velocity of the selected notes, or of new notes when none is selected");
        if resp.changed() {
            app.piano.velocity = vel;
            if !selected.is_empty() {
                let updated: Vec<Note> = selected
                    .iter()
                    .map(|n| Note {
                        velocity: vel,
                        ..*n
                    })
                    .collect();
                let key = app.gestures.key(resp.id);
                let r = actions::update_notes(
                    &app.session,
                    clip.id,
                    updated,
                    "Change velocity",
                    Some(key),
                );
                app.report(r);
            }
        }
        if ui
            .button("Fit")
            .on_hover_text("Show the whole clip")
            .clicked()
        {
            refit = true;
        }
        ui.checkbox(&mut app.audition.hear_notes, "Hear notes")
            .on_hover_text("Play notes as you add, press or move them");
        ui.label(
            RichText::new(format!(
                "{} note{} · {} selected",
                notes.len(),
                if notes.len() == 1 { "" } else { "s" },
                selected.len()
            ))
            .small()
            .color(theme::TEXT_DIM),
        );
        ui.label(
            RichText::new(
                "Click to add · drag to move · drag the end to resize · right-click deletes \
                 · press the keys to hear them",
            )
            .small()
            .color(theme::TEXT_DIM),
        );
    });

    let full = ui.available_rect_before_wrap();
    if full.width() < KEYS_W + 40.0 || full.height() < RULER_H + 30.0 {
        return;
    }
    let grid_rect = Rect::from_min_max(pos2(full.left() + KEYS_W, full.top() + RULER_H), full.max);
    let rows = grid_rect.height() / app.piano.row_h;
    if refit {
        fit(&mut app.piano, clip, notes, grid_rect.width(), rows);
        app.piano.clip = Some(clip.id);
        app.piano.drag = None;
    }
    input(
        app,
        ui,
        Rect::from_min_max(pos2(full.left(), full.top()), full.max),
        grid_rect,
        rows,
    );

    let view = View {
        grid: grid_rect,
        left: grid_rect.left(),
        scroll: app.piano.scroll,
        ppt: app.piano.px_per_tick,
        top_pitch: app.piano.top_pitch,
        row_h: app.piano.row_h,
    };
    update_drag(app, ui.ctx(), &view, project, track, clip);

    // The keys play the track's instrument while pressed; dragging along
    // them plays each key passed.
    let keys_rect = Rect::from_min_max(
        pos2(full.left(), grid_rect.top()),
        pos2(grid_rect.left(), full.bottom()),
    );
    let keys = ui
        .interact(
            keys_rect,
            Id::new(("piano_keys", clip.id)),
            Sense::click_and_drag(),
        )
        .on_hover_text("Press to hear the track's instrument");
    if keys.is_pointer_button_down_on()
        && let Some(p) = ui.input(|i| i.pointer.latest_pos())
    {
        let pitch = view.pitch_at(p.y).floor().clamp(0.0, 127.0) as u8;
        app.hold_key(track.id, Some(pitch), app.piano.velocity);
    }
    let held = app
        .audition
        .held
        .filter(|(t, _)| *t == track.id)
        .map(|(_, p)| p);

    let painter = ui.painter_at(full);
    draw_background(&painter, &view, project, clip);

    // Background interaction first so notes on top take precedence.
    let bg = ui.interact(grid_rect, Id::new(("piano_bg", clip.id)), Sense::click());
    if bg.clicked()
        && let Some(p) = bg.interact_pointer_pos()
    {
        app.sel.focus = Focus::Editor;
        let free = ui.input(|i| i.modifiers.alt);
        add_note_at(app, project, track, clip, &view, p, free);
    }

    let color = theme::track_color(track.color);
    let drag = app.piano.drag.clone();
    let shown: Vec<Note> = match &drag {
        Some(d) => notes
            .iter()
            .map(|n| {
                d.preview
                    .iter()
                    .find(|p| p.id == n.id)
                    .copied()
                    .unwrap_or(*n)
            })
            .collect(),
        None => notes.to_vec(),
    };
    let note_painter = painter.with_clip_rect(grid_rect.intersect(painter.clip_rect()));
    for n in &shown {
        let r = view.note_rect(n);
        if !r.intersects(grid_rect) {
            continue;
        }
        let selected = app.sel.notes.contains(&n.id);
        draw_note(&note_painter, r, n, color, selected);
        if drag.is_some() {
            continue;
        }
        let vis = r.intersect(grid_rect);
        let id = Id::new(("note", clip.id, n.id));
        let body = ui.interact(vis, id, Sense::click_and_drag());
        let edge_rect = Rect::from_min_max(
            pos2((r.right() - EDGE).max(r.left() + 2.0), r.top()),
            pos2(r.right() + 1.0, r.bottom()),
        )
        .intersect(grid_rect);
        let edge = ui.interact(edge_rect, id.with("edge"), Sense::drag());
        if edge.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        } else if body.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
        body.clone().on_hover_text(format!(
            "{} · velocity {} · {} ticks",
            note_name(n.pitch),
            n.velocity,
            n.length
        ));
        // Heard as the button goes down, so a click and a drag both sound.
        if body.hovered() && ui.input(|i| i.pointer.primary_pressed()) {
            let seconds = audition::note_seconds(project, clip.start + n.start, n.length);
            app.hear_notes(track.id, &[(n.pitch, n.velocity)], seconds);
        }
        if body.secondary_clicked() {
            app.sel.focus = Focus::Editor;
            app.sel.notes.retain(|id| *id != n.id);
            let r = actions::remove_notes(&app.session, clip.id, vec![n.id]);
            app.report(r);
            continue;
        }
        if body.clicked() {
            app.sel.focus = Focus::Editor;
            let shift = ui.input(|i| i.modifiers.shift);
            if shift {
                if let Some(i) = app.sel.notes.iter().position(|id| *id == n.id) {
                    app.sel.notes.remove(i);
                } else {
                    app.sel.notes.push(n.id);
                }
            } else {
                app.sel.notes = vec![n.id];
            }
        }
        let origin = ui.input(|i| i.pointer.press_origin());
        let kind = if edge.drag_started() {
            Some(NoteDragKind::Resize)
        } else if body.drag_started() {
            Some(NoteDragKind::Move)
        } else {
            None
        };
        if let (Some(kind), Some(origin)) = (kind, origin) {
            app.sel.focus = Focus::Editor;
            if !app.sel.notes.contains(&n.id) {
                app.sel.notes = vec![n.id];
            }
            let orig: Vec<Note> = notes
                .iter()
                .filter(|m| app.sel.notes.contains(&m.id))
                .copied()
                .collect();
            app.piano.drag = Some(NoteDrag {
                kind,
                anchor: n.id,
                preview: orig.clone(),
                orig,
                grab_tick: view.tick_at(origin.x),
                grab_pitch: view.pitch_at(origin.y),
                heard: 0,
            });
        }
    }

    // Playhead inside the clip.
    let rel = app.transport.position_ticks - clip.start as f64;
    if rel >= 0.0 && rel <= clip.length as f64 {
        let x = view.x_of(rel);
        if grid_rect.x_range().contains(x) {
            painter.vline(x, grid_rect.y_range(), Stroke::new(1.0, theme::ACCENT));
        }
    }
    draw_keys(&painter, &view, keys_rect, held);
    draw_ruler(
        &painter,
        &view,
        Rect::from_min_max(
            pos2(grid_rect.left(), full.top()),
            pos2(full.right(), grid_rect.top()),
        ),
        project,
        clip,
    );
    painter.rect_filled(
        Rect::from_min_max(full.min, pos2(grid_rect.left(), grid_rect.top())),
        CornerRadius::ZERO,
        theme::HEADER,
    );
    ui.advance_cursor_after_rect(full);
}

fn input(app: &mut App, ui: &mut Ui, area: Rect, grid: Rect, rows: f32) {
    if !ui.rect_contains_pointer(area) {
        return;
    }
    let (pos, zoom, delta) =
        ui.input(|i| (i.pointer.hover_pos(), i.zoom_delta(), i.smooth_scroll_delta));
    let st = &mut app.piano;
    if zoom != 1.0
        && let Some(p) = pos
    {
        let x = p.x.max(grid.left());
        let anchor = st.scroll + ((x - grid.left()) / st.px_per_tick) as f64;
        st.px_per_tick = (st.px_per_tick * zoom).clamp(0.005, 4.0);
        st.scroll = (anchor - ((x - grid.left()) / st.px_per_tick) as f64).max(0.0);
    }
    if delta != egui::Vec2::ZERO {
        st.top_pitch = (st.top_pitch + delta.y / st.row_h).clamp(rows.min(128.0), 128.0);
        st.scroll = (st.scroll - (delta.x / st.px_per_tick) as f64).max(0.0);
        ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
    }
}

fn add_note_at(
    app: &mut App,
    project: &Project,
    track: &Track,
    clip: &Clip,
    view: &View,
    p: egui::Pos2,
    free: bool,
) {
    let tick = view.tick_at(p.x);
    if tick < 0.0 || tick >= clip.length as f64 {
        app.sel.notes.clear();
        return;
    }
    let pitch = view.pitch_at(p.y).floor();
    if !(0.0..=127.0).contains(&pitch) {
        return;
    }
    let grid = app.piano.grid.ticks();
    let start = snap_floor(tick, if free { None } else { Some(grid) }).max(0);
    let length = grid.min(clip.length - start).max(1);
    let note = Note::new(start, length, pitch as u8, app.piano.velocity.clamp(1, 127));
    let r = actions::add_note(&app.session, clip.id, note);
    if let Some(id) = app.report(r) {
        app.sel.notes = vec![id];
        let seconds = audition::note_seconds(project, clip.start + start, length);
        app.hear_notes(track.id, &[(note.pitch, note.velocity)], seconds);
    }
}

fn update_drag(
    app: &mut App,
    ctx: &Context,
    view: &View,
    project: &Project,
    track: &Track,
    clip: &Clip,
) {
    let Some(mut drag) = app.piano.drag.clone() else {
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
        app.piano.drag = None;
        return;
    }
    let grid = if alt {
        None
    } else {
        Some(app.piano.grid.ticks())
    };
    let Some(anchor) = drag.orig.iter().find(|n| n.id == drag.anchor).copied() else {
        app.piano.drag = None;
        return;
    };
    if let Some(p) = pos {
        let tick = view.tick_at(p.x);
        match drag.kind {
            NoteDragKind::Move => {
                let new_start = snap_round(anchor.start as f64 + (tick - drag.grab_tick), grid);
                let min_start = drag.orig.iter().map(|n| n.start).min().unwrap_or(0);
                let dt = (new_start - anchor.start).max(-min_start);
                let dp = (view.pitch_at(p.y) - drag.grab_pitch).round() as i32;
                let lo = drag.orig.iter().map(|n| n.pitch as i32).min().unwrap_or(0);
                let hi = drag
                    .orig
                    .iter()
                    .map(|n| n.pitch as i32)
                    .max()
                    .unwrap_or(127);
                let dp = dp.clamp(-lo, 127 - hi);
                drag.preview = drag
                    .orig
                    .iter()
                    .map(|n| Note {
                        start: n.start + dt,
                        pitch: (n.pitch as i32 + dp) as u8,
                        ..*n
                    })
                    .collect();
                if dp != drag.heard {
                    drag.heard = dp;
                    let notes: Vec<(u8, u8)> =
                        drag.preview.iter().map(|n| (n.pitch, n.velocity)).collect();
                    let seconds = audition::note_seconds(
                        project,
                        clip.start + anchor.start + dt,
                        anchor.length,
                    );
                    app.hear_notes(track.id, &notes, seconds);
                }
                ctx.set_cursor_icon(CursorIcon::Grabbing);
            }
            NoteDragKind::Resize => {
                let min_len = grid.unwrap_or(PPQ / 32).max(1);
                let end = snap_round(
                    (anchor.start + anchor.length) as f64 + (tick - drag.grab_tick),
                    grid,
                );
                let dl = (end - anchor.start).max(min_len) - anchor.length;
                drag.preview = drag
                    .orig
                    .iter()
                    .map(|n| Note {
                        length: (n.length + dl).max(min_len),
                        ..*n
                    })
                    .collect();
                ctx.set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
        }
    }
    if down {
        app.piano.drag = Some(drag);
        return;
    }
    app.piano.drag = None;
    if drag.preview != drag.orig {
        let label = match drag.kind {
            NoteDragKind::Move => "Move notes",
            NoteDragKind::Resize => "Resize notes",
        };
        let r = actions::update_notes(&app.session, clip.id, drag.preview, label, None);
        app.report(r);
    }
}

fn draw_background(p: &Painter, v: &View, project: &Project, clip: &Clip) {
    let g = v.grid;
    p.rect_filled(g, CornerRadius::ZERO, theme::LANE);
    let first = (v.top_pitch.ceil() as i32 - 1).min(127);
    let last = (v.pitch_at(g.bottom()).floor() as i32).max(0);
    let mut pitch = first;
    while pitch >= last {
        let pu = pitch as u8;
        let y = v.y_of(pu);
        if is_black(pu) {
            p.rect_filled(
                Rect::from_min_max(pos2(g.left(), y), pos2(g.right(), y + v.row_h)),
                CornerRadius::ZERO,
                theme::darken(theme::LANE, 0.25),
            );
        }
        if pu.is_multiple_of(12) {
            p.hline(g.x_range(), y + v.row_h, Stroke::new(1.0, theme::GRID_BAR));
        }
        pitch -= 1;
    }
    // Time grid: sixteenths, beats and bars.
    let tpb = project.ticks_per_bar().max(1);
    let beat = project.time_signature.ticks_per_beat().max(1);
    let from = v.tick_at(g.left()).max(0.0) as i64;
    let to = v.tick_at(g.right()).max(0.0) as i64 + 1;
    let step = if (PPQ / 4) as f32 * v.ppt >= 8.0 {
        PPQ / 4
    } else if beat as f32 * v.ppt >= 8.0 {
        beat
    } else {
        tpb
    };
    let mut t = from / step * step;
    let mut n = 0;
    while t <= to && n < 4000 {
        let abs = clip.start + t;
        let c = if abs % tpb == 0 {
            theme::GRID_BAR
        } else if abs % beat == 0 {
            theme::GRID_BEAT
        } else {
            theme::GRID_FAINT
        };
        p.vline(v.x_of(t as f64), g.y_range(), Stroke::new(1.0, c));
        t += step;
        n += 1;
    }
    // Past the clip end.
    let end_x = v.x_of(clip.length as f64);
    if end_x < g.right() {
        p.rect_filled(
            Rect::from_min_max(pos2(end_x.max(g.left()), g.top()), g.max),
            CornerRadius::ZERO,
            Color32::from_black_alpha(110),
        );
        p.vline(end_x, g.y_range(), Stroke::new(1.5, theme::TEXT_DIM));
    }
}

fn draw_note(p: &Painter, r: Rect, n: &Note, color: Color32, selected: bool) {
    // Brighter means louder.
    let base = theme::lighten(color, 0.25);
    let fill = theme::darken(base, (1.0 - n.velocity as f32 / 127.0) * 0.65);
    p.rect_filled(r, CornerRadius::same(2), fill);
    let stroke = if selected {
        Stroke::new(1.5, theme::ACCENT)
    } else {
        Stroke::new(1.0, theme::darken(color, 0.5))
    };
    p.rect_stroke(r, CornerRadius::same(2), stroke, StrokeKind::Inside);
    if r.width() > 34.0 && r.height() >= 9.0 {
        p.with_clip_rect(r.intersect(p.clip_rect())).text(
            pos2(r.left() + 3.0, r.center().y),
            Align2::LEFT_CENTER,
            note_name(n.pitch),
            egui::FontId::proportional((r.height() - 2.0).min(10.5)),
            Color32::from_black_alpha(200),
        );
    }
}

fn draw_keys(p: &Painter, v: &View, rect: Rect, held: Option<u8>) {
    let p = p.with_clip_rect(rect.intersect(p.clip_rect()));
    p.rect_filled(rect, CornerRadius::ZERO, Color32::from_gray(200));
    let first = (v.top_pitch.ceil() as i32 - 1).min(127);
    let last = (v.pitch_at(rect.bottom()).floor() as i32).max(0);
    let mut pitch = first;
    while pitch >= last {
        let pu = pitch as u8;
        let y = v.y_of(pu);
        let row = Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), y + v.row_h));
        if held == Some(pu) {
            p.rect_filled(row, CornerRadius::ZERO, theme::ACCENT);
        } else if is_black(pu) {
            p.rect_filled(
                Rect::from_min_max(
                    row.min,
                    pos2(rect.left() + rect.width() * 0.62, row.bottom()),
                ),
                CornerRadius::ZERO,
                Color32::from_gray(40),
            );
        } else {
            p.hline(
                rect.x_range(),
                row.bottom(),
                Stroke::new(1.0, Color32::from_gray(150)),
            );
        }
        if pu.is_multiple_of(12) {
            p.text(
                pos2(rect.right() - 3.0, row.center().y),
                Align2::RIGHT_CENTER,
                note_name(pu),
                egui::FontId::proportional(10.0),
                Color32::from_gray(60),
            );
        }
        pitch -= 1;
    }
    p.vline(
        rect.right() - 0.5,
        rect.y_range(),
        Stroke::new(1.0, theme::BG),
    );
}

fn draw_ruler(p: &Painter, v: &View, rect: Rect, project: &Project, clip: &Clip) {
    let p = p.with_clip_rect(rect.intersect(p.clip_rect()));
    p.rect_filled(rect, CornerRadius::ZERO, theme::HEADER);
    let tpb = project.ticks_per_bar().max(1);
    let beat = project.time_signature.ticks_per_beat().max(1);
    let from = v.tick_at(rect.left()).max(0.0) as i64;
    let to = v.tick_at(rect.right()).max(0.0) as i64 + 1;
    let step = if beat as f32 * v.ppt >= 36.0 {
        beat
    } else {
        tpb
    };
    let mut t = from / step * step;
    let mut n = 0;
    while t <= to && n < 2000 {
        let x = v.x_of(t as f64);
        let abs = clip.start + t;
        p.vline(x, rect.y_range(), Stroke::new(1.0, theme::GRID_BAR));
        let label = if abs % tpb == 0 {
            format!("{}", abs / tpb + 1)
        } else {
            project
                .time_signature
                .format_position(abs)
                .rsplit_once('.')
                .map_or_else(String::new, |(head, _)| head.to_string())
        };
        p.text(
            pos2(x + 3.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            theme::small(),
            theme::TEXT_WEAK,
        );
        t += step;
        n += 1;
    }
}
