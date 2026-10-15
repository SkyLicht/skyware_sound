// SPDX-License-Identifier: GPL-3.0-or-later
//! Dialogs: choosing paths (native dialogs with a typed fallback), unsaved
//! changes, loop and WAV export, and audio settings. The Load sounds dialog
//! is in [`crate::sounds`], the Game assets window in [`crate::game`].

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{
    Align, ComboBox, Context, Grid, Id, Key, Layout, Modal, RichText, TextEdit, Ui, Window,
};
use sky_core::Ticks;
use sky_core::manifest::{Asset, Category};
use sky_io::WavFormat;
use sky_io::device::OutputMode;
use sky_model::{LicenseReport, Origin};
use sky_session::LoopExportOptions;

use crate::app::{Action, App};
use crate::audio::{self, BUFFER_SIZES, SAMPLE_RATES};
use crate::audition::{PreviewKey, preview_button};
use crate::game::GameDialog;
use crate::jobs::JobOutput;
use crate::sounds::{LoadSoundsDialog, license_report_ui};
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathPurpose {
    OpenProject,
    SaveAs,
    ImportAudio,
    ExportLoopFolder,
    ExportWavFile,
    SoundPack,
    GameFolder,
}

impl PathPurpose {
    pub fn title(self) -> &'static str {
        match self {
            PathPurpose::OpenProject => "Open project",
            PathPurpose::SaveAs => "Save project as",
            PathPurpose::ImportAudio => "Import audio",
            PathPurpose::ExportLoopFolder => "Export folder",
            PathPurpose::ExportWavFile => "Export WAV to",
            PathPurpose::SoundPack => "Sound pack folder",
            PathPurpose::GameFolder => "Game export folder",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            PathPurpose::OpenProject => "A project folder ending in .skyproj",
            PathPurpose::SaveAs => "A new folder ending in .skyproj; it is created on save",
            PathPurpose::ImportAudio => "A WAV, FLAC, OGG or MP3 file",
            PathPurpose::ExportLoopFolder => {
                "The folder that holds skyware-manifest.json for your game"
            }
            PathPurpose::ExportWavFile => "A .wav file",
            PathPurpose::SoundPack => "A folder with a strudel.json sample map in it",
            PathPurpose::GameFolder => "Your game's audio folder, such as assets/audio",
        }
    }
}

/// The typed-path dialog.
pub struct PathPrompt {
    pub purpose: PathPurpose,
    pub text: String,
    pub error: Option<String>,
    focus: bool,
}

/// A native dialog is open on another thread.
pub struct NativeWait {
    pub purpose: PathPurpose,
    pub token: u64,
}

pub struct Confirm {
    pub then: Action,
}

pub struct LoopSummary {
    pub file: PathBuf,
    pub manifest: PathBuf,
    pub verdict: String,
    pub note: Option<String>,
    pub lufs: Option<f64>,
    pub peak_dbfs: Option<f64>,
    pub bars: f64,
    pub seconds: f64,
    /// Credits written into the manifest, and warnings.
    pub licenses: LicenseReport,
}

pub struct WavSummary {
    pub path: PathBuf,
    pub seconds: f64,
    pub peak_dbfs: f32,
    pub lufs: Option<f64>,
    pub licenses: LicenseReport,
}

pub struct ExportLoopDialog {
    pub name: String,
    pub folder: String,
    pub bits: u16,
    pub category: Category,
    /// What the sounds in the export ask for, as of when the dialog opened.
    pub licenses: LicenseReport,
    pub running: bool,
    pub result: Option<Result<LoopSummary, String>>,
}

pub struct ExportWavDialog {
    pub path: String,
    pub bits: u16,
    pub whole_song: bool,
    pub tail: f64,
    pub licenses: LicenseReport,
    pub running: bool,
    pub result: Option<Result<WavSummary, String>>,
}

pub struct SettingsDialog {
    pub devices: Option<Vec<String>>,
    pub device: Option<String>,
    pub buffer: Option<u32>,
    pub rate: Option<u32>,
    pub mode: OutputMode,
}

#[derive(Default)]
pub struct Dialogs {
    pub confirm: Option<Confirm>,
    pub path_prompt: Option<PathPrompt>,
    pub native: Option<NativeWait>,
    pub export_loop: Option<ExportLoopDialog>,
    pub export_wav: Option<ExportWavDialog>,
    pub game: Option<GameDialog>,
    pub load_sounds: Option<LoadSoundsDialog>,
    pub settings: Option<SettingsDialog>,
    /// An action to run once a "Save as" from the unsaved-changes dialog succeeds.
    pub after_save_as: Option<Action>,
    next_token: u64,
}

