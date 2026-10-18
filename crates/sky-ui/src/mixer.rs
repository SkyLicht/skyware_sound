// SPDX-License-Identifier: GPL-3.0-or-later
//! The mixer: one strip per track plus the master, with fader, pan, mute,
//! solo, a peak meter and the effect chain, and the master's loudness.
//! Every strip shows as many insert slots as the busiest one, so the
//! faders line up, and the faders take the height the panel has.

use std::collections::HashMap;

use egui::{
    Align, Color32, ComboBox, Context, CornerRadius, DragAndDrop, Frame, Id, LayerId, Layout,
    Margin, Order, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind, Ui, UiBuilder, Window,
    pos2, vec2,
};
use sky_core::{EffectId, TrackId};
use sky_dsp::EffectType;
use sky_dsp::loudness::Loudness;
use sky_engine::{EngineShared, Reduction};
use sky_model::{Channel, ChannelRef, EffectSlot, Project};

use crate::actions::{self, ChannelChange};
use crate::app::{Action, App};
use crate::fx_view::{self, REDUCTION_RANGE, ReductionLevel};
use crate::params::{Edits, param_grid};
use crate::pictures::{self, PictureState, Style, Target};
use crate::presets::{self, Device, Kind};
use crate::theme;
use crate::widgets::{self, MeterLevel};

const STRIP_W: f32 = 132.0;
/// The picture at the top of a strip, when the panel has room for it.
const PICTURE_H: f32 = 52.0;
/// With less room than this the picture is a small tile beside the name.
const PICTURE_MIN: f32 = 34.0;
/// The small tile beside the name.
const PICTURE_SMALL: f32 = 20.0;
/// What a strip needs besides its picture, insert slots and fader: the
/// name, the pan row, the level under the fader and the gaps between.
const STRIP_REST: f32 = 105.0;
/// The fader keeps this much height before the picture takes any, which
/// leaves room for the master's loudness readouts beside it.
const FADER_ROOM: f32 = 130.0;
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
    /// The effects whose windows are open, oldest first.
    pub open_effects: Vec<(ChannelRef, EffectId)>,
    /// A window to bring to the front next frame.
    raise: Option<(ChannelRef, EffectId)>,
    pub loudness: LoudnessState,
    /// The gain reduction of each compressor, limiter and duck.
    pub reductions: HashMap<(ChannelRef, EffectId), ReductionLevel>,
}

impl MixerState {
    pub fn is_open(&self, effect: (ChannelRef, EffectId)) -> bool {
        self.open_effects.contains(&effect)
    }

    /// Opens an effect's window, or brings it to the front.
    pub fn open_effect(&mut self, effect: (ChannelRef, EffectId)) {
        if !self.is_open(effect) {
            self.open_effects.push(effect);
        }
        self.raise = Some(effect);
    }

    pub fn close_effect(&mut self, effect: (ChannelRef, EffectId)) {
        self.open_effects.retain(|e| *e != effect);
    }

    /// True while any meter still shows something, so it keeps falling smoothly.
    pub fn any_active(&self) -> bool {
        self.master.is_active()
            || self.meters.values().any(MeterLevel::is_active)
            || self.loudness.meter.momentary().is_some()
            || self.reductions.values().any(ReductionLevel::is_active)
    }

