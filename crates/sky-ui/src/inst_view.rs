// SPDX-License-Identifier: GPL-3.0-or-later
//! Pictures of the instruments for the Instrument tab: the envelopes as
//! graphs to drag, the filter sweeps of the synth and the acid bass, the
//! sampler's waveform with its start and end, and the drum kit as pads to
//! play and pick. Their handles change the same parameters as the sliders
//! under them.

use std::collections::HashMap;
use std::f32::consts::LN_2;

use egui::{
    Align2, CornerRadius, FontId, Id, Rect, RichText, Sense, Stroke, StrokeKind, Ui, Vec2,
    WidgetInfo, WidgetType, pos2, vec2,
};
use sky_core::TrackId;
use sky_dsp::curves::{self, SIXTY_DB};
use sky_dsp::instruments::ACID_ENV_OCTAVES;
use sky_dsp::{InstrumentType, ParamSpec};
use sky_model::InstrumentSlot;

use crate::app::App;
use crate::graph::{self, Axis, Plot};
use crate::params::{Edits, ParamChange, param_grid};
use crate::theme;
use crate::waveform::Peaks;

/// What the Instrument tab remembers between frames.
#[derive(Default)]
pub struct DeviceState {
    /// The drum pad picked on each drum track, as an index into [`PADS`].
    pub pads: HashMap<TrackId, usize>,
}

/// The size of an envelope or sweep graph.
const GRAPH: Vec2 = vec2(300.0, 120.0);
/// The size of the sampler's waveform.
const WAVE: Vec2 = vec2(420.0, 120.0);

/// The id of a handle in an instrument's picture, for tests.
pub fn handle_id(track: TrackId, name: &str) -> Id {
    Id::new(("inst_handle", track, name))
}

/// Draws the pictures of an instrument other than the drums and returns
/// the changes their handles ask for.
pub fn instrument_view(
    app: &mut App,
    ui: &mut Ui,
    track: TrackId,
    slot: &InstrumentSlot,
) -> Vec<ParamChange> {
    let peaks = match (slot.kind, slot.sample) {
        (InstrumentType::Sampler, Some(id)) => {
            let (peaks, needs_job) = app.waveforms.get(id);
            if needs_job {
                app.spawn_peaks(ui.ctx(), id);
            }
            peaks
        }
        _ => None,
    };
    let value_of = |name: &str| slot.param(name).unwrap_or(0.0);
    let mut e = Edits::new(slot.kind.params(), &mut app.gestures, &value_of);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(10.0, 8.0);
        match slot.kind {
            InstrumentType::Synth => {
                envelope_graph(ui, track, &mut e, &ADSR, "Amp envelope");
                sweep_graph(ui, track, &mut e, &SYNTH_SWEEP);
            }
            InstrumentType::Acid => sweep_graph(ui, track, &mut e, &ACID_SWEEP),
            InstrumentType::Sampler => {
                sample_view(ui, track, slot.sample.is_some(), peaks.as_deref(), &mut e);
                envelope_graph(ui, track, &mut e, &AR, "Envelope");
            }
            InstrumentType::Bank => envelope_graph(ui, track, &mut e, &AR, "Envelope"),
            InstrumentType::Drums => {}
        }
    });
    e.changes
}

/// Time on an envelope graph grows like the log of this plus the time, so
/// a few milliseconds and a few seconds both have room.
const WARP_MS: f32 = 5.0;

/// Where `ms` sits along a graph that reaches `max` ms, from 0 to 1.
fn warp(ms: f32, max: f32) -> f32 {
    (1.0 + ms.max(0.0) / WARP_MS).ln() / (1.0 + max / WARP_MS).ln()
}

/// The time at `frac` along a graph that reaches `max` ms.
fn unwarp(frac: f32, max: f32) -> f32 {
    WARP_MS * ((1.0 + max / WARP_MS).powf(frac.clamp(0.0, 1.0)) - 1.0)
}

/// The parameters an envelope graph edits.
struct EnvParams {
    attack: &'static str,
    decay: Option<&'static str>,
    sustain: Option<&'static str>,
    release: &'static str,
}

