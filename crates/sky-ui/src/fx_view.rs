// SPDX-License-Identifier: GPL-3.0-or-later
//! Pictures of the effects for their windows: the EQ's and the filter's
//! response, the compressor's and the limiter's transfer curves with a
//! gain-reduction meter and its recent history, the duck's dip over a bar
//! and the distortion's curve. Their handles change the same parameters as
//! the sliders under them, and the curves come from the functions the
//! effects themselves use.

use std::collections::VecDeque;

use egui::{Align2, Color32, CornerRadius, FontId, Id, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};
use sky_core::EffectId;
use sky_dsp::EffectType;
use sky_dsp::curves;
use sky_dsp::effects::eq_coeffs;
use sky_dsp::util::db_to_gain;

use crate::graph::{self, Axis, Plot};
use crate::params::Edits;
use crate::theme;

/// The sample rate the curves are drawn for. Only the top octave of the
/// filter curves depends on it.
pub const CURVE_RATE: f32 = 48_000.0;
/// The size of a wide picture, and of a square one.
const WIDE: Vec2 = vec2(360.0, 170.0);
const SQUARE: Vec2 = vec2(170.0, 170.0);
/// The gain-reduction meter's width and the most reduction it shows.
const METER_W: f32 = 14.0;
pub const REDUCTION_RANGE: f32 = 24.0;
/// How far back the history of gain reduction reaches, and how many
/// points it keeps a second.
const HISTORY_SECONDS: f32 = 4.0;
const HISTORY_RATE: f32 = 30.0;

/// Whether `kind` has a picture in its window.
pub fn has_view(kind: EffectType) -> bool {
    matches!(
        kind,
        EffectType::Eq
            | EffectType::Filter
            | EffectType::Compressor
            | EffectType::Limiter
            | EffectType::Duck
            | EffectType::Distortion
    )
}

/// The id of a handle in an effect's picture, for tests.
pub fn handle_id(effect: EffectId, name: &str) -> Id {
    Id::new(("fx_handle", effect, name))
}

/// Draws the picture of an effect: its curve, the handles that change it
/// and, for effects that turn the signal down, how much they do.
pub fn effect_view(
    ui: &mut Ui,
    effect: EffectId,
    kind: EffectType,
    e: &mut Edits,
    bpm: f64,
    reduction: Option<&ReductionLevel>,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        match kind {
            EffectType::Eq => eq_view(ui, effect, e),
            EffectType::Filter => filter_view(ui, effect, e),
            EffectType::Compressor => compressor_view(ui, effect, e),
            EffectType::Limiter => limiter_view(ui, effect, e),
            EffectType::Duck => duck_view(ui, effect, e, bpm),
            EffectType::Distortion => distortion_view(ui, effect, e),
            _ => {}
        }
        if kind.reduces_gain() {
            let empty = ReductionLevel::default();
            let level = reduction.unwrap_or(&empty);
            let height = if kind == EffectType::Duck {
                WIDE.y
            } else {
                SQUARE.y
            };
            reduction_meter(ui, level, vec2(METER_W, height));
            if kind != EffectType::Duck {
                let w = WIDE.x - SQUARE.x - METER_W - 12.0;
                reduction_history(ui, level, vec2(w, SQUARE.y));
            }
        }
    });
}

fn db_label(db: f32) -> String {
    if db > 0.0 {
        format!("+{db}")
    } else {
        format!("{db}")
    }
}

/// A caption in a plot's top-right corner.
fn caption(plot: &Plot, painter: &egui::Painter, text: &str) {
    painter.text(
        pos2(plot.rect.right() - 4.0, plot.rect.top() + 3.0),
        Align2::RIGHT_TOP,
        text,
        FontId::proportional(10.5),
        theme::TEXT_WEAK,
    );
}

/// A caption low in a plot's right side, clear of the axis labels: where
/// a transfer curve never goes.
fn caption_low(plot: &Plot, painter: &egui::Painter, text: &str) {
    painter.text(
        pos2(plot.rect.right() - 4.0, plot.rect.bottom() - 16.0),
        Align2::RIGHT_BOTTOM,
        text,
        FontId::proportional(10.5),
        theme::TEXT_WEAK,
    );
}

