// SPDX-License-Identifier: GPL-3.0-or-later
//! Instrument libraries: multi-sampled instruments such as pianos, read
//! from SFZ files.
//!
//! Libraries are too big to copy into every project, so they live in one
//! shared folder (see [`Instruments::default_dir`]) and projects name them
//! with a [`LibraryRef`]. Each comes from a known source with its license,
//! checked by SHA-256 when downloaded. The small Upright Piano KW is built
//! into the program, so a piano works offline from the first start.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};
use sky_dsp::InstrumentType;
use sky_dsp::instruments::{SampleData, SampleLibrary};
use sky_model::{LibraryRef, License};

use crate::LibraryError;
use crate::sfz;

/// Files built into the program, by path under `instruments/`.
static BUNDLED: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/bundled.rs"));

/// The library a piano plays when its project names none.
pub const DEFAULT_PIANO: &str = "upright-piano-kw-small";
/// Written into an installed library's folder, holding its archive's hash.
const MARKER: &str = ".skyware-installed";
/// Decoding threads.
const WORKERS: usize = 8;
/// Limits on what an archive may unpack to.
const MAX_ENTRIES: usize = 20_000;
const MAX_UNPACKED: u64 = 8 << 30;

/// Where a library comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstrumentSource {
    /// Built into the program: the folder under `instruments/` and the SFZ
    /// file in it.
    Bundled {
        folder: &'static str,
        sfz: &'static str,
    },
    /// A `.7z` archive to download, and its SFZ file's path inside it.
    Download {
        url: &'static str,
        bytes: u64,
        sha256: &'static str,
        sfz: &'static str,
    },
}

/// A library Skyware Sound knows how to get.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstrumentInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: InstrumentType,
    pub version: &'static str,
    pub description: &'static str,
    pub license: License,
    pub credit: Option<&'static str>,
    pub homepage: &'static str,
    /// Bytes on disk once unpacked.
    pub size: u64,
    pub source: InstrumentSource,
}

/// Every known library. Only CC0, public domain or credit-only libraries
/// are listed, so anything made with them can ship in a game.
pub const INSTRUMENTS: &[InstrumentInfo] = &[
    InstrumentInfo {
        id: DEFAULT_PIANO,
        name: sky_model::BUILT_IN_PIANO,
        kind: InstrumentType::Piano,
        version: "2019-07-03",
        description: "A Kawai upright recorded by FreePats: one velocity layer, mono, \
                      44.1 kHz 16-bit, loops in the bass. Built in.",
        license: License::Cc0,
        credit: None,
        homepage: "https://freepats.zenvoid.org/Piano/acoustic-grand-piano.html#UprightKW",
        size: 2_356_000,
        source: InstrumentSource::Bundled {
            folder: "upright-piano-kw-small",
            sfz: "UprightPianoKW-small-20190703.sfz",
        },
    },
    InstrumentInfo {
        id: "upright-piano-kw",
        name: "Upright Piano KW",
        kind: InstrumentType::Piano,
        version: "2022-02-21",
        description: "The full Kawai upright from FreePats: two velocity layers, stereo, \
                      44.1 kHz 24-bit, 68 samples. A 32 MiB download.",
        license: License::Cc0,
        credit: None,
        homepage: "https://freepats.zenvoid.org/Piano/acoustic-grand-piano.html#UprightKW",
        size: 33_859_795,
        source: InstrumentSource::Download {
            url: "https://freepats.zenvoid.org/Piano/UprightPianoKW/UprightPianoKW-SFZ+FLAC-20220221.7z",
            bytes: 33_294_287,
            sha256: "3a66da3b25a2fd9499d90bcf09f1aee4431825796e249361b48af59b1201e703",
            sfz: "UprightPianoKW-SFZ+FLAC-20220221/UprightPianoKW-20220221.sfz",
        },
    },
];

/// A known library by id or name, ignoring case.
pub fn find_instrument(name: &str) -> Option<&'static InstrumentInfo> {
    let name = name.trim();
    INSTRUMENTS
        .iter()
        .find(|i| i.id.eq_ignore_ascii_case(name) || i.name.eq_ignore_ascii_case(name))
}

impl InstrumentInfo {
    /// What a project stores to name this library.
    pub fn reference(&self) -> LibraryRef {
        LibraryRef {
            id: self.id.to_string(),
            name: self.name.to_string(),
            version: self.version.to_string(),
            sha256: match self.source {
                InstrumentSource::Download { sha256, .. } => Some(sha256.to_string()),
                InstrumentSource::Bundled { .. } => None,
            },
            license: self.license,
            credit: self.credit.map(str::to_string),
        }
    }

    pub fn is_bundled(&self) -> bool {
        matches!(self.source, InstrumentSource::Bundled { .. })
    }
}

