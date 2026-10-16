// SPDX-License-Identifier: GPL-3.0-or-later
//! The marker and tempo lanes under the bar ruler.
//!
//! Markers are named places in the song: click one to go there, drag it to
//! move it. The tempo lane draws the tempo map as steps; each point sets
//! the tempo from there on and drags up, down and along.

use egui::{
    Align2, Color32, Context, CornerRadius, CursorIcon, DragValue, Id, Key, Pos2, Rect, Sense,
    Shape, Stroke, StrokeKind, TextEdit, Ui, pos2, vec2,
};
use serde::{Deserialize, Serialize};
use sky_core::{MarkerId, TempoMap, TempoPoint, Ticks};
use sky_model::{Command, Marker, Project, Transaction};

use crate::actions;
use crate::app::App;
use crate::arrange::{self, snap_round};
use crate::theme;

pub const MARKER_H: f32 = 18.0;
pub const TEMPO_H: f32 = 34.0;

/// A lane under the ruler that can be shown or hidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lane {
    Markers,
    Tempo,
}

impl Lane {
    pub fn label(self) -> &'static str {
        match self {
            Lane::Markers => "Marker lane",
            Lane::Tempo => "Tempo lane",
        }
    }
}

/// Which lanes show under the ruler, saved with the panel layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Lanes {
    pub markers: bool,
    pub tempo: bool,
}

impl Default for Lanes {
    fn default() -> Self {
        Self {
            markers: true,
            tempo: true,
        }
    }
}

impl Lanes {
    pub fn shows(self, lane: Lane) -> bool {
        match lane {
            Lane::Markers => self.markers,
            Lane::Tempo => self.tempo,
        }
    }