impl Dialogs {
    /// True while a modal dialog should get the keyboard instead of the app.
    pub fn blocks_input(&self) -> bool {
        self.confirm.is_some()
            || self.path_prompt.is_some()
            || self.native.is_some()
            || self.export_loop.is_some()
            || self.export_wav.is_some()
            || self.load_sounds.is_some()
    }
}

pub const CATEGORIES: [(Category, &str); 5] = [
    (Category::Music, "Music"),
    (Category::Sfx, "Sound effect"),
    (Category::Ui, "UI"),
    (Category::Voice, "Voice"),
    (Category::Ambience, "Ambience"),
];

fn format_of(bits: u16) -> WavFormat {
    WavFormat::from_bits(bits).unwrap_or(WavFormat::Pcm24)
}

/// Documents/Skyware Sound, or the home folder, or the current folder.
pub fn default_dir() -> PathBuf {
    let dirs = directories::UserDirs::new();
    let base = dirs
        .as_ref()
        .and_then(|d| d.document_dir().map(Path::to_path_buf))
        .or_else(|| dirs.as_ref().map(|d| d.home_dir().to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Skyware Sound")
}

/// A safe file or folder name from a project name.
pub fn file_name_for(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() {
        "Untitled".to_string()
    } else {
        cleaned
    }
}

/// Adds `ext` unless the path already ends with it.
pub fn with_extension(path: PathBuf, ext: &str) -> PathBuf {
    let has = path
        .extension()
        .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case(ext));
    if has {
        path
    } else {
        let mut s = path.into_os_string();
        s.push(".");
        s.push(ext);
        PathBuf::from(s)
    }
}

fn existing_dir(path: &Path) -> PathBuf {
    let mut p = path.to_path_buf();
    while !p.is_dir() {
        match p.parent() {
            Some(parent) if parent != p => p = parent.to_path_buf(),
            _ => return PathBuf::from("."),
        }
    }
    p
}

fn native_pick(purpose: PathPurpose, default: &Path) -> Option<PathBuf> {
    let (dir, name) = match purpose {
        PathPurpose::SaveAs | PathPurpose::ExportWavFile => (
            existing_dir(default.parent().unwrap_or(default)),
            default
                .file_name()
                .map(|n| n.to_string_lossy().into_owned()),
        ),
        _ => (existing_dir(default), None),
    };
    let mut d = rfd::FileDialog::new()
        .set_title(purpose.title())
        .set_directory(&dir);
    if let Some(n) = name {
        d = d.set_file_name(n);
    }
    match purpose {
        PathPurpose::OpenProject
        | PathPurpose::ExportLoopFolder
        | PathPurpose::SoundPack
        | PathPurpose::GameFolder => d.pick_folder(),
        PathPurpose::SaveAs => d.save_file(),
        PathPurpose::ImportAudio => d
            .add_filter("Audio", &["wav", "flac", "ogg", "mp3"])
            .pick_file(),
        PathPurpose::ExportWavFile => d.add_filter("WAV", &["wav"]).save_file(),
    }
}

impl App {
    fn default_path(&self, purpose: PathPurpose) -> PathBuf {
        let project = &self.view.project;
        let stem = file_name_for(&project.name);
        match purpose {
            PathPurpose::OpenProject | PathPurpose::ImportAudio => self
                .view
                .path
                .as_ref()
                .and_then(|p| p.parent().map(Path::to_path_buf))
                .unwrap_or_else(default_dir),
            PathPurpose::SaveAs => default_dir().join(format!("{stem}.skyproj")),
            PathPurpose::ExportLoopFolder => match &self.view.path {
                Some(p) => p.join("renders").join("export"),
                None => default_dir().join("export"),
            },
            PathPurpose::ExportWavFile => match &self.view.path {
                Some(p) => p.join("renders").join(format!("{stem}.wav")),
                None => default_dir().join(format!("{stem}.wav")),
            },
            PathPurpose::SoundPack => self
                .dialogs
                .load_sounds
                .as_ref()
                .map(|d| PathBuf::from(d.address.trim()))
                .filter(|p| p.is_dir())
                .unwrap_or_else(default_dir),
            PathPurpose::GameFolder => self
                .session
                .game_export_dir()
                .unwrap_or_else(|_| default_dir().join("export")),
        }
    }

    /// Asks for a path: a native dialog when available, else a typed path.
    pub fn request_path(&mut self, ctx: &Context, purpose: PathPurpose) {
        let default = self.default_path(purpose);
        if !self.native_dialogs {
            self.open_path_prompt(purpose, &default);
            return;
        }
        self.dialogs.next_token += 1;
        let token = self.dialogs.next_token;
        self.dialogs.native = Some(NativeWait { purpose, token });
        self.jobs.spawn(ctx, None, move || {
            let started = Instant::now();
            let path = native_pick(purpose, &default);
            JobOutput::PathPicked {
                purpose,
                token,
                path,
                elapsed: started.elapsed(),
            }
        });
    }

    fn open_path_prompt(&mut self, purpose: PathPurpose, default: &Path) {
        let mut text = default.display().to_string();
        if matches!(
            purpose,
            PathPurpose::OpenProject | PathPurpose::ImportAudio | PathPurpose::SoundPack
        ) && !text.ends_with(std::path::MAIN_SEPARATOR)
        {
            text.push(std::path::MAIN_SEPARATOR);
        }
        self.dialogs.path_prompt = Some(PathPrompt {
            purpose,
            text,
            error: None,
            focus: true,
        });
    }

    pub fn native_path_result(
        &mut self,
        ctx: &Context,
        purpose: PathPurpose,
        token: u64,
        path: Option<PathBuf>,
        elapsed: Duration,
    ) {
        if self.dialogs.native.as_ref().map(|n| n.token) != Some(token) {
            return;
        }
        self.dialogs.native = None;
        match path {
            Some(p) => {
                if let Some(e) = self.path_chosen(ctx, purpose, p) {
                    self.error(e);
                }
            }
            // Back too fast for a person to have cancelled: there is no
            // working native dialog here, so ask for a typed path.
            None if elapsed < Duration::from_millis(400) => {
                let default = self.default_path(purpose);
                self.open_path_prompt(purpose, &default);
            }
            None => {
                if purpose == PathPurpose::SaveAs {
                    self.dialogs.after_save_as = None;
                }
            }
        }
    }

    /// Acts on a chosen path. Returns an error to show in the typed-path dialog.
    pub fn path_chosen(
        &mut self,
        ctx: &Context,
        purpose: PathPurpose,
        path: PathBuf,
    ) -> Option<String> {
        match purpose {
            PathPurpose::OpenProject => {
                if !path.join(sky_model::file::PROJECT_FILE).is_file() {
                    let msg = format!(
                        "{} is not a Skyware Sound project folder (it has no {})",
                        path.display(),
                        sky_model::file::PROJECT_FILE
                    );
                    self.error(msg.clone());
                    return Some(msg);
                }
                let session = self.session.clone();
                self.jobs.spawn(ctx, Some("Opening".into()), move || {
                    JobOutput::Opened(
                        session
                            .open(&path)
                            .map(|n| (path.clone(), n))
                            .map_err(|e| e.to_string()),
                    )
                });
            }
            PathPurpose::SaveAs => {
                let path = with_extension(path, sky_model::file::EXTENSION);
                let is_project = path.join(sky_model::file::PROJECT_FILE).is_file();
                let busy = path.is_file()
                    || (path.is_dir()
                        && !is_project
                        && std::fs::read_dir(&path).is_ok_and(|mut d| d.next().is_some()));
                if busy {
                    let msg = format!(
                        "{} already exists and is not a Skyware Sound project; choose another name",
                        path.display()
                    );
                    self.error(msg.clone());
                    return Some(msg);
                }
                let then = self.dialogs.after_save_as.take();
                self.spawn_save(ctx, Some(path), then);
            }
            PathPurpose::ImportAudio => {
                if !path.is_file() {
                    let msg = format!("{} is not a file", path.display());
                    self.error(msg.clone());
                    return Some(msg);
                }
                let session = self.session.clone();
                let name = path
                    .file_stem()
                    .map_or_else(|| "audio".to_string(), |s| s.to_string_lossy().into_owned());
                self.jobs
                    .spawn(ctx, Some(format!("Importing {name}")), move || {
                        JobOutput::Imported {
                            result: session
                                .import_audio(&path, Origin::User)
                                .map_err(|e| e.to_string()),
                            name,
                        }
                    });
            }
            PathPurpose::ExportLoopFolder => {
                if let Some(d) = self.dialogs.export_loop.as_mut() {
                    d.folder = path.display().to_string();
                }
            }
            PathPurpose::ExportWavFile => {
                if let Some(d) = self.dialogs.export_wav.as_mut() {
                    d.path = with_extension(path, "wav").display().to_string();
                }
            }
            PathPurpose::GameFolder => {
                if let Some(d) = self.dialogs.game.as_mut() {
                    d.folder = path.display().to_string();
                }
            }
            PathPurpose::SoundPack => {
                if let Some(d) = self.dialogs.load_sounds.as_mut() {
                    d.pack = None;
                    d.address = path.display().to_string();
                    d.bank = None;
                    d.chosen.clear();
                }
                self.read_pack(ctx);
            }
        }
        None
    }

    pub fn open_export_loop(&mut self) {
        let folder = self
            .default_path(PathPurpose::ExportLoopFolder)
            .display()
            .to_string();
        self.dialogs.export_loop = Some(ExportLoopDialog {
            name: self.view.project.name.clone(),
            folder,
            bits: 24,
            category: Category::Music,
            licenses: self.session.license_report(),
            running: false,
            result: None,
        });
    }

    pub fn open_export_wav(&mut self) {
        let path = self.default_path(PathPurpose::ExportWavFile);
        self.dialogs.export_wav = Some(ExportWavDialog {
            path: path.display().to_string(),
            bits: 24,
            whole_song: true,
            tail: 2.0,
            licenses: self.session.license_report(),
            running: false,
            result: None,
        });
    }

    pub fn open_settings(&mut self, ctx: &Context) {
        let cfg = &self.audio.config;
        self.dialogs.settings = Some(SettingsDialog {
            devices: None,
            device: cfg.device.clone(),
            buffer: cfg.buffer_frames,
            rate: cfg.sample_rate,
            mode: cfg.mode,
        });
        self.spawn_device_list(ctx);
    }

    fn spawn_device_list(&mut self, ctx: &Context) {
        self.jobs.spawn(ctx, None, || {
            JobOutput::Devices(sky_io::device::list_output_devices())
        });
    }

    pub fn start_loop_export(&mut self, ctx: &Context) {
        let Some(d) = self.dialogs.export_loop.as_mut() else {
            return;
        };
        let folder = d.folder.trim().to_string();
        if folder.is_empty() {
            d.result = Some(Err("Choose a folder to export to".into()));
            return;
        }
        let opts = LoopExportOptions {
            name: Some(d.name.trim().to_string()).filter(|n| !n.is_empty()),
            dir: Some(PathBuf::from(folder)),
            range: None,
            category: d.category,
            tags: Vec::new(),
            format: format_of(d.bits),
        };
        d.running = true;
        d.result = None;
        let session = self.session.clone();
        self.jobs
            .spawn(ctx, Some("Exporting loop".into()), move || {
                let result = session
                    .export_loop(&opts)
                    .map_err(|e| e.to_string())
                    .map(|x| {
                        let (lufs, peak) = match &x.exported.asset {
                            Asset::Loop(l) => (l.loudness_lufs, l.peak_dbfs),
                            Asset::Sfx(_) => (None, None),
                        };
                        LoopSummary {
                            file: x.exported.file.clone(),
                            manifest: x.exported.manifest.clone(),
                            verdict: x.seam.verdict.to_string(),
                            note: x.seam.note.map(str::to_string),
                            lufs,
                            peak_dbfs: peak,
                            bars: x.bars,
                            seconds: x.frames as f64 / x.sample_rate.max(1) as f64,
                            licenses: x.licenses,
                        }
                    });
                JobOutput::LoopExported(result)
            });
    }

    pub fn start_wav_export(&mut self, ctx: &Context) {
        let project = self.view.project.clone();
        let Some(d) = self.dialogs.export_wav.as_mut() else {
            return;
        };
        let text = d.path.trim();
        if text.is_empty() {
            d.result = Some(Err("Choose a file to export to".into()));
            return;
        }
        let path = with_extension(PathBuf::from(text), "wav");
        d.path = path.display().to_string();
        let range: Option<(Ticks, Ticks)> = if d.whole_song {
            None
        } else {
            Some((project.loop_region.start, project.loop_region.end))
        };
        let tail = d.tail;
        let format = format_of(d.bits);
        d.running = true;
        d.result = None;
        let session = self.session.clone();
        self.jobs.spawn(ctx, Some("Exporting WAV".into()), move || {
            let result = session
                .export_wav(&path, range, tail, format)
                .map_err(|e| e.to_string())
                .map(|audio| {
                    let stats = sky_dsp::analysis::analyze(&audio);
                    WavSummary {
                        path: path.clone(),
                        seconds: audio.duration_seconds(),
                        peak_dbfs: stats.peak_dbfs,
                        lufs: stats.integrated_lufs,
                        licenses: session.license_report(),
                    }
                });
            JobOutput::WavExported(result)
        });
    }
}

/// Shows whichever dialogs are open.
pub fn show(app: &mut App, ctx: &Context) {
    settings_window(app, ctx);
    export_loop_dialog(app, ctx);
    export_wav_dialog(app, ctx);
    crate::game::window(app, ctx);
    crate::sounds::load_sounds_dialog(app, ctx);
    confirm_dialog(app, ctx);
    path_prompt(app, ctx);
    native_wait(app, ctx);
}

fn native_wait(app: &mut App, ctx: &Context) {
    let Some(wait) = &app.dialogs.native else {
        return;
    };
    let purpose = wait.purpose;
    let mut typed = false;
    let mut cancel = false;
    Modal::new(Id::new("native_wait")).show(ctx, |ui| {
        ui.set_width(360.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new(purpose.title()).strong());
        });
        ui.label(RichText::new("Choose in the file dialog that opened.").color(theme::TEXT_WEAK));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Type a path instead").clicked() {
                typed = true;
            }
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
        });
    });
    if typed {
        app.dialogs.native = None;
        let default = app.default_path(purpose);
        app.open_path_prompt(purpose, &default);
    } else if cancel {
        app.dialogs.native = None;
    }
}

