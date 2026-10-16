// SPDX-License-Identifier: GPL-3.0-or-later
//! The mixer: one strip per track plus the master, with fader, pan, mute,
//! solo, a peak meter and the effect chain, and the master's loudness.
//! Every strip shows as many insert slots as the busiest one, so the
//! faders line up, and the faders take the height the panel has.

use std::collections::HashMap;

use egui::{
    Align, Color32, ComboBox, Context, CornerRadius, Frame, Id, Layout, Margin, Rect, RichText,
    ScrollArea, Sense, Stroke, StrokeKind, Ui, UiBuilder, Window, pos2, vec2,
};
use sky_core::{EffectId, TrackId};
use sky_dsp::EffectType;
use sky_dsp::loudness::Loudness;
use sky_engine::EngineShared;
use sky_model::{Channel, ChannelRef, EffectSlot, Project};

use crate::actions::{self, ChannelChange};
use crate::app::{Action, App};
use crate::params::param_grid;
use crate::theme;
use crate::widgets::{self, MeterLevel};

const STRIP_W: f32 = 132.0;
/// The master's loudness meter and readout, beside its fader.
const LOUDNESS_W: f32 = 104.0;
/// One insert slot, and the gap under it.
const SLOT_H: f32 = 22.0;
const SLOT_GAP: f32 = 3.0;
/// Insert slots every strip shows at least, so the first effect added
/// doesn't move the faders.
const MIN_SLOTS: usize = 2;
/// The fader never gets shorter than this.
const FADER_MIN: f32 = 70.0;
/// Bottom and top of the loudness meter's scale, in LUFS.
const LUFS_FLOOR: f64 = -48.0;
const LUFS_TOP: f64 = 0.0;
/// True peaks above this are marked: encoders for streaming and games
/// can clip them.
const TRUE_PEAK_CEILING_DB: f32 = -1.0;

#[derive(Default)]
pub struct MixerState {
    pub meters: HashMap<TrackId, MeterLevel>,
    pub master: MeterLevel,
    /// The effect whose parameter window is open.
    pub open_effect: Option<(ChannelRef, EffectId)>,
    pub loudness: LoudnessState,
}

impl MixerState {
    /// True while any meter still shows something, so it keeps falling smoothly.
    pub fn any_active(&self) -> bool {
        self.master.is_active()
            || self.meters.values().any(MeterLevel::is_active)
            || self.loudness.meter.momentary().is_some()
    }
}

/// The master's loudness, from the engine's 100 ms blocks.
#[derive(Default)]
pub struct LoudnessState {
    /// Blocks read so far.
    seen: u64,
    pub meter: Loudness,
    /// The highest true peak since play started or the reset, as gain.
    pub true_peak: f32,
    was_playing: bool,
}

impl LoudnessState {
    /// Reads the blocks and true peak the engine measured since the last
    /// call. Pressing play starts the integrated loudness and the true
    /// peak again.
    pub fn update(&mut self, shared: Option<&EngineShared>, playing: bool) {
        if playing && !self.was_playing {
            self.reset();
        }
        self.was_playing = playing;
        let Some(shared) = shared else {
            return;
        };
        let meter = &mut self.meter;
        shared
            .loudness
            .read(&mut self.seen, |b| meter.push(b.power, b.playing));
        let (l, r) = shared.master_true_peak.take();
        self.true_peak = self.true_peak.max(l).max(r);
    }

    /// Starts the integrated loudness and the highest true peak again.
    pub fn reset(&mut self) {
        self.meter.reset_integrated();
        self.true_peak = 0.0;
    }
}

/// Insert slots each strip shows: one more than the busiest channel has,
/// so there is always room to add one.
pub fn slot_count(project: &Project) -> usize {
    let most = project
        .tracks
        .iter()
        .map(|t| t.channel.effects.len())
        .chain([project.master.effects.len()])
        .max()
        .unwrap_or(0);
    (most + 1).max(MIN_SLOTS)
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
    let slots = slot_count(project);
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
                        Strip {
                            project,
                            channel_ref: ChannelRef::Track(track.id),
                            name: &track.name,
                            color: theme::track_color(track.color),
                            channel: &track.channel,
                            meter: &meter,
                            selected,
                            slots,
                        },
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
                    Strip {
                        project,
                        channel_ref: ChannelRef::Master,
                        name: "Master",
                        color: theme::ACCENT,
                        channel: &project.master,
                        meter: &meter,
                        selected: false,
                        slots,
                    },
                );
            });
        });
}

