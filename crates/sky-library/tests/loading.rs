// SPDX-License-Identifier: GPL-3.0-or-later
//! Loading packs and instrument libraries over HTTP from a local test
//! server, and packs from a folder.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use sky_dsp::AudioData;
use sky_dsp::InstrumentType;
use sky_dsp::instruments::sampled::Trigger;
use sky_io::{WavFormat, write_wav};
use sky_library::{InstrumentInfo, InstrumentSource, Instruments, Library, PackSource, Stage};
use sky_model::License;

/// Serves `files` by path on 127.0.0.1 until `stop` is set. Counts requests.
struct Server {
    base: String,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

impl Server {
    fn start(files: HashMap<String, Vec<u8>>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let files = Arc::new(Mutex::new(files));
        let (req, st) = (requests.clone(), stop.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                        break;
                    }
                }
                if st.load(Ordering::SeqCst) {
                    let _ = stream.write_all(
                        b"HTTP/1.1 503 Down\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    continue;
                }
                req.fetch_add(1, Ordering::SeqCst);
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                let body = files.lock().unwrap().get(&path).cloned();
                let head = match &body {
                    Some(b) => format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        b.len()
                    ),
                    None => {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_string()
                    }
                };
                let _ = stream.write_all(head.as_bytes());
                if let Some(b) = body {
                    let _ = stream.write_all(&b);
                }
            }
        });
        Server {
            base,
            requests,
            stop,
        }
    }
}

fn wav(dir: &Path, name: &str, level: f32, frames: usize) -> Vec<u8> {
    let path = dir.join(name);
    let audio = AudioData {
        sample_rate: 44_100,
        channels: vec![vec![level; frames]],
    };
    write_wav(&path, &audio, WavFormat::Pcm16, false).unwrap();
    std::fs::read(path).unwrap()
}

const MAP: &str = r#"{
    "Kit_bd": ["bd/0.wav", "bd/1 two.wav"],
    "Kit_sd": "sd.wav",
    "Kit_oh": "missing.wav",
    "keys": { "C4": "keys/c4.wav", "G3": "keys/g3.wav" },
    "Other_cp": "cp.wav",
    "Other_rim": "rim.wav"
}"#;

#[test]
fn packs_load_over_http_once_and_then_from_the_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let mut files = HashMap::new();
    files.insert("/kit/strudel.json".to_string(), MAP.as_bytes().to_vec());
    files.insert(
        "/kit/bd/0.wav".to_string(),
        wav(tmp.path(), "a.wav", 0.1, 500),
    );
    files.insert(
        "/kit/bd/1%20two.wav".to_string(),
        wav(tmp.path(), "b.wav", 0.2, 600),
    );
    files.insert(
        "/kit/sd.wav".to_string(),
        wav(tmp.path(), "c.wav", 0.3, 700),
    );
    files.insert(
        "/kit/keys/c4.wav".to_string(),
        wav(tmp.path(), "d.wav", 0.4, 800),
    );
    files.insert(
        "/kit/keys/g3.wav".to_string(),
        wav(tmp.path(), "e.wav", 0.5, 900),
    );
    let server = Server::start(files);
    let library = Library::new(tmp.path().join("cache"));
    let source = PackSource::parse(&format!("{}/kit/", server.base), Some(License::Cc0));
    assert_eq!(source.name, "kit");

    let pack = library
        .open(&PackSource {
            map: format!("{}/kit/strudel.json", server.base),
            ..source.clone()
        })
        .unwrap();
    assert!(!pack.offline);
    assert_eq!(
        pack.map.banks().into_iter().collect::<Vec<_>>(),
        [("Kit".to_string(), 3), ("Other".to_string(), 2)]
    );

    // A bank, with one file missing on the server.
    let selection = pack.select(Some("kit"), &[]).unwrap();
    assert_eq!(selection.name, "kit");
    assert_eq!(selection.file_count(), 4);
    let bank = library.load(&pack, &selection).unwrap();
    assert_eq!(bank.sounds.keys().collect::<Vec<_>>(), ["bd", "sd"]);
    assert_eq!(bank.sounds["bd"].len(), 2);
    assert_eq!(
        bank.sounds["bd"][1].audio.frames(),
        600,
        "map order is kept"
    );
    assert_eq!(bank.failed.len(), 1);
    assert!(bank.failed[0].contains("missing.wav"), "{:?}", bank.failed);
    assert_eq!(bank.source.license, License::Cc0);

    // The pitched sound keeps its notes.
    let keys = pack.select(None, &["KEYS".to_string()]).unwrap();
    assert_eq!(keys.name, "keys");
    let loaded = library.load(&pack, &keys).unwrap();
    let roots: Vec<_> = loaded.sounds["keys"].iter().map(|s| s.root).collect();
    assert_eq!(roots, [Some(55), Some(60)]);

    // Loading again fetches only the map; the files come from the cache.
    let before = server.requests.load(Ordering::SeqCst);
    let pack = library.open(&pack.source).unwrap();
    library.load(&pack, &selection).unwrap();
    // The map, and the missing file again.
    assert_eq!(server.requests.load(Ordering::SeqCst) - before, 2);

    // With the server down, the saved map and cached files still work.
    server.stop.store(true, Ordering::SeqCst);
    let pack = library.open(&pack.source).unwrap();
    assert!(pack.offline);
    let again = library.load(&pack, &selection).unwrap();
    assert_eq!(again.sounds["sd"][0].audio.frames(), 700);

    // Choices that cannot work say what can.
    let err = pack.select(Some("Nope"), &[]).unwrap_err().to_string();
    assert!(err.contains("Kit, Other"), "{err}");
    let err = pack
        .select(Some("Kit"), &["hh".into()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("bd, oh, sd"), "{err}");
}

