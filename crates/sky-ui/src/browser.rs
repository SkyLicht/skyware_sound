// SPDX-License-Identifier: GPL-3.0-or-later
//! The browser: a panel on the left listing the project's audio, the sample
//! packs, and folders of samples on disk, with one search box. Clicking an
//! item plays it; dragging it onto the arrangement puts it there (a clip on
//! an audio track, the sample of a sampler, a sound of a sample bank, or a
//! new track below the others); double-clicking puts it where Import audio
//! would. Files and folders dropped on the window from the desktop arrive
//! here too.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align, Align2, Color32, Context, CornerRadius, DragAndDrop, Frame, Id, LayerId, Layout, Margin,
    Order, Rect, Response, RichText, ScrollArea, Sense, Stroke, StrokeKind, TextEdit, Ui, pos2,
    vec2,
};
use sky_core::{AudioId, PPQ, Ticks};
use sky_dsp::InstrumentType;
use sky_library::{MapFile, PACKS, Pack, PackInfo, PackSource};
use sky_model::{AudioSource, Origin, Project, TrackKind};
use sky_session::{AudioInput, AudioTarget, ImportedFiles, Placed, sound_name};

use crate::app::{App, Tab};
use crate::arrange::snap_round;
use crate::audition::{ButtonState, PreviewKey};
use crate::dialogs::PathPurpose;
use crate::jobs::JobOutput;
use crate::sounds::{license_color, license_note};
use crate::theme;

/// Audio files the browser lists and the window takes when they are
/// dropped on it.
pub const AUDIO_EXTENSIONS: [&str; 4] = ["wav", "flac", "ogg", "mp3"];
/// The most files indexed under one folder.
const MAX_FOLDER_FILES: usize = 20_000;
/// How many folders deep a folder is indexed.
const MAX_DEPTH: usize = 12;
/// The most search results listed under one pack or folder.
const MAX_RESULTS: usize = 200;
const ROW_H: f32 = 20.0;
const INDENT: f32 = 12.0;
const NOT_TAKEN: &str =
    "only WAV, FLAC, OGG and MP3 files, folders and strudel.json can be dropped here";
const ITEM_TIP: &str = "Click to hear it, drag it onto a track, or double-click to add it";

pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.iter().any(|a| a.eq_ignore_ascii_case(e)))
}

/// Something in the browser that can be heard and placed. A drag from the
/// browser carries one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Item {
    Pool(AudioId),
    /// A sound of a pack: the pack's sample map address, the sound's bank,
    /// and its name in the bank.
    PackSound {
        map: String,
        bank: Option<String>,
        sound: String,
    },
    File(PathBuf),
}

/// The widget id of an item's row.
pub fn item_id(item: &Item) -> Id {
    Id::new(("browser_item", item))
}

/// The widget id of a row that holds others, such as a pack or folder.
pub fn row_id(key: &str) -> Id {
    Id::new(("browser_row", key))
}

pub fn pack_key(info: &PackInfo) -> String {
    format!("pack:{}", info.id)
}

pub fn bank_key(info: &PackInfo, bank: &str) -> String {
    format!("bank:{}:{bank}", info.id)
}

pub fn folder_key(path: &Path) -> String {
    format!("dir:{}", path.display())
}

/// A pack as the browser lists it.
pub struct PackListing {
    pub pack: Arc<Pack>,
    /// Each bank's sounds, with how many files each sound has.
    pub banks: Vec<(String, Vec<(String, usize)>)>,
    /// The sounds in no bank.
    pub plain: Vec<(String, usize)>,
}

impl PackListing {
    pub fn new(pack: Arc<Pack>) -> Self {
        let banks = pack
            .map
            .banks()
            .into_keys()
            .map(|bank| {
                let sounds = pack
                    .map
                    .bank_sounds(Some(&bank))
                    .into_iter()
                    .map(|(name, files)| (name, files.len()))
                    .collect();
                (bank, sounds)
            })
            .collect();
        let plain = pack
            .map
            .plain_sounds()
            .into_iter()
            .map(|(name, files)| (name, files.len()))
            .collect();
        Self { pack, banks, plain }
    }
}

/// A folder's audio files and subfolders, as indexed.
pub struct Folder {
    pub path: PathBuf,
    pub dirs: Vec<Folder>,
    pub files: Vec<PathBuf>,
}

/// What was found under one of the browser's folders.
pub struct FolderIndex {
    pub tree: Folder,
    /// Every audio file found.
    pub all: Vec<PathBuf>,
    /// True when the folder holds more than the browser lists.
    pub truncated: bool,
}

impl FolderIndex {
    /// Looks through `root` for audio files, leaving out hidden folders.
    pub fn build(root: &Path) -> Result<FolderIndex, String> {
        if !root.is_dir() {
            return Err(format!("{} is not a folder", root.display()));
        }
        let mut all = Vec::new();
        let mut truncated = false;
        let tree = walk(root, 0, &mut all, &mut truncated)
            .map_err(|e| format!("cannot read {}: {e}", root.display()))?;
        Ok(FolderIndex {
            tree,
            all,
            truncated,
        })
    }
}

fn walk(
    dir: &Path,
    depth: usize,
    all: &mut Vec<PathBuf>,
    truncated: &mut bool,
) -> std::io::Result<Folder> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name().to_string_lossy().to_lowercase());
    let mut subdirs = Vec::new();
    let mut files = Vec::new();
    for e in entries {
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(kind) = e.file_type() else {
            continue;
        };
        let path = e.path();
        if kind.is_dir() {
            subdirs.push(path);
        } else if kind.is_file() && is_audio(&path) {
            if all.len() >= MAX_FOLDER_FILES {
                *truncated = true;
                break;
            }
            all.push(path.clone());
            files.push(path);
        }
    }
    let mut dirs = Vec::new();
    for sub in subdirs {
        if depth + 1 >= MAX_DEPTH || all.len() >= MAX_FOLDER_FILES {
            *truncated = true;
            break;
        }
        // A folder that cannot be read is left out, not fatal.
        if let Ok(f) = walk(&sub, depth + 1, all, truncated)
            && !(f.files.is_empty() && f.dirs.is_empty())
        {
            dirs.push(f);
        }
    }
    Ok(Folder {
        path: dir.to_path_buf(),
        dirs,
        files,
    })
}

