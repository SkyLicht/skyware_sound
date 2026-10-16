// SPDX-License-Identifier: GPL-3.0-or-later
//! The key command table: every keyboard shortcut, where it works and what
//! it runs.
//!
//! The app's shortcuts run from it, the menus and the command palette show
//! its keys next to their commands, and Help > Keyboard shortcuts prints it.
//! `docs/keyboard-shortcuts.md` is made from it too, and a test keeps the two
//! the same.

use egui::{Align2, Context, Key, KeyboardShortcut, Modifiers, RichText, ScrollArea, Window, vec2};

use crate::app::{Action, App, Focus};
use crate::clip_tools::Tool;
use crate::dialogs::PathPurpose;
use crate::theme;

/// Where a key command works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Anywhere,
    /// When the arrangement was clicked last.
    Arrange,
    /// When the piano roll was clicked last and is in view.
    Editor,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::Anywhere => "Anywhere",
            Scope::Arrange => "Arrangement",
            Scope::Editor => "Piano roll",
        }
    }

    pub fn applies(self, focus: Focus) -> bool {
        match self {
            Scope::Anywhere => true,
            Scope::Arrange => focus == Focus::Arrange,
            Scope::Editor => focus == Focus::Editor,
        }
    }
}

pub struct KeyCommand {
    /// The heading it is listed under.
    pub group: &'static str,
    /// What it does, for the printed list.
    pub label: &'static str,
    /// The first is the one menus show.
    pub keys: Vec<KeyboardShortcut>,
    pub scope: Scope,
    pub action: Action,
    /// Also works while typing in a text field, such as Ctrl+S.
    pub while_typing: bool,
}

impl KeyCommand {
    fn new(
        group: &'static str,
        label: &'static str,
        keys: &[KeyboardShortcut],
        action: Action,
    ) -> Self {
        Self {
            group,
            label,
            keys: keys.to_vec(),
            scope: Scope::Anywhere,
            action,
            while_typing: false,
        }
    }

    fn only(mut self, scope: Scope) -> Self {
        self.scope = scope;
        self
    }

    fn typing(mut self) -> Self {
        self.while_typing = true;
        self
    }

    /// Every key, as in "Ctrl+Shift+Z or Ctrl+Y".
    pub fn keys_text(&self) -> String {
        let keys: Vec<String> = self.keys.iter().map(text).collect();
        keys.join(" or ")
    }
}

const fn plain(key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(Modifiers::NONE, key)
}

const fn ctrl(key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(Modifiers::COMMAND, key)
}

const fn shift(key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(Modifiers::SHIFT, key)
}

const fn ctrl_shift(key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(
        Modifiers {
            alt: false,
            ctrl: false,
            shift: true,
            mac_cmd: false,
            command: true,
        },
        key,
    )
}

/// The groups, in the order they are listed.
pub const GROUPS: [&str; 7] = [
    "File",
    "Edit",
    "Selection",
    "Tools",
    "Transport",
    "View",
    "Help",
];

