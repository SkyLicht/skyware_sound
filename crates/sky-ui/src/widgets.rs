// SPDX-License-Identifier: GPL-3.0-or-later
//! Small custom widgets (fader, knob, meter) and the bookkeeping that turns
//! one drag into one undo step.

use std::collections::HashMap;

use egui::{
    Color32, CornerRadius, Id, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui,
    Vec2, pos2, vec2,
};

use crate::theme;

/// A rectangle dragged out over empty space to select what it touches.
#[derive(Clone, Debug)]
pub struct Marquee<T> {
    /// Where the drag began and where the pointer is now, on screen.
    pub from: Pos2,
    pub to: Pos2,
    /// What was selected before, back again if Escape cancels.
    pub before: Vec<T>,
    /// What stays selected: the same, when Shift or Ctrl was held.
    pub kept: Vec<T>,
}

impl<T: Clone> Marquee<T> {
    pub fn new(from: Pos2, before: Vec<T>, adding: bool) -> Self {
        let kept = if adding { before.clone() } else { Vec::new() };
        Self {
            from,
            to: from,
            before,
            kept,
        }
    }

    pub fn rect(&self) -> Rect {
        Rect::from_two_pos(self.from, self.to)
    }

    pub fn paint(&self, painter: &Painter) {
        let r = self.rect();
        painter.rect_filled(r, CornerRadius::ZERO, theme::ACCENT.gamma_multiply(0.12));
        painter.rect_stroke(
            r,
            CornerRadius::ZERO,
            Stroke::new(1.0, theme::ACCENT),
            StrokeKind::Inside,
        );
    }
}

/// Coalescing keys for gestures.
///
/// A widget asks for a key whenever its value changes. While the pointer
/// stays down the same widget gets the same key, so every change of one drag
/// merges into one undo step. Once all buttons are up the keys are dropped,
/// and the next change starts a new step.
pub struct Gestures {
    keys: HashMap<Id, u64>,
    next: u64,
    /// The widget last scrolled over, its key and when.
    scroll: Option<(Id, u64, f64)>,
}

impl Default for Gestures {
    fn default() -> Self {
        // Start far from small numbers so keys never meet ones that other
        // code might pick by hand.
        Self {
            keys: HashMap::new(),
            next: 0x5157_0000_0000,
            scroll: None,
        }
    }
}

impl Gestures {
    /// Call once per frame before any widget.
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.pointer.any_down()) {
            self.keys.clear();
        }
    }

    /// The key for the gesture `id` is part of.
    pub fn key(&mut self, id: Id) -> u64 {
        if let Some(k) = self.keys.get(&id) {
            return *k;
        }
        let k = self.fresh();
        self.keys.insert(id, k);
        k
    }

    /// The key for scrolling over `id` at `now` seconds: steps less than
    /// half a second apart are one gesture.
    pub fn scroll_key(&mut self, id: Id, now: f64) -> u64 {
        if let Some((last, key, at)) = &mut self.scroll
            && *last == id
            && now - *at < 0.5
        {
            *at = now;
            return *key;
        }
        let key = self.fresh();
        self.scroll = Some((id, key, now));
        key
    }

    /// A key no other gesture uses.
    pub fn fresh(&mut self) -> u64 {
        self.next += 1;
        self.next
    }
}

/// Lowest fader position above -inf.
pub const FADER_FLOOR_DB: f32 = -60.0;
pub const FADER_TOP_DB: f32 = 12.0;
/// What -inf is stored as.
pub const SILENT_DB: f32 = -120.0;

fn fader_pos(db: f32) -> f32 {
    if db <= FADER_FLOOR_DB {
        0.0
    } else {
        ((db - FADER_FLOOR_DB) / (FADER_TOP_DB - FADER_FLOOR_DB)).clamp(0.0, 1.0)
    }
}

fn fader_db(pos: f32) -> f32 {
    if pos <= 0.003 {
        SILENT_DB
    } else {
        (FADER_FLOOR_DB + pos.clamp(0.0, 1.0) * (FADER_TOP_DB - FADER_FLOOR_DB)).min(FADER_TOP_DB)
    }
}

pub fn format_db(db: f32) -> String {
    if db <= FADER_FLOOR_DB {
        "-inf".to_string()
    } else if db.abs() < 0.05 {
        "0.0 dB".to_string()
    } else {
        format!("{db:+.1} dB")
    }
}

