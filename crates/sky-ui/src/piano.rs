// SPDX-License-Identifier: GPL-3.0-or-later
//! The piano section of the Instrument tab: which recorded piano a track
//! plays and under what license, downloading one that is not here yet, a
//! map of the keys and velocity layers its recordings cover that plays
//! like a keyboard and lights the keys that sound, and the pedal light.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use egui::text::LayoutJob;
use egui::{
    Align2, Color32, ComboBox, CornerRadius, FontId, Id, Pos2, ProgressBar, Rect, Response,
    RichText, Sense, Stroke, StrokeKind, TextFormat, Ui, WidgetInfo, WidgetType, pos2, vec2,
};
use sky_core::TrackId;
use sky_core::music::note_name;
use sky_dsp::curves;
use sky_dsp::instruments::sampled::Trigger;
use sky_dsp::instruments::{PIANO_UNDAMPED_FROM, SampleLibrary};
use sky_dsp::{InstrumentType, SUSTAIN_PEDAL};
use sky_library::{DEFAULT_PIANO, InstrumentInfo, InstrumentSource, find_instrument};
use sky_model::{InstrumentSlot, License, Origin, Track};
use sky_session::{LibrarySet, SessionError};

use crate::app::{App, Tab};
use crate::jobs::JobOutput;
use crate::sounds::{license_color, license_note};
use crate::theme;

/// The lowest and highest keys of the map: a piano's 88, A0 to C8.
const LOWEST: u8 = 21;
const HIGHEST: u8 = 108;
const KEYS: f32 = (HIGHEST - LOWEST + 1) as f32;
/// The heights of the map's velocity layers, its row of release
/// recordings and its keys.
const MAP_H: f32 = 78.0;
const RELEASE_H: f32 = 9.0;
const STRIP_H: f32 = 36.0;
/// How far down a key its black part reaches.
const BLACK_DEPTH: f32 = 0.6;

/// A library being downloaded, for its progress bar.
pub struct Download {
    pub track: TrackId,
    pub name: &'static str,
    pub done: Arc<AtomicU64>,
    pub total: Arc<AtomicU64>,
}

impl Download {
    fn new(track: TrackId, info: &'static InstrumentInfo) -> Self {
        Self {
            track,
            name: info.name,
            done: Arc::new(AtomicU64::new(0)),
            total: Arc::new(AtomicU64::new(download_bytes(info).unwrap_or(0))),
        }
    }

    /// The bytes received and the total.
    pub fn progress(&self) -> (u64, u64) {
        (
            self.done.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
        )
    }
}

/// How big a library's download is, for one that is not built in.
fn download_bytes(info: &InstrumentInfo) -> Option<u64> {
    match info.source {
        InstrumentSource::Download { bytes, .. } => Some(bytes),
        InstrumentSource::Bundled { .. } => None,
    }
}

/// The status bar label while a track's piano is being changed.
fn job_label(name: &str) -> String {
    format!("Getting the {name} piano")
}

pub fn map_id(track: TrackId) -> Id {
    Id::new(("piano_map", track))
}

pub fn keys_id(track: TrackId) -> Id {
    Id::new(("piano_keys", track))
}

pub fn pedal_id(track: TrackId) -> Id {
    Id::new(("piano_pedal", track))
}

/// A license as a small colored tag, with what it means on hover.
pub fn license_badge(ui: &mut Ui, license: License, credit: Option<&str>) -> Response {
    let color = license_color(license);
    let text = if license.needs_credit() {
        format!("{} · credit", license.label())
    } else {
        license.label().to_string()
    };
    let galley = ui
        .painter()
        .layout_no_wrap(text.clone(), theme::small(), color);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(14.0, 4.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(9),
        color.gamma_multiply(0.14),
        Stroke::new(1.0, color.gamma_multiply(0.55)),
        StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &text));
    let mut tip = license_note(license).to_string();
    if let Some(c) = credit {
        tip.push_str("\nCredit: ");
        tip.push_str(c);
    }
    resp.on_hover_text(tip)
}