    /// Reads the gain reduction the engine measured in a channel's inserts
    /// since the last call; `slots` are its meters by insert position.
    /// Adds the inserts it saw to `seen`.
    pub fn update_reductions(
        &mut self,
        channel_ref: ChannelRef,
        channel: &Channel,
        slots: Option<&[Reduction]>,
        dt: f32,
        seen: &mut Vec<(ChannelRef, EffectId)>,
    ) {
        for (i, fx) in channel.effects.iter().enumerate() {
            if !fx.kind.reduces_gain() {
                continue;
            }
            let db = slots.and_then(|s| s.get(i)).map_or(0.0, Reduction::take);
            let key = (channel_ref, fx.id);
            self.reductions.entry(key).or_default().update(db, dt);
            seen.push(key);
        }
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
            // The picture, then the name. A short panel keeps its height
            // for the fader, and the picture becomes a small tile beside
            // the name.
            let effects_h = slots as f32 * (SLOT_H + SLOT_GAP) - SLOT_GAP;
            let room = ui.available_height() - STRIP_REST - effects_h - FADER_ROOM;
            let in_strip = ui.rect_contains_pointer(ui.max_rect());
            let big = room >= PICTURE_MIN;
            if big {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(width, room.min(PICTURE_H)), Sense::hover());
                strip_picture(app, ui, project, channel_ref, rect, Style::Banner, in_strip);
            }
            let label = ui
                .horizontal(|ui| {
                    if !big {
                        let (rect, _) = ui
                            .allocate_exact_size(egui::Vec2::splat(PICTURE_SMALL), Sense::hover());
                        strip_picture(app, ui, project, channel_ref, rect, Style::Tile, in_strip);
                    }
                    ui.add(
                        egui::Label::new(RichText::new(name).strong().color(theme::TEXT))
                            .truncate()
                            .sense(Sense::click()),
                    )
                })
                .inner
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

/// A strip's picture: a track's opens the picture editor, the master's
/// is only shown.
fn strip_picture(
    app: &mut App,
    ui: &mut Ui,
    project: &Project,
    channel_ref: ChannelRef,
    rect: Rect,
    style: Style,
    in_strip: bool,
) {
    let radius = if style == Style::Banner { 5.0 } else { 4.0 };
    match channel_ref {
        ChannelRef::Track(id) => {
            if let Ok(track) = project.track(id) {
                let shown = app.pictures.track_shown(project, track);
                let target = Target::Track(id);
                pictures::button(
                    app, ui, "mixer", rect, radius, &shown, target, style, in_strip,
                );
            }
        }
        ChannelRef::Master => pictures::paint(
            ui.painter(),
            rect,
            radius,
            &PictureState::master_shown(),
            &mut app.pictures.images,
            style,
        ),
    }
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
            if let Some(level) = app.mixer.reductions.get(&(channel_ref, effect.id))
                && level.db > 0.0
            {
                // A thin bar along the slot's foot shows how hard it works.
                let frac = (level.db / REDUCTION_RANGE).clamp(0.0, 1.0);
                let foot = Rect::from_min_size(
                    pos2(rect.left() + 2.0, rect.bottom() - 3.0),
                    vec2((rect.width() - 4.0) * frac, 2.0),
                );
                ui.painter()
                    .rect_filled(foot, CornerRadius::same(1), theme::REDUCTION);
            }
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
    insert_drop(app, ui, area, project, channel_ref, channel);
}

/// An insert being dragged to another place in its strip or another one.
#[derive(Clone, Copy, Debug, PartialEq)]
struct InsertDrag {
    channel: ChannelRef,
    id: EffectId,
    kind: EffectType,
}

/// While an insert is dragged over a channel's slots, shows where it would
/// go with a line, and moves it there when let go.
fn insert_drop(
    app: &mut App,
    ui: &Ui,
    area: Rect,
    project: &Project,
    channel_ref: ChannelRef,
    channel: &Channel,
) {
    let ctx = ui.ctx().clone();
    let Some(drag) = DragAndDrop::payload::<InsertDrag>(&ctx) else {
        return;
    };
    let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) else {
        return;
    };
    let pitch = SLOT_H + SLOT_GAP;
    if !area.expand2(vec2(0.0, pitch / 2.0)).contains(pos) {
        return;
    }
    let count = channel.effects.len();
    // The gap between slots nearest the pointer, and where the insert
    // lands counted without it.
    let gap = (((pos.y - area.top() + SLOT_GAP / 2.0) / pitch)
        .round()
        .max(0.0) as usize)
        .min(count);
    let same = drag.channel == channel_ref;
    let from = if same {
        channel.effects.iter().position(|e| e.id == drag.id)
    } else {
        None
    };
    let index = match from {
        Some(i) if gap > i => gap - 1,
        _ => gap,
    };
    let moves = from != Some(index);
    if moves {
        let y = area.top() + gap as f32 * pitch - SLOT_GAP / 2.0;
        ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("insert_drop")))
            .hline(area.x_range(), y, Stroke::new(2.0, theme::ACCENT));
    }
    if !ctx.input(|i| i.pointer.any_released()) {
        return;
    }
    DragAndDrop::clear_payload(&ctx);
    if !moves {
        return;
    }
    let r = if same {
        actions::move_effect(&app.session, channel_ref, drag.id, index)
    } else {
        let Some(slot) = channel_of(project, drag.channel).and_then(|c| c.effect(drag.id)) else {
            return;
        };
        actions::move_effect_to(
            &app.session,
            drag.channel,
            slot.clone(),
            channel_ref,
            index,
            &format!(
                "Move {} to {}",
                drag.kind.label(),
                channel_name(project, channel_ref)
            ),
        )
    };
    if app.report(r).is_some() && !same && app.mixer.is_open((drag.channel, drag.id)) {
        // Its window follows it.
        app.mixer.close_effect((drag.channel, drag.id));
        app.mixer.open_effect((channel_ref, drag.id));
    }
}