    pub fn toggle(&mut self, lane: Lane) {
        match lane {
            Lane::Markers => self.markers = !self.markers,
            Lane::Tempo => self.tempo = !self.tempo,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MarkerDrag {
    pub id: MarkerId,
    pub grab_tick: f64,
    pub orig: Ticks,
    pub tick: Ticks,
}

/// A tempo point being dragged; the map is applied on release.
#[derive(Clone, Debug)]
pub struct TempoDrag {
    pub index: usize,
    pub grab: Pos2,
    pub orig: TempoMap,
    pub preview: TempoMap,
    /// The lane's tempo range when the drag began, kept so it does not
    /// rescale under the pointer.
    pub range: (f64, f64),
    pub bpm_per_px: f64,
}

#[derive(Default)]
pub struct RulerLanes {
    pub marker_drag: Option<MarkerDrag>,
    /// The marker last clicked.
    pub marker: Option<MarkerId>,
    /// A marker whose name is being edited.
    pub renaming: Option<(MarkerId, String)>,
    rename_focus: bool,
    pub tempo_drag: Option<TempoDrag>,
}

pub const MARKER: Color32 = Color32::from_rgb(236, 128, 176);
pub const TEMPO: Color32 = Color32::from_rgb(176, 146, 255);

/// "Marker 3", the first such name no marker has.
fn next_name(project: &Project) -> String {
    (1..)
        .map(|n| format!("Marker {n}"))
        .find(|n| !project.markers.iter().any(|m| &m.name == n))
        .unwrap_or_default()
}

/// "128" or "127.5".
pub fn bpm_text(bpm: f64) -> String {
    let s = format!("{bpm:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

impl App {
    /// Adds a marker at `tick`, or picks the one already there.
    pub fn add_marker(&mut self, tick: Ticks) -> Option<MarkerId> {
        let project = self.view.project.clone();
        if let Some(m) = project.markers.iter().find(|m| m.tick == tick) {
            self.arrange.ruler.marker = Some(m.id);
            return Some(m.id);
        }
        let marker = Marker {
            id: MarkerId::new(),
            tick: tick.max(0),
            name: next_name(&project),
        };
        let id = marker.id;
        let tx = Transaction::single(Command::AddMarker { marker });
        self.report(actions::apply(&self.session, tx, None))?;
        self.arrange.ruler.marker = Some(id);
        self.workspace.lanes.markers = true;
        self.refresh();
        Some(id)
    }

    /// Transport > Add a marker at the playhead.
    pub fn add_marker_at_playhead(&mut self) {
        let tick = self.transport.position_ticks.max(0.0).round() as Ticks;
        self.add_marker(tick);
    }

    /// Puts a changed marker in place of the one with its ID.
    fn replace_marker(&mut self, marker: Marker, label: &str) {
        let tx = Transaction::user(
            label,
            vec![
                Command::RemoveMarker { id: marker.id },
                Command::AddMarker { marker },
            ],
        );
        let r = actions::apply(&self.session, tx, None);
        self.report(r);
        self.refresh();
    }

    pub fn remove_marker(&mut self, id: MarkerId) {
        let tx = Transaction::user("Delete marker", vec![Command::RemoveMarker { id }]);
        let r = actions::apply(&self.session, tx, None);
        if self.report(r).is_some() && self.arrange.ruler.marker == Some(id) {
            self.arrange.ruler.marker = None;
        }
        self.refresh();
    }

    /// Replaces the tempo map, merged into one undo step with the last
    /// change of the same `key`.
    pub fn set_tempo_map(&mut self, map: TempoMap, key: Option<u64>) {
        let tx = Transaction::single(Command::SetTempoMap { map });
        let r = actions::apply(&self.session, tx, key);
        self.report(r);
        self.refresh();
    }

    /// Sets the tempo from `tick` on, replacing a change already there.
    pub fn add_tempo_change(&mut self, tick: Ticks, bpm: f64) {
        let mut points = self.view.project.tempo.points().to_vec();
        points.retain(|p| p.tick != tick.max(0));
        points.push(TempoPoint {
            tick: tick.max(0),
            bpm,
        });
        match TempoMap::from_points(points) {
            Ok(map) => self.set_tempo_map(map, None),
            Err(e) => self.error(e.to_string()),
        }
    }
}

/// The tempo range the lane shows: the map's, with room above and below.
fn range_of(map: &TempoMap) -> (f64, f64) {
    let (lo, hi) = map
        .points()
        .iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), p| {
            (lo.min(p.bpm), hi.max(p.bpm))
        });
    let pad = ((hi - lo) * 0.2).max(8.0);
    ((lo - pad).max(0.0), hi + pad)
}

fn corner(ui: &Ui, rect: Rect, text: String) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, theme::HEADER);
    painter.text(
        pos2(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        theme::small(),
        theme::TEXT_WEAK,
    );
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, theme::BG),
    );
}

fn background(app: &App, ui: &Ui, rect: Rect, project: &Project) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, theme::PANEL);
    arrange::draw_grid(&painter, &app.arrange, rect, project);
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, theme::BG),
    );
}

/// The grid of a drag: off while Alt is held.
fn grid(app: &App, ctx: &Context, project: &Project) -> Option<Ticks> {
    if ctx.input(|i| i.modifiers.alt) {
        None
    } else {
        app.arrange.snap.grid(project.time_signature)
    }
}

/// Markers as they stand, the one being dragged where it is going.
fn placed_markers(app: &App, project: &Project) -> Vec<Marker> {
    let drag = app.arrange.ruler.marker_drag;
    let mut out: Vec<Marker> = project
        .markers
        .iter()
        .map(|m| match drag {
            Some(d) if d.id == m.id => Marker {
                tick: d.tick,
                ..m.clone()
            },
            _ => m.clone(),
        })
        .collect();
    out.sort_by_key(|m| m.tick);
    out
}

/// Draws the markers as faint lines over a lane of clips.
pub fn marker_lines(app: &App, painter: &egui::Painter, rect: Rect, project: &Project) {
    if !app.workspace.lanes.markers {
        return;
    }
    for m in placed_markers(app, project) {
        let x = app.arrange.x_of(m.tick as f64);
        if rect.x_range().contains(x) {
            painter.vline(
                x,
                rect.y_range(),
                Stroke::new(1.0, MARKER.gamma_multiply(0.3)),
            );
        }
    }
}