/// A dashed reference line from `a` to `b`, in plot values.
fn dashed(plot: &Plot, painter: &egui::Painter, a: (f32, f32), b: (f32, f32), color: Color32) {
    painter.add(egui::Shape::dashed_line(
        &[plot.pos(a.0, a.1), plot.pos(b.0, b.1)],
        Stroke::new(1.0, color),
        4.0,
        3.0,
    ));
}

/// The EQ's three bands as names of their parameters.
struct Band {
    freq: &'static str,
    gain: &'static str,
    label: &'static str,
    name: &'static str,
    color: Color32,
}

const BANDS: [Band; 3] = [
    Band {
        freq: "low_freq",
        gain: "low_gain",
        label: "L",
        name: "Low shelf",
        color: theme::EQ_LOW,
    },
    Band {
        freq: "mid_freq",
        gain: "mid_gain",
        label: "M",
        name: "Mid peak",
        color: theme::EQ_MID,
    },
    Band {
        freq: "high_freq",
        gain: "high_gain",
        label: "H",
        name: "High shelf",
        color: theme::EQ_HIGH,
    },
];

/// The EQ's parameters in the order [`eq_coeffs`] takes them.
const EQ_ORDER: [&str; 7] = [
    "low_freq",
    "low_gain",
    "mid_freq",
    "mid_gain",
    "mid_q",
    "high_freq",
    "high_gain",
];

fn eq_view(ui: &mut Ui, effect: EffectId, e: &mut Edits) {
    let values = EQ_ORDER.map(|n| e.get(n));
    let coeffs = eq_coeffs(&values, CURVE_RATE);
    let (plot, painter) = Plot::new(ui, WIDE, Axis::log(20.0, 20_000.0), Axis::lin(-24.0, 24.0));
    plot.grid_x(&painter, &graph::HZ_GRID, graph::hz_label);
    plot.grid_y(
        &painter,
        &[-18.0, -12.0, -6.0, 0.0, 6.0, 12.0, 18.0],
        Some(0.0),
        db_label,
    );
    for (c, band) in coeffs.iter().zip(&BANDS) {
        let points = plot.points(|f| c.magnitude_db(f, CURVE_RATE));
        plot.line(
            &painter,
            points,
            Stroke::new(1.0, band.color.gamma_multiply(0.55)),
        );
    }
    let sum = plot.points(|f| coeffs.iter().map(|c| c.magnitude_db(f, CURVE_RATE)).sum());
    plot.fill(&painter, &sum, 0.0, theme::ACCENT.gamma_multiply(0.16));
    plot.line(&painter, sum, Stroke::new(2.0, theme::ACCENT));
    plot.border(&painter);
    caption(&plot, &painter, &format!("Q {:.2}", e.get("mid_q")));
    for band in &BANDS {
        let id = handle_id(effect, band.freq);
        let (freq, gain) = (e.get(band.freq), e.get(band.gain));
        let h = graph::handle(ui, &plot, id, (freq, gain), band.color, band.label);
        if let Some((f, g)) = h.dragged_to {
            e.drag(id, band.freq, f);
            e.drag(id, band.gain, g);
        }
        if h.response.double_clicked() {
            e.reset(band.gain);
        }
        let mut tip = format!(
            "{}: {} Hz, {:+.1} dB\nDrag to move it, double-click to reset its gain",
            band.name,
            graph::hz_label(freq),
            gain
        );
        if band.gain == "mid_gain" {
            tip.push_str("\nScroll to make it wider or narrower");
            if h.response.hovered() {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0.0 {
                    let now = ui.input(|i| i.time);
                    let q = e.get("mid_q") * 2f32.powf(scroll / 200.0);
                    e.scroll(id, "mid_q", q, now);
                    ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
                }
            }
        }
        h.response.on_hover_text(tip);
    }
}

/// The filter's level at the cutoff, in dB, for a resonance.
fn resonance_peak_db(resonance: f32) -> f32 {
    -20.0 * (2.0 - 1.95 * resonance.clamp(0.0, 1.0)).log10()
}

/// The resonance that puts the level at the cutoff at `db`.
fn resonance_for_peak(db: f32) -> f32 {
    (2.0 - 10f32.powf(-db / 20.0)) / 1.95
}

