// SPDX-License-Identifier: GPL-3.0-or-later
//! Small plots for the device editors: axes on a linear or log scale, a
//! grid with labels, curves, filled areas and handles to drag.

use egui::epaint::Mesh;
use egui::{
    Align2, Color32, CornerRadius, FontId, Id, Painter, Pos2, Rect, Response, Sense, Shape, Stroke,
    StrokeKind, Ui, pos2, vec2,
};

use crate::theme;

/// Maps values to a share of a plot's width or height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axis {
    pub min: f32,
    pub max: f32,
    pub log: bool,
}

impl Axis {
    pub const fn lin(min: f32, max: f32) -> Self {
        Self {
            min,
            max,
            log: false,
        }
    }

    pub const fn log(min: f32, max: f32) -> Self {
        Self {
            min,
            max,
            log: true,
        }
    }

    /// Where `v` falls, 0 at `min` and 1 at `max`, unclamped.
    pub fn frac(&self, v: f32) -> f32 {
        if self.log {
            let (lo, hi) = (self.min.max(1e-9).ln(), self.max.max(1e-9).ln());
            (v.max(1e-9).ln() - lo) / (hi - lo)
        } else {
            (v - self.min) / (self.max - self.min)
        }
    }

    /// The value at a share of the axis, clamped to it.
    pub fn value(&self, frac: f32) -> f32 {
        let f = frac.clamp(0.0, 1.0);
        if self.log {
            let (lo, hi) = (self.min.max(1e-9).ln(), self.max.max(1e-9).ln());
            (lo + (hi - lo) * f).exp()
        } else {
            self.min + (self.max - self.min) * f
        }
    }
}

/// A plot area with an axis across and one up.
#[derive(Clone, Copy, Debug)]
pub struct Plot {
    pub rect: Rect,
    pub x: Axis,
    pub y: Axis,
}

impl Plot {
    /// Takes `size` from `ui` for a plot and draws its background.
    pub fn new(ui: &mut Ui, size: egui::Vec2, x: Axis, y: Axis) -> (Self, Painter) {
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::same(3), theme::BG);
        (Self { rect, x, y }, painter)
    }

    pub fn pos(&self, x: f32, y: f32) -> Pos2 {
        pos2(
            self.rect.left() + self.x.frac(x) * self.rect.width(),
            self.rect.bottom() - self.y.frac(y) * self.rect.height(),
        )
    }

    /// The values under a point, clamped to the axes.
    pub fn value_at(&self, p: Pos2) -> (f32, f32) {
        (
            self.x
                .value((p.x - self.rect.left()) / self.rect.width().max(1.0)),
            self.y
                .value((self.rect.bottom() - p.y) / self.rect.height().max(1.0)),
        )
    }

    /// Vertical grid lines at `values`, labelled along the bottom.
    pub fn grid_x(&self, painter: &Painter, values: &[f32], label: impl Fn(f32) -> String) {
        for v in values {
            let x = self.pos(*v, self.y.min).x;
            painter.vline(x, self.rect.y_range(), Stroke::new(1.0, theme::GRID_FAINT));
            painter.text(
                pos2(x + 2.0, self.rect.bottom() - 2.0),
                Align2::LEFT_BOTTOM,
                label(*v),
                FontId::proportional(9.0),
                theme::TEXT_DIM,
            );
        }
    }

    /// Horizontal grid lines at `values`, labelled at the left; `strong`
    /// draws one of them brighter.
    pub fn grid_y(
        &self,
        painter: &Painter,
        values: &[f32],
        strong: Option<f32>,
        label: impl Fn(f32) -> String,
    ) {
        for v in values {
            let y = self.pos(self.x.min, *v).y;
            let color = if Some(*v) == strong {
                theme::GRID_BAR
            } else {
                theme::GRID_FAINT
            };
            painter.hline(self.rect.x_range(), y, Stroke::new(1.0, color));
            painter.text(
                pos2(self.rect.left() + 2.0, y - 1.0),
                Align2::LEFT_BOTTOM,
                label(*v),
                FontId::proportional(9.0),
                theme::TEXT_DIM,
            );
        }
    }

    /// The points of `f` along the x axis, one every two pixels.
    pub fn points(&self, f: impl Fn(f32) -> f32) -> Vec<Pos2> {
        let steps = (self.rect.width() / 2.0).ceil().max(2.0) as usize;
        (0..=steps)
            .map(|i| {
                let x = self.x.value(i as f32 / steps as f32);
                let y = f(x);
                let p = self.pos(x, y);
                pos2(p.x, p.y.clamp(self.rect.top(), self.rect.bottom()))
            })
            .collect()
    }

    /// Fills between `points` and the line at `base`.
    pub fn fill(&self, painter: &Painter, points: &[Pos2], base: f32, color: Color32) {
        let base_y = self
            .pos(self.x.min, base)
            .y
            .clamp(self.rect.top(), self.rect.bottom());
        let mut mesh = Mesh::default();
        for w in points.windows(2) {
            let [a, b] = [w[0], w[1]];
            let i = mesh.vertices.len() as u32;
            mesh.colored_vertex(a, color);
            mesh.colored_vertex(b, color);
            mesh.colored_vertex(pos2(b.x, base_y), color);
            mesh.colored_vertex(pos2(a.x, base_y), color);
            mesh.add_triangle(i, i + 1, i + 2);
            mesh.add_triangle(i, i + 2, i + 3);
        }
        painter.add(Shape::mesh(mesh));
    }

    pub fn line(&self, painter: &Painter, points: Vec<Pos2>, stroke: Stroke) {
        painter.add(Shape::line(points, stroke));
    }

    pub fn border(&self, painter: &Painter) {
        painter.rect_stroke(
            self.rect,
            CornerRadius::same(3),
            Stroke::new(1.0, theme::GRID_BEAT),
            StrokeKind::Inside,
        );
    }
}

