// SPDX-License-Identifier: GPL-3.0-or-later
//! The step sequencer editor: a grid with one row per lane, and the
//! settings of the selected step below it.

use egui::{
    Align2, Color32, ComboBox, CornerRadius, DragValue, FontId, Rect, RichText, ScrollArea, Sense,
    Stroke, StrokeKind, Ui, pos2, vec2,
};
use sky_core::ClipId;
use sky_core::music::note_name;
use sky_model::steps::{
    Condition, MAX_LANES, MAX_LOCKED_PARAMS, MAX_NUDGE, MAX_RATCHET, MAX_STEPS, RATES, Step,
    StepClip, StepKind, StepLane,
};
use sky_model::{Clip, Swing, Track};

use crate::actions;
use crate::app::App;
use crate::pattern_editor::format_number;
use crate::theme;

#[derive(Default)]
pub struct StepState {
    /// The selected step: its clip, lane and index.
    pub selected: Option<(ClipId, usize, usize)>,
    /// Where each step was drawn last frame, by lane and index.
    pub cells: Vec<((usize, usize), Rect)>,
}

/// An edit to the clip, made once the grid is drawn.
type Change = Option<Box<dyn FnOnce(&mut StepClip)>>;

const CELL: f32 = 22.0;
const GAP: f32 = 2.0;

/// The drum pieces offered for new lanes, by General MIDI note.
const DRUM_PIECES: [(u8, &str); 15] = [
    (36, "Kick"),
    (35, "Kick 2"),
    (37, "Rim"),
    (38, "Snare"),
    (40, "Snare 2"),
    (39, "Clap"),
    (42, "Closed hat"),
    (44, "Pedal hat"),
    (46, "Open hat"),
    (41, "Low tom"),
    (45, "Mid tom"),
    (48, "High tom"),
    (49, "Crash"),
    (51, "Ride"),
    (56, "Cowbell"),
];

fn rate_label(rate: u8) -> &'static str {
    match rate {
        4 => "1/4",
        8 => "1/8",
        12 => "1/8 triplet",
        16 => "1/16",
        24 => "1/16 triplet",
        32 => "1/32",
        _ => "?",
    }
}

/// A lane's name: the drum piece, or the note.
pub fn lane_label(drums: bool, note: u8) -> String {
    if drums && let Some((_, name)) = DRUM_PIECES.iter().find(|(n, _)| *n == note) {
        return (*name).to_string();
    }
    note_name(note)
}

fn condition_label(c: Condition) -> String {
    match c {
        Condition::Always => "Always".into(),
        Condition::First => "First pass".into(),
        Condition::NotFirst => "Not the first pass".into(),
        Condition::Fill => "Fill (last pass)".into(),
        Condition::NotFill => "Not the last pass".into(),
        Condition::Ratio { a, b } => format!("{a}:{b}"),
    }
}

pub fn show(app: &mut App, ui: &mut Ui, track: &Track, clip: &Clip, steps: &StepClip) {
    let drums = actions::plays_drums(track);
    settings_row(app, ui, clip, steps, drums);
    ui.add_space(4.0);
    if app.steps.selected.is_some_and(|(c, l, s)| {
        c != clip.id || steps.lanes.get(l).is_none_or(|lane| s >= lane.len())
    }) {
        app.steps.selected = None;
    }
    ScrollArea::horizontal()
        .id_salt(("step_grid", clip.id))
        .show(ui, |ui| grid(app, ui, track, clip, steps, drums));
    ui.label(
        RichText::new(
            "Click a step to turn it on or off and select it. Shift-click accents, \
             Alt-click slides, Ctrl-click ties.",
        )
        .small()
        .color(theme::TEXT_DIM),
    );
    ui.add_space(4.0);
    if let Some((_, li, si)) = app.steps.selected
        && let Some(step) = steps.lanes.get(li).and_then(|l| l.steps.get(si))
    {
        inspector(app, ui, track, clip, steps, li, si, step, drums);
    }
}