#[test]
fn packs_load_from_a_local_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("my kit");
    std::fs::create_dir_all(dir.join("bd")).unwrap();
    wav(&dir.join("bd"), "kick 1.wav", 0.5, 1_000);
    std::fs::write(
        dir.join("strudel.json"),
        r#"{"bd": ["bd/kick%201.wav"], "sd": ["nowhere.wav"]}"#,
    )
    .unwrap();
    let library = Library::new(tmp.path().join("cache"));
    let source = PackSource::parse(&dir.to_string_lossy(), None);
    assert_eq!(source.license, License::Unknown);
    let pack = library.open(&source).unwrap();
    let selection = pack.select(None, &[]).unwrap();
    assert_eq!(selection.name, "my kit");
    let bank = library.load(&pack, &selection).unwrap();
    assert_eq!(bank.sounds["bd"][0].audio.frames(), 1_000);
    assert_eq!(bank.failed.len(), 1);
    // Nothing from a local pack goes into the cache.
    assert!(!tmp.path().join("cache").exists());

    let only_missing = pack.select(None, &["sd".into()]).unwrap();
    let err = library.load(&pack, &only_missing).unwrap_err().to_string();
    assert!(err.starts_with("none of the 1 files"), "{err}");
}

/// Loads from Strudel's real packs on GitHub. Needs the network, so it only
/// runs when asked: `cargo test -p sky-library -- --ignored`.
#[test]
#[ignore = "downloads from GitHub"]
fn strudel_packs_load_from_github() {
    let tmp = tempfile::tempdir().unwrap();
    let library = Library::new(tmp.path().to_path_buf());
    let tdm = library
        .open(&PackSource::parse("tidal-drum-machines", None))
        .unwrap();
    assert!(tdm.map.banks().contains_key("RolandTR808"));
    let selection = tdm
        .select(
            Some("RolandTR808"),
            &["bd".into(), "sd".into(), "hh".into()],
        )
        .unwrap();
    let bank = library.load(&tdm, &selection).unwrap();
    assert!(bank.failed.is_empty(), "{:?}", bank.failed);
    assert!(bank.sounds["bd"].len() >= 2);

    let piano = library.open(&PackSource::parse("piano", None)).unwrap();
    let selection = piano.select(None, &[]).unwrap();
    assert_eq!(selection.name, "piano");
    let loaded = library.load(&piano, &selection).unwrap();
    assert!(loaded.failed.is_empty(), "{:?}", loaded.failed);
    assert!(loaded.sounds["piano"].iter().all(|s| s.root.is_some()));

    // VCSL paths are already percent-encoded and have spaces in them.
    let vcsl = library.open(&PackSource::parse("vcsl", None)).unwrap();
    let selection = vcsl.select(None, &["bassdrum1".into()]).unwrap();
    let loaded = library.load(&vcsl, &selection).unwrap();
    assert!(loaded.failed.is_empty(), "{:?}", loaded.failed);

    // Clean-breaks paths are not encoded and have & ' ( ) [ ] in them.
    let breaks = library
        .open(&PackSource::parse("clean-breaks", None))
        .unwrap();
    let selection = breaks.select(None, &["kool".into(), "do".into()]).unwrap();
    let loaded = library.load(&breaks, &selection).unwrap();
    assert!(loaded.failed.is_empty(), "{:?}", loaded.failed);
}