/// The marker lane and its corner label.
pub fn markers_ui(app: &mut App, ui: &mut Ui, corner_rect: Rect, rect: Rect, project: &Project) {
    corner(ui, corner_rect, "Markers".into());
    background(app, ui, rect, project);
    let resp = ui
        .interact(rect, Id::new("marker_lane"), Sense::click_and_drag())
        .on_hover_text("Double-click to add a marker, or press M to add one at the playhead");
    if resp.clicked() {
        app.arrange.ruler.marker = None;
    }
    if resp.double_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let tick = snap_round(app.arrange.tick_at(p.x), grid(app, ui.ctx(), project)).max(0);
        app.add_marker(tick);
    }
    if resp.secondary_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        app.arrange.context_tick = app.arrange.tick_at(p.x).max(0.0);
    }
    resp.context_menu(|ui| {
        if ui.button("Add a marker here").clicked() {
            let grid = app.arrange.snap.grid(project.time_signature);
            app.add_marker(snap_round(app.arrange.context_tick, grid).max(0));
        }
    });

    let painter = ui.painter_at(rect);
    let markers = placed_markers(app, project);
    for (i, m) in markers.iter().enumerate() {
        let x = app.arrange.x_of(m.tick as f64);
        let next_x = markers
            .get(i + 1)
            .map_or(f32::INFINITY, |n| app.arrange.x_of(n.tick as f64));
        if x > rect.right() || next_x < rect.left() {
            continue;
        }
        let galley = painter.layout_no_wrap(m.name.clone(), theme::small(), Color32::WHITE);
        let w = (galley.size().x + 10.0).min((next_x - x - 2.0).max(12.0));
        let flag = Rect::from_min_max(pos2(x, rect.top() + 2.0), pos2(x + w, rect.bottom() - 2.0));
        let chosen = app.arrange.ruler.marker == Some(m.id);
        painter.vline(x, rect.y_range(), Stroke::new(1.5, MARKER));
        painter.rect_filled(
            flag,
            CornerRadius {
                nw: 0,
                ne: 3,
                sw: 0,
                se: 3,
            },
            if chosen {
                MARKER
            } else {
                MARKER.gamma_multiply(0.6)
            },
        );
        if chosen {
            painter.rect_stroke(
                flag,
                CornerRadius::same(1),
                Stroke::new(1.0, Color32::WHITE),
                StrokeKind::Inside,
            );
        }
        painter.with_clip_rect(flag.intersect(rect)).galley(
            pos2(flag.left() + 5.0, flag.center().y - galley.size().y * 0.5),
            galley,
            Color32::WHITE,
        );

        if app
            .arrange
            .ruler
            .renaming
            .as_ref()
            .is_some_and(|(id, _)| *id == m.id)
        {
            rename_ui(app, ui, flag, m);
            continue;
        }
        let hit = flag.union(Rect::from_min_max(
            pos2(x - 4.0, rect.top()),
            pos2(x + 4.0, rect.bottom()),
        ));
        let r = ui
            .interact(
                hit.intersect(rect),
                Id::new(("marker", m.id)),
                Sense::click_and_drag(),
            )
            .on_hover_text(format!(
                "{} at {}: click to go there, drag to move, double-click to rename",
                m.name,
                project.time_signature.format_position(m.tick)
            ));
        if r.hovered() && app.arrange.ruler.marker_drag.is_none() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
        if r.clicked() {
            app.arrange.ruler.marker = Some(m.id);
            arrange::seek(app, m.tick);
        }
        if r.double_clicked() {
            app.arrange.ruler.renaming = Some((m.id, m.name.clone()));
            app.arrange.ruler.rename_focus = true;
        }
        if r.drag_started() {
            let grab = ui
                .input(|i| i.pointer.press_origin())
                .map_or(m.tick as f64, |p| app.arrange.tick_at(p.x));
            app.arrange.ruler.marker = Some(m.id);
            app.arrange.ruler.marker_drag = Some(MarkerDrag {
                id: m.id,
                grab_tick: grab,
                orig: m.tick,
                tick: m.tick,
            });
        }
        let next = markers.get(i + 1).map(|n| n.tick);
        r.context_menu(|ui| {
            if ui.button("Rename").clicked() {
                app.arrange.ruler.renaming = Some((m.id, m.name.clone()));
                app.arrange.ruler.rename_focus = true;
            }
            if let Some(end) = next
                && ui.button("Loop to the next marker").clicked()
            {
                let r = actions::set_loop_region(&app.session, m.tick, end);
                app.report(r);
            }
            if ui
                .button(egui::RichText::new("Delete").color(theme::ERROR))
                .clicked()
            {
                app.remove_marker(m.id);
            }
        });
    }
}

