// SPDX-License-Identifier: GPL-3.0-or-later
//! The live session: one open project, its undo history, the audio engine
//! that plays it, and saving.
//!
//! The UI and the MCP server both hold a [`Session`] and change the project
//! only through [`Session::apply`], so every change from either side is one
//! undo step, is written to the crash journal, and reaches the engine as a
//! new snapshot. Readers never lock: [`Session::view`] returns the latest
//! published [`ProjectView`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arc_swap::{ArcSwap, ArcSwapOption};
use serde::Serialize;
use sky_core::manifest::{Asset, Category};
use sky_core::{AudioId, Ticks, TrackId};
use sky_dsp::AudioData;
use sky_dsp::InstrumentType;
use sky_dsp::analysis::{self, SeamReport};
use sky_engine::{
    AudioCache, Engine, EngineHandle, EngineMsg, EngineShared, compile, create_engine,
};
use sky_game::{AssetAudio, AssetExport, ExportSet, Exported, LoopExport};
use sky_io::{WavFormat, read_audio, write_wav};
use sky_library::{Library, LibraryError, PackSource};
use sky_model::file::{self, Journal};
use sky_model::{
    AudioSource, BankSample, Command, GameAssetKind, History, InstrumentSlot, License,
    LicenseReport, ModelError, Origin, PoolEntry, Project, SampleBank, TrackKind, Transaction,
};
use thiserror::Error;

/// Longest render the session will make in one call.
const MAX_RENDER_SECONDS: f64 = 30.0 * 60.0;
/// Activity entries kept for the AI panel.
const MAX_ACTIVITY: usize = 200;
/// Undo steps copied into each view.
const VIEW_HISTORY: usize = 100;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("{0}")]
    Model(#[from] ModelError),
    #[error("{0}")]
    Io(#[from] sky_io::IoError),
    #[error("{0}")]
    Export(#[from] sky_game::GameError),
    #[error("{0}")]
    Library(#[from] LibraryError),
    #[error("the project has not been saved yet; give a folder to save it in")]
    NoProjectPath,
    #[error("no audio device is running")]
    NoEngine,
    #[error("{0}")]
    Invalid(String),
}

/// One change an MCP client made, for the AI activity panel.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEntry {
    pub seq: u64,
    pub time_unix_ms: u64,
    pub client: String,
    pub tool: String,
    pub label: String,
    pub undone: bool,
}

/// One step on the undo stack.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItem {
    pub seq: u64,
    pub label: String,
    pub origin: Origin,
}

/// An immutable picture of the session, replaced after every change.
#[derive(Clone, Debug)]
pub struct ProjectView {
    pub project: Arc<Project>,
    /// Goes up by one with every published change.
    pub revision: u64,
    pub path: Option<PathBuf>,
    pub dirty: bool,
    pub undo_label: Option<String>,
    pub redo_label: Option<String>,
    /// The newest undo steps, oldest first.
    pub history: Vec<HistoryItem>,
    /// Changes made by MCP clients, oldest first.
    pub activity: Vec<ActivityEntry>,
    /// Sample rate of the running engine, if there is one.
    pub engine_rate: Option<u32>,
    /// A short message about the last save, open or recovery.
    pub status: Option<String>,
}

/// Where the playhead is and how the engine is doing.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportState {
    pub running: bool,
    pub playing: bool,
    pub position_samples: i64,
    pub position_ticks: f64,
    pub sample_rate: u32,
    /// Share of each device buffer the engine used, 0..1.
    pub load: f32,
    pub dropouts: u64,
    pub overruns: u64,
}

/// Options for [`Session::export_loop`].
#[derive(Clone, Debug)]
pub struct LoopExportOptions {
    /// Asset name; defaults to the project name.
    pub name: Option<String>,
    /// Export folder; defaults to `renders/export` inside the project folder.
    pub dir: Option<PathBuf>,
    /// Loop range in ticks; defaults to the project's loop region.
    pub range: Option<(Ticks, Ticks)>,
    pub category: Category,
    pub tags: Vec<String>,
    pub format: WavFormat,
}

impl Default for LoopExportOptions {
    fn default() -> Self {
        Self {
            name: None,
            dir: None,
            range: None,
            category: Category::Music,
            tags: Vec::new(),
            format: WavFormat::Pcm24,
        }
    }
}

/// What [`Session::export_loop`] wrote.
pub struct LoopExported {
    pub exported: Exported,
    /// How the end of the loop meets its start.
    pub seam: SeamReport,
    pub bars: f64,
    pub frames: usize,
    pub sample_rate: u32,
    /// Credits written into the manifest, and license warnings.
    pub licenses: LicenseReport,
}

/// Options for [`Session::export_game`].
#[derive(Clone, Debug, Default)]
pub struct GameExportOptions {
    /// Export folder; defaults to the project's game export folder (see
    /// [`Session::game_export_dir`]).
    pub dir: Option<PathBuf>,
    /// Names of the game assets to export. Empty exports all of them and
    /// removes the project's assets that the folder's manifest still has
    /// but the project no longer lists.
    pub only: Vec<String>,
}