fn path_prompt(app: &mut App, ctx: &Context) {
    let Some(prompt) = app.dialogs.path_prompt.as_mut() else {
        return;
    };
    let purpose = prompt.purpose;
    let mut ok = false;
    let mut cancel = false;
    let modal = Modal::new(Id::new("path_prompt")).show(ctx, |ui| {
        ui.set_width(560.0);
        ui.label(RichText::new(purpose.title()).heading());
        ui.label(RichText::new(purpose.hint()).color(theme::TEXT_WEAK));
        ui.add_space(4.0);
        let resp = ui.add(
            TextEdit::singleline(&mut prompt.text)
                .desired_width(f32::INFINITY)
                .font(theme::mono(13.0)),
        );
        if prompt.focus {
            resp.request_focus();
            prompt.focus = false;
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            ok = true;
        }
        if let Some(e) = &prompt.error {
            ui.label(RichText::new(e).color(theme::ERROR));
        }
        ui.add_space(6.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
            let label = match purpose {
                PathPurpose::OpenProject => "Open",
                PathPurpose::SaveAs => "Save",
                PathPurpose::ImportAudio => "Import",
                _ => "Choose",
            };
            if ui.button(RichText::new(label).strong()).clicked() {
                ok = true;
            }
        });
    });
    if modal.should_close() {
        cancel = true;
    }
    if cancel {
        app.dialogs.path_prompt = None;
        if purpose == PathPurpose::SaveAs {
            app.dialogs.after_save_as = None;
        }
        return;
    }
    if ok {
        let text = prompt.text.trim().to_string();
        if text.is_empty() {
            prompt.error = Some("Type a path".into());
            return;
        }
        let path = PathBuf::from(text);
        app.dialogs.path_prompt = None;
        if let Some(err) = app.path_chosen(ctx, purpose, path.clone()) {
            app.dialogs.path_prompt = Some(PathPrompt {
                purpose,
                text: path.display().to_string(),
                error: Some(err),
                focus: true,
            });
        }
    }
}

