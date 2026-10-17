// SPDX-License-Identifier: GPL-3.0-or-later
//! The picture editor: one small window to give a track, pack, bank or
//! folder its own picture. An image (chosen, pasted or dropped, filling or
//! fitting its frame, zoomed and moved), or a symbol and a color, with the
//! pictures picked lately and a way back to the default. Changes show at
//! once; a track's are undo steps like any other edit.

use std::path::PathBuf;

use egui::{
    Align, Context, CornerRadius, Id, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui,
    Vec2, WidgetInfo, WidgetType, Window, pos2, vec2,
};
use sky_model::{Picture, PictureFit, PictureImage, Symbol, picture::MAX_ZOOM};

use crate::actions;
use crate::app::App;
use crate::dialogs::PathPurpose;
use crate::jobs::JobOutput;
use crate::pictures::{self, Shown, Style, Target, fill_uv, fit_rect};
use crate::theme;

/// Colors offered beside Custom.
pub const SWATCHES: [[u8; 3]; 12] = [
    [226, 92, 84],
    [232, 128, 58],
    [222, 178, 64],
    [150, 186, 72],
    [72, 176, 108],
    [62, 166, 154],
    [72, 160, 214],
    [86, 120, 214],
    [148, 98, 222],
    [206, 96, 180],
    [150, 120, 96],
    [120, 128, 140],
];

const WIDTH: f32 = 344.0;

/// The open picture editor.
pub struct Editor {
    pub target: Target,
    /// Where it was opened from, so it opens beside it.
    at: Pos2,
    /// The target's own picture when it opened, so closing knows whether a
    /// new one was picked.
    original: Option<Picture>,
    /// An image is being read for it.
    pub loading: bool,
    pub error: Option<String>,
}

impl Editor {
    pub fn new(target: Target, at: Pos2, original: Option<Picture>) -> Self {
        Self {
            target,
            at,
            original,
            loading: false,
            error: None,
        }
    }
}

impl App {
    /// What a picture is for, in words.
    pub fn target_name(&self, target: &Target) -> String {
        match target {
            Target::Browser { name, .. } => name.clone(),
            Target::Track(id) => self
                .view
                .project
                .track(*id)
                .map_or_else(|_| "Track".to_string(), |t| t.name.clone()),
        }
    }

    /// The picture someone gave a target, if any.
    pub fn own_picture(&self, target: &Target) -> Option<Picture> {
        match target {
            Target::Browser { key, .. } => self.pictures.store.get(key).cloned(),
            Target::Track(id) => match &self.pictures.preview {
                Some((p, picture)) if p == id => picture.clone(),
                _ => self.view.project.track(*id).ok()?.picture.clone(),
            },
        }
    }

    /// What a target shows now, or `None` when it is gone.
    pub fn shown_of(&self, target: &Target) -> Option<Shown> {
        match target {
            Target::Browser { key, default, .. } => Some(self.pictures.browser_shown(key, default)),
            Target::Track(id) => {
                let project = &self.view.project;
                let track = project.track(*id).ok()?;
                Some(self.pictures.track_shown(project, track))
            }
        }
    }

    /// Gives a target its own picture, or takes it away with `None`. For a
    /// track it is an undo step. A change made by a drag (`gesture`) shows
    /// at once and goes into the project when the drag ends.
    pub fn set_picture(&mut self, target: &Target, picture: Option<Picture>, gesture: bool) {
        match target {
            Target::Browser { key, .. } => self.pictures.store.set(key, picture),
            Target::Track(id) if gesture => self.pictures.preview = Some((*id, picture)),
            Target::Track(id) => {
                if self.pictures.preview.as_ref().is_some_and(|(p, _)| p == id) {
                    self.pictures.preview = None;
                }
                let r = actions::set_track_picture(&self.session, *id, picture, None);
                if self.report(r).is_some() {
                    self.refresh();
                }
            }
        }
    }