fn settings_row(app: &mut App, ui: &mut Ui, clip: &Clip, steps: &StepClip, drums: bool) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Step").color(theme::TEXT_WEAK));
        let mut rate = steps.rate;
        ComboBox::from_id_salt(("step_rate", clip.id))
            .selected_text(rate_label(rate))
            .width(100.0)
            .show_ui(ui, |ui| {
                for r in RATES {
                    ui.selectable_value(&mut rate, r, rate_label(r));
                }
            });
        if rate != steps.rate {
            let r = actions::edit_steps(&app.session, clip.id, None, |s| s.rate = rate);
            app.report(r);
        }

        ui.separator();
        ui.label(RichText::new("Swing").color(theme::TEXT_WEAK));
        let mut amount = steps.swing.amount;
        let r1 = ui
            .add(
                DragValue::new(&mut amount)
                    .range(Swing::STRAIGHT..=Swing::MAX)
                    .speed(0.2)
                    .suffix(" %"),
            )
            .on_hover_text("Where every second step lands within its pair: 50 is straight");
        let mut grid = steps.swing.grid;
        ComboBox::from_id_salt(("step_swing_grid", clip.id))
            .selected_text(format!("1/{grid}"))
            .width(60.0)
            .show_ui(ui, |ui| {
                for g in Swing::GRIDS {
                    ui.selectable_value(&mut grid, g, format!("1/{g}"));
                }
            });
        let swing = Swing::new(amount, grid);
        if swing != steps.swing {
            let key = r1.changed().then(|| app.gestures.key(r1.id));
            let r = actions::edit_steps(&app.session, clip.id, key, |s| s.swing = swing);
            app.report(r);
        }

        ui.separator();
        ui.label(RichText::new("Seed").color(theme::TEXT_WEAK));
        let mut seed = steps.seed;
        let r2 = ui
            .add(
                DragValue::new(&mut seed)
                    .range(0..=u32::MAX as u64)
                    .speed(0.2),
            )
            .on_hover_text("Changes which steps a probability below 100% plays");
        if seed != steps.seed {
            let key = app.gestures.key(r2.id);
            let r = actions::edit_steps(&app.session, clip.id, Some(key), |s| s.seed = seed);
            app.report(r);
        }

        ui.separator();
        if steps.lanes.len() < MAX_LANES {
            let length = steps.lanes.last().map_or(16, StepLane::len);
            if drums {
                ui.menu_button("Add lane", |ui| {
                    for (note, name) in DRUM_PIECES {
                        if ui.button(name).clicked() {
                            let r = actions::edit_steps(&app.session, clip.id, None, |s| {
                                s.lanes.push(StepLane::new(note, length));
                            });
                            app.report(r);
                            ui.close();
                        }
                    }
                });
            } else if ui.button("Add lane").clicked() {
                let note = steps.lanes.last().map_or(48, |l| l.note);
                let r = actions::edit_steps(&app.session, clip.id, None, |s| {
                    s.lanes.push(StepLane::new(note, length));
                });
                app.report(r);
            }
        }
    });
}

fn grid(app: &mut App, ui: &mut Ui, track: &Track, clip: &Clip, steps: &StepClip, drums: bool) {
    let color = theme::track_color(track.color);
    let modifiers = ui.input(|i| i.modifiers);
    let mut change: Change = None;
    let mut select = None;
    let lanes = steps.lanes.len();
    app.steps.cells.clear();
    for (li, lane) in steps.lanes.iter().enumerate() {
        ui.horizontal(|ui| {
            lane_controls(app, ui, clip, lane, li, lanes, drums, &mut change);
            for (si, step) in lane.steps.iter().enumerate() {
                if si > 0 && si % 4 == 0 {
                    ui.add_space(GAP * 2.0);
                }
                let (rect, resp) = ui.allocate_exact_size(vec2(CELL, CELL), Sense::click());
                app.steps.cells.push(((li, si), rect));
                let selected = app.steps.selected == Some((clip.id, li, si));
                paint_step(ui, rect, step, lane, si, color, selected, drums);
                if resp.clicked() {
                    select = Some((clip.id, li, si));
                    let kind = step.kind;
                    change = Some(if modifiers.shift {
                        Box::new(move |s: &mut StepClip| {
                            if let Some(st) = step_mut(s, li, si) {
                                st.accent = !st.accent;
                                st.kind = StepKind::Note;
                            }
                        })
                    } else if modifiers.alt {
                        Box::new(move |s: &mut StepClip| {
                            if let Some(st) = step_mut(s, li, si) {
                                st.slide = !st.slide;
                                st.kind = StepKind::Note;
                            }
                        })
                    } else if modifiers.command || modifiers.ctrl {
                        Box::new(move |s: &mut StepClip| {
                            if let Some(st) = step_mut(s, li, si) {
                                st.kind = if kind == StepKind::Tie {
                                    StepKind::Rest
                                } else {
                                    StepKind::Tie
                                };
                            }
                        })
                    } else {
                        Box::new(move |s: &mut StepClip| {
                            if let Some(st) = step_mut(s, li, si) {
                                st.kind = if kind == StepKind::Note {
                                    StepKind::Rest
                                } else {
                                    StepKind::Note
                                };
                            }
                        })
                    });
                }
                let tip = sky_model::steps::format_step(step, lane.note);
                resp.on_hover_text(format!("Step {}: {tip}", si + 1));
            }
        });
    }
    if let Some(sel) = select {
        app.steps.selected = Some(sel);
    }
    if let Some(edit) = change {
        let r = actions::edit_steps(&app.session, clip.id, None, edit);
        app.report(r);
    }
}

