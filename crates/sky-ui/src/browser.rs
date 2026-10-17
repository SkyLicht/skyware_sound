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
use sky_library::{MapFile, PACKS, Pack, PackInfo, PackSource, packs};
use sky_model::{AudioSource, Origin, PictureImage, Project, TrackKind};
use sky_session::{AudioInput, AudioTarget, ImportedFiles, Placed, sound_name};

use crate::app::{App, Tab};
use crate::arrange::snap_round;
use crate::audition::{ButtonState, PreviewKey};
use crate::dialogs::PathPurpose;
use crate::jobs::JobOutput;
use crate::pictures::{self, Cover, Style, Target};
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
/// A pack's or folder's row: its picture, name and a line about it.
const BIG_ROW_H: f32 = 40.0;
/// A bank's row, with a small picture.
const BANK_ROW_H: f32 = 22.0;
const INDENT: f32 = 12.0;
/// The narrowest a pack's tile gets before fewer fit in a row.
const TILE_W: f32 = 96.0;
/// Images a folder can bring as its cover, in its own folder.
const FOLDER_COVERS: [&str; 4] = ["cover.png", "cover.jpg", "folder.png", "folder.jpg"];
const NOT_TAKEN: &str = "only WAV, FLAC, OGG and MP3 files, folders, strudel.json and images (on a \
                         picture) can be dropped here";
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
    /// The cover image the folder brings, such as a `cover.png` in it.
    pub cover: Option<PictureImage>,
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
        let cover = FOLDER_COVERS
            .iter()
            .map(|name| root.join(name))
            .find(|p| p.is_file())
            .and_then(|p| match pictures::image_from_file(&p) {
                Ok(image) => Some(image),
                Err(e) => {
                    tracing::warn!("{e}");
                    None
                }
            });
        Ok(FolderIndex {
            tree,
            all,
            truncated,
            cover,
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
    /// Show the sample packs as tiles instead of a list.
    pub tiles: bool,
    /// The pack whose card shows at the bottom, by id.
    pub card: Option<&'static str>,
    /// How tall the card was last frame, to leave room for it.
    card_h: f32,
    /// A row to scroll into view once it is drawn, by key.
    reveal: Option<String>,
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
            tiles: false,
            card: None,
            card_h: 0.0,
            reveal: None,
        }
    }

    /// Shows a pack in the list, open, and scrolls to it.
    pub fn reveal_pack(&mut self, info: &PackInfo) {
        self.tiles = false;
        self.open = true;
        self.expanded.insert("packs".into());
        self.expanded.insert(pack_key(info));
        self.reveal = Some(pack_key(info));
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
        let map = info.map.to_string();
        self.browser.packs.insert(map.clone(), Reading::Busy);
        let library = self.session.library();
        let source = PackSource::from(info);
        self.jobs
            .spawn(ctx, Some(format!("Reading {}", info.name)), move || {
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
            Ok(i) => {
                let cover = i.cover.clone().map_or(Cover::None, Cover::Found);
                self.pictures.covers.insert(folder_key(&path), cover);
                Reading::Ready(i)
            }
            Err(e) => Reading::Failed(e),
        };
        self.browser.indexes.insert(path, reading);
    }

    /// Looks for the cover image a pack brings, once its map is read.
    pub fn look_for_pack_cover(&mut self, ctx: &Context, info: &PackInfo) {
        let key = pack_key(info);
        if self.pictures.covers.contains_key(&key) {
            return;
        }
        let Some(listing) = self.browser.listing(info.map) else {
            return;
        };
        let pack = listing.pack.clone();
        let library = self.session.library();
        self.pictures.covers.insert(key.clone(), Cover::Looking);
        self.jobs.spawn(ctx, None, move || {
            let cover = match library.cover(&pack) {
                Ok(Some((bytes, name))) => pictures::image_from_bytes(&bytes, &name)
                    .map_err(|e| tracing::warn!("cannot use the cover {name}: {e}"))
                    .ok(),
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!("cannot fetch the cover of {}: {e}", pack.source.name);
                    None
                }
            };
            JobOutput::CoverFound { key, cover }
        });
    }

    pub fn cover_found(&mut self, key: String, cover: Option<PictureImage>) {
        self.pictures
            .covers
            .insert(key, cover.map_or(Cover::None, Cover::Found));
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
        let mut images = Vec::new();
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
            } else if pictures::is_image(&path) {
                images.push(path);
            } else {
                ignored.push(file_name(&path));
            }
        }
        self.take_dropped_images(ctx, images);
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
        paint_disclosure(&painter, x0, cy, open);
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

