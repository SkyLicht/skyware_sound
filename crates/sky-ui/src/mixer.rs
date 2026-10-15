// SPDX-License-Identifier: GPL-3.0-or-later
//! The mixer: one strip per track plus the master, with fader, pan, mute,
//! solo, a peak meter and the effect chain.

use std::collections::HashMap;

use egui::{
    Align, Color32, ComboBox, Context, CornerRadius, Frame, Id, Layout, Margin, RichText,
    ScrollArea, Sense, Ui, Window, vec2,
};
use sky_core::{EffectId, TrackId};
use sky_dsp::EffectType;
use sky_model::{Channel, ChannelRef, EffectSlot, Project};

use crate::actions::{self, ChannelChange};
use crate::app::{Action, App};
use crate::params::param_grid;
use crate::theme;
use crate::widgets::{self, MeterLevel};

const STRIP_W: f32 = 132.0;

#[derive(Default)]
pub struct MixerState {
    pub meters: HashMap<TrackId, MeterLevel>,
    pub master: MeterLevel,
    /// The effect whose parameter window is open.
    pub open_effect: Option<(ChannelRef, EffectId)>,
}

impl MixerState {
    /// True while any meter still shows something, so it keeps falling smoothly.
    pub fn any_active(&self) -> bool {
        self.master.is_active() || self.meters.values().any(MeterLevel::is_active)
    }
}

pub fn channel_name(project: &Project, channel: ChannelRef) -> String {
    match channel {
        ChannelRef::Master => "Master".to_string(),
        ChannelRef::Track(id) => project
            .track(id)
            .map_or_else(|_| "Track".to_string(), |t| t.name.clone()),
    }
}

pub fn channel_of(project: &Project, channel: ChannelRef) -> Option<&Channel> {
    match channel {
        ChannelRef::Master => Some(&project.master),
        ChannelRef::Track(id) => project.track(id).ok().map(|t| &t.channel),
    }
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let view = app.view.clone();
    let project = &view.project;
    ScrollArea::horizontal()
        .id_salt("mixer")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for track in &project.tracks {
                    let meter = app.mixer.meters.get(&track.id).copied().unwrap_or_default();
                    let selected = app.sel.track == Some(track.id);
                    strip(
                        app,
                        ui,
                        project,
                        ChannelRef::Track(track.id),
                        &track.name,
                        theme::track_color(track.color),
                        &track.channel,
                        &meter,
                        selected,
                    );
                }
                if project.tracks.is_empty() {
                    ui.label(RichText::new("No tracks yet.").color(theme::TEXT_WEAK));
                }
                ui.separator();
                let meter = app.mixer.master;
                strip(
                    app,
                    ui,
                    project,
                    ChannelRef::Master,
                    "Master",
                    theme::ACCENT,
                    &project.master,
                    &meter,
                    false,
                );
            });
        });
}

