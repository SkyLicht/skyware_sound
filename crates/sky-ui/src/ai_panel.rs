// SPDX-License-Identifier: GPL-3.0-or-later
//! The AI activity tab: what MCP clients changed, with "Undo to here", and
//! how to connect an assistant.

use std::time::{SystemTime, UNIX_EPOCH};

use egui::{Grid, Panel, RichText, ScrollArea, Ui};
use sky_core::{AssetId, ClipId, EffectId, TrackId};
use sky_model::{ChannelRef, Project, Target, Track};
use sky_session::ProjectView;

use crate::app::{App, Tab};
use crate::{mixer, theme};

/// Seconds what an AI change touched stays fully lit, then fades.
const LIT_FULL: f64 = 3.0;
const LIT_FADE: f64 = 5.0;

struct Lit {
    target: Target,
    at: f64,
    seq: u64,
}

/// Notices new AI changes so the transport can flash its indicator and
/// the clips, tracks and effects they touched light up.
pub struct AiState {
    last_seq: Option<u64>,
    last_len: usize,
    pub flash_until: f64,
    pub latest: Option<String>,
    pub show_token: bool,
    /// What recent AI changes touched, when, and by which change.
    lit: Vec<Lit>,
}

impl AiState {
    pub fn new(view: &ProjectView) -> Self {
        Self {
            last_seq: view.activity.last().map(|a| a.seq),
            last_len: view.activity.len(),
            flash_until: f64::NEG_INFINITY,
            latest: None,
            show_token: false,
            lit: Vec::new(),
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
            let fresh = view
                .activity
                .iter()
                .filter(|a| !a.undone && self.last_seq.is_none_or(|s| a.seq > s));
            for a in fresh {
                for t in &a.targets {
                    self.lit.retain(|l| l.target != *t);
                    self.lit.push(Lit {
                        target: *t,
                        at: now,
                        seq: a.seq,
                    });
                }
            }
        }
        self.last_seq = seq;
        self.last_len = len;
        // Undone changes go dark at once.
        let undone = |seq: u64| view.activity.iter().any(|a| a.seq == seq && a.undone);
        self.lit
            .retain(|l| now - l.at < LIT_FULL + LIT_FADE && !undone(l.seq));
    }

    pub fn flashing(&self, now: f64) -> bool {
        now < self.flash_until || !self.lit.is_empty()
    }

    /// How lit `target` is, 0..1.
    fn glow_where(&self, now: f64, mut hit: impl FnMut(&Target) -> bool) -> f32 {
        self.lit
            .iter()
            .filter(|l| hit(&l.target))
            .map(|l| (1.0 - (now - l.at - LIT_FULL) / LIT_FADE).clamp(0.0, 1.0) as f32)
            .fold(0.0, f32::max)
    }

    /// How lit a clip is.
    pub fn clip_glow(&self, id: ClipId, now: f64) -> f32 {
        self.glow_where(now, |t| *t == Target::Clip(id))
    }

    /// How lit a track's header is: the track, its instrument, its channel
    /// and effects, or one of its clips changed.
    pub fn track_glow(&self, track: &Track, now: f64) -> f32 {
        let channel = ChannelRef::Track(track.id);
        self.glow_where(now, |t| match t {
            Target::Track(id) | Target::Instrument(id) => *id == track.id,
            Target::Channel(c) | Target::Effect(c, _) => *c == channel,
            Target::Clip(id) => track.clips.iter().any(|c| c.id == *id),
            _ => false,
        })
    }

    /// How lit a mixer strip is: its level, pan, mute or solo, or which
    /// effects it has, changed.
    pub fn channel_glow(&self, channel: ChannelRef, now: f64) -> f32 {
        self.glow_where(now, |t| *t == Target::Channel(channel))
    }

    pub fn effect_glow(&self, channel: ChannelRef, id: EffectId, now: f64) -> f32 {
        self.glow_where(now, |t| *t == Target::Effect(channel, id))
    }

    pub fn asset_glow(&self, id: AssetId, now: f64) -> f32 {
        self.glow_where(now, |t| *t == Target::Asset(id))
    }