/// The triangle that shows whether a row is open.
fn paint_disclosure(painter: &egui::Painter, x0: f32, cy: f32, open: bool) {
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
}

/// A text truncated to `width`, drawn with its left middle at `at`.
fn text_at(
    painter: &egui::Painter,
    at: egui::Pos2,
    text: &str,
    font: egui::FontId,
    color: Color32,
    width: f32,
) {
    let mut job = LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = TextWrapping::truncate_at_width(width.max(8.0));
    let g = painter.layout_job(job);
    painter.galley(pos2(at.x, at.y - g.size().y / 2.0), g, color);
}

/// A browser pack, bank or folder's picture: what it is for, and what it
/// shows until someone picks another.
pub fn pack_target(info: &PackInfo) -> Target {
    Target::Browser {
        key: pack_key(info),
        name: info.name.to_string(),
        default: pictures::pack_default(info),
    }
}

pub fn bank_target(info: &PackInfo, bank: &str) -> Target {
    Target::Browser {
        key: bank_key(info, bank),
        name: bank.to_string(),
        default: pictures::bank_default(info, bank),
    }
}

pub fn folder_target(path: &Path) -> Target {
    Target::Browser {
        key: folder_key(path),
        name: file_name(path),
        default: pictures::folder_default(path),
    }
}

/// What a row with a picture shows besides it.
struct RowText<'a> {
    text: &'a str,
    /// A second, smaller line under the name.
    sub: Option<&'a str>,
    detail: Option<(&'a str, Color32)>,
}