/// A library and whether it can be loaded now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstrumentStatus {
    pub info: &'static InstrumentInfo,
    pub installed: bool,
}

/// The folder of downloaded libraries, and how to get and load them.
#[derive(Clone)]
pub struct Instruments {
    dir: PathBuf,
    agent: ureq::Agent,
}

impl Default for Instruments {
    fn default() -> Self {
        Instruments::new(
            Instruments::default_dir()
                .unwrap_or_else(|| std::env::temp_dir().join("skyware-sound-instruments")),
        )
    }
}

/// The text of a file, with the error saying which.
fn read_text(path: &Path) -> Result<String, String> {
    std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// `path`'s folder, with `/` between folders.
fn folder_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// An archive entry's name as a safe relative path, or `None` when it would
/// land outside the folder it is unpacked into.
fn entry_path(name: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return None,
            p if p.contains(':') => return None,
            p => out.push(p),
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn err(message: impl Into<String>) -> LibraryError {
    LibraryError::Instrument(message.into())
}

impl Instruments {
    pub fn new(dir: PathBuf) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_global(Some(Duration::from_secs(30 * 60)))
            .user_agent(concat!("Skyware Sound/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self { dir, agent }
    }

    /// The per-user folder, such as
    /// `%LOCALAPPDATA%\Skyware Sound\data\instruments` on Windows.
    pub fn default_dir() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "Skyware Sound")
            .map(|d| d.data_local_dir().join("instruments"))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where a downloaded library is unpacked.
    pub fn folder(&self, info: &InstrumentInfo) -> PathBuf {
        self.dir.join(format!("{}-{}", info.id, info.version))
    }

    /// True when the library can be loaded without a download.
    pub fn is_installed(&self, info: &InstrumentInfo) -> bool {
        match info.source {
            InstrumentSource::Bundled { .. } => true,
            InstrumentSource::Download { sha256, .. } => {
                std::fs::read_to_string(self.folder(info).join(MARKER))
                    .is_ok_and(|m| m.trim() == sha256)
            }
        }
    }

    /// Every known library and whether it is installed.
    pub fn list(&self) -> Vec<InstrumentStatus> {
        INSTRUMENTS
            .iter()
            .map(|info| InstrumentStatus {
                info,
                installed: self.is_installed(info),
            })
            .collect()
    }

    /// The best installed library of a kind: the first downloaded one, else
    /// the built-in one.
    pub fn best(&self, kind: InstrumentType) -> Option<&'static InstrumentInfo> {
        let mut of_kind = INSTRUMENTS.iter().filter(|i| i.kind == kind);
        of_kind
            .clone()
            .find(|i| !i.is_bundled() && self.is_installed(i))
            .or_else(|| of_kind.find(|i| i.is_bundled()))
    }

    /// Downloads, checks and unpacks a library, reporting bytes received
    /// and the total. Does nothing when it is installed already.
    pub fn install(
        &self,
        info: &InstrumentInfo,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<(), LibraryError> {
        let InstrumentSource::Download {
            url, bytes, sha256, ..
        } = info.source
        else {
            return Ok(());
        };
        if self.is_installed(info) {
            return Ok(());
        }
        let downloads = self.dir.join("downloads");
        std::fs::create_dir_all(&downloads)
            .map_err(|e| err(format!("cannot create {}: {e}", downloads.display())))?;
        let archive = downloads.join(format!("{}-{}.7z", info.id, info.version));
        let have = std::fs::metadata(&archive).is_ok_and(|m| m.len() == bytes)
            && Self::hash_file(&archive).is_ok_and(|h| h == sha256);
        if !have {
            self.download(url, bytes, sha256, &archive, progress)?;
        }
        let folder = self.folder(info);
        let tmp = self.dir.join(format!(
            "{}-{}.part{}",
            info.id,
            info.version,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let unpacked = Self::unpack(&archive, &tmp);
        if let Err(e) = unpacked {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::rename(&tmp, &folder)
            .map_err(|e| err(format!("cannot move {} into place: {e}", folder.display())))?;
        std::fs::write(folder.join(MARKER), sha256)
            .map_err(|e| err(format!("cannot finish {}: {e}", folder.display())))?;
        // The unpacked library is all that is needed from now on.
        let _ = std::fs::remove_file(&archive);
        Ok(())
    }

    fn hash_file(path: &Path) -> std::io::Result<String> {
        let mut file = File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(hex(&hasher.finalize()))
    }

    fn download(
        &self,
        url: &str,
        bytes: u64,
        sha256: &str,
        to: &Path,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<(), LibraryError> {
        let failed = |e: String| err(format!("cannot download {url}: {e}"));
        let mut response = self
            .agent
            .get(url)
            .call()
            .map_err(|e| failed(e.to_string()))?;
        let mut body = response.body_mut().with_config().limit(bytes + 1).reader();
        let part = to.with_extension("7z.part");
        let mut file = File::create(&part)
            .map_err(|e| err(format!("cannot write {}: {e}", part.display())))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut done = 0u64;
        progress(0, bytes);
        loop {
            let n = body.read(&mut buf).map_err(|e| failed(e.to_string()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n])
                .map_err(|e| err(format!("cannot write {}: {e}", part.display())))?;
            done += n as u64;
            progress(done, bytes);
        }
        drop(file);
        let got = hex(&hasher.finalize());
        if done != bytes || got != sha256 {
            let _ = std::fs::remove_file(&part);
            return Err(failed(format!(
                "the file is not the one expected ({done} bytes, SHA-256 {got}; expected \
                 {bytes} bytes, {sha256})"
            )));
        }
        std::fs::rename(&part, to).map_err(|e| err(format!("cannot save {}: {e}", to.display())))
    }

    /// Unpacks a `.7z` archive into `into`, refusing entries that would land
    /// outside it.
    fn unpack(archive: &Path, into: &Path) -> Result<(), LibraryError> {
        let open = |e: String| err(format!("cannot unpack {}: {e}", archive.display()));
        let file = File::open(archive).map_err(|e| open(e.to_string()))?;
        let mut reader = sevenz_rust2::ArchiveReader::new(file, sevenz_rust2::Password::empty())
            .map_err(|e| open(e.to_string()))?;
        let mut problem: Option<String> = None;
        let (mut entries, mut total) = (0usize, 0u64);
        let result = reader.for_each_entries(|entry, data| {
            entries += 1;
            total += entry.size();
            if entries > MAX_ENTRIES || total > MAX_UNPACKED {
                problem = Some("the archive is larger than any instrument library".into());
                return Ok(false);
            }
            let Some(rel) = entry_path(entry.name()) else {
                // The folder the archive was made from has no name.
                if entry.is_directory()
                    && entry
                        .name()
                        .split(['/', '\\'])
                        .all(|p| p.is_empty() || p == ".")
                {
                    return Ok(true);
                }
                problem = Some(format!(
                    "the archive names a file outside it: {}",
                    entry.name()
                ));
                return Ok(false);
            };
            let path = into.join(rel);
            if entry.is_directory() {
                std::fs::create_dir_all(&path)?;
                return Ok(true);
            }
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut out = File::create(&path)?;
            std::io::copy(data, &mut out)?;
            Ok(true)
        });
        if let Some(p) = problem {
            return Err(open(p));
        }
        result.map_err(|e| open(e.to_string()))
    }

    /// Reads a library's SFZ file and decodes its samples, several at a
    /// time. The library must be installed.
    pub fn load(&self, info: &InstrumentInfo) -> Result<SampleLibrary, LibraryError> {
        let (sfz_path, read): (String, Box<FindFile>) = match info.source {
            InstrumentSource::Bundled { folder, sfz } => (
                format!("{folder}/{sfz}"),
                Box::new(|path: &str| {
                    BUNDLED
                        .iter()
                        .find(|(p, _)| *p == path)
                        .map(|(_, bytes)| Source::Memory(bytes))
                        .ok_or_else(|| format!("{path} is not built in"))
                }),
            ),
            InstrumentSource::Download { sfz, .. } => {
                if !self.is_installed(info) {
                    return Err(err(format!(
                        "{} is not downloaded yet; install it first",
                        info.name
                    )));
                }
                let folder = self.folder(info);
                (
                    sfz.to_string(),
                    Box::new(move |path: &str| Ok(Source::File(folder.join(path)))),
                )
            }
        };
        let text = |path: &str| -> Result<String, String> {
            match read(path)? {
                Source::Memory(bytes) => Ok(String::from_utf8_lossy(bytes).into_owned()),
                Source::File(p) => read_text(&p),
            }
        };
        let main = text(&sfz_path).map_err(err)?;
        let parsed = sfz::parse(&main, folder_of(&sfz_path), &mut |p| text(p))
            .map_err(|e| err(format!("{}: {e}", info.name)))?;
        if !parsed.skipped.is_empty() {
            tracing::info!(
                "{}: skipped SFZ opcodes {}",
                info.name,
                parsed.skipped.join(", ")
            );
        }
        let decoded: Mutex<Vec<Option<Result<SampleData, String>>>> =
            Mutex::new((0..parsed.samples.len()).map(|_| None).collect());
        let next = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..WORKERS.min(parsed.samples.len()) {
                scope.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(path) = parsed.samples.get(i) else {
                            break;
                        };
                        let audio = read(path).and_then(|source| match source {
                            Source::Memory(bytes) => {
                                sky_io::decode_audio(bytes, path).map_err(|e| e.to_string())
                            }
                            Source::File(p) => sky_io::read_audio(&p).map_err(|e| e.to_string()),
                        });
                        let sample = audio.map(|a| SampleData::from_audio(&a));
                        if let Ok(mut d) = decoded.lock() {
                            d[i] = Some(sample);
                        }
                    }
                });
            }
        });
        let mut samples = Vec::with_capacity(parsed.samples.len());
        for (path, result) in parsed
            .samples
            .iter()
            .zip(decoded.into_inner().unwrap_or_default())
        {
            match result {
                Some(Ok(s)) => samples.push(s),
                Some(Err(e)) => {
                    return Err(err(format!(
                        "{}: {e}; the library may be damaged, so install it again",
                        info.name
                    )));
                }
                None => return Err(err(format!("{}: {path} was not read", info.name))),
            }
        }
        Ok(SampleLibrary::new(info.name, samples, parsed.zones))
    }

    /// Loads the library `reference` names (the default piano for none),
    /// shared for every track that plays it.
    pub fn load_ref(
        &self,
        reference: Option<&LibraryRef>,
    ) -> Result<Arc<SampleLibrary>, LibraryError> {
        let id = reference.map_or(DEFAULT_PIANO, |r| r.id.as_str());
        let info = find_instrument(id).ok_or_else(|| {
            err(format!(
                "the instrument library {id:?} is not one Skyware Sound knows"
            ))
        })?;
        if let Some(r) = reference
            && r.version != info.version
        {
            return Err(err(format!(
                "the project uses {} version {}, but this version of Skyware Sound has {}",
                info.name, r.version, info.version
            )));
        }
        self.load(info).map(Arc::new)
    }
}

/// Where a library file's bytes are.
enum Source {
    Memory(&'static [u8]),
    File(PathBuf),
}

/// Finds a library file by its path inside the library.
type FindFile = dyn Fn(&str) -> Result<Source, String> + Sync;

#[cfg(test)]
mod tests {
    use sky_dsp::instruments::sampled::{LoopMode, Trigger};

    use super::*;

    #[test]
    fn the_built_in_piano_loads() {
        let info = find_instrument(DEFAULT_PIANO).expect("known");
        assert_eq!(
            info.license,
            License::Cc0,
            "projects without a library rely on this"
        );
        let instruments = Instruments::new(std::env::temp_dir().join("never-used"));
        assert!(instruments.is_installed(info));
        let lib = instruments.load(info).expect("loads");
        assert_eq!(lib.samples().len(), 26);
        assert_eq!(lib.zones().len(), 26);
        assert_eq!(lib.key_range(), Some((21, 108)));
        assert!(!lib.is_stereo());
        // Every key of the piano plays exactly one zone, looped in the bass.
        for key in 21..=108 {
            assert_eq!(
                lib.zones_at(Trigger::Attack, key, 100).len(),
                1,
                "key {key}"
            );
        }
        let c1 = &lib.zones()[0];
        assert_eq!((c1.root, c1.loop_mode), (24, LoopMode::Continuous));
        assert_eq!((c1.loop_start, c1.loop_end), (306_378, 424_657));
        assert_eq!(c1.release, 0.6);
        assert_eq!(lib.samples()[0].sample_rate, 44_100);
        assert!(
            lib.bytes() > 8_000_000 && lib.bytes() < 12_000_000,
            "{}",
            lib.bytes()
        );
    }

    #[test]
    fn references_name_the_library() {
        let full = find_instrument("Upright Piano KW").expect("by name");
        let r = full.reference();
        assert_eq!(
            (r.id.as_str(), r.version.as_str()),
            ("upright-piano-kw", "2022-02-21")
        );
        assert_eq!(r.sha256.as_deref().map(str::len), Some(64));
        assert_eq!(r.key(), "upright-piano-kw@2022-02-21");
        let instruments = Instruments::new(std::env::temp_dir().join("no-instruments-here"));
        assert!(!instruments.is_installed(full));
        assert!(instruments.load(full).is_err());
        assert_eq!(
            instruments.best(InstrumentType::Piano).map(|i| i.id),
            Some(DEFAULT_PIANO)
        );
    }

    #[test]
    fn archive_entries_stay_inside() {
        assert_eq!(
            entry_path("a/b.flac"),
            Some(PathBuf::from("a").join("b.flac"))
        );
        assert_eq!(
            entry_path("a\\b.flac"),
            Some(PathBuf::from("a").join("b.flac"))
        );
        assert_eq!(entry_path("../b.flac"), None);
        assert_eq!(entry_path("C:/b.flac"), None);
        assert_eq!(entry_path("/"), None);
    }

    #[test]
    fn every_library_is_safe_to_ship() {
        for info in INSTRUMENTS {
            assert!(
                matches!(info.license, License::Cc0 | License::PublicDomain)
                    || (info.license.needs_credit()
                        && !info.license.share_alike()
                        && info.credit.is_some()),
                "{}",
                info.id
            );
        }
    }
}
