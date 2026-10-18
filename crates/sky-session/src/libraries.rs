// SPDX-License-Identifier: GPL-3.0-or-later
//! Instrument libraries for the project's pianos: loaded once, shared by
//! every track that plays them, and kept outside the project.
//!
//! After every change the session looks for pianos whose library is not
//! loaded and loads it on a background thread, so the UI never waits; the
//! piano is silent until then. Renders and exports load what is missing
//! first, so they never miss a piano.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use serde::Serialize;
use sky_core::TrackId;
use sky_dsp::InstrumentType;
use sky_dsp::instruments::SampleLibrary;
use sky_library::{InstrumentInfo, InstrumentStatus, Instruments, find_instrument};
use sky_model::{
    Command, InstrumentSlot, LibraryRef, License, ModelError, Origin, Project, TrackKind,
    Transaction,
};

use crate::{Inner, Session, SessionError, State, lock};

/// The libraries `project`'s pianos play, by cache key.
fn needed(project: &Project) -> HashMap<String, Option<LibraryRef>> {
    project
        .tracks
        .iter()
        .filter_map(|t| t.instrument.as_ref())
        .filter(|slot| slot.kind == InstrumentType::Piano)
        .map(|slot| (slot.library_key(), slot.library.clone()))
        .collect()
}

/// What [`Session::set_piano_library`] did.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibrarySet {
    pub track: TrackId,
    pub library: String,
    pub version: String,
    pub license: License,
    pub credit: Option<String>,
    /// True when it was downloaded this time.
    pub downloaded: bool,
    pub samples: usize,
    pub zones: usize,
    /// The most velocity layers any key has.
    pub layers: usize,
    pub lowest_key: u8,
    pub highest_key: u8,
    pub stereo: bool,
    /// Memory the samples take, in MB.
    pub memory_mb: f64,
}

impl Session {
    /// Where instrument libraries come from.
    pub fn instruments(&self) -> Instruments {
        lock(&self.inner.instruments).clone()
    }

    /// Uses `instruments` for libraries from now on, such as a folder for
    /// tests.
    pub fn set_instruments(&self, instruments: Instruments) {
        *lock(&self.inner.instruments) = instruments;
    }

    /// Every library Skyware Sound knows, and whether it is installed.
    pub fn instrument_libraries(&self) -> Vec<InstrumentStatus> {
        self.instruments().list()
    }

    /// Starts loading, on a background thread, each library the project's
    /// pianos need that is neither loaded nor loading nor failed. The
    /// thread recompiles the project once its library is in.
    pub(crate) fn load_libraries_later(&self, st: &mut State) {
        let wanted: Vec<(String, Option<LibraryRef>)> = needed(&st.project)
            .into_iter()
            .filter(|(key, _)| {
                !st.libraries.contains_key(key)
                    && !st.loading.contains(key)
                    && !st.failed.contains_key(key)
            })
            .collect();
        for (key, reference) in wanted {
            st.loading.insert(key.clone());
            let inner = Arc::downgrade(&self.inner);
            let spawned = std::thread::Builder::new()
                .name("sky-instrument-load".into())
                .spawn(move || load_in_background(inner, key, reference));
            if let Err(e) = spawned {
                tracing::error!("cannot start loading an instrument library: {e}");
            }
        }
    }