const ADSR: EnvParams = EnvParams {
    attack: "attack",
    decay: Some("decay"),
    sustain: Some("sustain"),
    release: "release",
};

/// The sampler's and the bank's: they hold full level until let go.
const AR: EnvParams = EnvParams {
    attack: "attack",
    decay: None,
    sustain: None,
    release: "release",
};

/// A graph's title, in its top-right corner where the handles seldom go.
fn title(plot: &Plot, painter: &egui::Painter, text: &str) {
    painter.text(
        pos2(plot.rect.right() - 5.0, plot.rect.top() + 3.0),
        Align2::RIGHT_TOP,
        text,
        FontId::proportional(11.0),
        theme::TEXT_WEAK,
    );
}

/// A line of values under a graph.
fn readout(ui: &mut Ui, width: f32, text: String) {
    ui.add_sized(
        [width, 14.0],
        egui::Label::new(RichText::new(text).small().color(theme::TEXT_WEAK)),
    );
}

/// An envelope as a graph: the attack, the decay to the sustain level, a
/// stretch held there and the release. Time is stretched so short times
/// stay visible.
fn envelope_graph(ui: &mut Ui, track: TrackId, e: &mut Edits, names: &EnvParams, label: &str) {
    let max = |name: &str| e.spec(name).map_or(1_000.0, |s| s.max);
    let (a_max, r_max) = (max(names.attack), max(names.release));
    let d_max = names.decay.map_or(1.0, max);
    let a = e.get(names.attack);
    let d = names.decay.map_or(0.0, |n| e.get(n));
    let s = names.sustain.map_or(1.0, |n| e.get(n));
    let r = e.get(names.release);
    // The share of the width each stage gets at its longest.
    let (a_share, d_share, s_share, r_share) = if names.decay.is_some() {
        (0.22, 0.22, 0.16, 0.40)
    } else {
        (0.30, 0.0, 0.25, 0.45)
    };
    let a_end = a_share * warp(a, a_max);
    let d_end = a_end + d_share * warp(d, d_max);
    let s_end = d_end + s_share;
    let r_end = s_end + r_share * warp(r, r_max);
    let held = |t: f32| curves::adsr_level(a, d.max(1e-3), s, r, f32::INFINITY, t);

    ui.vertical(|ui| {
        let (plot, painter) = Plot::new(ui, GRAPH, Axis::lin(0.0, 1.0), Axis::lin(-0.03, 1.15));
        plot.grid_y(&painter, &[0.5, 1.0], None, |_| String::new());
        let steps = 40;
        let mut points = Vec::with_capacity(4 * steps + 4);
        for i in 0..=steps {
            let ms = unwarp(i as f32 / steps as f32 * warp(a, a_max), a_max);
            points.push(plot.pos(a_share * warp(ms, a_max), held(ms)));
        }
        if names.decay.is_some() {
            for i in 0..=steps {
                let ms = unwarp(i as f32 / steps as f32 * warp(d, d_max), d_max);
                points.push(plot.pos(a_end + d_share * warp(ms, d_max), held(a + ms)));
            }
        }
        points.push(plot.pos(s_end, s));
        for i in 0..=steps {
            let ms = unwarp(i as f32 / steps as f32 * warp(r, r_max), r_max);
            let x = s_end + r_share * warp(ms, r_max);
            points.push(plot.pos(x, s * curves::decay_level(r, ms)));
        }
        for x in [a_end, d_end, s_end] {
            let at = plot.pos(x, 0.0).x;
            painter.vline(
                at,
                plot.rect.y_range(),
                Stroke::new(1.0, theme::GRID_FAINT),
            );
        }
        plot.fill(&painter, &points, 0.0, theme::ACCENT.gamma_multiply(0.16));
        plot.line(&painter, points, Stroke::new(2.0, theme::ACCENT));
        plot.border(&painter);
        title(&plot, &painter, label);

        let id = handle_id(track, names.attack);
        let h = graph::handle(ui, &plot, id, (a_end, 1.0), theme::ACCENT, "");
        if let Some((x, _)) = h.dragged_to {
            e.drag(id, names.attack, unwarp(x / a_share, a_max));
        }
        if h.response.double_clicked() {
            e.reset(names.attack);
        }
        h.response.on_hover_text(format!(
            "Attack {}\nDrag across, double-click to reset",
            graph::ms_label(a)
        ));

        if let (Some(decay), Some(sustain)) = (names.decay, names.sustain) {
            let id = handle_id(track, decay);
            let h = graph::handle(ui, &plot, id, (d_end, s), theme::ACCENT, "");
            if let Some((x, y)) = h.dragged_to {
                e.drag(id, decay, unwarp((x - a_end) / d_share, d_max));
                e.drag(id, sustain, y);
            }
            if h.response.double_clicked() {
                e.reset(decay);
                e.reset(sustain);
            }
            h.response.on_hover_text(format!(
                "Decay {}, sustain {:.0}%\nDrag across for the decay and up or down for the sustain",
                graph::ms_label(d),
                s * 100.0
            ));
        }

        let id = handle_id(track, names.release);
        let h = graph::handle(ui, &plot, id, (r_end, 0.0), theme::ACCENT, "");
        if let Some((x, _)) = h.dragged_to {
            e.drag(id, names.release, unwarp((x - s_end) / r_share, r_max));
        }
        if h.response.double_clicked() {
            e.reset(names.release);
        }
        h.response.on_hover_text(format!(
            "Release {}\nDrag across, double-click to reset",
            graph::ms_label(r)
        ));

        let text = if names.decay.is_some() {
            format!(
                "A {}   D {}   S {:.0}%   R {}",
                graph::ms_label(a),
                graph::ms_label(d),
                s * 100.0,
                graph::ms_label(r)
            )
        } else {
            format!(
                "Attack {}   Release {}",
                graph::ms_label(a),
                graph::ms_label(r)
            )
        };
        readout(ui, GRAPH.x, text);
    });
}