/// A library in the list: its name, license and whether it is here.
fn choice_text(info: &InstrumentInfo, installed: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.append(
        info.name,
        0.0,
        TextFormat::simple(theme::body(), theme::TEXT),
    );
    job.append(
        &format!("   {}", info.license.label()),
        0.0,
        TextFormat::simple(theme::small(), license_color(info.license)),
    );
    let state = match download_bytes(info) {
        None => "built in".to_string(),
        Some(_) if installed => "downloaded".to_string(),
        Some(bytes) => format!("{:.0} MB download", bytes as f64 / 1e6),
    };
    job.append(
        &format!("   {state}"),
        0.0,
        TextFormat::simple(theme::small(), theme::TEXT_DIM),
    );
    job
}

/// What the loaded library holds, in a line.
fn library_line(lib: &SampleLibrary) -> String {
    let (low, high) = lib.key_range().unwrap_or((0, 0));
    let layers = (low..=high).map(|k| lib.layers(k)).max().unwrap_or(0);
    let mut parts = vec![
        format!(
            "{layers} velocity layer{}",
            if layers == 1 { "" } else { "s" }
        ),
        format!("{} recordings", lib.samples().len()),
        format!("{} to {}", note_name(low), note_name(high)),
        (if lib.is_stereo() { "stereo" } else { "mono" }).to_string(),
        format!("{:.0} MB in memory", lib.bytes() as f64 / 1e6),
    ];
    if lib.has_release_zones() {
        parts.push("release noises".into());
    }
    if lib.has_pedal_noise() {
        parts.push("pedal noises".into());
    }
    parts.join(" · ")
}

/// The library row: a choice of pianos with the license of the one
/// playing, then what it holds or how its download is going.
pub fn library_row(app: &mut App, ui: &mut Ui, track: TrackId, slot: &InstrumentSlot) {
    let current = slot
        .library
        .as_ref()
        .map_or(DEFAULT_PIANO, |l| l.id.as_str());
    let info = find_instrument(current);
    let current_name = info.map_or_else(
        || {
            slot.library
                .as_ref()
                .map_or("?", |l| l.name.as_str())
                .to_string()
        },
        |i| i.name.to_string(),
    );
    let license = slot.library.as_ref().map_or(License::Cc0, |l| l.license);
    let credit = slot.library.as_ref().and_then(|l| l.credit.clone());
    let busy = app
        .jobs
        .busy_labels()
        .any(|l| l.starts_with("Getting the ") && l.ends_with(" piano"));
    let loaded = app.session.track_library(track);
    let mut chosen = None;
    ui.horizontal(|ui| {
        ui.label("Piano");
        ui.add_enabled_ui(!busy, |ui| {
            ComboBox::from_id_salt(("piano_library", track))
                .selected_text(&current_name)
                .width(240.0)
                .show_ui(ui, |ui| {
                    for status in app.session.instrument_libraries() {
                        let info = status.info;
                        if info.kind != InstrumentType::Piano {
                            continue;
                        }
                        let hover = format!(
                            "{}\n{}: {}",
                            info.description,
                            info.license.label(),
                            license_note(info.license)
                        );
                        if ui
                            .selectable_label(
                                info.id == current,
                                choice_text(info, status.installed),
                            )
                            .on_hover_text(hover)
                            .clicked()
                            // The current one again downloads it when this
                            // computer does not have it yet.
                            && (info.id != current || loaded.is_none())
                        {
                            chosen = Some(info);
                        }
                    }
                });
        });
        license_badge(ui, license, credit.as_deref());
        if let Some(info) = info {
            ui.hyperlink_to(RichText::new("Homepage").small(), info.homepage)
                .on_hover_text(info.homepage);
        }
    });
    let download = app.devices.download.as_ref().filter(|d| d.track == track);
    if let Some(d) = download {
        let (done, total) = d.progress();
        let frac = if total > 0 {
            done as f32 / total as f32
        } else {
            0.0
        };
        ui.add(
            ProgressBar::new(frac)
                .desired_width(360.0)
                .desired_height(16.0)
                .text(format!(
                    "Downloading {}: {:.1} of {:.1} MB",
                    d.name,
                    done as f64 / 1e6,
                    total as f64 / 1e6
                )),
        );
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    } else {
        let line = match &loaded {
            _ if busy => "Getting the new piano…".to_string(),
            Some(lib) => library_line(lib),
            None => match app.session.track_library_error(track) {
                Some(_)
                    if find_instrument(current)
                        .is_some_and(|i| !app.session.instruments().is_installed(i)) =>
                {
                    "Not on this computer yet: choose it above to download it".to_string()
                }
                Some(e) => format!("Cannot load: {e}"),
                None => "Loading…".to_string(),
            },
        };
        ui.label(RichText::new(line).small().color(theme::TEXT_DIM));
    }
    if let Some(info) = chosen {
        let download = Download::new(track, info);
        let (done, total) = (download.done.clone(), download.total.clone());
        app.devices.download = (!app.session.instruments().is_installed(info)).then_some(download);
        let session = app.session.clone();
        let id = info.id;
        app.jobs
            .spawn(ui.ctx(), Some(job_label(info.name)), move || {
                let mut progress = |d: u64, t: u64| {
                    done.store(d, Ordering::Relaxed);
                    total.store(t, Ordering::Relaxed);
                };
                JobOutput::PianoLibrary(
                    session
                        .set_piano_library_reporting(track, id, true, Origin::User, &mut progress)
                        .map_err(|e| e.to_string()),
                )
            });
    }
}