fn step_mut(steps: &mut StepClip, lane: usize, step: usize) -> Option<&mut Step> {
    steps.lanes.get_mut(lane)?.steps.get_mut(step)
}

#[allow(clippy::too_many_arguments)]
fn lane_controls(
    app: &mut App,
    ui: &mut Ui,
    clip: &Clip,
    lane: &StepLane,
    li: usize,
    lanes: usize,
    drums: bool,
    change: &mut Change,
) {
    let label = lane_label(drums, lane.note);
    let mut note = lane.note;
    ComboBox::from_id_salt(("lane_note", clip.id, li))
        .selected_text(label)
        .width(96.0)
        .height(320.0)
        .show_ui(ui, |ui| {
            if drums {
                for (n, name) in DRUM_PIECES {
                    ui.selectable_value(&mut note, n, name);
                }
            } else {
                for n in (24..=84).rev() {
                    ui.selectable_value(&mut note, n, note_name(n));
                }
            }
        });
    if note != lane.note {
        *change = Some(Box::new(move |s: &mut StepClip| {
            if let Some(l) = s.lanes.get_mut(li) {
                l.note = note;
            }
        }));
    }
    let mut len = lane.len();
    let r = ui
        .add(
            DragValue::new(&mut len)
                .range(1..=MAX_STEPS)
                .speed(0.1)
                .suffix(" st"),
        )
        .on_hover_text("Steps in this lane; lanes of different lengths drift against each other");
    if len != lane.len() {
        let key = app.gestures.key(r.id);
        let res = actions::edit_steps(&app.session, clip.id, Some(key), |s| {
            if let Some(l) = s.lanes.get_mut(li) {
                l.resize(len);
            }
        });
        app.report(res);
    }
    let mut gate = lane.gate;
    let r = ui
        .add(
            DragValue::new(&mut gate)
                .range(1..=100)
                .speed(0.3)
                .suffix(" %"),
        )
        .on_hover_text("How long notes hold, as a share of a step");
    if gate != lane.gate {
        let key = app.gestures.key(r.id);
        let res = actions::edit_steps(&app.session, clip.id, Some(key), |s| {
            if let Some(l) = s.lanes.get_mut(li) {
                l.gate = gate;
            }
        });
        app.report(res);
    }
    let mute = ui
        .selectable_label(lane.mute, "M")
        .on_hover_text("Mute this lane");
    if mute.clicked() {
        *change = Some(Box::new(move |s: &mut StepClip| {
            if let Some(l) = s.lanes.get_mut(li) {
                l.mute = !l.mute;
            }
        }));
    }
    if lanes > 1
        && ui
            .small_button("×")
            .on_hover_text("Remove this lane")
            .clicked()
    {
        *change = Some(Box::new(move |s: &mut StepClip| {
            if li < s.lanes.len() {
                s.lanes.remove(li);
            }
        }));
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_step(
    ui: &Ui,
    rect: Rect,
    step: &Step,
    lane: &StepLane,
    index: usize,
    color: Color32,
    selected: bool,
    drums: bool,
) {
    let p = ui.painter();
    let base = if (index / 4).is_multiple_of(2) {
        theme::LANE
    } else {
        theme::LANE_ALT
    };
    p.rect_filled(rect, CornerRadius::same(3), base);
    match step.kind {
        StepKind::Rest => {}
        StepKind::Tie => {
            let bar = Rect::from_center_size(rect.center(), vec2(rect.width() + GAP * 2.0, 6.0));
            p.rect_filled(bar, CornerRadius::same(1), theme::darken(color, 0.2));
        }
        StepKind::Note => {
            let fill = if lane.mute {
                theme::darken(color, 0.55)
            } else if step.accent {
                theme::lighten(color, 0.35)
            } else {
                color
            };
            // A probability below 100% fills only part of the step.
            let share = (f32::from(step.probability) / 100.0).max(0.25);
            let inner = rect.shrink(2.0);
            let body = Rect::from_min_max(
                pos2(inner.left(), inner.bottom() - inner.height() * share),
                inner.max,
            );
            p.rect_filled(body, CornerRadius::same(2), fill);
            if step.accent {
                p.rect_stroke(
                    inner,
                    CornerRadius::same(2),
                    Stroke::new(1.0, Color32::WHITE),
                    StrokeKind::Inside,
                );
            }
            if !drums {
                let key = step.key(lane.note);
                let name = note_name(key);
                p.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '-'),
                    FontId::proportional(9.0),
                    Color32::BLACK,
                );
            }
            if step.slide {
                p.line_segment(
                    [
                        pos2(rect.right() - 7.0, rect.bottom() - 3.0),
                        pos2(rect.right() - 1.0, rect.top() + 3.0),
                    ],
                    Stroke::new(1.5, Color32::WHITE),
                );
            }
            if step.ratchet > 1 {
                for d in 0..step.ratchet {
                    let x = rect.left() + 3.0 + f32::from(d) * 2.5;
                    p.circle_filled(pos2(x, rect.top() + 3.0), 0.9, Color32::WHITE);
                }
            }
            if !step.condition.is_always() {
                p.text(
                    pos2(rect.left() + 2.0, rect.bottom() - 1.0),
                    Align2::LEFT_BOTTOM,
                    step.condition.to_string(),
                    FontId::proportional(7.0),
                    Color32::WHITE,
                );
            }
            if !step.locks.is_empty() {
                p.circle_filled(pos2(rect.right() - 3.0, rect.top() + 3.0), 1.6, theme::WARN);
            }
        }
    }
    if selected {
        p.rect_stroke(
            rect,
            CornerRadius::same(3),
            Stroke::new(2.0, theme::ACCENT),
            StrokeKind::Outside,
        );
    }
}