/// The parameters a filter sweep graph edits, and how the instrument
/// turns them into a sweep.
struct Sweep {
    cutoff: &'static str,
    amount: &'static str,
    decay: &'static str,
    /// Octaves the sweep opens per unit of `amount`.
    octaves: f32,
    /// The envelope's attack, in ms.
    attack: f32,
    note: &'static str,
}

/// The synth's filter envelope: a 0.5 ms attack, then down to nothing in
/// the filter decay.
const SYNTH_SWEEP: Sweep = Sweep {
    cutoff: "cutoff",
    amount: "env_amount",
    decay: "filter_decay",
    octaves: 1.0,
    attack: 0.5,
    note: "",
};

const ACID_SWEEP: Sweep = Sweep {
    cutoff: "cutoff",
    amount: "env_mod",
    decay: "decay",
    octaves: ACID_ENV_OCTAVES,
    attack: 0.0,
    note: "Accented notes open it further",
};

/// The filter's cutoff over the first seconds of a note.
fn sweep_graph(ui: &mut Ui, track: TrackId, e: &mut Edits, sw: &Sweep) {
    let t_max = e.spec(sw.decay).map_or(5_000.0, |s| s.max);
    let (cutoff, amount, decay) = (e.get(sw.cutoff), e.get(sw.amount), e.get(sw.decay));
    let octaves = amount * sw.octaves;
    let level = |t: f32| {
        if sw.attack > 0.0 {
            curves::adsr_level(sw.attack, decay, 0.0, decay, f32::INFINITY, t)
        } else {
            curves::decay_level(decay, t)
        }
    };
    let hz = |level: f32| (cutoff * 2f32.powf(octaves * level)).min(20_000.0);

    ui.vertical(|ui| {
        let (plot, painter) = Plot::new(
            ui,
            GRAPH,
            Axis::lin(0.0, 1.0),
            Axis::log(20.0, 20_000.0),
        );
        plot.grid_y(&painter, &[100.0, 1_000.0, 10_000.0], None, graph::hz_label);
        for ms in [10.0, 100.0, 1_000.0] {
            let x = plot.pos(warp(ms, t_max), 20.0).x;
            painter.vline(x, plot.rect.y_range(), Stroke::new(1.0, theme::GRID_FAINT));
            painter.text(
                pos2(x + 2.0, plot.rect.bottom() - 2.0),
                Align2::LEFT_BOTTOM,
                graph::ms_label(ms),
                FontId::proportional(9.0),
                theme::TEXT_DIM,
            );
        }
        let base = plot.pos(0.0, cutoff).y;
        painter.add(egui::Shape::dashed_line(
            &[pos2(plot.rect.left(), base), pos2(plot.rect.right(), base)],
            Stroke::new(1.0, theme::GRID_BAR),
            4.0,
            3.0,
        ));
        let points = plot.points(|u| hz(level(unwarp(u, t_max))));
        plot.fill(&painter, &points, 20.0, theme::ACCENT.gamma_multiply(0.16));
        plot.line(&painter, points, Stroke::new(2.0, theme::ACCENT));
        plot.border(&painter);
        title(&plot, &painter, "Filter envelope");

        let id = handle_id(track, sw.amount);
        let at = warp(sw.attack, t_max).max(0.02);
        let h = graph::handle(ui, &plot, id, (at, hz(1.0)), theme::ACCENT, "");
        if let Some((_, y)) = h.dragged_to {
            e.drag(id, sw.amount, (y / cutoff).log2() / sw.octaves);
        }
        if h.response.double_clicked() {
            e.reset(sw.amount);
        }
        h.response.on_hover_text(format!(
            "The envelope opens the filter {octaves:+.1} octaves\nDrag up or down, double-click to reset"
        ));

        // The decay's handle sits where the sweep is half way down.
        let half = sw.attack + decay * LN_2 / SIXTY_DB;
        let id = handle_id(track, sw.decay);
        let h = graph::handle(
            ui,
            &plot,
            id,
            (warp(half, t_max), hz(0.5)),
            theme::ACCENT,
            "",
        );
        if let Some((x, _)) = h.dragged_to {
            let t = unwarp(x, t_max);
            e.drag(id, sw.decay, (t - sw.attack) * SIXTY_DB / LN_2);
        }
        if h.response.double_clicked() {
            e.reset(sw.decay);
        }
        h.response.on_hover_text(format!(
            "Decay {}\nDrag across, double-click to reset",
            graph::ms_label(decay)
        ));

        let id = handle_id(track, sw.cutoff);
        let h = graph::handle(ui, &plot, id, (0.96, cutoff), theme::ACCENT, "");
        if let Some((_, y)) = h.dragged_to {
            e.drag(id, sw.cutoff, y);
        }
        if h.response.double_clicked() {
            e.reset(sw.cutoff);
        }
        let mut tip = format!(
            "Cutoff {} Hz, where the sweep ends\nDrag up or down, double-click to reset",
            graph::hz_label(cutoff)
        );
        if !sw.note.is_empty() {
            tip = format!("{tip}\n{}", sw.note);
        }
        h.response.on_hover_text(tip);

        readout(
            ui,
            GRAPH.x,
            format!(
                "Cutoff {} Hz   Env {octaves:+.1} oct   Decay {}",
                graph::hz_label(cutoff),
                graph::ms_label(decay)
            ),
        );
    });
}

