// SPDX-License-Identifier: GPL-3.0-or-later
//! Pictures for tracks, sample packs, banks and folders. A picture is a
//! symbol on a tile shaded from a color, or an image someone picked.
//!
//! A track keeps its picture in the project. Pictures given to packs,
//! banks and folders in the browser are kept on this computer, in one file
//! beside the browser's folders, so every project sees them. A pack or
//! folder can also bring its own cover image; it shows until someone picks
//! another picture, and is never bundled with the app.

use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use egui::epaint::{Mesh, RectShape, StrokeKind};
use egui::{
    Color32, ColorImage, Context, CornerRadius, Id, Painter, Pos2, Rect, Response, Sense, Stroke,
    TextureHandle, TextureId, TextureOptions, Ui, Vec2, pos2, vec2,
};
use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use serde::{Deserialize, Serialize};
use sky_core::TrackId;
use sky_dsp::InstrumentType;
use sky_library::{PackInfo, packs};
use sky_model::{Picture, PictureFit, PictureImage, Project, Symbol, Track, TrackKind};

use crate::browser::{bank_key, pack_key};
use crate::theme;

/// Image files a picture can be made from.
pub const IMAGE_EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "webp", "bmp", "gif"];
/// The longest side of an image a picture keeps; larger ones are shrunk.
const KEEP_SIDE: u32 = 512;
/// Images larger than this on a side are not read at all.
const MAX_SIDE: u32 = 12_000;
/// Image files larger than this are not read.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Pictures picked lately, offered again in the picture editor.
const RECENT: usize = 7;
/// Frames an unused texture is kept before it is let go.
const TEXTURE_FRAMES: u64 = 600;

pub fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTENSIONS.iter().any(|a| a.eq_ignore_ascii_case(e)))
}

/// What a picture shows once worked out.
#[derive(Clone, Debug, PartialEq)]
pub enum Shown {
    Symbol {
        symbol: Symbol,
        color: Color32,
    },
    Image {
        image: PictureImage,
        /// Around the image when it fits inside, and its tint.
        color: Color32,
        /// Drawn instead when the image cannot be read.
        fallback: Symbol,
    },
}

impl Shown {
    /// `color` is used when the picture names none, as a track's does.
    pub fn of(picture: &Picture, color: [u8; 3], fallback: Symbol) -> Shown {
        match picture {
            Picture::Symbol { symbol, color: own } => Shown::Symbol {
                symbol: *symbol,
                color: theme::track_color(own.unwrap_or(color)),
            },
            Picture::Image(image) => Shown::Image {
                image: image.clone(),
                color: theme::track_color(color),
                fallback,
            },
        }
    }

    pub fn color(&self) -> Color32 {
        match self {
            Shown::Symbol { color, .. } | Shown::Image { color, .. } => *color,
        }
    }
}

/// How a picture is laid out in its frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// The symbol in the middle.
    Tile,
    /// A wide strip: the symbol on the right, its echo across.
    Banner,
}

/// A picture in the browser or a track: what it is for and what its own
/// picture is when nobody picked one.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// A pack, bank or folder in the browser, by its browser key.
    Browser {
        key: String,
        name: String,
        default: Picture,
    },
    Track(TrackId),
}

// ----- drawing -----

/// A rounded rectangle's outline, clockwise from the top left.
fn rounded_outline(rect: Rect, radius: f32) -> Vec<Pos2> {
    let r = radius
        .min(rect.width() / 2.0)
        .min(rect.height() / 2.0)
        .max(0.0);
    let steps = if r < 1.0 { 0 } else { 5 };
    let corners = [
        (pos2(rect.left() + r, rect.top() + r), 180.0f32),
        (pos2(rect.right() - r, rect.top() + r), 270.0),
        (pos2(rect.right() - r, rect.bottom() - r), 0.0),
        (pos2(rect.left() + r, rect.bottom() - r), 90.0),
    ];
    let mut points = Vec::with_capacity(4 * (steps + 1));
    for (center, start) in corners {
        for s in 0..=steps {
            let a = (start + 90.0 * s as f32 / steps.max(1) as f32).to_radians();
            points.push(center + vec2(a.cos(), a.sin()) * r);
        }
    }
    points
}