/// A tiny piano library packed the way FreePats packs theirs: one folder
/// holding the SFZ file and its samples. With a `method`, every file is
/// coded with it; without, the folder is packed as 7-Zip packs one.
fn piano_archive(dir: &Path, method: Option<sevenz_rust2::EncoderMethod>) -> Vec<u8> {
    let lib = dir.join("src").join("Tiny-1");
    std::fs::create_dir_all(lib.join("samples")).unwrap();
    std::fs::write(
        lib.join("Tiny.sfz"),
        "<group> ampeg_release=0.5\r\n\
         <region> sample=samples/c4.wav pitch_keycenter=60 lokey=0 hikey=127\r\n",
    )
    .unwrap();
    std::fs::write(
        lib.join("samples").join("c4.wav"),
        wav(dir, "c4.wav", 0.3, 4_410),
    )
    .unwrap();
    let archive = dir.join("tiny.7z");
    match method {
        None => sevenz_rust2::compress_to_path(dir.join("src"), &archive).unwrap(),
        Some(method) => {
            let mut writer = sevenz_rust2::ArchiveWriter::create(&archive).unwrap();
            writer.set_content_methods(vec![method.into()]);
            for name in ["Tiny-1/Tiny.sfz", "Tiny-1/samples/c4.wav"] {
                let path = dir.join("src").join(name);
                let entry = sevenz_rust2::ArchiveEntry::from_path(&path, name.to_string());
                let file = std::fs::File::open(&path).unwrap();
                writer.push_archive_entry(entry, Some(file)).unwrap();
            }
            writer.finish().unwrap();
        }
    }
    std::fs::read(archive).unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn tiny_info(url: String, bytes: u64, sha256: String) -> InstrumentInfo {
    InstrumentInfo {
        id: "tiny-piano",
        name: "Tiny piano",
        kind: InstrumentType::Piano,
        version: "1",
        description: "",
        license: License::Cc0,
        credit: None,
        homepage: "",
        size: 0,
        layers: 1,
        memory: 0,
        source: InstrumentSource::Download {
            url: Box::leak(url.into_boxed_str()),
            bytes,
            sha256: Box::leak(sha256.into_boxed_str()),
            sfz: "Tiny-1/Tiny.sfz",
        },
    }
}

#[test]
fn instrument_libraries_install_once_and_only_when_they_match() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = piano_archive(tmp.path(), None);
    let mut files = HashMap::new();
    files.insert("/tiny.7z".to_string(), archive.clone());
    let server = Server::start(files);
    let url = format!("{}/tiny.7z", server.base);
    let instruments = Instruments::new(tmp.path().join("instruments"));

    // An archive that is not the one listed is refused and leaves nothing.
    let wrong = tiny_info(url.clone(), archive.len() as u64, "00".repeat(32));
    let e = instruments.install(&wrong, &mut |_, _, _| {}).unwrap_err();
    assert!(e.to_string().contains("not the one expected"), "{e}");
    assert!(!instruments.is_installed(&wrong));
    assert!(instruments.load(&wrong).is_err());
    let missing = tiny_info(
        format!("{}/gone.7z", server.base),
        archive.len() as u64,
        sha256_hex(&archive),
    );
    assert!(instruments.install(&missing, &mut |_, _, _| {}).is_err());

    let info = tiny_info(url, archive.len() as u64, sha256_hex(&archive));
    assert!(!instruments.is_installed(&info));
    let mut seen = Vec::new();
    instruments
        .install(&info, &mut |stage, done, total| {
            seen.push((stage, done, total))
        })
        .unwrap();
    let downloaded = seen.iter().rfind(|s| s.0 == Stage::Download);
    assert_eq!(
        downloaded,
        Some(&(Stage::Download, archive.len() as u64, archive.len() as u64))
    );
    // Unpacking follows, and its count only grows.
    let unpacked: Vec<u64> = seen
        .iter()
        .filter(|s| s.0 == Stage::Unpack)
        .map(|s| s.1)
        .collect();
    assert!(!unpacked.is_empty() && unpacked.is_sorted(), "{seen:?}");
    assert_eq!(seen.last().map(|s| s.0), Some(Stage::Unpack));
    assert!(instruments.is_installed(&info));
    let folder = instruments.folder(&info);
    assert!(
        folder
            .join("Tiny-1")
            .join("samples")
            .join("c4.wav")
            .is_file()
    );
    // Only the unpacked library is kept.
    let downloads = instruments.dir().join("downloads");
    assert_eq!(std::fs::read_dir(&downloads).unwrap().count(), 0);

    let lib = instruments.load(&info).unwrap();
    assert_eq!(lib.zones().len(), 1);
    assert_eq!(lib.key_range(), Some((0, 127)));
    assert!((lib.zones()[0].release - 0.5).abs() < 1e-6);

    // Installed: nothing is downloaded again.
    let requests = server.requests.load(Ordering::SeqCst);
    instruments.install(&info, &mut |_, _, _| {}).unwrap();
    assert_eq!(server.requests.load(Ordering::SeqCst), requests);
    server.stop.store(true, Ordering::SeqCst);
}