/// The least share of a sample between its start and end.
const MIN_SPAN: f32 = 0.005;

/// The sampler's waveform with its start and end markers to drag.
fn sample_view(
    ui: &mut Ui,
    track: TrackId,
    has_sample: bool,
    peaks: Option<&Peaks>,
    e: &mut Edits,
) {
    let (start, end) = (e.get("start"), e.get("end"));
    ui.vertical(|ui| {
        let (rect, _) = ui.allocate_exact_size(WAVE, Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::same(3), theme::BG);
        let x_of = |frac: f32| rect.left() + frac.clamp(0.0, 1.0) * rect.width();
        let seconds = peaks.map(|p| p.frames as f32 / p.sample_rate.max(1) as f32);
        match peaks {
            Some(p) if p.frames > 0 => {
                let mid = rect.center().y;
                let half = rect.height() * 0.45;
                let columns = rect.width().max(1.0) as usize;
                for c in 0..columns {
                    let from = c as f64 / columns as f64 * p.frames as f64;
                    let to = (c + 1) as f64 / columns as f64 * p.frames as f64;
                    let Some((lo, hi)) = p.range(from, to) else {
                        continue;
                    };
                    let frac = (c as f32 + 0.5) / columns as f32;
                    let color = if frac >= start && frac <= end {
                        theme::ACCENT
                    } else {
                        theme::TEXT_DIM
                    };
                    let x = rect.left() + c as f32 + 0.5;
                    painter.vline(
                        x,
                        (mid - hi * half)..=(mid - lo * half + 0.5),
                        Stroke::new(1.0, color),
                    );
                }
            }
            _ => {
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    if has_sample {
                        "Reading the sample…"
                    } else {
                        "Choose a sample to see it here"
                    },
                    FontId::proportional(12.0),
                    theme::TEXT_DIM,
                );
            }
        }
        // Dim what the markers leave out.
        let shade = theme::BG.gamma_multiply(0.6);
        painter.rect_filled(
            Rect::from_x_y_ranges(rect.left()..=x_of(start), rect.y_range()),
            CornerRadius::ZERO,
            shade,
        );
        painter.rect_filled(
            Rect::from_x_y_ranges(x_of(end)..=rect.right(), rect.y_range()),
            CornerRadius::ZERO,
            shade,
        );
        for (name, value, color, label) in [
            ("start", start, theme::OK, "Start"),
            ("end", end, theme::WARN, "End"),
        ] {
            let x = x_of(value);
            let id = handle_id(track, name);
            let hit = Rect::from_x_y_ranges((x - 6.0)..=(x + 6.0), rect.y_range());
            let response = ui.interact(hit, id, Sense::click_and_drag());
            let active = response.hovered() || response.dragged();
            painter.vline(
                x,
                rect.y_range(),
                Stroke::new(if active { 2.5 } else { 1.5 }, color),
            );
            let flag = if name == "start" {
                Rect::from_min_size(pos2(x, rect.top()), vec2(28.0, 13.0))
            } else {
                Rect::from_min_size(pos2(x - 22.0, rect.top()), vec2(22.0, 13.0))
            };
            painter.rect_filled(flag, CornerRadius::same(2), color);
            painter.text(
                flag.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(9.5),
                theme::BG,
            );
            if response.dragged()
                && let Some(p) = response.interact_pointer_pos()
            {
                let frac = (p.x - rect.left()) / rect.width().max(1.0);
                let frac = if name == "start" {
                    frac.min(end - MIN_SPAN)
                } else {
                    frac.max(start + MIN_SPAN)
                };
                e.drag(id, name, frac);
            }
            if response.double_clicked() {
                e.reset(name);
            }
            let at = seconds.map_or_else(
                || format!("{:.0}%", value * 100.0),
                |s| format!("{:.3} s", value * s),
            );
            response.on_hover_text(format!(
                "{label} at {at}\nDrag to move it, double-click to reset"
            ));
        }
        painter.rect_stroke(
            rect,
            CornerRadius::same(3),
            Stroke::new(1.0, theme::GRID_BEAT),
            StrokeKind::Inside,
        );
        let text = match seconds {
            Some(s) => format!("Plays {:.3} s to {:.3} s of {:.3} s", start * s, end * s, s),
            None => format!("Plays {:.0}% to {:.0}%", start * 100.0, end * 100.0),
        };
        readout(ui, WAVE.x, text);
    });
}

