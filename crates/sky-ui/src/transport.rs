// SPDX-License-Identifier: GPL-3.0-or-later
//! The transport bar along the top of the window.

use egui::{
    Align, Color32, CornerRadius, DragValue, Frame, Layout, Margin, Pos2, Rect, Response, RichText,
    Sense, Shape, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType, pos2, vec2,
};

use crate::actions;
use crate::app::{Action, App, Tab};
use crate::theme;

/// The shapes drawn on the transport buttons.
#[derive(Clone, Copy)]
enum Icon {
    Play,
    Pause,
    Stop,
    Loop,
}

/// A square button with a painted icon, lit in the accent when `active`.
fn icon_button(ui: &mut Ui, icon: Icon, active: bool, label: &str, tip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(32.0, 26.0), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), label));
    if ui.is_rect_visible(rect) {
        let fill = if active {
            theme::ACCENT_BG
        } else if response.hovered() {
            theme::HEADER
        } else {
            theme::PANEL
        };
        let stroke = if active {
            theme::ACCENT
        } else {
            theme::GRID_BEAT
        };
        let painter = ui.painter();
        painter.rect(
            rect,
            CornerRadius::same(4),
            fill,
            Stroke::new(1.0, stroke),
            StrokeKind::Inside,
        );
        let color = if active { theme::ACCENT } else { theme::TEXT };
        let c = rect.center();
        match icon {
            Icon::Play => {
                let points = vec![
                    c + vec2(-4.5, -6.5),
                    c + vec2(-4.5, 6.5),
                    c + vec2(7.0, 0.0),
                ];
                painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
            }
            Icon::Pause => {
                for dx in [-3.5, 3.5] {
                    let r = Rect::from_center_size(c + vec2(dx, 0.0), vec2(3.5, 12.0));
                    painter.rect_filled(r, CornerRadius::same(1), color);
                }
            }
            Icon::Stop => {
                let r = Rect::from_center_size(c, vec2(11.0, 11.0));
                painter.rect_filled(r, CornerRadius::same(1), color);
            }
            Icon::Loop => {
                let r = Rect::from_center_size(c, vec2(16.0, 10.0));
                painter.rect_stroke(
                    r,
                    CornerRadius::same(4),
                    Stroke::new(1.6, color),
                    StrokeKind::Middle,
                );
                // An arrow head on the top edge pointing right.
                let tip_at = pos2(r.center().x + 3.0, r.top());
                let head = vec![
                    tip_at + vec2(-4.0, -3.5),
                    tip_at + vec2(-4.0, 3.5),
                    tip_at + vec2(1.5, 0.0),
                ];
                painter.add(Shape::convex_polygon(head, color, Stroke::NONE));
            }
        }
    }
    response.on_hover_text(tip)
}

/// A small framed read-out, such as the position or the engine load.
fn readout(ui: &mut Ui, add: impl FnOnce(&mut Ui)) -> Response {
    Frame::new()
        .fill(theme::LANE)
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                add(ui);
            })
        })
        .response
}

/// Minutes, seconds and milliseconds, as in `1:04.250`.
pub fn format_clock(seconds: f64) -> String {
    let seconds = if seconds.is_finite() {
        seconds.max(0.0)
    } else {
        0.0
    };
    let total_ms = (seconds * 1000.0).round() as u64;
    format!(
        "{}:{:02}.{:03}",
        total_ms / 60_000,
        total_ms / 1000 % 60,
        total_ms % 1000
    )
}

/// The color of the engine load read-out.
fn load_color(load: f32) -> Color32 {
    if load >= 0.85 {
        theme::ERROR
    } else if load >= 0.6 {
        theme::WARN
    } else {
        theme::TEXT
    }
}