#[allow(clippy::too_many_arguments)]
fn strip(
    app: &mut App,
    ui: &mut Ui,
    project: &Project,
    channel_ref: ChannelRef,
    name: &str,
    color: Color32,
    channel: &Channel,
    meter: &MeterLevel,
    selected: bool,
) {
    let frame = Frame::new()
        .fill(if selected {
            theme::HEADER_SELECTED
        } else {
            theme::HEADER
        })
        .corner_radius(CornerRadius::same(5))
        .inner_margin(Margin::same(6));
    frame.show(ui, |ui| {
        ui.set_width(STRIP_W);
        ui.set_min_height(ui.available_height());
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            // Name with the track color underneath.
            let (rect, _) = ui.allocate_exact_size(vec2(STRIP_W, 3.0), Sense::hover());
            ui.painter().rect_filled(rect, CornerRadius::same(1), color);
            let label = ui
                .add(
                    egui::Label::new(RichText::new(name).strong().color(theme::TEXT))
                        .truncate()
                        .sense(Sense::click()),
                )
                .on_hover_text("Click to select");
            if let ChannelRef::Track(id) = channel_ref
                && label.clicked()
            {
                app.sel.select_track(id);
            }

            effects_section(app, ui, project, channel_ref, channel);

            // Pan knob.
            ui.horizontal(|ui| {
                let mut pan = channel.pan;
                let r = widgets::knob(ui, &mut pan, -1.0..=1.0, 0.0, 30.0)
                    .on_hover_text("Pan: drag up or right, double-click to centre");
                if r.changed() && pan != channel.pan {
                    let key = app.gestures.key(r.id);
                    let res = actions::set_channel(
                        &app.session,
                        channel_ref,
                        ChannelChange {
                            pan: Some(pan.clamp(-1.0, 1.0)),
                            ..Default::default()
                        },
                        Some(key),
                    );
                    app.report(res);
                }
                ui.vertical(|ui| {
                    ui.label(RichText::new("Pan").small().color(theme::TEXT_DIM));
                    ui.label(
                        RichText::new(widgets::format_pan(channel.pan))
                            .small()
                            .color(theme::TEXT_WEAK),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    if let ChannelRef::Track(_) = channel_ref
                        && widgets::toggle(ui, channel.solo, "S", theme::ACCENT, "Solo").clicked()
                    {
                        let r = actions::set_channel(
                            &app.session,
                            channel_ref,
                            ChannelChange {
                                solo: Some(!channel.solo),
                                ..Default::default()
                            },
                            None,
                        );
                        app.report(r);
                    }
                    if widgets::toggle(ui, channel.mute, "M", theme::MUTE, "Mute").clicked() {
                        let r = actions::set_channel(
                            &app.session,
                            channel_ref,
                            ChannelChange {
                                mute: Some(!channel.mute),
                                ..Default::default()
                            },
                            None,
                        );
                        app.report(r);
                    }
                });
            });

            // Fader and meter.
            let fader_h = (ui.available_height() - 22.0).clamp(70.0, 280.0);
            ui.horizontal(|ui| {
                let mut db = channel.volume_db;
                let r = widgets::fader(ui, &mut db, vec2(40.0, fader_h));
                if r.changed() && db != channel.volume_db {
                    let key = app.gestures.key(r.id);
                    let res = actions::set_channel(
                        &app.session,
                        channel_ref,
                        ChannelChange {
                            volume_db: Some(db),
                            ..Default::default()
                        },
                        Some(key),
                    );
                    app.report(res);
                }
                widgets::meter(ui, meter, vec2(18.0, fader_h));
                ui.vertical(|ui| {
                    ui.add_space(fader_h - 34.0);
                    let peak = meter.hold[0].max(meter.hold[1]);
                    let text = if peak > 0.0 {
                        format!("{:+.1}", sky_dsp::util::gain_to_db(peak))
                    } else {
                        "-inf".to_string()
                    };
                    let c = if peak >= 1.0 {
                        theme::ERROR
                    } else {
                        theme::TEXT_DIM
                    };
                    ui.label(RichText::new(text).small().color(c));
                });
            });
            ui.label(
                RichText::new(widgets::format_db(channel.volume_db))
                    .monospace()
                    .color(theme::TEXT_WEAK),
            );
        });
    });
}

fn effects_section(
    app: &mut App,
    ui: &mut Ui,
    project: &Project,
    channel_ref: ChannelRef,
    channel: &Channel,
) {
    for (i, effect) in channel.effects.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let mut on = effect.enabled;
            if ui
                .checkbox(&mut on, "")
                .on_hover_text(if effect.enabled { "Bypass" } else { "Enable" })
                .changed()
            {
                let r = actions::set_effect_enabled(&app.session, channel_ref, effect.id, on);
                app.report(r);
            }
            let open = app.mixer.open_effect == Some((channel_ref, effect.id));
            let text = RichText::new(effect.kind.label()).color(if effect.enabled {
                theme::TEXT
            } else {
                theme::TEXT_DIM
            });
            let w = (ui.available_width() - 22.0).max(30.0);
            if ui
                .add_sized([w, 20.0], egui::Button::selectable(open, text).truncate())
                .on_hover_text(format!(
                    "{} (slot {})\nClick to edit",
                    effect.kind.description(),
                    i + 1
                ))
                .clicked()
            {
                app.mixer.open_effect = if open {
                    None
                } else {
                    Some((channel_ref, effect.id))
                };
            }
            if ui
                .add(egui::Button::new(RichText::new("×").color(theme::TEXT_WEAK)).small())
                .on_hover_text("Remove effect")
                .clicked()
            {
                let r = actions::remove_effect(&app.session, channel_ref, effect.id);
                app.report(r);
            }
        });
    }
    ui.menu_button(RichText::new("+ Effect").color(theme::TEXT_WEAK), |ui| {
        for kind in EffectType::ALL {
            if ui
                .button(kind.label())
                .on_hover_text(kind.description())
                .clicked()
            {
                let ctx = ui.ctx().clone();
                app.run_action(
                    &ctx,
                    Action::AddEffect {
                        channel: channel_ref,
                        kind,
                    },
                );
            }
        }
    })
    .response
    .on_hover_text(format!(
        "Add an effect to {}",
        channel_name(project, channel_ref)
    ));
}