/// One game asset [`Session::export_game`] wrote.
#[derive(Clone, Debug)]
pub struct GameAssetExported {
    /// The manifest entry.
    pub asset: Asset,
    /// For loops: how the end of the body meets its start.
    pub seam: Option<SeamReport>,
}

/// What [`Session::export_game`] wrote.
#[derive(Clone, Debug)]
pub struct GameExported {
    pub dir: PathBuf,
    pub manifest: PathBuf,
    pub assets: Vec<GameAssetExported>,
    /// Every audio file written.
    pub files: Vec<PathBuf>,
    /// Assets removed from the manifest because the project dropped them.
    pub removed: Vec<String>,
    /// Old audio files the manifest no longer uses, now deleted.
    pub deleted: Vec<PathBuf>,
    /// Credits written into the manifest, and license warnings.
    pub licenses: LicenseReport,
}

/// Samples quieter than this (about -80 dBFS) count as silence when SFX
/// variations are trimmed.
const SILENCE: f32 = 1e-4;

/// Cuts trailing silence, keeping at least one frame.
fn trim_silence(audio: &mut AudioData) {
    let last = audio
        .channels
        .iter()
        .filter_map(|ch| ch.iter().rposition(|s| s.abs() > SILENCE))
        .max()
        .unwrap_or(0);
    for ch in &mut audio.channels {
        ch.truncate(last + 1);
    }
}

/// Mixes stereo down to one channel at the same loudness for centered
/// sounds.
fn fold_to_mono(audio: &mut AudioData) {
    if audio.channels.len() < 2 {
        return;
    }
    let mono: Vec<f32> = audio
        .left()
        .iter()
        .zip(audio.right())
        .map(|(l, r)| 0.5 * (l + r))
        .collect();
    audio.channels = vec![mono];
}

/// Which sounds [`Session::load_bank`] loads, and from where.
#[derive(Clone, Debug)]
pub struct BankRequest {
    pub source: PackSource,
    /// A bank such as `RolandTR808`; without one, the pack's plain sounds.
    pub bank: Option<String>,
    /// Sound names to load; empty loads all of the bank's.
    pub sounds: Vec<String>,
    /// Add to the track's sounds instead of replacing them.
    pub add: bool,
    /// The sound notes play. Without one, a bank of a single pitched sound
    /// (such as `piano`) plays it, and other banks play drums by note.
    pub pitched: Option<String>,
}

/// What [`Session::load_bank`] did.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BankLoaded {
    pub track: TrackId,
    pub bank: String,
    pub pack: String,
    pub license: License,
    /// Each sound in the track's bank and its variation count.
    pub sounds: Vec<(String, usize)>,
    pub pitched: Option<String>,
    /// Files fetched and added to the pool this time.
    pub new_files: usize,
    /// Files already in the pool from an earlier load.
    pub reused_files: usize,
    /// Files that could not be loaded.
    pub failed: Vec<String>,
    /// True when the network failed and a saved copy of the map was used.
    pub offline: bool,
}

struct State {
    project: Project,
    history: History,
    path: Option<PathBuf>,
    journal: Option<Journal>,
    dirty: bool,
    revision: u64,
    snapshot_version: u64,
    activity: Vec<ActivityEntry>,
    audio: AudioCache,
    engine_rate: Option<u32>,
    status: Option<String>,
}

struct Inner {
    state: Mutex<State>,
    /// Where sound packs come from; never locked with `state`.
    library: Mutex<Library>,
    /// Locked after `state` when both are needed, never before.
    engine: Mutex<Option<EngineHandle>>,
    shared: ArcSwapOption<EngineShared>,
    view: ArcSwap<ProjectView>,
}

/// A handle to the live session. Clones share the same session.
#[derive(Clone)]
pub struct Session {
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic elsewhere must not take the session down with it.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn build_view(st: &State) -> ProjectView {
    let steps = st.history.undo_steps();
    let from = steps.len().saturating_sub(VIEW_HISTORY);
    ProjectView {
        project: Arc::new(st.project.clone()),
        revision: st.revision,
        path: st.path.clone(),
        dirty: st.dirty,
        undo_label: st.history.undo_label().map(str::to_string),
        redo_label: st.history.redo_label().map(str::to_string),
        history: steps
            .get(from..)
            .unwrap_or_default()
            .iter()
            .map(|a| HistoryItem {
                seq: a.seq,
                label: a.label.clone(),
                origin: a.origin.clone(),
            })
            .collect(),
        activity: st.activity.clone(),
        engine_rate: st.engine_rate,
        status: st.status.clone(),
    }
}

impl State {
    fn new(project: Project) -> Self {
        Self {
            project,
            history: History::new(),
            path: None,
            journal: None,
            dirty: false,
            revision: 0,
            snapshot_version: 0,
            activity: Vec::new(),
            audio: AudioCache::new(),
            engine_rate: None,
            status: None,
        }
    }

