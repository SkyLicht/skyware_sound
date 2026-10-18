// SPDX-License-Identifier: GPL-3.0-or-later
//! Sound packs: Strudel sample maps (`strudel.json`), the packs Strudel
//! loads by default, and a download cache so each file is fetched once.
//!
//! Loading a bank is two steps. [`Library::open`] reads a pack's map, from
//! the network or a local folder. [`Pack::select`] picks the sounds (a bank
//! such as `RolandTR808`, a list of names, or both), and [`Library::load`]
//! downloads and decodes them. The session then puts them into the
//! project's audio pool with the pack's license, so an export knows what it
//! must credit or check.
//!
//! Nothing here touches a project; it only hands back decoded audio.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sky_dsp::AudioData;
use sky_model::License;
use thiserror::Error;

pub mod instruments;
pub mod map;
pub mod packs;
pub mod samples;
pub mod sfz;
mod steinway_b;

pub use instruments::{
    DEFAULT_PIANO, INSTRUMENTS, InstrumentInfo, InstrumentSource, InstrumentStatus, Instruments,
    Stage, find_instrument,
};
pub use map::{MapFile, SampleMap, expand_github};
pub use packs::{PACKS, PackInfo};
pub use samples::{CallError, Samples, SamplesCall, find_samples};

/// The most files one load may fetch. A bank of a drum machine is around a
/// hundred; asking for a whole large pack at once is almost always a mistake.
pub const MAX_FILES: usize = 400;
/// The largest single sample file accepted.
const MAX_FILE_BYTES: u64 = 64 << 20;
/// The largest sample map accepted.
const MAX_MAP_BYTES: u64 = 16 << 20;
/// Downloads that run at once.
const WORKERS: usize = 8;
/// The longest sound name a project accepts.
const MAX_NAME: usize = 64;
/// The largest cover image accepted.
const MAX_COVER_BYTES: u64 = 8 << 20;
/// How long a pack found to have no cover goes before it is looked at again.
const COVER_RECHECK: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("cannot read the sample map {location}: {message}")]
    Map { location: String, message: String },
    #[error("{0}")]
    Select(String),
    #[error("none of the {count} files could be loaded; the first problem was: {first}")]
    NothingLoaded { count: usize, first: String },
    #[error("{0}")]
    Instrument(String),
}

/// Where to load a pack from and what its license is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackSource {
    pub name: String,
    /// The sample map: an http(s) URL, a `github:user/repo` shortcut, or a
    /// path to a `strudel.json` file or a folder holding one.
    pub map: String,
    pub license: License,
    pub credit: Option<String>,
}

impl From<&PackInfo> for PackSource {
    fn from(p: &PackInfo) -> Self {
        Self {
            name: p.name.to_string(),
            map: p.map.to_string(),
            license: p.license,
            credit: p.credit.map(str::to_string),
        }
    }
}

impl PackSource {
    /// A known pack by id or name. Anything else is a map address, with
    /// `license` (unknown when not given): only the person loading it can
    /// say what its terms are.
    pub fn parse(text: &str, license: Option<License>) -> PackSource {
        if let Some(p) = packs::find(text) {
            let mut source = PackSource::from(p);
            if let Some(l) = license {
                source.license = l;
            }
            return source;
        }
        let text = text.trim();
        let name = text
            .trim_end_matches(['/', '\\'])
            .trim_end_matches("strudel.json")
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\', ':'])
            .find(|s| !s.is_empty() && *s != "main" && *s != "master")
            .unwrap_or(text)
            .to_string();
        PackSource {
            name,
            map: text.to_string(),
            license: license.unwrap_or(License::Unknown),
            credit: None,
        }
    }
}

/// A pack whose sample map has been read.
#[derive(Clone, Debug)]
pub struct Pack {
    pub source: PackSource,
    /// Where the map was read from.
    pub location: String,
    pub map: SampleMap,
    /// True when the network failed and the map came from the cache.
    pub offline: bool,
}

/// The sounds chosen from a pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    /// What to call the bank: the bank name, the one sound, or the pack.
    pub name: String,
    pub sounds: BTreeMap<String, Vec<MapFile>>,
}

impl Selection {
    pub fn file_count(&self) -> usize {
        self.sounds.values().map(Vec::len).sum()
    }
}

