// SPDX-License-Identifier: GPL-3.0-or-later
//! The application state and the per-frame layout.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use egui::{
    Align, CentralPanel, Context, Event, Frame, Key, Layout, Margin, Panel, RichText, Ui,
    ViewportCommand,
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
use crate::clip_tools::Tool;
use crate::dialogs::{Confirm, Dialogs, PathPurpose};
use crate::edit::EditState;
use crate::inst_view::DeviceState;
use crate::jobs::{JobOutput, Jobs};
use crate::keys;
use crate::loop_inspector::InspectorState;
use crate::mixer::MixerState;
use crate::palette::PaletteState;
use crate::pattern_editor::PatternState;
use crate::piano_roll::PianoState;
use crate::pictures::{PictureState, PictureStore};
use crate::presets::{PresetState, PresetStore};
use crate::ruler_lanes::Lane;
use crate::step_editor::StepState;
use crate::waveform::{Peaks, WaveformCache};
use crate::widgets::Gestures;
pub use crate::workspace::Tab;
use crate::workspace::Workspace;
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
    /// Open a project from File > Open recent.
    OpenRecent(PathBuf),
    ClearRecent,
    Save,
    SaveAs,
    ImportAudio,
    /// Load sounds from a sample pack into the selected sample bank, or a
    /// new one.
    LoadSounds,
    /// Close an open panel or open a closed one.
    TogglePanel(Tab),
    /// Put the panels back where they are on a first run.
    ResetLayout,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    About,
    /// Add a folder of samples to the browser.
    AddBrowserFolder,
    ExportLoop,
    ExportWav,
    /// The Game assets window: loops and SFX sets for a game, and their
    /// export.
    GameAssets,
    ToggleLoop,
    PlayStop,
    /// Stop, or go back to the start when already stopped.
    Stop,
    Undo,
    Redo,
    SetTempo(f64),
    /// Ask for a tempo in the palette.
    PromptTempo,
    OpenSettings,
    OpenPalette,
    /// Help > Keyboard shortcuts.
    Shortcuts,
    Cut,
    Copy,
    /// Paste at the playhead.
    Paste,
    /// Copy the selection to just after itself.
    Duplicate,
    DeleteSelection,
    SelectAll,
    SelectNone,
    /// Move the selected clips or notes along by this many grid steps.
    Nudge(i32),
    /// Move the selected clips or notes along by this many bars.
    NudgeBars(i32),
    /// Move the selected notes up by this many semitones (down when negative).
    Transpose(i32),
    /// Move the selected clips this many tracks down (up when negative).
    MoveToTrack(i32),
    /// Zoom the arrangement or piano roll in (positive) or out.
    TimelineZoom(i32),
    /// Show the whole song, or the whole clip in the piano roll.
    TimelineFit,
    /// Pick an arrangement tool.
    Tool(Tool),
    /// Cut the selected clips, or those on the selected track, at the
    /// playhead.
    SplitAtPlayhead,
    /// Mute the selected clips, or unmute them when all are muted.
    MuteClips,
    AddMarker,
    /// Show or hide a lane under the ruler.
    ToggleLane(Lane),
    /// Move the piano roll's notes onto the grid; `true` moves their ends
    /// too.
    Quantize(bool),
    Quit,
}

/// Which area the edit commands and arrow keys act on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Focus {
    #[default]
    Arrange,
    Editor,
}

#[derive(Default)]
pub struct Selection {
    pub track: Option<TrackId>,
    /// The clip the editor shows.
    pub clip: Option<ClipId>,
    /// The clips selected in the arrangement, usually `clip` among them.
    pub clips: Vec<ClipId>,
    /// Selected notes of the selected MIDI clip.
    pub notes: Vec<NoteId>,
    pub focus: Focus,
}

impl Selection {
    pub fn select_track(&mut self, id: TrackId) {
        if self.track != Some(id) {
            self.clip = None;
            self.clips.clear();
            self.notes.clear();
        }
        self.track = Some(id);
        self.focus = Focus::Arrange;
    }

    /// Selects one clip and nothing else.
    pub fn select_clip(&mut self, track: TrackId, clip: ClipId) {
        self.open_clip(track, clip);
        self.clips = vec![clip];
    }

