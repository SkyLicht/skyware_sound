// SPDX-License-Identifier: GPL-3.0-or-later
//! Hearing things before keeping them: keys played on a track's instrument
//! (the piano roll's keys and the Instrument tab's keyboard), notes as they
//! are placed or moved, a bank's sounds, a pack's sounds in the Load sounds
//! dialog, and loops and SFX sets rendered the way they export. None of it
//! changes the project.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use egui::{
    Align2, Color32, Context, CornerRadius, FontId, Id, Rect, Sense, Stroke, StrokeKind, Ui, pos2,
    vec2,
};
use sky_core::manifest::Selection;
use sky_core::music::note_name;
use sky_core::{AssetId, TrackId};
use sky_dsp::{AudioData, InstrumentType};
use sky_model::{GameAssetKind, Track};
use sky_session::{PreviewOptions, SessionError};

use crate::app::App;
use crate::browser::Item;
use crate::jobs::JobOutput;
use crate::theme;

/// The shortest and longest a placed or moved note sounds, in seconds.
const NOTE_SECONDS: (f64, f64) = (0.08, 0.6);
/// How long before a loop's seam Join starts, in seconds.
const JOIN_SECONDS: f64 = 2.0;
/// How long after a preview starts its button shows Stop, before the
/// engine has said it plays.
const START_GRACE: f64 = 0.3;
const NO_DEVICE: &str = "Nothing to play it through: no audio device is running";
/// Most notes heard at once when a chord is moved.
const MAX_HEARD: usize = 8;

/// What the preview player plays, so the button that started it can show
/// Stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewKey {
    /// A game loop: its intro then its body, repeating, or (`join`) from
    /// just before the body wraps around.
    GameLoop { asset: AssetId, join: bool },
    /// An SFX set: one variation per trigger.
    GameSfx(AssetId),
    /// The loop region as Export loop renders it.
    LoopRegion { join: bool },
    /// A sound of the pack in the Load sounds dialog.
    PackSound(String),
    /// Something in the browser.
    Browser(Item),
}

/// Audio rendered or fetched to be heard.
pub struct Rendered {
    pub audio: Vec<Arc<AudioData>>,
    /// The frame a loop repeats from.
    pub loop_start: Option<usize>,
    /// The project revision it was rendered from.
    pub revision: u64,
}

/// A small random number generator for SFX triggers; nothing here needs
/// more than that.
struct Rng(u64);

impl Rng {
    fn new() -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0x2545_f491, |d| d.as_nanos() as u64);
        Self(seed | 1)
    }

    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A number in `0..n`, for `n` at least 1.
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    /// A number in `[0, 1]`.
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / ((1u64 << 24) - 1) as f32
    }
}

/// Picks SFX variations the way `sky-runtime` does for each selection rule.
struct Picker {
    mode: Selection,
    last: Option<usize>,
    next_in_order: usize,
    order: Vec<usize>,
    order_pos: usize,
}

impl Picker {
    fn new(mode: Selection, count: usize) -> Self {
        Self {
            mode,
            last: None,
            next_in_order: 0,
            order: (0..count).collect(),
            order_pos: count,
        }
    }

    fn next(&mut self, rng: &mut Rng) -> usize {
        let count = self.order.len();
        if count <= 1 {
            return 0;
        }
        let pick = match self.mode {
            Selection::Random => rng.below(count),
            Selection::RandomNoRepeat => match self.last {
                Some(last) => {
                    let r = rng.below(count - 1);
                    if r >= last { r + 1 } else { r }
                }
                None => rng.below(count),
            },
            Selection::RoundRobin => {
                let pick = self.next_in_order % count;
                self.next_in_order = (pick + 1) % count;
                pick
            }
            Selection::Shuffle => {
                if self.order_pos >= count {
                    for i in (1..count).rev() {
                        let j = rng.below(i + 1);
                        self.order.swap(i, j);
                    }
                    self.order_pos = 0;
                }
                let pick = self.order.get(self.order_pos).copied().unwrap_or(0);
                self.order_pos += 1;
                pick
            }
        };
        self.last = Some(pick);
        pick
    }
}

