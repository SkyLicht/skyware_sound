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
use sky_dsp::instruments::sampled::Trigger;
use sky_dsp::instruments::{SampleData, SampleLibrary};
use sky_model::{LibraryRef, License};

use crate::LibraryError;
use crate::{sfz, steinway_b, strudel_piano};

/// Files built into the program, by path under `instruments/`.
static BUNDLED: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/bundled.rs"));

/// The library a piano plays when its project names none.
pub const DEFAULT_PIANO: &str = "upright-piano-kw-small";
/// Strudel's piano pack as a library.
pub const STRUDEL_PIANO: &str = "strudel-piano";
/// Written into an installed library's folder, holding its archive's hash.
const MARKER: &str = ".skyware-installed";
/// Decoding threads.
const WORKERS: usize = 8;
/// Limits on what an archive may unpack to.
const MAX_ENTRIES: usize = 20_000;
const MAX_UNPACKED: u64 = 8 << 30;

/// What getting a library ready is busy with, for progress reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Bytes received of the archive's size.
    Download,
    /// Bytes unpacked of the library's size on disk.
    Unpack,
    /// Recordings decoded of the ones to load.
    Load,
}

/// Where a library comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstrumentSource {
    /// Built into the program: the folder under `instruments/` and the SFZ
    /// file in it.
    Bundled {
        folder: &'static str,
        sfz: &'static str,
    },
    /// A `.7z` or `.tar.gz` archive to download, and its SFZ file's path
    /// inside it.
    Download {
        url: &'static str,
        bytes: u64,
        sha256: &'static str,
        sfz: &'static str,
    },
    /// Separate files to download, each by its path percent-encoded onto
    /// `base` and checked by its size and SHA-256, played by an SFZ file
    /// built into the program. `sha256` is the hash of the list as
    /// `path\tbytes\tsha256\n` lines; `sfz` is the SFZ file's path under
    /// `instruments/`, and the built-in files in its folder are installed
    /// with the downloads.
    Files {
        base: &'static str,
        files: &'static [(&'static str, u64, &'static str)],
        sha256: &'static str,
        sfz: &'static str,
    },
}