/// Fills a rounded rectangle shaded from `light` at the top left to `dark`
/// at the bottom right.
fn shade(painter: &Painter, rect: Rect, radius: f32, light: Color32, dark: Color32) {
    let outline = rounded_outline(rect, radius);
    let color_at = |p: Pos2| {
        let t = ((p.x - rect.left()) / rect.width().max(1.0)) * 0.45
            + ((p.y - rect.top()) / rect.height().max(1.0)) * 0.55;
        light.lerp_to_gamma(dark, t.clamp(0.0, 1.0))
    };
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.center(), color_at(rect.center()));
    for p in &outline {
        mesh.colored_vertex(*p, color_at(*p));
    }
    let n = outline.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    painter.add(mesh);
}

/// The two ends of a tile's shading for a color.
pub fn tile_colors(color: Color32) -> (Color32, Color32) {
    (theme::lighten(color, 0.12), theme::darken(color, 0.55))
}

fn edge(painter: &Painter, rect: Rect, radius: f32) {
    painter.add(RectShape::stroke(
        rect,
        CornerRadius::same(radius.round() as u8),
        Stroke::new(1.0, Color32::from_white_alpha(22)),
        StrokeKind::Inside,
    ));
}

/// Draws a symbol with a soft shadow under it.
fn symbol_with_shadow(painter: &Painter, square: Rect, symbol: Symbol) {
    let drop = (square.width() / 64.0 * 1.2).max(0.8);
    crate::glyphs::paint(
        painter,
        square.translate(vec2(0.0, drop)),
        symbol,
        Color32::from_black_alpha(70),
    );
    crate::glyphs::paint(painter, square, symbol, Color32::WHITE);
}

fn paint_symbol(
    painter: &Painter,
    rect: Rect,
    radius: f32,
    symbol: Symbol,
    color: Color32,
    style: Style,
) {
    let (light, dark) = tile_colors(color);
    let side = rect.width().min(rect.height());
    match style {
        Style::Tile => {
            shade(painter, rect, radius, light, dark);
            if side >= 56.0 {
                // Faint echoes of the symbol in two corners.
                let echo = rect.height() * 0.55;
                let clip = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
                let faint = Color32::from_white_alpha(24);
                for square in [
                    Rect::from_min_size(
                        rect.min - vec2(echo * 0.15, echo * 0.25),
                        Vec2::splat(echo),
                    ),
                    Rect::from_min_size(
                        rect.max - vec2(echo * 0.75, echo * 0.6),
                        Vec2::splat(echo),
                    ),
                ] {
                    crate::glyphs::paint(&clip, square, symbol, faint);
                }
            }
            let k = if side <= 26.0 { 0.84 } else { 0.74 };
            let square = Rect::from_center_size(rect.center(), Vec2::splat(side * k));
            symbol_with_shadow(painter, square, symbol);
        }
        Style::Banner => {
            // Shaded from the left so text over it stays readable.
            shade(painter, rect, radius, dark, light);
            let clip = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
            let faint = Color32::from_white_alpha(26);
            let echo = rect.height() * 0.7;
            let mut x = rect.right() - echo * 2.6;
            let mut up = true;
            while x > rect.center().x - echo {
                let y = if up { -0.2 } else { 0.45 };
                crate::glyphs::paint(
                    &clip,
                    Rect::from_min_size(pos2(x, rect.top() + echo * y), Vec2::splat(echo)),
                    symbol,
                    faint,
                );
                x -= echo * 0.95;
                up = !up;
            }
            let square = Rect::from_min_size(
                pos2(
                    rect.right() - rect.height() * 1.05,
                    rect.top() + rect.height() * 0.12,
                ),
                Vec2::splat(rect.height() * 0.76),
            );
            symbol_with_shadow(painter, square, symbol);
        }
    }
}

/// The part of an image a frame shows when the image covers it.
pub fn fill_uv(frame: Vec2, image: Vec2, zoom: f32, offset: [f32; 2]) -> Rect {
    let fa = frame.x / frame.y.max(1.0);
    let ia = image.x / image.y.max(1.0);
    let (w, h) = if ia > fa {
        (fa / ia, 1.0)
    } else {
        (1.0, ia / fa)
    };
    let size = vec2(w, h) / zoom.max(1.0);
    let center = pos2(
        0.5 + offset[0] * (1.0 - size.x) / 2.0,
        0.5 + offset[1] * (1.0 - size.y) / 2.0,
    );
    Rect::from_center_size(center, size)
}