/// True for names a project can store and a pattern can mostly type.
pub fn valid_sound_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '#'))
}

fn list<'a>(names: impl Iterator<Item = &'a String>, max: usize) -> String {
    let names: Vec<&String> = names.collect();
    let mut s = names
        .iter()
        .take(max)
        .map(|n| n.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > max {
        s.push_str(&format!(" and {} more", names.len() - max));
    }
    s
}

impl Pack {
    /// Picks the sounds to load: `sounds` from `bank` (named without its
    /// prefix), or every sound of the bank when `sounds` is empty. Without a
    /// bank, `sounds` may name any sound by its full name, and an empty list
    /// picks the sounds that belong to no bank. Names ignore case.
    pub fn select(&self, bank: Option<&str>, sounds: &[String]) -> Result<Selection, LibraryError> {
        let bank = bank.map(str::trim).filter(|b| !b.is_empty());
        let all = sounds.iter().all(|s| s.trim().is_empty());
        let available = if bank.is_none() && all {
            self.map.plain_sounds()
        } else {
            self.map.bank_sounds(bank)
        };
        if let Some(b) = bank
            && available.is_empty()
        {
            let banks = self.map.banks();
            return Err(LibraryError::Select(if banks.is_empty() {
                format!("{} has no banks; leave the bank out", self.source.name)
            } else {
                format!(
                    "{} has no bank {b:?}; its banks are {}",
                    self.source.name,
                    list(banks.keys(), 30)
                )
            }));
        }
        let mut chosen = BTreeMap::new();
        if all {
            for (name, files) in &available {
                if valid_sound_name(name) {
                    chosen.insert(name.clone(), files.to_vec());
                }
            }
        } else {
            for want in sounds.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
                let Some((name, files)) = available
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(want))
                else {
                    return Err(LibraryError::Select(format!(
                        "{}{} has no sound {want:?}; it has {}",
                        self.source.name,
                        bank.map(|b| format!(" bank {b}")).unwrap_or_default(),
                        list(available.keys(), 40)
                    )));
                };
                if !valid_sound_name(name) {
                    return Err(LibraryError::Select(format!(
                        "the sound name {name:?} has characters a project cannot store"
                    )));
                }
                chosen.insert(name.clone(), files.to_vec());
            }
        }
        if chosen.is_empty() {
            let banks = self.map.banks();
            return Err(LibraryError::Select(
                if bank.is_none() && !banks.is_empty() {
                    format!(
                        "every sound in {} is in a bank; choose one of {}",
                        self.source.name,
                        list(banks.keys(), 30)
                    )
                } else {
                    format!("{} has no sounds to load", self.source.name)
                },
            ));
        }
        let selection = Selection {
            name: match (bank, chosen.len()) {
                (Some(b), _) => b.to_string(),
                (None, 1) => chosen.keys().next().cloned().unwrap_or_default(),
                (None, _) => self.source.name.clone(),
            },
            sounds: chosen,
        };
        let count = selection.file_count();
        if count > MAX_FILES {
            return Err(LibraryError::Select(format!(
                "that is {count} files, more than the {MAX_FILES} one load can fetch; choose a \
                 bank or name the sounds you want"
            )));
        }
        Ok(selection)
    }
}

/// One decoded file of a loaded sound.
#[derive(Clone, Debug)]
pub struct LoadedSample {
    /// Where it was loaded from.
    pub location: String,
    pub root: Option<u8>,
    pub audio: Arc<AudioData>,
}

/// Decoded sounds, ready for a project's audio pool.
#[derive(Clone, Debug)]
pub struct LoadedBank {
    pub name: String,
    pub source: PackSource,
    /// Each sound's variations in the map's order.
    pub sounds: BTreeMap<String, Vec<LoadedSample>>,
    /// Files that could not be loaded, with the reason.
    pub failed: Vec<String>,
}

/// Downloads packs and keeps what it fetched. Clones share the connection
/// pool.
#[derive(Clone)]
pub struct Library {
    cache: PathBuf,
    agent: ureq::Agent,
}

impl Default for Library {
    fn default() -> Self {
        Library::new(
            Library::default_cache_dir()
                .unwrap_or_else(|| std::env::temp_dir().join("skyware-sound-cache")),
        )
    }
}