pub fn bar(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let project = app.view.project.clone();
    let mut wanted: Vec<Action> = Vec::new();

    // Below this width the bar drops the clock and shortens labels.
    let narrow = ui.available_width() < 1320.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;

        // Play, stop and loop.
        let playing = app.transport.playing;
        let stop_tip = if playing {
            "Stop"
        } else {
            "Stop (again to return to the start)"
        };
        if icon_button(ui, Icon::Stop, false, "Stop", stop_tip).clicked() {
            app.stop();
        }
        let (icon, label) = if playing {
            (Icon::Pause, "Pause")
        } else {
            (Icon::Play, "Play")
        };
        if icon_button(ui, icon, playing, label, &format!("{label} (Space)")).clicked() {
            wanted.push(Action::PlayStop);
        }
        let loop_tip = if project.loop_enabled {
            "Loop is on: playback repeats the loop region"
        } else {
            "Loop is off"
        };
        if icon_button(ui, Icon::Loop, project.loop_enabled, "Loop", loop_tip).clicked() {
            wanted.push(Action::ToggleLoop);
        }

        ui.add_space(4.0);

        // Position as bar.beat.tick and as a clock.
        let tick = app.transport.position_ticks;
        let whole = if tick.is_finite() {
            tick.max(0.0) as i64
        } else {
            0
        };
        let position = project.time_signature.format_position(whole);
        let clock = format_clock(project.tempo.tick_to_seconds(whole));
        readout(ui, |ui| {
            ui.label(
                RichText::new(position)
                    .font(theme::mono(17.0))
                    .color(theme::TEXT),
            );
            if !narrow {
                ui.label(
                    RichText::new(clock)
                        .font(theme::mono(12.0))
                        .color(theme::TEXT_WEAK),
                );
            }
        })
        .on_hover_text("Position: bar . beat . tick, and time from the start");

        ui.add_space(4.0);

        // Tempo: a drag is one undo step.
        let mut bpm = project.tempo.initial_bpm();
        let resp = ui
            .add(
                DragValue::new(&mut bpm)
                    .range(10.0..=999.0)
                    .speed(0.2)
                    .max_decimals(2)
                    .suffix(" BPM"),
            )
            .on_hover_text(if project.tempo.points().len() > 1 {
                "Tempo at the start of the song: drag, or double-click to type. \
                 The tempo lane holds the changes after it."
            } else {
                "Tempo: drag, or double-click to type"
            });
        if resp.changed() && (bpm - project.tempo.initial_bpm()).abs() > 1e-9 {
            let key = app.gestures.key(resp.id);
            let r = actions::set_tempo(&app.session, bpm, Some(key));
            app.report(r);
        }

        // Time signature.
        let ts = project.time_signature;
        let mut numerator = ts.numerator;
        let resp = ui
            .add(DragValue::new(&mut numerator).range(1..=32).speed(0.05))
            .on_hover_text("Beats per bar");
        if resp.changed() && numerator != ts.numerator {
            let key = app.gestures.key(resp.id);
            let r = actions::set_time_signature(&app.session, numerator, ts.denominator, Some(key));
            app.report(r);
        }
        ui.label(RichText::new("/").color(theme::TEXT_WEAK));
        let mut denominator = ts.denominator;
        egui::ComboBox::from_id_salt("time_signature_denominator")
            .width(44.0)
            .selected_text(denominator.to_string())
            .show_ui(ui, |ui| {
                for d in [1, 2, 4, 8, 16, 32] {
                    ui.selectable_value(&mut denominator, d, d.to_string());
                }
            })
            .response
            .on_hover_text("The note value of one beat");
        if denominator != ts.denominator {
            let r = actions::set_time_signature(&app.session, ts.numerator, denominator, None);
            app.report(r);
        }

        ui.separator();

        // Undo and redo, labelled with what they would do.
        let undo_tip = match &app.view.undo_label {
            Some(l) => format!("Undo “{l}” (Ctrl+Z)"),
            None => "Nothing to undo".to_string(),
        };
        let undo = ui
            .add_enabled(app.view.undo_label.is_some(), egui::Button::new("Undo"))
            .on_hover_text(&undo_tip)
            .on_disabled_hover_text(&undo_tip);
        if undo.clicked() {
            wanted.push(Action::Undo);
        }
        let redo_tip = match &app.view.redo_label {
            Some(l) => format!("Redo “{l}” (Ctrl+Shift+Z)"),
            None => "Nothing to redo".to_string(),
        };
        let redo = ui
            .add_enabled(app.view.redo_label.is_some(), egui::Button::new("Redo"))
            .on_hover_text(&redo_tip)
            .on_disabled_hover_text(&redo_tip);
        if redo.clicked() {
            wanted.push(Action::Redo);
        }

        ui.separator();

        // Save and export; the rest is in the menu bar.
        let save_text = if app.view.dirty { "Save •" } else { "Save" };
        let save_tip = match &app.view.path {
            Some(p) => format!("Save to {} (Ctrl+S)", p.display()),
            None => "Choose where to save the project (Ctrl+S)".to_string(),
        };
        if ui.button(save_text).on_hover_text(save_tip).clicked() {
            wanted.push(Action::Save);
        }
        let (loop_text, wav_text, game_text) = if narrow {
            ("Loop…", "WAV…", "Game…")
        } else {
            ("Export loop…", "Export WAV…", "Game assets…")
        };
        if ui
            .button(loop_text)
            .on_hover_text("Export the loop region as a seamless game loop with a manifest")
            .clicked()
        {
            wanted.push(Action::ExportLoop);
        }
        if ui
            .button(wav_text)
            .on_hover_text("Export the song or the loop region to a WAV file")
            .clicked()
        {
            wanted.push(Action::ExportWav);
        }
        if ui
            .button(game_text)
            .on_hover_text(
                "The loops and SFX sets this project exports for a game, and their export",
            )
            .clicked()
        {
            wanted.push(Action::GameAssets);
        }

        // The right side: AI indicator, engine health and the palette.
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if ui
                .button(RichText::new("Ctrl+K").color(theme::TEXT_WEAK))
                .on_hover_text("Command palette")
                .clicked()
            {
                wanted.push(Action::OpenPalette);
            }
            if ui.available_width() > 50.0 && ai_indicator(app, ui).clicked() {
                app.workspace.show(Tab::Ai);
            }
            engine_health(app, ui);
        });
    });

    for action in wanted {
        app.run_action(&ctx, action);
    }
}