    /// Puts a track picture changed by a drag into the project once the
    /// drag is over.
    pub fn commit_picture_preview(&mut self, ctx: &Context) {
        if self.pictures.preview.is_none() || ctx.input(|i| i.pointer.any_down()) {
            return;
        }
        if let Some((id, picture)) = self.pictures.preview.take() {
            self.set_picture(&Target::Track(id), picture, false);
        }
    }

    /// Gives a target a picture and keeps it among the recent ones.
    pub fn use_picture(&mut self, target: &Target, picture: Picture) {
        self.pictures.store.remember(&picture);
        self.set_picture(target, Some(picture), false);
    }

    pub fn open_picture_editor(&mut self, target: Target, at: Pos2) {
        if let Some(e) = &self.pictures.editor
            && e.target == target
        {
            return;
        }
        self.close_picture_editor();
        let original = self.own_picture(&target);
        self.pictures.editor = Some(Editor::new(target, at, original));
    }

    pub fn close_picture_editor(&mut self) {
        let Some(editor) = self.pictures.editor.take() else {
            return;
        };
        let now = self.own_picture(&editor.target);
        if let Some(p) = now.filter(|p| Some(p) != editor.original.as_ref()) {
            self.pictures.store.remember(&p);
        }
    }

    fn editor_for(&mut self, target: &Target) -> Option<&mut Editor> {
        self.pictures
            .editor
            .as_mut()
            .filter(|e| &e.target == target)
    }

    /// Reads an image file in the background and gives it to `target`.
    pub fn load_picture_file(&mut self, ctx: &Context, target: Target, path: PathBuf) {
        if let Some(e) = self.editor_for(&target) {
            e.loading = true;
            e.error = None;
        }
        self.jobs
            .spawn(ctx, None, move || JobOutput::PictureLoaded {
                result: pictures::image_from_file(&path),
                target,
            });
    }

    /// Takes an image, or an image file, copied in another app.
    pub fn paste_picture_image(&mut self, ctx: &Context, target: Target) {
        if let Some(e) = self.editor_for(&target) {
            e.loading = true;
            e.error = None;
        }
        self.jobs
            .spawn(ctx, None, move || JobOutput::PictureLoaded {
                result: pictures::clipboard_image(),
                target,
            });
    }

    /// An image read for a target.
    pub fn picture_loaded(&mut self, target: Target, result: Result<PictureImage, String>) {
        let in_editor = self.editor_for(&target).is_some();
        if let Some(e) = self.editor_for(&target) {
            e.loading = false;
            e.error = result.as_ref().err().cloned();
        }
        match result {
            Ok(mut image) => {
                // A track's new image keeps the tint its last one had.
                if let Some(Picture::Image(old)) = self.own_picture(&target) {
                    image.tint = old.tint;
                }
                let name = image.name.clone();
                self.use_picture(&target, Picture::Image(image));
                if !in_editor {
                    let what = self.target_name(&target);
                    self.info(format!("{what} now shows {name}"));
                }
            }
            Err(e) if !in_editor => self.error(e),
            Err(_) => {}
        }
    }

