// SPDX-License-Identifier: GPL-3.0-or-later
//! The Editor tab: a header for the selected clip, then the piano roll, the
//! pattern editor or the audio clip settings, depending on the clip.

use egui::{
    Align2, Color32, CornerRadius, DragValue, Id, RichText, Sense, Slider, Stroke, TextEdit, Ui,
    vec2,
};
use sky_core::{ClipId, PPQ, Ticks};
use sky_model::{Clip, ClipContent, Project, Track};
use sky_pattern::Time;

use crate::actions::{self, AudioClipChange};
use crate::app::App;
use crate::theme;

#[derive(Default)]
pub struct PatternState {
    pub clip: Option<ClipId>,
    pub text: String,
    /// The source, seed and cycle the text box was loaded from.
    synced: Option<(String, u64, Ticks)>,
    /// The clip name being edited.
    pub name: Option<(ClipId, String)>,
}

pub fn clip_editor(app: &mut App, ui: &mut Ui) {
    let view = app.view.clone();
    let project = &view.project;
    let Some((track, clip)) = app.sel.clip.and_then(|c| project.find_clip(c).ok()) else {
        ui.add_space(8.0);
        ui.label(RichText::new("Select a clip to edit it here.").color(theme::TEXT_WEAK));
        ui.label(
            RichText::new(
                "Double-click an empty spot on an instrument track to create a one-bar MIDI clip, \
                 or right-click a lane for more.",
            )
            .small()
            .color(theme::TEXT_DIM),
        );
        return;
    };
    clip_header(app, ui, project, track, clip);
    match &clip.content {
        ClipContent::Midi { notes } => {
            crate::piano_roll::show(app, ui, project, track, clip, notes)
        }
        ClipContent::Pattern {
            source,
            seed,
            cycle,
        } => pattern_ui(app, ui, project, clip, source, *seed, *cycle),
        ClipContent::Audio {
            source,
            gain_db,
            fade_in,
            fade_out,
            ..
        } => audio_ui(
            app, ui, project, clip, *source, *gain_db, *fade_in, *fade_out,
        ),
    }
}

fn clip_header(app: &mut App, ui: &mut Ui, project: &Project, track: &Track, clip: &Clip) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(10.0, 18.0), Sense::hover());
        ui.painter()
            .rect_filled(rect, CornerRadius::same(2), theme::track_color(track.color));
        ui.label(RichText::new(&track.name).color(theme::TEXT_WEAK));
        ui.label(RichText::new("›").color(theme::TEXT_DIM));

        // Clip name.
        let editing = app
            .pattern
            .name
            .as_ref()
            .is_some_and(|(id, _)| *id == clip.id);
        if !editing {
            app.pattern.name = Some((clip.id, clip.name.clone()));
        }
        let mut commit = None;
        if let Some((_, text)) = app.pattern.name.as_mut() {
            let resp = ui.add(
                TextEdit::singleline(text)
                    .desired_width(160.0)
                    .id(Id::new(("clip_name", clip.id))),
            );
            if resp.lost_focus() && text.trim() != clip.name {
                commit = Some(text.clone());
            }
            if !resp.has_focus() && !resp.lost_focus() && *text != clip.name {
                *text = clip.name.clone();
            }
        }
        if let Some(name) = commit {
            let r = actions::rename_clip(&app.session, clip.id, &name);
            app.report(r);
        }

        ui.separator();
        let tpb = project.ticks_per_bar().max(1) as f64;
        ui.label(RichText::new("Length").color(theme::TEXT_WEAK));
        let mut bars = clip.length as f64 / tpb;
        let resp = ui
            .add(
                DragValue::new(&mut bars)
                    .range(1.0 / 16.0..=1024.0)
                    .speed(0.02)
                    .max_decimals(3)
                    .suffix(" bars"),
            )
            .on_hover_text("Clip length in bars");
        if resp.changed() {
            let len = ((bars * tpb).round() as Ticks).max(1);
            if len != clip.length {
                let key = app.gestures.key(resp.id);
                let r = actions::apply(
                    &app.session,
                    sky_model::Transaction::single(sky_model::Command::SetClipLength {
                        id: clip.id,
                        length: len,
                    }),
                    Some(key),
                );
                app.report(r);
            }
        }
        ui.label(
            RichText::new(format!(
                "starts at {}",
                project.time_signature.format_position(clip.start)
            ))
            .small()
            .color(theme::TEXT_DIM),
        );
    });
    ui.add_space(2.0);
}

/// The pattern's parse result, for the status line.
#[derive(Debug)]
pub enum ParseStatus {
    Ok { events: usize },
    Error { message: String, column: usize },
}