/// A vertical volume fader in dB. Drag to move (Shift for fine steps),
/// double-click for 0 dB.
pub fn fader(ui: &mut Ui, db: &mut f32, size: Vec2) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let track = Rect::from_min_max(
        pos2(rect.center().x - 2.0, rect.top() + 6.0),
        pos2(rect.center().x + 2.0, rect.bottom() - 6.0),
    );
    let travel = track.height().max(1.0);
    let mut pos = fader_pos(*db);
    if response.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let dy = response.drag_delta().y / travel * if fine { 0.15 } else { 1.0 };
        if dy != 0.0 {
            pos = (pos - dy).clamp(0.0, 1.0);
            let new = fader_db(pos);
            if new != *db {
                *db = new;
                response.mark_changed();
            }
        }
    }
    if response.double_clicked() && *db != 0.0 {
        *db = 0.0;
        pos = fader_pos(0.0);
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(track, CornerRadius::same(2), theme::BG);
        for mark in [12.0, 6.0, 0.0, -6.0, -12.0, -24.0, -36.0, -48.0, -60.0] {
            let y = track.bottom() - fader_pos(mark) * travel;
            let w = if mark == 0.0 { 7.0 } else { 4.0 };
            let c = if mark == 0.0 {
                theme::TEXT_WEAK
            } else {
                theme::TEXT_DIM
            };
            painter.hline(
                (track.left() - w - 2.0)..=(track.left() - 2.0),
                y,
                Stroke::new(1.0, c),
            );
        }
        let y = track.bottom() - pos * travel;
        let thumb = Rect::from_center_size(pos2(rect.center().x, y), vec2(size.x.min(26.0), 11.0));
        let fill = if response.dragged() || response.hovered() {
            theme::lighten(Color32::from_rgb(120, 128, 142), 0.2)
        } else {
            Color32::from_rgb(120, 128, 142)
        };
        painter.rect_filled(thumb, CornerRadius::same(2), fill);
        painter.hline(
            (thumb.left() + 3.0)..=(thumb.right() - 3.0),
            y,
            Stroke::new(1.5, theme::BG),
        );
    }
    response.on_hover_text(format!(
        "{}\nDrag to change, Shift for fine steps, double-click for 0 dB",
        format_db(*db)
    ))
}

/// A rotary knob. Drag up or right to increase (Shift for fine steps),
/// double-click to reset to `default`.
pub fn knob(
    ui: &mut Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    default: f32,
    diameter: f32,
) -> Response {
    let (min, max) = (*range.start(), *range.end());
    let (rect, mut response) =
        ui.allocate_exact_size(Vec2::splat(diameter), Sense::click_and_drag());
    let span = (max - min).max(f32::EPSILON);
    if response.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let d = response.drag_delta();
        let delta = (d.x - d.y) / 150.0 * span * if fine { 0.1 } else { 1.0 };
        if delta != 0.0 {
            let new = (*value + delta).clamp(min, max);
            if new != *value {
                *value = new;
                response.mark_changed();
            }
        }
    }
    if response.double_clicked() && *value != default {
        *value = default;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect.expand(1.0));
        let c = rect.center();
        let r = diameter * 0.5 - 2.0;
        painter.circle_filled(c, r, Color32::from_rgb(48, 53, 63));
        let start = std::f32::consts::PI * 0.75;
        let sweep = std::f32::consts::PI * 1.5;
        let angle_of = |v: f32| start + (v - min) / span * sweep;
        let arc = |from: f32, to: f32| -> Vec<Pos2> {
            let steps = 24;
            (0..=steps)
                .map(|i| {
                    let a = from + (to - from) * i as f32 / steps as f32;
                    c + Vec2::angled(a) * (r - 1.0)
                })
                .collect()
        };
        painter.add(Shape::line(
            arc(start, start + sweep),
            Stroke::new(2.0, theme::BG),
        ));
        let origin = if min < 0.0 && max > 0.0 { 0.0 } else { min };
        let (a0, a1) = (angle_of(origin), angle_of(*value));
        if (a1 - a0).abs() > 0.01 {
            painter.add(Shape::line(
                arc(a0.min(a1), a0.max(a1)),
                Stroke::new(2.0, theme::ACCENT),
            ));
        }
        painter.line_segment(
            [c, c + Vec2::angled(a1) * (r - 3.0)],
            Stroke::new(2.0, theme::TEXT),
        );
    }
    response
}

/// Text for a pan position.
pub fn format_pan(pan: f32) -> String {
    let pct = (pan * 100.0).round() as i32;
    match pct {
        0 => "C".to_string(),
        p if p < 0 => format!("{}L", -p),
        p => format!("{p}R"),
    }
}

/// Meter levels with decay and peak hold, in linear gain.
#[derive(Clone, Copy, Debug, Default)]
pub struct MeterLevel {
    pub level: [f32; 2],
    pub hold: [f32; 2],
    hold_age: [f32; 2],
}

