// SPDX-License-Identifier: GPL-3.0-or-later
//! The sustain pedal lane under the velocity lane: a bar wherever the pedal
//! is down. Dragging across empty space draws a stretch, dragging a bar
//! moves it, dragging either end moves that end, and right-clicking a bar
//! takes it away. Each change is one undo step.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Id, Key, Painter, Rect, Sense, Stroke, Ui, pos2,
};
use sky_core::{ClipId, PPQ, Ticks};
use sky_model::{Clip, Pedal};

use crate::actions;
use crate::app::{App, Focus};
use crate::arrange::{snap_floor, snap_round};
use crate::piano_roll::View;
use crate::theme;

/// The lane's height.
pub const PEDAL_H: f32 = 22.0;

/// How near a bar's end the pointer must be to grab the end.
const EDGE: f32 = 5.0;

/// What a drag in the lane changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PedalEdit {
    /// A new stretch from where the drag began.
    Draw,
    /// Stretch `i`, whole.
    Move(usize),
    /// Where stretch `i` goes down.
    Start(usize),
    /// Where stretch `i` comes up.
    End(usize),
}

/// Stretches being drawn or changed; applied once on release.
#[derive(Clone, Debug)]
pub struct PedalDrag {
    pub clip: ClipId,
    pub edit: PedalEdit,
    pub orig: Vec<Pedal>,
    pub preview: Vec<Pedal>,
    /// The tick where the button went down.
    pub grab: f64,
}

/// The part of a stretch at tick `tick`, with `edge` ticks of reach at its
/// ends.
pub fn hit(pedal: &[Pedal], tick: f64, edge: f64) -> Option<PedalEdit> {
    let i = pedal
        .iter()
        .position(|p| (p.start as f64 - edge..=p.end() as f64 + edge).contains(&tick))?;
    let p = pedal[i];
    Some(if (tick - p.end() as f64).abs() <= edge {
        PedalEdit::End(i)
    } else if (tick - p.start as f64).abs() <= edge {
        PedalEdit::Start(i)
    } else {
        PedalEdit::Move(i)
    })
}

/// The stretches after a drag from `grab` to `now` (ticks from the clip
/// start), snapped to `grid`. Nothing goes before the clip's start, and
/// every stretch keeps at least one grid step.
pub fn edited(
    orig: &[Pedal],
    edit: PedalEdit,
    grab: f64,
    now: f64,
    grid: Option<Ticks>,
) -> Vec<Pedal> {
    let least = grid.unwrap_or(PPQ / 32).max(1);
    let mut out = orig.to_vec();
    match edit {
        PedalEdit::Draw => {
            let (a, b) = (grab.min(now), grab.max(now));
            let start = snap_floor(a, grid).max(0);
            let end = snap_round(b, grid).max(start + least);
            out.push(Pedal::new(start, end - start));
        }
        PedalEdit::Move(i) => {
            if let Some(p) = out.get_mut(i) {
                let start = snap_round(p.start as f64 + now - grab, grid).max(0);
                p.start = start;
            }
        }
        PedalEdit::Start(i) => {
            if let Some(p) = out.get_mut(i) {
                let end = p.end();
                let start = snap_round(now, grid).clamp(0, end - least);
                *p = Pedal::new(start, end - start);
            }
        }
        PedalEdit::End(i) => {
            if let Some(p) = out.get_mut(i) {
                let end = snap_round(now, grid).max(p.start + least);
                p.length = end - p.start;
            }
        }
    }
    Pedal::tidy(out)
}

/// Follows a drag, and applies it on release.
fn update(app: &mut App, ui: &Ui, view: &View) {
    let Some(mut d) = app.piano.pedal_drag.take() else {
        return;
    };
    let (pos, down, alt, escape) = ui.input(|i| {
        (
            i.pointer.latest_pos(),
            i.pointer.primary_down(),
            i.modifiers.alt,
            i.key_pressed(Key::Escape),
        )
    });
    if escape {
        return;
    }
    let grid = (!alt).then(|| app.piano.grid.ticks());
    if let Some(p) = pos {
        d.preview = edited(&d.orig, d.edit, d.grab, view.tick_at(p.x), grid);
        ui.ctx().set_cursor_icon(match d.edit {
            PedalEdit::Move(_) => CursorIcon::Grabbing,
            _ => CursorIcon::ResizeHorizontal,
        });
    }
    if down {
        app.piano.pedal_drag = Some(d);
        return;
    }
    if d.preview != d.orig {
        let r = actions::set_pedal(&app.session, d.clip, d.preview, "Edit pedal");
        app.report(r);
    }
}

