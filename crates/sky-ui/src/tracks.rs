// SPDX-License-Identifier: GPL-3.0-or-later
//! Track headers: name, color, mute, solo and volume, left of each lane.

use egui::{
    Align, CornerRadius, Id, Key, Layout, Rect, RichText, Sense, Stroke, TextEdit, Ui, UiBuilder,
    pos2, vec2,
};
use sky_model::{ChannelRef, Project, Track, TrackKind};

use crate::actions::{self, ChannelChange};
use crate::app::{App, Tab};
use crate::theme;
use crate::widgets::{self, FADER_FLOOR_DB, SILENT_DB};

pub fn kind_label(track: &Track) -> &'static str {
    match (&track.kind, &track.instrument) {
        (TrackKind::Audio, _) => "Audio",
        (TrackKind::Instrument, Some(i)) => match i.kind {
            sky_dsp::InstrumentType::Synth => "Synth",
            sky_dsp::InstrumentType::Drums => "Drums",
            sky_dsp::InstrumentType::Sampler => "Sampler",
            sky_dsp::InstrumentType::Bank => "Sample bank",
            sky_dsp::InstrumentType::Acid => "Acid bass",
        },
        (TrackKind::Instrument, None) => "No instrument",
    }
}

pub fn header(
    app: &mut App,
    ui: &mut Ui,
    rect: Rect,
    project: &Project,
    track: &Track,
    index: usize,
) {
    let selected = app.sel.track == Some(track.id);
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        CornerRadius::ZERO,
        if selected {
            theme::HEADER_SELECTED
        } else {
            theme::HEADER
        },
    );
    let strip = Rect::from_min_max(rect.min, pos2(rect.left() + 4.0, rect.bottom()));
    painter.rect_filled(strip, CornerRadius::ZERO, theme::track_color(track.color));
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, theme::BG),
    );
    painter.vline(
        rect.right() - 0.5,
        rect.y_range(),
        Stroke::new(1.0, theme::BG),
    );
    crate::widgets::ai_glow(
        &painter,
        rect.shrink(1.0),
        2,
        app.ai.track_glow(track, app.now),
    );

    let bg = ui.interact(rect, Id::new(("header", track.id)), Sense::click());
    if bg.clicked() {
        app.sel.select_track(track.id);
    }
    if bg.double_clicked() && track.kind == TrackKind::Instrument {
        app.sel.select_track(track.id);
        app.workspace.show(Tab::Instrument);
    }
    bg.context_menu(|ui| header_menu(app, ui, project, track, index));

    let inner = Rect::from_min_max(
        pos2(rect.left() + 12.0, rect.top() + 6.0),
        pos2(rect.right() - 8.0, rect.bottom() - 5.0),
    );
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::top_down(Align::Min)),
    );
    child.spacing_mut().item_spacing = vec2(5.0, 5.0);
    first_row(app, &mut child, track);
    second_row(app, &mut child, track);
}

fn first_row(app: &mut App, ui: &mut Ui, track: &Track) {
    ui.horizontal(|ui| {
        let mut rgb = track.color;
        let r = ui
            .color_edit_button_srgb(&mut rgb)
            .on_hover_text("Track color");
        if r.changed() && rgb != track.color {
            let key = app.gestures.key(r.id);
            let res = actions::set_track_color(&app.session, track.id, rgb, Some(key));
            app.report(res);
        }

        let renaming = app
            .arrange
            .renaming
            .as_ref()
            .is_some_and(|(id, _)| *id == track.id);
        let buttons_w = 50.0;
        if renaming {
            let mut done = None;
            if let Some((_, text)) = app.arrange.renaming.as_mut() {
                let edit = TextEdit::singleline(text)
                    .desired_width((ui.available_width() - buttons_w).max(40.0))
                    .id(Id::new(("rename", track.id)));
                let resp = ui.add(edit);
                if app.arrange.rename_focus {
                    resp.request_focus();
                    app.arrange.rename_focus = false;
                }
                if resp.lost_focus() {
                    let cancelled = ui.input(|i| i.key_pressed(Key::Escape));
                    done = Some((!cancelled).then(|| text.clone()));
                }
            }
            if let Some(result) = done {
                app.arrange.renaming = None;
                if let Some(name) = result
                    && name.trim() != track.name
                {
                    let r = actions::rename_track(&app.session, track.id, &name);
                    app.report(r);
                }
            }
        } else {
            let name = egui::Label::new(RichText::new(&track.name).strong().color(theme::TEXT))
                .truncate()
                .sense(Sense::click());
            let width = (ui.available_width() - buttons_w).max(30.0);
            let resp = ui
                .add_sized([width, 18.0], name)
                .on_hover_text("Double-click to rename");
            if resp.clicked() {
                app.sel.select_track(track.id);
            }
            if resp.double_clicked() {
                start_rename(app, track);
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let ch = &track.channel;
            if widgets::toggle(ui, ch.solo, "S", theme::ACCENT, "Solo").clicked() {
                let r = actions::set_channel(
                    &app.session,
                    ChannelRef::Track(track.id),
                    ChannelChange {
                        solo: Some(!ch.solo),
                        ..Default::default()
                    },
                    None,
                );
                app.report(r);
            }
            if widgets::toggle(ui, ch.mute, "M", theme::MUTE, "Mute").clicked() {
                let r = actions::set_channel(
                    &app.session,
                    ChannelRef::Track(track.id),
                    ChannelChange {
                        mute: Some(!ch.mute),
                        ..Default::default()
                    },
                    None,
                );
                app.report(r);
            }
        });
    });
}