/// The "AI" lamp: lit for a moment when an MCP client changes the project.
fn ai_indicator(app: &App, ui: &mut Ui) -> Response {
    let glow = app.ai.glow(app.now);
    let connected = app.mcp.is_some();
    let galley = ui.painter().layout_no_wrap(
        "AI".to_string(),
        egui::FontId::proportional(12.5),
        Color32::WHITE,
    );
    let size = vec2(galley.size().x + 26.0, 22.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "AI activity"));
    if ui.is_rect_visible(rect) {
        let base = if response.hovered() {
            theme::HEADER
        } else {
            theme::PANEL
        };
        let fill = base.lerp_to_gamma(theme::ACCENT, glow * 0.85);
        let painter = ui.painter();
        painter.rect(
            rect,
            CornerRadius::same(11),
            fill,
            Stroke::new(1.0, theme::GRID_BEAT.lerp_to_gamma(theme::ACCENT, glow)),
            StrokeKind::Inside,
        );
        let dot_color = if !connected {
            theme::TEXT_DIM
        } else if glow > 0.0 {
            Color32::WHITE
        } else {
            theme::ACCENT
        };
        painter.circle_filled(pos2(rect.left() + 10.0, rect.center().y), 3.5, dot_color);
        let text_color = if glow > 0.3 {
            Color32::BLACK
        } else if connected {
            theme::TEXT
        } else {
            theme::TEXT_DIM
        };
        painter.galley(
            Pos2::new(rect.left() + 18.0, rect.center().y - galley.size().y / 2.0),
            galley,
            text_color,
        );
    }
    let tip = match (&app.ai.latest, connected) {
        (Some(latest), _) => format!("Latest AI change: {latest}\nClick to see all AI activity"),
        (None, true) => "No AI changes yet. Click for connection details.".to_string(),
        (None, false) => "The MCP server is not running in this session".to_string(),
    };
    response.on_hover_text(tip)
}

/// Engine load and dropouts, right aligned. The status bar shows the
/// engine load too, so this is the first part to go in a narrow window.
fn engine_health(app: &App, ui: &mut Ui) {
    let t = &app.transport;
    if app.audio.stream.is_none() {
        if ui.available_width() > 80.0 {
            ui.label(RichText::new("No audio").color(theme::WARN))
                .on_hover_text(app.audio.describe());
        }
        return;
    }
    if ui.available_width() < 200.0 {
        return;
    }
    let drop_color = if t.dropouts > 0 {
        theme::WARN
    } else {
        theme::TEXT_WEAK
    };
    readout(ui, |ui| {
        ui.label(
            RichText::new(format!("{}", t.dropouts))
                .font(theme::mono(12.5))
                .color(drop_color),
        );
        ui.label(RichText::new("dropouts").small().color(theme::TEXT_WEAK));
    })
    .on_hover_text(format!(
        "Buffers the device asked for that came late: {}\nEngine overruns: {}",
        t.dropouts, t.overruns
    ));
    readout(ui, |ui| {
        ui.label(
            RichText::new(format!("{:>3.0}%", (t.load * 100.0).clamp(0.0, 999.0)))
                .font(theme::mono(12.5))
                .color(load_color(t.load)),
        );
        ui.label(RichText::new("CPU").small().color(theme::TEXT_WEAK));
    })
    .on_hover_text("Share of each audio buffer the engine needed to render it");
}

#[cfg(test)]
mod tests {
    use super::format_clock;

    #[test]
    fn clock_format() {
        assert_eq!(format_clock(0.0), "0:00.000");
        assert_eq!(format_clock(64.25), "1:04.250");
        assert_eq!(format_clock(-3.0), "0:00.000");
        assert_eq!(format_clock(f64::NAN), "0:00.000");
    }
}