/// A row that holds others and shows a picture: a pack, bank or folder.
/// Clicking the picture changes it; clicking elsewhere opens or closes the
/// row. Returns whether it is open, after a click.
#[allow(clippy::too_many_arguments)]
fn picture_row(
    app: &mut App,
    ui: &mut Ui,
    key: &str,
    depth: usize,
    height: f32,
    words: RowText,
    target: Target,
    selected: bool,
    forced: bool,
) -> (bool, Response) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let resp = ui.interact(rect, row_id(key), Sense::click());
    if resp.clicked() && !forced {
        let open = app.browser.is_expanded(key);
        app.browser.set_expanded(key, !open);
    }
    let open = forced || app.browser.is_expanded(key);
    if app.browser.reveal.as_deref() == Some(key) {
        resp.scroll_to_me(Some(Align::TOP));
        app.browser.reveal = None;
    }
    if !ui.is_rect_visible(rect) {
        return (open, resp);
    }
    let in_row = ui.rect_contains_pointer(rect);
    {
        let painter = ui.painter_at(rect);
        if selected {
            painter.rect_filled(rect, CornerRadius::same(3), theme::ACCENT_BG);
        } else if in_row {
            painter.rect_filled(
                rect,
                CornerRadius::same(3),
                theme::lighten(theme::PANEL, 0.05),
            );
        }
    }
    let x0 = rect.left() + 4.0 + depth as f32 * INDENT;
    let cy = rect.center().y;
    paint_disclosure(&ui.painter_at(rect), x0, cy, open);
    let side = if height >= 30.0 {
        height - 8.0
    } else {
        height - 4.0
    };
    let pic = Rect::from_min_size(pos2(x0 + 12.0, cy - side / 2.0), vec2(side, side));
    if let Some(shown) = app.shown_of(&target) {
        let radius = (side * 0.2).round();
        pictures::button(
            app,
            ui,
            "browser",
            pic,
            radius,
            &shown,
            target,
            Style::Tile,
            in_row,
        );
    }
    let painter = ui.painter_at(rect);
    let x = pic.right() + 8.0;
    let mut right = rect.right() - 4.0;
    if let Some((detail, color)) = words.detail {
        let g = painter.layout_no_wrap(detail.to_string(), theme::small(), color);
        right -= g.size().x;
        painter.galley(pos2(right, cy - g.size().y / 2.0), g, color);
        right -= 8.0;
    }
    match words.sub {
        Some(sub) => {
            text_at(
                &painter,
                pos2(x, cy - 7.5),
                words.text,
                theme::body(),
                theme::TEXT,
                right - x,
            );
            text_at(
                &painter,
                pos2(x, cy + 8.0),
                sub,
                theme::small(),
                theme::TEXT_WEAK,
                right - x,
            );
        }
        None => text_at(
            &painter,
            pos2(x, cy),
            words.text,
            theme::body(),
            theme::TEXT,
            right - x,
        ),
    }
    (open, resp)
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
    let card = app.browser.card.and_then(packs::find);
    let room = if card.is_some() {
        // Last frame's height, or a guess on the first.
        let h = if app.browser.card_h > 0.0 {
            app.browser.card_h
        } else {
            200.0
        };
        (ui.available_height() - h).max(80.0)
    } else {
        f32::INFINITY
    };
    ScrollArea::vertical()
        .id_salt("browser_rows")
        .auto_shrink([false, false])
        .max_height(room)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            pool_section(app, ui, &project, &terms);
            packs_section(app, ui, &terms);
            folders_section(app, ui, &terms);
        });
    if let Some(info) = card {
        let top = ui.cursor().top();
        ui.add_space(6.0);
        pack_card(app, ui, info);
        app.browser.card_h = ui.cursor().top() - top;
    }
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
    let searching = !terms.is_empty();
    let (open, resp) = toggle_row(app, ui, "packs", 0, "Sample packs", None, searching);
    if open && !searching {
        view_buttons(app, ui, resp.rect);
    }
    if !open {
        return;
    }
    // A search lists what it finds, whichever view is picked.
    if app.browser.tiles && !searching {
        pack_tiles(app, ui);
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

/// A pack's context menu, in the list, the tiles and the card.
fn pack_menu(app: &mut App, ui: &mut Ui, info: &'static PackInfo, at: egui::Pos2) {
    let ctx = ui.ctx().clone();
    if ui.button("Load into a sample bank…").clicked() {
        app.open_load_sounds_on(&ctx, info, None);
    }
    ui.separator();
    app.picture_menu(ui, &pack_target(info), at);
}

/// The List and Tiles buttons at the right of the Sample packs row.
fn view_buttons(app: &mut App, ui: &mut Ui, row: Rect) {
    let size = vec2(20.0, 16.0);
    let cy = row.center().y;
    let tiles_rect = Rect::from_center_size(pos2(row.right() - 14.0, cy), size);
    let list_rect = tiles_rect.translate(vec2(-size.x - 4.0, 0.0));
    for (rect, tiles, label) in [
        (list_rect, false, "List view"),
        (tiles_rect, true, "Tiles view"),
    ] {
        let resp = ui.interact(rect, Id::new(("pack_view", tiles)), Sense::click());
        let on = app.browser.tiles == tiles;
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, label));
        let painter = ui.painter();
        if on || resp.hovered() {
            painter.rect_filled(
                rect,
                CornerRadius::same(3),
                if on {
                    theme::ACCENT_BG
                } else {
                    theme::lighten(theme::PANEL, 0.08)
                },
            );
        }
        let color = if on { theme::ACCENT } else { theme::TEXT_WEAK };
        let inner = rect.shrink2(vec2(5.0, 3.5));
        if tiles {
            let half = (inner.size() - vec2(2.0, 2.0)) / 2.0;
            for (dx, dy) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                let min = inner.min + vec2(dx * (half.x + 2.0), dy * (half.y + 2.0));
                painter.rect_filled(Rect::from_min_size(min, half), CornerRadius::same(1), color);
            }
        } else {
            for i in 0..3 {
                let y = inner.top() + 0.5 + i as f32 * (inner.height() - 1.0) / 2.0;
                painter.hline(inner.x_range(), y, Stroke::new(1.5, color));
            }
        }
        if resp.on_hover_text(label).clicked() {
            app.browser.tiles = tiles;
        }
    }
}