/// Where an image goes when it fits inside a frame.
pub fn fit_rect(frame: Rect, image: Vec2, zoom: f32, offset: [f32; 2]) -> Rect {
    let scale = (frame.width() / image.x.max(1.0)).min(frame.height() / image.y.max(1.0));
    let size = image * scale * zoom.max(1.0);
    let spare = (size - frame.size()).max(Vec2::ZERO);
    Rect::from_center_size(
        frame.center() - vec2(offset[0] * spare.x, offset[1] * spare.y) / 2.0,
        size,
    )
}

/// Draws a picture in `rect`.
pub fn paint(
    painter: &Painter,
    rect: Rect,
    radius: f32,
    shown: &Shown,
    images: &mut ImageCache,
    style: Style,
) {
    match shown {
        Shown::Symbol { symbol, color } => {
            paint_symbol(painter, rect, radius, *symbol, *color, style);
        }
        Shown::Image {
            image,
            color,
            fallback,
        } => {
            let Some((texture, size)) = images.texture(painter.ctx(), &image.data) else {
                paint_symbol(painter, rect, radius, *fallback, *color, style);
                edge(painter, rect, radius);
                return;
            };
            let cr = CornerRadius::same(radius.round() as u8);
            match image.fit {
                PictureFit::Fill => {
                    let uv = fill_uv(rect.size(), size, image.zoom, image.offset);
                    painter
                        .add(RectShape::filled(rect, cr, Color32::WHITE).with_texture(texture, uv));
                }
                PictureFit::Fit => {
                    let (light, dark) = tile_colors(*color);
                    shade(painter, rect, radius, light, dark);
                    let dest = fit_rect(rect, size, image.zoom, image.offset);
                    painter
                        .with_clip_rect(rect.intersect(painter.clip_rect()))
                        .image(
                            texture,
                            dest,
                            Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                }
            }
            if image.tint > 0.0 {
                let [r, g, b, _] = color.to_array();
                let alpha = (image.tint.clamp(0.0, 1.0) * 170.0) as u8;
                painter.rect_filled(rect, cr, Color32::from_rgba_unmultiplied(r, g, b, alpha));
            }
            if style == Style::Banner {
                // Darker on the left, for the name written over it.
                let mut mesh = Mesh::default();
                let dark = Color32::from_black_alpha(170);
                let clear = Color32::TRANSPARENT;
                let mid = pos2(rect.left() + rect.width() * 0.7, rect.bottom());
                mesh.colored_vertex(rect.left_top(), dark);
                mesh.colored_vertex(pos2(mid.x, rect.top()), clear);
                mesh.colored_vertex(mid, clear);
                mesh.colored_vertex(rect.left_bottom(), dark);
                mesh.add_triangle(0, 1, 2);
                mesh.add_triangle(0, 2, 3);
                painter.add(mesh);
            }
        }
    }
    edge(painter, rect, radius);
}

/// The little pencil that says a picture can be changed.
pub fn paint_pencil(painter: &Painter, center: Pos2, radius: f32) {
    painter.circle(
        center,
        radius,
        Color32::from_rgba_unmultiplied(14, 16, 20, 235),
        Stroke::new(1.0, Color32::from_rgb(102, 112, 127)),
    );
    let s = radius * 1.2 / 8.0;
    let at = |x: f32, y: f32| center + vec2(x - 8.0, y - 8.0) * s;
    let stroke = Stroke::new((1.4 * s).max(1.0), Color32::WHITE);
    painter.add(egui::Shape::closed_line(
        vec![
            at(3.0, 13.0),
            at(3.6, 10.4),
            at(10.6, 3.4),
            at(12.6, 5.4),
            at(5.6, 12.4),
        ],
        stroke,
    ));
    painter.line_segment([at(9.4, 4.6), at(11.4, 6.6)], stroke);
}

/// The widget id of a picture shown in `place`, such as `"header"`; one
/// track's picture shows in its header and its mixer strip at once.
pub fn button_id(place: &str, target: &Target) -> Id {
    Id::new(("picture", place, picture_id(target)))
}

/// A picture that opens the picture editor when clicked. It shows a pencil
/// while it or its row (`row_hovered`) is under the pointer, and lights up
/// while an image file is held over the window above it.
#[allow(clippy::too_many_arguments)]
pub fn button(
    app: &mut crate::app::App,
    ui: &Ui,
    place: &str,
    rect: Rect,
    radius: f32,
    shown: &Shown,
    target: Target,
    style: Style,
    row_hovered: bool,
) -> Response {
    let resp = ui.interact(rect, button_id(place, &target), Sense::click());
    let name = app.target_name(&target);
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Picture of {name}"))
    });
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let painter = ui.painter();
    paint(
        painter,
        rect,
        radius,
        shown,
        &mut app.pictures.images,
        style,
    );
    let files = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
    let over = resp.hovered() || (files && ui.rect_contains_pointer(rect));
    if over {
        app.pictures.hovered = Some(target.clone());
    }
    let waiting = app.pictures.waiting.is_some();
    if over || waiting {
        // While a dropped image waits for a picture, every picture pulses.
        let alpha = if waiting && !over {
            let t = ui.input(|i| i.time) as f32;
            ui.ctx().request_repaint();
            0.45 + 0.35 * (t * 4.0).sin()
        } else {
            1.0
        };
        painter.rect_stroke(
            rect.expand(1.5),
            CornerRadius::same(radius.round() as u8 + 2),
            Stroke::new(2.0, theme::ACCENT.gamma_multiply(alpha)),
            StrokeKind::Outside,
        );
    }
    if over || row_hovered {
        let r = (rect.height() * 0.24).clamp(7.0, 13.0);
        paint_pencil(painter, rect.right_bottom() - vec2(r * 0.55, r * 0.55), r);
    }
    let tip = if waiting {
        format!("Give {name} the dropped image")
    } else if let Target::Track(id) = &target
        && let Ok(track) = app.view.project.track(*id)
    {
        format!(
            "{}\nClick to change its picture and color",
            crate::tracks::kind_label(track)
        )
    } else {
        format!("Change the picture of {name}")
    };
    if resp.clicked() {
        let ctx = ui.ctx().clone();
        if let Some(path) = app.pictures.waiting.take() {
            app.load_picture_file(&ctx, target, path);
        } else {
            // Beside the picture, past the panel it is in, so the editor
            // does not cover what it changes.
            let x = ui.clip_rect().right().min(rect.right() + 240.0);
            app.open_picture_editor(target, pos2(x, rect.top()));
        }
    }
    resp.on_hover_text(tip)
}