/// The name field over a marker being renamed: Enter or a click elsewhere
/// keeps the name, Escape drops it.
fn rename_ui(app: &mut App, ui: &mut Ui, flag: Rect, marker: &Marker) {
    let mut done = None;
    if let Some((_, text)) = app.arrange.ruler.renaming.as_mut() {
        let rect = Rect::from_min_size(flag.min, vec2(flag.width().max(140.0), flag.height()));
        let resp = ui.put(
            rect,
            TextEdit::singleline(text)
                .font(theme::small())
                .margin(vec2(4.0, 0.0))
                .id(Id::new(("rename_marker", marker.id))),
        );
        if app.arrange.ruler.rename_focus {
            resp.request_focus();
            app.arrange.ruler.rename_focus = false;
        }
        if resp.lost_focus() {
            let cancelled = ui.input(|i| i.key_pressed(Key::Escape));
            done = Some((!cancelled).then(|| text.trim().to_string()));
        }
    }
    if let Some(result) = done {
        app.arrange.ruler.renaming = None;
        if let Some(name) = result.filter(|n| !n.is_empty() && *n != marker.name) {
            app.replace_marker(
                Marker {
                    name,
                    ..marker.clone()
                },
                "Rename marker",
            );
        }
    }
}

/// Follows a marker drag, and moves it on release.
pub fn update_marker_drag(app: &mut App, ctx: &Context, project: &Project) {
    let Some(mut d) = app.arrange.ruler.marker_drag.take() else {
        return;
    };
    let Some(marker) = project.markers.iter().find(|m| m.id == d.id).cloned() else {
        return;
    };
    let (pos, down, escape) = ctx.input(|i| {
        (
            i.pointer.latest_pos(),
            i.pointer.primary_down(),
            i.key_pressed(Key::Escape),
        )
    });
    if escape {
        return;
    }
    if let Some(p) = pos {
        let dt = app.arrange.tick_at(p.x) - d.grab_tick;
        d.tick = snap_round(d.orig as f64 + dt, grid(app, ctx, project)).max(0);
        ctx.set_cursor_icon(CursorIcon::Grabbing);
    }
    if down {
        app.arrange.ruler.marker_drag = Some(d);
        return;
    }
    if d.tick != d.orig {
        app.replace_marker(
            Marker {
                tick: d.tick,
                ..marker
            },
            "Move marker",
        );
    }
}