/// What a strip shows.
struct Strip<'a> {
    project: &'a Project,
    channel_ref: ChannelRef,
    name: &'a str,
    color: Color32,
    channel: &'a Channel,
    meter: &'a MeterLevel,
    selected: bool,
    /// Insert slots to show, the same on every strip.
    slots: usize,
}

fn strip(app: &mut App, ui: &mut Ui, s: Strip) {
    let Strip {
        project,
        channel_ref,
        name,
        color,
        channel,
        meter,
        selected,
        slots,
    } = s;
    let master = channel_ref == ChannelRef::Master;
    let width = STRIP_W + if master { LOUDNESS_W } else { 0.0 };
    let frame = Frame::new()
        .fill(if selected {
            theme::HEADER_SELECTED
        } else {
            theme::HEADER
        })
        .corner_radius(CornerRadius::same(5))
        .inner_margin(Margin::same(6));
    let response = frame.show(ui, |ui| {
        ui.set_width(width);
        ui.set_min_height(ui.available_height());
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            // Name with the track color underneath.
            let (rect, _) = ui.allocate_exact_size(vec2(width, 3.0), Sense::hover());
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

            effects_section(app, ui, project, channel_ref, channel, slots);

            // Pan knob.
            ui.horizontal(|ui| {
                ui.set_width(STRIP_W);
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

            // Fader and meter, as tall as the panel allows.
            let fader_h = (ui.available_height() - 22.0).max(FADER_MIN);
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
                    ui.set_width(44.0);
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
                if master {
                    loudness_ui(app, ui, fader_h);
                }
            });
            ui.label(
                RichText::new(widgets::format_db(channel.volume_db))
                    .monospace()
                    .color(theme::TEXT_WEAK),
            );
        });
    });
    let glow = app.ai.channel_glow(channel_ref, app.now);
    widgets::ai_glow(ui.painter(), response.response.rect, 5, glow);
}

/// The insert slots: the channel's effects, then "+ Effect", then empty
/// slots up to `slots`.
fn effects_section(
    app: &mut App,
    ui: &mut Ui,
    project: &Project,
    channel_ref: ChannelRef,
    channel: &Channel,
    slots: usize,
) {
    let height = slots as f32 * (SLOT_H + SLOT_GAP) - SLOT_GAP;
    let (area, _) = ui.allocate_exact_size(vec2(STRIP_W, height), Sense::hover());
    for i in 0..slots {
        let top = area.top() + i as f32 * (SLOT_H + SLOT_GAP);
        let rect = Rect::from_min_size(pos2(area.left(), top), vec2(STRIP_W, SLOT_H));
        let mut slot = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        slot.spacing_mut().item_spacing.x = 3.0;
        if let Some(effect) = channel.effects.get(i) {
            effect_slot(app, &mut slot, channel_ref, effect, i);
            let glow = app.ai.effect_glow(channel_ref, effect.id, app.now);
            widgets::ai_glow(ui.painter(), rect.expand(1.0), 3, glow);
        } else if i == channel.effects.len() {
            add_effect_button(app, &mut slot, project, channel_ref);
        } else {
            ui.painter().rect_stroke(
                rect.shrink(1.0),
                CornerRadius::same(3),
                Stroke::new(1.0, theme::GRID_FAINT),
                StrokeKind::Inside,
            );
        }
    }
}

fn effect_slot(app: &mut App, ui: &mut Ui, channel_ref: ChannelRef, effect: &EffectSlot, i: usize) {
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
}