/// A stable name for what a picture is for.
pub fn picture_id(target: &Target) -> String {
    match target {
        Target::Browser { key, .. } => key.clone(),
        Target::Track(id) => format!("track:{id}"),
    }
}

// ----- images -----

fn limits(side: u32) -> Limits {
    let mut l = Limits::default();
    l.max_image_width = Some(side);
    l.max_image_height = Some(side);
    l.max_alloc = Some(512 * 1024 * 1024);
    l
}

fn decode(bytes: &[u8], side: u32) -> Result<DynamicImage, String> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    if reader.format().is_none() {
        return Err("it is not a PNG, JPEG, WebP, BMP or GIF image".into());
    }
    reader.limits(limits(side));
    reader.decode().map_err(|e| e.to_string())
}

/// Shrinks an image to what a picture keeps and stores it as a JPEG, or
/// as a PNG when it has see-through parts.
fn keep(image: DynamicImage, name: &str) -> Result<PictureImage, String> {
    let image = if image.width() > KEEP_SIDE || image.height() > KEEP_SIDE {
        image.resize(
            KEEP_SIDE,
            KEEP_SIDE,
            image::imageops::FilterType::CatmullRom,
        )
    } else {
        image
    };
    let rgba = image.to_rgba8();
    let opaque = rgba.pixels().all(|p| p.0[3] == 255);
    let mut out = Vec::new();
    if opaque {
        let rgb = DynamicImage::ImageRgba8(rgba).to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88)
            .encode_image(&rgb)
            .map_err(|e| e.to_string())?;
    } else {
        DynamicImage::ImageRgba8(rgba)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .map_err(|e| e.to_string())?;
    }
    Ok(PictureImage::new(out, name))
}

/// A picture's image from the bytes of an image file.
pub fn image_from_bytes(bytes: &[u8], name: &str) -> Result<PictureImage, String> {
    keep(decode(bytes, MAX_SIDE)?, name)
}