enum Reading<T> {
    Busy,
    Ready(T),
    Failed(String),
}

pub struct BrowserState {
    pub open: bool,
    pub search: String,
    /// Play an item when it is clicked.
    pub hear: bool,
    /// Folders of samples, in the order they were added.
    pub folders: Vec<PathBuf>,
    /// The item last clicked.
    pub selected: Option<Item>,
    /// The rows shown open: sections, packs, banks and folders, by key.
    expanded: HashSet<String>,
    packs: HashMap<String, Reading<Arc<PackListing>>>,
    indexes: HashMap<PathBuf, Reading<Arc<FolderIndex>>>,
    /// Where the list of folders is kept between runs.
    store: Option<PathBuf>,
    /// What dropping the dragged item where it is would do, and whether it
    /// can be dropped there.
    drop_hint: Option<(String, bool)>,
    /// Files dropped on the window that were not taken, to mention once
    /// the audio dropped with them is in.
    turned_away: Vec<String>,
}

impl BrowserState {
    /// The browser, with the folders kept in `store` by an earlier run.
    pub fn new(store: Option<PathBuf>) -> Self {
        let folders = store
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|text| {
                text.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            open: true,
            search: String::new(),
            hear: true,
            folders,
            selected: None,
            expanded: ["pool", "packs", "folders"].map(String::from).into(),
            packs: HashMap::new(),
            indexes: HashMap::new(),
            store,
            drop_hint: None,
            turned_away: Vec::new(),
        }
    }

    fn save_folders(&self) {
        let Some(path) = &self.store else {
            return;
        };
        let text: String = self
            .folders
            .iter()
            .map(|f| format!("{}\n", f.display()))
            .collect();
        let saved = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(path, text));
        if let Err(e) = saved {
            tracing::warn!(
                "cannot keep the browser's folders in {}: {e}",
                path.display()
            );
        }
    }

    /// Adds a folder and shows it open. False when it was there already.
    pub fn add_folder(&mut self, path: PathBuf) -> bool {
        self.open = true;
        self.expanded.insert("folders".into());
        self.expanded.insert(folder_key(&path));
        if self.folders.contains(&path) {
            return false;
        }
        self.folders.push(path);
        self.save_folders();
        true
    }

    pub fn remove_folder(&mut self, path: &Path) {
        self.folders.retain(|f| f != path);
        self.indexes.remove(path);
        self.save_folders();
    }

    pub fn is_expanded(&self, key: &str) -> bool {
        self.expanded.contains(key)
    }

    pub fn set_expanded(&mut self, key: &str, open: bool) {
        if open {
            self.expanded.insert(key.to_string());
        } else {
            self.expanded.remove(key);
        }
    }

    /// A pack's listing, once read.
    pub fn listing(&self, map: &str) -> Option<&Arc<PackListing>> {
        match self.packs.get(map) {
            Some(Reading::Ready(l)) => Some(l),
            _ => None,
        }
    }

    /// True once a pack's map has been asked for, whether it was read or
    /// not.
    pub fn pack_asked(&self, map: &str) -> bool {
        self.packs.contains_key(map)
    }

    /// True when reading a pack's map failed.
    pub fn pack_failed(&self, map: &str) -> bool {
        matches!(self.packs.get(map), Some(Reading::Failed(_)))
    }

    /// The first file of a pack's sound, with the pack, once it is read.
    pub fn pack_file(
        &self,
        map: &str,
        bank: Option<&str>,
        sound: &str,
    ) -> Option<(Arc<Pack>, MapFile)> {
        let pack = &self.listing(map)?.pack;
        let sounds = match bank {
            Some(b) => pack.map.bank_sounds(Some(b)),
            None => pack.map.plain_sounds(),
        };
        let file = sounds.get(sound)?.first()?.clone();
        Some((pack.clone(), file))
    }

    pub fn folder_index(&self, path: &Path) -> Option<&Arc<FolderIndex>> {
        match self.indexes.get(path) {
            Some(Reading::Ready(i)) => Some(i),
            _ => None,
        }
    }
}

/// What an item is called in the project and in messages.
pub fn item_name(project: &Project, item: &Item) -> String {
    match item {
        Item::Pool(id) => project
            .pool_entry(*id)
            .map_or_else(|_| "audio".to_string(), |e| e.name.clone()),
        Item::PackSound { bank, sound, .. } => match bank {
            Some(b) => format!("{b} {sound}"),
            None => sound.clone(),
        },
        Item::File(path) => path
            .file_stem()
            .map_or_else(|| "audio".to_string(), |s| s.to_string_lossy().into_owned()),
    }
}

/// The search's words, in lower case.
fn terms(search: &str) -> Vec<String> {
    search.split_whitespace().map(str::to_lowercase).collect()
}

/// True when `text` has every word of the search.
fn matches(terms: &[String], text: &str) -> bool {
    let text = text.to_lowercase();
    terms.iter().all(|w| text.contains(w.as_str()))
}

