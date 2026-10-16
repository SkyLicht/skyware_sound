// SPDX-License-Identifier: GPL-3.0-or-later
//! The menu bar along the top of the window, and Help > About.
//!
//! The menus are built as data by [`menus`] and drawn by [`bar`], so the
//! tests can read what each menu offers. Their shortcuts come from the key
//! command table in [`crate::keys`].

use egui::{Align2, Atom, Button, Checkbox, Context, RichText, Ui, Window};
use sky_dsp::EffectType;
use sky_model::ChannelRef;

use crate::actions::NewTrack;
use crate::app::{Action, App, Tab};
use crate::clip_tools::Tool;
use crate::ruler_lanes::Lane;
use crate::{keys, theme};

pub struct Menu {
    pub title: &'static str,
    pub entries: Vec<Entry>,
}

pub enum Entry {
    Item(Item),
    /// A submenu.
    Sub(String, Vec<Entry>),
    Separator,
    /// Text that does nothing, such as "No recent projects".
    Note(&'static str),
}

pub struct Item {
    pub label: String,
    /// From the key command table; empty when the action has no key.
    pub shortcut: String,
    pub action: Action,
    pub enabled: bool,
    /// Drawn as a checkbox when set.
    pub checked: Option<bool>,
    pub tip: Option<String>,
}

fn item(label: impl Into<String>, action: Action) -> Entry {
    Entry::Item(Item {
        label: label.into(),
        shortcut: keys::shortcut_for(&action),
        action,
        enabled: true,
        checked: None,
        tip: None,
    })
}

/// An item that can be greyed out.
fn item_if(enabled: bool, label: impl Into<String>, action: Action) -> Entry {
    let Entry::Item(mut it) = item(label, action) else {
        unreachable!()
    };
    it.enabled = enabled;
    Entry::Item(it)
}

fn check(on: bool, label: impl Into<String>, action: Action) -> Entry {
    let Entry::Item(mut it) = item(label, action) else {
        unreachable!()
    };
    it.checked = Some(on);
    Entry::Item(it)
}

/// Every menu, labelled for the current project, selection and layout.
pub fn menus(app: &App) -> Vec<Menu> {
    vec![
        Menu {
            title: "File",
            entries: file_menu(app),
        },
        Menu {
            title: "Edit",
            entries: edit_menu(app),
        },
        Menu {
            title: "Track",
            entries: track_menu(app),
        },
        Menu {
            title: "Transport",
            entries: transport_menu(app),
        },
        Menu {
            title: "View",
            entries: view_menu(app),
        },
        Menu {
            title: "Help",
            entries: vec![
                item("Command palette…", Action::OpenPalette),
                item("Keyboard shortcuts…", Action::Shortcuts),
                item("About Skyware Sound", Action::About),
            ],
        },
    ]
}

fn file_menu(app: &App) -> Vec<Entry> {
    let mut recent: Vec<Entry> = app
        .workspace
        .recent
        .iter()
        .map(|path| {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into(),
            );
            let Entry::Item(mut it) = item(name, Action::OpenRecent(path.clone())) else {
                unreachable!()
            };
            it.tip = Some(path.display().to_string());
            Entry::Item(it)
        })
        .collect();
    if recent.is_empty() {
        recent.push(Entry::Note("No recent projects"));
    } else {
        recent.push(Entry::Separator);
        recent.push(item("Clear the list", Action::ClearRecent));
    }
    vec![
        item("New empty project", Action::NewEmpty),
        item("New demo project", Action::NewDemo),
        item("New techno loop", Action::NewTechno),
        item("New acid loop", Action::NewAcid),
        Entry::Separator,
        item("Open project…", Action::Open),
        Entry::Sub("Open recent".into(), recent),
        item("Save", Action::Save),
        item("Save as…", Action::SaveAs),
        Entry::Separator,
        item("Import audio…", Action::ImportAudio),
        item("Load sounds from a sample pack…", Action::LoadSounds),
        item("Add a folder to the browser…", Action::AddBrowserFolder),
        Entry::Separator,
        item("Export WAV…", Action::ExportWav),
        item("Export loop for a game…", Action::ExportLoop),
        item("Game assets…", Action::GameAssets),
        Entry::Separator,
        item("Audio settings…", Action::OpenSettings),
        item("Quit", Action::Quit),
    ]
}