/// The sample packs as a grid of tiles.
fn pack_tiles(app: &mut App, ui: &mut Ui) {
    let gap = 8.0;
    let width = (ui.available_width() - 8.0).max(60.0);
    let cols = ((width + gap) / (TILE_W + gap)).floor().max(1.0) as usize;
    let w = (width - gap * (cols - 1) as f32) / cols as f32;
    let h = (w * 0.62).round().clamp(56.0, 132.0);
    ui.add_space(4.0);
    for row in PACKS.chunks(cols) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            ui.add_space(4.0);
            for info in row {
                pack_tile(app, ui, info, vec2(w, h));
            }
        });
        ui.add_space(gap);
    }
}

/// One pack's tile: its picture with the name and license under it.
/// Clicking shows its card, double-clicking opens it in the list, and the
/// pencil changes its picture.
fn pack_tile(app: &mut App, ui: &mut Ui, info: &'static PackInfo, size: egui::Vec2) {
    let label_h = 32.0;
    let (rect, _) = ui.allocate_exact_size(size + vec2(0.0, label_h), Sense::hover());
    let resp = ui.interact(rect, row_id(&format!("tile:{}", info.id)), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, info.name));
    if !ui.is_rect_visible(rect) {
        return;
    }
    let ctx = ui.ctx().clone();
    if app.browser.listing(info.map).is_some() {
        app.look_for_pack_cover(&ctx, info);
    }
    let image = Rect::from_min_size(rect.min, size);
    let target = pack_target(info);
    let shown = app
        .pictures
        .browser_shown(&pack_key(info), &pictures::pack_default(info));
    let painter = ui.painter();
    pictures::paint(
        painter,
        image,
        6.0,
        &shown,
        &mut app.pictures.images,
        Style::Tile,
    );
    let files = ctx.input(|i| !i.raw.hovered_files.is_empty());
    let over = ui.rect_contains_pointer(image);
    if over && files {
        app.pictures.hovered = Some(target.clone());
    }
    let selected = app.browser.card == Some(info.id);
    if selected || (over && files) || app.pictures.waiting.is_some() {
        painter.rect_stroke(
            image.expand(1.5),
            CornerRadius::same(8),
            Stroke::new(2.0, theme::ACCENT),
            StrokeKind::Outside,
        );
    }
    let pencil = image.right_top() + vec2(-14.0, 14.0);
    let on_pencil = resp.hover_pos().is_some_and(|p| p.distance(pencil) <= 12.0);
    if resp.hovered() {
        pictures::paint_pencil(painter, pencil, 10.0);
    }
    let text_w = size.x - 2.0;
    text_at(
        painter,
        pos2(rect.left() + 1.0, image.bottom() + 9.0),
        info.name,
        theme::body(),
        theme::TEXT,
        text_w,
    );
    text_at(
        painter,
        pos2(rect.left() + 1.0, image.bottom() + 24.0),
        info.license.label(),
        theme::small(),
        license_color(info.license),
        text_w,
    );
    if resp.double_clicked() {
        app.browser.card = Some(info.id);
        app.browser.reveal_pack(info);
    } else if resp.clicked() {
        if let Some(path) = app.pictures.waiting.take() {
            app.load_picture_file(&ctx, target, path);
        } else if on_pencil {
            app.open_picture_editor(target, pencil);
        } else {
            app.browser.card = Some(info.id);
        }
    }
    let at = image.right_top();
    let tip = if on_pencil {
        "Change the picture".to_string()
    } else {
        format!(
            "{}\n{}\nClick for its card, double-click to list its sounds",
            info.about,
            license_note(info.license)
        )
    };
    resp.on_hover_text(tip)
        .context_menu(|ui| pack_menu(app, ui, info, at));
}

