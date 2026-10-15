// SPDX-License-Identifier: GPL-3.0-or-later
//! Putting audio into the project in one undo step: a file from disk, a
//! sound from a pack or audio already in the pool, onto a track as a clip,
//! into a sampler or into a sample bank. This is what dropping something
//! from the browser does.

use std::path::PathBuf;
use std::sync::Arc;

use sky_core::{AudioId, ClipId, Ticks, TrackId};
use sky_dsp::{AudioData, InstrumentType};
use sky_io::{WavFormat, read_audio, write_wav};
use sky_model::file;
use sky_model::{
    AudioSource, BankSample, Clip, ClipContent, Command, Origin, PoolEntry, Project, SampleBank,
    Track, TrackKind, Transaction,
};

use crate::{Session, SessionError, lock};

/// Audio for [`Session::place_audio`].
#[derive(Clone, Debug)]
pub enum AudioInput {
    /// Audio already in the pool.
    Pool(AudioId),
    /// An audio file on disk. It joins the pool as the user's own, with no
    /// pack or license.
    File(PathBuf),
    /// A sound fetched from a pack. A file already in the pool from the
    /// same address is used again.
    Pack {
        /// What to call it in the pool, such as `RolandTR808 bd`.
        name: String,
        /// The sound's name in the pack, such as `bd`: what it is called in
        /// a sample bank.
        sound: String,
        audio: Arc<AudioData>,
        source: AudioSource,
        /// The note it sounds at, for a pitched sound.
        root: Option<u8>,
    },
}

/// Where [`Session::place_audio`] puts audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioTarget {
    /// Only into the pool.
    Pool,
    /// A clip from `start` on an audio track.
    Clip { track: TrackId, start: Ticks },
    /// A new audio track, at `index` or else after the last, with a clip
    /// from `start`.
    NewTrack { start: Ticks, index: Option<usize> },
    /// The sample a sampler track plays.
    Sampler(TrackId),
    /// A sound of a sample-bank track: a new sound, or one more variation
    /// of the sound with the same name.
    Bank(TrackId),
}

/// What [`Session::place_audio`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    pub audio: AudioId,
    /// The track it went on, if any.
    pub track: Option<TrackId>,
    /// True when that track was added for it.
    pub new_track: bool,
    pub clip: Option<ClipId>,
    /// In a sample bank: the sound and its variation, from 0.
    pub sound: Option<(String, usize)>,
    /// True when the audio was in the pool already.
    pub reused: bool,
}

/// What [`Session::import_files`] brought in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportedFiles {
    pub audio: Vec<AudioId>,
    /// The files that could not be read, each with why.
    pub failed: Vec<String>,
}

/// A clip that plays a pool entry once from `start`, as long as the audio
/// lasts at the project's tempo.
pub fn audio_clip(project: &Project, entry: &PoolEntry, start: Ticks) -> Clip {
    let seconds = entry.frames as f64 / f64::from(entry.sample_rate.max(1));
    let start = start.max(0);
    let begin = project.tempo.tick_to_seconds(start);
    let end = project.tempo.seconds_to_ticks(begin + seconds).ceil() as Ticks;
    Clip {
        id: ClipId::new(),
        name: entry.name.clone(),
        start,
        length: (end - start).max(1),
        content: ClipContent::Audio {
            source: entry.id,
            offset: 0,
            gain_db: 0.0,
            fade_in: 0,
            fade_out: 0,
        },
    }
}

/// A sample-bank sound name from a file or pool name: lower case, with
/// anything a pattern cannot type replaced.
pub fn sound_name(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '#') {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    let cleaned = cleaned.trim_matches('_');
    if cleaned.is_empty() {
        "sound".to_string()
    } else {
        cleaned.to_string()
    }
}

/// The audio as it arrives: in the pool already, or decoded with where it
/// came from.
enum Have {
    Pool(AudioId),
    Decoded(Arc<AudioData>, Option<AudioSource>),
}

fn not_for(track: &Track, what: &str) -> SessionError {
    SessionError::Invalid(format!("{} is not {what}", track.name))
}