    /// Records applied commands so a crash loses nothing. A journal that
    /// cannot be written is logged, not fatal: the change already happened.
    fn journal(&mut self, label: &str, commands: &[Command]) {
        let Some(dir) = self.path.clone() else {
            return;
        };
        if self.journal.is_none() {
            match Journal::open(&dir) {
                Ok(j) => self.journal = Some(j),
                Err(e) => {
                    tracing::warn!("cannot open the crash journal: {e}");
                    return;
                }
            }
        }
        if let Some(j) = self.journal.as_mut()
            && let Err(e) = j.append(label, commands)
        {
            tracing::warn!("cannot write the crash journal: {e}");
        }
    }

    fn mark_activity(&mut self, seq: u64, undone: bool) {
        if let Some(a) = self.activity.iter_mut().rev().find(|a| a.seq == seq) {
            a.undone = undone;
        }
    }
}

impl Session {
    /// Starts a session on `project`. With `engine_rate` set, also creates
    /// the real-time engine at that sample rate and returns its audio-thread
    /// end, which the caller hands to a device.
    pub fn new(project: Project, engine_rate: Option<u32>) -> (Session, Option<Engine>) {
        let mut state = State::new(project);
        let mut engine_out = None;
        let mut handle_out = None;
        let mut shared_out = None;
        if let Some(sr) = engine_rate {
            state.snapshot_version += 1;
            let snap = compile(&state.project, &state.audio, sr, state.snapshot_version);
            let (handle, engine) = create_engine(snap);
            shared_out = Some(handle.shared().clone());
            handle_out = Some(handle);
            engine_out = Some(engine);
            state.engine_rate = Some(sr);
        }
        let view = build_view(&state);
        let inner = Arc::new(Inner {
            state: Mutex::new(state),
            library: Mutex::new(Library::default()),
            engine: Mutex::new(handle_out),
            shared: ArcSwapOption::new(shared_out),
            view: ArcSwap::from_pointee(view),
        });
        spawn_housekeeping(Arc::downgrade(&inner));
        (Session { inner }, engine_out)
    }

    /// The latest published view. Cheap; never blocks.
    pub fn view(&self) -> Arc<ProjectView> {
        self.inner.view.load_full()
    }

    /// The current project.
    pub fn project(&self) -> Arc<Project> {
        self.view().project.clone()
    }

    /// Meters, playhead and load of the running engine.
    pub fn engine_shared(&self) -> Option<Arc<EngineShared>> {
        self.inner.shared.load_full()
    }

    fn publish(&self, st: &mut State, recompile: bool) {
        st.revision += 1;
        if recompile && let Some(sr) = st.engine_rate {
            st.snapshot_version += 1;
            let snap = compile(&st.project, &st.audio, sr, st.snapshot_version);
            if let Some(h) = lock(&self.inner.engine).as_mut() {
                h.send(EngineMsg::Swap(Box::new(snap)));
            }
        }
        self.inner.view.store(Arc::new(build_view(st)));
    }

    fn apply_locked(&self, st: &mut State, tx: Transaction) -> Result<u64, SessionError> {
        let label = tx.label.clone();
        let origin = tx.origin.clone();
        let commands = tx.commands.clone();
        let seq = st.history.apply(&mut st.project, tx)?.seq;
        st.journal(&label, &commands);
        if let Origin::Mcp { client, tool } = origin
            && st.activity.last().is_none_or(|a| a.seq != seq)
        {
            st.activity.push(ActivityEntry {
                seq,
                time_unix_ms: now_ms(),
                client,
                tool,
                label,
                undone: false,
            });
            if st.activity.len() > MAX_ACTIVITY {
                st.activity.remove(0);
            }
        }
        st.dirty = true;
        self.publish(st, true);
        Ok(seq)
    }

    /// Applies a transaction as one undo step and returns its sequence
    /// number. On error nothing changes.
    pub fn apply(&self, tx: Transaction) -> Result<u64, SessionError> {
        let mut st = lock(&self.inner.state);
        self.apply_locked(&mut st, tx)
    }

    /// Undoes the latest step and returns its label.
    pub fn undo(&self) -> Result<String, SessionError> {
        let mut st = lock(&self.inner.state);
        let st = &mut *st;
        let step = st.history.undo(&mut st.project)?;
        let applied: Vec<Command> = step.inverse.iter().rev().cloned().collect();
        st.journal(&format!("Undo {}", step.label), &applied);
        st.mark_activity(step.seq, true);
        st.dirty = true;
        self.publish(st, true);
        Ok(step.label)
    }

    /// Redoes the latest undone step and returns its label.
    pub fn redo(&self) -> Result<String, SessionError> {
        let mut st = lock(&self.inner.state);
        let st = &mut *st;
        let step = st.history.redo(&mut st.project)?;
        st.journal(&format!("Redo {}", step.label), &step.forward);
        st.mark_activity(step.seq, false);
        st.dirty = true;
        self.publish(st, true);
        Ok(step.label)
    }

    /// Undoes every step back to and including step `seq`. Returns how many
    /// steps were undone.
    pub fn undo_to(&self, seq: u64) -> Result<usize, SessionError> {
        let present = lock(&self.inner.state)
            .history
            .undo_steps()
            .iter()
            .any(|a| a.seq == seq);
        if !present {
            return Err(SessionError::Invalid(
                "that change is no longer in the undo history".into(),
            ));
        }
        let mut count = 0;
        while self.view_top_seq().is_some_and(|top| top >= seq) {
            self.undo()?;
            count += 1;
        }
        Ok(count)
    }

