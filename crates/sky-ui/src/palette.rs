// SPDX-License-Identifier: GPL-3.0-or-later
//! The command palette (Ctrl+K): a filtered list of actions.

use egui::{Align2, Context, Id, Key, Modal, Modifiers, RichText, ScrollArea, TextEdit, vec2};
use sky_dsp::EffectType;
use sky_model::ChannelRef;

use crate::actions::NewTrack;
use crate::app::{Action, App};
use crate::theme;

#[derive(Default)]
pub struct PaletteState {
    pub open: bool,
    pub query: String,
    pub selected: usize,
    focus: bool,
}

impl PaletteState {
    pub fn show_palette(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
        self.focus = true;
    }

    /// Reopens the palette with "tempo " typed, waiting for a number.
    pub fn prompt_tempo(&mut self) {
        self.open = true;
        self.query = "tempo ".to_string();
        self.selected = 0;
        self.focus = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
    }
}

pub struct Entry {
    pub label: String,
    pub shortcut: &'static str,
    pub action: Action,
}

fn entry(label: impl Into<String>, shortcut: &'static str, action: Action) -> Entry {
    Entry {
        label: label.into(),
        shortcut,
        action,
    }
}

/// Every action, labelled for the current selection.
pub fn entries(app: &App) -> Vec<Entry> {
    let project = &app.view.project;
    let mut out = Vec::new();
    for kind in NewTrack::ALL {
        out.push(entry(
            format!("Add {} track", kind.label().to_lowercase()),
            "",
            Action::AddTrack(kind),
        ));
    }
    let selected = app
        .sel
        .track
        .and_then(|t| project.track(t).ok())
        .map(|t| (t.id, t.name.clone()));
    for kind in EffectType::ALL {
        if let Some((id, name)) = &selected {
            out.push(entry(
                format!("Add {} to {name}", kind.label()),
                "",
                Action::AddEffect {
                    channel: ChannelRef::Track(*id),
                    kind,
                },
            ));
        }
        out.push(entry(
            format!("Add {} to master", kind.label()),
            "",
            Action::AddEffect {
                channel: ChannelRef::Master,
                kind,
            },
        ));
    }
    out.extend([
        entry("Play / stop", "Space", Action::PlayStop),
        entry(
            if project.loop_enabled {
                "Turn loop off"
            } else {
                "Turn loop on"
            },
            "",
            Action::ToggleLoop,
        ),
        entry("Undo", "Ctrl+Z", Action::Undo),
        entry("Redo", "Ctrl+Shift+Z", Action::Redo),
        entry("Set tempo…", "", Action::PromptTempo),
        entry("New empty project", "", Action::NewEmpty),
        entry("New demo project", "", Action::NewDemo),
        entry("Open project…", "Ctrl+O", Action::Open),
        entry("Save", "Ctrl+S", Action::Save),
        entry("Save as…", "Ctrl+Shift+S", Action::SaveAs),
        entry("Import audio…", "", Action::ImportAudio),
        entry("Load sounds from a sample pack…", "", Action::LoadSounds),
        entry("Export loop for a game…", "", Action::ExportLoop),
        entry("Export WAV…", "", Action::ExportWav),
        entry("Audio settings…", "", Action::OpenSettings),
    ]);
    out
}

/// A tempo typed into the query, as in "tempo 128" or "128 bpm".
pub fn tempo_in(query: &str) -> Option<f64> {
    let q = query.to_lowercase();
    if !(q.contains("tempo") || q.contains("bpm")) {
        return None;
    }
    q.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .filter(|s| !s.is_empty())
        .find_map(|s| s.parse::<f64>().ok())
        .filter(|b| (10.0..=999.0).contains(b))
}

/// Entries whose label holds every word of the query, best matches first.
pub fn filter(entries: Vec<Entry>, query: &str) -> Vec<Entry> {
    let q = query.trim().to_lowercase();
    let mut out: Vec<Entry> = Vec::new();
    if let Some(bpm) = tempo_in(&q) {
        out.push(entry(
            format!("Set tempo to {bpm} BPM"),
            "",
            Action::SetTempo(bpm),
        ));
    }
    let words: Vec<&str> = q.split_whitespace().collect();
    let mut scored: Vec<(usize, Entry)> = entries
        .into_iter()
        .filter_map(|e| {
            let label = e.label.to_lowercase();
            let all = words.iter().all(|w| {
                label.contains(w) || (w.parse::<f64>().is_ok() && label.contains("tempo"))
            });
            if !all {
                return None;
            }
            let score = if q.is_empty() || label.starts_with(&q) {
                0
            } else if label.contains(&q) {
                1
            } else {
                2
            };
            Some((score, e))
        })
        .collect();
    scored.sort_by_key(|(s, _)| *s);
    out.extend(scored.into_iter().map(|(_, e)| e));
    out
}

pub fn show(app: &mut App, ctx: &Context) {
    if !app.palette.open {
        return;
    }
    let list = filter(entries(app), &app.palette.query);
    let count = list.len();
    let (down, up, enter) = ctx.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::ArrowUp),
            i.consume_key(Modifiers::NONE, Key::Enter),
        )
    });
    if down {
        app.palette.selected = (app.palette.selected + 1).min(count.saturating_sub(1));
    }
    if up {
        app.palette.selected = app.palette.selected.saturating_sub(1);
    }
    app.palette.selected = app.palette.selected.min(count.saturating_sub(1));
    let mut run = None;
    if enter && let Some(e) = list.get(app.palette.selected) {
        run = Some(e.action.clone());
    }

    let id = Id::new("command_palette");
    let modal = Modal::new(id)
        .area(Modal::default_area(id).anchor(Align2::CENTER_TOP, vec2(0.0, 90.0)))
        .show(ctx, |ui| {
            ui.set_width(540.0);
            let before = app.palette.query.clone();
            let resp = ui.add(
                TextEdit::singleline(&mut app.palette.query)
                    .hint_text("Type a command, such as “add reverb” or “tempo 128”")
                    .desired_width(f32::INFINITY)
                    .font(theme::body()),
            );
            if app.palette.focus {
                resp.request_focus();
                app.palette.focus = false;
            }
            if app.palette.query != before {
                app.palette.selected = 0;
            }
            ui.separator();
            if list.is_empty() {
                ui.label(RichText::new("No matching command").color(theme::TEXT_WEAK));
            }
            ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                for (i, e) in list.iter().enumerate() {
                    let selected = i == app.palette.selected;
                    let button = egui::Button::selectable(
                        selected,
                        RichText::new(&e.label).color(theme::TEXT),
                    )
                    .right_text(RichText::new(e.shortcut).small().color(theme::TEXT_DIM))
                    .min_size(vec2(ui.available_width(), 24.0));
                    let r = ui.add(button);
                    if selected && (down || up) {
                        r.scroll_to_me(None);
                    }
                    if r.clicked() {
                        run = Some(e.action.clone());
                    }
                }
            });
        });
    if modal.should_close() {
        app.palette.close();
    }
    if let Some(action) = run {
        app.palette.close();
        app.run_action(ctx, action);
    }
}