fn seconds_text(seconds: f64) -> String {
    if seconds < 10.0 {
        format!("{seconds:.1} s")
    } else if seconds < 60.0 {
        format!("{seconds:.0} s")
    } else {
        let s = seconds.round() as u64;
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// "bar 5" or "bar 5, beat 3" for a tick.
fn position_text(project: &Project, tick: Ticks) -> String {
    let bar_len = project.ticks_per_bar().max(1);
    let beat_len = (PPQ * 4 / i64::from(project.time_signature.denominator.max(1))).max(1);
    let bar = tick.max(0) / bar_len + 1;
    let into = tick.max(0) % bar_len;
    if into == 0 {
        format!("bar {bar}")
    } else {
        format!("bar {bar}, beat {}", into / beat_len + 1)
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

impl App {
    /// The start of the bar the playhead is in.
    pub fn playhead_bar(&self) -> Ticks {
        let bar = self.view.project.ticks_per_bar().max(1);
        (self.transport.position_ticks.max(0.0) as i64 / bar) * bar
    }

    /// Where audio goes when it is not dropped anywhere in particular: on
    /// the selected audio track at the playhead's bar, into the selected
    /// sampler or sample bank, or else on a new track after the selected
    /// one.
    pub fn default_audio_target(&self) -> AudioTarget {
        let project = &self.view.project;
        let start = self.playhead_bar();
        let Some(track) = self.sel.track.and_then(|t| project.track(t).ok()) else {
            return AudioTarget::NewTrack { start, index: None };
        };
        match (track.kind, track.instrument.as_ref().map(|i| i.kind)) {
            (TrackKind::Audio, _) => AudioTarget::Clip {
                track: track.id,
                start,
            },
            (_, Some(InstrumentType::Sampler)) => AudioTarget::Sampler(track.id),
            (_, Some(InstrumentType::Bank)) => AudioTarget::Bank(track.id),
            _ => AudioTarget::NewTrack {
                start,
                index: project.track_index(track.id).ok().map(|i| i + 1),
            },
        }
    }

    /// Puts a browser item where `target` says, fetching or decoding it in
    /// the background first when it needs that.
    pub fn drop_item(&mut self, ctx: &Context, item: &Item, target: AudioTarget) {
        let name = item_name(&self.view.project, item);
        let session = self.session.clone();
        match item {
            Item::Pool(id) => {
                let r = session
                    .place_audio(AudioInput::Pool(*id), target, Origin::User)
                    .map_err(|e| e.to_string());
                self.audio_placed(&name, r);
            }
            Item::File(path) => {
                let path = path.clone();
                self.jobs
                    .spawn(ctx, Some(format!("Adding {name}")), move || {
                        JobOutput::Placed {
                            result: session
                                .place_audio(AudioInput::File(path), target, Origin::User)
                                .map_err(|e| e.to_string()),
                            name,
                        }
                    });
            }
            Item::PackSound { map, bank, sound } => {
                let Some((pack, file)) = self.browser.pack_file(map, bank.as_deref(), sound) else {
                    self.error(format!("{name} is not in a pack the browser has read"));
                    return;
                };
                let library = self.session.library();
                let sound = sound.clone();
                self.jobs
                    .spawn(ctx, Some(format!("Adding {name}")), move || {
                        let result = library.fetch(&pack, &file).and_then(|audio| {
                            let source = AudioSource {
                                pack: pack.source.name.clone(),
                                url: pack.map.locate(&file),
                                license: pack.source.license,
                                credit: pack.source.credit.clone(),
                            };
                            let input = AudioInput::Pack {
                                name: name.clone(),
                                sound,
                                audio: Arc::new(audio),
                                source,
                                root: file.root,
                            };
                            session
                                .place_audio(input, target, Origin::User)
                                .map_err(|e| e.to_string())
                        });
                        JobOutput::Placed { name, result }
                    });
            }
        }
    }

    /// Audio was put into the project: selects where it went and says so.
    pub fn audio_placed(&mut self, name: &str, result: Result<Placed, String>) {
        let turned_away = std::mem::take(&mut self.browser.turned_away);
        let Some(placed) = self.report(result.map_err(|e| format!("Cannot add {name}: {e}")))
        else {
            return;
        };
        self.refresh();
        let project = self.view.project.clone();
        let track = placed.track.and_then(|t| project.track(t).ok());
        let text = match (track, placed.clip, &placed.sound) {
            (Some(t), Some(clip), _) => {
                self.sel.select_clip(t.id, clip);
                if placed.new_track {
                    format!("Added {name} on a new track")
                } else {
                    format!("Added {name} to {}", t.name)
                }
            }
            (Some(t), None, Some((sound, variation))) => {
                self.sel.select_track(t.id);
                self.workspace.show(Tab::Instrument);
                if *variation == 0 {
                    format!("Added {name} to {} as {sound}", t.name)
                } else {
                    format!("Added {name} to {} as {sound}:{variation}", t.name)
                }
            }
            (Some(t), None, None) => {
                self.sel.select_track(t.id);
                self.workspace.show(Tab::Instrument);
                format!("Loaded {name} into {}", t.name)
            }
            (None, _, _) => format!("Imported {name}"),
        };
        self.say_turned_away(text, &turned_away);
    }

    /// Says what was done, and which dropped files were not taken.
    fn say_turned_away(&mut self, done: String, turned_away: &[String]) {
        if turned_away.is_empty() {
            self.info(done);
        } else {
            self.error(format!(
                "{done}. Not {}: {NOT_TAKEN}",
                turned_away.join(", ")
            ));
        }
    }

    /// Several files came into the pool at once.
    pub fn files_imported(&mut self, result: Result<ImportedFiles, String>) {
        let turned_away = std::mem::take(&mut self.browser.turned_away);
        let Some(done) = self.report(result.map_err(|e| format!("Import failed: {e}"))) else {
            return;
        };
        self.refresh();
        self.workspace.show(Tab::Browser);
        self.browser.set_expanded("pool", true);
        let mut text = format!(
            "Imported {} files into the project's audio; drag them from the browser onto a track",
            done.audio.len()
        );
        if let Some(first) = done.failed.first() {
            text.push_str(&format!(
                ". {} could not be read, such as {first}",
                done.failed.len()
            ));
        }
        self.say_turned_away(text, &turned_away);
    }

    /// Reads a pack's sample map in the background, for the browser.
    pub fn read_browser_pack(&mut self, ctx: &Context, info: &PackInfo) {
        self.read_pack_listing(ctx, PackSource::from(info));
    }

    /// Reads a pack's sample map in the background, kept by its address
    /// for the browser and for completing the names of linked packs.
    pub fn read_pack_listing(&mut self, ctx: &Context, source: PackSource) {
        let map = source.map.clone();
        self.browser.packs.insert(map.clone(), Reading::Busy);
        let library = self.session.library();
        let name = source.name.clone();
        self.jobs
            .spawn(ctx, Some(format!("Reading {name}")), move || {
                JobOutput::BrowserPack {
                    result: library
                        .open(&source)
                        .map(|p| Arc::new(PackListing::new(Arc::new(p))))
                        .map_err(|e| e.to_string()),
                    map,
                }
            });
    }

    pub fn browser_pack_read(&mut self, map: String, result: Result<Arc<PackListing>, String>) {
        let reading = match result {
            Ok(l) => Reading::Ready(l),
            Err(e) => Reading::Failed(e),
        };
        self.browser.packs.insert(map, reading);
    }

    /// Looks through a folder of the browser in the background.
    pub fn index_folder(&mut self, ctx: &Context, path: &Path) {
        self.browser
            .indexes
            .insert(path.to_path_buf(), Reading::Busy);
        let path = path.to_path_buf();
        self.jobs
            .spawn(ctx, None, move || JobOutput::FolderIndexed {
                result: FolderIndex::build(&path).map(Arc::new),
                path,
            });
    }

    pub fn folder_indexed(&mut self, path: PathBuf, result: Result<Arc<FolderIndex>, String>) {
        // A folder removed in the meantime stays removed.
        if !self.browser.folders.contains(&path) {
            return;
        }
        let reading = match result {
            Ok(i) => Reading::Ready(i),
            Err(e) => Reading::Failed(e),
        };
        self.browser.indexes.insert(path, reading);
    }

    /// Takes files and folders dropped on the window from the desktop: a
    /// folder joins the browser, a `strudel.json` opens Load sounds on its
    /// pack, one audio file goes where Import audio would put it, and
    /// several go into the project's audio, as one undo step each.
    pub fn take_dropped_files(&mut self, ctx: &Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if dropped.is_empty() {
            return;
        }
        let mut audio = Vec::new();
        let mut ignored = Vec::new();
        for path in dropped {
            if path.is_dir() {
                if !self.browser.add_folder(path.clone()) {
                    self.info(format!("{} is in the browser already", path.display()));
                }
            } else if file_name(&path).eq_ignore_ascii_case("strudel.json") {
                self.run_action(ctx, crate::app::Action::LoadSounds);
                if let Some(d) = self.dialogs.load_sounds.as_mut() {
                    d.pack = None;
                    d.address = path.display().to_string();
                }
                self.read_pack(ctx);
            } else if is_audio(&path) {
                audio.push(path);
            } else {
                ignored.push(file_name(&path));
            }
        }
        if !ignored.is_empty() {
            if audio.is_empty() {
                self.error(format!("Not {}: {NOT_TAKEN}", ignored.join(", ")));
            } else {
                // Said once the audio is in, so that message does not hide it.
                self.browser.turned_away = ignored;
            }
        }
        match audio.as_slice() {
            [] => {}
            [one] => {
                let target = self.default_audio_target();
                self.drop_item(ctx, &Item::File(one.clone()), target);
            }
            _ => {
                let session = self.session.clone();
                let count = audio.len();
                self.jobs
                    .spawn(ctx, Some(format!("Importing {count} files")), move || {
                        JobOutput::FilesImported(
                            session
                                .import_files(&audio, Origin::User)
                                .map_err(|e| e.to_string()),
                        )
                    });
            }
        }
    }
}

/// One row of the tree: indented, with a disclosure triangle when `open`
/// is given, a name, and a detail on the right.
#[allow(clippy::too_many_arguments)]
fn paint_row(
    ui: &Ui,
    rect: Rect,
    resp: &Response,
    depth: usize,
    open: Option<bool>,
    text: &str,
    detail: Option<(&str, Color32)>,
    selected: bool,
    state: ButtonState,
) {
    let painter = ui.painter_at(rect);
    if selected {
        painter.rect_filled(rect, CornerRadius::same(3), theme::ACCENT_BG);
    } else if resp.hovered() {
        painter.rect_filled(
            rect,
            CornerRadius::same(3),
            theme::lighten(theme::PANEL, 0.05),
        );
    }
    let x0 = rect.left() + 4.0 + depth as f32 * INDENT;
    let cy = rect.center().y;
    // The slot before the name holds the disclosure triangle of a row that
    // holds others, or the dot of an item that plays.
    if let Some(open) = open {
        let s = 3.5;
        let points = if open {
            vec![
                pos2(x0, cy - s * 0.6),
                pos2(x0 + 2.0 * s, cy - s * 0.6),
                pos2(x0 + s, cy + s * 0.8),
            ]
        } else {
            vec![
                pos2(x0 + s * 0.3, cy - s),
                pos2(x0 + s * 1.7, cy),
                pos2(x0 + s * 0.3, cy + s),
            ]
        };
        painter.add(egui::Shape::convex_polygon(
            points,
            theme::TEXT_WEAK,
            Stroke::NONE,
        ));
    } else if state == ButtonState::Playing {
        painter.circle_filled(pos2(x0 + 3.5, cy), 3.0, theme::ACCENT);
    }
    let x = x0 + 12.0;
    let mut right = rect.right() - 4.0;
    if state == ButtonState::Pending {
        let r = Rect::from_center_size(pos2(right - 6.0, cy), vec2(12.0, 12.0));
        egui::Spinner::new().paint_at(ui, r);
        right -= 16.0;
    }
    if let Some((detail, color)) = detail {
        let g = painter.layout_no_wrap(detail.to_string(), theme::small(), color);
        right -= g.size().x;
        painter.galley(pos2(right, cy - g.size().y / 2.0), g, color);
        right -= 8.0;
    }
    let mut job = LayoutJob::simple_singleline(text.to_string(), theme::body(), theme::TEXT);
    job.wrap = TextWrapping::truncate_at_width((right - x).max(8.0));
    let g = painter.layout_job(job);
    painter.galley(pos2(x, cy - g.size().y / 2.0), g, theme::TEXT);
}

/// A row that holds others. Returns whether it is open, after a click.
fn toggle_row(
    app: &mut App,
    ui: &mut Ui,
    key: &str,
    depth: usize,
    text: &str,
    detail: Option<(&str, Color32)>,
    forced: bool,
) -> (bool, Response) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
    let resp = ui.interact(rect, row_id(key), Sense::click());
    if resp.clicked() && !forced {
        let open = app.browser.is_expanded(key);
        app.browser.set_expanded(key, !open);
    }
    let open = forced || app.browser.is_expanded(key);
    if ui.is_rect_visible(rect) {
        paint_row(
            ui,
            rect,
            &resp,
            depth,
            Some(open),
            text,
            detail,
            false,
            ButtonState::Idle,
        );
    }
    (open, resp)
}

/// A dim line of text in the tree, such as "No matches".
fn note_row(ui: &mut Ui, depth: usize, text: &str, color: Color32) {
    ui.horizontal(|ui| {
        ui.add_space(4.0 + depth as f32 * INDENT + 12.0);
        ui.add(egui::Label::new(RichText::new(text).small().color(color)).wrap());
    });
}

/// A row that is being read or looked through.
fn busy_row(ui: &mut Ui, depth: usize, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(4.0 + depth as f32 * INDENT + 12.0);
        ui.spinner();
        ui.label(RichText::new(text).small().color(theme::TEXT_WEAK));
    });
}

/// A row for something that can be heard and placed.
fn item_row(
    app: &mut App,
    ui: &mut Ui,
    item: Item,
    depth: usize,
    text: &str,
    detail: Option<(&str, Color32)>,
    tip: &str,
) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let resp = ui.interact(rect, item_id(&item), Sense::click_and_drag());
    let key = PreviewKey::Browser(item.clone());
    let selected = app.browser.selected.as_ref() == Some(&item);
    paint_row(
        ui,
        rect,
        &resp,
        depth,
        None,
        text,
        detail,
        selected,
        app.preview_state(&key),
    );
    resp.dnd_set_drag_payload(item.clone());
    let ctx = ui.ctx().clone();
    if resp.double_clicked() {
        app.browser.selected = Some(item.clone());
        let target = app.default_audio_target();
        app.drop_item(&ctx, &item, target);
    } else if resp.clicked() {
        app.browser.selected = Some(item.clone());
        if app.browser.hear {
            app.toggle_preview(&ctx, key);
        }
    }
    resp.context_menu(|ui| {
        if ui.button("Add on a new audio track").clicked() {
            let start = app.playhead_bar();
            app.drop_item(&ctx, &item, AudioTarget::NewTrack { start, index: None });
        }
    });
    resp.on_hover_text(tip);
}