    fn view_top_seq(&self) -> Option<u64> {
        lock(&self.inner.state).history.top_seq()
    }

    /// Clears the undo history (after opening a project, for example).
    pub fn clear_history(&self) {
        let mut st = lock(&self.inner.state);
        st.history.clear();
        self.publish(&mut st, false);
    }

    /// Sets the status message shown in the UI.
    pub fn set_status(&self, message: impl Into<String>) {
        let mut st = lock(&self.inner.state);
        st.status = Some(message.into());
        self.publish(&mut st, false);
    }

    // ----- transport -----

    fn send(&self, msg: EngineMsg) -> Result<(), SessionError> {
        match lock(&self.inner.engine).as_mut() {
            Some(h) => {
                h.send(msg);
                Ok(())
            }
            None => Err(SessionError::NoEngine),
        }
    }

    pub fn play(&self) -> Result<(), SessionError> {
        self.send(EngineMsg::Play)
    }

    pub fn stop(&self) -> Result<(), SessionError> {
        self.send(EngineMsg::Stop)
    }

    /// Moves the playhead to a tick position.
    pub fn seek(&self, tick: Ticks) -> Result<(), SessionError> {
        let sr = self.view().engine_rate.ok_or(SessionError::NoEngine)?;
        let sample = self.project().tempo.tick_to_sample(tick.max(0), sr);
        self.send(EngineMsg::Seek(sample))
    }

    /// Silences every instrument and effect tail at once.
    pub fn all_sound_off(&self) -> Result<(), SessionError> {
        self.send(EngineMsg::AllSoundOff)
    }

    pub fn transport(&self) -> TransportState {
        let Some(shared) = self.engine_shared() else {
            return TransportState {
                running: false,
                playing: false,
                position_samples: 0,
                position_ticks: 0.0,
                sample_rate: self.project().sample_rate,
                load: 0.0,
                dropouts: 0,
                overruns: 0,
            };
        };
        use std::sync::atomic::Ordering::Relaxed;
        let sr = shared.sample_rate.load(Relaxed);
        let pos = shared.playhead();
        TransportState {
            running: true,
            playing: shared.is_playing(),
            position_samples: pos,
            position_ticks: self.project().tempo.sample_to_ticks(pos, sr),
            sample_rate: sr,
            load: shared.load(),
            dropouts: shared.dropouts.load(Relaxed),
            overruns: shared.overruns.load(Relaxed),
        }
    }

    /// Creates a new engine at `sample_rate`, replacing any old one, and
    /// returns its audio-thread end. Used when the device starts or changes.
    pub fn attach_engine(&self, sample_rate: u32) -> Engine {
        let mut st = lock(&self.inner.state);
        st.snapshot_version += 1;
        let snap = compile(&st.project, &st.audio, sample_rate, st.snapshot_version);
        let (handle, engine) = create_engine(snap);
        self.inner.shared.store(Some(handle.shared().clone()));
        *lock(&self.inner.engine) = Some(handle);
        st.engine_rate = Some(sample_rate);
        self.publish(&mut st, false);
        engine
    }

    /// Drops the engine handle, for when the device is gone.
    pub fn detach_engine(&self) {
        let mut st = lock(&self.inner.state);
        *lock(&self.inner.engine) = None;
        self.inner.shared.store(None);
        st.engine_rate = None;
        self.publish(&mut st, false);
    }

    // ----- files -----

    fn replace_project(&self, st: &mut State, project: Project, path: Option<PathBuf>) {
        st.project = project;
        st.history.clear();
        st.path = path;
        st.journal = None;
        st.dirty = false;
        st.activity.clear();
        if let Some(h) = lock(&self.inner.engine).as_mut() {
            h.send(EngineMsg::Stop);
            h.send(EngineMsg::Seek(0));
            h.send(EngineMsg::AllSoundOff);
        }
    }

    /// Replaces the open project with a new, unsaved one.
    pub fn new_project(&self, project: Project) {
        let mut st = lock(&self.inner.state);
        st.audio.clear();
        st.status = Some(format!("New project \"{}\"", project.name));
        self.replace_project(&mut st, project, None);
        self.publish(&mut st, true);
    }

    /// Saves the project into its folder, or into `dir` (which then becomes
    /// its folder). Pool audio not yet in the folder is written first.
    pub fn save(&self, dir: Option<&Path>) -> Result<PathBuf, SessionError> {
        let mut st = lock(&self.inner.state);
        let dir = dir
            .map(Path::to_path_buf)
            .or_else(|| st.path.clone())
            .ok_or(SessionError::NoProjectPath)?;
        write_pool_audio(&st.project, &st.audio, &dir)?;
        // Close the journal first: saving deletes it.
        st.journal = None;
        file::save_project(&st.project, &dir)?;
        st.path = Some(dir.clone());
        st.dirty = false;
        st.status = Some(format!("Saved {}", dir.display()));
        self.publish(&mut st, false);
        Ok(dir)
    }