fn second_row(app: &mut App, ui: &mut Ui, track: &Track) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [52.0, 18.0],
            egui::Label::new(
                RichText::new(kind_label(track))
                    .small()
                    .color(theme::TEXT_WEAK),
            )
            .truncate(),
        );
        ui.spacing_mut().slider_width = (ui.available_width() - 64.0).max(40.0);
        let db = track.channel.volume_db;
        let mut v = if db <= FADER_FLOOR_DB {
            FADER_FLOOR_DB - 1.0
        } else {
            db
        };
        let slider = egui::Slider::new(&mut v, (FADER_FLOOR_DB - 1.0)..=12.0)
            .show_value(true)
            .custom_formatter(|v, _| {
                if v <= (FADER_FLOOR_DB - 0.5) as f64 {
                    "-inf".to_string()
                } else {
                    format!("{v:.1}")
                }
            })
            .custom_parser(|s| {
                let s = s.trim().trim_end_matches("dB").trim();
                if s.eq_ignore_ascii_case("-inf") {
                    Some((FADER_FLOOR_DB - 1.0) as f64)
                } else {
                    s.parse().ok()
                }
            });
        let resp = ui.add(slider).on_hover_text("Volume in dB");
        if resp.changed() {
            let new = if v <= FADER_FLOOR_DB - 0.5 {
                SILENT_DB
            } else {
                v
            };
            if new != db {
                let key = app.gestures.key(resp.id);
                let r = actions::set_channel(
                    &app.session,
                    ChannelRef::Track(track.id),
                    ChannelChange {
                        volume_db: Some(new),
                        ..Default::default()
                    },
                    Some(key),
                );
                app.report(r);
            }
        }
    });
}

pub fn start_rename(app: &mut App, track: &Track) {
    app.arrange.renaming = Some((track.id, track.name.clone()));
    app.arrange.rename_focus = true;
}

fn header_menu(app: &mut App, ui: &mut Ui, project: &Project, track: &Track, index: usize) {
    if ui.button("Rename").clicked() {
        start_rename(app, track);
    }
    if track.kind == TrackKind::Instrument && ui.button("Edit instrument").clicked() {
        app.sel.select_track(track.id);
        app.workspace.show(Tab::Instrument);
    }
    ui.separator();
    if ui
        .add_enabled(index > 0, egui::Button::new("Move up"))
        .clicked()
    {
        let r = actions::move_track(&app.session, track.id, index - 1);
        app.report(r);
    }
    if ui
        .add_enabled(
            index + 1 < project.tracks.len(),
            egui::Button::new("Move down"),
        )
        .clicked()
    {
        let r = actions::move_track(&app.session, track.id, index + 1);
        app.report(r);
    }
    ui.separator();
    if ui
        .button(RichText::new("Delete track").color(theme::ERROR))
        .clicked()
    {
        let r = actions::delete_track(&app.session, track.id);
        if app.report(r).is_some() {
            app.info(format!("Deleted “{}” (Ctrl+Z to undo)", track.name));
        }
    }
}