#[test]
fn deflate_archives_unpack() {
    // FreePats packs the full Upright KW with DEFLATE.
    let tmp = tempfile::tempdir().unwrap();
    let archive = piano_archive(tmp.path(), Some(sevenz_rust2::EncoderMethod::DEFLATE));
    let mut files = HashMap::new();
    files.insert("/tiny.7z".to_string(), archive.clone());
    let server = Server::start(files);
    let instruments = Instruments::new(tmp.path().join("instruments"));
    let info = tiny_info(
        format!("{}/tiny.7z", server.base),
        archive.len() as u64,
        sha256_hex(&archive),
    );
    instruments.install(&info, &mut |_, _, _| {}).unwrap();
    assert_eq!(instruments.load(&info).unwrap().zones().len(), 1);
    server.stop.store(true, Ordering::SeqCst);
}

/// A `.tar.gz` of a two-layer piano, the way FreePats packs Salamander,
/// with a link that must not be followed and, given `escape`, an entry that
/// would land outside the library.
fn layered_tar_gz(dir: &Path, escape: bool) -> Vec<u8> {
    let sfz = "<group> ampeg_release=0.5\n\
               <region> sample=samples/C4v1.flac lokey=0 hikey=127 lovel=1 hivel=64\n\
               <region> sample=samples/C4v2.flac lokey=0 hikey=127 lovel=65\n\
               <group> trigger=release\n<region> sample=samples/rel1.flac\n";
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    let mut add = |name: &str, bytes: &[u8]| {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, bytes).unwrap();
    };
    add("Tiny-1/Tiny.sfz", sfz.as_bytes());
    for (name, level) in [("C4v1", 0.2), ("C4v2", 0.6), ("rel1", 0.1)] {
        let file = format!("Tiny-1/samples/{name}.flac");
        // Any audio file reads by its contents; WAV is the easy one to make.
        add(&file, &wav(dir, &format!("{name}.wav"), level, 2_205));
    }
    let mut link = tar::Header::new_gnu();
    link.set_entry_type(tar::EntryType::Symlink);
    link.set_size(0);
    builder
        .append_link(&mut link, "Tiny-1/samples/elsewhere", "/etc/passwd")
        .unwrap();
    if escape {
        // `append_data` refuses `..`, so the name goes in by hand.
        let mut header = tar::Header::new_old();
        header.as_old_mut().name[..9].copy_from_slice(b"../evil.t");
        header.set_size(0);
        header.set_cksum();
        builder.append(&header, std::io::empty()).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

#[test]
fn tar_gz_libraries_install_and_load_fewer_layers() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = layered_tar_gz(tmp.path(), false);
    let evil = layered_tar_gz(tmp.path(), true);
    let mut files = HashMap::new();
    files.insert("/tiny.tar.gz".to_string(), archive.clone());
    files.insert("/evil.tar.gz".to_string(), evil.clone());
    let server = Server::start(files);
    let instruments = Instruments::new(tmp.path().join("instruments"));

    let bad = tiny_info(
        format!("{}/evil.tar.gz", server.base),
        evil.len() as u64,
        sha256_hex(&evil),
    );
    let e = instruments.install(&bad, &mut |_, _, _| {}).unwrap_err();
    assert!(e.to_string().contains("outside"), "{e}");
    assert!(!instruments.is_installed(&bad));
    assert!(!tmp.path().join("instruments").join("evil.t").exists());

    let mut info = tiny_info(
        format!("{}/tiny.tar.gz", server.base),
        archive.len() as u64,
        sha256_hex(&archive),
    );
    info.layers = 2;
    instruments.install(&info, &mut |_, _, _| {}).unwrap();
    let samples = instruments.folder(&info).join("Tiny-1").join("samples");
    assert!(samples.join("C4v2.flac").is_file());
    assert!(!samples.join("elsewhere").exists(), "links are left out");

    let all = instruments.load(&info).unwrap();
    assert_eq!((all.samples().len(), all.layers(60)), (3, 2));
    let mut seen = Vec::new();
    let one = instruments
        .load_with(&info, Some(1), &mut |stage, done, total| {
            seen.push((stage, done, total))
        })
        .unwrap();
    // The loud layer now covers every velocity, and the soft one is never
    // decoded.
    assert_eq!((one.samples().len(), one.layers(60)), (2, 1));
    assert_eq!(one.zones_at(Trigger::Attack, 60, 10).len(), 1);
    assert_eq!(seen.first(), Some(&(Stage::Load, 0, 2)));
    assert_eq!(seen.last(), Some(&(Stage::Load, 2, 2)));
    server.stop.store(true, Ordering::SeqCst);
}

