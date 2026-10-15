// SPDX-License-Identifier: GPL-3.0-or-later
//! The application state and the per-frame layout.

use std::sync::Arc;
use std::time::Duration;

use egui::{
    Align, CentralPanel, Context, Frame, Key, KeyboardShortcut, Layout, Margin, Modifiers, Panel,
    RichText, Ui, ViewportCommand,
};
use sky_core::{ClipId, NoteId, ProjectId, TrackId};
use sky_dsp::{EffectType, InstrumentType};
use sky_model::templates::{acid_project, demo_project, empty_project, techno_project};
use sky_model::{ChannelRef, ClipContent, Project};
use sky_session::{ProjectView, Session, TransportState};

use crate::actions::{self, NewTrack};
use crate::ai_panel::AiState;
use crate::arrange::ArrangeState;
use crate::audio::AudioState;
use crate::audition::Audition;
use crate::browser::BrowserState;
use crate::dialogs::{Confirm, Dialogs, PathPurpose};
use crate::jobs::{JobOutput, Jobs};
use crate::mixer::MixerState;
use crate::palette::PaletteState;
use crate::pattern_editor::PatternState;
use crate::piano_roll::PianoState;
use crate::step_editor::StepState;
use crate::waveform::{Peaks, WaveformCache};
use crate::widgets::Gestures;
use crate::{McpInfo, UiConfig, theme};

pub const WINDOW_TITLE: &str = "Skyware Sound";

/// Everything the palette, menus, buttons and shortcuts can ask for.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    AddTrack(NewTrack),
    AddEffect {
        channel: ChannelRef,
        kind: EffectType,
    },
    NewEmpty,
    NewDemo,
    NewTechno,
    NewAcid,
    Open,
    Save,
    SaveAs,
    ImportAudio,
    /// Load sounds from a sample pack into the selected sample bank, or a
    /// new one.
    LoadSounds,
    /// Show or hide the browser.
    ToggleBrowser,
    /// Add a folder of samples to the browser.
    AddBrowserFolder,
    ExportLoop,
    ExportWav,
    /// The Game assets window: loops and SFX sets for a game, and their
    /// export.
    GameAssets,
    ToggleLoop,
    PlayStop,
    Undo,
    Redo,
    SetTempo(f64),
    /// Ask for a tempo in the palette.
    PromptTempo,
    OpenSettings,
    OpenPalette,
    DeleteSelection,
    Quit,
}

/// The tabs of the bottom panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Editor,
    Mixer,
    Instrument,
    Ai,
}

/// Which area the Delete key acts on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Focus {
    #[default]
    Arrange,
    Editor,
}

#[derive(Default)]
pub struct Selection {
    pub track: Option<TrackId>,
    pub clip: Option<ClipId>,
    /// Selected notes of the selected MIDI clip.
    pub notes: Vec<NoteId>,
    pub focus: Focus,
}

impl Selection {
    pub fn select_track(&mut self, id: TrackId) {
        if self.track != Some(id) {
            self.clip = None;
            self.notes.clear();
        }
        self.track = Some(id);
        self.focus = Focus::Arrange;
    }

    pub fn select_clip(&mut self, track: TrackId, clip: ClipId) {
        if self.clip != Some(clip) {
            self.notes.clear();
        }
        self.track = Some(track);
        self.clip = Some(clip);
        self.focus = Focus::Arrange;
    }

    /// Drops anything that no longer exists.
    fn validate(&mut self, project: &Project) {
        if let Some(t) = self.track
            && project.track(t).is_err()
        {
            self.track = None;
        }
        if let Some(c) = self.clip {
            match project.find_clip(c) {
                Ok((track, clip)) => {
                    self.track = Some(track.id);
                    if let ClipContent::Midi { notes } = &clip.content {
                        self.notes.retain(|id| notes.iter().any(|n| n.id == *id));
                    } else {
                        self.notes.clear();
                    }
                }
                Err(_) => {
                    self.clip = None;
                    self.notes.clear();
                }
            }
        }
    }
}

/// A short message in the status bar.
pub struct Notice {
    pub text: String,
    pub error: bool,
    pub until: f64,
}