    /// Shows a clip in the editor, leaving the other selected clips selected.
    pub fn open_clip(&mut self, track: TrackId, clip: ClipId) {
        if self.clip != Some(clip) {
            self.notes.clear();
        }
        self.track = Some(track);
        self.clip = Some(clip);
        self.focus = Focus::Arrange;
    }

    /// Adds a clip to the selection or takes it out, as Shift-click does.
    pub fn toggle_clip(&mut self, track: TrackId, clip: ClipId) {
        if let Some(i) = self.clips.iter().position(|c| *c == clip) {
            self.clips.remove(i);
            self.focus = Focus::Arrange;
        } else {
            self.open_clip(track, clip);
            self.clips.push(clip);
        }
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
                    if let ClipContent::Midi { notes, .. } = &clip.content {
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
        self.clips.retain(|c| project.find_clip(*c).is_ok());
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
    /// The Loop inspector's pick and its last render.
    pub inspector: InspectorState,
    pub browser: BrowserState,
    pub palette: PaletteState,
    pub dialogs: Dialogs,
    pub jobs: Jobs,
    pub gestures: Gestures,
    pub devices: DeviceState,
    pub presets: PresetState,
    /// Pictures of tracks, packs, banks and folders, and their editor.
    pub pictures: PictureState,
    pub waveforms: WaveformCache,
    /// The panels, where they are, and the recent projects.
    pub workspace: Workspace,
    /// Help > About is open.
    pub about: bool,
    /// Help > Keyboard shortcuts is open.
    pub shortcuts_open: bool,
    /// The clipboard and what the last paste did.
    pub edit: EditState,
    pub notice: Option<Notice>,
    /// Seconds since start, from egui.
    pub now: f64,
    project_id: ProjectId,
    pool_len: usize,
    /// The project's folder last frame, to notice opens and saves for the
    /// recent list.
    last_path: Option<PathBuf>,
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
            inspector: InspectorState::default(),
            browser: BrowserState::new(config.browser_store),
            palette: PaletteState::default(),
            dialogs: Dialogs::default(),
            jobs: Jobs::default(),
            gestures: Gestures::default(),
            devices: DeviceState::default(),
            presets: PresetState {
                store: PresetStore::load(config.preset_store),
                naming: None,
            },
            pictures: PictureState::new(PictureStore::load(config.picture_store)),
            waveforms: WaveformCache::default(),
            workspace: Workspace::default(),
            about: false,
            shortcuts_open: false,
            edit: EditState::default(),
            notice: None,
            now: 0.0,
            project_id,
            pool_len,
            last_path: None,
            title: String::new(),
            allow_close: false,
            view,
            session,
        }
    }

