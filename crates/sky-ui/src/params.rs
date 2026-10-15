// SPDX-License-Identifier: GPL-3.0-or-later
//! Parameter editors generated from instrument and effect parameter specs,
//! and the Instrument tab.

use std::hash::Hash;

use egui::{ComboBox, Grid, Id, RichText, ScrollArea, Sense, Slider, Ui};
use sky_dsp::{InstrumentType, ParamKind, ParamSpec};
use sky_model::{InstrumentSlot, TrackKind};

use crate::actions;
use crate::app::{Action, App};
use crate::theme;
use crate::widgets::Gestures;

/// One parameter change asked for by the user.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamChange {
    pub name: &'static str,
    /// `None` restores the default.
    pub value: Option<f32>,
    /// Coalescing key: every change of one slider drag shares it.
    pub key: Option<u64>,
}

fn decimals(spec: &ParamSpec) -> usize {
    let span = (spec.max - spec.min).abs();
    if span >= 200.0 {
        0
    } else if span >= 20.0 {
        1
    } else {
        2
    }
}

/// Draws a grid of controls for `specs` and returns what changed.
pub fn param_grid(
    ui: &mut Ui,
    gestures: &mut Gestures,
    salt: impl Hash + Copy + std::fmt::Debug,
    specs: &'static [ParamSpec],
    value_of: impl Fn(&str) -> f32,
) -> Vec<ParamChange> {
    let mut changes = Vec::new();
    Grid::new(Id::new(("params", salt)))
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            for spec in specs {
                let current = value_of(spec.name);
                let tip = match spec.kind {
                    ParamKind::Continuous { .. } => format!(
                        "{}: {} to {}{}\nDouble-click or right-click to reset to {}",
                        spec.name,
                        spec.min,
                        spec.max,
                        unit_suffix(spec.unit),
                        spec.default
                    ),
                    _ => format!("{}\nDouble-click or right-click to reset", spec.name),
                };
                let label = ui
                    .add(
                        egui::Label::new(RichText::new(spec.label).color(theme::TEXT))
                            .sense(Sense::click()),
                    )
                    .on_hover_text(tip);
                if (label.double_clicked() || label.secondary_clicked()) && current != spec.default
                {
                    changes.push(ParamChange {
                        name: spec.name,
                        value: None,
                        key: None,
                    });
                }
                match spec.kind {
                    ParamKind::Continuous { log } => {
                        let mut v = current;
                        let mut slider = Slider::new(&mut v, spec.min..=spec.max)
                            .logarithmic(log)
                            .max_decimals(decimals(spec))
                            .suffix(unit_suffix(spec.unit));
                        if log && spec.min <= 0.0 {
                            slider = slider
                                .smallest_positive(((spec.max - spec.min) * 1e-3).max(1e-3) as f64);
                        }
                        let resp = ui.add(slider);
                        if resp.changed() && v != current {
                            let key = gestures.key(resp.id);
                            changes.push(ParamChange {
                                name: spec.name,
                                value: Some(v.clamp(spec.min, spec.max)),
                                key: Some(key),
                            });
                        }
                    }
                    ParamKind::Choice { options } => {
                        let idx = current.round().max(0.0) as usize;
                        let mut selected = idx;
                        ComboBox::from_id_salt((salt, spec.name))
                            .selected_text(options.get(idx).copied().unwrap_or("?"))
                            .show_ui(ui, |ui| {
                                for (i, o) in options.iter().enumerate() {
                                    ui.selectable_value(&mut selected, i, *o);
                                }
                            });
                        if selected != idx {
                            changes.push(ParamChange {
                                name: spec.name,
                                value: Some(selected as f32),
                                key: None,
                            });
                        }
                    }
                    ParamKind::Toggle => {
                        let mut on = current >= 0.5;
                        if ui.checkbox(&mut on, "").changed() {
                            changes.push(ParamChange {
                                name: spec.name,
                                value: Some(if on { 1.0 } else { 0.0 }),
                                key: None,
                            });
                        }
                    }
                }
                ui.end_row();
            }
        });
    changes
}

fn unit_suffix(unit: &str) -> String {
    if unit.is_empty() {
        String::new()
    } else {
        format!(" {unit}")
    }
}