impl Session {
    /// Puts audio into the project as one undo step: into the pool unless
    /// it is there already, then where `target` says.
    pub fn place_audio(
        &self,
        input: AudioInput,
        target: AudioTarget,
        origin: Origin,
    ) -> Result<Placed, SessionError> {
        // Decode before locking: a long file must not hold up the UI.
        let (name, sound, have, root) = match input {
            AudioInput::Pool(id) => {
                let project = self.project();
                let entry = project.pool_entry(id)?;
                (
                    entry.name.clone(),
                    sound_name(&entry.name),
                    Have::Pool(id),
                    None,
                )
            }
            AudioInput::File(path) => {
                let audio = read_audio(&path)?;
                if audio.frames() == 0 {
                    return Err(SessionError::Invalid(format!(
                        "{} has no audio in it",
                        path.display()
                    )));
                }
                let name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "audio".into());
                let sound = sound_name(&name);
                (name, sound, Have::Decoded(Arc::new(audio), None), None)
            }
            AudioInput::Pack {
                name,
                sound,
                audio,
                source,
                root,
            } => {
                if audio.frames() == 0 {
                    return Err(SessionError::Invalid(format!("{name} has no audio in it")));
                }
                let sound = sound_name(&sound);
                (name, sound, Have::Decoded(audio, Some(source)), root)
            }
        };

        let mut st = lock(&self.inner.state);
        // The pool entry, and the audio when it is new to the pool.
        let (entry, new_audio) = match have {
            Have::Pool(id) => (st.project.pool_entry(id)?.clone(), None),
            Have::Decoded(audio, source) => {
                let same = source.as_ref().and_then(|src| {
                    st.project
                        .audio_pool
                        .iter()
                        .find(|e| e.source.as_ref().is_some_and(|s| s.url == src.url))
                });
                match same {
                    Some(e) => (e.clone(), None),
                    None => {
                        let id = AudioId::new();
                        let entry = PoolEntry {
                            id,
                            name,
                            file: format!("{}/{id}.wav", file::AUDIO_DIR),
                            channels: audio.channel_count() as u16,
                            sample_rate: audio.sample_rate,
                            frames: audio.frames() as u64,
                            source,
                        };
                        (entry, Some(audio))
                    }
                }
            }
        };
        let (label, placed, placing) = place_commands(&st.project, &entry, &sound, root, target)?;
        let placed = Placed {
            reused: new_audio.is_none(),
            ..placed
        };
        let mut commands = Vec::new();
        if let Some(audio) = &new_audio {
            if let Some(dir) = &st.path {
                write_wav(&dir.join(&entry.file), audio, WavFormat::Float32, false)?;
            }
            st.audio.insert(entry.id, audio.clone());
            commands.push(Command::AddPoolAudio {
                entry: entry.clone(),
                index: None,
            });
        }
        commands.extend(placing);
        if commands.is_empty() {
            // Into the pool, where it already is.
            return Ok(placed);
        }
        let tx = Transaction::new(label, origin, commands);
        if let Err(e) = self.apply_locked(&mut st, tx) {
            if new_audio.is_some() {
                st.audio.remove(&entry.id);
            }
            return Err(e);
        }
        Ok(placed)
    }

    /// Decodes audio files into the pool as one undo step. A file that
    /// cannot be read is left out and listed; it is an error only when
    /// none can.
    pub fn import_files(
        &self,
        paths: &[PathBuf],
        origin: Origin,
    ) -> Result<ImportedFiles, SessionError> {
        let mut decoded = Vec::new();
        let mut failed = Vec::new();
        for path in paths {
            match read_audio(path) {
                Ok(audio) if audio.frames() > 0 => decoded.push((path, audio)),
                Ok(_) => failed.push(format!("{} has no audio in it", path.display())),
                Err(e) => failed.push(e.to_string()),
            }
        }
        if decoded.is_empty() {
            return Err(SessionError::Invalid(match failed.first() {
                Some(first) if paths.len() > 1 => {
                    format!("none of the {} files could be read: {first}", paths.len())
                }
                Some(first) => first.clone(),
                None => "no files to import".to_string(),
            }));
        }
        let mut st = lock(&self.inner.state);
        let mut commands = Vec::new();
        let mut ids = Vec::new();
        let mut label = String::new();
        let mut written = Ok(());
        for (path, audio) in decoded {
            let id = AudioId::new();
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "audio".into());
            let entry = PoolEntry {
                id,
                name: name.clone(),
                file: format!("{}/{id}.wav", file::AUDIO_DIR),
                channels: audio.channel_count() as u16,
                sample_rate: audio.sample_rate,
                frames: audio.frames() as u64,
                source: None,
            };
            if let Some(dir) = &st.path
                && let Err(e) = write_wav(&dir.join(&entry.file), &audio, WavFormat::Float32, false)
            {
                written = Err(e);
                break;
            }
            st.audio.insert(id, Arc::new(audio));
            ids.push(id);
            label = format!("Import {name}");
            commands.push(Command::AddPoolAudio { entry, index: None });
        }
        if ids.len() > 1 {
            label = format!("Import {} files", ids.len());
        }
        let applied = written
            .map_err(SessionError::from)
            .and_then(|()| self.apply_locked(&mut st, Transaction::new(label, origin, commands)));
        if let Err(e) = applied {
            for id in &ids {
                st.audio.remove(id);
            }
            return Err(e);
        }
        Ok(ImportedFiles { audio: ids, failed })
    }
}