/// The browser panel.
pub fn panel(app: &mut App, ui: &mut Ui) {
    // The tab names the panel, so the search and Hear share the top row.
    let search = ui
        .horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.toggle_value(&mut app.browser.hear, "Hear")
                    .on_hover_text("Play an item when it is clicked");
                ui.add(
                    TextEdit::singleline(&mut app.browser.search)
                        .id(Id::new("browser_search"))
                        .hint_text("Search")
                        .desired_width(ui.available_width()),
                )
            })
            .inner
        })
        .inner;
    if search.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.browser.search.clear();
    }
    ui.add_space(4.0);
    let terms = terms(&app.browser.search);
    let project = app.view.project.clone();
    ScrollArea::vertical()
        .id_salt("browser_rows")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            pool_section(app, ui, &project, &terms);
            packs_section(app, ui, &terms);
            folders_section(app, ui, &terms);
        });
}

fn pool_section(app: &mut App, ui: &mut Ui, project: &Project, terms: &[String]) {
    let searching = !terms.is_empty();
    let count = project.audio_pool.len().to_string();
    let (open, _) = toggle_row(
        app,
        ui,
        "pool",
        0,
        "Project audio",
        Some((&count, theme::TEXT_DIM)),
        searching,
    );
    if !open {
        return;
    }
    let mut shown = 0;
    for e in &project.audio_pool {
        if !matches(terms, &e.name) {
            continue;
        }
        shown += 1;
        let seconds = e.frames as f64 / f64::from(e.sample_rate.max(1));
        let tip = match &e.source {
            Some(s) => format!(
                "{} from {} ({})\n{ITEM_TIP}",
                e.name,
                s.pack,
                s.license.label()
            ),
            None => format!("{}\n{ITEM_TIP}", e.name),
        };
        let length = seconds_text(seconds);
        item_row(
            app,
            ui,
            Item::Pool(e.id),
            1,
            &e.name,
            Some((&length, theme::TEXT_DIM)),
            &tip,
        );
    }
    if shown == 0 {
        let text = if searching {
            "No matches"
        } else {
            "Nothing yet: import audio or drop files on the window"
        };
        note_row(ui, 1, text, theme::TEXT_DIM);
    }
}