fn is_url(s: &str) -> bool {
    let head = s.get(..8).unwrap_or(s).to_ascii_lowercase();
    head.starts_with("http://") || head.starts_with("https://")
}

/// FNV-1a, so cache names stay the same across builds.
fn fnv64(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A cache file name for `url`: a hash, then a readable, safe tail of the
/// URL's last segment, which keeps its extension for the decoder.
fn cache_name(url: &str) -> String {
    let last = url.rsplit('/').next().unwrap_or_default();
    let last = last.split(['?', '#']).next().unwrap_or_default();
    let safe: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let tail = &safe[safe.len().saturating_sub(48)..];
    format!("{:016x}-{tail}", fnv64(url))
}

/// Root certificates from `SSL_CERT_FILE`, for networks that inspect TLS
/// with their own authority.
fn env_roots() -> Option<Vec<ureq::tls::Certificate<'static>>> {
    let pem = std::fs::read(std::env::var_os("SSL_CERT_FILE")?).ok()?;
    let certs: Vec<_> = ureq::tls::parse_pem(&pem)
        .filter_map(|item| match item {
            Ok(ureq::tls::PemItem::Certificate(c)) => Some(c),
            _ => None,
        })
        .collect();
    (!certs.is_empty()).then_some(certs)
}

/// The names a pack's cover image may have beside its sample map:
/// `cover.png` beside a `strudel.json`, or the map's own name with an image
/// extension, such as `vcsl.png` beside `vcsl.json`.
pub fn cover_names(map_file: &str) -> Vec<String> {
    let stem = map_file.rsplit_once('.').map_or(map_file, |(s, _)| s);
    let stem = if stem.eq_ignore_ascii_case("strudel") {
        "cover"
    } else {
        stem
    };
    ["png", "jpg", "jpeg"]
        .iter()
        .map(|e| format!("{stem}.{e}"))
        .collect()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("part{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

impl Library {
    /// A library that keeps downloads in `cache`.
    pub fn new(cache: PathBuf) -> Self {
        let mut tls = ureq::tls::TlsConfig::builder();
        if let Some(roots) = env_roots() {
            tls = tls.root_certs(ureq::tls::RootCerts::Specific(Arc::new(roots)));
        }
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_global(Some(Duration::from_secs(300)))
            .user_agent(concat!("Skyware Sound/", env!("CARGO_PKG_VERSION")))
            .tls_config(tls.build())
            .build()
            .into();
        Self { cache, agent }
    }

    /// The per-user cache folder, such as
    /// `%LOCALAPPDATA%\Skyware Sound\cache\sounds` on Windows.
    pub fn default_cache_dir() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "Skyware Sound")
            .map(|d| d.cache_dir().join("sounds"))
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache
    }

    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, String> {
        let mut response = self.agent.get(url).call().map_err(|e| e.to_string())?;
        response
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map_err(|e| e.to_string())
    }

    /// Reads a pack's sample map. Maps from the network are saved, and the
    /// saved copy is used when the network is down.
    pub fn open(&self, source: &PackSource) -> Result<Pack, LibraryError> {
        let mut location = expand_github(source.map.trim());
        let err = |location: &str, message: String| LibraryError::Map {
            location: location.to_string(),
            message,
        };
        let mut offline = false;
        let text = if is_url(&location) {
            let cached = self
                .cache
                .join("maps")
                .join(format!("{:016x}.json", fnv64(&location)));
            match self.get(&location, MAX_MAP_BYTES) {
                Ok(bytes) => {
                    if let Err(e) = write_atomic(&cached, &bytes) {
                        tracing::warn!("cannot cache {location}: {e}");
                    }
                    String::from_utf8_lossy(&bytes).into_owned()
                }
                Err(e) => match std::fs::read_to_string(&cached) {
                    Ok(text) => {
                        tracing::warn!("{location}: {e}; using the saved copy");
                        offline = true;
                        text
                    }
                    Err(_) => return Err(err(&location, e)),
                },
            }
        } else {
            let path = Path::new(location.strip_prefix("file://").unwrap_or(&location));
            let path = if path.is_dir() {
                path.join("strudel.json")
            } else {
                path.to_path_buf()
            };
            location = path.to_string_lossy().into_owned();
            std::fs::read_to_string(&path).map_err(|e| err(&location, e.to_string()))?
        };
        let map = SampleMap::parse(&text, &location).map_err(|m| err(&location, m))?;
        Ok(Pack {
            source: source.clone(),
            location,
            map,
            offline,
        })
    }

    /// The cover image a pack brings beside its sample map, if any (see
    /// [`cover_names`]): the file's bytes and its name. Covers from the
    /// network are kept in the cache, and so is finding none, for a week.
    pub fn cover(&self, pack: &Pack) -> Result<Option<(Vec<u8>, String)>, String> {
        let location = pack.location.as_str();
        if !is_url(location) {
            let path = Path::new(location);
            let (Some(dir), Some(file)) = (path.parent(), path.file_name()) else {
                return Ok(None);
            };
            for name in cover_names(&file.to_string_lossy()) {
                let p = dir.join(&name);
                let Ok(meta) = std::fs::metadata(&p) else {
                    continue;
                };
                if !meta.is_file() {
                    continue;
                }
                if meta.len() > MAX_COVER_BYTES {
                    return Err(format!(
                        "{} is larger than {} MB",
                        p.display(),
                        MAX_COVER_BYTES >> 20
                    ));
                }
                return std::fs::read(&p)
                    .map(|b| Some((b, name)))
                    .map_err(|e| format!("cannot read {}: {e}", p.display()));
            }
            return Ok(None);
        }
        let base = location.split(['?', '#']).next().unwrap_or(location);
        let Some((dir, file)) = base.rsplit_once('/') else {
            return Ok(None);
        };
        let covers = self.cache.join("covers");
        let names = cover_names(file);
        let urls: Vec<String> = names.iter().map(|n| format!("{dir}/{n}")).collect();
        for (name, url) in names.iter().zip(&urls) {
            if let Ok(bytes) = std::fs::read(covers.join(cache_name(url))) {
                return Ok(Some((bytes, name.clone())));
            }
        }
        let none = covers.join(format!("{:016x}.none", fnv64(location)));
        let fresh = std::fs::metadata(&none)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|age| age < COVER_RECHECK));
        if fresh {
            return Ok(None);
        }
        for (name, url) in names.iter().zip(&urls) {
            match self.agent.get(url).call() {
                Ok(mut response) => {
                    let bytes = response
                        .body_mut()
                        .with_config()
                        .limit(MAX_COVER_BYTES)
                        .read_to_vec()
                        .map_err(|e| format!("{url}: {e}"))?;
                    if let Err(e) = write_atomic(&covers.join(cache_name(url)), &bytes) {
                        tracing::warn!("cannot cache {url}: {e}");
                    }
                    return Ok(Some((bytes, name.clone())));
                }
                Err(ureq::Error::StatusCode(400..=499)) => continue,
                Err(e) => return Err(format!("{url}: {e}")),
            }
        }
        if let Err(e) = write_atomic(&none, b"") {
            tracing::warn!("cannot cache that {location} has no cover: {e}");
        }
        Ok(None)
    }

    /// One file, decoded: from the cache, the network or a local folder.
    fn fetch_audio(&self, location: &str) -> Result<AudioData, String> {
        if !is_url(location) {
            return sky_io::read_audio(Path::new(location)).map_err(|e| e.to_string());
        }
        let path = self.cache.join("files").join(cache_name(location));
        if path.exists() {
            match sky_io::read_audio(&path) {
                Ok(audio) => return Ok(audio),
                // A damaged copy is fetched again.
                Err(e) => tracing::warn!("{e}; downloading it again"),
            }
        }
        let bytes = self.get(location, MAX_FILE_BYTES)?;
        write_atomic(&path, &bytes).map_err(|e| format!("cannot save {}: {e}", path.display()))?;
        sky_io::read_audio(&path).map_err(|e| e.to_string())
    }

    /// One file of a pack, decoded, such as to hear a sound before loading
    /// it. It comes from the cache when it was fetched before, and stays
    /// there for a later load.
    pub fn fetch(&self, pack: &Pack, file: &MapFile) -> Result<AudioData, String> {
        let location = pack.map.locate(file);
        let audio = self
            .fetch_audio(&location)
            .map_err(|e| format!("{location}: {e}"))?;
        if audio.frames() == 0 {
            return Err(format!("{location}: the file has no audio in it"));
        }
        Ok(audio)
    }

    /// Downloads and decodes the chosen sounds, several files at a time.
    /// Files that fail are listed in [`LoadedBank::failed`]; it is an error
    /// only when none load.
    pub fn load(&self, pack: &Pack, selection: &Selection) -> Result<LoadedBank, LibraryError> {
        let jobs: Vec<(&str, &MapFile, String)> = selection
            .sounds
            .iter()
            .flat_map(|(name, files)| files.iter().map(move |f| (name.as_str(), f)))
            .map(|(name, f)| (name, f, pack.map.locate(f)))
            .collect();
        let results: Mutex<Vec<Option<Result<AudioData, String>>>> =
            Mutex::new((0..jobs.len()).map(|_| None).collect());
        let next = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..WORKERS.min(jobs.len()) {
                scope.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some((_, _, location)) = jobs.get(i) else {
                            break;
                        };
                        let result = self.fetch_audio(location).and_then(|a| {
                            if a.frames() == 0 {
                                Err("the file has no audio in it".into())
                            } else {
                                Ok(a)
                            }
                        });
                        if let Ok(mut r) = results.lock() {
                            r[i] = Some(result);
                        }
                    }
                });
            }
        });
        let results = results.into_inner().unwrap_or_default();
        let mut sounds: BTreeMap<String, Vec<LoadedSample>> = BTreeMap::new();
        let mut failed = Vec::new();
        for ((name, file, location), result) in jobs.iter().zip(results) {
            match result {
                Some(Ok(audio)) => sounds
                    .entry(name.to_string())
                    .or_default()
                    .push(LoadedSample {
                        location: location.clone(),
                        root: file.root,
                        audio: Arc::new(audio),
                    }),
                Some(Err(e)) => failed.push(format!("{location}: {e}")),
                None => failed.push(format!("{location}: not loaded")),
            }
        }
        if sounds.is_empty() {
            return Err(LibraryError::NothingLoaded {
                count: jobs.len(),
                first: failed.first().cloned().unwrap_or_default(),
            });
        }
        Ok(LoadedBank {
            name: selection.name.clone(),
            source: pack.source.clone(),
            sounds,
            failed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_names_are_stable_and_safe() {
        let a = cache_name("https://x.org/a/Bass%20Drum%201/BD_hit v2.wav");
        assert_eq!(
            a,
            cache_name("https://x.org/a/Bass%20Drum%201/BD_hit v2.wav")
        );
        assert!(a.ends_with("BD_hit_v2.wav"), "{a}");
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        );
        assert_ne!(
            a,
            cache_name("https://x.org/b/Bass%20Drum%201/BD_hit v2.wav")
        );
        assert!(cache_name(&format!("https://x/{}.wav", "a".repeat(300))).len() < 80);
    }

    #[test]
    fn covers_sit_beside_the_map() {
        assert_eq!(
            cover_names("strudel.json"),
            ["cover.png", "cover.jpg", "cover.jpeg"]
        );
        assert_eq!(cover_names("vcsl.json")[0], "vcsl.png");

        let dir = tempfile::tempdir().expect("temp dir");
        let map = dir.path().join("strudel.json");
        std::fs::write(&map, r#"{"_base": "./", "bd": ["bd.wav"]}"#).expect("map");
        let library = Library::new(dir.path().join("cache"));
        let pack = library
            .open(&PackSource::parse(&map.to_string_lossy(), None))
            .expect("pack");
        assert_eq!(library.cover(&pack).expect("looked"), None);
        std::fs::write(dir.path().join("cover.jpg"), b"jpeg bytes").expect("cover");
        let (bytes, name) = library.cover(&pack).expect("looked").expect("a cover");
        assert_eq!(
            (bytes.as_slice(), name.as_str()),
            (&b"jpeg bytes"[..], "cover.jpg")
        );
    }

    #[test]
    fn sources_name_known_packs_and_addresses() {
        let p = PackSource::parse("Tidal-Drum-Machines", None);
        assert_eq!(p.name, "Tidal drum machines");
        assert_eq!(p.license, License::Unknown);
        let p = PackSource::parse("github:yaxu/spicule/master", Some(License::Cc0));
        assert_eq!((p.name.as_str(), p.license), ("spicule", License::Cc0));
        let p = PackSource::parse("C:\\packs\\my kit\\strudel.json", None);
        assert_eq!(p.name, "my kit");
    }
}
