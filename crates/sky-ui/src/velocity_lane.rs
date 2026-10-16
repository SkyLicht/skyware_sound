// SPDX-License-Identifier: GPL-3.0-or-later
//! The velocity lane under the piano roll: one stem per note, as tall as
//! the note is loud. Dragging across it sets the velocities the pointer
//! passes over, and Shift-dragging draws a straight ramp.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Id, Key, Painter, Pos2, Rect, Sense, Stroke, Ui,
    pos2,
};
use sky_core::music::note_name;
use sky_core::{ClipId, NoteId};
use sky_model::{Clip, Note};

use crate::actions;
use crate::app::{App, Focus};
use crate::piano_roll::View;
use crate::theme;

/// The lane's height.
pub const VEL_H: f32 = 64.0;

/// Room above and below the tallest and shortest stems.
const PAD: f32 = 4.0;

/// How near a stem the pointer must pass to set it.
const REACH: f32 = 4.0;

/// Velocities being drawn, applied as one undo step on release.
#[derive(Clone, Debug)]
pub struct VelDrag {
    pub clip: ClipId,
    /// The notes the drag may change: the selected ones, or all of them.
    pub orig: Vec<Note>,
    pub preview: Vec<Note>,
    /// Where the button went down.
    pub from: Pos2,
    /// Where the pointer was last frame.
    pub last: Pos2,
    /// Shift was held: a straight line from where the drag began.
    pub line: bool,
}

/// The velocity at a height in the lane.
pub fn velocity_at(lane: Rect, y: f32) -> u8 {
    let h = (lane.height() - 2.0 * PAD).max(1.0);
    let v = (lane.bottom() - PAD - y) / h * 127.0;
    v.round().clamp(1.0, 127.0) as u8
}

/// The height of a velocity in the lane.
pub fn y_of(lane: Rect, velocity: u8) -> f32 {
    let h = (lane.height() - 2.0 * PAD).max(1.0);
    lane.bottom() - PAD - f32::from(velocity) / 127.0 * h
}

/// The velocity a line from `a` to `b` has at `x`.
fn along(lane: Rect, a: Pos2, b: Pos2, x: f32) -> u8 {
    let y = if (b.x - a.x).abs() < f32::EPSILON {
        b.y
    } else {
        let t = ((x - a.x) / (b.x - a.x)).clamp(0.0, 1.0);
        a.y + (b.y - a.y) * t
    };
    velocity_at(lane, y)
}

/// The notes a drag from `a` to `b` passes over, with the velocity it sets
/// each to. `x_of` places a note's stem.
pub fn swept(
    notes: &[Note],
    x_of: impl Fn(&Note) -> f32,
    lane: Rect,
    a: Pos2,
    b: Pos2,
    reach: f32,
) -> Vec<(NoteId, u8)> {
    let (lo, hi) = (a.x.min(b.x) - reach, a.x.max(b.x) + reach);
    notes
        .iter()
        .filter_map(|n| {
            let x = x_of(n);
            (lo..=hi).contains(&x).then(|| (n.id, along(lane, a, b, x)))
        })
        .collect()
}

/// The notes the lane edits: the selected ones in this clip, or all.
fn editable(app: &App, notes: &[Note]) -> Vec<Note> {
    let chosen: Vec<Note> = notes
        .iter()
        .filter(|n| app.sel.notes.contains(&n.id))
        .copied()
        .collect();
    if chosen.is_empty() {
        notes.to_vec()
    } else {
        chosen
    }
}

/// Follows a velocity drag, and applies it on release.
fn update(app: &mut App, ui: &Ui, view: &View, lane: Rect) {
    let Some(mut d) = app.piano.vel_drag.take() else {
        return;
    };
    let (pos, down, escape) = ui.input(|i| {
        (
            i.pointer.latest_pos(),
            i.pointer.primary_down(),
            i.key_pressed(Key::Escape),
        )
    });
    if escape {
        return;
    }
    let x_of = |n: &Note| view.x_of(n.start as f64);
    if let Some(p) = pos {
        if d.line {
            d.preview = d.orig.clone();
            for (id, v) in swept(&d.orig, x_of, lane, d.from, p, 0.0) {
                if let Some(n) = d.preview.iter_mut().find(|n| n.id == id) {
                    n.velocity = v;
                }
            }
        } else {
            for (id, v) in swept(&d.orig, x_of, lane, d.last, p, REACH) {
                if let Some(n) = d.preview.iter_mut().find(|n| n.id == id) {
                    n.velocity = v;
                }
            }
        }
        d.last = p;
        ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
    }
    if down {
        app.piano.vel_drag = Some(d);
        return;
    }
    let changed: Vec<Note> = d
        .preview
        .iter()
        .filter(|n| {
            d.orig
                .iter()
                .any(|o| o.id == n.id && o.velocity != n.velocity)
        })
        .copied()
        .collect();
    if !changed.is_empty() {
        let r = actions::update_notes(&app.session, d.clip, changed, "Change velocity", None);
        app.report(r);
    }
}

