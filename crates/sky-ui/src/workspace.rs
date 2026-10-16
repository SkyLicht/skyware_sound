// SPDX-License-Identifier: GPL-3.0-or-later
//! The dockable workspace: the arrangement and the panels around it as tabs
//! that can be stacked, put side by side or floated, and the layout and the
//! recent projects kept between runs.

use std::path::{Path, PathBuf};

use egui::{CornerRadius, Frame, Id, Margin, Stroke, Ui, WidgetInfo, WidgetText, WidgetType};
use egui_dock::{DockArea, DockState, NodeIndex, TabViewer, Translations};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::ruler_lanes::Lanes;
use crate::theme;

/// The panels of the workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tab {
    /// The tracks and their clips. Always open.
    Arrange,
    /// The selected clip's editor.
    Editor,
    Mixer,
    /// The selected track's instrument.
    Instrument,
    /// Changes made by AI assistants, and how to connect one.
    Ai,
    /// Project audio, sample packs and folders of samples.
    Browser,
    /// A game loop's seam: the waveform either side of it, and the check.
    Loop,
}

impl Tab {
    /// In the order the View menu lists them.
    pub const ALL: [Tab; 7] = [
        Tab::Arrange,
        Tab::Browser,
        Tab::Editor,
        Tab::Mixer,
        Tab::Instrument,
        Tab::Loop,
        Tab::Ai,
    ];
    /// The panels that share the lower area unless moved.
    pub const LOWER: [Tab; 5] = [Tab::Editor, Tab::Mixer, Tab::Instrument, Tab::Loop, Tab::Ai];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Arrange => "Arrangement",
            Tab::Editor => "Editor",
            Tab::Mixer => "Mixer",
            Tab::Instrument => "Instrument",
            Tab::Ai => "AI activity",
            Tab::Browser => "Browser",
            Tab::Loop => "Loop inspector",
        }
    }
}

/// How many projects File > Open recent lists.
pub const MAX_RECENT: usize = 10;

/// The share of the width the browser opens with.
const BROWSER_SHARE: f32 = 0.18;
/// The share of the height the arrangement keeps above the lower panels.
const ARRANGE_SHARE: f32 = 0.6;

/// The key the workspace is saved under in eframe's storage.
pub const STORAGE_KEY: &str = "skyware-workspace";

/// A change asked for while the dock may be out being drawn; applied
/// between frames and after the dock is drawn.
enum Change {
    Show(Tab),
    Toggle(Tab),
    Reset,
}

pub struct Workspace {
    dock: DockState<Tab>,
    changes: Vec<Change>,
    /// Open panels and the ones in front, as of the last change, so they
    /// read the same while the dock is out being drawn.
    open: Vec<Tab>,
    front: Vec<Tab>,
    /// Projects opened or saved, newest first.
    pub recent: Vec<PathBuf>,
    /// The lanes shown under the ruler.
    pub lanes: Lanes,
}

impl Default for Workspace {
    fn default() -> Self {
        let mut w = Self {
            dock: default_dock(),
            changes: Vec::new(),
            open: Vec::new(),
            front: Vec::new(),
            recent: Vec::new(),
            lanes: Lanes::default(),
        };
        w.apply();
        w
    }
}

/// The browser on the left, the arrangement above the editor, mixer,
/// instrument, loop inspector and AI tabs.
pub fn default_dock() -> DockState<Tab> {
    let mut dock = DockState::new(vec![Tab::Arrange]).with_translations(translations());
    let tree = dock.main_surface_mut();
    let [arrange, _] = tree.split_left(NodeIndex::root(), BROWSER_SHARE, vec![Tab::Browser]);
    tree.split_below(arrange, ARRANGE_SHARE, Tab::LOWER.to_vec());
    dock
}

/// The words on a tab's right-click menu.
fn translations() -> Translations {
    let mut t = Translations::english();
    t.tab_context_menu.eject_button = "Float in a window".into();
    t
}

/// Whether a saved layout can be used: each panel at most once, and the
/// arrangement in the main window.
fn usable(dock: &DockState<Tab>) -> bool {
    let tabs: Vec<Tab> = dock.iter_all_tabs().map(|(_, t)| *t).collect();
    let once = tabs
        .iter()
        .all(|t| tabs.iter().filter(|u| *u == t).count() == 1);
    once && dock.find_main_surface_tab(&Tab::Arrange).is_some()
}

#[derive(Serialize)]
struct SavedRef<'a> {
    dock: &'a DockState<Tab>,
    recent: &'a [PathBuf],
    lanes: Lanes,
}

impl Workspace {
    /// Brings a panel to the front, opening it where it belongs if closed.
    pub fn show(&mut self, tab: Tab) {
        self.changes.push(Change::Show(tab));
    }

    /// Closes an open panel or opens a closed one.
    pub fn toggle(&mut self, tab: Tab) {
        self.changes.push(Change::Toggle(tab));
    }