fn packs_section(app: &mut App, ui: &mut Ui, terms: &[String]) {
    let (open, _) = toggle_row(app, ui, "packs", 0, "Sample packs", None, !terms.is_empty());
    if !open {
        return;
    }
    let top = ui.cursor().top();
    for info in PACKS {
        pack_rows(app, ui, info, terms);
    }
    if !terms.is_empty() && ui.cursor().top() == top {
        note_row(ui, 1, "No matches", theme::TEXT_DIM);
    }
}

fn pack_rows(app: &mut App, ui: &mut Ui, info: &PackInfo, terms: &[String]) {
    let ctx = ui.ctx().clone();
    let key = pack_key(info);
    let searching = !terms.is_empty();
    // A search looks inside every pack, so each is read once.
    if searching && !app.browser.packs.contains_key(info.map) {
        app.read_browser_pack(&ctx, info);
    }
    let busy = matches!(app.browser.packs.get(info.map), Some(Reading::Busy));
    let listing = app.browser.listing(info.map).cloned();
    // Searching shows a pack whose name matches or, once read, any of
    // whose sounds do.
    let mut hits: Vec<(Option<&str>, &str, usize)> = Vec::new();
    if let Some(l) = listing.as_deref().filter(|_| searching) {
        for (bank, sounds) in &l.banks {
            for (sound, files) in sounds {
                if matches(terms, &format!("{bank} {sound}")) {
                    hits.push((Some(bank), sound, *files));
                }
            }
        }
        for (sound, files) in &l.plain {
            if matches(terms, sound) {
                hits.push((None, sound, *files));
            }
        }
    }
    if searching && hits.is_empty() && !busy && !matches(terms, info.name) {
        return;
    }
    let (open, resp) = toggle_row(
        app,
        ui,
        &key,
        1,
        info.name,
        Some((info.license.label(), license_color(info.license))),
        !hits.is_empty() || (searching && busy),
    );
    let resp = resp.on_hover_text(format!("{}\n{}", info.about, license_note(info.license)));
    resp.context_menu(|ui| {
        if ui.button("Load into a sample bank…").clicked() {
            app.open_load_sounds_on(&ctx, info, None);
        }
    });
    if !open {
        return;
    }
    let listing = match app.browser.packs.get(info.map) {
        None => {
            app.read_browser_pack(&ctx, info);
            busy_row(ui, 2, "Reading the pack");
            return;
        }
        Some(Reading::Busy) => {
            busy_row(ui, 2, "Reading the pack");
            return;
        }
        Some(Reading::Failed(e)) => {
            let e = e.clone();
            note_row(ui, 2, &e, theme::ERROR);
            ui.horizontal(|ui| {
                ui.add_space(4.0 + 2.0 * INDENT + 12.0);
                if ui.small_button("Try again").clicked() {
                    app.read_browser_pack(&ctx, info);
                }
            });
            return;
        }
        Some(Reading::Ready(l)) => l.clone(),
    };
    if !hits.is_empty() {
        for (bank, sound, files) in hits.iter().take(MAX_RESULTS) {
            let item = Item::PackSound {
                map: info.map.to_string(),
                bank: bank.map(str::to_string),
                sound: sound.to_string(),
            };
            let text = match bank {
                Some(b) => format!("{b} {sound}"),
                None => sound.to_string(),
            };
            sound_row(app, ui, item, 2, &text, *files, info);
        }
        if hits.len() > MAX_RESULTS {
            let more = format!(
                "and {} more: search for more words",
                hits.len() - MAX_RESULTS
            );
            note_row(ui, 2, &more, theme::TEXT_DIM);
        }
        return;
    }
    for (bank, sounds) in &listing.banks {
        let count = sounds.len().to_string();
        let (open, resp) = toggle_row(
            app,
            ui,
            &bank_key(info, bank),
            2,
            bank,
            Some((&count, theme::TEXT_DIM)),
            false,
        );
        resp.context_menu(|ui| {
            if ui.button("Load this bank…").clicked() {
                app.open_load_sounds_on(&ctx, info, Some(bank));
            }
        });
        if open {
            for (sound, files) in sounds {
                let item = Item::PackSound {
                    map: info.map.to_string(),
                    bank: Some(bank.clone()),
                    sound: sound.clone(),
                };
                sound_row(app, ui, item, 3, sound, *files, info);
            }
        }
    }
    for (sound, files) in &listing.plain {
        let item = Item::PackSound {
            map: info.map.to_string(),
            bank: None,
            sound: sound.clone(),
        };
        sound_row(app, ui, item, 2, sound, *files, info);
    }
}