fn filter_view(ui: &mut Ui, effect: EffectId, e: &mut Edits) {
    let mode = e.get("mode").round().clamp(0.0, 2.0) as u8;
    let (cutoff, resonance) = (e.get("cutoff"), e.get("resonance"));
    let (plot, painter) = Plot::new(ui, WIDE, Axis::log(20.0, 20_000.0), Axis::lin(-42.0, 30.0));
    plot.grid_x(&painter, &graph::HZ_GRID, graph::hz_label);
    plot.grid_y(
        &painter,
        &[-36.0, -24.0, -12.0, 0.0, 12.0, 24.0],
        Some(0.0),
        db_label,
    );
    let points = plot.points(|f| curves::svf_magnitude_db(mode, cutoff, resonance, f, CURVE_RATE));
    plot.fill(&painter, &points, -42.0, theme::ACCENT.gamma_multiply(0.16));
    plot.line(&painter, points, Stroke::new(2.0, theme::ACCENT));
    plot.border(&painter);
    let id = handle_id(effect, "cutoff");
    let h = graph::handle(
        ui,
        &plot,
        id,
        (cutoff, resonance_peak_db(resonance)),
        theme::ACCENT,
        "",
    );
    if let Some((f, db)) = h.dragged_to {
        e.drag(id, "cutoff", f);
        e.drag(id, "resonance", resonance_for_peak(db));
    }
    if h.response.double_clicked() {
        e.reset("cutoff");
        e.reset("resonance");
    }
    h.response.on_hover_text(format!(
        "Cutoff {} Hz, resonance {:.2}\nDrag across for the cutoff and up for more resonance; double-click to reset",
        graph::hz_label(cutoff),
        resonance
    ));
}

fn compressor_view(ui: &mut Ui, effect: EffectId, e: &mut Edits) {
    let (threshold, ratio, knee, makeup) = (
        e.get("threshold"),
        e.get("ratio"),
        e.get("knee"),
        e.get("makeup"),
    );
    let out = |x: f32| x - curves::compressor_reduction_db(x, threshold, ratio, knee);
    let (plot, painter) = Plot::new(ui, SQUARE, Axis::lin(-60.0, 0.0), Axis::lin(-60.0, 0.0));
    let grid = [-48.0, -36.0, -24.0, -12.0];
    plot.grid_x(&painter, &grid, |v| format!("{v}"));
    plot.grid_y(&painter, &grid, None, |_| String::new());
    if knee > 0.0 {
        let a = plot.pos(threshold - knee * 0.5, 0.0);
        let b = plot.pos(threshold + knee * 0.5, -60.0);
        painter.rect_filled(
            Rect::from_two_pos(a, b),
            CornerRadius::ZERO,
            theme::ACCENT.gamma_multiply(0.07),
        );
    }
    dashed(&plot, &painter, (-60.0, -60.0), (0.0, 0.0), theme::GRID_BAR);
    if makeup > 0.0 {
        let lifted = plot.points(|x| out(x) + makeup);
        plot.line(
            &painter,
            lifted,
            Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.45)),
        );
    }
    let points = plot.points(out);
    plot.line(&painter, points, Stroke::new(2.0, theme::ACCENT));
    plot.border(&painter);
    caption_low(&plot, &painter, &format!("{ratio:.1}:1"));

    let id = handle_id(effect, "threshold");
    let h = graph::handle(
        ui,
        &plot,
        id,
        (threshold, out(threshold)),
        theme::ACCENT,
        "T",
    );
    if let Some((x, _)) = h.dragged_to {
        e.drag(id, "threshold", x);
    }
    if h.response.double_clicked() {
        e.reset("threshold");
    }
    h.response.on_hover_text(format!(
        "Threshold {threshold:.1} dB\nDrag across to move it, double-click to reset"
    ));

    let id = handle_id(effect, "ratio");
    let h = graph::handle(ui, &plot, id, (0.0, out(0.0)), theme::ACCENT, "R");
    if let Some((_, y)) = h.dragged_to {
        let above = y - threshold;
        let ratio = if above > 0.05 {
            -threshold / above
        } else {
            f32::MAX
        };
        e.drag(id, "ratio", ratio);
    }
    if h.response.double_clicked() {
        e.reset("ratio");
    }
    h.response.on_hover_text(format!(
        "Ratio {ratio:.1}:1\nDrag down to compress harder, double-click to reset"
    ));
}