/// The name of the insert being dragged, beside the pointer. Once per
/// frame, after everything else is drawn.
fn insert_drag_label(app: &App, ctx: &Context) {
    let (Some(drag), Some(pos)) = (
        DragAndDrop::payload::<InsertDrag>(ctx),
        ctx.input(|i| i.pointer.latest_pos()),
    ) else {
        return;
    };
    let from = channel_name(&app.view.project, drag.channel);
    egui::Area::new(Id::new("insert_drag_label"))
        .order(Order::Tooltip)
        .fixed_pos(pos + vec2(14.0, 10.0))
        .interactable(false)
        .show(ctx, |ui| {
            Frame::popup(ui.style())
                .inner_margin(Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(format!("{from} · {}", drag.kind.label()))
                            .wrap_mode(egui::TextWrapMode::Extend),
                    );
                });
        });
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
    let key = (channel_ref, effect.id);
    let open = app.mixer.is_open(key);
    let text = RichText::new(effect.kind.label()).color(if effect.enabled {
        theme::TEXT
    } else {
        theme::TEXT_DIM
    });
    let w = (ui.available_width() - 22.0).max(30.0);
    let button = ui
        .add_sized(
            [w, 20.0],
            egui::Button::selectable(open, text)
                .truncate()
                .sense(Sense::click_and_drag()),
        )
        .on_hover_text(format!(
            "{} (slot {})\nClick to edit, drag to move",
            effect.kind.description(),
            i + 1
        ));
    button.dnd_set_drag_payload(InsertDrag {
        channel: channel_ref,
        id: effect.id,
        kind: effect.kind,
    });
    if button.clicked() {
        if open {
            app.mixer.close_effect(key);
        } else {
            app.mixer.open_effect(key);
        }
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

/// The open effect windows, each where it was left, the newest on top;
/// and the label of an insert being dragged.
pub fn effect_windows(app: &mut App, ctx: &Context) {
    let open = app.mixer.open_effects.clone();
    for (i, key) in open.into_iter().enumerate() {
        effect_window(app, ctx, key, i);
    }
    if let Some(key) = app.mixer.raise.take() {
        ctx.move_to_top(LayerId::new(Order::Middle, window_id(key)));
    }
    insert_drag_label(app, ctx);
}

fn window_id((channel, effect): (ChannelRef, EffectId)) -> Id {
    Id::new(("effect_window", channel, effect))
}

/// One effect's window. `nth` staggers where new windows open.
fn effect_window(app: &mut App, ctx: &Context, key: (ChannelRef, EffectId), nth: usize) {
    let (channel_ref, effect_id) = key;
    let view = app.view.clone();
    let project = &view.project;
    let Some((index, effect, count)) = channel_of(project, channel_ref).and_then(|channel| {
        let (i, e) = channel
            .effects
            .iter()
            .enumerate()
            .find(|(_, e)| e.id == effect_id)?;
        Some((i, e, channel.effects.len()))
    }) else {
        app.mixer.close_effect(key);
        return;
    };
    let mut open = true;
    let mut changes = Vec::new();
    let step = (nth % 8) as f32;
    Window::new(format!(
        "{} · {}",
        channel_name(project, channel_ref),
        effect.kind.label()
    ))
    .id(window_id(key))
    .open(&mut open)
    .collapsible(false)
    .resizable(false)
    .pivot(egui::Align2::CENTER_CENTER)
    .default_pos(ctx.content_rect().center() - vec2(180.0, 200.0) + vec2(48.0, 32.0) * step)
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
        presets::bar(
            app,
            ui,
            Device::Effect(channel_ref, effect_id),
            Kind::Effect(effect.kind),
            &effect.params,
            effect.compare.as_ref(),
        );
        if effect.kind.takes_sidechain() {
            sidechain_row(app, ui, project, channel_ref, effect);
        }
        ui.separator();
        let value_of = |name: &str| effect.param(name).unwrap_or(0.0);
        if fx_view::has_view(effect.kind) {
            let mut edits = Edits::new(effect.kind.params(), &mut app.gestures, &value_of);
            fx_view::effect_view(
                ui,
                effect_id,
                effect.kind,
                &mut edits,
                project.tempo.initial_bpm(),
                app.mixer.reductions.get(&key),
            );
            changes = edits.changes;
            ui.separator();
        }
        changes.extend(param_grid(
            ui,
            &mut app.gestures,
            ("fx", effect_id),
            effect.kind.params(),
            value_of,
        ));
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
        app.mixer.close_effect(key);
    }
}
