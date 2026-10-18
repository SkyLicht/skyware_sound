// SPDX-License-Identifier: GPL-3.0-or-later
//! The sounds a pattern asks for, loaded the way Strudel loads them: a
//! `samples('github:tidalcycles/dirt-samples')` line links a pack to the
//! track's sample bank, and each sound the pattern names that the bank
//! lacks then loads from the bank's linked packs. A `samples({ ... })` map
//! written in the pattern loads all of its sounds.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use sky_core::TrackId;
use sky_dsp::InstrumentType;
use sky_library::{MAX_FILES, MapFile, Pack, PackSource, SampleMap, Selection, expand_github};
use sky_model::{
    Command, InstrumentSlot, License, ModelError, Origin, PackLink, SampleBank, TrackKind,
    Transaction,
};

use crate::{Session, SessionError, lock, pool_loaded};

/// What [`Session::load_pattern_sounds`] loads.
#[derive(Clone, Debug, Default)]
pub struct SoundsRequest {
    /// Packs to link, from `samples('…')` lines. They are searched before
    /// the packs linked already, and each must be readable.
    pub link: Vec<PackSource>,
    /// Maps from `samples({ … })` calls; all of their sounds load.
    pub maps: Vec<SampleMap>,
    /// The sounds the pattern names.
    pub names: Vec<String>,
}

/// What [`Session::load_pattern_sounds`] did.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SoundsLoaded {
    pub track: TrackId,
    /// The bank's name.
    pub bank: String,
    /// The packs this request linked, by name.
    pub linked: Vec<String>,
    /// The packs the bank is linked to now, in the order they are searched.
    pub packs: Vec<PackLink>,
    /// The sounds loaded into the bank, each with the pack it came from.
    pub loaded: Vec<(String, String)>,
    /// Names the pattern uses that neither the bank nor its packs have.
    pub missing: Vec<String>,
    /// Files fetched and added to the pool this time.
    pub new_files: usize,
    /// Files already in the pool from an earlier load.
    pub reused_files: usize,
    /// Files that could not be loaded, and linked packs that could not be
    /// read.
    pub failed: Vec<String>,
    /// True when a pack's map came from its saved copy because the network
    /// failed.
    pub offline: bool,
}

/// The sound names in mini-notation, in text order and each once. Empty
/// when the text does not parse.
pub fn pattern_sound_names(text: &str) -> Vec<String> {
    let Ok(pattern) = sky_pattern::parse(text) else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    pattern
        .atoms()
        .into_iter()
        .filter_map(|(v, _)| match v {
            sky_pattern::Value::Sound { name, .. } => Some(name.clone()),
            _ => None,
        })
        .filter(|n| seen.insert(n.to_ascii_lowercase()))
        .collect()
}

fn link_of(source: &PackSource) -> PackLink {
    PackLink {
        name: source.name.clone(),
        map: source.map.clone(),
        license: source.license,
        credit: source.credit.clone(),
    }
}

fn source_of(link: &PackLink) -> PackSource {
    PackSource {
        name: link.name.clone(),
        map: link.map.clone(),
        license: link.license,
        credit: link.credit.clone(),
    }
}

/// True when two addresses name the same map, such as `github:a/b` and
/// `github:a/b/main`.
fn same_map(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        expand_github(s.trim())
            .trim_end_matches('/')
            .to_ascii_lowercase()
    };
    norm(a) == norm(b)
}

/// What a map written in a pattern is called: the host its first file comes
/// from, such as `cdn.freesound.org`.
fn map_name(map: &SampleMap) -> String {
    let first = map
        .sounds
        .values()
        .flatten()
        .next()
        .map(|f| map.locate(f))
        .unwrap_or_default();
    first
        .split_once("://")
        .and_then(|(_, rest)| rest.split('/').next())
        .filter(|host| !host.is_empty())
        .map_or_else(|| "Written samples".to_string(), str::to_string)
}

fn too_many(count: usize) -> SessionError {
    SessionError::Invalid(format!(
        "the pattern asks for {count} files, more than the {MAX_FILES} one load can fetch; \
         name fewer sounds"
    ))
}