pub struct Audition {
    /// Play notes as they are added, pressed or moved in the piano roll.
    pub hear_notes: bool,
    /// The key held down on a keyboard.
    pub held: Option<(TrackId, u8)>,
    /// What the preview player was last asked to play, its id, and when.
    playing: Option<(PreviewKey, u64, f64)>,
    /// A render or download under way.
    pending: Option<PreviewKey>,
    /// Rendered SFX variations, by asset.
    sfx: HashMap<AssetId, Rendered>,
    pickers: HashMap<AssetId, Picker>,
    /// What the last trigger of each SFX set played.
    pub last_trigger: HashMap<AssetId, String>,
    /// The next variation of each bank sound to play.
    turns: HashMap<(TrackId, String), u32>,
    rng: Rng,
}

impl Default for Audition {
    fn default() -> Self {
        Self {
            hear_notes: true,
            held: None,
            playing: None,
            pending: None,
            sfx: HashMap::new(),
            pickers: HashMap::new(),
            last_trigger: HashMap::new(),
            turns: HashMap::new(),
            rng: Rng::new(),
        }
    }
}

impl Audition {
    /// Forgets what belonged to the old project, keeping the settings.
    pub fn reset(&mut self) {
        *self = Self {
            hear_notes: self.hear_notes,
            ..Self::default()
        };
    }
}

/// How a button that starts a preview looks now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonState {
    Idle,
    /// Rendering or downloading first.
    Pending,
    Playing,
}

/// A button that starts a preview, shows Stop while it plays and a spinner
/// while it renders. Returns true when clicked.
pub fn preview_button(ui: &mut Ui, label: &str, tip: &str, state: ButtonState) -> bool {
    if state == ButtonState::Pending {
        ui.spinner();
    }
    let text = match state {
        ButtonState::Idle => label,
        ButtonState::Pending | ButtonState::Playing => "Stop",
    };
    ui.button(text).on_hover_text(tip).clicked()
}

/// How long a note of `ticks` from `start` sounds when heard, in seconds.
pub fn note_seconds(project: &sky_model::Project, start: i64, ticks: i64) -> f64 {
    let t = &project.tempo;
    (t.tick_to_seconds(start + ticks.max(1)) - t.tick_to_seconds(start))
        .clamp(NOTE_SECONDS.0, NOTE_SECONDS.1)
}

impl App {
    /// Holds `pitch` down on a track's instrument, letting go of the key
    /// held before; `None` lets go. Keyboards call this while pressed.
    pub fn hold_key(&mut self, track: TrackId, pitch: Option<u8>, velocity: u8) {
        let want = pitch.map(|p| (track, p.min(127)));
        if self.audition.held == want {
            return;
        }
        if let Some((t, p)) = self.audition.held.take() {
            let _ = self.session.note_off(t, p);
        }
        if let Some((t, p)) = want
            && self.session.note_on(t, p, velocity, None).is_ok()
        {
            self.audition.held = want;
        }
    }

    /// Plays short notes on a track, for notes added, pressed or moved,
    /// unless hearing notes is off.
    pub fn hear_notes(&mut self, track: TrackId, notes: &[(u8, u8)], seconds: f64) {
        if !self.audition.hear_notes {
            return;
        }
        let mut heard: Vec<u8> = Vec::new();
        for &(pitch, velocity) in notes {
            if heard.len() >= MAX_HEARD || heard.contains(&pitch) {
                continue;
            }
            heard.push(pitch);
            // Without a device there is nothing to hear; that is fine here.
            let _ = self.session.note_on(track, pitch, velocity, Some(seconds));
        }
    }