fn confirm_dialog(app: &mut App, ctx: &Context) {
    let Some(confirm) = &app.dialogs.confirm else {
        return;
    };
    let then = confirm.then.clone();
    let what = match then {
        Action::Quit => "before closing",
        Action::Open => "before opening another project",
        _ => "before starting a new project",
    };
    let name = app.view.project.name.clone();
    let has_path = app.view.path.is_some();
    let mut choice = None;
    let modal = Modal::new(Id::new("confirm_discard")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.label(RichText::new("Unsaved changes").heading());
        ui.label(format!("Save the changes to “{name}” {what}?"));
        ui.add_space(8.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Cancel").clicked() {
                choice = Some(0);
            }
            if ui
                .button(RichText::new("Don't save").color(theme::ERROR))
                .clicked()
            {
                choice = Some(1);
            }
            let save = if has_path { "Save" } else { "Save as…" };
            if ui.button(RichText::new(save).strong()).clicked() {
                choice = Some(2);
            }
        });
    });
    if modal.should_close() && choice.is_none() {
        choice = Some(0);
    }
    match choice {
        Some(0) => app.dialogs.confirm = None,
        Some(1) => {
            app.dialogs.confirm = None;
            app.replace_project(ctx, then);
        }
        Some(2) => {
            app.dialogs.confirm = None;
            if has_path {
                app.spawn_save(ctx, None, Some(then));
            } else {
                app.dialogs.after_save_as = Some(then);
                app.request_path(ctx, PathPurpose::SaveAs);
            }
        }
        _ => {}
    }
}