fn edit_menu(app: &App) -> Vec<Entry> {
    let undo = match &app.view.undo_label {
        Some(l) => format!("Undo “{l}”"),
        None => "Undo".to_string(),
    };
    let redo = match &app.view.redo_label {
        Some(l) => format!("Redo “{l}”"),
        None => "Redo".to_string(),
    };
    let some = app.has_selection();
    let tools = Tool::ALL
        .into_iter()
        .map(|t| check(app.arrange.tool == t, t.label(), Action::Tool(t)))
        .collect();
    let moves = vec![
        item("Nudge left", Action::Nudge(-1)),
        item("Nudge right", Action::Nudge(1)),
        item("Nudge left by a bar", Action::NudgeBars(-1)),
        item("Nudge right by a bar", Action::NudgeBars(1)),
        Entry::Separator,
        item("Clips to the track above", Action::MoveToTrack(-1)),
        item("Clips to the track below", Action::MoveToTrack(1)),
        Entry::Separator,
        item("Notes up a semitone", Action::Transpose(1)),
        item("Notes down a semitone", Action::Transpose(-1)),
        item("Notes up an octave", Action::Transpose(12)),
        item("Notes down an octave", Action::Transpose(-12)),
    ];
    vec![
        item_if(app.view.undo_label.is_some(), undo, Action::Undo),
        item_if(app.view.redo_label.is_some(), redo, Action::Redo),
        Entry::Separator,
        item_if(some, "Cut", Action::Cut),
        item_if(some, "Copy", Action::Copy),
        item_if(app.edit.clipboard.is_some(), "Paste", Action::Paste),
        item_if(some, "Duplicate", Action::Duplicate),
        item_if(some, "Delete", Action::DeleteSelection),
        Entry::Separator,
        item("Split at the playhead", Action::SplitAtPlayhead),
        item_if(
            !app.sel.clips.is_empty(),
            "Mute or unmute clips",
            Action::MuteClips,
        ),
        item_if(
            app.can_quantize(),
            "Quantize notes",
            Action::Quantize(false),
        ),
        item_if(
            app.can_quantize(),
            "Quantize note starts and ends",
            Action::Quantize(true),
        ),
        Entry::Sub("Tool".into(), tools),
        Entry::Separator,
        item("Select all", Action::SelectAll),
        item("Select nothing", Action::SelectNone),
        Entry::Sub("Move the selection".into(), moves),
        Entry::Separator,
        item("Command palette…", Action::OpenPalette),
    ]
}

fn track_menu(app: &App) -> Vec<Entry> {
    let project = &app.view.project;
    let mut out: Vec<Entry> = NewTrack::ALL
        .into_iter()
        .map(|kind| {
            item(
                format!("Add {} track", kind.label().to_lowercase()),
                Action::AddTrack(kind),
            )
        })
        .collect();
    out.push(Entry::Separator);
    let effects = |channel: ChannelRef| -> Vec<Entry> {
        EffectType::ALL
            .into_iter()
            .map(|kind| item(kind.label(), Action::AddEffect { channel, kind }))
            .collect()
    };
    if let Some(track) = app.sel.track.and_then(|t| project.track(t).ok()) {
        out.push(Entry::Sub(
            format!("Add effect to {}", track.name),
            effects(ChannelRef::Track(track.id)),
        ));
    }
    out.push(Entry::Sub(
        "Add effect to master".into(),
        effects(ChannelRef::Master),
    ));
    out
}

fn transport_menu(app: &App) -> Vec<Entry> {
    let playing = app.transport.playing;
    vec![
        item(if playing { "Pause" } else { "Play" }, Action::PlayStop),
        item(
            if playing { "Stop" } else { "Back to the start" },
            Action::Stop,
        ),
        check(app.view.project.loop_enabled, "Loop", Action::ToggleLoop),
        Entry::Separator,
        item("Set tempo…", Action::PromptTempo),
        item("Add a marker at the playhead", Action::AddMarker),
    ]
}