impl MeterLevel {
    /// Folds in the latest peaks. Levels fall at about 26 dB a second;
    /// the hold line stays for a second and a half, then falls.
    pub fn update(&mut self, peaks: (f32, f32), dt: f32) {
        let fall = 10f32.powf(-26.0 * dt / 20.0);
        for (i, p) in [peaks.0, peaks.1].into_iter().enumerate() {
            let p = if p.is_finite() { p.abs() } else { 0.0 };
            self.level[i] = p.max(self.level[i] * fall);
            if p >= self.hold[i] {
                self.hold[i] = p;
                self.hold_age[i] = 0.0;
            } else {
                self.hold_age[i] += dt;
                if self.hold_age[i] > 1.5 {
                    self.hold[i] *= fall;
                }
            }
            if self.level[i] < 1e-5 {
                self.level[i] = 0.0;
            }
            if self.hold[i] < 1e-5 {
                self.hold[i] = 0.0;
            }
        }
    }

    pub fn is_active(&self) -> bool {
        self.level.iter().chain(&self.hold).any(|l| *l > 0.0)
    }
}

const METER_FLOOR: f32 = -60.0;
const METER_TOP: f32 = 6.0;

fn meter_frac(gain: f32) -> f32 {
    let db = sky_dsp::util::gain_to_db(gain);
    ((db - METER_FLOOR) / (METER_TOP - METER_FLOOR)).clamp(0.0, 1.0)
}

/// Draws a stereo peak meter.
pub fn meter(ui: &mut Ui, level: &MeterLevel, size: Vec2) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(2), theme::BG);
    let gap = 2.0;
    let w = (rect.width() - gap * 3.0) / 2.0;
    let zero = rect.bottom() - meter_frac(1.0) * rect.height();
    for ch in 0..2 {
        let x0 = rect.left() + gap + ch as f32 * (w + gap);
        let bar = Rect::from_min_max(
            pos2(x0, rect.top() + 1.0),
            pos2(x0 + w, rect.bottom() - 1.0),
        );
        let frac = meter_frac(level.level[ch]);
        let top = bar.bottom() - frac * bar.height();
        // Green up to -12 dB, amber to 0 dB, red above.
        let zones = [
            (-12.0f32, theme::OK),
            (0.0, theme::WARN),
            (METER_TOP, theme::ERROR),
        ];
        let mut lo = bar.bottom();
        for (db, color) in zones {
            let y = bar.bottom() - ((db - METER_FLOOR) / (METER_TOP - METER_FLOOR)) * bar.height();
            let seg_top = y.max(top);
            if seg_top < lo {
                painter.rect_filled(
                    Rect::from_min_max(pos2(bar.left(), seg_top), pos2(bar.right(), lo)),
                    CornerRadius::ZERO,
                    color,
                );
            }
            lo = y;
            if y <= top {
                break;
            }
        }
        if level.hold[ch] > 0.0 {
            let y = bar.bottom() - meter_frac(level.hold[ch]) * bar.height();
            let c = if level.hold[ch] >= 1.0 {
                theme::ERROR
            } else {
                theme::TEXT
            };
            painter.hline(bar.left()..=bar.right(), y, Stroke::new(1.5, c));
        }
    }
    painter.hline(
        rect.left()..=rect.right(),
        zero,
        Stroke::new(1.0, theme::TEXT_DIM),
    );
    painter.rect_stroke(
        rect,
        CornerRadius::same(2),
        Stroke::new(1.0, theme::GRID_BEAT),
        StrokeKind::Inside,
    );
    let peak = level.hold[0].max(level.hold[1]);
    response.on_hover_text(if peak > 0.0 {
        format!("Peak {:+.1} dBFS", sky_dsp::util::gain_to_db(peak))
    } else {
        "Silent".to_string()
    })
}

/// Marks something an AI assistant just changed; `glow` runs from 1 when
/// it changed down to 0.
pub fn ai_glow(painter: &Painter, rect: Rect, radius: u8, glow: f32) {
    if glow <= 0.0 {
        return;
    }
    let r = CornerRadius::same(radius);
    painter.rect_filled(rect, r, theme::AI.gamma_multiply(0.18 * glow));
    painter.rect_stroke(
        rect,
        r,
        Stroke::new(2.0, theme::AI.gamma_multiply(glow)),
        StrokeKind::Inside,
    );
}

/// A small toggle button that lights up in `on_color` when active.
pub fn toggle(ui: &mut Ui, on: bool, text: &str, on_color: Color32, tip: &str) -> Response {
    let rich = egui::RichText::new(text).strong().color(if on {
        Color32::BLACK
    } else {
        theme::TEXT_WEAK
    });
    let mut button = egui::Button::new(rich).min_size(vec2(22.0, 20.0));
    if on {
        button = button.fill(on_color);
    }
    ui.add(button).on_hover_text(tip)
}