    /// Plays a sound of a track's sample bank, each time its next variation.
    pub fn hear_bank_sound(&mut self, track: TrackId, name: &str, variations: usize) {
        let turn = self
            .audition
            .turns
            .entry((track, name.to_string()))
            .or_insert(0);
        let variation = *turn;
        *turn = (variation + 1) % variations.max(1) as u32;
        if let Err(e) = self.session.play_sound(track, name, variation, 110)
            && !matches!(e, SessionError::NoEngine)
        {
            self.error(e.to_string());
        }
    }

    /// How a preview's button looks now.
    pub fn preview_state(&self, key: &PreviewKey) -> ButtonState {
        if self.audition.pending.as_ref() == Some(key) {
            return ButtonState::Pending;
        }
        match &self.audition.playing {
            Some((k, id, since))
                if k == key
                    && (self.session.previewing() == Some(*id)
                        || self.now - since < START_GRACE) =>
            {
                ButtonState::Playing
            }
            _ => ButtonState::Idle,
        }
    }

    /// Starts a preview from its button, or stops it when it plays.
    pub fn toggle_preview(&mut self, ctx: &Context, key: PreviewKey) {
        if self.preview_state(&key) == ButtonState::Idle {
            self.start_preview(ctx, key);
        } else {
            self.stop_preview();
        }
    }

    /// Stops the preview, and forgets one still rendering.
    pub fn stop_preview(&mut self) {
        self.audition.pending = None;
        if self.audition.playing.take().is_some() {
            let _ = self.session.stop_preview();
        }
    }

    /// Renders or fetches what `key` plays in the background, then plays it.
    pub fn start_preview(&mut self, ctx: &Context, key: PreviewKey) {
        if self.view.engine_rate.is_none() {
            // No render or download for nothing to play it through.
            self.audition.pending = None;
            self.error(NO_DEVICE);
            return;
        }
        let session = self.session.clone();
        let revision = self.view.revision;
        let project = self.view.project.clone();
        let work: Box<dyn FnOnce() -> Result<Rendered, String> + Send> = match &key {
            PreviewKey::GameLoop { asset, .. } | PreviewKey::GameSfx(asset) => {
                let Some(name) = project.game.asset(*asset).map(|a| a.name.clone()) else {
                    return;
                };
                Box::new(move || {
                    let r = session
                        .render_game_asset(&name)
                        .map_err(|e| e.to_string())?;
                    let looped = r.seam.is_some();
                    Ok(Rendered {
                        audio: r.audio.into_iter().map(Arc::new).collect(),
                        loop_start: looped.then_some(r.loop_start),
                        revision,
                    })
                })
            }
            PreviewKey::LoopRegion { .. } => Box::new(move || {
                let audio = session.render_loop(None).map_err(|e| e.to_string())?;
                Ok(Rendered {
                    audio: vec![Arc::new(audio)],
                    loop_start: Some(0),
                    revision,
                })
            }),
            PreviewKey::PackSound(name) => {
                let Some((pack, file)) = self.dialogs.load_sounds.as_ref().and_then(|d| {
                    let pack = d.listed_pack()?;
                    let file = d.files_of(name)?.first()?.clone();
                    Some((pack, file))
                }) else {
                    return;
                };
                let library = self.session.library();
                Box::new(move || {
                    let audio = library.fetch(&pack, &file)?;
                    Ok(Rendered {
                        audio: vec![Arc::new(audio)],
                        loop_start: None,
                        revision,
                    })
                })
            }
            PreviewKey::Browser(item) => match item {
                Item::Pool(id) => {
                    let Some(audio) = session.audio(*id) else {
                        self.error("That audio is no longer in the project");
                        return;
                    };
                    // Already decoded: no need for a job.
                    let rendered = Rendered {
                        audio: vec![audio],
                        loop_start: None,
                        revision,
                    };
                    self.audition.pending = Some(key.clone());
                    self.preview_ready(key, Ok(rendered));
                    return;
                }
                Item::File(path) => {
                    let path = path.clone();
                    Box::new(move || {
                        let audio = sky_io::read_audio(&path).map_err(|e| e.to_string())?;
                        Ok(Rendered {
                            audio: vec![Arc::new(audio)],
                            loop_start: None,
                            revision,
                        })
                    })
                }
                Item::PackSound { map, bank, sound } => {
                    let Some((pack, file)) = self.browser.pack_file(map, bank.as_deref(), sound)
                    else {
                        return;
                    };
                    let library = self.session.library();
                    Box::new(move || {
                        let audio = library.fetch(&pack, &file)?;
                        Ok(Rendered {
                            audio: vec![Arc::new(audio)],
                            loop_start: None,
                            revision,
                        })
                    })
                }
            },
        };
        self.audition.pending = Some(key.clone());
        self.jobs.spawn(ctx, None, move || JobOutput::Previewed {
            key,
            result: work(),
        });
    }