pub(crate) fn bits_choice(ui: &mut Ui, bits: &mut u16) {
    ui.horizontal(|ui| {
        ui.radio_value(bits, 16, "16-bit");
        ui.radio_value(bits, 24, "24-bit");
        ui.radio_value(bits, 32, "32-bit float");
    });
}

fn export_loop_dialog(app: &mut App, ctx: &Context) {
    let play = PreviewKey::LoopRegion { join: false };
    let join = PreviewKey::LoopRegion { join: true };
    let (play_state, join_state) = (app.preview_state(&play), app.preview_state(&join));
    let Some(d) = app.dialogs.export_loop.as_mut() else {
        return;
    };
    let project = app.view.project.clone();
    let mut browse = false;
    let mut run = false;
    let mut close = false;
    let mut preview = None;
    let modal = Modal::new(Id::new("export_loop")).show(ctx, |ui| {
        ui.set_width(560.0);
        ui.label(RichText::new("Export loop").heading());
        let bars = project.loop_region.length() as f64 / project.ticks_per_bar().max(1) as f64;
        ui.label(
            RichText::new(format!(
                "Renders the loop region (bar {} to {}, {bars:.2} bars) so it repeats without a seam, \
                 and records it in the folder's manifest for sky-runtime.",
                project.ticks_to_bar(project.loop_region.start),
                project.ticks_to_bar(project.loop_region.end),
            ))
            .small()
            .color(theme::TEXT_WEAK),
        );
        ui.add_space(4.0);
        ui.add_enabled_ui(!d.running, |ui| {
            Grid::new("export_loop_grid")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Name");
                    ui.add(TextEdit::singleline(&mut d.name).desired_width(360.0));
                    ui.end_row();
                    ui.label("Folder");
                    ui.horizontal(|ui| {
                        ui.add(
                            TextEdit::singleline(&mut d.folder)
                                .desired_width(300.0)
                                .hint_text("Choose a folder"),
                        );
                        if ui.button("Browse…").clicked() {
                            browse = true;
                        }
                    });
                    ui.end_row();
                    ui.label("Format");
                    bits_choice(ui, &mut d.bits);
                    ui.end_row();
                    ui.label("Category");
                    let current = CATEGORIES
                        .iter()
                        .find(|(c, _)| *c == d.category)
                        .map_or("Music", |(_, l)| *l);
                    ComboBox::from_id_salt("export_category")
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            for (c, label) in CATEGORIES {
                                ui.selectable_value(&mut d.category, c, label);
                            }
                        });
                    ui.end_row();
                });
        });
        ui.add_space(6.0);
        let licenses = match &d.result {
            Some(Ok(s)) => &s.licenses,
            _ => &d.licenses,
        };
        license_report_ui(ui, licenses, "These credits go into the manifest:");
        match &d.result {
            Some(Ok(s)) => {
                ui.label(RichText::new("Exported").strong().color(theme::OK));
                Grid::new("export_loop_result")
                    .num_columns(2)
                    .spacing([10.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("File");
                        ui.label(RichText::new(s.file.display().to_string()).monospace());
                        ui.end_row();
                        ui.label("Manifest");
                        ui.label(RichText::new(s.manifest.display().to_string()).monospace());
                        ui.end_row();
                        ui.label("Seam");
                        let c = if s.verdict == "seamless" {
                            theme::OK
                        } else {
                            theme::WARN
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&s.verdict).color(c));
                            if let Some(n) = &s.note {
                                ui.label(RichText::new(n).small().color(theme::TEXT_WEAK));
                            }
                        });
                        ui.end_row();
                        ui.label("Loudness");
                        let lufs = s
                            .lufs
                            .map_or_else(|| "n/a".to_string(), |l| format!("{l:.1} LUFS"));
                        let peak = s
                            .peak_dbfs
                            .map_or_else(String::new, |p| format!(", peak {p:.1} dBFS"));
                        ui.label(format!("{lufs}{peak}"));
                        ui.end_row();
                        ui.label("Length");
                        ui.label(format!("{:.2} bars, {:.2} s", s.bars, s.seconds));
                        ui.end_row();
                    });
            }
            Some(Err(e)) => {
                ui.label(RichText::new(e).color(theme::ERROR));
            }
            None => {}
        }
        ui.add_space(6.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Close").clicked() {
                close = true;
            }
            if d.running {
                ui.spinner();
                ui.label(RichText::new("Rendering…").color(theme::TEXT_WEAK));
            } else if ui.button(RichText::new("Export").strong()).clicked() {
                run = true;
            }
            ui.separator();
            if preview_button(
                ui,
                "Join",
                "Play from just before the end, to hear the loop wrap around to its start",
                join_state,
            ) {
                preview = Some(join.clone());
            }
            if preview_button(
                ui,
                "Play",
                "Play the loop as it exports, repeating until stopped",
                play_state,
            ) {
                preview = Some(play.clone());
            }
        });
    });
    if modal.should_close() && !app.dialogs.export_loop.as_ref().is_some_and(|d| d.running) {
        close = true;
    }
    if close {
        app.dialogs.export_loop = None;
    } else if let Some(key) = preview {
        app.toggle_preview(ctx, key);
    } else if browse {
        app.request_path(ctx, PathPurpose::ExportLoopFolder);
    } else if run {
        app.start_loop_export(ctx);
    }
}