/// The settings of the selected step.
#[allow(clippy::too_many_arguments)]
fn inspector(
    app: &mut App,
    ui: &mut Ui,
    track: &Track,
    clip: &Clip,
    steps: &StepClip,
    li: usize,
    si: usize,
    step: &Step,
    drums: bool,
) {
    let lane = &steps.lanes[li];
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "{} · step {}",
                lane_label(drums, lane.note),
                si + 1
            ))
            .color(theme::TEXT),
        );
    });
    let mut next = step.clone();
    let mut gesture = None;
    egui::Grid::new(("step_inspector", clip.id))
        .num_columns(4)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            ui.label(RichText::new("Plays").color(theme::TEXT_WEAK));
            ui.horizontal(|ui| {
                ui.selectable_value(&mut next.kind, StepKind::Note, "Note");
                ui.selectable_value(&mut next.kind, StepKind::Tie, "Tie")
                    .on_hover_text("Holds the previous note through this step");
                ui.selectable_value(&mut next.kind, StepKind::Rest, "Rest");
            });
            ui.label(RichText::new("Key").color(theme::TEXT_WEAK));
            ui.horizontal(|ui| {
                let mut key = step.key(lane.note);
                let r = ui.add(
                    DragValue::new(&mut key)
                        .range(0..=127)
                        .speed(0.15)
                        .custom_formatter(|n, _| note_name(n as u8))
                        .custom_parser(|s| {
                            sky_core::music::parse_note_name(s)
                                .ok()
                                .map(f64::from)
                                .or_else(|| s.parse().ok())
                        }),
                );
                if ui.small_button("−oct").clicked() {
                    key = key.saturating_sub(12);
                }
                if ui.small_button("+oct").clicked() {
                    key = (key + 12).min(127);
                }
                if key != step.key(lane.note) {
                    next.note = (key != lane.note).then_some(key);
                    if r.changed() {
                        gesture = Some(r.id);
                    }
                }
            });
            ui.end_row();

            ui.label(RichText::new("Velocity").color(theme::TEXT_WEAK));
            let r = ui.add(DragValue::new(&mut next.velocity).range(1..=127).speed(0.3));
            if r.changed() {
                gesture = Some(r.id);
            }
            ui.label(RichText::new("Accent").color(theme::TEXT_WEAK));
            ui.horizontal(|ui| {
                ui.checkbox(&mut next.accent, "")
                    .on_hover_text("Plays at full velocity; the Acid bass also plays brighter");
                ui.label(RichText::new("Slide").color(theme::TEXT_WEAK));
                ui.checkbox(&mut next.slide, "")
                    .on_hover_text("Holds into the next note, so the Acid bass glides to it");
            });
            ui.end_row();

            ui.label(RichText::new("Chance").color(theme::TEXT_WEAK));
            let r = ui.add(
                DragValue::new(&mut next.probability)
                    .range(0..=100)
                    .speed(0.3)
                    .suffix(" %"),
            );
            if r.changed() {
                gesture = Some(r.id);
            }
            ui.label(RichText::new("Condition").color(theme::TEXT_WEAK));
            ComboBox::from_id_salt(("step_condition", clip.id))
                .selected_text(condition_label(next.condition))
                .width(150.0)
                .show_ui(ui, |ui| {
                    for c in Condition::choices() {
                        ui.selectable_value(&mut next.condition, c, condition_label(c));
                    }
                })
                .response
                .on_hover_text("Plays only on some passes through the lane");
            ui.end_row();

            ui.label(RichText::new("Ratchet").color(theme::TEXT_WEAK));
            let r = ui
                .add(
                    DragValue::new(&mut next.ratchet)
                        .range(1..=MAX_RATCHET)
                        .speed(0.05)
                        .suffix("×"),
                )
                .on_hover_text("Hits in the step");
            if r.changed() {
                gesture = Some(r.id);
            }
            ui.label(RichText::new("Timing").color(theme::TEXT_WEAK));
            let r = ui
                .add(
                    DragValue::new(&mut next.nudge)
                        .range(-MAX_NUDGE..=MAX_NUDGE)
                        .speed(0.3)
                        .suffix(" %"),
                )
                .on_hover_text("Plays late (positive) or early, as a share of a step");
            if r.changed() {
                gesture = Some(r.id);
            }
            ui.end_row();
        });
    locks_ui(ui, track, clip, steps, &mut next, &mut gesture);
    // Turning a step off keeps its settings for when it comes back.
    if next != *step {
        let key = gesture.map(|id| app.gestures.key(id));
        let r = actions::edit_steps(&app.session, clip.id, key, |s| {
            if let Some(st) = step_mut(s, li, si) {
                *st = next;
            }
        });
        app.report(r);
    }
}