fn sound_row(
    app: &mut App,
    ui: &mut Ui,
    item: Item,
    depth: usize,
    text: &str,
    files: usize,
    info: &PackInfo,
) {
    let detail = (files > 1).then(|| files.to_string());
    let tip = format!(
        "{text} from {} ({}){}\n{ITEM_TIP}",
        info.name,
        info.license.label(),
        if files > 1 {
            format!(", {files} variations; the first plays and is added")
        } else {
            String::new()
        }
    );
    item_row(
        app,
        ui,
        item,
        depth,
        text,
        detail.as_deref().map(|d| (d, theme::TEXT_DIM)),
        &tip,
    );
}

fn folders_section(app: &mut App, ui: &mut Ui, terms: &[String]) {
    let ctx = ui.ctx().clone();
    let (open, _) = toggle_row(app, ui, "folders", 0, "Folders", None, !terms.is_empty());
    if !open {
        return;
    }
    if app.browser.folders.is_empty() {
        note_row(
            ui,
            1,
            "Add a folder of samples, or drop one on the window",
            theme::TEXT_DIM,
        );
    }
    let top = ui.cursor().top();
    for root in app.browser.folders.clone() {
        folder_rows(app, ui, &root, terms);
    }
    if !terms.is_empty() && !app.browser.folders.is_empty() && ui.cursor().top() == top {
        note_row(ui, 1, "No matches", theme::TEXT_DIM);
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(4.0 + INDENT);
        if ui
            .small_button("Add folder…")
            .on_hover_text("List a folder of samples here")
            .clicked()
        {
            app.request_path(&ctx, PathPurpose::BrowseFolder);
        }
    });
}

