// SPDX-License-Identifier: GPL-3.0-or-later
//! Colors and the egui style: one dark theme with a single accent color.

use egui::{Color32, CornerRadius, FontId, Stroke, TextStyle, Theme, Vec2};

/// The one accent color: selection, the playhead, active toggles.
pub const ACCENT: Color32 = Color32::from_rgb(64, 168, 255);
/// The accent at low strength, for backgrounds of selected things.
pub const ACCENT_BG: Color32 = Color32::from_rgb(28, 58, 88);

pub const BG: Color32 = Color32::from_rgb(22, 24, 29);
pub const PANEL: Color32 = Color32::from_rgb(28, 31, 37);
pub const LANE: Color32 = Color32::from_rgb(32, 35, 42);
pub const LANE_ALT: Color32 = Color32::from_rgb(35, 38, 46);
pub const HEADER: Color32 = Color32::from_rgb(38, 42, 50);
pub const HEADER_SELECTED: Color32 = Color32::from_rgb(40, 56, 74);
pub const GRID_FAINT: Color32 = Color32::from_rgb(42, 46, 55);
pub const GRID_BEAT: Color32 = Color32::from_rgb(50, 55, 66);
pub const GRID_BAR: Color32 = Color32::from_rgb(70, 77, 92);
pub const TEXT: Color32 = Color32::from_rgb(216, 220, 228);
pub const TEXT_WEAK: Color32 = Color32::from_rgb(138, 146, 163);
pub const TEXT_DIM: Color32 = Color32::from_rgb(96, 102, 116);
pub const LOOP_FILL: Color32 = Color32::from_rgba_premultiplied(30, 80, 128, 60);

/// Functional colors that are not the accent: problems and meter zones.
pub const ERROR: Color32 = Color32::from_rgb(235, 96, 96);
pub const WARN: Color32 = Color32::from_rgb(232, 182, 72);
pub const OK: Color32 = Color32::from_rgb(110, 196, 120);
pub const MUTE: Color32 = Color32::from_rgb(196, 150, 60);

/// Game assets on the timeline: loops and SFX sets.
pub const GAME_LOOP: Color32 = Color32::from_rgb(72, 196, 172);
pub const GAME_SFX: Color32 = Color32::from_rgb(222, 188, 86);

/// A track's stored color.
pub fn track_color(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

/// A color darkened toward black by `amount` (0..1).
pub fn darken(c: Color32, amount: f32) -> Color32 {
    c.lerp_to_gamma(Color32::BLACK, amount.clamp(0.0, 1.0))
}

/// A muted clip's color: the track's, mostly grey.
pub fn muted(c: Color32) -> Color32 {
    c.lerp_to_gamma(Color32::from_gray(96), 0.8)
}

/// A color lightened toward white by `amount` (0..1).
pub fn lighten(c: Color32, amount: f32) -> Color32 {
    c.lerp_to_gamma(Color32::WHITE, amount.clamp(0.0, 1.0))
}

pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

pub fn small() -> FontId {
    FontId::proportional(11.5)
}

pub fn body() -> FontId {
    FontId::proportional(13.5)
}

/// Installs the dark theme on a context.
pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(Theme::Dark);
    ctx.style_mut_of(Theme::Dark, |style| {
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(11.5)),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Heading, FontId::proportional(18.0)),
            (TextStyle::Monospace, FontId::monospace(13.5)),
        ]
        .into();
        let s = &mut style.spacing;
        s.item_spacing = Vec2::new(6.0, 5.0);
        s.button_padding = Vec2::new(8.0, 3.0);
        s.interact_size.y = 22.0;
        s.slider_width = 120.0;

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.panel_fill = PANEL;
        v.window_fill = Color32::from_rgb(30, 33, 40);
        v.window_stroke = Stroke::new(1.0, Color32::from_rgb(56, 61, 72));
        v.window_corner_radius = CornerRadius::same(6);
        v.menu_corner_radius = CornerRadius::same(5);
        v.extreme_bg_color = BG;
        v.faint_bg_color = Color32::from_rgb(33, 36, 43);
        v.code_bg_color = Color32::from_rgb(36, 40, 48);
        v.hyperlink_color = ACCENT;
        v.selection.bg_fill = ACCENT_BG;
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.slider_trailing_fill = true;
        v.override_text_color = None;
        v.error_fg_color = ERROR;
        v.warn_fg_color = WARN;

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = PANEL;
        w.noninteractive.weak_bg_fill = PANEL;
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(48, 52, 62));
        w.inactive.bg_fill = Color32::from_rgb(44, 48, 58);
        w.inactive.weak_bg_fill = Color32::from_rgb(44, 48, 58);
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        w.hovered.bg_fill = Color32::from_rgb(54, 60, 72);
        w.hovered.weak_bg_fill = Color32::from_rgb(54, 60, 72);
        w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(88, 120, 150));
        w.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
        w.active.bg_fill = Color32::from_rgb(40, 74, 108);
        w.active.weak_bg_fill = Color32::from_rgb(40, 74, 108);
        w.active.bg_stroke = Stroke::new(1.0, ACCENT);
        w.open.bg_fill = Color32::from_rgb(48, 54, 66);
        w.open.weak_bg_fill = Color32::from_rgb(48, 54, 66);
        for wv in [
            &mut w.noninteractive,
            &mut w.inactive,
            &mut w.hovered,
            &mut w.active,
            &mut w.open,
        ] {
            wv.corner_radius = CornerRadius::same(4);
        }
    });
}