impl InstrumentSource {
    /// The hash that names what was downloaded, for sources that download.
    fn sha256(&self) -> Option<&'static str> {
        match *self {
            InstrumentSource::Bundled { .. } => None,
            InstrumentSource::Download { sha256, .. } | InstrumentSource::Files { sha256, .. } => {
                Some(sha256)
            }
        }
    }
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
    /// The most velocity layers any key has.
    pub layers: u8,
    /// Bytes the recordings take in memory with every layer loaded.
    pub memory: u64,
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
        layers: 1,
        memory: 9_300_000,
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
                      44.1 kHz 24-bit, 66 samples. A 32 MiB download.",
        license: License::Cc0,
        credit: None,
        homepage: "https://freepats.zenvoid.org/Piano/acoustic-grand-piano.html#UprightKW",
        size: 33_859_795,
        layers: 2,
        memory: 57_400_000,
        source: InstrumentSource::Download {
            url: "https://freepats.zenvoid.org/Piano/UprightPianoKW/UprightPianoKW-SFZ+FLAC-20220221.7z",
            bytes: 33_294_287,
            sha256: "3a66da3b25a2fd9499d90bcf09f1aee4431825796e249361b48af59b1201e703",
            sfz: "UprightPianoKW-SFZ+FLAC-20220221/UprightPianoKW-20220221.sfz",
        },
    },
    InstrumentInfo {
        id: "vcsl-steinway-b",
        name: "Steinway B (VCSL)",
        kind: InstrumentType::Piano,
        version: "1.2.2",
        description: "A Steinway Model B grand from the Versilian Community Sample Library: \
                      three velocity layers, stereo, 44.1 kHz 24-bit, every whole tone, with \
                      release samples. A 543 MiB download of 225 files.",
        license: License::Cc0,
        credit: None,
        homepage: "https://github.com/sgossner/VCSL",
        size: 569_671_282,
        layers: 3,
        memory: 380_000_000,
        source: InstrumentSource::Files {
            base: steinway_b::BASE,
            files: steinway_b::FILES,
            sha256: steinway_b::MANIFEST_SHA256,
            sfz: "vcsl-steinway-b/SteinwayB.sfz",
        },
    },
    InstrumentInfo {
        id: "salamander-grand-piano",
        name: "Salamander Grand Piano",
        kind: InstrumentType::Piano,
        version: "2020-06-02",
        description: "A Yamaha C5 grand recorded by Alexander Holm: 16 velocity layers, \
                      stereo, 48 kHz 24-bit, every minor third, with string resonance, \
                      hammer and pedal noises. A 707 MiB download; load fewer layers to \
                      save memory.",
        license: License::CcBy3,
        credit: Some(
            "Salamander Grand Piano V3 by Alexander Holm (CC BY 3.0), FLAC edition by FreePats",
        ),
        homepage: "https://freepats.zenvoid.org/Piano/acoustic-grand-piano.html",
        size: 746_912_846,
        layers: 16,
        memory: 1_307_000_000,
        source: InstrumentSource::Download {
            url: "https://freepats.zenvoid.org/Piano/SalamanderGrandPiano/\
                  SalamanderGrandPiano-SFZ+FLAC-V3+20200602.tar.gz",
            bytes: 741_757_374,
            sha256: "b7760e168494cf095344e217b0af013fc449ad033abbbdf1c65211cf11dc038b",
            sfz: "SalamanderGrandPiano-SFZ+FLAC-V3+20200602/SalamanderGrandPiano-V3+20200602.sfz",
        },
    },
    InstrumentInfo {
        id: STRUDEL_PIANO,
        name: "Salamander (Strudel)",
        kind: InstrumentType::Piano,
        version: "2026-10-18",
        description: "The piano Strudel plays for s(\"piano\"), from its piano pack: one \
                      velocity layer of the Salamander grand, stereo MP3s, a recording every \
                      three semitones. A 5 MiB download of 29 files.",
        license: License::CcBy3,
        credit: Some("Salamander Grand Piano by Alexander Holm (CC BY 3.0)"),
        homepage: "https://archive.org/details/SalamanderGrandPianoV3",
        size: 5_481_960,
        layers: 1,
        memory: 72_200_000,
        source: InstrumentSource::Files {
            base: strudel_piano::BASE,
            files: strudel_piano::FILES,
            sha256: strudel_piano::MANIFEST_SHA256,
            sfz: "strudel-piano/piano.sfz",
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
        self.reference_with(None)
    }

    /// What a project stores to name this library loaded with at most
    /// `layers` velocity layers; asking for all of them or more is the same
    /// as `None`.
    pub fn reference_with(&self, layers: Option<u8>) -> LibraryRef {
        LibraryRef {
            id: self.id.to_string(),
            name: self.name.to_string(),
            version: self.version.to_string(),
            sha256: self.source.sha256().map(str::to_string),
            license: self.license,
            credit: self.credit.map(str::to_string),
            layers: layers.filter(|n| *n < self.layers),
        }
    }

    /// The layer counts worth offering: all of them, then halving while two
    /// or more are left.
    pub fn layer_choices(&self) -> Vec<u8> {
        let mut out = vec![self.layers];
        let mut n = self.layers;
        while n >= 4 {
            n = n.div_ceil(2);
            out.push(n);
        }
        out
    }

    /// About how much memory the recordings take with `layers` loaded.
    pub fn memory_with(&self, layers: Option<u8>) -> u64 {
        match layers {
            Some(n) if n < self.layers && self.layers > 0 => {
                self.memory * u64::from(n) / u64::from(self.layers)
            }
            _ => self.memory,
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

/// `path` for a URL: every byte but letters, digits, `-._~` and `/`
/// percent-encoded.
fn percent_encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
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
        match info.source.sha256() {
            None => true,
            Some(sha256) => std::fs::read_to_string(self.folder(info).join(MARKER))
                .is_ok_and(|m| m.trim() == sha256),
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

    /// Downloads, checks and unpacks a library, reporting each stage's
    /// progress and its total. Does nothing when it is installed already.
    pub fn install(
        &self,
        info: &InstrumentInfo,
        progress: &mut dyn FnMut(Stage, u64, u64),
    ) -> Result<(), LibraryError> {
        if self.is_installed(info) {
            return Ok(());
        }
        match info.source {
            InstrumentSource::Bundled { .. } => Ok(()),
            InstrumentSource::Download {
                url, bytes, sha256, ..
            } => self.install_archive(info, url, bytes, sha256, progress),
            InstrumentSource::Files {
                base,
                files,
                sha256,
                sfz,
            } => self.install_files(info, base, files, sha256, sfz, progress),
        }
    }

    /// Downloads, checks and unpacks a library's archive.
    fn install_archive(
        &self,
        info: &InstrumentInfo,
        url: &str,
        bytes: u64,
        sha256: &str,
        progress: &mut dyn FnMut(Stage, u64, u64),
    ) -> Result<(), LibraryError> {
        let downloads = self.dir.join("downloads");
        std::fs::create_dir_all(&downloads)
            .map_err(|e| err(format!("cannot create {}: {e}", downloads.display())))?;
        let archive = downloads.join(format!(
            "{}-{}.{}",
            info.id,
            info.version,
            Packing::of(url).extension()
        ));
        let have = std::fs::metadata(&archive).is_ok_and(|m| m.len() == bytes)
            && Self::hash_file(&archive).is_ok_and(|h| h == sha256);
        if !have {
            self.download(url, bytes, sha256, &archive, &mut |n| {
                progress(Stage::Download, n, bytes);
            })?;
        }
        let folder = self.folder(info);
        let tmp = self.dir.join(format!(
            "{}-{}.part{}",
            info.id,
            info.version,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut report = |done| progress(Stage::Unpack, done, info.size.max(done));
        let unpacked = match Packing::of(url) {
            Packing::SevenZ => Self::unpack(&archive, &tmp, &mut report),
            Packing::TarGz => Self::unpack_tar_gz(&archive, &tmp, &mut report),
        };
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

    /// Downloads a library's files one by one, then adds the built-in files
    /// that go with them and moves them into place. The folder they are
    /// downloaded into is kept when a download fails, so the next try only
    /// gets the files still missing.
    fn install_files(
        &self,
        info: &InstrumentInfo,
        base: &str,
        files: &[(&str, u64, &str)],
        sha256: &str,
        sfz: &str,
        progress: &mut dyn FnMut(Stage, u64, u64),
    ) -> Result<(), LibraryError> {
        let tmp = self.dir.join(format!("{}-{}.part", info.id, info.version));
        let total: u64 = files.iter().map(|f| f.1).sum();
        let mut done = 0u64;
        progress(Stage::Download, 0, total);
        for &(path, bytes, sha) in files {
            let rel = entry_path(path).ok_or_else(|| {
                err(format!(
                    "{}: the file list names a file outside it: {path}",
                    info.name
                ))
            })?;
            let to = tmp.join(rel);
            let have = std::fs::metadata(&to).is_ok_and(|m| m.len() == bytes)
                && Self::hash_file(&to).is_ok_and(|h| h == sha);
            if !have {
                if let Some(dir) = to.parent() {
                    std::fs::create_dir_all(dir)
                        .map_err(|e| err(format!("cannot create {}: {e}", dir.display())))?;
                }
                let url = format!("{base}{}", percent_encode(path));
                self.download(&url, bytes, sha, &to, &mut |n| {
                    progress(Stage::Download, done + n, total);
                })?;
            }
            done += bytes;
            progress(Stage::Download, done, total);
        }
        // The SFZ file, and anything else built in beside it.
        let prefix = format!("{}/", folder_of(sfz));
        if !BUNDLED.iter().any(|(p, _)| *p == sfz) {
            return Err(err(format!("{}: {sfz} is not built in", info.name)));
        }
        for (path, data) in BUNDLED {
            let Some(rel) = path.strip_prefix(&prefix).and_then(entry_path) else {
                continue;
            };
            let to = tmp.join(rel);
            to.parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&to, data))
                .map_err(|e| err(format!("cannot write {}: {e}", to.display())))?;
        }
        let folder = self.folder(info);
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::rename(&tmp, &folder)
            .map_err(|e| err(format!("cannot move {} into place: {e}", folder.display())))?;
        std::fs::write(folder.join(MARKER), sha256)
            .map_err(|e| err(format!("cannot finish {}: {e}", folder.display())))
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
        progress: &mut dyn FnMut(u64),
    ) -> Result<(), LibraryError> {
        let failed = |e: String| err(format!("cannot download {url}: {e}"));
        let mut response = self
            .agent
            .get(url)
            .call()
            .map_err(|e| failed(e.to_string()))?;
        let mut body = response.body_mut().with_config().limit(bytes + 1).reader();
        let mut part = to.as_os_str().to_owned();
        part.push(".part");
        let part = PathBuf::from(part);
        let mut file = File::create(&part)
            .map_err(|e| err(format!("cannot write {}: {e}", part.display())))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut done = 0u64;
        progress(0);
        loop {
            let n = body.read(&mut buf).map_err(|e| failed(e.to_string()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n])
                .map_err(|e| err(format!("cannot write {}: {e}", part.display())))?;
            done += n as u64;
            progress(done);
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

    /// Unpacks a `.tar.gz` archive into `into`, like [`unpack`](Self::unpack).
    /// Links and other special entries are left out.
    fn unpack_tar_gz(
        archive: &Path,
        into: &Path,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(), LibraryError> {
        let open = |e: String| err(format!("cannot unpack {}: {e}", archive.display()));
        let file = File::open(archive).map_err(|e| open(e.to_string()))?;
        let gz = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
        let mut tar = tar::Archive::new(gz);
        let (mut entries, mut total) = (0usize, 0u64);
        for entry in tar.entries().map_err(|e| open(e.to_string()))? {
            let mut entry = entry.map_err(|e| open(e.to_string()))?;
            entries += 1;
            total += entry.size();
            if entries > MAX_ENTRIES || total > MAX_UNPACKED {
                return Err(open(
                    "the archive is larger than any instrument library".into(),
                ));
            }
            let kind = entry.header().entry_type();
            if !kind.is_file() && !kind.is_dir() {
                continue;
            }
            let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
            let Some(rel) = entry_path(&name) else {
                if kind.is_dir() && name.split(['/', '\\']).all(|p| p.is_empty() || p == ".") {
                    continue;
                }
                return Err(open(format!("the archive names a file outside it: {name}")));
            };
            let path = into.join(rel);
            let written = if kind.is_dir() {
                std::fs::create_dir_all(&path)
            } else {
                path.parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| File::create(&path))
                    .and_then(|mut out| std::io::copy(&mut entry, &mut out).map(|_| ()))
            };
            written.map_err(|e| open(e.to_string()))?;
            progress(total);
        }
        Ok(())
    }

    /// Unpacks a `.7z` archive into `into`, refusing entries that would land
    /// outside it, and reporting the bytes written so far.
    fn unpack(
        archive: &Path,
        into: &Path,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(), LibraryError> {
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
            progress(total);
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
        self.load_with(info, None, &mut |_, _, _| {})
    }

    /// [`load`](Self::load) with at most `layers` velocity layers, reporting
    /// the recordings decoded and the total as it goes.
    pub fn load_with(
        &self,
        info: &InstrumentInfo,
        layers: Option<u8>,
        progress: &mut dyn FnMut(Stage, u64, u64),
    ) -> Result<SampleLibrary, LibraryError> {
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
            InstrumentSource::Download { sfz, .. } | InstrumentSource::Files { sfz, .. } => {
                if !self.is_installed(info) {
                    return Err(err(format!(
                        "{} is not downloaded yet; install it first",
                        info.name
                    )));
                }
                // A built-in SFZ file is installed beside the downloads.
                let sfz = match info.source {
                    InstrumentSource::Files { .. } => sfz.rsplit_once('/').map_or(sfz, |(_, f)| f),
                    _ => sfz,
                };
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
        let mut parsed = sfz::parse(&main, folder_of(&sfz_path), &mut |p| text(p))
            .map_err(|e| err(format!("{}: {e}", info.name)))?;
        if let Some(n) = layers {
            keep_layers(&mut parsed, usize::from(n.max(1)));
        }
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
        let finished = AtomicUsize::new(0);
        let total = parsed.samples.len() as u64;
        progress(Stage::Load, 0, total);
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..WORKERS.min(parsed.samples.len()))
                .map(|_| {
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
                                Source::File(p) => {
                                    sky_io::read_audio(&p).map_err(|e| e.to_string())
                                }
                            });
                            let sample = audio.map(|a| SampleData::from_audio(&a));
                            if let Ok(mut d) = decoded.lock() {
                                d[i] = Some(sample);
                            }
                            finished.fetch_add(1, Ordering::Relaxed);
                        }
                    })
                })
                .collect();
            // Report from this thread while the workers decode.
            loop {
                let done = finished.load(Ordering::Relaxed) as u64;
                progress(Stage::Load, done, total);
                if done >= total || workers.iter().all(|w| w.is_finished()) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(40));
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
        self.load_ref_with(reference, &mut |_, _, _| {})
    }

    /// [`load_ref`](Self::load_ref), reporting the recordings decoded.
    pub fn load_ref_with(
        &self,
        reference: Option<&LibraryRef>,
        progress: &mut dyn FnMut(Stage, u64, u64),
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
        self.load_with(info, reference.and_then(|r| r.layers), progress)
            .map(Arc::new)
    }
}

/// Drops velocity layers until each key range has at most `max`, keeping
/// the loudest and spreading the rest evenly, and widens the kept ones over
/// the gaps. Only the recordings left are decoded.
fn keep_layers(sfz: &mut sfz::SfzInstrument, max: usize) {
    use std::collections::{BTreeMap, BTreeSet};
    /// A range of keys or of velocities, lowest and highest.
    type Span = (u8, u8);
    // Each key range's layers, by their velocity ranges.
    let mut ranges: BTreeMap<Span, BTreeSet<Span>> = BTreeMap::new();
    for z in sfz.zones.iter().filter(|z| z.trigger == Trigger::Attack) {
        ranges
            .entry((z.lokey, z.hikey))
            .or_default()
            .insert((z.lovel, z.hivel));
    }
    // Old velocity range to new, or `None` to drop, per key range.
    let mut plan: BTreeMap<(Span, Span), Option<Span>> = BTreeMap::new();
    for (keys, layers) in &ranges {
        let layers: Vec<Span> = layers.iter().copied().collect();
        let n = layers.len();
        if n <= max {
            continue;
        }
        let kept: BTreeSet<usize> = (1..=max).map(|j| (j * n).div_ceil(max) - 1).collect();
        let mut low = layers.first().map_or(0, |l| l.0);
        for (i, layer) in layers.iter().enumerate() {
            if kept.contains(&i) {
                plan.insert((*keys, *layer), Some((low.min(layer.1), layer.1)));
                low = layer.1.saturating_add(1);
            } else {
                plan.insert((*keys, *layer), None);
            }
        }
    }
    if plan.is_empty() {
        return;
    }
    sfz.zones.retain_mut(|z| {
        if z.trigger != Trigger::Attack {
            return true;
        }
        match plan.get(&((z.lokey, z.hikey), (z.lovel, z.hivel))) {
            Some(Some((lo, hi))) => {
                (z.lovel, z.hivel) = (*lo, *hi);
                true
            }
            Some(None) => false,
            None => true,
        }
    });
    // Keep only the recordings some zone still plays, renumbered.
    let mut used = vec![None; sfz.samples.len()];
    let mut samples = Vec::new();
    for z in &mut sfz.zones {
        let Some(slot) = used.get_mut(z.sample as usize) else {
            continue;
        };
        let index = *slot.get_or_insert_with(|| {
            samples.push(z.sample as usize);
            samples.len() as u32 - 1
        });
        z.sample = index;
    }
    let old = std::mem::take(&mut sfz.samples);
    sfz.samples = samples
        .into_iter()
        .filter_map(|i| old.get(i).cloned())
        .collect();
}

/// How a downloaded library is packed, told by its URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Packing {
    SevenZ,
    TarGz,
}

impl Packing {
    fn of(url: &str) -> Self {
        if url.ends_with(".tar.gz") || url.ends_with(".tgz") {
            Packing::TarGz
        } else {
            Packing::SevenZ
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Packing::SevenZ => "7z",
            Packing::TarGz => "tar.gz",
        }
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
    fn fewer_layers_keep_the_loudest_and_cover_every_velocity() {
        // Four layers on the low keys, two on the high ones, and a release.
        let text = "<group> lokey=0 hikey=59 pitch_keycenter=48\n\
             <region> sample=a1.wav lovel=1 hivel=40\n\
             <region> sample=a2.wav lovel=41 hivel=80\n\
             <region> sample=a3.wav lovel=81 hivel=110\n\
             <region> sample=a4.wav lovel=111 hivel=127\n\
             <group> lokey=60 hikey=127 pitch_keycenter=72\n\
             <region> sample=b1.wav lovel=1 hivel=90\n\
             <region> sample=b2.wav lovel=91 hivel=127\n\
             <region> sample=rel.wav trigger=release\n";
        let mut sfz = sfz::parse(text, "", &mut |_| Err("none".into())).expect("parses");
        keep_layers(&mut sfz, 2);
        let attacks: Vec<(&str, u8, u8)> = sfz
            .zones
            .iter()
            .filter(|z| z.trigger == Trigger::Attack)
            .map(|z| (sfz.samples[z.sample as usize].as_str(), z.lovel, z.hivel))
            .collect();
        assert_eq!(
            attacks,
            [
                ("a2.wav", 1, 80),
                ("a4.wav", 81, 127),
                ("b1.wav", 1, 90),
                ("b2.wav", 91, 127)
            ]
        );
        // Dropped recordings are not loaded; the release stays.
        assert_eq!(
            sfz.samples,
            ["a2.wav", "a4.wav", "b1.wav", "b2.wav", "rel.wav"]
        );
        // One layer keeps only the loudest.
        let mut one = sfz::parse(text, "", &mut |_| Err("none".into())).expect("parses");
        keep_layers(&mut one, 1);
        assert_eq!(one.samples, ["a4.wav", "b2.wav", "rel.wav"]);
        assert!(one.zones.iter().all(|z| z.lovel <= 1 && z.hivel == 127));
    }

    #[test]
    fn layer_choices_halve_down_to_two() {
        let mut info = *find_instrument("upright-piano-kw").expect("known");
        assert_eq!(info.layer_choices(), [2]);
        info.layers = 16;
        assert_eq!(info.layer_choices(), [16, 8, 4, 2]);
        info.layers = 5;
        assert_eq!(info.layer_choices(), [5, 3]);
        assert_eq!(info.reference_with(Some(16)).layers, None);
        assert_eq!(
            info.reference_with(Some(3)).key(),
            "upright-piano-kw@2022-02-21/3"
        );
        assert_eq!(info.memory_with(Some(2)), info.memory * 2 / 5);
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

    /// Checks a library that downloads its files one by one against the
    /// hash of its list, and that its built-in SFZ file plays every file
    /// listed and only those. Returns the library and the parsed SFZ.
    fn listed_files(id: &str, manifest: &str) -> (&'static InstrumentInfo, sfz::SfzInstrument) {
        let info = find_instrument(id).expect("known");
        let InstrumentSource::Files { files, sfz, .. } = info.source else {
            panic!("downloads its files one by one");
        };
        let mut list = String::new();
        for (path, bytes, sha) in files {
            list.push_str(&format!("{path}\t{bytes}\t{sha}\n"));
            assert!(entry_path(path).is_some(), "{path}");
            assert_eq!(sha.len(), 64, "{path}");
        }
        assert_eq!(hex(&Sha256::digest(list.as_bytes())), manifest);
        let listed: u64 = files.iter().map(|f| f.1).sum();
        let built_in: u64 = BUNDLED
            .iter()
            .filter(|(p, _)| p.starts_with(&format!("{}/", folder_of(sfz))))
            .map(|(_, b)| b.len() as u64)
            .sum();
        assert_eq!(info.size, listed + built_in);
        let text = BUNDLED
            .iter()
            .find(|(p, _)| *p == sfz)
            .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
            .expect("built in");
        let parsed = sfz::parse(&text, "", &mut |p| Err(format!("no {p}"))).expect("parses");
        let mut named = parsed.samples.clone();
        named.sort();
        let mut paths: Vec<String> = files.iter().map(|f| f.0.to_string()).collect();
        paths.sort();
        assert_eq!(named, paths);
        assert!(parsed.skipped.is_empty(), "{:?}", parsed.skipped);
        (info, parsed)
    }

    /// A library of silent recordings with the zones of `parsed`.
    fn silent(info: &InstrumentInfo, parsed: sfz::SfzInstrument) -> SampleLibrary {
        let silence = sky_dsp::AudioData {
            sample_rate: 44_100,
            channels: vec![vec![0.0; 16]],
        };
        SampleLibrary::new(
            info.name,
            (0..parsed.samples.len())
                .map(|_| SampleData::from_audio(&silence))
                .collect(),
            parsed.zones,
        )
    }

    #[test]
    fn the_steinway_file_list_is_the_one_its_hash_names() {
        let (info, parsed) = listed_files("vcsl-steinway-b", steinway_b::MANIFEST_SHA256);
        // Notes fade in over 4 ms and the release samples over 100 ms.
        for z in &parsed.zones {
            let fade = if z.trigger == Trigger::Attack {
                0.004
            } else {
                0.1
            };
            assert_eq!(z.attack, fade);
        }
        let attack = parsed
            .zones
            .iter()
            .filter(|z| z.trigger == Trigger::Attack)
            .count();
        assert_eq!((attack, parsed.zones.len() - attack), (126, 99));
        let lib = silent(info, parsed);
        assert_eq!(lib.key_range(), Some((21, 108)));
        // VCSL recorded the release up to D6; the keys above ring on.
        for key in 21..=108 {
            assert_eq!(lib.layers(key), 3, "key {key}");
            assert_eq!(
                lib.zones_at(Trigger::Release, key, 64).len(),
                usize::from(key <= 87),
                "key {key}"
            );
        }
        assert_eq!(info.layers, 3);
    }

    #[test]
    fn the_strudel_piano_plays_its_nearest_recording_on_every_key() {
        let (info, parsed) = listed_files(STRUDEL_PIANO, strudel_piano::MANIFEST_SHA256);
        assert_eq!(parsed.samples.len(), 29);
        let roots: Vec<u8> = parsed.zones.iter().map(|z| z.root).collect();
        assert!(parsed.zones.iter().all(|z| z.release == 0.6));
        let lib = silent(info, parsed);
        assert_eq!(lib.key_range(), Some((21, 108)));
        assert!(!lib.has_release_zones());
        for key in 21..=108u8 {
            assert_eq!(lib.layers(key), 1, "key {key}");
            for vel in [1, 64, 127] {
                let zones = lib.zones_at(Trigger::Attack, key, vel);
                assert_eq!(zones.len(), 1, "key {key}");
                let root = lib.zones()[zones[0] as usize].root;
                // The nearest recording, the lower one of two as near, as
                // the sample bank picks.
                let nearest = roots
                    .iter()
                    .copied()
                    .min_by_key(|r| (i32::from(*r) - i32::from(key)).abs())
                    .expect("recordings");
                assert_eq!(root, nearest, "key {key}");
            }
        }
        assert_eq!(info.layers, 1);
        assert_eq!(info.license, License::CcBy3);
        assert_eq!(
            info.credit,
            crate::packs::find("piano").and_then(|p| p.credit)
        );
    }

    #[test]
    fn paths_are_percent_encoded_for_urls() {
        assert_eq!(
            percent_encode("NoSus/JHPiano_NoSus_Close_A#0_vl2_rr1.wav"),
            "NoSus/JHPiano_NoSus_Close_A%230_vl2_rr1.wav"
        );
        assert_eq!(percent_encode("a b/c~d-e.f"), "a%20b/c~d-e.f");
        assert_eq!(percent_encode("é"), "%C3%A9");
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