    /// Back to the layout of a first run.
    pub fn reset(&mut self) {
        self.changes.push(Change::Reset);
    }

    /// Whether the panel is open, in front or behind another tab.
    pub fn is_open(&self, tab: Tab) -> bool {
        self.open.contains(&tab)
    }

    /// Whether the panel is open and in front of its tab group.
    pub fn is_front(&self, tab: Tab) -> bool {
        self.front.contains(&tab)
    }

    #[cfg(test)]
    pub fn dock(&self) -> &DockState<Tab> {
        &self.dock
    }

    #[cfg(test)]
    pub fn dock_mut(&mut self) -> &mut DockState<Tab> {
        &mut self.dock
    }

    /// Carries out the changes asked for since the last call.
    pub fn apply(&mut self) {
        for change in std::mem::take(&mut self.changes) {
            match change {
                Change::Show(tab) => self.bring_forward(tab),
                Change::Toggle(tab) => {
                    if self.dock.find_tab(&tab).is_some() {
                        self.remove(tab);
                    } else {
                        self.bring_forward(tab);
                    }
                }
                Change::Reset => self.dock = default_dock(),
            }
        }
        self.open = self.dock.iter_all_tabs().map(|(_, t)| *t).collect();
        self.front = self
            .dock
            .iter_leaves()
            .filter_map(|(_, leaf)| leaf.tabs().get(leaf.active.0).copied())
            .collect();
    }

    fn remove(&mut self, tab: Tab) {
        // The arrangement stays: the window would be empty without it.
        if tab == Tab::Arrange {
            return;
        }
        if let Some(path) = self.dock.find_tab(&tab) {
            self.dock.remove_tab(path);
        }
    }

    fn bring_forward(&mut self, tab: Tab) {
        if self.dock.find_tab(&tab).is_none() {
            self.reopen(tab);
        }
        if let Some(path) = self.dock.find_tab(&tab) {
            let _ = self.dock.set_active_tab(path);
            self.dock.set_focused_node_and_surface(path.node_path());
        }
    }

    /// Opens a closed panel where it opens on a first run, or as near as
    /// the current layout allows.
    fn reopen(&mut self, tab: Tab) {
        match tab {
            Tab::Arrange => self.dock.push_to_first_leaf(tab),
            Tab::Browser => {
                self.dock.main_surface_mut().split_left(
                    NodeIndex::root(),
                    BROWSER_SHARE,
                    vec![tab],
                );
            }
            _ => {
                // A lower panel joins any other lower panel that is open.
                let sibling = Tab::LOWER.iter().find_map(|t| self.dock.find_tab(t));
                if let Some(path) = sibling
                    && let Ok(leaf) = self.dock.leaf_mut(path.node_path())
                {
                    leaf.append_tab(tab);
                    return;
                }
                match self.dock.find_main_surface_tab(&Tab::Arrange) {
                    Some((node, _)) => {
                        self.dock
                            .main_surface_mut()
                            .split_below(node, ARRANGE_SHARE, vec![tab]);
                    }
                    None => self.dock.push_to_first_leaf(tab),
                }
            }
        }
    }

    /// Puts a project at the top of the recent list.
    pub fn remember(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(MAX_RECENT);
    }

    pub fn forget(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
    }

    /// The layout, the lanes shown and the recent projects, for eframe's
    /// storage.
    pub fn saved_json(&mut self) -> String {
        self.apply();
        let saved = SavedRef {
            dock: &self.dock,
            recent: &self.recent,
            lanes: self.lanes,
        };
        serde_json::to_string(&saved).unwrap_or_default()
    }

    /// Takes back what [`saved_json`](Self::saved_json) saved. A layout this
    /// version cannot use leaves the default one in place; the recent
    /// projects are kept either way.
    pub fn restore(&mut self, json: &str) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
            tracing::warn!("the saved workspace is not JSON; using the default layout");
            return;
        };
        if let Some(recent) = value
            .get("recent")
            .and_then(|r| serde_json::from_value::<Vec<PathBuf>>(r.clone()).ok())
        {
            self.recent = recent;
            self.recent.truncate(MAX_RECENT);
        }
        if let Some(lanes) = value
            .get("lanes")
            .and_then(|l| serde_json::from_value::<Lanes>(l.clone()).ok())
        {
            self.lanes = lanes;
        }
        match value
            .get("dock")
            .map(|d| serde_json::from_value::<DockState<Tab>>(d.clone()))
        {
            // Translations are not saved.
            Some(Ok(dock)) if usable(&dock) => self.dock = dock.with_translations(translations()),
            _ => tracing::warn!("the saved layout cannot be used; using the default layout"),
        }
        self.apply();
    }
}

