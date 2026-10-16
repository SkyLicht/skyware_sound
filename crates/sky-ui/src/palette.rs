// SPDX-License-Identifier: GPL-3.0-or-later
//! The command palette (Ctrl+K): a filtered list of actions.

use egui::{Align2, Context, Id, Key, Modal, Modifiers, RichText, ScrollArea, TextEdit, vec2};
use sky_dsp::EffectType;
use sky_model::ChannelRef;

use crate::actions::NewTrack;
use crate::app::{Action, App, Tab};
use crate::clip_tools::Tool;
use crate::ruler_lanes::Lane;
use crate::{keys, theme};

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
    /// From the key command table; empty when the action has no key.
    pub shortcut: String,
    pub action: Action,
}

fn entry(label: impl Into<String>, action: Action) -> Entry {
    Entry {
        label: label.into(),
        shortcut: keys::shortcut_for(&action),
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
                Action::AddEffect {
                    channel: ChannelRef::Track(*id),
                    kind,
                },
            ));
        }
        out.push(entry(
            format!("Add {} to master", kind.label()),
            Action::AddEffect {
                channel: ChannelRef::Master,
                kind,
            },
        ));
    }
    out.extend([
        entry("Play / stop", Action::PlayStop),
        entry(
            if project.loop_enabled {
                "Turn loop off"
            } else {
                "Turn loop on"
            },
            Action::ToggleLoop,
        ),
        entry("Undo", Action::Undo),
        entry("Redo", Action::Redo),
        entry("Set tempo…", Action::PromptTempo),
        entry("New empty project", Action::NewEmpty),
        entry("New demo project", Action::NewDemo),
        entry("New techno loop", Action::NewTechno),
        entry("New acid loop", Action::NewAcid),
        entry("Open project…", Action::Open),
        entry("Save", Action::Save),
        entry("Save as…", Action::SaveAs),
        entry("Import audio…", Action::ImportAudio),
        entry("Load sounds from a sample pack…", Action::LoadSounds),
        entry(
            "Add a folder of samples to the browser…",
            Action::AddBrowserFolder,
        ),
        entry("Export loop for a game…", Action::ExportLoop),
        entry(
            "Game assets: loops and SFX sets for a game…",
            Action::GameAssets,
        ),
        entry("Export WAV…", Action::ExportWav),
        entry("Audio settings…", Action::OpenSettings),
    ]);
    for tab in Tab::ALL {
        let name = match tab {
            Tab::Arrange => continue,
            Tab::Browser => "the browser",
            Tab::Editor => "the clip editor",
            Tab::Mixer => "the mixer",
            Tab::Instrument => "the instrument panel",
            Tab::Ai => "AI activity",
            Tab::Loop => "the loop inspector",
        };
        let verb = if app.workspace.is_open(tab) {
            "Hide"
        } else {
            "Show"
        };
        out.push(entry(format!("{verb} {name}"), Action::TogglePanel(tab)));
    }
    out.extend([
        entry("Reset the panel layout", Action::ResetLayout),
        entry("Cut", Action::Cut),
        entry("Copy", Action::Copy),
        entry("Paste at the playhead", Action::Paste),
        entry("Duplicate the selection", Action::Duplicate),
        entry("Delete the selection", Action::DeleteSelection),
        entry("Split clips at the playhead", Action::SplitAtPlayhead),
        entry("Mute or unmute the selected clips", Action::MuteClips),
        entry("Add a marker at the playhead", Action::AddMarker),
        entry("Select all", Action::SelectAll),
        entry("Select nothing", Action::SelectNone),
        entry("Zoom the timeline in", Action::TimelineZoom(1)),
        entry("Zoom the timeline out", Action::TimelineZoom(-1)),
        entry("Fit the timeline", Action::TimelineFit),
        entry("Keyboard shortcuts", Action::Shortcuts),
    ]);
    for tool in Tool::ALL {
        out.push(entry(format!("{} tool", tool.label()), Action::Tool(tool)));
    }
    for lane in [Lane::Markers, Lane::Tempo] {
        let verb = if app.workspace.lanes.shows(lane) {
            "Hide"
        } else {
            "Show"
        };
        let name = lane.label().to_lowercase();
        out.push(entry(
            format!("{verb} the {name}"),
            Action::ToggleLane(lane),
        ));
    }
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
                    .right_text(RichText::new(&e.shortcut).small().color(theme::TEXT_DIM))
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