fn view_menu(app: &App) -> Vec<Entry> {
    let mut out: Vec<Entry> = Tab::ALL
        .into_iter()
        .filter(|t| *t != Tab::Arrange)
        .map(|tab| {
            check(
                app.workspace.is_open(tab),
                tab.label(),
                Action::TogglePanel(tab),
            )
        })
        .collect();
    out.push(Entry::Separator);
    for lane in [Lane::Markers, Lane::Tempo] {
        out.push(check(
            app.workspace.lanes.shows(lane),
            lane.label(),
            Action::ToggleLane(lane),
        ));
    }
    out.extend([
        Entry::Separator,
        item("Reset the panel layout", Action::ResetLayout),
        Entry::Separator,
        item("Zoom the timeline in", Action::TimelineZoom(1)),
        item("Zoom the timeline out", Action::TimelineZoom(-1)),
        item("Fit the timeline", Action::TimelineFit),
        Entry::Separator,
        item("Make everything bigger", Action::ZoomIn),
        item("Make everything smaller", Action::ZoomOut),
        item("Actual size", Action::ZoomReset),
    ]);
    out
}

pub fn bar(app: &mut App, ui: &mut Ui) {
    let menus = menus(app);
    let mut wanted = None;
    egui::MenuBar::new().ui(ui, |ui| {
        for menu in &menus {
            ui.menu_button(menu.title, |ui| {
                ui.set_min_width(200.0);
                entries(ui, &menu.entries, &mut wanted);
            });
        }
    });
    if let Some(action) = wanted {
        let ctx = ui.ctx().clone();
        app.run_action(&ctx, action);
    }
}

fn entries(ui: &mut Ui, list: &[Entry], wanted: &mut Option<Action>) {
    for entry in list {
        match entry {
            Entry::Item(it) => {
                let label = it.label.as_str();
                let response = match it.checked {
                    Some(on) if it.shortcut.is_empty() => {
                        ui.add_enabled(it.enabled, Checkbox::new(&mut on.clone(), label))
                    }
                    Some(on) => {
                        let atoms = (label, Atom::grow(), RichText::new(&it.shortcut).weak());
                        ui.add_enabled(it.enabled, Checkbox::new(&mut on.clone(), atoms))
                    }
                    None if it.shortcut.is_empty() => {
                        ui.add_enabled(it.enabled, Button::new(label))
                    }
                    None => {
                        let button = Button::new(label).shortcut_text(it.shortcut.as_str());
                        ui.add_enabled(it.enabled, button)
                    }
                };
                let response = match &it.tip {
                    Some(tip) => response.on_hover_text(tip),
                    None => response,
                };
                if response.clicked() {
                    *wanted = Some(it.action.clone());
                }
            }
            Entry::Sub(label, list) => {
                ui.menu_button(label.as_str(), |ui| entries(ui, list, wanted));
            }
            Entry::Separator => {
                ui.separator();
            }
            Entry::Note(text) => {
                ui.label(RichText::new(*text).color(theme::TEXT_DIM));
            }
        }
    }
}

/// Help > About: the version, the license and how an AI connects.
pub fn about_window(app: &mut App, ctx: &Context) {
    if !app.about {
        return;
    }
    let mut open = true;
    Window::new("About Skyware Sound")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(RichText::new("Skyware Sound").heading().color(theme::TEXT));
            ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
            ui.add_space(4.0);
            ui.label("A music workstation for game audio, with AI assistants over MCP.");
            ui.label(
                RichText::new(
                    "Free software under the GNU General Public License, version 3 or later.",
                )
                .color(theme::TEXT_WEAK),
            );
            ui.add_space(4.0);
            let mcp = match &app.mcp {
                Some(info) => format!("MCP server: {}", info.url),
                None => "The MCP server is not running in this session.".to_string(),
            };
            ui.label(RichText::new(mcp).color(theme::TEXT_WEAK));
        });
    if !open {
        app.about = false;
    }
}