fn add_effect_button(app: &mut App, ui: &mut Ui, project: &Project, channel_ref: ChannelRef) {
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

/// The master's loudness: a meter of the momentary loudness with a line
/// at the integrated, and the numbers beside it.
fn loudness_ui(app: &mut App, ui: &mut Ui, height: f32) {
    let st = &app.mixer.loudness;
    let momentary = st.meter.momentary();
    let short_term = st.meter.short_term();
    let integrated = st.meter.integrated();
    let true_peak = st.true_peak;
    let (rect, bar) = ui.allocate_exact_size(vec2(14.0, height), Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::same(2), theme::BG);
        let y_of = |lufs: f64| {
            let frac = ((lufs - LUFS_FLOOR) / (LUFS_TOP - LUFS_FLOOR)).clamp(0.0, 1.0) as f32;
            rect.bottom() - 1.0 - frac * (rect.height() - 2.0)
        };
        for mark in [-12.0, -24.0, -36.0] {
            painter.hline(
                rect.left()..=rect.left() + 3.0,
                y_of(mark),
                Stroke::new(1.0, theme::TEXT_DIM),
            );
        }
        if let Some(m) = momentary {
            let fill = Rect::from_min_max(
                pos2(rect.left() + 3.0, y_of(m)),
                pos2(rect.right() - 2.0, rect.bottom() - 1.0),
            );
            painter.rect_filled(fill, CornerRadius::ZERO, theme::GAME_LOOP);
        }
        if let Some(i) = integrated {
            painter.hline(
                rect.left()..=rect.right(),
                y_of(i),
                Stroke::new(2.0, theme::TEXT),
            );
        }
        painter.rect_stroke(
            rect,
            CornerRadius::same(2),
            Stroke::new(1.0, theme::GRID_BEAT),
            StrokeKind::Inside,
        );
    }
    bar.on_hover_text(format!(
        "Loudness, {LUFS_FLOOR:.0} to {LUFS_TOP:.0} LUFS: the bar is the last 0.4 s, the line the integrated loudness"
    ));
    let lufs = |v: Option<f64>| v.map_or_else(|| "–".to_string(), |v| format!("{v:.1}"));
    let tp_db = sky_dsp::util::gain_to_db(true_peak);
    let tp = if true_peak > 0.0 {
        format!("{tp_db:+.1}")
    } else {
        "–".to_string()
    };
    let over = true_peak > 0.0 && tp_db > TRUE_PEAK_CEILING_DB;
    // The numbers keep to the fader's height, so the master's level
    // lines up with the other strips whatever the panel's height.
    let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let mut col = ui.new_child(
        UiBuilder::new()
            .max_rect(area)
            .layout(Layout::top_down(Align::Min)),
    );
    col.set_clip_rect(area.intersect(ui.clip_rect()));
    col.spacing_mut().item_spacing.y = 1.0;
    let row = |ui: &mut Ui, name: &str, value: String, unit: &str, color: Color32, tip: &str| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.add_sized(
                [16.0, 16.0],
                egui::Label::new(RichText::new(name).small().color(theme::TEXT_DIM)),
            );
            ui.label(RichText::new(value).monospace().color(color));
            ui.label(RichText::new(unit).small().color(theme::TEXT_DIM));
        })
        .response
        .on_hover_text(tip);
    };
    row(
        &mut col,
        "M",
        lufs(momentary),
        "",
        theme::TEXT,
        "Momentary loudness in LUFS: the last 0.4 seconds",
    );
    row(
        &mut col,
        "S",
        lufs(short_term),
        "",
        theme::TEXT,
        "Short-term loudness in LUFS: the last 3 seconds",
    );
    row(
        &mut col,
        "I",
        lufs(integrated),
        "LUFS",
        theme::TEXT,
        "Integrated loudness since play started or Reset. Quiet passages are left \
         out, as EBU R128 measures it, the same as game exports report.",
    );
    row(
        &mut col,
        "TP",
        tp,
        "dBTP",
        if over { theme::ERROR } else { theme::TEXT },
        "Highest true peak since play started or Reset, found between the samples. \
         Keep it under -1 dBTP so encoders don't clip it.",
    );
    col.add_space(3.0);
    let reset = col
        .small_button("Reset")
        .on_hover_text("Start the integrated loudness and the true peak again")
        .clicked();
    if reset {
        app.mixer.loudness.reset();
    }
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
