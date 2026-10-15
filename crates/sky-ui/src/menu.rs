// SPDX-License-Identifier: GPL-3.0-or-later
//! The menu bar along the top of the window, and Help > About.
//!
//! The menus are built as data by [`menus`] and drawn by [`bar`], so the
//! tests can read what each menu offers.

use egui::{Align2, Atom, Button, Checkbox, Context, RichText, Ui, Window};
use sky_dsp::EffectType;
use sky_model::ChannelRef;

use crate::actions::NewTrack;
use crate::app::{Action, App, Tab};
use crate::theme;

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
    pub shortcut: &'static str,
    pub action: Action,
    pub enabled: bool,
    /// Drawn as a checkbox when set.
    pub checked: Option<bool>,
    pub tip: Option<String>,
}

fn item(label: impl Into<String>, shortcut: &'static str, action: Action) -> Entry {
    Entry::Item(Item {
        label: label.into(),
        shortcut,
        action,
        enabled: true,
        checked: None,
        tip: None,
    })
}

/// An item that can be greyed out.
fn item_if(
    enabled: bool,
    label: impl Into<String>,
    shortcut: &'static str,
    action: Action,
) -> Entry {
    let Entry::Item(mut it) = item(label, shortcut, action) else {
        unreachable!()
    };
    it.enabled = enabled;
    Entry::Item(it)
}

fn check(on: bool, label: impl Into<String>, shortcut: &'static str, action: Action) -> Entry {
    let Entry::Item(mut it) = item(label, shortcut, action) else {
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
                item("Command palette…", "Ctrl+K", Action::OpenPalette),
                item("About Skyware Sound", "", Action::About),
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
            let Entry::Item(mut it) = item(name, "", Action::OpenRecent(path.clone())) else {
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
        recent.push(item("Clear the list", "", Action::ClearRecent));
    }
    vec![
        item("New empty project", "", Action::NewEmpty),
        item("New demo project", "", Action::NewDemo),
        item("New techno loop", "", Action::NewTechno),
        item("New acid loop", "", Action::NewAcid),
        Entry::Separator,
        item("Open project…", "Ctrl+O", Action::Open),
        Entry::Sub("Open recent".into(), recent),
        item("Save", "Ctrl+S", Action::Save),
        item("Save as…", "Ctrl+Shift+S", Action::SaveAs),
        Entry::Separator,
        item("Import audio…", "", Action::ImportAudio),
        item("Load sounds from a sample pack…", "", Action::LoadSounds),
        item("Add a folder to the browser…", "", Action::AddBrowserFolder),
        Entry::Separator,
        item("Export WAV…", "", Action::ExportWav),
        item("Export loop for a game…", "", Action::ExportLoop),
        item("Game assets…", "", Action::GameAssets),
        Entry::Separator,
        item("Audio settings…", "", Action::OpenSettings),
        item("Quit", "Ctrl+Q", Action::Quit),
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
    vec![
        item_if(app.view.undo_label.is_some(), undo, "Ctrl+Z", Action::Undo),
        item_if(
            app.view.redo_label.is_some(),
            redo,
            "Ctrl+Shift+Z",
            Action::Redo,
        ),
        Entry::Separator,
        item_if(
            app.sel.clip.is_some(),
            "Delete",
            "Del",
            Action::DeleteSelection,
        ),
        Entry::Separator,
        item("Command palette…", "Ctrl+K", Action::OpenPalette),
    ]
}

fn track_menu(app: &App) -> Vec<Entry> {
    let project = &app.view.project;
    let mut out: Vec<Entry> = NewTrack::ALL
        .into_iter()
        .map(|kind| {
            item(
                format!("Add {} track", kind.label().to_lowercase()),
                "",
                Action::AddTrack(kind),
            )
        })
        .collect();
    out.push(Entry::Separator);
    let effects = |channel: ChannelRef| -> Vec<Entry> {
        EffectType::ALL
            .into_iter()
            .map(|kind| item(kind.label(), "", Action::AddEffect { channel, kind }))
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
        item(
            if playing { "Pause" } else { "Play" },
            "Space",
            Action::PlayStop,
        ),
        item(
            if playing { "Stop" } else { "Back to the start" },
            "",
            Action::Stop,
        ),
        check(
            app.view.project.loop_enabled,
            "Loop",
            "",
            Action::ToggleLoop,
        ),
        Entry::Separator,
        item("Set tempo…", "", Action::PromptTempo),
    ]
}

fn view_menu(app: &App) -> Vec<Entry> {
    let mut out: Vec<Entry> = Tab::ALL
        .into_iter()
        .filter(|t| *t != Tab::Arrange)
        .map(|tab| {
            let shortcut = if tab == Tab::Browser { "Ctrl+B" } else { "" };
            check(
                app.workspace.is_open(tab),
                tab.label(),
                shortcut,
                Action::TogglePanel(tab),
            )
        })
        .collect();
    out.extend([
        Entry::Separator,
        item("Reset the panel layout", "", Action::ResetLayout),
        Entry::Separator,
        item("Zoom in", "Ctrl+Plus", Action::ZoomIn),
        item("Zoom out", "Ctrl+Minus", Action::ZoomOut),
        item("Actual size", "Ctrl+0", Action::ZoomReset),
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
                        let atoms = (label, Atom::grow(), RichText::new(it.shortcut).weak());
                        ui.add_enabled(it.enabled, Checkbox::new(&mut on.clone(), atoms))
                    }
                    None if it.shortcut.is_empty() => {
                        ui.add_enabled(it.enabled, Button::new(label))
                    }
                    None => {
                        ui.add_enabled(it.enabled, Button::new(label).shortcut_text(it.shortcut))
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