    /// Plays the next variation of an SFX set with its random pitch and
    /// gain, rendering the set first if it changed since.
    pub fn trigger_sfx(&mut self, ctx: &Context, asset: AssetId) {
        let fresh = self
            .audition
            .sfx
            .get(&asset)
            .is_some_and(|r| r.revision == self.view.revision);
        if fresh {
            self.play_sfx(asset);
        } else {
            self.start_preview(ctx, PreviewKey::GameSfx(asset));
        }
    }

    fn play_sfx(&mut self, asset: AssetId) {
        let project = self.view.project.clone();
        let Some(GameAssetKind::Sfx {
            selection,
            pitch_semitones,
            gain_db,
            ..
        }) = project.game.asset(asset).map(|a| &a.kind)
        else {
            return;
        };
        let Some(count) = self.audition.sfx.get(&asset).map(|r| r.audio.len()) else {
            return;
        };
        let au = &mut self.audition;
        let picker = au
            .pickers
            .entry(asset)
            .or_insert_with(|| Picker::new(*selection, count));
        if picker.mode != *selection || picker.order.len() != count {
            *picker = Picker::new(*selection, count);
        }
        let index = picker.next(&mut au.rng);
        let semitones = pitch_semitones.lerp(au.rng.unit());
        let gain = gain_db.lerp(au.rng.unit());
        let Some(audio) = au.sfx.get(&asset).and_then(|r| r.audio.get(index).cloned()) else {
            return;
        };
        let opts = PreviewOptions {
            semitones,
            gain_db: gain,
            ..Default::default()
        };
        let what = format!(
            "Variation {} of {count}, {semitones:+.1} st, {gain:+.1} dB",
            index + 1
        );
        self.audition.last_trigger.insert(asset, what);
        self.play(PreviewKey::GameSfx(asset), audio, opts);
    }

    fn play(&mut self, key: PreviewKey, audio: Arc<AudioData>, opts: PreviewOptions) {
        match self.session.preview(audio, opts) {
            Ok(id) => self.audition.playing = Some((key, id, self.now)),
            Err(SessionError::NoEngine) => self.error(NO_DEVICE),
            Err(e) => self.error(format!("Cannot play it: {e}")),
        }
    }

    /// A render or download for a preview finished.
    pub fn preview_ready(&mut self, key: PreviewKey, result: Result<Rendered, String>) {
        if self.audition.pending.as_ref() != Some(&key) {
            // Stopped, or another preview started, in the meantime.
            return;
        }
        self.audition.pending = None;
        let rendered = match result {
            Ok(r) => r,
            Err(e) => {
                self.error(format!("Cannot play it: {e}"));
                return;
            }
        };
        if let PreviewKey::GameSfx(asset) = key {
            self.audition.sfx.insert(asset, rendered);
            self.play_sfx(asset);
            return;
        }
        let Some(audio) = rendered.audio.into_iter().next() else {
            return;
        };
        let frames = audio.frames();
        let join = matches!(
            key,
            PreviewKey::GameLoop { join: true, .. } | PreviewKey::LoopRegion { join: true }
        );
        let start = match rendered.loop_start {
            Some(body) if join => {
                let lead = (JOIN_SECONDS * f64::from(audio.sample_rate)) as usize;
                frames - lead.min(frames.saturating_sub(body) / 2)
            }
            _ => 0,
        };
        let opts = PreviewOptions {
            start,
            loop_start: rendered.loop_start,
            ..Default::default()
        };
        self.play(key, audio, opts);
    }