fn limiter_view(ui: &mut Ui, effect: EffectId, e: &mut Edits) {
    let ceiling = e.get("ceiling");
    let (plot, painter) = Plot::new(ui, SQUARE, Axis::lin(-30.0, 6.0), Axis::lin(-30.0, 6.0));
    let grid = [-24.0, -12.0, 0.0];
    plot.grid_x(&painter, &grid, |v| format!("{v}"));
    plot.grid_y(&painter, &grid, None, |_| String::new());
    dashed(&plot, &painter, (-30.0, -30.0), (6.0, 6.0), theme::GRID_BAR);
    let points = plot.points(|x| x.min(ceiling));
    plot.line(&painter, points, Stroke::new(2.0, theme::ACCENT));
    plot.border(&painter);
    caption_low(&plot, &painter, &format!("ceiling {ceiling:.1} dB"));
    let id = handle_id(effect, "ceiling");
    let h = graph::handle(ui, &plot, id, (ceiling, ceiling), theme::ACCENT, "");
    if let Some((_, y)) = h.dragged_to {
        e.drag(id, "ceiling", y);
    }
    if h.response.double_clicked() {
        e.reset("ceiling");
    }
    h.response.on_hover_text(format!(
        "Ceiling {ceiling:.1} dB: nothing gets louder\nDrag up or down, double-click to reset"
    ));
}

/// The duck's gain over one beat once it has settled on a note every beat:
/// the gain each dip starts from, and the curve of one dip, in beats.
struct DuckShape {
    from: f32,
    depth: f32,
    power: f32,
    attack: f32,
    length: f32,
}

impl DuckShape {
    fn new(e: &Edits, bpm: f64) -> Self {
        let length = e.get("length");
        let beats_per_ms = bpm.max(1.0) as f32 / 60_000.0;
        let attack = (e.get("attack") * beats_per_ms).min(length * 0.5);
        let mut shape = Self {
            from: 1.0,
            depth: e.get("depth"),
            power: curves::duck_power(e.get("curve")),
            attack,
            length,
        };
        // A dip longer than a beat starts the next from where it got to.
        for _ in 0..16 {
            shape.from = shape.gain(1.0);
        }
        shape
    }

    /// The gain `t` beats after a note, if no other note came.
    fn gain(&self, t: f32) -> f32 {
        curves::duck_gain(
            self.from,
            self.depth,
            self.power,
            self.attack,
            self.length,
            t,
        )
    }
}

/// The top of the duck's picture, above full level to leave room for its
/// caption.
const DUCK_TOP: f32 = 1.2;

fn duck_view(ui: &mut Ui, effect: EffectId, e: &mut Edits, bpm: f64) {
    let shape = DuckShape::new(e, bpm);
    let width = WIDE.x - METER_W - 6.0;
    let (plot, painter) = Plot::new(
        ui,
        vec2(width, WIDE.y),
        Axis::lin(0.0, 4.0),
        Axis::lin(0.0, DUCK_TOP),
    );
    plot.grid_x(&painter, &[0.0, 1.0, 2.0, 3.0], |b| {
        format!("{}", b as i32 + 1)
    });
    let marks = [-12.0, -6.0, -3.0].map(db_to_gain);
    plot.grid_y(&painter, &marks, None, |g| {
        format!("{:.0} dB", sky_dsp::util::gain_to_db(g))
    });
    if shape.length > 1.0 {
        // The whole dip of the first note, as it would go on alone.
        let alone = plot.points(|t| {
            if t <= shape.length {
                shape.gain(t)
            } else {
                1.0
            }
        });
        plot.line(
            &painter,
            alone,
            Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.35)),
        );
    }
    let points = plot.points(|t| shape.gain(t.rem_euclid(1.0)));
    plot.fill(&painter, &points, 0.0, theme::ACCENT.gamma_multiply(0.16));
    plot.line(&painter, points, Stroke::new(2.0, theme::ACCENT));
    for beat in 0..4 {
        // A small mark at the top where each note lands.
        let at = plot.pos(beat as f32, DUCK_TOP);
        painter.add(egui::Shape::convex_polygon(
            vec![
                at + vec2(-4.0, 0.0),
                at + vec2(4.0, 0.0),
                at + vec2(0.0, 6.0),
            ],
            theme::TEXT_WEAK,
            Stroke::NONE,
        ));
    }
    plot.border(&painter);
    caption_low(&plot, &painter, "a note on every beat");

    let trough = 1.0 - shape.depth;
    let id = handle_id(effect, "depth");
    let h = graph::handle(ui, &plot, id, (shape.attack, trough), theme::ACCENT, "");
    if let Some((_, y)) = h.dragged_to {
        e.drag(id, "depth", 1.0 - y);
    }
    if h.response.double_clicked() {
        e.reset("depth");
    }
    h.response.on_hover_text(format!(
        "Depth {:.0}%\nDrag down to duck deeper, double-click to reset",
        shape.depth * 100.0
    ));

    // The handle for the curve sits half way through the recovery.
    let mid = shape.attack + (shape.length - shape.attack) * 0.5;
    let id = handle_id(effect, "curve");
    let h = graph::handle(ui, &plot, id, (mid, shape.gain(mid)), theme::ACCENT, "");
    if let Some((_, y)) = h.dragged_to
        && shape.depth > 0.01
    {
        let share = ((y - trough) / shape.depth).clamp(0.002, 0.998);
        let power = share.ln() / 0.5f32.ln();
        e.drag(id, "curve", power.ln() / 4f32.ln());
    }
    if h.response.double_clicked() {
        e.reset("curve");
    }
    h.response.on_hover_text(format!(
        "Curve {:.2}\nDrag down to stay down longer, up to come back sooner",
        e.get("curve")
    ));

    let id = handle_id(effect, "length");
    let h = graph::handle(ui, &plot, id, (shape.length, 1.0), theme::ACCENT, "");
    if let Some((x, _)) = h.dragged_to {
        e.drag(id, "length", x);
    }
    if h.response.double_clicked() {
        e.reset("length");
    }
    h.response.on_hover_text(format!(
        "Length {:.2} beats\nDrag across, double-click to reset",
        shape.length
    ));
}