/// The tempo lane and its corner label.
pub fn tempo_ui(app: &mut App, ui: &mut Ui, corner_rect: Rect, rect: Rect, project: &Project) {
    let here = project
        .tempo
        .bpm_at(app.transport.position_ticks.max(0.0) as Ticks);
    corner(ui, corner_rect, format!("Tempo {} BPM", bpm_text(here)));
    background(app, ui, rect, project);
    let drag = app.arrange.ruler.tempo_drag.clone();
    let map = drag.as_ref().map_or(&project.tempo, |d| &d.preview).clone();
    let (lo, hi) = drag.as_ref().map_or_else(|| range_of(&map), |d| d.range);
    let inner = rect.shrink2(vec2(0.0, 6.0));
    let span = (hi - lo).max(1.0);
    let y_of = |bpm: f64| inner.bottom() - ((bpm - lo) / span) as f32 * inner.height();
    let bpm_at_y = |y: f32| lo + f64::from((inner.bottom() - y) / inner.height().max(1.0)) * span;

    let resp = ui
        .interact(rect, Id::new("tempo_lane"), Sense::click_and_drag())
        .on_hover_text("Double-click to change the tempo from there on");
    if resp.double_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let tick = snap_round(app.arrange.tick_at(p.x), grid(app, ui.ctx(), project)).max(0);
        app.add_tempo_change(tick, bpm_at_y(p.y).round().clamp(10.0, 999.0));
    }
    if resp.secondary_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        app.arrange.context_tick = app.arrange.tick_at(p.x).max(0.0);
    }
    resp.context_menu(|ui| {
        if ui.button("Add a tempo change here").clicked() {
            let grid = app.arrange.snap.grid(project.time_signature);
            let tick = snap_round(app.arrange.context_tick, grid).max(0);
            app.add_tempo_change(tick, project.tempo.bpm_at(tick));
        }
        if project.tempo.points().len() > 1
            && ui
                .button("Keep the starting tempo throughout")
                .on_hover_text("Removes every tempo change after the start")
                .clicked()
        {
            app.set_tempo_map(TempoMap::constant(project.tempo.initial_bpm()), None);
        }
    });

    // The steps.
    let painter = ui.painter_at(rect);
    let points = map.points();
    let mut line = Vec::new();
    for (i, p) in points.iter().enumerate() {
        let x0 = app.arrange.x_of(p.tick as f64).max(rect.left() - 2.0);
        let x1 = points
            .get(i + 1)
            .map_or(rect.right() + 2.0, |n| app.arrange.x_of(n.tick as f64))
            .min(rect.right() + 2.0);
        if x1 < rect.left() - 2.0 || x0 > rect.right() + 2.0 {
            continue;
        }
        line.push(pos2(x0, y_of(p.bpm)));
        line.push(pos2(x1, y_of(p.bpm)));
    }
    painter.add(Shape::line(line, Stroke::new(1.5, TEMPO)));

    // The points, with their tempo.
    for (i, p) in points.iter().enumerate() {
        let at = pos2(app.arrange.x_of(p.tick as f64), y_of(p.bpm));
        if !rect.expand(4.0).contains(at) {
            continue;
        }
        let dragged = drag.as_ref().is_some_and(|d| d.index == i);
        let hit = Rect::from_center_size(at, vec2(12.0, 12.0)).intersect(rect);
        let r = ui
            .interact(hit, Id::new(("tempo_point", i)), Sense::click_and_drag())
            .on_hover_text(format!(
                "{} BPM from {}: drag to change, right-click to type",
                bpm_text(p.bpm),
                project.time_signature.format_position(p.tick)
            ));
        let lit = dragged || r.hovered();
        painter.circle(
            at,
            if lit { 4.5 } else { 3.5 },
            if lit { Color32::WHITE } else { TEMPO },
            Stroke::new(1.0, theme::BG),
        );
        let above = at.y - 13.0 > rect.top();
        painter.text(
            pos2(at.x + 6.0, if above { at.y - 2.0 } else { at.y + 2.0 }),
            if above {
                Align2::LEFT_BOTTOM
            } else {
                Align2::LEFT_TOP
            },
            bpm_text(p.bpm),
            theme::small(),
            if lit {
                Color32::WHITE
            } else {
                theme::TEXT_WEAK
            },
        );
        if r.hovered() && drag.is_none() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
        if r.drag_started() {
            let grab = ui.input(|i| i.pointer.press_origin()).unwrap_or(at);
            app.arrange.ruler.tempo_drag = Some(TempoDrag {
                index: i,
                grab,
                orig: project.tempo.clone(),
                preview: project.tempo.clone(),
                range: (lo, hi),
                bpm_per_px: span / f64::from(inner.height().max(1.0)),
            });
        }
        r.context_menu(|ui| {
            let mut bpm = p.bpm;
            let field = ui.add(
                DragValue::new(&mut bpm)
                    .range(10.0..=999.0)
                    .speed(0.2)
                    .max_decimals(2)
                    .suffix(" BPM"),
            );
            if field.changed() && bpm != p.bpm {
                let mut points = project.tempo.points().to_vec();
                if let Some(q) = points.get_mut(i) {
                    q.bpm = bpm;
                }
                if let Ok(map) = TempoMap::from_points(points) {
                    let key = app.gestures.key(field.id);
                    app.set_tempo_map(map, Some(key));
                }
            }
            if i > 0
                && ui
                    .button(egui::RichText::new("Delete").color(theme::ERROR))
                    .clicked()
            {
                let mut points = project.tempo.points().to_vec();
                points.remove(i);
                if let Ok(map) = TempoMap::from_points(points) {
                    app.set_tempo_map(map, None);
                }
                ui.close();
            }
        });
    }
}