/// One pad of the drum kit: its name, the note it plays and the kit's
/// parameters that shape it.
pub struct Pad {
    pub label: &'static str,
    pub note: u8,
    pub params: &'static [&'static str],
    /// The parameter for its level, shown on the pad.
    pub level: &'static str,
}

const HAT_CLOSED: &[&str] = &["hat_tune", "hat_decay", "hat_level"];
const HAT_OPEN: &[&str] = &["hat_tune", "open_decay", "hat_level"];
const TOM: &[&str] = &["tom_tune", "tom_decay", "tom_level"];
const CYMBAL: &[&str] = &["cymbal_tune", "cymbal_decay", "cymbal_level"];

/// The pads, in rows of four.
pub const PADS: [Pad; 12] = [
    Pad {
        label: "Kick",
        note: 36,
        params: &[
            "kick_tune",
            "kick_decay",
            "kick_sweep",
            "kick_click",
            "kick_drive",
            "kick_level",
        ],
        level: "kick_level",
    },
    Pad {
        label: "Rim",
        note: 37,
        params: &["rim_level"],
        level: "rim_level",
    },
    Pad {
        label: "Snare",
        note: 38,
        params: &["snare_tune", "snare_decay", "snare_snap", "snare_level"],
        level: "snare_level",
    },
    Pad {
        label: "Clap",
        note: 39,
        params: &["clap_decay", "clap_level"],
        level: "clap_level",
    },
    Pad {
        label: "Closed hat",
        note: 42,
        params: HAT_CLOSED,
        level: "hat_level",
    },
    Pad {
        label: "Open hat",
        note: 46,
        params: HAT_OPEN,
        level: "hat_level",
    },
    Pad {
        label: "Crash",
        note: 49,
        params: CYMBAL,
        level: "cymbal_level",
    },
    Pad {
        label: "Ride",
        note: 51,
        params: CYMBAL,
        level: "cymbal_level",
    },
    Pad {
        label: "Low tom",
        note: 41,
        params: TOM,
        level: "tom_level",
    },
    Pad {
        label: "Mid tom",
        note: 45,
        params: TOM,
        level: "tom_level",
    },
    Pad {
        label: "High tom",
        note: 48,
        params: TOM,
        level: "tom_level",
    },
    Pad {
        label: "Cowbell",
        note: 56,
        params: &["cowbell_level"],
        level: "cowbell_level",
    },
];