/// A picture's image from an image file.
pub fn image_from_file(path: &Path) -> Result<PictureImage, String> {
    let name = path
        .file_name()
        .map_or_else(|| "image".to_string(), |n| n.to_string_lossy().into_owned());
    let size = std::fs::metadata(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?
        .len();
    if size > MAX_FILE_BYTES {
        return Err(format!(
            "{name} is {} MB; pictures can be made from images up to {} MB",
            size / (1024 * 1024),
            MAX_FILE_BYTES / (1024 * 1024)
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    image_from_bytes(&bytes, &name).map_err(|e| format!("cannot use {name}: {e}"))
}

/// A picture's image from raw RGBA pixels, such as the clipboard's.
pub fn image_from_rgba(
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    name: &str,
) -> Result<PictureImage, String> {
    let buffer = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| "the image's size does not match its pixels".to_string())?;
    keep(DynamicImage::ImageRgba8(buffer), name)
}

/// An image copied in another app: an image file among copied files, or
/// the image itself.
pub fn clipboard_image() -> Result<PictureImage, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("cannot read the clipboard: {e}"))?;
    if let Ok(files) = clipboard.get().file_list()
        && let Some(path) = files.iter().find(|p| is_image(p))
    {
        return image_from_file(path);
    }
    match clipboard.get_image() {
        Ok(image) => image_from_rgba(
            image.width as u32,
            image.height as u32,
            image.bytes.into_owned(),
            "pasted image",
        ),
        Err(_) => {
            Err("There is no image on the clipboard: copy an image or an image file first".into())
        }
    }
}

struct Entry {
    data: Weak<[u8]>,
    texture: Option<TextureHandle>,
    size: Vec2,
    used: u64,
}

/// Images turned into textures, by the bytes they came from.
#[derive(Default)]
pub struct ImageCache {
    entries: HashMap<usize, Entry>,
    frame: u64,
}

impl ImageCache {
    /// The texture for an image's bytes and its size in pixels, or `None`
    /// when they are not an image that can be read.
    pub fn texture(&mut self, ctx: &Context, data: &Arc<[u8]>) -> Option<(TextureId, Vec2)> {
        let key = Arc::as_ptr(data).cast::<u8>() as usize;
        let fresh = self
            .entries
            .get(&key)
            .is_some_and(|e| e.data.upgrade().is_some_and(|d| Arc::ptr_eq(&d, data)));
        if !fresh {
            let entry = match decode(data, 4096) {
                Ok(image) => {
                    let image = if image.width() > 1024 || image.height() > 1024 {
                        image.thumbnail(1024, 1024)
                    } else {
                        image
                    };
                    let rgba = image.to_rgba8();
                    let size = [rgba.width() as usize, rgba.height() as usize];
                    let color = ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                    let options = TextureOptions {
                        mipmap_mode: Some(egui::TextureFilter::Linear),
                        ..TextureOptions::LINEAR
                    };
                    Entry {
                        data: Arc::downgrade(data),
                        texture: Some(ctx.load_texture("picture", color, options)),
                        size: vec2(size[0] as f32, size[1] as f32),
                        used: self.frame,
                    }
                }
                Err(e) => {
                    tracing::warn!("cannot show a picture's image: {e}");
                    Entry {
                        data: Arc::downgrade(data),
                        texture: None,
                        size: Vec2::ZERO,
                        used: self.frame,
                    }
                }
            };
            self.entries.insert(key, entry);
        }
        let entry = self.entries.get_mut(&key)?;
        entry.used = self.frame;
        entry.texture.as_ref().map(|t| (t.id(), entry.size))
    }

    /// Lets go of textures no picture has used for a while.
    pub fn end_frame(&mut self) {
        self.frame += 1;
        if self.frame.is_multiple_of(120) {
            let frame = self.frame;
            self.entries
                .retain(|_, e| e.data.strong_count() > 0 && frame - e.used < TEXTURE_FRAMES);
        }
    }
}

// ----- the pictures things show by themselves -----

/// FNV-1a, so a name's color stays the same across runs.
fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A color picked from a name, the same every time.
pub fn color_for(name: &str) -> [u8; 3] {
    let h = fnv(&name.to_lowercase());
    let hue = (h % 360) as f32 / 360.0;
    let c = egui::ecolor::Hsva::new(hue, 0.58, 0.8, 1.0).to_srgb();
    [c[0], c[1], c[2]]
}

/// The picture a known pack shows until someone picks another.
pub fn pack_default(info: &PackInfo) -> Picture {
    let (symbol, color) = match info.id {
        "vcsl" => (Symbol::Strings, [200, 150, 60]),
        "piano" => (Symbol::Piano, [74, 106, 160]),
        "tidal-drum-machines" => (Symbol::DrumMachine, [232, 100, 58]),
        "dirt-samples" => (Symbol::Noise, [130, 168, 74]),
        "emu-sp12" => (Symbol::Groovebox, [62, 166, 154]),
        "mridangam" => (Symbol::HandDrum, [208, 104, 46]),
        "clean-breaks" => (Symbol::Record, [148, 98, 222]),
        other => (Symbol::SampleBank, color_for(other)),
    };
    Picture::Symbol {
        symbol,
        color: Some(color),
    }
}

/// The picture a bank in a pack shows until someone picks another.
pub fn bank_default(info: &PackInfo, bank: &str) -> Picture {
    let symbol = match pack_default(info) {
        Picture::Symbol { symbol, .. } => symbol,
        Picture::Image(_) => Symbol::SampleBank,
    };
    Picture::Symbol {
        symbol,
        color: Some(color_for(bank)),
    }
}

/// The picture a folder in the browser shows until someone picks another:
/// a symbol for what its name suggests it holds.
pub fn folder_default(path: &Path) -> Picture {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let lower = name.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    let symbol = if has(&["break"]) {
        Symbol::Record
    } else if has(&["kick", "snare", "drum", "perc", "hat", "808", "909"]) {
        Symbol::DrumMachine
    } else if has(&["piano", "keys", "rhodes"]) {
        Symbol::Piano
    } else if has(&["string", "violin", "cello", "orch"]) {
        Symbol::Strings
    } else if has(&["vox", "vocal", "voice", "foley", "field", "dialog"]) {
        Symbol::Microphone
    } else if has(&["synth", "bass", "lead", "pad"]) {
        Symbol::Synth
    } else if has(&["noise", "fx", "sfx", "glitch", "texture"]) {
        Symbol::Noise
    } else {
        Symbol::Folder
    };
    Picture::Symbol {
        symbol,
        color: Some(color_for(&name)),
    }
}

/// The symbol for what a track plays.
pub fn track_symbol(track: &Track) -> Symbol {
    match (&track.kind, track.instrument.as_ref().map(|i| i.kind)) {
        (TrackKind::Audio, _) => Symbol::Audio,
        (_, Some(InstrumentType::Synth)) => Symbol::Synth,
        (_, Some(InstrumentType::Drums)) => Symbol::Drums,
        (_, Some(InstrumentType::Sampler)) => Symbol::Sampler,
        (_, Some(InstrumentType::Bank)) => Symbol::SampleBank,
        (_, Some(InstrumentType::Acid)) => Symbol::AcidBass,
        (_, None) => Symbol::Synth,
    }
}

/// The known pack a sample bank track's sounds came from, and its bank.
pub fn pack_of(project: &Project, track: &Track) -> Option<(&'static PackInfo, String)> {
    let bank = track.instrument.as_ref()?.bank.as_ref()?;
    let pack = bank.sounds.values().flatten().find_map(|s| {
        let source = project.pool_entry(s.audio).ok()?.source.as_ref()?;
        packs::find(&source.pack)
    })?;
    Some((pack, bank.name.clone()))
}

// ----- the pictures kept on this computer -----

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PictureFile {
    /// Pictures by browser key, such as `pack:vcsl`.
    #[serde(default)]
    pictures: BTreeMap<String, Picture>,
    #[serde(default)]
    recent: Vec<Picture>,
}

/// The pictures picked for packs, banks and folders, and the ones picked
/// lately.
#[derive(Default)]
pub struct PictureStore {
    path: Option<PathBuf>,
    file: PictureFile,
    /// The file was there but could not be read, so the first write keeps
    /// a copy of it.
    unreadable: bool,
    dirty: bool,
}

impl PictureStore {
    /// Reads the pictures kept in `path`; `None` keeps them only while the
    /// app runs.
    pub fn load(path: Option<PathBuf>) -> Self {
        let mut store = Self {
            path,
            ..Self::default()
        };
        let Some(path) = &store.path else {
            return store;
        };
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<PictureFile>(&text) {
                Ok(file) => store.file = file,
                Err(e) => {
                    tracing::warn!("cannot read the pictures in {}: {e}", path.display());
                    store.unreadable = true;
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!("cannot read the pictures in {}: {e}", path.display());
                store.unreadable = true;
            }
        }
        store
    }

    pub fn get(&self, key: &str) -> Option<&Picture> {
        self.file.pictures.get(key)
    }

    /// Gives a key its own picture, or takes it away with `None`. Written
    /// to disk by [`flush`](Self::flush).
    pub fn set(&mut self, key: &str, picture: Option<Picture>) {
        let changed = match picture {
            Some(p) => {
                self.file
                    .pictures
                    .insert(key.to_string(), p.clone())
                    .as_ref()
                    != Some(&p)
            }
            None => self.file.pictures.remove(key).is_some(),
        };
        self.dirty |= changed;
    }

    pub fn recent(&self) -> &[Picture] {
        &self.file.recent
    }

    /// Puts a picture first among the recent ones.
    pub fn remember(&mut self, picture: &Picture) {
        self.file.recent.retain(|p| p != picture);
        self.file.recent.insert(0, picture.clone());
        self.file.recent.truncate(RECENT);
        self.dirty = true;
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Writes the pictures out if anything changed.
    pub fn flush(&mut self) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        self.dirty = false;
        let Some(path) = &self.path else {
            return Ok(());
        };
        let failed = |e: std::io::Error| {
            format!(
                "The pictures are kept until the app closes: cannot write {}: {e}",
                path.display()
            )
        };
        if self.unreadable {
            let kept = path.with_extension("json.unreadable");
            std::fs::rename(path, &kept).map_err(failed)?;
            tracing::warn!("kept the unreadable pictures file as {}", kept.display());
            self.unreadable = false;
        }
        let text = serde_json::to_string(&self.file).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(failed)?;
        }
        // Write beside it and swap, so a crash never leaves half a file.
        let next = path.with_extension("json.new");
        std::fs::write(&next, text).map_err(failed)?;
        std::fs::rename(&next, path).map_err(failed)
    }
}