    /// Writes a copy of the project and its audio into `dir` without
    /// changing where the session saves or whether it has unsaved changes.
    /// Used for autosave; the session stays responsive while it writes.
    pub fn save_copy(&self, dir: &Path) -> Result<(), SessionError> {
        let (project, audio) = {
            let st = lock(&self.inner.state);
            (st.project.clone(), st.audio.clone())
        };
        write_pool_audio(&project, &audio, dir)?;
        file::save_project(&project, dir)?;
        Ok(())
    }

    /// Opens a project folder, replaying any changes a crash left in its
    /// journal. Returns how many journal entries were recovered.
    pub fn open(&self, dir: &Path) -> Result<usize, SessionError> {
        let entries = file::read_journal(dir)?;
        let (project, replayed) = file::recover_project(dir)?;
        let mut audio = AudioCache::new();
        let mut missing = Vec::new();
        for entry in &project.audio_pool {
            match read_audio(&dir.join(&entry.file)) {
                Ok(data) => {
                    audio.insert(entry.id, Arc::new(data));
                }
                Err(e) => {
                    tracing::warn!("{e}");
                    missing.push(entry.name.clone());
                }
            }
        }
        if replayed < entries.len() {
            // Keep only the entries that replayed, so later appends follow
            // a journal that recovers cleanly.
            file::clear_journal(dir)?;
            let mut journal = Journal::open(dir)?;
            for e in entries.iter().take(replayed) {
                journal.append(&e.label, &e.commands)?;
            }
        }
        let mut st = lock(&self.inner.state);
        st.audio = audio;
        self.replace_project(&mut st, project, Some(dir.to_path_buf()));
        st.dirty = replayed > 0;
        let mut status = format!("Opened {}", dir.display());
        if replayed > 0 {
            status = format!(
                "Recovered {replayed} unsaved change(s) in {}",
                dir.display()
            );
        }
        if !missing.is_empty() {
            status.push_str(&format!("; missing audio: {}", missing.join(", ")));
        }
        st.status = Some(status);
        self.publish(&mut st, true);
        Ok(replayed)
    }

    /// Decodes an audio file into the project's pool as one undo step and
    /// returns its ID.
    pub fn import_audio(&self, path: &Path, origin: Origin) -> Result<AudioId, SessionError> {
        let data = read_audio(path)?;
        if data.frames() == 0 {
            return Err(SessionError::Invalid(format!(
                "{} has no audio in it",
                path.display()
            )));
        }
        let id = AudioId::new();
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "audio".into());
        let entry = PoolEntry {
            id,
            name: name.clone(),
            file: format!("{}/{id}.wav", file::AUDIO_DIR),
            channels: data.channel_count() as u16,
            sample_rate: data.sample_rate,
            frames: data.frames() as u64,
            source: None,
        };
        let mut st = lock(&self.inner.state);
        if let Some(dir) = &st.path {
            write_wav(&dir.join(&entry.file), &data, WavFormat::Float32, false)?;
        }
        st.audio.insert(id, Arc::new(data));
        let tx = Transaction::new(
            format!("Import {name}"),
            origin,
            vec![Command::AddPoolAudio { entry, index: None }],
        );
        self.apply_locked(&mut st, tx)?;
        Ok(id)
    }

    /// The sound-pack library used by [`Session::load_bank`].
    pub fn library(&self) -> Library {
        lock(&self.inner.library).clone()
    }

    /// Uses `library` for sound packs from now on, such as one with its
    /// cache in another folder.
    pub fn set_library(&self, library: Library) {
        *lock(&self.inner.library) = library;
    }