/// The colors and sizes of tabs and the gaps between panels.
fn style(ctx: &egui::Context) -> egui_dock::Style {
    let mut s = egui_dock::Style::from_egui(&ctx.global_style());
    s.dock_area_padding = None;
    s.main_surface_border_stroke = Stroke::NONE;
    s.main_surface_border_rounding = CornerRadius::ZERO;

    s.tab_bar.bg_fill = theme::BG;
    s.tab_bar.height = 26.0;
    s.tab_bar.hline_color = theme::GRID_BEAT;
    s.tab_bar.corner_radius = CornerRadius::ZERO;

    let t = &mut s.tab;
    t.tab_body.bg_fill = theme::PANEL;
    t.tab_body.inner_margin = Margin::ZERO;
    t.tab_body.stroke = Stroke::NONE;
    t.tab_body.corner_radius = CornerRadius::ZERO;
    for active in [
        &mut t.active,
        &mut t.focused,
        &mut t.active_with_kb_focus,
        &mut t.focused_with_kb_focus,
    ] {
        active.bg_fill = theme::PANEL;
        active.outline_color = theme::GRID_BEAT;
        active.text_color = theme::TEXT;
    }
    t.focused.text_color = egui::Color32::WHITE;
    for idle in [&mut t.inactive, &mut t.inactive_with_kb_focus] {
        idle.bg_fill = theme::BG;
        idle.outline_color = theme::BG;
        idle.text_color = theme::TEXT_WEAK;
    }
    t.hovered.bg_fill = theme::HEADER;
    t.hovered.outline_color = theme::GRID_BEAT;
    t.hovered.text_color = theme::TEXT;

    s.separator.width = 2.0;
    s.separator.color_idle = theme::BG;
    s.separator.color_hovered = theme::darken(theme::ACCENT, 0.3);
    s.separator.color_dragged = theme::ACCENT;

    s.overlay.selection_color = theme::ACCENT.gamma_multiply(0.35);
    s.overlay.button_color = theme::TEXT_WEAK;
    s
}

/// Draws every open panel in its place.
pub fn show(app: &mut App, ui: &mut Ui) {
    app.workspace.apply();
    // The panels need the app while the dock is drawn, so it is taken out
    // for the length of the draw; changes asked for meanwhile wait in
    // `changes`.
    let mut dock = std::mem::replace(&mut app.workspace.dock, DockState::new(Vec::new()));
    DockArea::new(&mut dock)
        .id(Id::new("workspace"))
        .style(style(ui.ctx()))
        .show_add_buttons(false)
        .show_leaf_close_all_buttons(false)
        .show_leaf_collapse_buttons(false)
        .show_inside(ui, &mut Panels { app });
    app.workspace.dock = dock;
    app.workspace.apply();
}

/// Draws the panels for the dock.
struct Panels<'a> {
    app: &'a mut App,
}

impl TabViewer for Panels<'_> {
    type Tab = Tab;

    fn id(&mut self, tab: &mut Tab) -> Id {
        Id::new(("workspace_tab", *tab))
    }

    fn title(&mut self, tab: &mut Tab) -> WidgetText {
        let count = self.app.view.activity.len();
        if *tab == Tab::Ai && count > 0 {
            format!("AI activity ({count})").into()
        } else {
            tab.label().into()
        }
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Tab) {
        let app = &mut *self.app;
        if *tab == Tab::Arrange {
            ui.painter()
                .rect_filled(ui.max_rect(), CornerRadius::ZERO, theme::BG);
            crate::arrange::show(app, ui);
            return;
        }
        let margin = if *tab == Tab::Browser {
            Margin::same(6)
        } else {
            Margin::symmetric(8, 6)
        };
        Frame::new().inner_margin(margin).show(ui, |ui| {
            // Keep the panel at its full size whatever it holds.
            ui.set_min_size(ui.available_size());
            match tab {
                Tab::Arrange => {}
                Tab::Editor => crate::pattern_editor::clip_editor(app, ui),
                Tab::Mixer => crate::mixer::show(app, ui),
                Tab::Instrument => crate::params::instrument_panel(app, ui),
                Tab::Ai => crate::ai_panel::show(app, ui),
                Tab::Browser => crate::browser::panel(app, ui),
                Tab::Loop => crate::loop_inspector::show(app, ui),
            }
        });
    }

    fn on_tab_button(&mut self, tab: &mut Tab, response: &egui::Response) {
        // Name the tab for screen readers (and for the tests).
        let label = tab.label();
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
        response.clone().on_hover_text(
            "Drag the tab beside or below another panel to see both, or right-click it to float it",
        );
    }

    fn is_closeable(&self, tab: &Tab) -> bool {
        *tab != Tab::Arrange
    }

    fn allowed_in_windows(&self, tab: &mut Tab) -> bool {
        *tab != Tab::Arrange
    }

    fn scroll_bars(&self, _tab: &Tab) -> [bool; 2] {
        // Each panel scrolls what needs scrolling itself.
        [false, false]
    }
}