/// The Instrument tab: the selected track's instrument parameters.
pub fn instrument_panel(app: &mut App, ui: &mut Ui) {
    let view = app.view.clone();
    let project = &view.project;
    let Some(track) = app.sel.track.and_then(|t| project.track(t).ok()) else {
        ui.label(
            RichText::new("Select an instrument track to edit its sound.").color(theme::TEXT_WEAK),
        );
        return;
    };
    if track.kind != TrackKind::Instrument {
        ui.label(
            RichText::new(format!(
                "“{}” is an audio track; it has no instrument.",
                track.name
            ))
            .color(theme::TEXT_WEAK),
        );
        return;
    }
    let slot = track
        .instrument
        .clone()
        .unwrap_or_else(|| InstrumentSlot::new(InstrumentType::Synth));
    let track_id = track.id;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(&track.name)
                .heading()
                .color(theme::track_color(track.color)),
        );
        let mut kind = slot.kind;
        ComboBox::from_id_salt(("instrument_kind", track_id))
            .selected_text(kind.label())
            .show_ui(ui, |ui| {
                for k in InstrumentType::ALL {
                    ui.selectable_value(&mut kind, k, k.label())
                        .on_hover_text(k.description());
                }
            });
        if kind != slot.kind {
            let r = actions::set_instrument(
                &app.session,
                track_id,
                InstrumentSlot::new(kind),
                &format!("Change instrument to {}", kind.label()),
            );
            app.report(r);
        }
        if ui
            .button("Reset all")
            .on_hover_text("Set every parameter back to its default")
            .clicked()
            && !slot.params.is_empty()
        {
            let mut fresh = slot.clone();
            fresh.params.clear();
            let r = actions::set_instrument(&app.session, track_id, fresh, "Reset instrument");
            app.report(r);
        }
        ui.label(
            RichText::new(slot.kind.description())
                .small()
                .color(theme::TEXT_DIM),
        );
    });

    if slot.kind == InstrumentType::Sampler {
        ui.horizontal(|ui| {
            ui.label("Sample");
            let current = slot
                .sample
                .and_then(|id| project.pool_entry(id).ok())
                .map_or_else(|| "(none)".to_string(), |e| e.name.clone());
            let mut chosen = slot.sample;
            ComboBox::from_id_salt(("sampler_sample", track_id))
                .selected_text(current)
                .width(220.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut chosen, None, "(none)");
                    for e in &project.audio_pool {
                        ui.selectable_value(&mut chosen, Some(e.id), &e.name);
                    }
                });
            if chosen != slot.sample {
                let mut next = slot.clone();
                next.sample = chosen;
                let r = actions::set_instrument(&app.session, track_id, next, "Choose sample");
                app.report(r);
            }
            if ui
                .button("Import…")
                .on_hover_text("Import an audio file into the pool and load it here")
                .clicked()
            {
                let ctx = ui.ctx().clone();
                app.run_action(&ctx, Action::ImportAudio);
            }
            if project.audio_pool.is_empty() {
                ui.label(
                    RichText::new("The audio pool is empty.")
                        .small()
                        .color(theme::TEXT_DIM),
                );
            }
        });
    }
    if slot.kind == InstrumentType::Bank {
        crate::sounds::bank_panel(app, ui, track_id, &slot);
    }
    ui.add_space(2.0);
    crate::audition::keyboard(app, ui, track);
    ui.separator();

    let specs = slot.kind.params();
    let columns = ((ui.available_width() / 360.0).floor() as usize).clamp(1, 3);
    let per = specs.len().div_ceil(columns).max(1);
    let mut changes = Vec::new();
    ScrollArea::vertical()
        .id_salt(("instrument_params", track_id))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.columns(columns, |cols| {
                for (c, col) in cols.iter_mut().enumerate() {
                    let from = (c * per).min(specs.len());
                    let to = ((c + 1) * per).min(specs.len());
                    let Some(chunk) = specs.get(from..to) else {
                        continue;
                    };
                    changes.extend(param_grid(
                        col,
                        &mut app.gestures,
                        ("inst", track_id, c),
                        chunk,
                        |name| slot.param(name).unwrap_or(0.0),
                    ));
                }
            });
        });
    for ch in changes {
        let r = actions::set_instrument_param(&app.session, track_id, ch.name, ch.value, ch.key);
        app.report(r);
    }
}