    /// Image files dropped on the window: they go to the open picture
    /// editor, or the picture they were dropped on. When neither is known,
    /// the next picture clicked takes the first one.
    pub fn take_dropped_images(&mut self, ctx: &Context, images: Vec<PathBuf>) {
        let Some(first) = images.first().cloned() else {
            return;
        };
        let name = first.file_name().map_or_else(
            || "the image".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let target = self
            .pictures
            .editor
            .as_ref()
            .map(|e| e.target.clone())
            .or_else(|| self.pictures.last_hovered.clone());
        match target {
            Some(target) => {
                if images.len() > 1 {
                    self.info(format!("Used {name}; a picture takes one image"));
                }
                self.load_picture_file(ctx, target, first);
            }
            None => {
                self.info(format!(
                    "Click the picture that should show {name}, or press Esc"
                ));
                self.pictures.waiting = Some(first);
            }
        }
    }

    /// Change picture…, Reset picture, Copy picture and Paste picture, for
    /// a picture's context menu.
    pub fn picture_menu(&mut self, ui: &mut Ui, target: &Target, at: Pos2) {
        if ui.button("Change picture…").clicked() {
            self.open_picture_editor(target.clone(), at);
        }
        let own = self.own_picture(target);
        if ui
            .add_enabled(own.is_some(), egui::Button::new("Reset picture"))
            .on_disabled_hover_text("It shows its default picture")
            .clicked()
        {
            self.set_picture(target, None, false);
        }
        if ui.button("Copy picture").clicked() {
            let picture = own.or_else(|| match self.shown_of(target) {
                Some(Shown::Symbol { symbol, color }) => {
                    let c = color.to_array();
                    Some(Picture::Symbol {
                        symbol,
                        color: Some([c[0], c[1], c[2]]),
                    })
                }
                Some(Shown::Image { image, .. }) => Some(Picture::Image(image)),
                None => None,
            });
            self.pictures.copied = picture;
        }
        let copied = self.pictures.copied.clone();
        if ui
            .add_enabled(copied.is_some(), egui::Button::new("Paste picture"))
            .on_disabled_hover_text("Copy a picture first")
            .clicked()
            && let Some(p) = copied
        {
            self.use_picture(target, p);
        }
    }
}

/// A painted tile that acts as a button, with a label for screen readers
/// and tests.
fn tile_button(
    app: &mut App,
    ui: &mut Ui,
    size: Vec2,
    shown: &Shown,
    label: &str,
    on: bool,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, on, label));
    if ui.is_rect_visible(rect) {
        let radius = (size.y * 0.18).round();
        pictures::paint(
            ui.painter(),
            rect,
            radius,
            shown,
            &mut app.pictures.images,
            Style::Tile,
        );
        if on || resp.hovered() {
            ui.painter().rect_stroke(
                rect.expand(1.5),
                CornerRadius::same(radius as u8 + 2),
                Stroke::new(2.0, if on { theme::ACCENT } else { theme::TEXT_WEAK }),
                StrokeKind::Outside,
            );
        }
    }
    resp.on_hover_text(label).clicked()
}

fn section(ui: &mut Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(text).small().strong().color(theme::TEXT_WEAK));
}

/// The picture editor window, when it is open.
pub fn show(app: &mut App, ctx: &Context) {
    let Some(editor) = &app.pictures.editor else {
        return;
    };
    let target = editor.target.clone();
    let at = editor.at;
    let Some(shown) = app.shown_of(&target) else {
        // The track was deleted.
        app.pictures.editor = None;
        return;
    };
    let name = app.target_name(&target);
    let mut open = true;
    let mut done = false;
    let screen = ctx.content_rect();
    let pos = pos2(
        (at.x + 16.0)
            .min(screen.right() - WIDTH - 24.0)
            .max(screen.left()),
        (at.y - 40.0)
            .min(screen.bottom() - 560.0)
            .max(screen.top() + 30.0),
    );
    Window::new(format!("Picture · {name}"))
        .id(Id::new(("picture_editor", pictures::picture_id(&target))))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_pos(pos)
        .show(ctx, |ui| {
            ui.set_width(WIDTH);
            body(app, ui, &target, &shown, &name, &mut done);
        });
    if !open || done {
        app.close_picture_editor();
    }
}