    /// Takes back the layout and recent projects saved by an earlier run.
    pub fn restore(&mut self, storage: &dyn eframe::Storage) {
        if let Some(json) = storage.get_string(crate::workspace::STORAGE_KEY) {
            self.workspace.restore(&json);
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

        Panel::top("menu")
            .frame(
                Frame::new()
                    .fill(theme::BG)
                    .inner_margin(Margin::symmetric(6, 2)),
            )
            .show(ui, |ui| crate::menu::bar(self, ui));
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
        CentralPanel::default()
            .frame(Frame::new().fill(theme::BG).inner_margin(Margin::same(0)))
            .show(ui, |ui| crate::workspace::show(self, ui));

        crate::mixer::effect_windows(self, &ctx);
        crate::menu::about_window(self, &ctx);
        keys::window(self, &ctx);
        crate::dialogs::show(self, &ctx);
        crate::palette::show(self, &ctx);
        crate::picture_editor::show(self, &ctx);
        crate::browser::overlays(self, &ctx);
        self.end_frame(&ctx);
    }

    fn begin_frame(&mut self, ctx: &Context) {
        self.now = ctx.input(|i| i.time);
        // Ctrl+Plus and the like come from the key command table instead.
        if ctx.options(|o| o.zoom_with_keyboard) {
            ctx.options_mut(|o| o.zoom_with_keyboard = false);
        }
        self.gestures.begin_frame(ctx);
        self.pictures.begin_frame();
        if self.pictures.waiting.is_some() && ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.pictures.waiting = None;
            self.info("The dropped image was not used");
        }
        for out in self.jobs.poll() {
            self.handle_job(ctx, out);
        }
        self.view = self.session.view();
        self.transport = self.session.transport();
        self.workspace.apply();
        self.browser.open = self.workspace.is_open(Tab::Browser);
        // A project opened or saved somewhere new, here or by an AI.
        if self.view.path != self.last_path {
            if let Some(path) = &self.view.path {
                self.workspace.remember(path);
            }
            self.last_path = self.view.path.clone();
        }
        let project = self.view.project.clone();
        if project.id != self.project_id {
            // A different project: nothing selected or cached carries over.
            self.project_id = project.id;
            self.stop_preview();
            self.audition.reset();
            self.inspector.reset();
            self.sel = Selection::default();
            self.arrange.reset_for_new_project();
            self.piano = PianoState::default();
            self.pattern = PatternState::default();
            self.mixer.open_effects.clear();
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
        self.commit_picture_preview(ctx);
        self.pictures.images.end_frame();
        // Written once a drag that changes a picture is over.
        if self.pictures.store.is_dirty()
            && !ctx.input(|i| i.pointer.any_down())
            && let Err(e) = self.pictures.store.flush()
        {
            self.error(e);
        }
        let fast = self.transport.playing
            || self.arrange.gesture()
            || self.piano.gesture()
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
        self.mixer
            .loudness
            .update(shared.as_deref(), self.transport.playing);
        if self.mixer.meters.len() != project.tracks.len() {
            self.mixer
                .meters
                .retain(|id, _| project.tracks.iter().any(|t| t.id == *id));
        }
        let mut seen = Vec::new();
        for (i, t) in project.tracks.iter().enumerate() {
            let peaks = shared
                .as_ref()
                .and_then(|s| s.track_meter(i))
                .map_or((0.0, 0.0), |m| m.take());
            self.mixer.meters.entry(t.id).or_default().update(peaks, dt);
            self.mixer.update_reductions(
                ChannelRef::Track(t.id),
                &t.channel,
                shared.as_ref().map(|s| s.reductions(Some(i))),
                dt,
                &mut seen,
            );
        }
        self.mixer.update_reductions(
            ChannelRef::Master,
            &project.master,
            shared.as_ref().map(|s| s.reductions(None)),
            dt,
            &mut seen,
        );
        self.mixer.reductions.retain(|k, _| seen.contains(k));
    }

    /// Where the edit commands and arrow keys act: the piano roll while it
    /// was clicked last and is in view, otherwise the arrangement.
    pub fn edit_focus(&self) -> Focus {
        if self.sel.focus == Focus::Editor && self.workspace.is_front(Tab::Editor) {
            Focus::Editor
        } else {
            Focus::Arrange
        }
    }

    /// Runs the key commands pressed this frame, from [`keys::table`].
    fn shortcuts(&mut self, ctx: &Context) {
        if self.palette.open || self.dialogs.blocks_input() {
            return;
        }
        let typing = ctx.text_edit_focused();
        // Arrow keys move between widgets while one has the keyboard.
        let widget_focus = ctx.memory(|m| m.focused().is_some());
        // Escape cancels a drag or closes a menu before it deselects.
        let escape_taken =
            self.arrange.gesture() || self.piano.gesture() || egui::Popup::is_any_open(ctx);
        let label_selected = ctx
            .plugin::<egui::text_selection::LabelSelectionState>()
            .lock()
            .has_selection();
        let focus = self.edit_focus();
        let table = keys::table();
        let mut bindings: Vec<_> = table
            .iter()
            .filter(|c| c.scope.applies(focus) && (c.while_typing || !typing))
            .flat_map(|c| c.keys.iter().map(move |k| (k, c)))
            .filter(|(k, _)| {
                let arrow = matches!(
                    k.logical_key,
                    Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown
                );
                !(arrow && widget_focus) && !(k.logical_key == Key::Escape && escape_taken)
            })
            .collect();
        bindings.sort_by_key(|(k, _)| std::cmp::Reverse(keys::modifier_count(k.modifiers)));
        let mut wanted = Vec::new();
        let mut pasted = None;
        ctx.input_mut(|i| {
            if !typing {
                // The system turns Ctrl+X, C and V into these, not key presses.
                i.events.retain(|e| match e {
                    Event::Cut if !label_selected => {
                        wanted.push(Action::Cut);
                        false
                    }
                    Event::Copy if !label_selected => {
                        wanted.push(Action::Copy);
                        false
                    }
                    Event::Paste(text) => {
                        pasted = Some(text.clone());
                        wanted.push(Action::Paste);
                        false
                    }
                    _ => true,
                });
            }
            for (key, command) in &bindings {
                if i.consume_shortcut(key) {
                    wanted.push(command.action.clone());
                }
            }
        });
        if pasted.is_some() {
            self.edit.pasted = pasted;
        }
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
                        self.workspace.show(Tab::Instrument);
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
                    self.mixer.open_effect((channel, id));
                }
            }
            Action::NewEmpty
            | Action::NewDemo
            | Action::NewTechno
            | Action::NewAcid
            | Action::Open
            | Action::OpenRecent(_)
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
            Action::ClearRecent => self.workspace.recent.clear(),
            Action::TogglePanel(tab) => self.workspace.toggle(tab),
            Action::ResetLayout => self.workspace.reset(),
            Action::ZoomIn => egui::gui_zoom::zoom_in(ctx),
            Action::ZoomOut => egui::gui_zoom::zoom_out(ctx),
            Action::ZoomReset => ctx.set_zoom_factor(1.0),
            Action::About => self.about = true,
            Action::AddBrowserFolder => self.request_path(ctx, PathPurpose::BrowseFolder),
            Action::ExportLoop => self.open_export_loop(),
            Action::ExportWav => self.open_export_wav(),
            Action::GameAssets => self.open_game_assets(),
            Action::ToggleLoop => {
                let r = actions::set_loop_enabled(&self.session, !self.view.project.loop_enabled);
                self.report(r);
            }
            Action::PlayStop => self.play_stop(),
            Action::Stop => self.stop(),
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
            Action::Shortcuts => self.shortcuts_open = true,
            Action::Cut => self.cut(ctx),
            Action::Copy => {
                self.copy(ctx);
            }
            Action::Paste => self.paste(),
            Action::Duplicate => self.duplicate(),
            Action::DeleteSelection => self.delete_selection(),
            Action::SelectAll => self.select_all(),
            Action::SelectNone => self.select_none(),
            Action::Nudge(steps) => self.nudge(steps, false),
            Action::NudgeBars(bars) => self.nudge(bars, true),
            Action::Transpose(semitones) => self.transpose(semitones),
            Action::MoveToTrack(rows) => self.move_to_track(rows),
            Action::TimelineZoom(dir) => self.timeline_zoom(dir),
            Action::TimelineFit => self.timeline_fit(),
            Action::Tool(tool) => self.set_tool(tool),
            Action::SplitAtPlayhead => self.split_at_playhead(),
            Action::MuteClips => self.mute_selection(),
            Action::AddMarker => self.add_marker_at_playhead(),
            Action::ToggleLane(lane) => self.workspace.lanes.toggle(lane),
            Action::Quantize(ends) => self.quantize_notes(ends),
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
            Action::OpenRecent(path) => {
                if self
                    .path_chosen(ctx, PathPurpose::OpenProject, path.clone())
                    .is_some()
                {
                    // Moved or deleted since: no use listing it.
                    self.workspace.forget(&path);
                }
            }
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
            JobOutput::PianoLibrary(result) => self.piano_library_set(result),
            JobOutput::PatternSounds {
                clip,
                text,
                source,
                seed,
                cycle,
                result,
            } => self.pattern_sounds_loaded(clip, &text, source, seed, cycle, result),
            JobOutput::Previewed { key, result } => self.preview_ready(key, result),
            JobOutput::PictureLoaded { target, result } => self.picture_loaded(target, result),
            JobOutput::CoverFound { key, cover } => self.cover_found(key, cover),
            JobOutput::SeamChecked {
                target,
                revision,
                result,
            } => self.seam_checked(target, revision, result),
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
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string(crate::workspace::STORAGE_KEY, self.workspace.saved_json());
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.audio.stop(&self.session);
    }
}