/// The selected pack's card at the bottom of the browser: a banner, what
/// it holds and its license, and buttons for what to do with it.
fn pack_card(app: &mut App, ui: &mut Ui, info: &'static PackInfo) {
    let ctx = ui.ctx().clone();
    Frame::new()
        .fill(theme::HEADER)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let (banner, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::hover());
            let target = pack_target(info);
            let shown = app
                .pictures
                .browser_shown(&pack_key(info), &pictures::pack_default(info));
            let in_banner = ui.rect_contains_pointer(banner);
            let at = banner.right_top();
            pictures::button(
                app,
                ui,
                "card",
                banner,
                6.0,
                &shown,
                target.clone(),
                Style::Banner,
                in_banner,
            )
            .context_menu(|ui| pack_menu(app, ui, info, at));
            let painter = ui.painter();
            let white = Color32::WHITE;
            text_at(
                painter,
                pos2(banner.left() + 10.0, banner.top() + 22.0),
                info.name,
                egui::FontId::proportional(16.0),
                white,
                banner.width() * 0.62,
            );
            let held = match app.browser.listing(info.map) {
                Some(l) if !l.banks.is_empty() => format!("{} banks", l.banks.len()),
                Some(l) => format!("{} sounds", l.plain.len()),
                None => "Not read yet".to_string(),
            };
            text_at(
                painter,
                pos2(banner.left() + 10.0, banner.top() + 44.0),
                &held,
                theme::small(),
                Color32::from_white_alpha(210),
                banner.width() * 0.62,
            );
            let close =
                Rect::from_center_size(banner.right_top() + vec2(-12.0, 12.0), vec2(18.0, 18.0));
            let x = ui.interact(close, Id::new("pack_card_close"), Sense::click());
            x.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Close the pack card")
            });
            let bg = if x.hovered() {
                Color32::from_black_alpha(200)
            } else {
                Color32::from_black_alpha(120)
            };
            painter.circle_filled(close.center(), 9.0, bg);
            let s = 3.5;
            let c = close.center();
            for (a, b) in [(vec2(-s, -s), vec2(s, s)), (vec2(-s, s), vec2(s, -s))] {
                painter.line_segment([c + a, c + b], Stroke::new(1.5, white));
            }
            if x.on_hover_text("Close").clicked() {
                app.browser.card = None;
            }
            ui.add_space(2.0);
            ui.add(
                egui::Label::new(RichText::new(info.about).small().color(theme::TEXT_WEAK)).wrap(),
            );
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(info.license.label())
                        .small()
                        .color(license_color(info.license)),
                )
                .on_hover_text(license_note(info.license));
                ui.hyperlink_to(RichText::new("Homepage").small(), info.homepage);
            });
            ui.horizontal_wrapped(|ui| {
                if ui.button("Change picture…").clicked() {
                    app.open_picture_editor(target.clone(), banner.right_top());
                }
                if ui.button("Show sounds").clicked() {
                    app.browser.reveal_pack(info);
                }
                if ui
                    .button("Load…")
                    .on_hover_text("Load into a sample bank")
                    .clicked()
                {
                    app.open_load_sounds_on(&ctx, info, None);
                }
            });
        });
}