    /// Lets go of a key whose button came up, and stops previews whose
    /// window closed. Once per frame.
    pub fn tidy_auditions(&mut self, ctx: &Context) {
        if self.audition.held.is_some() && !ctx.input(|i| i.pointer.primary_down()) {
            let track = self.audition.held.map(|(t, _)| t);
            if let Some(t) = track {
                self.hold_key(t, None, 0);
            }
        }
        let owner_open = |key: &PreviewKey| match key {
            PreviewKey::GameLoop { .. } | PreviewKey::GameSfx(_) => self.dialogs.game.is_some(),
            PreviewKey::LoopRegion { .. } => self.dialogs.export_loop.is_some(),
            PreviewKey::PackSound(_) => self.dialogs.load_sounds.is_some(),
            PreviewKey::Browser(_) => self.browser.open,
        };
        let orphaned = self
            .audition
            .playing
            .as_ref()
            .is_some_and(|(k, _, _)| !owner_open(k))
            || self
                .audition
                .pending
                .as_ref()
                .is_some_and(|k| !owner_open(k));
        if orphaned {
            self.stop_preview();
        }
    }
}

/// The key range the Instrument tab's keyboard shows for a track, and
/// whether it plays drums.
fn key_range(track: &Track, octaves: u8) -> (u8, u8, bool) {
    let drums = track.instrument.as_ref().is_some_and(|i| match i.kind {
        InstrumentType::Drums => true,
        InstrumentType::Bank => i.bank.as_ref().is_none_or(|b| b.pitched.is_none()),
        _ => false,
    });
    // Drums from C2, where the General MIDI kit starts, over the two
    // octaves that hold its pieces; others around C4.
    let octaves = octaves.clamp(1, if drums { 2 } else { 5 });
    let lo = if drums {
        36
    } else {
        60 - 12 * (octaves / 2).min(2)
    };
    (lo, (lo + 12 * octaves - 1).min(127), drums)
}

fn is_black(pitch: u8) -> bool {
    matches!(pitch % 12, 1 | 3 | 6 | 8 | 10)
}

pub fn keyboard_id(track: TrackId) -> Id {
    Id::new(("keyboard", track))
}