/// A library of separate files at `base`, played by the built-in Steinway
/// SFZ file.
fn files_info(base: &str, files: &[(&str, &[u8])]) -> InstrumentInfo {
    let files: Vec<(&'static str, u64, &'static str)> = files
        .iter()
        .map(|(path, bytes)| {
            let path: &'static str = Box::leak(path.to_string().into_boxed_str());
            let sha: &'static str = Box::leak(sha256_hex(bytes).into_boxed_str());
            (path, bytes.len() as u64, sha)
        })
        .collect();
    InstrumentInfo {
        source: InstrumentSource::Files {
            base: Box::leak(base.to_string().into_boxed_str()),
            files: Box::leak(files.into_boxed_slice()),
            sha256: "list-hash",
            sfz: "vcsl-steinway-b/SteinwayB.sfz",
        },
        ..tiny_info(String::new(), 0, String::new())
    }
}

#[test]
fn file_libraries_download_each_file_and_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let a = wav(tmp.path(), "a.wav", 0.1, 500);
    let b = wav(tmp.path(), "b.wav", 0.2, 700);
    let list: [(&str, &[u8]); 2] = [("NoSus/A#0 soft.wav", &a), ("Rel/b.wav", &b)];
    let total = (a.len() + b.len()) as u64;
    // The first server lacks the second file, so the first try fails.
    let mut some = HashMap::new();
    some.insert("/lib/NoSus/A%230%20soft.wav".to_string(), a.clone());
    let first = Server::start(some.clone());
    let instruments = Instruments::new(tmp.path().join("instruments"));
    let info = files_info(&format!("{}/lib/", first.base), &list);
    let e = instruments
        .install(&info, &mut |_, _, _| {})
        .unwrap_err()
        .to_string();
    assert!(e.contains("Rel/b.wav"), "{e}");
    assert!(!instruments.is_installed(&info));
    // The second try only gets the file still missing.
    some.insert("/lib/Rel/b.wav".to_string(), b.clone());
    let second = Server::start(some);
    let info = files_info(&format!("{}/lib/", second.base), &list);
    let mut seen = Vec::new();
    instruments
        .install(&info, &mut |stage, done, of| seen.push((stage, done, of)))
        .unwrap();
    assert_eq!(second.requests.load(Ordering::SeqCst), 1);
    assert!(
        seen.iter()
            .all(|(stage, _, of)| *stage == Stage::Download && *of == total)
    );
    assert!(seen.windows(2).all(|w| w[0].1 <= w[1].1), "{seen:?}");
    assert_eq!(seen.last(), Some(&(Stage::Download, total, total)));
    assert!(instruments.is_installed(&info));
    // The downloads, and the built-in SFZ file and readme beside them.
    let folder = instruments.folder(&info);
    assert_eq!(
        std::fs::read(folder.join("NoSus").join("A#0 soft.wav")).unwrap(),
        a
    );
    assert_eq!(std::fs::read(folder.join("Rel").join("b.wav")).unwrap(), b);
    assert!(folder.join("SteinwayB.sfz").is_file());
    assert!(folder.join("readme.txt").is_file());
    assert!(
        !tmp.path()
            .join("instruments")
            .join("tiny-piano-1.part")
            .exists()
    );
    // A file that is not the one listed is refused.
    let mut wrong = HashMap::new();
    wrong.insert(
        "/lib/NoSus/A%230%20soft.wav".to_string(),
        wav(tmp.path(), "c.wav", 0.3, 500),
    );
    let third = Server::start(wrong);
    let other = Instruments::new(tmp.path().join("other"));
    let info = files_info(&format!("{}/lib/", third.base), &list);
    let e = other
        .install(&info, &mut |_, _, _| {})
        .unwrap_err()
        .to_string();
    assert!(e.contains("not the one expected"), "{e}");
    for s in [first, second, third] {
        s.stop.store(true, Ordering::SeqCst);
    }
}