/// The parameters of the whole kit.
const KIT: &[&str] = &["style", "tune", "decay", "tone", "gain"];

const PAD_SIZE: Vec2 = vec2(92.0, 54.0);

/// The id of a drum pad, for tests.
pub fn pad_id(track: TrackId, index: usize) -> Id {
    Id::new(("drum_pad", track, index))
}

fn specs_named(all: &[ParamSpec], names: &[&str]) -> Vec<ParamSpec> {
    names
        .iter()
        .filter_map(|n| all.iter().find(|s| s.name == *n).copied())
        .collect()
}

/// The drum kit as pads: pressing one plays it and picks it, and the
/// picked pad's sound settings show beside the kit's.
pub fn drum_panel(
    app: &mut App,
    ui: &mut Ui,
    track: TrackId,
    slot: &InstrumentSlot,
) -> Vec<ParamChange> {
    let all = slot.kind.params();
    let value_of = |name: &str| slot.param(name).unwrap_or(0.0);
    let picked = app
        .devices
        .pads
        .get(&track)
        .copied()
        .unwrap_or(0)
        .min(PADS.len() - 1);
    let held = app.audition.held;
    let mut pressed = None;
    let mut changes = Vec::new();
    let pad = &PADS[picked];
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(20.0, 10.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for (row, pads) in PADS.chunks(4).enumerate() {
                ui.horizontal(|ui| {
                    for (col, pad) in pads.iter().enumerate() {
                        let i = row * 4 + col;
                        let lit = held == Some((track, pad.note));
                        let level = value_of(pad.level);
                        if pad_button(ui, track, i, pad, i == picked, lit, level) {
                            pressed = Some(i);
                        }
                    }
                });
            }
            ui.label(
                RichText::new("Press a pad to hear it and edit its sound")
                    .small()
                    .color(theme::TEXT_DIM),
            );
        });
        ui.vertical(|ui| {
            ui.label(RichText::new("Kit").strong().color(theme::TEXT));
            changes.extend(param_grid(
                ui,
                &mut app.gestures,
                ("kit", track),
                &specs_named(all, KIT),
                value_of,
            ));
        });
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(pad.label).strong().color(theme::TEXT));
                ui.label(
                    RichText::new(format!("note {}", pad.note))
                        .small()
                        .color(theme::TEXT_DIM),
                );
            });
            changes.extend(param_grid(
                ui,
                &mut app.gestures,
                ("pad", track),
                &specs_named(all, pad.params),
                value_of,
            ));
            let sharing: Vec<&str> = PADS
                .iter()
                .filter(|p| p.note != pad.note && p.level == pad.level)
                .map(|p| p.label)
                .collect();
            if !sharing.is_empty() {
                ui.label(
                    RichText::new(format!("Shared with {}", sharing.join(", ")))
                        .small()
                        .color(theme::TEXT_DIM),
                );
            }
        });
    });
    if let Some(i) = pressed {
        app.devices.pads.insert(track, i);
        app.hold_key(track, Some(PADS[i].note), 100);
    }
    changes
}