pub struct App {
    pub session: Session,
    pub mcp: Option<McpInfo>,
    pub audio: AudioState,
    /// Use the system file dialogs; off in tests.
    pub native_dialogs: bool,
    pub view: Arc<ProjectView>,
    pub transport: TransportState,
    pub sel: Selection,
    pub arrange: ArrangeState,
    pub piano: PianoState,
    pub pattern: PatternState,
    pub steps: StepState,
    pub mixer: MixerState,
    pub ai: AiState,
    pub audition: Audition,
    pub browser: BrowserState,
    pub palette: PaletteState,
    pub dialogs: Dialogs,
    pub jobs: Jobs,
    pub gestures: Gestures,
    pub waveforms: WaveformCache,
    pub tab: Tab,
    pub notice: Option<Notice>,
    /// Seconds since start, from egui.
    pub now: f64,
    project_id: ProjectId,
    pool_len: usize,
    title: String,
    allow_close: bool,
}

impl App {
    pub fn new(session: Session, config: UiConfig, native_dialogs: bool) -> Self {
        if let Some(n) = config.demo_tracks {
            session.new_project(actions::stress_project(n));
        }
        let view = session.view();
        let transport = session.transport();
        let project_id = view.project.id;
        let pool_len = view.project.audio_pool.len();
        Self {
            audio: AudioState::new(config.output),
            mcp: config.mcp,
            native_dialogs,
            transport,
            sel: Selection::default(),
            arrange: ArrangeState::default(),
            piano: PianoState::default(),
            pattern: PatternState::default(),
            steps: StepState::default(),
            mixer: MixerState::default(),
            ai: AiState::new(&view),
            audition: Audition::default(),
            browser: BrowserState::new(config.browser_store),
            palette: PaletteState::default(),
            dialogs: Dialogs::default(),
            jobs: Jobs::default(),
            gestures: Gestures::default(),
            waveforms: WaveformCache::default(),
            tab: Tab::default(),
            notice: None,
            now: 0.0,
            project_id,
            pool_len,
            title: String::new(),
            allow_close: false,
            view,
            session,
        }
    }