/// Installs and loads a library from the folder `SKY_INSTRUMENTS` names,
/// printing how long it took and the memory it uses.
fn install_and_load(id: &str, layers: Option<u8>) -> sky_dsp::instruments::SampleLibrary {
    let dir = std::env::var_os("SKY_INSTRUMENTS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("skyware-sound-instruments-test"));
    let instruments = Instruments::new(dir);
    let info = sky_library::find_instrument(id).unwrap();
    let started = std::time::Instant::now();
    instruments.install(info, &mut |_, _, _| {}).unwrap();
    let installed = started.elapsed();
    let started = std::time::Instant::now();
    let lib = instruments
        .load_with(info, layers, &mut |_, _, _| {})
        .unwrap();
    println!(
        "{id} with {layers:?} layers: installed in {installed:?}, loaded in {:?}: {} samples, \
         {} zones, {:.0} MB, stereo {}",
        started.elapsed(),
        lib.samples().len(),
        lib.zones().len(),
        lib.bytes() as f64 / 1e6,
        lib.is_stereo()
    );
    lib
}

#[test]
#[ignore = "downloads 543 MB from raw.githubusercontent.com"]
fn the_steinway_b_installs_and_loads() {
    let lib = install_and_load("vcsl-steinway-b", None);
    assert_eq!(lib.samples().len(), 225);
    assert_eq!(lib.key_range(), Some((21, 108)));
    for key in 21..=108 {
        assert_eq!(lib.layers(key), 3, "key {key}");
    }
}

#[test]
#[ignore = "downloads 707 MB from freepats.zenvoid.org"]
fn the_salamander_grand_installs_and_loads() {
    let all = install_and_load("salamander-grand-piano", None);
    assert_eq!(all.samples().len(), 641);
    assert_eq!(all.key_range(), Some((21, 108)));
    for key in 21..=108 {
        assert_eq!(all.layers(key), 16, "key {key}");
    }
    assert_eq!(all.zones_at(Trigger::PedalDown, 0, 1).len(), 2);
    let half = install_and_load("salamander-grand-piano", Some(8));
    assert_eq!(half.layers(60), 8);
    assert!(half.bytes() < all.bytes() * 6 / 10);
}

#[test]
#[ignore = "downloads 33 MB from freepats.zenvoid.org"]
fn the_full_upright_piano_installs_and_loads() {
    let dir = std::env::var_os("SKY_INSTRUMENTS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("skyware-sound-instruments-test"));
    let instruments = Instruments::new(dir);
    let info = sky_library::find_instrument("upright-piano-kw").unwrap();
    let started = std::time::Instant::now();
    instruments.install(info, &mut |_, _, _| {}).unwrap();
    let installed = started.elapsed();
    let started = std::time::Instant::now();
    let lib = instruments.load(info).unwrap();
    println!(
        "installed in {installed:?}, loaded in {:?}: {} samples, {} zones, {:.0} MB, stereo {}",
        started.elapsed(),
        lib.samples().len(),
        lib.zones().len(),
        lib.bytes() as f64 / 1e6,
        lib.is_stereo()
    );
    assert_eq!(lib.samples().len(), 66);
    let (low, high) = lib.key_range().unwrap();
    assert!(low <= 21 && high >= 108, "{low}..{high}");
    assert!(lib.is_stereo());
    for key in 21..=108 {
        assert_eq!(lib.layers(key), 2, "key {key}");
    }
}