    /// Loads sounds from a pack into a track's sample bank, as one undo
    /// step. The track becomes a sample bank if it was another instrument.
    /// Downloads happen first, with nothing locked, so this can take a
    /// while; files already in the pool from the same address are reused.
    pub fn load_bank(
        &self,
        track: TrackId,
        request: &BankRequest,
        origin: Origin,
    ) -> Result<BankLoaded, SessionError> {
        {
            let project = self.project();
            if project.track(track)?.kind != TrackKind::Instrument {
                return Err(ModelError::NotAnInstrumentTrack(track).into());
            }
        }
        let library = self.library();
        let pack = library.open(&request.source)?;
        let selection = pack.select(request.bank.as_deref(), &request.sounds)?;
        let loaded = library.load(&pack, &selection)?;

        let mut st = lock(&self.inner.state);
        let t = st.project.track(track)?;
        if t.kind != TrackKind::Instrument {
            return Err(ModelError::NotAnInstrumentTrack(track).into());
        }
        let mut slot = match &t.instrument {
            Some(s) if s.kind == InstrumentType::Bank => s.clone(),
            _ => InstrumentSlot::new(InstrumentType::Bank),
        };
        let previous = slot.bank.take().filter(|_| request.add);
        let mut sounds = previous
            .as_ref()
            .map(|b| b.sounds.clone())
            .unwrap_or_default();
        let source = &loaded.source;
        let mut commands = Vec::new();
        let mut added = Vec::new();
        let (mut new_files, mut reused_files) = (0, 0);
        for (name, samples) in &loaded.sounds {
            let mut variations = Vec::with_capacity(samples.len());
            for (i, sample) in samples.iter().enumerate() {
                let existing = st
                    .project
                    .audio_pool
                    .iter()
                    .find(|e| e.source.as_ref().is_some_and(|s| s.url == sample.location))
                    .map(|e| e.id);
                let id = match existing {
                    Some(id) => {
                        reused_files += 1;
                        st.audio.entry(id).or_insert_with(|| sample.audio.clone());
                        id
                    }
                    None => {
                        new_files += 1;
                        let id = AudioId::new();
                        let entry = PoolEntry {
                            id,
                            name: format!("{} {name}:{i}", selection.name),
                            file: format!("{}/{id}.wav", file::AUDIO_DIR),
                            channels: sample.audio.channel_count() as u16,
                            sample_rate: sample.audio.sample_rate,
                            frames: sample.audio.frames() as u64,
                            source: Some(AudioSource {
                                pack: source.name.clone(),
                                url: sample.location.clone(),
                                license: source.license,
                                credit: source.credit.clone(),
                            }),
                        };
                        if let Some(dir) = &st.path {
                            write_wav(
                                &dir.join(&entry.file),
                                &sample.audio,
                                WavFormat::Float32,
                                false,
                            )?;
                        }
                        st.audio.insert(id, sample.audio.clone());
                        added.push(id);
                        commands.push(Command::AddPoolAudio { entry, index: None });
                        id
                    }
                };
                variations.push(BankSample {
                    audio: id,
                    root: sample.root,
                });
            }
            sounds.insert(name.clone(), variations);
        }
        let pitched = match &request.pitched {
            Some(p) => Some(p.clone()),
            None => match &previous {
                Some(prev) => prev.pitched.clone(),
                None if loaded.sounds.len() == 1 => loaded
                    .sounds
                    .iter()
                    .find(|(_, s)| s.iter().all(|x| x.root.is_some()))
                    .map(|(n, _)| n.clone()),
                None => None,
            },
        };
        let name = match &previous {
            Some(prev) if prev.name != selection.name => {
                format!("{} + {}", prev.name, selection.name)
            }
            _ => selection.name.clone(),
        };
        let bank = SampleBank {
            name: name.clone(),
            sounds,
            pitched,
        };
        slot.bank = Some(bank.clone());
        commands.push(Command::SetInstrument {
            track,
            instrument: Some(slot),
        });
        let tx = Transaction::new(format!("Load {} sounds", selection.name), origin, commands);
        if let Err(e) = self.apply_locked(&mut st, tx) {
            for id in added {
                st.audio.remove(&id);
            }
            return Err(e);
        }
        Ok(BankLoaded {
            track,
            bank: name,
            pack: source.name.clone(),
            license: source.license,
            sounds: bank
                .sounds
                .iter()
                .map(|(n, v)| (n.clone(), v.len()))
                .collect(),
            pitched: bank.pitched,
            new_files,
            reused_files,
            failed: loaded.failed,
            offline: pack.offline,
        })
    }

    /// Credits and warnings for the pack audio an export would contain.
    pub fn license_report(&self) -> LicenseReport {
        sky_model::license_report(&self.project())
    }

    /// Decoded audio from the pool.
    pub fn audio(&self, id: AudioId) -> Option<Arc<AudioData>> {
        lock(&self.inner.state).audio.get(&id).cloned()
    }

    // ----- rendering -----

    fn render_inputs(&self) -> (Arc<Project>, AudioCache) {
        let st = lock(&self.inner.state);
        let audio: HashMap<_, _> = st.audio.clone();
        (Arc::new(st.project.clone()), audio)
    }

    fn check_length(seconds: f64) -> Result<(), SessionError> {
        if seconds > MAX_RENDER_SECONDS {
            return Err(SessionError::Invalid(format!(
                "a render can be at most {} minutes long",
                MAX_RENDER_SECONDS / 60.0
            )));
        }
        Ok(())
    }

    /// Renders `[start, end)` plus `tail_seconds` at the project's sample
    /// rate, without looping. Runs on the calling thread.
    pub fn render_range(
        &self,
        start: Ticks,
        end: Ticks,
        tail_seconds: f64,
    ) -> Result<AudioData, SessionError> {
        if end <= start || start < 0 {
            return Err(SessionError::Invalid(
                "the range must start at or after 0 and end after it starts".into(),
            ));
        }
        let (project, audio) = self.render_inputs();
        let sr = project.sample_rate;
        let s = project.tempo.tick_to_sample(start, sr);
        let e = project.tempo.tick_to_sample(end, sr);
        let tail = tail_seconds.clamp(0.0, 60.0);
        Self::check_length((e - s) as f64 / sr as f64 + tail)?;
        let frames = (e - s) as usize + (tail * sr as f64) as usize;
        let snap = compile(&project, &audio, sr, 0);
        Ok(sky_engine::render(snap, s, frames, false))
    }