fn folder_rows(app: &mut App, ui: &mut Ui, root: &Path, terms: &[String]) {
    let ctx = ui.ctx().clone();
    let searching = !terms.is_empty();
    let name = file_name(root);
    // A search looks inside every folder, so each is looked through once.
    if searching && !app.browser.indexes.contains_key(root) {
        app.index_folder(&ctx, root);
    }
    let busy = matches!(app.browser.indexes.get(root), Some(Reading::Busy));
    let index = app.browser.folder_index(root).cloned();
    let hits: Vec<&PathBuf> = match index.as_deref() {
        Some(i) if searching => i
            .all
            .iter()
            .filter(|p| matches(terms, &p.strip_prefix(root).unwrap_or(p).to_string_lossy()))
            .take(MAX_RESULTS + 1)
            .collect(),
        _ => Vec::new(),
    };
    if searching && hits.is_empty() && !busy && !matches(terms, &name) {
        return;
    }
    let count = index.as_ref().map(|i| i.all.len().to_string());
    let (open, resp) = toggle_row(
        app,
        ui,
        &folder_key(root),
        1,
        &name,
        count.as_deref().map(|c| (c, theme::TEXT_DIM)),
        !hits.is_empty() || (searching && busy),
    );
    let resp = resp.on_hover_text(root.display().to_string());
    resp.context_menu(|ui| {
        if ui.button("Look through it again").clicked() {
            app.index_folder(&ctx, root);
        }
        if ui.button("Remove from the browser").clicked() {
            app.browser.remove_folder(root);
        }
    });
    if !open || !app.browser.folders.iter().any(|f| f == root) {
        return;
    }
    let index = match app.browser.indexes.get(root) {
        None => {
            app.index_folder(&ctx, root);
            busy_row(ui, 2, "Looking for audio files");
            return;
        }
        Some(Reading::Busy) => {
            busy_row(ui, 2, "Looking for audio files");
            return;
        }
        Some(Reading::Failed(e)) => {
            let e = e.clone();
            note_row(ui, 2, &e, theme::ERROR);
            return;
        }
        Some(Reading::Ready(i)) => i.clone(),
    };
    if !hits.is_empty() {
        for path in hits.iter().take(MAX_RESULTS) {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .display()
                .to_string();
            item_row(
                app,
                ui,
                Item::File((*path).clone()),
                2,
                &rel,
                None,
                &format!("{}\n{ITEM_TIP}", path.display()),
            );
        }
        if hits.len() > MAX_RESULTS {
            note_row(ui, 2, "and more: search for more words", theme::TEXT_DIM);
        }
        return;
    }
    if index.all.is_empty() {
        note_row(
            ui,
            2,
            "No WAV, FLAC, OGG or MP3 files here",
            theme::TEXT_DIM,
        );
    }
    dir_rows(app, ui, &index.tree, 2);
    if index.truncated {
        let text = format!(
            "Only the first {} files and {MAX_DEPTH} folders deep are listed",
            index.all.len()
        );
        note_row(ui, 2, &text, theme::WARN);
    }
}

fn dir_rows(app: &mut App, ui: &mut Ui, dir: &Folder, depth: usize) {
    for sub in &dir.dirs {
        let (open, _) = toggle_row(
            app,
            ui,
            &folder_key(&sub.path),
            depth,
            &file_name(&sub.path),
            None,
            false,
        );
        if open {
            dir_rows(app, ui, sub, depth + 1);
        }
    }
    for f in &dir.files {
        item_row(
            app,
            ui,
            Item::File(f.clone()),
            depth,
            &file_name(f),
            None,
            &format!("{}\n{ITEM_TIP}", f.display()),
        );
    }
}

/// Where an item dropped at `pos` in the arrangement goes, or why it
/// cannot go there.
fn drop_target(
    app: &App,
    project: &Project,
    pos: egui::Pos2,
    free: bool,
) -> Result<AudioTarget, String> {
    let st = &app.arrange;
    let start = if pos.x >= st.lane_left {
        // Alt places it off the grid, as when moving a clip.
        let grid = if free {
            None
        } else {
            st.snap.grid(project.time_signature)
        };
        snap_round(st.tick_at(pos.x), grid).max(0)
    } else {
        // Over a track header: at the playhead's bar.
        app.playhead_bar()
    };
    let row = st.lanes.iter().find(|(_, r)| r.y_range().contains(pos.y));
    let Some((id, _)) = row else {
        return Ok(AudioTarget::NewTrack { start, index: None });
    };
    let track = project.track(*id).map_err(|e| e.to_string())?;
    match (track.kind, track.instrument.as_ref().map(|i| i.kind)) {
        (TrackKind::Audio, _) => Ok(AudioTarget::Clip {
            track: track.id,
            start,
        }),
        (_, Some(InstrumentType::Sampler)) => Ok(AudioTarget::Sampler(track.id)),
        (_, Some(InstrumentType::Bank)) => Ok(AudioTarget::Bank(track.id)),
        (_, kind) => Err(format!(
            "{} plays {}, which takes no audio: drop it on an audio track, a sampler or a \
             sample bank",
            track.name,
            kind.map_or("nothing", |k| k.label())
        )),
    }
}