/// A cover image being looked for, or found.
pub enum Cover {
    Looking,
    Found(PictureImage),
    None,
}

/// Pictures in the app: those kept on this computer, their textures, the
/// editor, and covers that packs and folders bring.
#[derive(Default)]
pub struct PictureState {
    pub store: PictureStore,
    pub images: ImageCache,
    pub editor: Option<crate::picture_editor::Editor>,
    /// A picture copied with Copy picture, for Paste picture.
    pub copied: Option<Picture>,
    /// Covers found beside packs and in folders, by browser key.
    pub covers: HashMap<String, Cover>,
    /// The picture under the pointer this frame.
    pub hovered: Option<Target>,
    /// The picture that was under the pointer last frame, for an image
    /// dropped on the window.
    pub last_hovered: Option<Target>,
    /// An image file dropped where no picture was known to be; the next
    /// picture clicked takes it.
    pub waiting: Option<PathBuf>,
    /// The picture a file chooser was opened for.
    pub choosing: Option<Target>,
    /// A track's picture while a drag changes it, such as its zoom. It
    /// goes into the project, as one undo step, when the drag ends, so a
    /// drag does not write the image to the crash journal every frame.
    pub preview: Option<(TrackId, Option<Picture>)>,
}

impl PictureState {
    pub fn new(store: PictureStore) -> Self {
        Self {
            store,
            ..Self::default()
        }
    }