    /// Renders the loop `range` (or the project's loop region) so that it
    /// repeats seamlessly: tails from the end carry into the start.
    pub fn render_loop(&self, range: Option<(Ticks, Ticks)>) -> Result<AudioData, SessionError> {
        let (project, audio) = self.render_inputs();
        let (start, end) = range.unwrap_or((project.loop_region.start, project.loop_region.end));
        if end <= start || start < 0 {
            return Err(SessionError::Invalid("the loop range is empty".into()));
        }
        let sr = project.sample_rate;
        let s = project.tempo.tick_to_sample(start, sr);
        let e = project.tempo.tick_to_sample(end, sr);
        Self::check_length((e - s) as f64 / sr as f64 * 2.0)?;
        let snap = compile(&project, &audio, sr, 0);
        Ok(sky_engine::render_loop(snap, s, e, 2))
    }

    /// Renders the arrangement (or `range`) with a tail and writes a WAV file.
    pub fn export_wav(
        &self,
        path: &Path,
        range: Option<(Ticks, Ticks)>,
        tail_seconds: f64,
        format: WavFormat,
    ) -> Result<AudioData, SessionError> {
        let (start, end) = match range {
            Some(r) => r,
            None => {
                let end = self.project().content_end();
                if end <= 0 {
                    return Err(SessionError::Invalid(
                        "the project has no clips to export".into(),
                    ));
                }
                (0, end)
            }
        };
        let audio = self.render_range(start, end, tail_seconds)?;
        write_wav(path, &audio, format, format != WavFormat::Float32)?;
        Ok(audio)
    }

    /// Renders the loop and exports it, with a manifest entry, for
    /// `sky-runtime`.
    pub fn export_loop(&self, opts: &LoopExportOptions) -> Result<LoopExported, SessionError> {
        let project = self.project();
        let dir = match (&opts.dir, &self.view().path) {
            (Some(d), _) => d.clone(),
            (None, Some(p)) => p.join(file::RENDERS_DIR).join("export"),
            (None, None) => return Err(SessionError::NoProjectPath),
        };
        let (start, end) = opts
            .range
            .unwrap_or((project.loop_region.start, project.loop_region.end));
        let audio = self.render_loop(Some((start, end)))?;
        let frames = audio.frames();
        let seam = analysis::seam_check(&audio, 0, frames);
        let bars = (end - start) as f64 / project.ticks_per_bar() as f64;
        let constant_tempo = project.tempo.points().len() == 1;
        let name = opts.name.clone().unwrap_or_else(|| project.name.clone());
        let licenses = sky_model::license_report(&project);
        let exported = sky_game::export_loop(
            &dir,
            &LoopExport {
                name: &name,
                project: &project.name,
                audio: &audio,
                loop_start: 0,
                loop_end: frames as u64,
                tempo_bpm: constant_tempo.then(|| project.tempo.initial_bpm()),
                time_signature: Some([
                    project.time_signature.numerator,
                    project.time_signature.denominator,
                ]),
                bars: Some(bars),
                category: opts.category,
                tags: opts.tags.clone(),
                credits: licenses.credits.clone(),
                format: opts.format,
            },
        )?;
        Ok(LoopExported {
            exported,
            seam,
            bars,
            frames,
            sample_rate: audio.sample_rate,
            licenses,
        })
    }
}

impl Session {
    /// The folder game exports go to: the project's game export folder
    /// (relative to the project folder unless absolute), or else
    /// `renders/export` inside the project folder.
    pub fn game_export_dir(&self) -> Result<PathBuf, SessionError> {
        let view = self.view();
        let base = view.path.as_deref();
        match (view.project.game.export_dir.as_deref(), base) {
            (Some(d), _) if Path::new(d).is_absolute() => Ok(PathBuf::from(d)),
            (Some(d), Some(p)) => Ok(p.join(d)),
            (Some(d), None) => Err(SessionError::Invalid(format!(
                "the game export folder {d:?} is relative to the project folder; save the \
                 project first or set an absolute folder"
            ))),
            (None, Some(p)) => Ok(p.join(file::RENDERS_DIR).join("export")),
            (None, None) => Err(SessionError::NoProjectPath),
        }
    }