fn body(app: &mut App, ui: &mut Ui, target: &Target, shown: &Shown, name: &str, done: &mut bool) {
    let own = app.own_picture(target);
    let is_track = matches!(target, Target::Track(_));
    previews(app, ui, target, shown, own.as_ref(), name);

    // ----- your image -----
    section(ui, "Your image");
    let ctx = ui.ctx().clone();
    let files_held = ctx.input(|i| !i.raw.hovered_files.is_empty());
    let zone = egui::Frame::new()
        .stroke(Stroke::new(
            1.0,
            if files_held {
                theme::ACCENT
            } else {
                theme::GRID_BAR
            },
        ))
        .fill(if files_held {
            theme::ACCENT_BG
        } else {
            theme::BG
        })
        .corner_radius(CornerRadius::same(5))
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if ui.button("Choose file…").clicked() {
                    app.pictures.choosing = Some(target.clone());
                    app.request_path(&ctx, PathPurpose::PictureImage);
                }
                if ui
                    .button("Paste")
                    .on_hover_text("An image, or an image file, copied in another app")
                    .clicked()
                {
                    app.paste_picture_image(&ctx, target.clone());
                }
                let loading = app.pictures.editor.as_ref().is_some_and(|e| e.loading);
                if loading {
                    ui.spinner();
                } else {
                    ui.label(
                        RichText::new("or drop one here")
                            .small()
                            .color(theme::TEXT_DIM),
                    );
                }
            });
            if let Some(Picture::Image(image)) = &own {
                image_controls(app, ui, target, image, is_track);
            } else if matches!(shown, Shown::Image { .. }) {
                ui.label(
                    RichText::new("Showing the cover that came with it")
                        .small()
                        .color(theme::TEXT_WEAK),
                );
            }
        });
    zone.response
        .widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, "Drop an image here"));
    if let Some(e) = app.pictures.editor.as_ref().and_then(|e| e.error.clone()) {
        ui.label(RichText::new(e).small().color(theme::ERROR));
    }

    // ----- symbol -----
    section(ui, "Symbol");
    let color = shown.color();
    let current = match &own {
        Some(Picture::Symbol { symbol, .. }) => Some(*symbol),
        _ => None,
    };
    // A browser picture's own color, kept when its symbol changes; a
    // track's symbol follows the track's color.
    let own_color = match (&own, target) {
        (_, Target::Track(_)) => None,
        (Some(Picture::Symbol { color: Some(c), .. }), _) => Some(*c),
        (_, Target::Browser { default, .. }) => match default {
            Picture::Symbol { color, .. } => *color,
            Picture::Image(_) => None,
        },
    };
    let tile = vec2(36.0, 36.0);
    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
    for row in Symbol::ALL.chunks(8) {
        ui.horizontal(|ui| {
            for symbol in row {
                let tile_shown = Shown::Symbol {
                    symbol: *symbol,
                    color,
                };
                if tile_button(
                    app,
                    ui,
                    tile,
                    &tile_shown,
                    symbol.label(),
                    current == Some(*symbol),
                ) {
                    let p = Picture::Symbol {
                        symbol: *symbol,
                        color: own_color,
                    };
                    app.set_picture(target, Some(p), false);
                }
            }
        });
    }

    // ----- color -----
    section(ui, if is_track { "Track color" } else { "Color" });
    let image_in_browser = !is_track && matches!(own, Some(Picture::Image(_)));
    ui.add_enabled_ui(!image_in_browser, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            let now = color.to_array();
            let now = [now[0], now[1], now[2]];
            for rgb in SWATCHES {
                let (rect, resp) = ui.allocate_exact_size(vec2(19.0, 19.0), Sense::click());
                let hex = format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
                resp.widget_info(|| {
                    WidgetInfo::selected(WidgetType::Button, true, rgb == now, &hex)
                });
                let p = ui.painter();
                p.rect_filled(rect, CornerRadius::same(4), theme::track_color(rgb));
                if rgb == now || resp.hovered() {
                    p.rect_stroke(
                        rect.expand(1.0),
                        CornerRadius::same(5),
                        Stroke::new(1.5, egui::Color32::WHITE),
                        StrokeKind::Outside,
                    );
                }
                if resp.clicked() {
                    set_color(app, target, own.as_ref(), rgb, None);
                }
            }
            let mut rgb = now;
            let r = ui
                .color_edit_button_srgb(&mut rgb)
                .on_hover_text("Custom color");
            if r.changed() && rgb != now {
                let key = app.gestures.key(r.id);
                set_color(app, target, own.as_ref(), rgb, Some(key));
            }
        });
    })
    .response
    .on_disabled_hover_text("Colors are for symbols");

    // ----- recent -----
    let recent: Vec<Picture> = app.pictures.store.recent().to_vec();
    if !recent.is_empty() {
        section(ui, "Recent");
        let c = color.to_array();
        let base = [c[0], c[1], c[2]];
        ui.horizontal(|ui| {
            for (i, p) in recent.iter().enumerate() {
                let tile_shown = Shown::of(p, base, Symbol::Folder);
                let label = format!("Recent picture {}", i + 1);
                if tile_button(
                    app,
                    ui,
                    vec2(30.0, 30.0),
                    &tile_shown,
                    &label,
                    own.as_ref() == Some(p),
                ) {
                    app.set_picture(target, Some(p.clone()), false);
                }
            }
        });
    }

    ui.add_space(8.0);
    ui.separator();
    ui.horizontal(|ui| {
        if ui
            .add_enabled(own.is_some(), egui::Button::new("Reset to default"))
            .on_hover_text("Show the picture it had at first")
            .clicked()
        {
            app.set_picture(target, None, false);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Done").clicked() {
                *done = true;
            }
        });
    });
}