/// Draws one pad; true while the pointer is pressed on it.
fn pad_button(
    ui: &mut Ui,
    track: TrackId,
    index: usize,
    pad: &Pad,
    picked: bool,
    lit: bool,
    level_db: f32,
) -> bool {
    let (rect, _) = ui.allocate_exact_size(PAD_SIZE, Sense::hover());
    let response = ui
        .interact(rect, pad_id(track, index), Sense::click())
        .on_hover_text(format!("{} (note {})", pad.label, pad.note));
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, pad.label));
    let pressed = response.is_pointer_button_down_on();
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        let fill = if lit || pressed {
            theme::ACCENT_BG
        } else if response.hovered() {
            theme::HEADER_SELECTED
        } else {
            theme::HEADER
        };
        painter.rect_filled(rect, CornerRadius::same(4), fill);
        if picked {
            painter.rect_stroke(
                rect,
                CornerRadius::same(4),
                Stroke::new(1.5, theme::ACCENT),
                StrokeKind::Inside,
            );
        }
        painter.text(
            rect.left_top() + vec2(6.0, 5.0),
            Align2::LEFT_TOP,
            pad.label,
            FontId::proportional(12.0),
            theme::TEXT,
        );
        painter.text(
            rect.right_top() + vec2(-5.0, 5.0),
            Align2::RIGHT_TOP,
            pad.note.to_string(),
            FontId::proportional(9.0),
            theme::TEXT_DIM,
        );
        // Its level, from -60 to +12 dB.
        let frac = ((level_db + 60.0) / 72.0).clamp(0.0, 1.0);
        let track_rect = Rect::from_min_size(
            pos2(rect.left() + 6.0, rect.bottom() - 9.0),
            vec2(rect.width() - 12.0, 3.0),
        );
        painter.rect_filled(track_rect, CornerRadius::same(1), theme::BG);
        painter.rect_filled(
            Rect::from_min_size(
                track_rect.min,
                vec2(track_rect.width() * frac, track_rect.height()),
            ),
            CornerRadius::same(1),
            if lit { theme::ACCENT } else { theme::TEXT_WEAK },
        );
    }
    pressed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warped_time_maps_both_ways() {
        for ms in [0.0, 0.5, 12.0, 300.0, 5_000.0] {
            let back = unwarp(warp(ms, 5_000.0), 5_000.0);
            assert!((back - ms).abs() < 1e-2 * ms.max(1.0), "{ms} -> {back}");
        }
        assert_eq!(warp(5_000.0, 5_000.0), 1.0);
    }

    #[test]
    fn every_drum_parameter_is_on_a_pad_or_the_kit() {
        for spec in InstrumentType::Drums.params() {
            let on_pad = PADS.iter().any(|p| p.params.contains(&spec.name));
            assert!(
                on_pad || KIT.contains(&spec.name),
                "{} has no place",
                spec.name
            );
        }
        for pad in &PADS {
            assert!(pad.params.contains(&pad.level), "{}", pad.label);
            for name in pad.params {
                assert!(
                    InstrumentType::Drums
                        .params()
                        .iter()
                        .any(|s| s.name == *name),
                    "{name}"
                );
            }
        }
    }
}