    pub fn start_audio(&mut self) {
        self.audio.start(&self.session);
        self.view = self.session.view();
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            error: false,
            until: self.now + 6.0,
        });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        let text = text.into();
        tracing::warn!("{text}");
        self.notice = Some(Notice {
            text,
            error: true,
            until: self.now + 10.0,
        });
    }

    /// Shows the error of a failed edit.
    pub fn report<T>(&mut self, result: Result<T, String>) -> Option<T> {
        match result {
            Ok(v) => Some(v),
            Err(e) => {
                self.error(e);
                None
            }
        }
    }

    /// Re-reads the session after an edit so the rest of the frame sees it.
    pub fn refresh(&mut self) {
        self.view = self.session.view();
        self.sel.validate(&self.view.project);
    }

    /// One frame of the whole interface.
    pub fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.begin_frame(&ctx);
        self.shortcuts(&ctx);
        self.take_dropped_files(&ctx);

        Panel::top("transport")
            .frame(
                Frame::new()
                    .fill(theme::BG)
                    .inner_margin(Margin::symmetric(10, 6)),
            )
            .show(ui, |ui| crate::transport::bar(self, ui));
        if self.audio.is_lost() {
            Panel::top("audio_lost")
                .frame(
                    Frame::new()
                        .fill(theme::darken(theme::ERROR, 0.6))
                        .inner_margin(Margin::symmetric(10, 4)),
                )
                .show(ui, |ui| self.audio_lost_banner(ui));
        }
        Panel::bottom("status")
            .frame(
                Frame::new()
                    .fill(theme::BG)
                    .inner_margin(Margin::symmetric(10, 3)),
            )
            .show(ui, |ui| self.status_bar(ui));
        if self.browser.open {
            Panel::left("browser")
                .resizable(true)
                .default_size(260.0)
                .size_range(180.0..=480.0)
                .frame(
                    Frame::new()
                        .fill(theme::PANEL)
                        .inner_margin(Margin::symmetric(6, 6)),
                )
                .show(ui, |ui| crate::browser::panel(self, ui));
        }
        // Leave the arrangement room for a few tracks in a small window.
        let height = ui.available_height();
        Panel::bottom("editor")
            .resizable(true)
            .default_size((height * 0.4).clamp(150.0, 330.0))
            .size_range(150.0..=(height - 200.0).max(150.0))
            .frame(
                Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(Margin::symmetric(8, 6)),
            )
            .show(ui, |ui| self.bottom_panel(ui));
        CentralPanel::default()
            .frame(Frame::new().fill(theme::BG).inner_margin(Margin::same(0)))
            .show(ui, |ui| crate::arrange::show(self, ui));

        crate::mixer::effect_window(self, &ctx);
        crate::dialogs::show(self, &ctx);
        crate::palette::show(self, &ctx);
        crate::browser::overlays(self, &ctx);
        self.end_frame(&ctx);
    }

    fn begin_frame(&mut self, ctx: &Context) {
        self.now = ctx.input(|i| i.time);
        self.gestures.begin_frame(ctx);
        for out in self.jobs.poll() {
            self.handle_job(ctx, out);
        }
        self.view = self.session.view();
        self.transport = self.session.transport();
        let project = self.view.project.clone();
        if project.id != self.project_id {
            // A different project: nothing selected or cached carries over.
            self.project_id = project.id;
            self.stop_preview();
            self.audition.reset();
            self.sel = Selection::default();
            self.arrange.reset_for_new_project();
            self.piano = PianoState::default();
            self.pattern = PatternState::default();
            self.mixer.open_effect = None;
            self.waveforms.clear();
        }
        if project.audio_pool.len() != self.pool_len {
            self.pool_len = project.audio_pool.len();
            self.waveforms.clear();
        }
        self.sel.validate(&project);
        self.tidy_auditions(ctx);
        self.update_meters(ctx, &project);
        self.ai.observe(&self.view, self.now);
        if self.notice.as_ref().is_some_and(|n| n.until < self.now) {
            self.notice = None;
        }

        let title = format!(
            "{WINDOW_TITLE} — {}{}",
            project.name,
            if self.view.dirty { " •" } else { "" }
        );
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.view.dirty && !self.allow_close {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.dialogs.confirm = Some(Confirm { then: Action::Quit });
        }
    }

    fn end_frame(&mut self, ctx: &Context) {
        let fast = self.transport.playing
            || self.arrange.drag.is_some()
            || self.mixer.any_active()
            || self.ai.flashing(self.now)
            || self.jobs.is_busy()
            || self.dialogs.native.is_some();
        // While idle, still look for changes an AI makes now and then.
        ctx.request_repaint_after(if fast {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(250)
        });
    }

    fn update_meters(&mut self, ctx: &Context, project: &Project) {
        let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
        let shared = self.session.engine_shared();
        let master = shared.as_ref().map_or((0.0, 0.0), |s| s.master.take());
        self.mixer.master.update(master, dt);
        if self.mixer.meters.len() != project.tracks.len() {
            self.mixer
                .meters
                .retain(|id, _| project.tracks.iter().any(|t| t.id == *id));
        }
        for (i, t) in project.tracks.iter().enumerate() {
            let peaks = shared
                .as_ref()
                .and_then(|s| s.track_meter(i))
                .map_or((0.0, 0.0), |m| m.take());
            self.mixer.meters.entry(t.id).or_default().update(peaks, dt);
        }
    }

    fn shortcuts(&mut self, ctx: &Context) {
        if self.palette.open || self.dialogs.blocks_input() {
            return;
        }
        let text_focus = ctx.text_edit_focused();
        let cmd = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let cmd_shift = |key| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, key);
        let mut wanted = Vec::new();
        ctx.input_mut(|i| {
            if i.consume_shortcut(&cmd(Key::K)) {
                wanted.push(Action::OpenPalette);
            }
            if i.consume_shortcut(&cmd_shift(Key::S)) {
                wanted.push(Action::SaveAs);
            } else if i.consume_shortcut(&cmd(Key::S)) {
                wanted.push(Action::Save);
            }
            if i.consume_shortcut(&cmd(Key::O)) {
                wanted.push(Action::Open);
            }
            if i.consume_shortcut(&cmd(Key::B)) {
                wanted.push(Action::ToggleBrowser);
            }
            if text_focus {
                return;
            }
            if i.consume_shortcut(&cmd_shift(Key::Z)) || i.consume_shortcut(&cmd(Key::Y)) {
                wanted.push(Action::Redo);
            } else if i.consume_shortcut(&cmd(Key::Z)) {
                wanted.push(Action::Undo);
            }
            if i.consume_key(Modifiers::NONE, Key::Space) {
                wanted.push(Action::PlayStop);
            }
            if i.consume_key(Modifiers::NONE, Key::Delete)
                || i.consume_key(Modifiers::NONE, Key::Backspace)
            {
                wanted.push(Action::DeleteSelection);
            }
        });
        for action in wanted {
            self.run_action(ctx, action);
        }
    }

    /// Runs an action from a button, the palette or a shortcut.
    pub fn run_action(&mut self, ctx: &Context, action: Action) {
        match action {
            Action::AddTrack(kind) => {
                let r = actions::add_track(&self.session, kind, self.sel.track);
                if let Some(id) = self.report(r) {
                    self.refresh();
                    self.sel.select_track(id);
                    if matches!(kind, NewTrack::Instrument(_)) {
                        self.tab = Tab::Instrument;
                    }
                    // A sample bank does nothing until it has sounds.
                    if kind == NewTrack::Instrument(InstrumentType::Bank) {
                        self.open_load_sounds(Some(id));
                    }
                }
            }
            Action::AddEffect { channel, kind } => {
                let r = actions::add_effect(&self.session, channel, kind);
                if let Some(id) = self.report(r) {
                    self.mixer.open_effect = Some((channel, id));
                }
            }
            Action::NewEmpty
            | Action::NewDemo
            | Action::NewTechno
            | Action::NewAcid
            | Action::Open
            | Action::Quit => {
                if self.view.dirty && !(action == Action::Quit && self.allow_close) {
                    self.dialogs.confirm = Some(Confirm { then: action });
                } else {
                    self.replace_project(ctx, action);
                }
            }
            Action::Save => {
                if self.view.path.is_some() {
                    self.spawn_save(ctx, None, None);
                } else {
                    self.request_path(ctx, PathPurpose::SaveAs);
                }
            }
            Action::SaveAs => self.request_path(ctx, PathPurpose::SaveAs),
            Action::ImportAudio => self.request_path(ctx, PathPurpose::ImportAudio),
            Action::LoadSounds => {
                let project = self.view.project.clone();
                let bank_track = self.sel.track.filter(|t| {
                    project.track(*t).is_ok_and(|t| {
                        t.instrument
                            .as_ref()
                            .is_some_and(|i| i.kind == InstrumentType::Bank)
                    })
                });
                self.open_load_sounds(bank_track);
            }
            Action::ToggleBrowser => self.browser.open = !self.browser.open,
            Action::AddBrowserFolder => self.request_path(ctx, PathPurpose::BrowseFolder),
            Action::ExportLoop => self.open_export_loop(),
            Action::ExportWav => self.open_export_wav(),
            Action::GameAssets => self.open_game_assets(),
            Action::ToggleLoop => {
                let r = actions::set_loop_enabled(&self.session, !self.view.project.loop_enabled);
                self.report(r);
            }
            Action::PlayStop => self.play_stop(),
            Action::Undo => match self.session.undo() {
                Ok(label) => self.info(format!("Undid “{label}”")),
                Err(e) => self.info(e.to_string()),
            },
            Action::Redo => match self.session.redo() {
                Ok(label) => self.info(format!("Redid “{label}”")),
                Err(e) => self.info(e.to_string()),
            },
            Action::SetTempo(bpm) => {
                let r = actions::set_tempo(&self.session, bpm, None);
                self.report(r);
            }
            Action::PromptTempo => self.palette.prompt_tempo(),
            Action::OpenSettings => self.open_settings(ctx),
            Action::OpenPalette => self.palette.show_palette(),
            Action::DeleteSelection => self.delete_selection(),
        }
        self.refresh();
    }

    /// Carries out New, Open or Quit once unsaved changes are dealt with.
    pub fn replace_project(&mut self, ctx: &Context, action: Action) {
        match action {
            Action::NewEmpty => self.session.new_project(empty_project("Untitled")),
            Action::NewDemo => self.session.new_project(demo_project()),
            Action::NewTechno => self.session.new_project(techno_project()),
            Action::NewAcid => self.session.new_project(acid_project()),
            Action::Open => self.request_path(ctx, PathPurpose::OpenProject),
            Action::Quit => {
                self.allow_close = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            other => self.run_action(ctx, other),
        }
    }

    pub fn play_stop(&mut self) {
        let result = if self.transport.playing {
            self.session.stop()
        } else {
            self.session.play()
        };
        if let Err(e) = result {
            self.error(format!("Cannot play: {e}"));
        }
    }

    pub fn stop(&mut self) {
        self.stop_preview();
        let result = if self.transport.playing {
            self.session.stop()
        } else {
            // Stop while stopped goes back to the start.
            self.session.seek(0)
        };
        if let Err(e) = result {
            self.error(format!("Cannot stop: {e}"));
        }
    }

    pub fn delete_selection(&mut self) {
        let Some(clip) = self.sel.clip else {
            return;
        };
        if self.sel.focus == Focus::Editor && !self.sel.notes.is_empty() {
            let ids = std::mem::take(&mut self.sel.notes);
            let r = actions::remove_notes(&self.session, clip, ids);
            self.report(r);
            return;
        }
        let r = actions::delete_clip(&self.session, clip);
        if self.report(r).is_some() {
            self.sel.clip = None;
            self.sel.notes.clear();
        }
    }

    pub fn spawn_save(
        &mut self,
        ctx: &Context,
        dir: Option<std::path::PathBuf>,
        then: Option<Action>,
    ) {
        let session = self.session.clone();
        self.jobs
            .spawn(ctx, Some("Saving".into()), move || JobOutput::Saved {
                result: session.save(dir.as_deref()).map_err(|e| e.to_string()),
                then,
            });
    }

    /// Starts computing a waveform overview.
    pub fn spawn_peaks(&mut self, ctx: &Context, id: sky_core::AudioId) {
        let session = self.session.clone();
        self.jobs.spawn(ctx, None, move || {
            let peaks = session.audio(id).map(|a| Arc::new(Peaks::compute(&a)));
            JobOutput::Peaks(id, peaks)
        });
    }

    pub fn handle_job(&mut self, ctx: &Context, out: JobOutput) {
        match out {
            JobOutput::Saved { result, then } => match result {
                Ok(path) => {
                    self.info(format!("Saved {}", path.display()));
                    self.refresh();
                    if let Some(action) = then {
                        self.replace_project(ctx, action);
                    }
                }
                Err(e) => self.error(format!("Save failed: {e}")),
            },
            JobOutput::Opened(result) => match result {
                Ok((path, recovered)) => {
                    if recovered > 0 {
                        self.info(format!(
                            "Opened {} and recovered {recovered} unsaved change(s)",
                            path.display()
                        ));
                    } else {
                        self.info(format!("Opened {}", path.display()));
                    }
                }
                Err(e) => self.error(format!("Open failed: {e}")),
            },
            JobOutput::Placed { name, result } => self.audio_placed(&name, result),
            JobOutput::FilesImported(result) => self.files_imported(result),
            JobOutput::BrowserPack { map, result } => self.browser_pack_read(map, result),
            JobOutput::FolderIndexed { path, result } => self.folder_indexed(path, result),
            JobOutput::LoopExported(result) => {
                if let Some(d) = self.dialogs.export_loop.as_mut() {
                    d.running = false;
                    d.result = Some(result);
                }
            }
            JobOutput::GameExported(result) => {
                if let Some(d) = self.dialogs.game.as_mut() {
                    d.running = false;
                    d.result = Some(result);
                }
            }
            JobOutput::WavExported(result) => {
                if let Some(d) = self.dialogs.export_wav.as_mut() {
                    d.running = false;
                    d.result = Some(result);
                }
            }
            JobOutput::PathPicked {
                purpose,
                token,
                path,
                elapsed,
            } => self.native_path_result(ctx, purpose, token, path, elapsed),
            JobOutput::Devices(list) => {
                if let Some(s) = self.dialogs.settings.as_mut() {
                    s.devices = Some(list);
                }
            }
            JobOutput::Peaks(id, peaks) => self.waveforms.insert(id, peaks),
            JobOutput::PackListed { map, result } => self.pack_listed(map, result),
            JobOutput::BankLoaded(result) => self.bank_loaded(result),
            JobOutput::Previewed { key, result } => self.preview_ready(key, result),
        }
    }

    fn audio_lost_banner(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("The audio device was lost.").color(theme::TEXT));
            if ui.button("Restart audio").clicked() {
                self.start_audio();
            }
            if ui.button("Audio settings…").clicked() {
                let ctx = ui.ctx().clone();
                self.open_settings(&ctx);
            }
        });
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if self.jobs.is_busy() {
                ui.spinner();
                let labels: Vec<&str> = self.jobs.busy_labels().collect();
                ui.label(RichText::new(format!("{}…", labels.join(", "))).color(theme::TEXT_WEAK));
            }
            match &self.notice {
                Some(n) => {
                    let color = if n.error { theme::ERROR } else { theme::TEXT };
                    ui.label(RichText::new(&n.text).color(color));
                }
                None => {
                    if let Some(s) = &self.view.status {
                        ui.label(RichText::new(s).color(theme::TEXT_WEAK));
                    }
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let path = self
                    .view
                    .path
                    .as_ref()
                    .map_or_else(|| "Not saved yet".to_string(), |p| p.display().to_string());
                ui.label(RichText::new(path).color(theme::TEXT_DIM));
                ui.separator();
                let load = format!("Engine {:.0}%", self.transport.load * 100.0);
                ui.label(RichText::new(load).color(theme::TEXT_WEAK));
                ui.separator();
                let audio = self.audio.describe();
                let color = if self.audio.stream.is_some() {
                    theme::TEXT_WEAK
                } else {
                    theme::WARN
                };
                let r = ui.add(
                    egui::Label::new(RichText::new(audio).color(color))
                        .truncate()
                        .sense(egui::Sense::click()),
                );
                let tip = match &self.audio.note {
                    Some(n) => format!("{n}\nClick for audio settings"),
                    None => "Click for audio settings".to_string(),
                };
                if r.on_hover_text(tip).clicked() {
                    let ctx = ui.ctx().clone();
                    self.open_settings(&ctx);
                }
            });
        });
    }

    fn bottom_panel(&mut self, ui: &mut Ui) {
        // Keep the panel at its set height whatever the tab holds.
        ui.set_min_size(ui.available_size());
        ui.horizontal(|ui| {
            let ai_count = self.view.activity.len();
            for (tab, label) in [
                (Tab::Editor, "Editor".to_string()),
                (Tab::Mixer, "Mixer".to_string()),
                (Tab::Instrument, "Instrument".to_string()),
                (
                    Tab::Ai,
                    if ai_count > 0 {
                        format!("AI activity ({ai_count})")
                    } else {
                        "AI activity".to_string()
                    },
                ),
            ] {
                let selected = self.tab == tab;
                let text = if selected {
                    RichText::new(label).strong().color(theme::TEXT)
                } else {
                    RichText::new(label).color(theme::TEXT_WEAK)
                };
                if ui.add(egui::Button::selectable(selected, text)).clicked() {
                    self.tab = tab;
                }
            }
        });
        ui.separator();
        match self.tab {
            Tab::Editor => crate::pattern_editor::clip_editor(self, ui),
            Tab::Mixer => crate::mixer::show(self, ui),
            Tab::Instrument => crate::params::instrument_panel(self, ui),
            Tab::Ai => crate::ai_panel::show(self, ui),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.audio.stop(&self.session);
    }
}