/// What dropping `item` on `target` does, in words.
fn target_text(project: &Project, item: &Item, target: AudioTarget) -> String {
    let name = item_name(project, item);
    let track = |id| project.track(id).map_or("the track", |t| t.name.as_str());
    match target {
        AudioTarget::Pool => format!("Import {name}"),
        AudioTarget::Clip { track: t, start } => format!(
            "Add {name} to {} at {}",
            track(t),
            position_text(project, start)
        ),
        AudioTarget::NewTrack { start, .. } => format!(
            "Add {name} on a new audio track at {}",
            position_text(project, start)
        ),
        AudioTarget::Sampler(t) => format!("Load {name} into {}", track(t)),
        AudioTarget::Bank(t) => {
            let sound = match item {
                Item::PackSound { sound, .. } => sound_name(sound),
                _ => sound_name(&name),
            };
            format!("Add {name} to {} as {sound}", track(t))
        }
    }
}

/// While an item from the browser is dragged over the arrangement, shows
/// where it would go, and puts it there when let go.
pub fn arrange_drop(app: &mut App, ui: &mut Ui, body: Rect, project: &Project) {
    let ctx = ui.ctx().clone();
    let Some(item) = DragAndDrop::payload::<Item>(&ctx) else {
        return;
    };
    let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) else {
        return;
    };
    if !body.contains(pos) {
        return;
    }
    let free = ctx.input(|i| i.modifiers.alt);
    let target = drop_target(app, project, pos, free);
    let painter = ctx
        .layer_painter(LayerId::new(Order::Foreground, Id::new("browser_drop")))
        .with_clip_rect(body);
    let row = app
        .arrange
        .lanes
        .iter()
        .find(|(_, r)| r.y_range().contains(pos.y))
        .map(|(_, r)| Rect::from_min_max(pos2(body.left(), r.top()), r.max));
    let area = row.unwrap_or_else(|| {
        let top = app
            .arrange
            .lanes
            .iter()
            .map(|(_, r)| r.bottom())
            .fold(body.top(), f32::max);
        Rect::from_min_max(pos2(body.left(), top), body.max)
    });
    let color = if target.is_ok() {
        theme::ACCENT
    } else {
        theme::ERROR
    };
    painter.rect_filled(area, CornerRadius::ZERO, color.gamma_multiply(0.12));
    painter.rect_stroke(
        area.shrink(1.0),
        CornerRadius::same(2),
        Stroke::new(1.5, color),
        StrokeKind::Inside,
    );
    if let Ok(AudioTarget::Clip { start, .. } | AudioTarget::NewTrack { start, .. }) = target {
        let x = app.arrange.x_of(start as f64);
        if x >= app.arrange.lane_left {
            painter.vline(x, area.y_range(), Stroke::new(2.0, theme::ACCENT));
        }
    }
    app.browser.drop_hint = Some(match &target {
        Ok(t) => (target_text(project, &item, *t), true),
        Err(e) => (e.clone(), false),
    });
    if ctx.input(|i| i.pointer.any_released()) {
        DragAndDrop::clear_payload(&ctx);
        app.browser.drop_hint = None;
        match target {
            Ok(t) => app.drop_item(&ctx, &item, t),
            Err(e) => app.error(e),
        }
    }
}

/// The label that follows a drag from the browser, and the note shown
/// while files from the desktop are held over the window. Once per frame,
/// after everything else is drawn.
pub fn overlays(app: &mut App, ctx: &Context) {
    let hint = app.browser.drop_hint.take();
    if let (Some(item), Some(pos)) = (
        DragAndDrop::payload::<Item>(ctx),
        ctx.input(|i| i.pointer.latest_pos()),
    ) {
        let (text, ok) = hint.unwrap_or_else(|| {
            (
                format!(
                    "{}: drop it on a track, or below them for a new one",
                    item_name(&app.view.project, &item)
                ),
                true,
            )
        });
        egui::Area::new(Id::new("browser_drag_label"))
            .order(Order::Tooltip)
            .fixed_pos(pos + vec2(14.0, 10.0))
            .interactable(false)
            .show(ctx, |ui| {
                Frame::popup(ui.style())
                    .inner_margin(Margin::symmetric(8, 4))
                    .show(ui, |ui| {
                        let color = if ok { theme::TEXT } else { theme::ERROR };
                        ui.set_max_width(360.0);
                        ui.label(RichText::new(text).color(color));
                    });
            });
    }

    let hovering = ctx.input(|i| i.raw.hovered_files.len());
    if hovering > 0 {
        let screen = ctx.content_rect();
        let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("files_hover")));
        painter.rect_filled(screen, CornerRadius::ZERO, theme::BG.gamma_multiply(0.7));
        painter.rect_stroke(
            screen.shrink(6.0),
            CornerRadius::same(8),
            Stroke::new(2.0, theme::ACCENT),
            StrokeKind::Inside,
        );
        let text = if hovering == 1 {
            "Drop to add it: an audio file goes where Import audio puts it, a folder joins the \
             browser, and a strudel.json opens Load sounds"
                .to_string()
        } else {
            format!(
                "Drop to bring in {hovering} items: audio files go into the project's audio, and \
                 folders join the browser"
            )
        };
        painter.text(
            screen.center(),
            Align2::CENTER_CENTER,
            text,
            theme::body(),
            theme::TEXT,
        );
    }
}