fn pack_rows(app: &mut App, ui: &mut Ui, info: &'static PackInfo, terms: &[String]) {
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
    if listing.is_some() {
        app.look_for_pack_cover(&ctx, info);
    }
    let selected = app.browser.card == Some(info.id);
    let (open, resp) = picture_row(
        app,
        ui,
        &key,
        1,
        BIG_ROW_H,
        RowText {
            text: info.name,
            sub: Some(info.about),
            detail: Some((info.license.label(), license_color(info.license))),
        },
        pack_target(info),
        selected,
        !hits.is_empty() || (searching && busy),
    );
    if resp.clicked() {
        app.browser.card = Some(info.id);
    }
    let at = resp.rect.right_top();
    let resp = resp.on_hover_text(format!("{}\n{}", info.about, license_note(info.license)));
    resp.context_menu(|ui| pack_menu(app, ui, info, at));
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
        let target = bank_target(info, bank);
        let (open, resp) = picture_row(
            app,
            ui,
            &bank_key(info, bank),
            2,
            BANK_ROW_H,
            RowText {
                text: bank,
                sub: None,
                detail: Some((&count, theme::TEXT_DIM)),
            },
            target.clone(),
            false,
            false,
        );
        let at = resp.rect.right_top();
        resp.context_menu(|ui| {
            if ui.button("Load this bank…").clicked() {
                app.open_load_sounds_on(&ctx, info, Some(bank));
            }
            ui.separator();
            app.picture_menu(ui, &target, at);
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
    let parent = root
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let target = folder_target(root);
    let (open, resp) = picture_row(
        app,
        ui,
        &folder_key(root),
        1,
        BIG_ROW_H,
        RowText {
            text: &name,
            sub: Some(&parent),
            detail: count.as_deref().map(|c| (c, theme::TEXT_DIM)),
        },
        target.clone(),
        false,
        !hits.is_empty() || (searching && busy),
    );
    let at = resp.rect.right_top();
    let resp = resp.on_hover_text(root.display().to_string());
    resp.context_menu(|ui| {
        if ui.button("Look through it again").clicked() {
            app.index_folder(&ctx, root);
        }
        if ui.button("Remove from the browser").clicked() {
            app.browser.remove_folder(root);
        }
        ui.separator();
        app.picture_menu(ui, &target, at);
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

    let (hovering, images) = ctx.input(|i| {
        let files = &i.raw.hovered_files;
        let images = files
            .iter()
            .all(|f| f.path.as_deref().is_some_and(pictures::is_image));
        (files.len(), images)
    });
    if hovering > 0 && images {
        // Images go on a picture, so the pictures stay in sight.
        let target = app
            .pictures
            .editor
            .as_ref()
            .map(|e| e.target.clone())
            .or_else(|| app.pictures.hovered.clone());
        let text = match target {
            Some(t) => format!("Drop to give {} this picture", app.target_name(&t)),
            None => "Drop the image on a picture: a pack, bank or folder in the browser, a \
                     track, or a mixer strip"
                .to_string(),
        };
        let screen = ctx.content_rect();
        let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("files_hover")));
        painter.rect_stroke(
            screen.shrink(4.0),
            CornerRadius::same(8),
            Stroke::new(2.0, theme::ACCENT.gamma_multiply(0.6)),
            StrokeKind::Inside,
        );
        let g = painter.layout_no_wrap(text, theme::body(), theme::TEXT);
        let pill = Rect::from_center_size(
            pos2(screen.center().x, screen.top() + 40.0),
            g.size() + vec2(24.0, 12.0),
        );
        painter.rect_filled(pill, CornerRadius::same(6), theme::BG.gamma_multiply(0.95));
        painter.rect_stroke(
            pill,
            CornerRadius::same(6),
            Stroke::new(1.0, theme::ACCENT),
            StrokeKind::Inside,
        );
        painter.galley(pill.center() - g.size() / 2.0, g, theme::TEXT);
    } else if hovering > 0 {
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