/// Every key command.
pub fn table() -> Vec<KeyCommand> {
    use Action as A;
    use Key::*;
    use Scope::{Arrange, Editor};
    let k = KeyCommand::new;
    let tools = Tool::ALL
        .into_iter()
        .map(|tool| k("Tools", tool.command(), &[plain(tool.key())], A::Tool(tool)).only(Arrange));
    let mut table = vec![
        k("File", "Open a project", &[ctrl(O)], A::Open).typing(),
        k("File", "Save", &[ctrl(S)], A::Save).typing(),
        k("File", "Save as a new project", &[ctrl_shift(S)], A::SaveAs).typing(),
        k("File", "Quit", &[ctrl(Q)], A::Quit).typing(),
        k("Edit", "Undo", &[ctrl(Z)], A::Undo),
        k("Edit", "Redo", &[ctrl_shift(Z), ctrl(Y)], A::Redo),
        k("Edit", "Cut", &[ctrl(X)], A::Cut),
        k("Edit", "Copy", &[ctrl(C)], A::Copy),
        k("Edit", "Paste at the playhead", &[ctrl(V)], A::Paste),
        k("Edit", "Duplicate right after", &[ctrl(D)], A::Duplicate),
        k(
            "Edit",
            "Delete",
            &[plain(Delete), plain(Backspace)],
            A::DeleteSelection,
        ),
        k(
            "Edit",
            "Split clips at the playhead",
            &[ctrl(E)],
            A::SplitAtPlayhead,
        )
        .only(Arrange),
        k("Edit", "Mute or unmute clips", &[shift(M)], A::MuteClips).only(Arrange),
        k("Edit", "Select all", &[ctrl(A)], A::SelectAll),
        k("Edit", "Select nothing", &[plain(Escape)], A::SelectNone),
        k(
            "Selection",
            "Nudge left by the grid",
            &[plain(ArrowLeft)],
            A::Nudge(-1),
        ),
        k(
            "Selection",
            "Nudge right by the grid",
            &[plain(ArrowRight)],
            A::Nudge(1),
        ),
        k(
            "Selection",
            "Nudge left by a bar",
            &[shift(ArrowLeft)],
            A::NudgeBars(-1),
        ),
        k(
            "Selection",
            "Nudge right by a bar",
            &[shift(ArrowRight)],
            A::NudgeBars(1),
        ),
        k(
            "Selection",
            "Move clips to the track above",
            &[plain(ArrowUp)],
            A::MoveToTrack(-1),
        )
        .only(Arrange),
        k(
            "Selection",
            "Move clips to the track below",
            &[plain(ArrowDown)],
            A::MoveToTrack(1),
        )
        .only(Arrange),
        k(
            "Selection",
            "Notes up a semitone",
            &[plain(ArrowUp)],
            A::Transpose(1),
        )
        .only(Editor),
        k(
            "Selection",
            "Notes down a semitone",
            &[plain(ArrowDown)],
            A::Transpose(-1),
        )
        .only(Editor),
        k(
            "Selection",
            "Notes up an octave",
            &[shift(ArrowUp)],
            A::Transpose(12),
        )
        .only(Editor),
        k(
            "Selection",
            "Notes down an octave",
            &[shift(ArrowDown)],
            A::Transpose(-12),
        )
        .only(Editor),
        k(
            "Selection",
            "Quantize notes to the grid",
            &[plain(Q)],
            A::Quantize(false),
        )
        .only(Editor),
        k(
            "Selection",
            "Quantize note starts and ends",
            &[shift(Q)],
            A::Quantize(true),
        )
        .only(Editor),
        k("Transport", "Play or pause", &[plain(Space)], A::PlayStop),
        k("Transport", "Loop on or off", &[ctrl(L)], A::ToggleLoop),
        k(
            "Transport",
            "Add a marker at the playhead",
            &[plain(M)],
            A::AddMarker,
        ),
        k(
            "View",
            "Show or hide the browser",
            &[ctrl(B)],
            A::TogglePanel(crate::app::Tab::Browser),
        )
        .typing(),
        k(
            "View",
            "Zoom the timeline in",
            &[plain(Plus), plain(Equals)],
            A::TimelineZoom(1),
        ),
        k(
            "View",
            "Zoom the timeline out",
            &[plain(Minus)],
            A::TimelineZoom(-1),
        ),
        k(
            "View",
            "Fit the timeline to the song or clip",
            &[plain(F)],
            A::TimelineFit,
        ),
        k(
            "View",
            "Make everything bigger",
            &[ctrl(Plus), ctrl(Equals)],
            A::ZoomIn,
        )
        .typing(),
        k(
            "View",
            "Make everything smaller",
            &[ctrl(Minus)],
            A::ZoomOut,
        )
        .typing(),
        k("View", "Actual size", &[ctrl(Num0)], A::ZoomReset).typing(),
        k("Help", "Command palette", &[ctrl(K)], A::OpenPalette).typing(),
        k("Help", "Keyboard shortcuts", &[ctrl(Slash)], A::Shortcuts).typing(),
    ];
    table.extend(tools);
    table
}

/// The keys menus show for an action, such as "Ctrl+D", or "" if none.
pub fn shortcut_for(action: &Action) -> String {
    table()
        .iter()
        .find(|c| c.action == *action)
        .and_then(|c| c.keys.first())
        .map(text)
        .unwrap_or_default()
}

/// A shortcut as people write it, such as "Ctrl+Shift+S" or "Shift+Left".
/// Ctrl stands for Cmd on a Mac.
pub fn text(shortcut: &KeyboardShortcut) -> String {
    let m = shortcut.modifiers;
    let mut out = String::new();
    if m.command || m.ctrl || m.mac_cmd {
        out.push_str("Ctrl+");
    }
    if m.alt {
        out.push_str("Alt+");
    }
    if m.shift {
        out.push_str("Shift+");
    }
    out.push_str(match shortcut.logical_key {
        Key::ArrowLeft => "Left",
        Key::ArrowRight => "Right",
        Key::ArrowUp => "Up",
        Key::ArrowDown => "Down",
        Key::Delete => "Del",
        Key::Escape => "Esc",
        Key::Equals => "=",
        Key::Slash => "/",
        other => other.name(),
    });
    out
}