    pub fn begin_frame(&mut self) {
        self.last_hovered = self.hovered.take();
    }

    /// A browser key's picture and whether someone picked it: its own, the
    /// cover it brought, or `default`.
    pub fn browser_picture(&self, key: &str, default: &Picture) -> (Picture, bool) {
        if let Some(p) = self.store.get(key) {
            return (p.clone(), true);
        }
        if let Some(Cover::Found(image)) = self.covers.get(key) {
            return (Picture::Image(image.clone()), false);
        }
        (default.clone(), false)
    }

    pub fn browser_shown(&self, key: &str, default: &Picture) -> Shown {
        let color = match default {
            Picture::Symbol { color, .. } => color.unwrap_or([120, 128, 140]),
            Picture::Image(_) => [120, 128, 140],
        };
        let fallback = match default {
            Picture::Symbol { symbol, .. } => *symbol,
            Picture::Image(_) => Symbol::Folder,
        };
        Shown::of(&self.browser_picture(key, default).0, color, fallback)
    }

    /// What a track's picture shows: its own; for a sample bank, the
    /// picture of its bank or pack; else a symbol for what it plays.
    pub fn track_shown(&self, project: &Project, track: &Track) -> Shown {
        let symbol = track_symbol(track);
        let own = match &self.preview {
            Some((id, p)) if *id == track.id => p.as_ref(),
            _ => track.picture.as_ref(),
        };
        if let Some(p) = own {
            return Shown::of(p, track.color, symbol);
        }
        if let Some((info, bank)) = pack_of(project, track) {
            let key = bank_key(info, &bank);
            if self.store.get(&key).is_some() {
                return self.browser_shown(&key, &bank_default(info, &bank));
            }
            return self.browser_shown(&pack_key(info), &pack_default(info));
        }
        Shown::Symbol {
            symbol,
            color: theme::track_color(track.color),
        }
    }