/// The floating parameter editor of the open effect.
/// Picks the track whose notes drive an effect such as a duck.
fn sidechain_row(
    app: &mut App,
    ui: &mut Ui,
    project: &Project,
    channel_ref: ChannelRef,
    effect: &EffectSlot,
) {
    ui.horizontal(|ui| {
        ui.label("Follows");
        let name_of = |id: TrackId| {
            project
                .track(id)
                .map_or_else(|_| "(deleted track)".to_string(), |t| t.name.clone())
        };
        let mut chosen = effect.sidechain;
        ComboBox::from_id_salt(("sidechain", effect.id))
            .selected_text(chosen.map_or_else(|| "(nothing)".to_string(), name_of))
            .width(180.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut chosen, None, "(nothing)");
                for t in project
                    .tracks
                    .iter()
                    .filter(|t| t.kind == sky_model::TrackKind::Instrument)
                {
                    ui.selectable_value(&mut chosen, Some(t.id), &t.name);
                }
            })
            .response
            .on_hover_text(
                "The track whose notes start each dip. It works even when that track is muted.",
            );
        if chosen != effect.sidechain {
            let r = actions::set_effect_sidechain(&app.session, channel_ref, effect.id, chosen);
            app.report(r);
        }
    });
}

pub fn effect_window(app: &mut App, ctx: &Context) {
    let Some((channel_ref, effect_id)) = app.mixer.open_effect else {
        return;
    };
    let view = app.view.clone();
    let project = &view.project;
    let Some(channel) = channel_of(project, channel_ref) else {
        app.mixer.open_effect = None;
        return;
    };
    let Some((index, effect)) = channel
        .effects
        .iter()
        .enumerate()
        .find(|(_, e)| e.id == effect_id)
    else {
        app.mixer.open_effect = None;
        return;
    };
    let count = channel.effects.len();
    let mut open = true;
    let mut changes = Vec::new();
    Window::new(format!(
        "{} · {}",
        channel_name(project, channel_ref),
        effect.kind.label()
    ))
    .id(Id::new("effect_window"))
    .open(&mut open)
    .collapsible(false)
    .resizable(false)
    .pivot(egui::Align2::CENTER_CENTER)
    .default_pos(ctx.content_rect().center())
    .default_pos(ctx.content_rect().center() - vec2(180.0, 200.0))
    .show(ctx, |ui| {
        ui.label(
            RichText::new(effect.kind.description())
                .small()
                .color(theme::TEXT_WEAK),
        );
        ui.horizontal(|ui| {
            let mut on = effect.enabled;
            if ui.checkbox(&mut on, "Enabled").changed() {
                let r = actions::set_effect_enabled(&app.session, channel_ref, effect_id, on);
                app.report(r);
            }
            if ui
                .add_enabled(index > 0, egui::Button::new("Move up"))
                .clicked()
            {
                let r = actions::move_effect(&app.session, channel_ref, effect_id, index - 1);
                app.report(r);
            }
            if ui
                .add_enabled(index + 1 < count, egui::Button::new("Move down"))
                .clicked()
            {
                let r = actions::move_effect(&app.session, channel_ref, effect_id, index + 1);
                app.report(r);
            }
            if ui
                .button(RichText::new("Remove").color(theme::ERROR))
                .clicked()
            {
                let r = actions::remove_effect(&app.session, channel_ref, effect_id);
                app.report(r);
            }
        });
        if effect.kind.takes_sidechain() {
            sidechain_row(app, ui, project, channel_ref, effect);
        }
        ui.separator();
        changes = param_grid(
            ui,
            &mut app.gestures,
            ("fx", effect_id),
            effect.kind.params(),
            |name| effect.param(name).unwrap_or(0.0),
        );
    });
    for ch in changes {
        let r = actions::set_effect_param(
            &app.session,
            channel_ref,
            effect_id,
            ch.name,
            ch.value,
            ch.key,
        );
        app.report(r);
    }
    if !open {
        app.mixer.open_effect = None;
    }
}