/// Sets the color a target's picture shows: a track's own color, or a
/// browser symbol's.
fn set_color(
    app: &mut App,
    target: &Target,
    own: Option<&Picture>,
    rgb: [u8; 3],
    key: Option<u64>,
) {
    match target {
        Target::Track(id) => {
            let r = actions::set_track_color(&app.session, *id, rgb, key);
            if app.report(r).is_some() {
                app.refresh();
            }
        }
        Target::Browser { default, .. } => {
            let symbol = match (own, default) {
                (Some(Picture::Symbol { symbol, .. }), _) | (_, Picture::Symbol { symbol, .. }) => {
                    *symbol
                }
                _ => Symbol::Folder,
            };
            let p = Picture::Symbol {
                symbol,
                color: Some(rgb),
            };
            app.set_picture(target, Some(p), key.is_some());
        }
    }
}

/// Fill or Fit, zoom and, for a track, tint.
fn image_controls(
    app: &mut App,
    ui: &mut Ui,
    target: &Target,
    image: &PictureImage,
    is_track: bool,
) {
    ui.horizontal(|ui| {
        let mut name = egui::text::LayoutJob::simple_singleline(
            image.name.clone(),
            theme::small(),
            theme::TEXT,
        );
        name.wrap = egui::text::TextWrapping::truncate_at_width(ui.available_width() - 80.0);
        ui.label(name);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.small_button("Remove").clicked() {
                app.set_picture(target, None, false);
            }
        });
    });
    let mut changed = image.clone();
    let mut gesture = false;
    ui.horizontal(|ui| {
        ui.selectable_value(&mut changed.fit, PictureFit::Fill, "Fill")
            .on_hover_text("Cover the frame, cropping what does not fit");
        ui.selectable_value(&mut changed.fit, PictureFit::Fit, "Fit")
            .on_hover_text("Show all of it");
        ui.add_space(8.0);
        ui.label(RichText::new("Zoom").small().color(theme::TEXT_WEAK));
        let r = ui.add(
            egui::Slider::new(&mut changed.zoom, 1.0..=MAX_ZOOM)
                .logarithmic(true)
                .fixed_decimals(1)
                .suffix("×"),
        );
        gesture |= r.changed();
    });
    if is_track {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Tint").small().color(theme::TEXT_WEAK))
                .on_hover_text("How much of the track's color is laid over it");
            let mut percent = changed.tint * 100.0;
            let r = ui.add(
                egui::Slider::new(&mut percent, 0.0..=100.0)
                    .fixed_decimals(0)
                    .suffix(" %"),
            );
            if r.changed() {
                changed.tint = (percent / 100.0).clamp(0.0, 1.0);
                gesture = true;
            }
        });
    }
    if changed != *image {
        if changed.fit != image.fit {
            // A new way to frame it starts from the middle.
            changed.offset = [0.0, 0.0];
        }
        app.set_picture(target, Some(Picture::Image(changed)), gesture);
    }
}