/// The selected step's parameter locks.
fn locks_ui(
    ui: &mut Ui,
    track: &Track,
    clip: &Clip,
    steps: &StepClip,
    next: &mut Step,
    gesture: &mut Option<egui::Id>,
) {
    let Some(slot) = &track.instrument else {
        return;
    };
    let specs = slot.kind.params();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Locks").color(theme::TEXT_WEAK));
        ui.label(
            RichText::new("instrument settings for this step's notes")
                .small()
                .color(theme::TEXT_DIM),
        );
    });
    let mut remove = None;
    for (name, value) in next.locks.iter_mut() {
        ui.horizontal(|ui| {
            match sky_dsp::params::find(specs, name) {
                Some(spec) => {
                    ui.label(spec.label);
                    let r = ui.add(
                        DragValue::new(value)
                            .range(spec.min..=spec.max)
                            .speed(((spec.max - spec.min) / 300.0).max(0.001))
                            .suffix(if spec.unit.is_empty() {
                                String::new()
                            } else {
                                format!(" {}", spec.unit)
                            }),
                    );
                    if r.changed() {
                        *gesture = Some(r.id);
                    }
                }
                None => {
                    ui.label(RichText::new(name.as_str()).color(theme::TEXT_DIM))
                        .on_hover_text(
                            "This instrument has no such parameter, so the lock does nothing",
                        );
                    ui.label(format_number(f64::from(*value)));
                }
            }
            if ui
                .small_button("×")
                .on_hover_text("Remove this lock")
                .clicked()
            {
                remove = Some(name.clone());
            }
        });
    }
    if let Some(name) = remove {
        next.locks.remove(&name);
    }
    let locked = steps.locked_params();
    let full = locked.len() >= MAX_LOCKED_PARAMS;
    let options: Vec<_> = specs
        .iter()
        .filter(|s| !next.locks.contains_key(s.name))
        .filter(|s| !full || locked.contains(&s.name))
        .collect();
    if !options.is_empty() {
        let mut picked = None;
        ComboBox::from_id_salt(("add_lock", clip.id))
            .selected_text("Lock parameter…")
            .width(150.0)
            .height(320.0)
            .show_ui(ui, |ui| {
                for spec in &options {
                    if ui.selectable_label(false, spec.label).clicked() {
                        picked = Some(*spec);
                    }
                }
            });
        if let Some(spec) = picked {
            // Start from the track's own value, so nothing changes yet.
            let value = slot.param(spec.name).unwrap_or(spec.default);
            next.locks.insert(spec.name.to_string(), value);
        }
    }
}