/// What a point on the map or the keys plays: the key, the velocity to
/// play it at, and how hard it sounds after the curve (1 to 127), which
/// picks the velocity layer.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Touch {
    key: u8,
    velocity: u8,
    heard: u8,
}

fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

/// The map of a piano's recordings over its 88 keys: each velocity layer
/// of each recording as a box over the keys and velocities it plays, the
/// keys with no dampers shaded, and keys below to play. Pressing the map
/// plays the key at the velocity that reaches the layer under the
/// pointer; lower on a key plays louder. Keys that sound light up, and
/// the pedal light shows the sustain pedal.
pub fn key_map(app: &mut App, ui: &mut Ui, track: &Track, slot: &InstrumentSlot) {
    let lib = app.session.track_library(track.id);
    let (held, pedal) = app.session.held_keys(track.id);
    let curve = slot.param("vel_curve").unwrap_or(0.0);
    let color = theme::track_color(track.color);

    // As wide as the panel shows, even where something wider is in it.
    let shown_width = ui.clip_rect().right() - ui.cursor().left() - 2.0;
    let width = ui.available_width().min(shown_width).max(KEYS * 4.0);
    let (header, _) = ui.allocate_exact_size(vec2(width, 22.0), Sense::hover());
    ui.painter().text(
        pos2(header.left(), header.center().y),
        Align2::LEFT_CENTER,
        "Keys and velocity layers",
        theme::small(),
        theme::TEXT_WEAK,
    );
    let light = Rect::from_min_size(
        pos2(header.right() - 68.0, header.top() + 1.0),
        vec2(68.0, 20.0),
    );
    pedal_light(app, ui, light, track.id, pedal);
    let has_release = lib.as_ref().is_some_and(|l| l.has_release_zones());
    let height = MAP_H + if has_release { RELEASE_H } else { 0.0 } + STRIP_H;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let map = Rect::from_min_size(rect.min, vec2(width, MAP_H));
    let strip = Rect::from_min_max(pos2(rect.left(), rect.bottom() - STRIP_H), rect.max);
    let release = Rect::from_min_max(
        pos2(rect.left(), map.bottom()),
        pos2(rect.right(), strip.top()),
    );
    let col = width / KEYS;
    let x_of = |key: u8| rect.left() + (f32::from(key) - f32::from(LOWEST)) * col;
    let key_at = |x: f32| {
        let i = ((x - rect.left()) / col).floor().clamp(0.0, KEYS - 1.0);
        LOWEST + i as u8
    };
    let y_of = |velocity: f32| map.bottom() - velocity / 128.0 * map.height();
    let on_map = |p: Pos2| {
        let heard = ((map.bottom() - p.y) / map.height() * 128.0).clamp(1.0, 127.0);
        Touch {
            key: key_at(p.x),
            velocity: curves::piano_velocity_for(heard / 127.0, curve).round() as u8,
            heard: heard as u8,
        }
    };
    let on_keys = |p: Pos2| {
        let depth = ((p.y - strip.top()) / strip.height()).clamp(0.0, 1.0);
        let velocity = (40.0 + depth * 87.0).round();
        Touch {
            key: key_at(p.x),
            velocity: velocity as u8,
            heard: (curves::piano_velocity(velocity, curve) * 127.0).round() as u8,
        }
    };

    let map_resp = ui.interact(map, map_id(track.id), Sense::click_and_drag());
    let keys_resp = ui.interact(strip, keys_id(track.id), Sense::click_and_drag());
    let pointer = ui.input(|i| i.pointer.latest_pos());
    let pressed = if map_resp.is_pointer_button_down_on() {
        pointer.map(on_map)
    } else if keys_resp.is_pointer_button_down_on() {
        pointer.map(on_keys)
    } else {
        None
    };
    if let Some(t) = pressed {
        app.hold_key(track.id, Some(t.key), t.velocity);
    }
    let hovered = map_resp
        .hover_pos()
        .map(on_map)
        .or_else(|| keys_resp.hover_pos().map(on_keys));
    let shown = pressed.or(hovered);
    let auditioned = app
        .audition
        .held
        .filter(|(t, _)| *t == track.id)
        .map(|(_, k)| k);
    let lit = |key: u8| held >> key & 1 == 1 || auditioned == Some(key);

    let painter = ui.painter_at(rect);
    painter.rect_filled(map, CornerRadius::same(2), theme::BG);
    for v in [32.0, 64.0, 96.0] {
        painter.hline(map.x_range(), y_of(v), Stroke::new(1.0, theme::GRID_FAINT));
    }
    for c in (24..=HIGHEST).step_by(12) {
        painter.vline(x_of(c), map.y_range(), Stroke::new(1.0, theme::GRID_FAINT));
    }
    match &lib {
        Some(lib) => {
            let layers = layer_starts(lib);
            let zones = lib.zones().iter().filter(|z| z.trigger == Trigger::Attack);
            for z in zones {
                let r = Rect::from_min_max(
                    pos2(x_of(z.lokey), y_of(f32::from(z.hivel) + 1.0)),
                    pos2(x_of(z.hikey.saturating_add(1)), y_of(f32::from(z.lovel))),
                )
                .intersect(map);
                if r.width() <= 0.0 || r.height() <= 0.0 {
                    continue;
                }
                let layer = layers.iter().filter(|lo| **lo <= z.lovel).count();
                let on = shown.is_some_and(|t| {
                    (z.lokey..=z.hikey).contains(&t.key) && (z.lovel..=z.hivel).contains(&t.heard)
                });
                let fill = if on {
                    theme::lighten(color, 0.3)
                } else {
                    theme::darken(color, (0.62 - 0.14 * layer as f32).max(0.15))
                };
                painter.rect(
                    r.shrink(0.5),
                    CornerRadius::same(1),
                    fill,
                    Stroke::new(1.0, theme::darken(color, 0.75)),
                    StrokeKind::Inside,
                );
                if (z.lokey..=z.hikey).contains(&z.root) && r.height() >= 8.0 {
                    painter.circle_filled(
                        pos2(x_of(z.root) + col / 2.0, r.bottom() - 3.5),
                        (col * 0.3).clamp(1.5, 2.5),
                        theme::lighten(color, 0.7),
                    );
                }
            }
            if has_release {
                painter.rect_filled(release, CornerRadius::ZERO, theme::PANEL);
                for z in lib.zones().iter().filter(|z| z.trigger == Trigger::Release) {
                    let r = Rect::from_min_max(
                        pos2(x_of(z.lokey), release.top() + 2.0),
                        pos2(x_of(z.hikey.saturating_add(1)), release.bottom() - 2.0),
                    )
                    .intersect(release);
                    painter.rect_filled(r, CornerRadius::ZERO, color.gamma_multiply(0.45));
                }
            }
        }
        None => {
            let text = match app.session.track_library_error(track.id) {
                Some(_) => "The recordings are not loaded",
                None => "Loading the recordings…",
            };
            painter.text(
                map.center(),
                Align2::CENTER_CENTER,
                text,
                theme::small(),
                theme::TEXT_DIM,
            );
        }
    }
    // Over the zones, so it shows on any library.
    let undamped = Rect::from_min_max(pos2(x_of(PIANO_UNDAMPED_FROM), map.top()), map.max);
    painter.rect_filled(undamped, CornerRadius::ZERO, Color32::from_black_alpha(70));
    painter.vline(
        undamped.left(),
        map.y_range(),
        Stroke::new(1.0, theme::GRID_BEAT),
    );
    painter.text(
        undamped.right_top() + vec2(-4.0, 3.0),
        Align2::RIGHT_TOP,
        "No dampers",
        FontId::proportional(9.0),
        theme::TEXT_WEAK,
    );
    draw_keys(&painter, strip, col, &x_of, lit);
    painter.rect_stroke(
        rect,
        CornerRadius::same(2),
        Stroke::new(1.0, theme::GRID_BEAT),
        StrokeKind::Inside,
    );

    map_resp.on_hover_ui_at_pointer(|ui| {
        let Some(t) = hovered else {
            return;
        };
        ui.label(format!(
            "{} sounding at velocity {} (played at {})",
            note_name(t.key),
            t.heard,
            t.velocity
        ));
        let Some(lib) = &lib else {
            return;
        };
        let zones = lib.zones_at(Trigger::Attack, t.key, t.heard);
        match zones.first().and_then(|i| lib.zones().get(*i as usize)) {
            Some(z) => {
                let layers = layer_starts(lib);
                let layer = layers.iter().filter(|lo| **lo <= z.lovel).count();
                ui.label(format!(
                    "Layer {layer} of {}: velocities {} to {}",
                    layers.len(),
                    z.lovel,
                    z.hivel
                ));
                ui.label(format!(
                    "Recorded at {}, playing {} to {}",
                    note_name(z.root),
                    note_name(z.lokey),
                    note_name(z.hikey)
                ));
                if zones.len() > 1 {
                    ui.label(format!("{} recordings take turns here", zones.len()));
                }
            }
            None => {
                ui.label("No recording here");
            }
        }
        if t.key >= PIANO_UNDAMPED_FROM {
            ui.label("No damper: the string rings on after the key comes up");
        }
        ui.label(
            RichText::new("Press to hear it")
                .small()
                .color(theme::TEXT_WEAK),
        );
    });
    keys_resp.on_hover_ui_at_pointer(|ui| {
        if let Some(t) = hovered {
            ui.label(note_name(t.key));
        }
        ui.label("Press to hear the piano; lower on a key plays louder");
    });
    if held != 0 || pedal {
        // The engine lets go in its own time: look again soon.
        ui.ctx().request_repaint_after(Duration::from_millis(50));
    }
}

