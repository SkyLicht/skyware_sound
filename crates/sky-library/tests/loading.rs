// SPDX-License-Identifier: GPL-3.0-or-later
//! Loading packs over HTTP from a local test server, and from a folder.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use sky_dsp::AudioData;
use sky_io::{WavFormat, write_wav};
use sky_library::{Library, PackSource};
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