/// The undo step's label, what it places, and the commands that put pool
/// audio `entry` where `target` says.
fn place_commands(
    project: &Project,
    entry: &PoolEntry,
    sound: &str,
    root: Option<u8>,
    target: AudioTarget,
) -> Result<(String, Placed, Vec<Command>), SessionError> {
    let mut commands = Vec::new();
    let mut placed = Placed {
        audio: entry.id,
        track: None,
        new_track: false,
        clip: None,
        sound: None,
        reused: false,
    };
    let name = &entry.name;
    let label = match target {
        AudioTarget::Pool => format!("Import {name}"),
        AudioTarget::Clip { track, start } => {
            let t = project.track(track)?;
            if t.kind != TrackKind::Audio {
                return Err(not_for(t, "an audio track"));
            }
            let clip = audio_clip(project, entry, start);
            placed.track = Some(track);
            placed.clip = Some(clip.id);
            commands.push(Command::AddClip {
                track,
                clip: Box::new(clip),
            });
            format!("Add {name}")
        }
        AudioTarget::NewTrack { start, index } => {
            let track = Track::new_audio(project.unique_track_name(name));
            let clip = audio_clip(project, entry, start);
            placed.track = Some(track.id);
            placed.new_track = true;
            placed.clip = Some(clip.id);
            let id = track.id;
            commands.push(Command::AddTrack {
                track: Box::new(track),
                index: index.map(|i| i.min(project.tracks.len())),
            });
            commands.push(Command::AddClip {
                track: id,
                clip: Box::new(clip),
            });
            format!("Add {name} on a new track")
        }
        AudioTarget::Sampler(track) => {
            let t = project.track(track)?;
            let Some(mut slot) = t
                .instrument
                .clone()
                .filter(|s| s.kind == InstrumentType::Sampler)
            else {
                return Err(not_for(t, "a sampler"));
            };
            slot.sample = Some(entry.id);
            placed.track = Some(track);
            commands.push(Command::SetInstrument {
                track,
                instrument: Some(slot),
            });
            format!("Load {name} into {}", t.name)
        }
        AudioTarget::Bank(track) => {
            let t = project.track(track)?;
            let Some(mut slot) = t
                .instrument
                .clone()
                .filter(|s| s.kind == InstrumentType::Bank)
            else {
                return Err(not_for(t, "a sample bank"));
            };
            let mut bank = slot.bank.take().unwrap_or_else(|| SampleBank {
                name: entry
                    .source
                    .as_ref()
                    .map_or_else(|| t.name.clone(), |s| s.pack.clone()),
                sounds: Default::default(),
                pitched: None,
            });
            let variations = bank.sounds.entry(sound.to_string()).or_default();
            variations.push(BankSample {
                audio: entry.id,
                root,
            });
            placed.track = Some(track);
            placed.sound = Some((sound.to_string(), variations.len() - 1));
            slot.bank = Some(bank);
            commands.push(Command::SetInstrument {
                track,
                instrument: Some(slot),
            });
            format!("Add {name} to {}", t.name)
        }
    };
    Ok((label, placed, commands))
}