fn distortion_view(ui: &mut Ui, effect: EffectId, e: &mut Edits) {
    let mode = e.get("mode").round().clamp(0.0, 2.0) as u8;
    let (drive_db, mix, output_db) = (e.get("drive"), e.get("mix"), e.get("output"));
    let (drive, output) = (db_to_gain(drive_db), db_to_gain(output_db));
    let (plot, painter) = Plot::new(ui, SQUARE, Axis::lin(-1.0, 1.0), Axis::lin(-1.0, 1.0));
    plot.grid_x(&painter, &[0.0], |_| String::new());
    plot.grid_y(&painter, &[0.0], Some(0.0), |_| String::new());
    dashed(&plot, &painter, (-1.0, -1.0), (1.0, 1.0), theme::GRID_BAR);
    let points = plot
        .points(|x| (x * (1.0 - mix) + curves::distortion_shape(mode, drive, x) * mix) * output);
    plot.line(&painter, points, Stroke::new(2.0, theme::ACCENT));
    plot.border(&painter);
    caption_low(&plot, &painter, &format!("drive {drive_db:.0} dB"));
    // Dragging up or down anywhere on it changes the drive.
    let id = handle_id(effect, "drive");
    let response = ui
        .interact(plot.rect, id, Sense::click_and_drag())
        .on_hover_text("Drag up or down to change the drive, double-click to reset it");
    if response.dragged() {
        let dy = response.drag_delta().y;
        if dy != 0.0 {
            e.drag(id, "drive", e.get("drive") - dy * 0.25);
        }
    }
    if response.double_clicked() {
        e.reset("drive");
    }
}

/// The gain reduction of one insert, as its meter shows it.
#[derive(Default)]
pub struct ReductionLevel {
    /// The reduction shown, in dB: the latest, falling back at 40 dB/s.
    pub db: f32,
    /// The most recent high point, held a second and a half.
    pub hold: f32,
    hold_age: f32,
    /// The most reduction in each step of the history, oldest first.
    pub history: VecDeque<f32>,
    /// The most reduction since the last history step, and the time.
    pending: f32,
    since: f32,
}

impl ReductionLevel {
    /// Folds in the most reduction the engine measured since the last call.
    pub fn update(&mut self, db: f32, dt: f32) {
        let db = if db.is_finite() { db.max(0.0) } else { 0.0 };
        self.db = db.max(self.db - 40.0 * dt);
        if db >= self.hold {
            self.hold = db;
            self.hold_age = 0.0;
        } else {
            self.hold_age += dt;
            if self.hold_age > 1.5 {
                self.hold = (self.hold - 40.0 * dt).max(self.db);
            }
        }
        if self.db < 0.01 {
            self.db = 0.0;
        }
        if self.hold < 0.01 {
            self.hold = 0.0;
        }
        self.pending = self.pending.max(db);
        self.since += dt;
        let step = 1.0 / HISTORY_RATE;
        while self.since >= step {
            self.since -= step;
            self.history.push_back(self.pending);
            self.pending = db;
        }
        let keep = (HISTORY_SECONDS * HISTORY_RATE) as usize;
        while self.history.len() > keep {
            self.history.pop_front();
        }
    }

