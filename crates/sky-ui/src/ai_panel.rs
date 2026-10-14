// SPDX-License-Identifier: GPL-3.0-or-later
//! The AI activity tab: what MCP clients changed, with "Undo to here", and
//! how to connect an assistant.

use std::time::{SystemTime, UNIX_EPOCH};

use egui::{Grid, Panel, RichText, ScrollArea, Ui};
use sky_session::ProjectView;

use crate::app::App;
use crate::theme;

/// Notices new AI changes so the transport can flash its indicator.
pub struct AiState {
    last_seq: Option<u64>,
    last_len: usize,
    pub flash_until: f64,
    pub latest: Option<String>,
    pub show_token: bool,
}

impl AiState {
    pub fn new(view: &ProjectView) -> Self {
        Self {
            last_seq: view.activity.last().map(|a| a.seq),
            last_len: view.activity.len(),
            flash_until: f64::NEG_INFINITY,
            latest: None,
            show_token: false,
        }
    }

    pub fn observe(&mut self, view: &ProjectView, now: f64) {
        let seq = view.activity.last().map(|a| a.seq);
        let len = view.activity.len();
        if seq.is_some() && (seq != self.last_seq || len > self.last_len) {
            self.flash_until = now + 2.5;
            self.latest = view
                .activity
                .last()
                .map(|a| format!("{} · {}", a.client, a.label));
        }
        self.last_seq = seq;
        self.last_len = len;
    }

    pub fn flashing(&self, now: f64) -> bool {
        now < self.flash_until
    }

    /// How lit the indicator is, 0..1, fading out over the flash.
    pub fn glow(&self, now: f64) -> f32 {
        ((self.flash_until - now) / 2.5).clamp(0.0, 1.0) as f32
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// "12 s ago", "3 min ago" and so on.
pub fn ago(time_ms: u64, now_ms: u64) -> String {
    let s = now_ms.saturating_sub(time_ms) / 1000;
    match s {
        0..=4 => "just now".to_string(),
        5..=59 => format!("{s} s ago"),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => format!("{} d ago", s / 86_400),
    }
}

/// Clock time in UTC, for the tooltip.
fn utc_clock(time_ms: u64) -> String {
    let secs = time_ms / 1000 % 86_400;
    format!(
        "{:02}:{:02}:{:02} UTC",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

pub fn mcp_command(url: &str, token: &str) -> String {
    format!(
        "claude mcp add --transport http skyware {url} --header \"Authorization: Bearer {token}\""
    )
}

pub fn show(app: &mut App, ui: &mut Ui) {
    Panel::right("ai_connection")
        .resizable(true)
        .default_size(400.0)
        .size_range(260.0..=700.0)
        .show(ui, |ui| connection(app, ui));
    activity(app, ui);
}

fn activity(app: &mut App, ui: &mut Ui) {
    let view = app.view.clone();
    ui.label(RichText::new("Changes made by AI assistants").strong());
    if view.activity.is_empty() {
        ui.label(
            RichText::new(
                "Nothing yet. When an assistant edits the project over MCP, each change \
                 appears here and can be undone.",
            )
            .color(theme::TEXT_WEAK),
        );
        return;
    }
    let now = now_ms();
    let mut undo_to = None;
    ScrollArea::vertical()
        .id_salt("ai_activity")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Grid::new("ai_activity_grid")
                .num_columns(5)
                .striped(true)
                .spacing([14.0, 6.0])
                .show(ui, |ui| {
                    for a in view.activity.iter().rev() {
                        let c = |t: RichText| {
                            if a.undone {
                                t.color(theme::TEXT_DIM).strikethrough()
                            } else {
                                t.color(theme::TEXT)
                            }
                        };
                        ui.label(c(RichText::new(ago(a.time_unix_ms, now))))
                            .on_hover_text(utc_clock(a.time_unix_ms));
                        ui.label(c(RichText::new(&a.client)));
                        ui.label(c(RichText::new(&a.tool).monospace()));
                        ui.label(c(RichText::new(&a.label)));
                        if a.undone {
                            ui.label(RichText::new("undone").small().color(theme::TEXT_DIM));
                        } else {
                            let in_history = view.history.iter().any(|h| h.seq == a.seq);
                            let resp = ui
                                .add_enabled(in_history, egui::Button::new("Undo to here"))
                                .on_hover_text(
                                    "Undo this change and everything after it, including your own edits",
                                )
                                .on_disabled_hover_text("This change is no longer in the undo history");
                            if resp.clicked() {
                                undo_to = Some(a.seq);
                            }
                        }
                        ui.end_row();
                    }
                });
        });
    if let Some(seq) = undo_to {
        match app.session.undo_to(seq) {
            Ok(n) => app.info(format!("Undid {n} change{}", if n == 1 { "" } else { "s" })),
            Err(e) => app.error(e.to_string()),
        }
        app.refresh();
    }
}

fn connection(app: &mut App, ui: &mut Ui) {
    ui.label(RichText::new("Connect an assistant").strong());
    let Some(mcp) = app.mcp.clone() else {
        ui.label(
            RichText::new("The MCP server is not running in this session.").color(theme::TEXT_WEAK),
        );
        return;
    };
    ui.label(
        RichText::new("Assistants reach this project through the local MCP server.")
            .small()
            .color(theme::TEXT_WEAK),
    );
    ui.add_space(4.0);
    Grid::new("mcp_info")
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            ui.label(RichText::new("URL").color(theme::TEXT_WEAK));
            ui.horizontal(|ui| {
                ui.label(RichText::new(&mcp.url).monospace());
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(mcp.url.clone());
                    app.info("Copied the MCP URL");
                }
            });
            ui.end_row();
            ui.label(RichText::new("Token").color(theme::TEXT_WEAK));
            ui.horizontal(|ui| {
                let shown = if app.ai.show_token {
                    mcp.token.clone()
                } else {
                    "•".repeat(mcp.token.chars().count().clamp(8, 24))
                };
                ui.label(RichText::new(shown).monospace());
                let toggle = if app.ai.show_token { "Hide" } else { "Show" };
                if ui.small_button(toggle).clicked() {
                    app.ai.show_token = !app.ai.show_token;
                }
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(mcp.token.clone());
                    app.info("Copied the MCP token");
                }
            });
            ui.end_row();
        });
    ui.add_space(6.0);
    ui.label(RichText::new("Claude Code").color(theme::TEXT_WEAK));
    let command = mcp_command(&mcp.url, &mcp.token);
    let shown = if app.ai.show_token {
        command.clone()
    } else {
        mcp_command(&mcp.url, "<token>")
    };
    egui::Frame::new()
        .fill(theme::BG)
        .corner_radius(egui::CornerRadius::same(4))
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(shown).font(theme::mono(11.5))).wrap());
        });
    if ui.button("Copy command").clicked() {
        ui.ctx().copy_text(command);
        app.info("Copied the command; paste it into a terminal");
    }
}