    #[cfg(test)]
    pub fn lit(&self) -> Vec<Target> {
        self.lit.iter().map(|l| l.target).collect()
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
    let mut show = None;
    ScrollArea::vertical()
        .id_salt("ai_activity")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Grid::new("ai_activity_grid")
                .num_columns(6)
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
                        let shown = !a.undone && first_shown(&view.project, &a.targets).is_some();
                        if ui
                            .add_enabled(shown, egui::Button::new("Show"))
                            .on_hover_text("Select what this change touched and bring it into view")
                            .on_disabled_hover_text("Nothing of this change is left to show")
                            .clicked()
                        {
                            show = Some(a.targets.clone());
                        }
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
    if let Some(targets) = show {
        reveal(app, &targets);
    }
    if let Some(seq) = undo_to {
        match app.session.undo_to(seq) {
            Ok(n) => app.info(format!("Undid {n} change{}", if n == 1 { "" } else { "s" })),
            Err(e) => app.error(e.to_string()),
        }
        app.refresh();
    }
}

/// The first of `targets` that can be shown: a clip, then an effect or
/// channel, then an instrument, track or game asset.
fn first_shown(project: &Project, targets: &[Target]) -> Option<Target> {
    let rank = |t: &Target| match t {
        Target::Clip(_) => 0,
        Target::Effect(..) | Target::Channel(_) => 1,
        Target::Instrument(_) | Target::Track(_) => 2,
        Target::Asset(_) => 3,
        Target::Song | Target::Marker(_) | Target::Audio(_) => 9,
    };
    let exists = |t: &Target| match *t {
        Target::Clip(id) => project.find_clip(id).is_ok(),
        Target::Effect(c, id) => {
            mixer::channel_of(project, c).is_some_and(|ch| ch.effects.iter().any(|e| e.id == id))
        }
        Target::Channel(c) => mixer::channel_of(project, c).is_some(),
        Target::Instrument(id) | Target::Track(id) => project.track(id).is_ok(),
        Target::Asset(id) => project.game.asset(id).is_some(),
        Target::Song | Target::Marker(_) | Target::Audio(_) => false,
    };
    targets
        .iter()
        .filter(|t| exists(t))
        .min_by_key(|t| rank(t))
        .copied()
}

/// Selects what an AI change touched and brings it into view.
pub fn reveal(app: &mut App, targets: &[Target]) {
    let project = app.view.project.clone();
    let Some(target) = first_shown(&project, targets) else {
        return;
    };
    let row_of = |id: TrackId| project.tracks.iter().position(|t| t.id == id);
    match target {
        Target::Clip(id) => {
            if let Ok((track, clip)) = project.find_clip(id) {
                app.sel.select_clip(track.id, clip.id);
                app.arrange.reveal_tick(clip.start);
                app.arrange.reveal_row = row_of(track.id);
                app.workspace.show(Tab::Editor);
            }
        }
        Target::Effect(channel, id) => {
            if let ChannelRef::Track(track) = channel {
                app.sel.select_track(track);
                app.arrange.reveal_row = row_of(track);
            }
            app.mixer.open_effect((channel, id));
            app.workspace.show(Tab::Mixer);
        }
        Target::Channel(channel) => {
            if let ChannelRef::Track(track) = channel {
                app.sel.select_track(track);
                app.arrange.reveal_row = row_of(track);
            }
            app.workspace.show(Tab::Mixer);
        }
        Target::Instrument(track) => {
            app.sel.select_track(track);
            app.arrange.reveal_row = row_of(track);
            app.workspace.show(Tab::Instrument);
        }
        Target::Track(track) => {
            app.sel.select_track(track);
            app.arrange.reveal_row = row_of(track);
        }
        Target::Asset(id) => {
            if let Some(asset) = project.game.asset(id) {
                app.arrange
                    .reveal_tick(crate::asset_lane::span(asset).start);
            }
            crate::asset_lane::select(app, &project, id);
        }
        Target::Song | Target::Marker(_) | Target::Audio(_) => {}
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