    /// The master strip's picture.
    pub fn master_shown() -> Shown {
        Shown::Symbol {
            symbol: Symbol::Speaker,
            color: theme::ACCENT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crops_keep_the_frame_shape() {
        // A wide image in a square frame shows its middle.
        let uv = fill_uv(vec2(100.0, 100.0), vec2(200.0, 100.0), 1.0, [0.0, 0.0]);
        assert!((uv.width() - 0.5).abs() < 1e-5 && (uv.height() - 1.0).abs() < 1e-5);
        assert!((uv.center().x - 0.5).abs() < 1e-5);
        // Offsets reach the edges and zoom halves what shows.
        let uv = fill_uv(vec2(100.0, 100.0), vec2(200.0, 100.0), 2.0, [1.0, -1.0]);
        assert!(
            (uv.max.x - 1.0).abs() < 1e-5 && uv.min.y.abs() < 1e-5,
            "{uv:?}"
        );
        let fit = fit_rect(
            Rect::from_min_size(Pos2::ZERO, vec2(100.0, 50.0)),
            vec2(10.0, 10.0),
            1.0,
            [0.0, 0.0],
        );
        assert_eq!(fit.size(), vec2(50.0, 50.0));
        assert_eq!(fit.center(), pos2(50.0, 25.0));
    }

    #[test]
    fn images_are_shrunk_and_kept_as_jpeg_or_png() {
        let mut big = image::RgbaImage::new(1600, 800);
        for (x, _, p) in big.enumerate_pixels_mut() {
            *p = image::Rgba([(x % 256) as u8, 40, 90, 255]);
        }
        let mut png = Vec::new();
        DynamicImage::ImageRgba8(big)
            .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .unwrap();
        let kept = image_from_bytes(&png, "wide.png").unwrap();
        assert_eq!(
            &kept.data[..3],
            &[0xff, 0xd8, 0xff],
            "opaque images become JPEG"
        );
        let back = image::load_from_memory(&kept.data).unwrap();
        assert_eq!((back.width(), back.height()), (512, 256));

        let clear = image_from_rgba(4, 4, vec![10; 64], "clip").unwrap();
        assert_eq!(&clear.data[1..4], b"PNG", "see-through images stay PNG");
        assert!(image_from_bytes(b"not an image", "x.png").is_err());
        assert!(image_from_rgba(4, 4, vec![0; 3], "short").is_err());
    }

    #[test]
    fn defaults_follow_names() {
        for info in sky_library::PACKS {
            assert!(matches!(
                pack_default(info),
                Picture::Symbol { color: Some(_), .. }
            ));
        }
        let kick = folder_default(Path::new("/samples/Kicks and Snares"));
        assert!(matches!(
            kick,
            Picture::Symbol {
                symbol: Symbol::DrumMachine,
                ..
            }
        ));
        let foley = folder_default(Path::new("C:\\Audio\\Foley"));
        assert!(matches!(
            foley,
            Picture::Symbol {
                symbol: Symbol::Microphone,
                ..
            }
        ));
        assert_eq!(color_for("RolandTR808"), color_for("rolandtr808"));
        assert_ne!(color_for("RolandTR808"), color_for("RolandTR909"));
    }

    #[test]
    fn the_store_keeps_pictures_between_runs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pictures.json");
        let mut store = PictureStore::load(Some(path.clone()));
        assert!(store.get("pack:vcsl").is_none());
        let pic = Picture::Symbol {
            symbol: Symbol::Piano,
            color: Some([1, 2, 3]),
        };
        store.set("pack:vcsl", Some(pic.clone()));
        store.remember(&pic);
        store.remember(&pic);
        assert!(store.is_dirty());
        store.flush().unwrap();
        assert!(!store.is_dirty());
        let again = PictureStore::load(Some(path.clone()));
        assert_eq!(again.get("pack:vcsl"), Some(&pic));
        assert_eq!(again.recent(), [pic]);

        // A damaged file is kept aside, not lost, on the next write.
        std::fs::write(&path, "{ nope").unwrap();
        let mut damaged = PictureStore::load(Some(path.clone()));
        assert!(damaged.get("pack:vcsl").is_none());
        damaged.set("pack:piano", Some(Picture::symbol(Symbol::Synth)));
        damaged.flush().unwrap();
        assert!(path.with_extension("json.unreadable").exists());
        assert!(PictureStore::load(Some(path)).get("pack:piano").is_some());
    }
}