/// Follows a tempo point drag: along to move it (never past its
/// neighbours, and the first stays at the start), up and down to change its
/// tempo in whole BPM, or tenths with Shift. Applies it on release.
pub fn update_tempo_drag(app: &mut App, ctx: &Context, project: &Project) {
    let Some(mut d) = app.arrange.ruler.tempo_drag.take() else {
        return;
    };
    let points = d.orig.points().to_vec();
    let Some(orig) = points.get(d.index).copied() else {
        return;
    };
    let (pos, down, escape, fine) = ctx.input(|i| {
        (
            i.pointer.latest_pos(),
            i.pointer.primary_down(),
            i.key_pressed(Key::Escape),
            i.modifiers.shift,
        )
    });
    if escape {
        return;
    }
    if let Some(p) = pos {
        let st = &app.arrange;
        let tick = if d.index == 0 || (p.x - d.grab.x).abs() < 3.0 {
            orig.tick
        } else {
            let lo = points[d.index - 1].tick + 1;
            let hi = points.get(d.index + 1).map_or(Ticks::MAX, |n| n.tick - 1);
            let dt = st.tick_at(p.x) - st.tick_at(d.grab.x);
            snap_round(orig.tick as f64 + dt, grid(app, ctx, project)).clamp(lo, hi.max(lo))
        };
        let bpm = if (p.y - d.grab.y).abs() < 3.0 {
            orig.bpm
        } else {
            let step = if fine { 0.1 } else { 1.0 };
            let raw = orig.bpm + f64::from(d.grab.y - p.y) * d.bpm_per_px;
            ((raw / step).round() * step).clamp(10.0, 999.0)
        };
        let mut moved = points.clone();
        moved[d.index] = TempoPoint { tick, bpm };
        if let Ok(map) = TempoMap::from_points(moved) {
            d.preview = map;
        }
        ctx.set_cursor_icon(CursorIcon::Grabbing);
    }
    if down {
        app.arrange.ruler.tempo_drag = Some(d);
        return;
    }
    if d.preview != d.orig {
        app.set_tempo_map(d.preview, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tempo_text_drops_needless_zeros() {
        assert_eq!(bpm_text(128.0), "128");
        assert_eq!(bpm_text(127.5), "127.5");
        assert_eq!(bpm_text(99.25), "99.25");
    }

    #[test]
    fn the_lane_shows_every_tempo_with_room() {
        let map = TempoMap::from_points(vec![
            TempoPoint {
                tick: 0,
                bpm: 120.0,
            },
            TempoPoint {
                tick: 3840,
                bpm: 140.0,
            },
        ])
        .unwrap();
        let (lo, hi) = range_of(&map);
        assert!(lo < 120.0 && hi > 140.0);
        let (lo, hi) = range_of(&TempoMap::constant(128.0));
        assert_eq!((lo, hi), (120.0, 136.0));
    }
}