/// A point on a plot that can be dragged.
pub struct Handle {
    pub response: Response,
    /// Where the pointer is while the handle is dragged, in plot values.
    pub dragged_to: Option<(f32, f32)>,
}

/// Radius of a handle's dot, and of the area that takes the pointer.
const HANDLE_R: f32 = 5.0;
const HANDLE_HIT: f32 = 9.0;

/// Draws a handle at `(x, y)` and lets it be dragged. `label` is drawn
/// beside it.
pub fn handle(
    ui: &mut Ui,
    plot: &Plot,
    id: Id,
    at: (f32, f32),
    color: Color32,
    label: &str,
) -> Handle {
    let center = plot.pos(at.0, at.1);
    let center = pos2(
        center.x.clamp(plot.rect.left(), plot.rect.right()),
        center.y.clamp(plot.rect.top(), plot.rect.bottom()),
    );
    let hit = Rect::from_center_size(center, vec2(HANDLE_HIT * 2.0, HANDLE_HIT * 2.0));
    let response = ui.interact(hit, id, Sense::click_and_drag());
    let dragged_to = if response.dragged() {
        response.interact_pointer_pos().map(|p| plot.value_at(p))
    } else {
        None
    };
    let active = response.hovered() || response.dragged();
    let painter = ui.painter_at(plot.rect.expand(HANDLE_R + 1.0));
    painter.circle_filled(
        center,
        if active { HANDLE_R + 1.5 } else { HANDLE_R },
        color,
    );
    painter.circle_stroke(center, HANDLE_R + 0.5, Stroke::new(1.0, theme::BG));
    if !label.is_empty() {
        painter.text(
            center + vec2(0.0, -HANDLE_R - 2.0),
            Align2::CENTER_BOTTOM,
            label,
            FontId::proportional(10.0),
            color,
        );
    }
    Handle {
        response,
        dragged_to,
    }
}

/// Labels a frequency in Hz the short way: 50, 500, 5k.
pub fn hz_label(hz: f32) -> String {
    if hz >= 1_000.0 {
        let k = hz / 1_000.0;
        if (k - k.round()).abs() < 0.05 {
            format!("{}k", k.round())
        } else {
            format!("{k:.1}k")
        }
    } else {
        format!("{}", hz.round())
    }
}

/// Grid lines for an axis of frequencies.
pub const HZ_GRID: [f32; 6] = [50.0, 100.0, 500.0, 1_000.0, 5_000.0, 10_000.0];

/// Formats milliseconds the short way: 0.5 ms, 120 ms, 1.2 s.
pub fn ms_label(ms: f32) -> String {
    if ms >= 1_000.0 {
        format!("{:.2} s", ms / 1_000.0)
    } else if ms >= 10.0 {
        format!("{} ms", ms.round())
    } else {
        format!("{ms:.1} ms")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axes_map_both_ways() {
        let log = Axis::log(20.0, 20_000.0);
        assert!((log.frac(632.455_5) - 0.5).abs() < 1e-4);
        assert!((log.value(0.5) - 632.455_5).abs() < 0.1);
        let lin = Axis::lin(-18.0, 18.0);
        assert_eq!(lin.frac(0.0), 0.5);
        assert_eq!(lin.value(2.0), 18.0, "values clamp to the axis");
    }

    #[test]
    fn labels_stay_short() {
        assert_eq!(hz_label(50.0), "50");
        assert_eq!(hz_label(5_000.0), "5k");
        assert_eq!(hz_label(1_500.0), "1.5k");
        assert_eq!(ms_label(0.5), "0.5 ms");
        assert_eq!(ms_label(120.0), "120 ms");
        assert_eq!(ms_label(1_250.0), "1.25 s");
    }
}