    /// Loads every library the project's pianos need that is not loaded
    /// yet, on this thread. Returns the first failure.
    pub fn load_libraries(&self) -> Result<(), SessionError> {
        let missing: Vec<(String, Option<LibraryRef>)> = {
            let st = lock(&self.inner.state);
            needed(&st.project)
                .into_iter()
                .filter(|(key, _)| !st.libraries.contains_key(key))
                .collect()
        };
        if missing.is_empty() {
            return Ok(());
        }
        let instruments = self.instruments();
        let mut first_error = None;
        let mut loaded = Vec::new();
        for (key, reference) in missing {
            match instruments.load_ref(reference.as_ref()) {
                Ok(lib) => loaded.push((key, lib)),
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        let mut st = lock(&self.inner.state);
        let any = !loaded.is_empty();
        for (key, lib) in loaded {
            st.loading.remove(&key);
            st.failed.remove(&key);
            st.libraries.entry(key).or_insert(lib);
        }
        if any {
            self.publish(&mut st, true);
        }
        match first_error {
            Some(e) => Err(e.into()),
            None => Ok(()),
        }
    }

    /// True when every piano in the project has its library loaded.
    pub fn libraries_ready(&self) -> bool {
        let st = lock(&self.inner.state);
        needed(&st.project)
            .keys()
            .all(|k| st.libraries.contains_key(k))
    }

    /// The loaded library a track's piano plays, if it is loaded.
    pub fn track_library(&self, track: TrackId) -> Option<Arc<SampleLibrary>> {
        let st = lock(&self.inner.state);
        let slot = st.project.track(track).ok()?.instrument.as_ref()?;
        (slot.kind == InstrumentType::Piano)
            .then(|| st.libraries.get(&slot.library_key()).cloned())
            .flatten()
    }

    /// Why a track's piano library could not be loaded, if it could not.
    pub fn track_library_error(&self, track: TrackId) -> Option<String> {
        let st = lock(&self.inner.state);
        let slot = st.project.track(track).ok()?.instrument.as_ref()?;
        st.failed.get(&slot.library_key()).cloned()
    }

    /// Downloads and unpacks a library, reporting bytes received and the
    /// total. Does nothing for one that is installed.
    pub fn install_library(
        &self,
        name: &str,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<&'static InstrumentInfo, SessionError> {
        let info = known(name)?;
        self.instruments().install(info, progress)?;
        Ok(info)
    }

    /// Makes a track's instrument a piano playing library `name`, as one
    /// undo step, keeping its settings when it is a piano already. With
    /// `download`, a library that is not installed is downloaded first;
    /// without, that is an error. Loading happens before anything changes,
    /// with nothing locked, so this can take a while.
    pub fn set_piano_library(
        &self,
        track: TrackId,
        name: &str,
        download: bool,
        origin: Origin,
    ) -> Result<LibrarySet, SessionError> {
        let info = known(name)?;
        if info.kind != InstrumentType::Piano {
            return Err(SessionError::Invalid(format!(
                "{} is not a piano library",
                info.name
            )));
        }
        if self.project().track(track)?.kind != TrackKind::Instrument {
            return Err(ModelError::NotAnInstrumentTrack(track).into());
        }
        let instruments = self.instruments();
        let installed = instruments.is_installed(info);
        if !installed {
            if !download {
                return Err(SessionError::Invalid(format!(
                    "{} is not downloaded yet ({:.0} MB); download it first",
                    info.name,
                    info.size as f64 / 1e6
                )));
            }
            instruments.install(info, &mut |_, _| {})?;
        }
        // The built-in piano is what a slot without a library plays.
        let reference = (!info.is_bundled()).then(|| info.reference());
        let key = reference.as_ref().map(LibraryRef::key).unwrap_or_default();
        let cached = lock(&self.inner.state).libraries.get(&key).cloned();
        let lib = match cached {
            Some(lib) => lib,
            None => Arc::new(instruments.load(info)?),
        };

        let mut st = lock(&self.inner.state);
        let t = st.project.track(track)?;
        if t.kind != TrackKind::Instrument {
            return Err(ModelError::NotAnInstrumentTrack(track).into());
        }
        let track_name = t.name.clone();
        let mut slot = match &t.instrument {
            Some(s) if s.kind == InstrumentType::Piano => s.clone(),
            _ => InstrumentSlot::new(InstrumentType::Piano),
        };
        slot.library = reference;
        st.failed.remove(&key);
        st.libraries.entry(key).or_insert(lib.clone());
        let tx = Transaction::new(
            format!("Play {} on {track_name}", info.name),
            origin,
            vec![Command::SetInstrument {
                track,
                instrument: Some(slot),
            }],
        );
        self.apply_locked(&mut st, tx)?;
        let (lowest_key, highest_key) = lib.key_range().unwrap_or((0, 0));
        Ok(LibrarySet {
            track,
            library: info.name.to_string(),
            version: info.version.to_string(),
            license: info.license,
            credit: info.credit.map(str::to_string),
            downloaded: !installed,
            samples: lib.samples().len(),
            zones: lib.zones().len(),
            layers: (lowest_key..=highest_key)
                .map(|k| lib.layers(k))
                .max()
                .unwrap_or(0),
            lowest_key,
            highest_key,
            stereo: lib.is_stereo(),
            memory_mb: lib.bytes() as f64 / 1e6,
        })
    }
}

fn known(name: &str) -> Result<&'static InstrumentInfo, SessionError> {
    find_instrument(name).ok_or_else(|| {
        SessionError::Invalid(format!(
            "there is no instrument library {name:?}; there are {}",
            sky_library::INSTRUMENTS
                .iter()
                .map(|i| i.id)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })
}

fn load_in_background(inner: Weak<Inner>, key: String, reference: Option<LibraryRef>) {
    let Some(strong) = inner.upgrade() else {
        return;
    };
    let instruments = lock(&strong.instruments).clone();
    drop(strong);
    let result = instruments.load_ref(reference.as_ref());
    let Some(inner) = inner.upgrade() else {
        return;
    };
    let session = Session { inner };
    let mut st = lock(&session.inner.state);
    st.loading.remove(&key);
    match result {
        Ok(lib) => {
            st.libraries.entry(key).or_insert(lib);
            session.publish(&mut st, true);
        }
        Err(e) => {
            let name = reference
                .as_ref()
                .map_or("the built-in piano", |r| r.name.as_str());
            tracing::warn!("cannot load {name}: {e}");
            st.status = Some(format!("Cannot load {name}: {e}"));
            st.failed.insert(key, e.to_string());
            session.publish(&mut st, false);
        }
    }
}