/// The lowest velocity of each layer of attack recordings, in order.
fn layer_starts(lib: &SampleLibrary) -> Vec<u8> {
    let mut starts: Vec<u8> = lib
        .zones()
        .iter()
        .filter(|z| z.trigger == Trigger::Attack)
        .map(|z| z.lovel)
        .collect();
    starts.sort_unstable();
    starts.dedup();
    starts
}

/// The keys under the map, one column per key so they line up with it:
/// black keys dark in their upper part, every C named.
fn draw_keys(
    painter: &egui::Painter,
    strip: Rect,
    col: f32,
    x_of: &dyn Fn(u8) -> f32,
    lit: impl Fn(u8) -> bool,
) {
    let split = strip.top() + strip.height() * BLACK_DEPTH;
    painter.rect_filled(strip, CornerRadius::same(2), Color32::from_gray(205));
    for key in LOWEST..=HIGHEST {
        let (x0, x1) = (x_of(key), x_of(key) + col);
        let on = lit(key);
        if is_black(key) {
            let top = Rect::from_min_max(pos2(x0, strip.top()), pos2(x1, split));
            let fill = if on {
                theme::ACCENT
            } else {
                Color32::from_gray(40)
            };
            painter.rect_filled(top, CornerRadius::ZERO, fill);
            // Below the black key, the two white keys beside it meet.
            painter.vline(
                (x0 + x1) / 2.0,
                split..=strip.bottom(),
                Stroke::new(1.0, Color32::from_gray(150)),
            );
        } else {
            if on {
                painter.rect_filled(
                    Rect::from_min_max(pos2(x0, strip.top()), pos2(x1, strip.bottom())),
                    CornerRadius::ZERO,
                    theme::ACCENT,
                );
            }
            // White keys next to each other, E-F and B-C, part all the way.
            if !is_black(key.wrapping_sub(1)) && key > LOWEST {
                painter.vline(
                    x0,
                    strip.y_range(),
                    Stroke::new(1.0, Color32::from_gray(150)),
                );
            }
        }
        if key % 12 == 0 && col * 2.0 >= 14.0 {
            painter.text(
                pos2(x0 + 1.0, strip.bottom() - 2.0),
                Align2::LEFT_BOTTOM,
                note_name(key),
                FontId::proportional(9.0),
                Color32::from_gray(60),
            );
        }
    }
}