impl Session {
    /// Loads what a pattern asks for into its track's sample bank, then
    /// applies `then` (such as the pattern change), all as one undo step:
    /// `request.link` packs are linked to the bank, every sound of
    /// `request.maps` loads, and each name in `request.names` that the bank
    /// lacks loads from the first linked pack that has it. The track
    /// becomes a sample bank if it was another instrument. Downloads happen
    /// first, with nothing locked, so this can take a while.
    ///
    /// A newly linked pack that cannot be read is an error, and so is a
    /// written map none of whose files load. Packs linked before and sounds
    /// that fail to load are listed in [`SoundsLoaded::failed`] instead, so
    /// the pattern still changes.
    pub fn load_pattern_sounds(
        &self,
        track: TrackId,
        request: &SoundsRequest,
        label: &str,
        then: Vec<Command>,
        origin: Origin,
    ) -> Result<SoundsLoaded, SessionError> {
        let current = {
            let project = self.project();
            let t = project.track(track)?;
            if t.kind != TrackKind::Instrument {
                return Err(ModelError::NotAnInstrumentTrack(track).into());
            }
            t.instrument
                .as_ref()
                .filter(|s| s.kind == InstrumentType::Bank)
                .and_then(|s| s.bank.clone())
        };
        let mut links: Vec<PackSource> = Vec::new();
        let earlier = current.iter().flat_map(|b| b.packs.iter().map(source_of));
        for source in request.link.iter().cloned().chain(earlier) {
            if !links.iter().any(|l| same_map(&l.map, &source.map)) {
                links.push(source);
            }
        }
        let fresh = |s: &PackSource| request.link.iter().any(|r| same_map(&r.map, &s.map));

        let library = self.library();
        let mut loads = Vec::new();
        let mut failed = Vec::new();
        let mut offline = false;
        let mut files = 0;
        // Names the written maps give, which the bank then has.
        let mut written = BTreeSet::new();
        for map in &request.maps {
            let sounds: BTreeMap<String, Vec<MapFile>> = map
                .sounds
                .iter()
                .filter(|(name, _)| sky_library::valid_sound_name(name))
                .map(|(name, f)| (name.clone(), f.clone()))
                .collect();
            if sounds.is_empty() {
                return Err(SessionError::Invalid(
                    "the map in samples({ ... }) has no sound names a project can store; use \
                     letters, digits, _ - . or #"
                        .into(),
                ));
            }
            written.extend(sounds.keys().map(|n| n.to_ascii_lowercase()));
            let pack = Pack {
                source: PackSource {
                    name: map_name(map),
                    map: String::new(),
                    license: License::Unknown,
                    credit: None,
                },
                location: String::new(),
                map: map.clone(),
                offline: false,
            };
            let selection = Selection {
                name: pack.source.name.clone(),
                sounds,
            };
            files += selection.file_count();
            if files > MAX_FILES {
                return Err(too_many(files));
            }
            let loaded = library.load(&pack, &selection)?;
            failed.extend(loaded.failed.iter().cloned());
            loads.push(loaded);
        }

        let has = |name: &str| {
            written.contains(&name.to_ascii_lowercase())
                || current.as_ref().is_some_and(|b| b.resolve(name).is_some())
        };
        let mut wanted: Vec<String> = Vec::new();
        for name in &request.names {
            let name = name.trim();
            if sky_library::valid_sound_name(name)
                && !has(name)
                && !wanted.iter().any(|w| w.eq_ignore_ascii_case(name))
            {
                wanted.push(name.to_string());
            }
        }
        let mut linked = Vec::new();
        for source in &links {
            let is_fresh = fresh(source);
            if !is_fresh && wanted.is_empty() {
                continue;
            }
            let pack = match library.open(source) {
                Ok(p) => p,
                Err(e) if is_fresh => return Err(e.into()),
                Err(e) => {
                    failed.push(e.to_string());
                    continue;
                }
            };
            offline |= pack.offline;
            if is_fresh {
                linked.push(source.name.clone());
            }
            let mut chosen = BTreeMap::new();
            wanted.retain(|name| match pack.map.find_sound(name) {
                Some((key, f)) if sky_library::valid_sound_name(key) => {
                    chosen.insert(key.to_string(), f.to_vec());
                    false
                }
                _ => true,
            });
            if chosen.is_empty() {
                continue;
            }
            let selection = Selection {
                name: source.name.clone(),
                sounds: chosen,
            };
            files += selection.file_count();
            if files > MAX_FILES {
                return Err(too_many(files));
            }
            match library.load(&pack, &selection) {
                Ok(loaded) => {
                    failed.extend(loaded.failed.iter().cloned());
                    loads.push(loaded);
                }
                Err(e) => failed.push(e.to_string()),
            }
        }

        let mut st = lock(&self.inner.state);
        let t = st.project.track(track)?;
        if t.kind != TrackKind::Instrument {
            return Err(ModelError::NotAnInstrumentTrack(track).into());
        }
        let was_bank = t
            .instrument
            .as_ref()
            .is_some_and(|s| s.kind == InstrumentType::Bank);
        let mut slot = match &t.instrument {
            Some(s) if was_bank => s.clone(),
            _ => InstrumentSlot::new(InstrumentType::Bank),
        };
        let first_name = linked
            .first()
            .cloned()
            .or_else(|| loads.first().map(|l| l.source.name.clone()))
            .unwrap_or_else(|| "Samples".into());
        let mut bank = slot.bank.take().unwrap_or_else(|| SampleBank {
            name: first_name,
            sounds: BTreeMap::new(),
            pitched: None,
            packs: Vec::new(),
        });
        let mut commands = Vec::new();
        let mut added = Vec::new();
        let (mut new_files, mut reused_files) = (0, 0);
        let mut loaded = Vec::new();
        for load in &loads {
            let pooled = match pool_loaded(&mut st, load, &mut commands, &mut added) {
                Ok(p) => p,
                Err(e) => {
                    for id in added {
                        st.audio.remove(&id);
                    }
                    return Err(e);
                }
            };
            for p in pooled {
                new_files += p.new_files;
                reused_files += p.reused_files;
                loaded.push((p.name.clone(), load.source.name.clone()));
                bank.sounds.insert(p.name, p.variations);
            }
        }
        let packs: Vec<PackLink> = links.iter().map(link_of).collect();
        let changed = !was_bank || !loaded.is_empty() || bank.packs != packs;
        bank.packs = packs;
        if changed {
            slot.bank = Some(bank.clone());
            commands.push(Command::SetInstrument {
                track,
                instrument: Some(slot),
            });
        }
        commands.extend(then);
        if !commands.is_empty() {
            let tx = Transaction::new(label, origin, commands);
            if let Err(e) = self.apply_locked(&mut st, tx) {
                for id in added {
                    st.audio.remove(&id);
                }
                return Err(e);
            }
        }
        Ok(SoundsLoaded {
            track,
            bank: bank.name,
            linked,
            packs: bank.packs,
            loaded,
            missing: wanted,
            new_files,
            reused_files,
            failed,
            offline,
        })
    }
}