fn export_wav_dialog(app: &mut App, ctx: &Context) {
    let Some(d) = app.dialogs.export_wav.as_mut() else {
        return;
    };
    let mut browse = false;
    let mut run = false;
    let mut close = false;
    let modal = Modal::new(Id::new("export_wav")).show(ctx, |ui| {
        ui.set_width(560.0);
        ui.label(RichText::new("Export WAV").heading());
        ui.add_enabled_ui(!d.running, |ui| {
            Grid::new("export_wav_grid")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label("File");
                    ui.horizontal(|ui| {
                        ui.add(TextEdit::singleline(&mut d.path).desired_width(300.0));
                        if ui.button("Browse…").clicked() {
                            browse = true;
                        }
                    });
                    ui.end_row();
                    ui.label("Range");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut d.whole_song, true, "Whole song");
                        ui.radio_value(&mut d.whole_song, false, "Loop region");
                    });
                    ui.end_row();
                    ui.label("Tail");
                    ui.add(
                        egui::DragValue::new(&mut d.tail)
                            .range(0.0..=30.0)
                            .speed(0.05)
                            .suffix(" s"),
                    )
                    .on_hover_text("Extra time after the end, for reverb and delay tails");
                    ui.end_row();
                    ui.label("Format");
                    bits_choice(ui, &mut d.bits);
                    ui.end_row();
                });
        });
        ui.add_space(6.0);
        let licenses = match &d.result {
            Some(Ok(s)) => &s.licenses,
            _ => &d.licenses,
        };
        license_report_ui(ui, licenses, "Credit these wherever the audio is used:");
        match &d.result {
            Some(Ok(s)) => {
                ui.label(RichText::new("Exported").strong().color(theme::OK));
                ui.label(RichText::new(s.path.display().to_string()).monospace());
                let lufs = s
                    .lufs
                    .map_or_else(|| "n/a".to_string(), |l| format!("{l:.1} LUFS"));
                ui.label(format!(
                    "{:.2} s · {lufs} · peak {:.1} dBFS",
                    s.seconds, s.peak_dbfs
                ));
            }
            Some(Err(e)) => {
                ui.label(RichText::new(e).color(theme::ERROR));
            }
            None => {}
        }
        ui.add_space(6.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Close").clicked() {
                close = true;
            }
            if d.running {
                ui.spinner();
                ui.label(RichText::new("Rendering…").color(theme::TEXT_WEAK));
            } else if ui.button(RichText::new("Export").strong()).clicked() {
                run = true;
            }
        });
    });
    if modal.should_close() && !app.dialogs.export_wav.as_ref().is_some_and(|d| d.running) {
        close = true;
    }
    if close {
        app.dialogs.export_wav = None;
    } else if browse {
        app.request_path(ctx, PathPurpose::ExportWavFile);
    } else if run {
        app.start_wav_export(ctx);
    }
}