/// The lane, under the note grid; `shown` are the notes as drawn above it.
/// `corner` is the space beside it under the keys.
#[allow(clippy::too_many_arguments)]
pub fn show(
    app: &mut App,
    ui: &mut Ui,
    painter: &Painter,
    view: &View,
    lane: Rect,
    corner: Rect,
    clip: &Clip,
    shown: &[Note],
    color: Color32,
) {
    if app
        .piano
        .vel_drag
        .as_ref()
        .is_some_and(|d| d.clip != clip.id)
    {
        app.piano.vel_drag = None;
    }
    update(app, ui, view, lane);

    let p = painter.with_clip_rect(lane.intersect(painter.clip_rect()));
    p.rect_filled(lane, CornerRadius::ZERO, theme::PANEL);
    p.hline(
        lane.x_range(),
        lane.top() + 0.5,
        Stroke::new(1.0, theme::BG),
    );
    for v in [32u8, 64, 96, 127] {
        p.hline(
            lane.x_range(),
            y_of(lane, v),
            Stroke::new(1.0, theme::GRID_FAINT),
        );
    }
    let end_x = view.x_of(clip.length as f64);
    if end_x < lane.right() {
        p.rect_filled(
            Rect::from_min_max(pos2(end_x.max(lane.left()), lane.top()), lane.max),
            CornerRadius::ZERO,
            Color32::from_black_alpha(110),
        );
    }

    // Stems: the drag's preview over what is drawn above.
    let drag = app.piano.vel_drag.clone();
    let editing = editable(app, shown);
    let some_chosen = editing.len() < shown.len();
    for n in shown {
        let velocity = drag
            .as_ref()
            .and_then(|d| d.preview.iter().find(|m| m.id == n.id))
            .map_or(n.velocity, |m| m.velocity);
        let x = view.x_of(n.start as f64);
        if !(lane.left() - 4.0..=lane.right() + 4.0).contains(&x) {
            continue;
        }
        let selected = app.sel.notes.contains(&n.id);
        let c = if selected {
            theme::ACCENT
        } else if some_chosen {
            theme::muted(color)
        } else {
            theme::lighten(color, 0.25)
        };
        let top = y_of(lane, velocity);
        p.line_segment(
            [pos2(x, lane.bottom() - PAD), pos2(x, top)],
            Stroke::new(2.0, c),
        );
        p.circle_filled(pos2(x, top), 3.0, c);
    }

    // Beside it, the lane's name, and the velocity under the pointer.
    let cp = painter.with_clip_rect(corner.intersect(painter.clip_rect()));
    cp.rect_filled(corner, CornerRadius::ZERO, theme::HEADER);
    cp.text(
        pos2(corner.right() - 4.0, corner.top() + 4.0),
        Align2::RIGHT_TOP,
        "Vel",
        theme::small(),
        theme::TEXT_WEAK,
    );

    let resp = ui.interact(
        lane,
        Id::new(("velocity_lane", clip.id)),
        Sense::click_and_drag(),
    );
    let hover = resp.hover_pos();
    if let Some(h) = hover
        && drag.is_none()
    {
        cp.text(
            pos2(corner.right() - 4.0, corner.bottom() - 4.0),
            Align2::RIGHT_BOTTOM,
            velocity_at(lane, h.y).to_string(),
            theme::small(),
            theme::TEXT_DIM,
        );
        let near = shown
            .iter()
            .filter(|n| (view.x_of(n.start as f64) - h.x).abs() <= REACH)
            .map(|n| format!("{} {}", note_name(n.pitch), n.velocity))
            .collect::<Vec<_>>();
        if !near.is_empty() {
            resp.clone()
                .on_hover_text(format!("Velocity: {}", near.join(", ")));
        }
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
    }
    let start = if resp.drag_started() {
        ui.input(|i| i.pointer.press_origin())
    } else if resp.clicked() {
        resp.interact_pointer_pos()
    } else {
        None
    };
    if let Some(from) = start {
        app.sel.focus = Focus::Editor;
        let orig = editable(app, shown);
        let line = ui.input(|i| i.modifiers.shift);
        app.piano.vel_drag = Some(VelDrag {
            clip: clip.id,
            preview: orig.clone(),
            orig,
            from,
            last: from,
            line,
        });
        if resp.clicked() {
            // A click sets the stems under it at once: the button is up.
            update(app, ui, view, lane);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    #[test]
    fn heights_and_velocities_match() {
        let lane = Rect::from_min_size(pos2(0.0, 100.0), vec2(400.0, VEL_H));
        for v in [1u8, 40, 64, 100, 127] {
            assert_eq!(velocity_at(lane, y_of(lane, v)), v);
        }
        assert_eq!(velocity_at(lane, lane.top() - 50.0), 127);
        assert_eq!(velocity_at(lane, lane.bottom() + 50.0), 1);
    }

    #[test]
    fn a_sweep_sets_the_notes_it_passes() {
        let lane = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, VEL_H));
        let notes: Vec<Note> = (0..4).map(|i| Note::new(i * 100, 50, 60, 64)).collect();
        // One pixel per tick: stems at x = 0, 100, 200 and 300.
        let x_of = |n: &Note| n.start as f32;
        let high = y_of(lane, 120);
        let low = y_of(lane, 20);
        // Level across the first two.
        let set = swept(&notes, x_of, lane, pos2(-5.0, high), pos2(150.0, high), 0.0);
        assert_eq!(set, [(notes[0].id, 120), (notes[1].id, 120)]);
        // A ramp from the first to the last.
        let set = swept(&notes, x_of, lane, pos2(0.0, low), pos2(300.0, high), 0.0);
        let v: Vec<u8> = set.iter().map(|(_, v)| *v).collect();
        assert_eq!(v.len(), 4);
        assert!(v.windows(2).all(|w| w[0] < w[1]), "{v:?}");
        assert_eq!((v[0], v[3]), (20, 120));
        // Standing still near a stem sets it.
        let set = swept(
            &notes,
            x_of,
            lane,
            pos2(202.0, low),
            pos2(202.0, low),
            REACH,
        );
        assert_eq!(set, [(notes[2].id, 20)]);
    }
}