    /// True while the meter or its history still shows something.
    pub fn is_active(&self) -> bool {
        self.db > 0.0 || self.hold > 0.0 || self.history.iter().any(|d| *d > 0.0)
    }
}

fn reduction_frac(db: f32) -> f32 {
    (db / REDUCTION_RANGE).clamp(0.0, 1.0)
}

/// A meter of gain reduction, growing down from the top.
pub fn reduction_meter(ui: &mut Ui, level: &ReductionLevel, size: Vec2) {
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(2), theme::BG);
    let inner = rect.shrink(2.0);
    let bar = Rect::from_min_max(
        inner.min,
        pos2(
            inner.right(),
            inner.top() + reduction_frac(level.db) * inner.height(),
        ),
    );
    painter.rect_filled(bar, CornerRadius::same(1), theme::REDUCTION);
    if level.hold > 0.0 {
        let y = inner.top() + reduction_frac(level.hold) * inner.height();
        painter.hline(
            inner.x_range(),
            y,
            Stroke::new(1.5, theme::lighten(theme::REDUCTION, 0.3)),
        );
    }
    for mark in [6.0, 12.0, 18.0] {
        let y = inner.top() + reduction_frac(mark) * inner.height();
        painter.hline(
            inner.left()..=inner.left() + 3.0,
            y,
            Stroke::new(1.0, theme::TEXT_DIM),
        );
    }
    response.on_hover_text(format!(
        "Gain reduction {:.1} dB (most lately {:.1} dB)",
        level.db, level.hold
    ));
}

/// The last few seconds of gain reduction, newest at the right.
pub fn reduction_history(ui: &mut Ui, level: &ReductionLevel, size: Vec2) {
    let (plot, painter) = Plot::new(
        ui,
        size,
        Axis::lin(0.0, HISTORY_SECONDS * HISTORY_RATE),
        Axis::lin(-REDUCTION_RANGE, 0.0),
    );
    plot.grid_y(&painter, &[-18.0, -12.0, -6.0], None, |v| format!("{v}"));
    let keep = HISTORY_SECONDS * HISTORY_RATE;
    let start = keep - level.history.len() as f32;
    let points: Vec<_> = level
        .history
        .iter()
        .enumerate()
        .map(|(i, db)| plot.pos(start + i as f32, -db.min(REDUCTION_RANGE)))
        .collect();
    if points.len() >= 2 {
        plot.fill(
            &painter,
            &points,
            0.0,
            theme::REDUCTION.gamma_multiply(0.35),
        );
        plot.line(&painter, points, Stroke::new(1.5, theme::REDUCTION));
    }
    plot.border(&painter);
    painter.text(
        pos2(plot.rect.left() + 4.0, plot.rect.bottom() - 3.0),
        Align2::LEFT_BOTTOM,
        format!("GR {:.1} dB", level.db),
        FontId::proportional(10.5),
        if level.db > 0.0 {
            theme::REDUCTION
        } else {
            theme::TEXT_DIM
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_resonance_handle_maps_both_ways() {
        for r in [0.0, 0.3, 0.9, 1.0] {
            let back = resonance_for_peak(resonance_peak_db(r));
            assert!((back - r).abs() < 1e-4, "{r} -> {back}");
        }
        // No resonance sits 6 dB down at the cutoff.
        assert!((resonance_peak_db(0.0) + 6.02).abs() < 0.01);
    }

    #[test]
    fn reduction_falls_back_and_scrolls_away() {
        let mut level = ReductionLevel::default();
        level.update(12.0, 1.0 / 60.0);
        assert_eq!(level.db, 12.0);
        for _ in 0..30 {
            level.update(0.0, 1.0 / 60.0);
        }
        assert!(level.db < 1.0 && level.db >= 0.0, "{}", level.db);
        assert_eq!(level.hold, 12.0, "the high point holds");
        assert!(level.is_active());
        for _ in 0..(5 * 60) {
            level.update(0.0, 1.0 / 60.0);
        }
        assert!(!level.is_active(), "{:?}", level.history);
        assert_eq!(
            level.history.len(),
            (HISTORY_SECONDS * HISTORY_RATE) as usize
        );
    }
}