fn settings_window(app: &mut App, ctx: &Context) {
    if app.dialogs.settings.is_none() {
        return;
    }
    let mut open = true;
    let mut apply = false;
    let mut refresh = false;
    let status = app.audio.describe();
    let note = app.audio.note.clone();
    let running = app.audio.stream.is_some();
    Window::new("Audio settings")
        .id(Id::new("audio_settings"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .show(ctx, |ui| {
            let Some(s) = app.dialogs.settings.as_mut() else {
                return;
            };
            Grid::new("audio_settings_grid")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Output device");
                    ui.horizontal(|ui| {
                        let current = s.device.clone().unwrap_or_else(|| "System default".into());
                        ComboBox::from_id_salt("audio_device")
                            .width(280.0)
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut s.device, None, "System default");
                                match &s.devices {
                                    Some(list) => {
                                        for name in list {
                                            ui.selectable_value(&mut s.device, Some(name.clone()), name);
                                        }
                                    }
                                    None => {
                                        ui.label(RichText::new("Looking for devices…").color(theme::TEXT_WEAK));
                                    }
                                }
                            });
                        if s.devices.is_none() {
                            ui.spinner();
                        } else if ui.small_button("Refresh").clicked() {
                            refresh = true;
                        }
                    });
                    ui.end_row();
                    ui.label("Sample rate");
                    ComboBox::from_id_salt("audio_rate")
                        .selected_text(audio::rate_label(s.rate))
                        .show_ui(ui, |ui| {
                            for r in SAMPLE_RATES {
                                ui.selectable_value(&mut s.rate, r, audio::rate_label(r));
                            }
                        });
                    ui.end_row();
                    ui.label("Buffer size");
                    ComboBox::from_id_salt("audio_buffer")
                        .selected_text(audio::buffer_label(s.buffer))
                        .show_ui(ui, |ui| {
                            for b in BUFFER_SIZES {
                                ui.selectable_value(&mut s.buffer, b, audio::buffer_label(b));
                            }
                        });
                    ui.end_row();
                    ui.label("Mode");
                    ComboBox::from_id_salt("audio_mode")
                        .selected_text(audio::mode_label(s.mode))
                        .show_ui(ui, |ui| {
                            for m in OutputMode::ALL {
                                ui.selectable_value(&mut s.mode, m, audio::mode_label(m))
                                    .on_hover_text(audio::mode_tip(m));
                            }
                        })
                        .response
                        .on_hover_text(audio::mode_tip(s.mode));
                    ui.end_row();
                    ui.label("Now");
                    ui.label(RichText::new(&status).color(if running { theme::TEXT_WEAK } else { theme::WARN }));
                    ui.end_row();
                });
            if let Some(n) = &note {
                ui.label(RichText::new(n).small().color(theme::WARN));
            }
            ui.label(
                RichText::new("Smaller buffers respond faster but need more CPU; raise it if you hear dropouts.")
                    .small()
                    .color(theme::TEXT_DIM),
            );
            ui.add_space(4.0);
            if ui
                .button("Restart audio")
                .on_hover_text("Changes apply at once; this reopens the device as it is set")
                .clicked()
            {
                apply = true;
            }
        });
    // A choice that differs from the running setup restarts audio at once.
    let cfg = &app.audio.config;
    if app.dialogs.settings.as_ref().is_some_and(|s| {
        (&s.device, s.rate, s.buffer, s.mode)
            != (&cfg.device, cfg.sample_rate, cfg.buffer_frames, cfg.mode)
    }) {
        apply = true;
    }
    if refresh && let Some(s) = app.dialogs.settings.as_mut() {
        s.devices = None;
        app.spawn_device_list(ctx);
    }
    if apply && let Some(s) = app.dialogs.settings.as_ref() {
        app.audio.config.device = s.device.clone();
        app.audio.config.sample_rate = s.rate;
        app.audio.config.buffer_frames = s.buffer;
        app.audio.config.mode = s.mode;
        app.start_audio();
        if app.audio.stream.is_some() {
            app.info(format!("Audio: {}", app.audio.describe()));
        } else {
            app.error(app.audio.describe());
        }
    }
    if !open {
        app.dialogs.settings = None;
    }
}