/// The pedal light: lit while the sustain pedal is down, from a clip or
/// from a click here, which holds it for playing the keys.
fn pedal_light(app: &mut App, ui: &mut Ui, rect: Rect, track: TrackId, down: bool) {
    let resp = ui.interact(rect, pedal_id(track), Sense::click());
    let painter = ui.painter_at(rect.expand(2.0));
    let (fill, lamp) = if down {
        (theme::ACCENT_BG, theme::ACCENT)
    } else {
        (theme::PANEL, theme::GRID_BAR)
    };
    let stroke = if resp.hovered() {
        theme::TEXT_DIM
    } else {
        theme::GRID_BEAT
    };
    painter.rect(
        rect,
        CornerRadius::same(10),
        fill,
        Stroke::new(1.0, stroke),
        StrokeKind::Inside,
    );
    let at = pos2(rect.left() + 11.0, rect.center().y);
    if down {
        painter.circle_filled(at, 7.0, theme::ACCENT.gamma_multiply(0.25));
    }
    painter.circle_filled(at, 4.5, lamp);
    painter.text(
        pos2(rect.left() + 21.0, rect.center().y),
        Align2::LEFT_CENTER,
        "Pedal",
        theme::small(),
        if down { theme::TEXT } else { theme::TEXT_WEAK },
    );
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Pedal"));
    let resp = resp.on_hover_text(if down {
        "The sustain pedal is down. Click to let it up."
    } else {
        "The sustain pedal is up. Click to hold it down while you play the keys; \
         playback moves it wherever a clip's pedal does."
    });
    if resp.clicked() {
        let value = if down { 0 } else { 127 };
        match app.session.controller(track, SUSTAIN_PEDAL, value) {
            Ok(()) | Err(SessionError::NoEngine) => {}
            Err(e) => app.error(e.to_string()),
        }
        ui.ctx().request_repaint_after(Duration::from_millis(30));
    }
}

impl App {
    /// A track's piano changed, or could not.
    pub fn piano_library_set(&mut self, result: Result<LibrarySet, String>) {
        self.devices.download = None;
        self.refresh();
        match result {
            Ok(set) => {
                if self.view.project.track(set.track).is_ok() {
                    self.sel.select_track(set.track);
                    self.workspace.show(Tab::Instrument);
                }
                let how = if set.downloaded {
                    "Downloaded and loaded"
                } else {
                    "Loaded"
                };
                self.info(format!("{how} {}", set.library));
            }
            Err(e) => self.error(format!("Changing the piano failed: {e}")),
        }
    }
}