/// The lane; `pedal` are the clip's stretches and `corner` the space
/// beside the lane under the keys.
#[allow(clippy::too_many_arguments)]
pub fn show(
    app: &mut App,
    ui: &mut Ui,
    painter: &Painter,
    view: &View,
    lane: Rect,
    corner: Rect,
    clip: &Clip,
    pedal: &[Pedal],
    color: Color32,
) {
    if app
        .piano
        .pedal_drag
        .as_ref()
        .is_some_and(|d| d.clip != clip.id)
    {
        app.piano.pedal_drag = None;
    }
    update(app, ui, view);

    let p = painter.with_clip_rect(lane.intersect(painter.clip_rect()));
    p.rect_filled(lane, CornerRadius::ZERO, theme::PANEL);
    p.hline(
        lane.x_range(),
        lane.top() + 0.5,
        Stroke::new(1.0, theme::BG),
    );
    let end_x = view.x_of(clip.length as f64);
    if end_x < lane.right() {
        p.rect_filled(
            Rect::from_min_max(pos2(end_x.max(lane.left()), lane.top()), lane.max),
            CornerRadius::ZERO,
            Color32::from_black_alpha(110),
        );
    }
    let shown = app
        .piano
        .pedal_drag
        .as_ref()
        .map_or(pedal, |d| d.preview.as_slice());
    let fill = theme::lighten(color, 0.1);
    for s in shown {
        let x0 = view.x_of(s.start as f64);
        let x1 = view.x_of(s.end() as f64).max(x0 + 2.0);
        if x1 < lane.left() || x0 > lane.right() {
            continue;
        }
        let bar = Rect::from_min_max(pos2(x0, lane.top() + 5.0), pos2(x1, lane.bottom() - 4.0));
        p.rect_filled(bar, CornerRadius::same(2), fill);
        // The pedal goes down at the left end and comes up at the right.
        p.vline(
            x0 + 0.5,
            bar.y_range(),
            Stroke::new(2.0, theme::lighten(color, 0.45)),
        );
    }

    let cp = painter.with_clip_rect(corner.intersect(painter.clip_rect()));
    cp.rect_filled(corner, CornerRadius::ZERO, theme::HEADER);
    cp.text(
        pos2(corner.right() - 4.0, corner.center().y),
        Align2::RIGHT_CENTER,
        "Ped",
        theme::small(),
        theme::TEXT_WEAK,
    );

    let resp = ui.interact(
        lane,
        Id::new(("pedal_lane", clip.id)),
        Sense::click_and_drag(),
    );
    let edge = f64::from(EDGE / view.px_per_tick());
    let under = |x: f32| hit(pedal, view.tick_at(x), edge);
    if let Some(h) = resp.hover_pos()
        && app.piano.pedal_drag.is_none()
    {
        match under(h.x) {
            Some(PedalEdit::Start(_) | PedalEdit::End(_)) => {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            Some(_) => ui.ctx().set_cursor_icon(CursorIcon::Grab),
            None => {}
        }
    }
    let resp = resp.on_hover_text(
        "Sustain pedal: drag across empty space to hold it down, drag a bar or its ends to \
         change it, right-click a bar to take it away. Alt turns off the grid.",
    );
    if resp.secondary_clicked()
        && let Some(p) = resp.interact_pointer_pos()
        && let Some(PedalEdit::Move(i) | PedalEdit::Start(i) | PedalEdit::End(i)) = under(p.x)
    {
        app.sel.focus = Focus::Editor;
        let mut rest = pedal.to_vec();
        rest.remove(i);
        let r = actions::set_pedal(&app.session, clip.id, rest, "Delete pedal");
        app.report(r);
        return;
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
        let edit = under(from.x).unwrap_or(PedalEdit::Draw);
        // A click on a bar changes nothing.
        if resp.clicked() && edit != PedalEdit::Draw {
            return;
        }
        let grab = view.tick_at(from.x);
        app.piano.pedal_drag = Some(PedalDrag {
            clip: clip.id,
            edit,
            orig: pedal.to_vec(),
            preview: pedal.to_vec(),
            grab,
        });
        if resp.clicked() {
            // A click on empty space holds the pedal for a grid step.
            update(app, ui, view);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEAT: Ticks = PPQ;

    #[test]
    fn the_pointer_finds_ends_and_bars() {
        let pedal = [Pedal::new(0, BEAT * 2), Pedal::new(BEAT * 4, BEAT)];
        let at = |beats: f64| hit(&pedal, beats * BEAT as f64, 10.0);
        assert_eq!(at(1.0), Some(PedalEdit::Move(0)));
        assert_eq!(at(2.0), Some(PedalEdit::End(0)));
        assert_eq!(at(4.0), Some(PedalEdit::Start(1)));
        assert_eq!(at(3.0), None);
    }

    #[test]
    fn drags_draw_move_and_stretch_on_the_grid() {
        let grid = Some(BEAT);
        let b = |x: f64| x * BEAT as f64;
        // Drawn backwards from 3.4 to 1.2 beats: beat 1 to beat 3.
        let drawn = edited(&[], PedalEdit::Draw, b(3.4), b(1.2), grid);
        assert_eq!(drawn, [Pedal::new(BEAT, BEAT * 2)]);
        // A click holds it for a grid step.
        let clicked = edited(&[], PedalEdit::Draw, b(5.5), b(5.5), grid);
        assert_eq!(clicked, [Pedal::new(BEAT * 5, BEAT)]);
        let one = [Pedal::new(BEAT, BEAT * 2)];
        // Moved a beat and a bit later, then too far back.
        let moved = edited(&one, PedalEdit::Move(0), b(2.0), b(3.2), grid);
        assert_eq!(moved, [Pedal::new(BEAT * 2, BEAT * 2)]);
        let moved = edited(&one, PedalEdit::Move(0), b(2.0), b(-5.0), grid);
        assert_eq!(moved, [Pedal::new(0, BEAT * 2)]);
        // Ends stay a grid step apart.
        let end = edited(&one, PedalEdit::End(0), b(3.0), b(0.0), grid);
        assert_eq!(end, [Pedal::new(BEAT, BEAT)]);
        let start = edited(&one, PedalEdit::Start(0), b(1.0), b(2.6), grid);
        assert_eq!(start, [Pedal::new(BEAT * 2, BEAT)]);
        // Drawn over another, they join.
        let joined = edited(&one, PedalEdit::Draw, b(2.5), b(6.0), grid);
        assert_eq!(joined, [Pedal::new(BEAT, BEAT * 5)]);
    }
}