/// The picture as a tile, in a browser row and as a banner. Dragging one
/// that shows an image moves what the frame shows.
fn previews(
    app: &mut App,
    ui: &mut Ui,
    target: &Target,
    shown: &Shown,
    own: Option<&Picture>,
    name: &str,
) {
    let (area, _) = ui.allocate_exact_size(vec2(WIDTH, 86.0), Sense::hover());
    let tile = Rect::from_min_size(area.min, vec2(86.0, 86.0));
    let right = area.left() + 98.0;
    let row = Rect::from_min_max(
        pos2(right, area.top()),
        pos2(area.right(), area.top() + 34.0),
    );
    let banner = Rect::from_min_max(pos2(right, area.top() + 42.0), area.max);
    let painter = ui.painter();
    pictures::paint(
        painter,
        tile,
        12.0,
        shown,
        &mut app.pictures.images,
        Style::Tile,
    );

    // As a browser row: a small picture and the name.
    painter.rect_filled(
        row,
        CornerRadius::same(4),
        theme::lighten(theme::PANEL, 0.04),
    );
    let small = Rect::from_min_size(row.min + vec2(4.0, 3.0), vec2(28.0, 28.0));
    pictures::paint(
        painter,
        small,
        5.0,
        shown,
        &mut app.pictures.images,
        Style::Tile,
    );
    let mut job =
        egui::text::LayoutJob::simple_singleline(name.to_string(), theme::body(), theme::TEXT);
    job.wrap = egui::text::TextWrapping::truncate_at_width(row.right() - small.right() - 14.0);
    let g = painter.layout_job(job);
    painter.galley(
        pos2(small.right() + 8.0, row.center().y - g.size().y / 2.0),
        g,
        theme::TEXT,
    );

    // As a banner, with the name over it.
    pictures::paint(
        painter,
        banner,
        6.0,
        shown,
        &mut app.pictures.images,
        Style::Banner,
    );
    let mut job = egui::text::LayoutJob::simple_singleline(
        name.to_string(),
        egui::FontId::proportional(15.0),
        egui::Color32::WHITE,
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(banner.width() * 0.6);
    let g = painter.layout_job(job);
    painter.galley(
        pos2(banner.left() + 10.0, banner.center().y - g.size().y / 2.0),
        g,
        egui::Color32::WHITE,
    );

    let Some(Picture::Image(image)) = own else {
        return;
    };
    let size = match app.pictures.images.texture(ui.ctx(), &image.data) {
        Some((_, size)) => size,
        None => return,
    };
    for (i, frame) in [tile, small, banner].into_iter().enumerate() {
        let resp = ui
            .interact(frame, Id::new(("picture_preview", i)), Sense::drag())
            .on_hover_text("Drag to move the image in its frame");
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        let delta = resp.drag_delta();
        if delta == Vec2::ZERO {
            continue;
        }
        let mut moved = image.clone();
        for axis in 0..2 {
            let (d, w) = (delta[axis], frame.size()[axis]);
            let step = match image.fit {
                PictureFit::Fill => {
                    let uv = fill_uv(frame.size(), size, image.zoom, image.offset);
                    let spare = 1.0 - uv.size()[axis];
                    if spare < 1e-3 {
                        0.0
                    } else {
                        -(d / w) * uv.size()[axis] * 2.0 / spare
                    }
                }
                PictureFit::Fit => {
                    let dest = fit_rect(frame, size, image.zoom, image.offset);
                    let spare = dest.size()[axis] - w;
                    if spare < 0.5 { 0.0 } else { -2.0 * d / spare }
                }
            };
            moved.offset[axis] = (moved.offset[axis] + step).clamp(-1.0, 1.0);
        }
        if moved != *image {
            app.set_picture(target, Some(Picture::Image(moved)), true);
        }
    }
}