/// How many modifiers a shortcut needs, to try Shift+Left before Left:
/// egui matches a key even when an extra Shift is held.
pub fn modifier_count(m: Modifiers) -> usize {
    [m.command || m.ctrl || m.mac_cmd, m.alt, m.shift]
        .into_iter()
        .filter(|on| *on)
        .count()
}

const NOTE: &str = "Ctrl is Cmd on a Mac. Keys marked Arrangement or Piano roll work in \
                    whichever of the two you clicked last.";

/// The list as Markdown, grouped like the Help window.
pub fn markdown() -> String {
    let table = table();
    let mut out = String::from("# Keyboard shortcuts\n\n");
    out.push_str(
        "<!-- Made from crates/sky-ui/src/keys.rs; a test fails when the two differ. -->\n\n",
    );
    out.push_str(NOTE);
    out.push('\n');
    for group in GROUPS {
        out.push_str(&format!(
            "\n## {group}\n\n| Keys | Does | Where |\n| --- | --- | --- |\n"
        ));
        for c in table.iter().filter(|c| c.group == group) {
            out.push_str(&format!(
                "| {} | {} | {} |\n",
                c.keys_text(),
                c.label,
                c.scope.label()
            ));
        }
    }
    out
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The list as a web page to print.
pub fn html() -> String {
    let table = table();
    let mut out = String::from(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>Skyware Sound keyboard shortcuts</title>\n<style>\n\
         body { font: 14px/1.4 system-ui, sans-serif; margin: 2em; color: #111; }\n\
         h1 { font-size: 1.6em; }\n\
         h2 { font-size: 1.15em; margin-top: 1.4em; break-after: avoid; }\n\
         table { border-collapse: collapse; width: 100%; break-inside: avoid; }\n\
         th, td { text-align: left; padding: 4px 10px; border-bottom: 1px solid #ccc; }\n\
         td:first-child { font-family: ui-monospace, monospace; white-space: nowrap; width: 14em; }\n\
         td:last-child { color: #555; width: 8em; }\n\
         </style>\n</head>\n<body>\n<h1>Skyware Sound keyboard shortcuts</h1>\n",
    );
    out.push_str(&format!("<p>{}</p>\n", escape_html(NOTE)));
    for group in GROUPS {
        out.push_str(&format!(
            "<h2>{}</h2>\n<table>\n<tr><th>Keys</th><th>Does</th><th>Where</th></tr>\n",
            escape_html(group)
        ));
        for c in table.iter().filter(|c| c.group == group) {
            out.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                escape_html(&c.keys_text()),
                escape_html(c.label),
                escape_html(c.scope.label())
            ));
        }
        out.push_str("</table>\n");
    }
    out.push_str("</body>\n</html>\n");
    out
}

/// Help > Keyboard shortcuts.
pub fn window(app: &mut App, ctx: &Context) {
    if !app.shortcuts_open {
        return;
    }
    let mut open = true;
    let mut save = false;
    Window::new("Keyboard shortcuts")
        .open(&mut open)
        .collapsible(false)
        .pivot(Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .default_size(vec2(540.0, 600.0))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button("Copy the list")
                    .on_hover_text("Copy every shortcut as text, to paste elsewhere")
                    .clicked()
                {
                    ui.ctx().copy_text(markdown());
                    app.info("Copied the keyboard shortcuts");
                }
                if ui
                    .button("Save as a web page…")
                    .on_hover_text("A page to print or keep open in a browser")
                    .clicked()
                {
                    save = true;
                }
            });
            ui.label(RichText::new(NOTE).small().color(theme::TEXT_WEAK));
            ui.separator();
            let table = table();
            ScrollArea::vertical().show(ui, |ui| {
                // One grid for every group, so the columns line up.
                egui::Grid::new("shortcuts")
                    .num_columns(3)
                    .spacing(vec2(14.0, 4.0))
                    .show(ui, |ui| {
                        for group in GROUPS {
                            ui.label(RichText::new(group).strong().color(theme::TEXT));
                            ui.end_row();
                            for c in table.iter().filter(|c| c.group == group) {
                                ui.label(RichText::new(c.keys_text()).font(theme::mono(12.0)));
                                ui.label(c.label);
                                let scope = match c.scope {
                                    Scope::Anywhere => "",
                                    other => other.label(),
                                };
                                ui.label(RichText::new(scope).small().color(theme::TEXT_DIM));
                                ui.end_row();
                            }
                        }
                    });
            });
        });
    if !open {
        app.shortcuts_open = false;
    }
    if save {
        app.request_path(ctx, PathPurpose::ShortcutsPage);
    }
}