    /// Renders the project's game assets and exports them for `sky-runtime`:
    /// loops (with their intros) and SFX sets with one file per variation,
    /// all recorded in the folder's manifest.
    pub fn export_game(&self, opts: &GameExportOptions) -> Result<GameExported, SessionError> {
        let (project, audio) = self.render_inputs();
        let game = &project.game;
        if game.assets.is_empty() {
            return Err(SessionError::Invalid(
                "the project has no game assets to export; add a loop or an SFX set first".into(),
            ));
        }
        for name in &opts.only {
            if game.find(name).is_none() {
                return Err(SessionError::Invalid(format!(
                    "the project has no game asset {name:?}; it has {}",
                    game.assets
                        .iter()
                        .map(|a| a.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        }
        let dir = match &opts.dir {
            Some(d) => d.clone(),
            None => self.game_export_dir()?,
        };
        let format = WavFormat::from_bits(game.bits).unwrap_or(WavFormat::Pcm24);
        let selected: Vec<_> = game
            .assets
            .iter()
            .filter(|a| opts.only.is_empty() || opts.only.contains(&a.name))
            .collect();

        // Render first, so a bad range fails before anything is written.
        let sr = project.sample_rate;
        let to_sample = |t: Ticks| project.tempo.tick_to_sample(t, sr);
        let constant_tempo = project.tempo.points().len() == 1;
        let time_signature = Some([
            project.time_signature.numerator,
            project.time_signature.denominator,
        ]);
        let mut total_seconds = 0.0;
        let mut rendered = Vec::with_capacity(selected.len());
        for asset in &selected {
            match &asset.kind {
                GameAssetKind::Loop { range, intro_start } => {
                    let (s, e) = (to_sample(range.start), to_sample(range.end));
                    let i = intro_start.map_or(s, to_sample);
                    total_seconds += ((s - i) + 2 * (e - s)) as f64 / sr as f64;
                    Self::check_length(total_seconds)?;
                    let snap = compile(&project, &audio, sr, 0);
                    let out = if i < s {
                        sky_engine::render_intro_loop(snap, i, s, e)
                    } else {
                        sky_engine::render_loop(snap, s, e, 2)
                    };
                    let loop_start = (s - i) as u64;
                    let seam = analysis::seam_check(&out, loop_start as usize, out.frames());
                    rendered.push((vec![out], loop_start, Some(seam)));
                }
                GameAssetKind::Sfx {
                    variations,
                    tail_seconds,
                    mono,
                    ..
                } => {
                    let tail = f64::from(*tail_seconds);
                    let mut outs = Vec::with_capacity(variations.len());
                    for v in variations {
                        let (s, e) = (to_sample(v.start), to_sample(v.end));
                        total_seconds += (e - s) as f64 / sr as f64 + tail;
                        Self::check_length(total_seconds)?;
                        let frames = (e - s) as usize + (tail * sr as f64) as usize;
                        let snap = compile(&project, &audio, sr, 0);
                        let mut out = sky_engine::render(snap, s, frames, false);
                        trim_silence(&mut out);
                        if *mono {
                            fold_to_mono(&mut out);
                        }
                        outs.push(out);
                    }
                    rendered.push((outs, 0, None));
                }
            }
        }

        let licenses = sky_model::license_report(&project);
        let exports: Vec<AssetExport> = selected
            .iter()
            .zip(&rendered)
            .map(|(asset, (outs, loop_start, _))| AssetExport {
                name: asset.name.clone(),
                id: asset.id,
                category: asset.category,
                tags: asset.tags.clone(),
                credits: licenses.credits.clone(),
                audio: match &asset.kind {
                    GameAssetKind::Loop { range, .. } => AssetAudio::Loop {
                        audio: &outs[0],
                        loop_start: *loop_start,
                        loop_end: outs[0].frames() as u64,
                        tempo_bpm: constant_tempo.then(|| project.tempo.initial_bpm()),
                        time_signature,
                        bars: Some(range.length() as f64 / project.ticks_per_bar() as f64),
                    },
                    GameAssetKind::Sfx {
                        selection,
                        pitch_semitones,
                        gain_db,
                        ..
                    } => AssetAudio::Sfx {
                        variations: outs.iter().collect(),
                        selection: *selection,
                        pitch_semitones: *pitch_semitones,
                        gain_db: *gain_db,
                    },
                },
            })
            .collect();
        let set = ExportSet {
            project: project.name.clone(),
            project_id: Some(project.id.to_string()),
            prune: opts.only.is_empty(),
            format,
        };
        let out = sky_game::export_assets(&dir, &set, &exports)?;
        let assets = out
            .assets
            .into_iter()
            .zip(rendered)
            .map(|(asset, (_, _, seam))| GameAssetExported { asset, seam })
            .collect();
        Ok(GameExported {
            dir,
            manifest: out.manifest,
            assets,
            files: out.files,
            removed: out.removed,
            deleted: out.deleted,
            licenses,
        })
    }
}

/// Writes pool audio that is not in `dir` yet.
fn write_pool_audio(project: &Project, audio: &AudioCache, dir: &Path) -> Result<(), SessionError> {
    for entry in &project.audio_pool {
        let path = dir.join(&entry.file);
        if path.exists() {
            continue;
        }
        let data = audio.get(&entry.id).ok_or_else(|| {
            SessionError::Invalid(format!("audio \"{}\" is not loaded", entry.name))
        })?;
        write_wav(&path, data, WavFormat::Float32, false)?;
    }
    Ok(())
}

/// Moves queued messages to the audio thread and frees snapshots it has
/// finished with, until the session is dropped.
fn spawn_housekeeping(inner: Weak<Inner>) {
    let spawned = std::thread::Builder::new()
        .name("sky-session".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(20));
                let Some(inner) = inner.upgrade() else {
                    break;
                };
                if let Some(h) = lock(&inner.engine).as_mut() {
                    h.flush();
                    h.collect_garbage();
                }
            }
        });
    if let Err(e) = spawned {
        tracing::error!("cannot start the session thread: {e}");
    }
}