pub fn parse_status(source: &str, seed: u64) -> ParseStatus {
    match sky_pattern::parse(source) {
        Ok(p) => ParseStatus::Ok {
            events: p
                .query(Time::from_integer(0), Time::from_integer(1), seed)
                .len(),
        },
        Err(e) => ParseStatus::Error {
            message: e.message,
            column: e.column,
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn pattern_ui(
    app: &mut App,
    ui: &mut Ui,
    project: &Project,
    clip: &Clip,
    source: &str,
    seed: u64,
    cycle: Ticks,
) {
    let text_id = Id::new(("pattern_text", clip.id));
    let focused = ui.ctx().memory(|m| m.has_focus(text_id));
    let current = (source.to_string(), seed, cycle);
    let st = &mut app.pattern;
    if st.clip != Some(clip.id) || (!focused && st.synced.as_ref() != Some(&current)) {
        st.clip = Some(clip.id);
        st.text = source.to_string();
        st.synced = Some(current);
    }

    ui.label(RichText::new("Pattern").color(theme::TEXT_WEAK));
    let resp = ui.add(
        TextEdit::singleline(&mut app.pattern.text)
            .id(text_id)
            .font(theme::mono(15.0))
            .desired_width(f32::INFINITY)
            .hint_text("bd*4, ~ sd ~ sd, hh*8"),
    );
    let status = parse_status(&app.pattern.text, seed);
    ui.horizontal(|ui| match &status {
        ParseStatus::Ok { events } => {
            ui.label(
                RichText::new(format!(
                    "Valid · {events} event{} per cycle",
                    if *events == 1 { "" } else { "s" }
                ))
                .color(theme::OK),
            );
            if app.pattern.text != source {
                ui.label(
                    RichText::new("Press Enter to apply")
                        .small()
                        .color(theme::TEXT_DIM),
                );
            }
        }
        ParseStatus::Error { message, column } => {
            ui.label(RichText::new(format!("Column {column}: {message}")).color(theme::ERROR));
        }
    });
    if let ParseStatus::Error { column, .. } = &status {
        // A caret under the offending column, in the same monospace font.
        let caret = format!("{}^", " ".repeat(column.saturating_sub(1)));
        ui.label(
            RichText::new(caret)
                .font(theme::mono(15.0))
                .color(theme::ERROR),
        );
    }
    if resp.lost_focus() && app.pattern.text != source {
        match &status {
            ParseStatus::Ok { .. } => {
                let text = app.pattern.text.clone();
                let r = actions::set_pattern(&app.session, clip.id, &text, seed, cycle, None);
                if app.report(r).is_some() {
                    app.pattern.synced = Some((text, seed, cycle));
                }
            }
            ParseStatus::Error { message, column } => {
                app.error(format!("Pattern not applied: {message} at column {column}"));
            }
        }
    }

    ui.horizontal(|ui| {
        let tpb = project.ticks_per_bar().max(1) as f64;
        ui.label(RichText::new("Cycle").color(theme::TEXT_WEAK));
        let mut bars = cycle as f64 / tpb;
        let r1 = ui
            .add(
                DragValue::new(&mut bars)
                    .range(1.0 / 16.0..=64.0)
                    .speed(0.01)
                    .max_decimals(3)
                    .suffix(" bars"),
            )
            .on_hover_text("How long one repetition of the pattern lasts");
        ui.label(RichText::new("Seed").color(theme::TEXT_WEAK));
        let mut new_seed = seed;
        let r2 = ui
            .add(
                DragValue::new(&mut new_seed)
                    .range(0..=u32::MAX as u64)
                    .speed(0.2),
            )
            .on_hover_text("Changes the random choices of ? and |");
        let new_cycle = ((bars * tpb).round() as Ticks).max(1);
        if (r1.changed() && new_cycle != cycle) || (r2.changed() && new_seed != seed) {
            let id = if r1.changed() { r1.id } else { r2.id };
            let key = app.gestures.key(id);
            let r = actions::set_pattern(
                &app.session,
                clip.id,
                source,
                new_seed,
                new_cycle,
                Some(key),
            );
            app.report(r);
        }
        if app.pattern.text != source && ui.button("Revert").clicked() {
            app.pattern.text = source.to_string();
        }
    });
    ui.add_space(4.0);
    pattern_preview(
        ui,
        &app.pattern.text,
        seed,
        (clip.length as f64 / cycle.max(1) as f64).ceil() as i64,
    );
    ui.collapsing("Syntax", |ui| {
        for (code, what) in [
            ("bd sd hh", "steps share the cycle equally"),
            ("~", "a rest"),
            ("[sd sd]", "a group in one step"),
            ("bd*2, hh*4", "layers played together"),
            ("<c e g>", "one per cycle, in turn"),
            ("hh*8?", "drop events at random"),
            ("bd(3,8)", "Euclidean rhythm"),
            ("c4 e4 g4", "notes; 60 64 67 also works"),
        ] {
            ui.horizontal(|ui| {
                ui.label(RichText::new(code).monospace().color(theme::TEXT));
                ui.label(RichText::new(what).small().color(theme::TEXT_WEAK));
            });
        }
    });
}

/// A strip showing the events of the first few cycles.
fn pattern_preview(ui: &mut Ui, text: &str, seed: u64, cycles: i64) {
    let Ok(pattern) = sky_pattern::parse(text) else {
        return;
    };
    let cycles = cycles.clamp(1, 4);
    let width = ui.available_width().max(100.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 54.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(3), theme::LANE);
    let cw = rect.width() / cycles as f32;
    let mut notes: Vec<(f32, f32, u8, String)> = Vec::new();
    for c in 0..cycles {
        let x0 = rect.left() + c as f32 * cw;
        if c > 0 {
            painter.vline(x0, rect.y_range(), Stroke::new(1.0, theme::GRID_BAR));
        }
        painter.text(
            egui::pos2(x0 + 4.0, rect.top() + 2.0),
            Align2::LEFT_TOP,
            format!("cycle {}", c + 1),
            theme::small(),
            theme::TEXT_DIM,
        );
        for e in pattern
            .query(Time::from_integer(c), Time::from_integer(c + 1), seed)
            .into_iter()
            .take(2048)
        {
            let b = (*e.begin.numer() as f64 / (*e.begin.denom()).max(1) as f64 - c as f64) as f32;
            let en = (*e.end.numer() as f64 / (*e.end.denom()).max(1) as f64 - c as f64) as f32;
            let label = match &e.value {
                sky_pattern::Value::Sound { name, .. } => name.clone(),
                sky_pattern::Value::Note(n) => sky_core::music::note_name(*n),
            };
            notes.push((
                x0 + b * cw,
                x0 + en.min(1.0) * cw,
                e.value.midi_note().unwrap_or(60),
                label,
            ));
        }
    }
    let lanes: Vec<u8> = {
        let mut v: Vec<u8> = notes.iter().map(|n| n.2).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let rows = lanes.len().max(1) as f32;
    let top = rect.top() + 16.0;
    let h = ((rect.bottom() - 3.0 - top) / rows).max(2.0);
    for (x0, x1, note, label) in notes {
        let row = lanes.iter().position(|n| *n == note).unwrap_or(0) as f32;
        let y = rect.bottom() - 3.0 - (row + 1.0) * h;
        let r = egui::Rect::from_min_max(
            egui::pos2(x0 + 1.0, y + 1.0),
            egui::pos2((x1 - 1.0).max(x0 + 3.0), y + h - 1.0),
        );
        painter.rect_filled(r, CornerRadius::same(2), theme::ACCENT.gamma_multiply(0.8));
        if r.width() > 22.0 && h >= 9.0 {
            painter.with_clip_rect(r).text(
                egui::pos2(r.left() + 2.0, r.center().y),
                Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional((h - 2.0).min(10.0)),
                Color32::BLACK,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn audio_ui(
    app: &mut App,
    ui: &mut Ui,
    project: &Project,
    clip: &Clip,
    source: sky_core::AudioId,
    gain_db: f32,
    fade_in: Ticks,
    fade_out: Ticks,
) {
    match project.pool_entry(source) {
        Ok(e) => {
            ui.label(
                RichText::new(format!(
                    "{} · {} ch · {} Hz · {:.2} s",
                    e.name,
                    e.channels,
                    e.sample_rate,
                    e.frames as f64 / e.sample_rate.max(1) as f64
                ))
                .color(theme::TEXT_WEAK),
            );
        }
        Err(e) => {
            ui.label(RichText::new(e.to_string()).color(theme::ERROR));
        }
    }
    let beats_len = clip.length as f64 / PPQ as f64;
    egui::Grid::new(("audio_clip", clip.id))
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Gain");
            let mut g = gain_db;
            let resp = ui.add(
                Slider::new(&mut g, -60.0..=24.0)
                    .suffix(" dB")
                    .max_decimals(1),
            );
            if resp.changed() && g != gain_db {
                let key = app.gestures.key(resp.id);
                let r = actions::set_audio_clip(
                    &app.session,
                    clip.id,
                    AudioClipChange {
                        gain_db: Some(g),
                        ..Default::default()
                    },
                    Some(key),
                );
                app.report(r);
            }
            ui.end_row();
            for (label, value, is_in) in [("Fade in", fade_in, true), ("Fade out", fade_out, false)]
            {
                ui.label(label);
                let mut beats = value as f64 / PPQ as f64;
                let resp = ui.add(
                    DragValue::new(&mut beats)
                        .range(0.0..=beats_len)
                        .speed(0.02)
                        .max_decimals(2)
                        .suffix(" beats"),
                );
                if resp.changed() {
                    let ticks = (beats * PPQ as f64).round() as Ticks;
                    if ticks != value {
                        let key = app.gestures.key(resp.id);
                        let change = if is_in {
                            AudioClipChange {
                                fade_in: Some(ticks),
                                ..Default::default()
                            }
                        } else {
                            AudioClipChange {
                                fade_out: Some(ticks),
                                ..Default::default()
                            }
                        };
                        let r = actions::set_audio_clip(&app.session, clip.id, change, Some(key));
                        app.report(r);
                    }
                }
                ui.end_row();
            }
        });
}