/// The Instrument tab's keyboard: press keys to hear the track's sound as
/// its parameters change. Lower on a key plays louder.
pub fn keyboard(app: &mut App, ui: &mut Ui, track: &Track) {
    const KEY_W: f32 = 18.0;
    const HEIGHT: f32 = 46.0;
    let width = ui.available_width().max(KEY_W * 7.0);
    let octaves = (width / (KEY_W * 7.0)).floor().min(5.0) as u8;
    let (lo, hi, drums) = key_range(track, octaves);
    let whites: Vec<u8> = (lo..=hi).filter(|p| !is_black(*p)).collect();
    // Wider keys for a kit, so each can carry its drum's name.
    let key_w = (width / whites.len().max(1) as f32).min(if drums { 44.0 } else { 28.0 });
    let (rect, _) =
        ui.allocate_exact_size(vec2(key_w * whites.len() as f32, HEIGHT), Sense::hover());
    let response = ui.interact(rect, keyboard_id(track.id), Sense::click_and_drag());
    let black_h = HEIGHT * 0.6;
    let black_rect = |p: u8| -> Option<Rect> {
        let below = p.checked_sub(1)?;
        let i = whites.iter().position(|w| *w == below)?;
        let x = rect.left() + (i as f32 + 1.0) * key_w;
        Some(Rect::from_min_max(
            pos2(x - key_w * 0.3, rect.top()),
            pos2(x + key_w * 0.3, rect.top() + black_h),
        ))
    };
    let key_at = |p: egui::Pos2| -> Option<(u8, f32)> {
        let y = ((p.y - rect.top()) / HEIGHT).clamp(0.0, 1.0);
        if p.y - rect.top() < black_h
            && let Some(b) = (lo..=hi)
                .filter(|k| is_black(*k))
                .find(|k| black_rect(*k).is_some_and(|r| r.x_range().contains(p.x)))
        {
            return Some((b, (p.y - rect.top()) / black_h));
        }
        let i = ((p.x - rect.left()) / key_w).floor().max(0.0) as usize;
        whites
            .get(i.min(whites.len().saturating_sub(1)))
            .map(|w| (*w, y))
    };
    if response.is_pointer_button_down_on()
        && let Some((pitch, depth)) = ui.input(|i| i.pointer.latest_pos()).and_then(key_at)
    {
        let velocity = (40.0 + depth.clamp(0.0, 1.0) * 87.0).round() as u8;
        app.hold_key(track.id, Some(pitch), velocity);
    }
    let held = app
        .audition
        .held
        .filter(|(t, _)| *t == track.id)
        .map(|(_, p)| p);
    let tip = if drums {
        "Press to hear the kit; lower on a key plays louder"
    } else {
        "Press to hear the instrument; lower on a key plays louder"
    };
    let hovered = response.hover_pos().and_then(key_at).map(|(k, _)| k);
    response.on_hover_ui(|ui| {
        ui.label(tip);
        if let Some(k) = hovered {
            let names: Vec<&str> = sky_pattern::drum_names(k).collect();
            if drums && !names.is_empty() {
                ui.label(format!("{} ({})", note_name(k), names.join(", ")));
            } else {
                ui.label(note_name(k));
            }
        }
    });

    let painter = ui.painter_at(rect);
    for (i, w) in whites.iter().enumerate() {
        let r = Rect::from_min_size(
            pos2(rect.left() + i as f32 * key_w, rect.top()),
            vec2(key_w, HEIGHT),
        );
        let fill = if held == Some(*w) {
            theme::ACCENT
        } else {
            Color32::from_gray(205)
        };
        painter.rect_filled(r.shrink2(vec2(0.5, 0.0)), CornerRadius::same(2), fill);
        let label = if drums {
            sky_pattern::drum_names(*w).next().map(str::to_string)
        } else {
            (w % 12 == 0).then(|| note_name(*w))
        };
        if let Some(label) = label.filter(|_| key_w >= 16.0) {
            painter.text(
                pos2(r.center().x, r.bottom() - 3.0),
                Align2::CENTER_BOTTOM,
                label,
                FontId::proportional(9.0),
                Color32::from_gray(60),
            );
        }
    }
    for b in (lo..=hi).filter(|k| is_black(*k)) {
        if let Some(r) = black_rect(b) {
            let fill = if held == Some(b) {
                theme::ACCENT
            } else {
                Color32::from_gray(40)
            };
            painter.rect_filled(r, CornerRadius::same(2), fill);
            if drums
                && r.width() >= 18.0
                && let Some(name) = sky_pattern::drum_names(b).next()
            {
                painter.text(
                    pos2(r.center().x, r.bottom() - 3.0),
                    Align2::CENTER_BOTTOM,
                    name,
                    FontId::proportional(9.0),
                    Color32::from_gray(200),
                );
            }
        }
    }
    painter.rect_stroke(
        rect,
        CornerRadius::same(2),
        Stroke::new(1.0, theme::BG),
        StrokeKind::Inside,
    );
}
