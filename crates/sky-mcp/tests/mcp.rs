// SPDX-License-Identifier: GPL-3.0-or-later
//! The MCP server driven by a real MCP client, in process. The main test is
//! the alpha gate: an assistant builds a 16-bar loop and exports it for the
//! game, and the game runtime plays the export.

use std::time::Duration;

use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::RunningService;
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use serde_json::{Value, json};
use sky_mcp::{Permission, Policy, SkywareMcp};
use sky_model::templates::empty_project;
use sky_session::Session;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone, Default)]
struct Client;

impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::default()
    }
}

type Running = RunningService<RoleClient, Client>;

async fn connect(server: SkywareMcp) -> Running {
    let (server_io, client_io) = tokio::io::duplex(1 << 16);
    tokio::spawn(async move {
        if let Ok(s) = server.serve(server_io).await {
            let _ = s.waiting().await;
        }
    });
    Client.serve(client_io).await.unwrap()
}

/// Calls a tool and returns its text and whether it reported an error.
async fn call(client: &Running, tool: &str, args: Value) -> (String, bool) {
    let mut params = CallToolRequestParams::new(tool.to_string());
    if let Value::Object(map) = args {
        params = params.with_arguments(map);
    }
    let result = client.call_tool(params).await.unwrap();
    let text = result
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.clone())
        .unwrap_or_default();
    (text, result.is_error == Some(true))
}

async fn ok(client: &Running, tool: &str, args: Value) -> Value {
    let (text, is_error) = call(client, tool, args).await;
    assert!(!is_error, "{tool} failed: {text}");
    serde_json::from_str(&text).unwrap_or(Value::String(text))
}

#[tokio::test(flavor = "multi_thread")]
async fn an_assistant_builds_and_exports_a_sixteen_bar_loop() {
    let tmp = tempfile::tempdir().unwrap();
    let (session, _) = Session::new(empty_project("Gate"), None);
    let server = SkywareMcp::new(
        session.clone(),
        Policy::new(Permission::Files, vec![tmp.path().to_path_buf()]),
    );
    let client = connect(server).await;

    let tools = client.list_all_tools().await.unwrap();
    assert!(tools.len() >= 30, "{} tools", tools.len());

    ok(&client, "set_tempo", json!({ "bpm": 100 })).await;
    ok(
        &client,
        "track_create",
        json!({ "name": "Drums", "instrument": "drums" }),
    )
    .await;
    ok(
        &client,
        "track_create",
        json!({ "name": "Bass", "params": { "wave": "square", "cutoff": 900 } }),
    )
    .await;
    ok(
        &client,
        "clip_create",
        json!({
            "track": "Drums", "start_bar": 1, "length_bars": 16,
            "pattern": "bd*4, ~ sd ~ sd, hh*8", "name": "Beat"
        }),
    )
    .await;
    let clip = ok(
        &client,
        "clip_create",
        json!({
            "track": "Bass", "start_bar": 1, "length_bars": 16, "name": "Bassline",
            "notes": "A1 1/4, A1 1/8, C2 1/8, E2 1/4, G1 1/4"
        }),
    )
    .await;
    assert_eq!(clip["noteCount"], 5);
    ok(
        &client,
        "effect_insert",
        json!({ "channel": "master", "type": "limiter" }),
    )
    .await;
    ok(
        &client,
        "effect_insert",
        json!({ "channel": "Bass", "type": "reverb", "params": { "mix": 0.3 } }),
    )
    .await;
    ok(
        &client,
        "track_set",
        json!({ "track": "Bass", "volume_db": -4, "pan": -0.2, "picture": "acid bass" }),
    )
    .await;
    ok(
        &client,
        "set_loop",
        json!({ "start_bar": 1, "end_bar": 17 }),
    )
    .await;

    let summary = ok(&client, "project_summary", json!({})).await;
    assert_eq!(summary["tempoBpm"], 100.0);
    assert_eq!(summary["tracks"].as_array().unwrap().len(), 2);
    assert_eq!(summary["loop"]["endBar"], 17.0);
    let bass = summary["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "Bass")
        .unwrap();
    assert_eq!(bass["picture"], "acid bass");

    let seam = ok(&client, "loop_check_seam", json!({})).await;
    assert_eq!(seam["seam"]["verdict"], "seamless", "{seam}");

    let export = ok(
        &client,
        "export_loop",
        json!({ "name": "Battle Loop", "dir": "game/audio", "bits": 24 }),
    )
    .await;
    assert_eq!(export["bars"], 16.0);
    assert_eq!(export["seam"]["verdict"], "seamless");

    // Every change is one undo step, labelled with the tool that made it.
    let view = session.view();
    assert_eq!(view.activity.len(), 9);
    assert!(view.activity.iter().all(|a| !a.undone));
    let (text, failed) = call(
        &client,
        "track_set",
        json!({ "track": "Bass", "picture": "kazoo" }),
    )
    .await;
    assert!(failed && text.contains("no picture symbol"), "{text}");

    // The game runtime loads the export and plays it.
    let dir = tmp.path().join("game/audio");
    let mut rt = sky_runtime::Runtime::new(48_000);
    rt.load_manifest(&dir).unwrap();
    let voice = rt.play_by_name("battle_loop", Default::default()).unwrap();
    let mut buf = vec![0.0f32; 2 * 48_000];
    rt.render(&mut buf);
    assert!(rt.is_playing(voice));
    assert!(buf.iter().any(|s| s.abs() > 0.01));
    // 16 bars of 4/4 at 100 BPM is 38.4 seconds.
    let sky_core::manifest::Asset::Loop(asset) =
        rt.asset_info(rt.asset("battle_loop").unwrap()).unwrap()
    else {
        panic!("expected a loop");
    };
    assert_eq!(asset.loop_end, (38.4 * 48_000.0) as u64);

    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_system_check_exports_and_verifies_a_loop() {
    let tmp = tempfile::tempdir().unwrap();
    let (session, _) = Session::new(empty_project("Open"), None);
    let client = connect(SkywareMcp::new(
        session.clone(),
        Policy::new(Permission::Files, vec![tmp.path().to_path_buf()]),
    ))
    .await;
    // No audio device in CI, so only the loop half.
    let report = ok(
        &client,
        "system_check",
        json!({ "device_seconds": 0, "dir": "check" }),
    )
    .await;
    assert_eq!(report["passed"], true, "{report}");
    assert!(report.get("device").is_none());
    assert_eq!(report["loop"]["bars"], 16.0);
    assert_eq!(report["loop"]["checks"].as_array().unwrap().len(), 5);
    assert!(tmp.path().join("check/skyware-manifest.json").exists());
    // The open project is left alone.
    assert!(session.view().history.is_empty());
    assert!(session.project().tracks.is_empty());

    let (text, is_error) = call(
        &client,
        "system_check",
        json!({ "device_seconds": 0, "bars": 0 }),
    )
    .await;
    assert!(is_error, "{text}");
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn mistakes_come_back_as_tool_errors_and_undo_works() {
    let (session, _) = Session::new(sky_model::templates::demo_project(), None);
    let client = connect(SkywareMcp::new(
        session.clone(),
        Policy::new(Permission::Edit, vec![]),
    ))
    .await;

    let (text, is_error) = call(&client, "set_tempo", json!({ "bpm": 5000 })).await;
    assert!(is_error);
    assert!(text.contains("tempo"), "{text}");

    let (text, is_error) = call(
        &client,
        "clip_create",
        json!({
            "track": "Bass", "start_bar": 1, "length_bars": 1, "notes": "C4 1/4, H4 1/4"
        }),
    )
    .await;
    assert!(is_error);
    assert!(text.contains("entry 2"), "{text}");

    let (text, is_error) = call(&client, "pattern_query", json!({ "pattern": "bd [sd" })).await;
    assert!(is_error, "{text}");

    let (text, is_error) = call(&client, "track_get", json!({ "track": "Nope" })).await;
    assert!(is_error);
    assert!(
        text.contains("Drums"),
        "the error lists the real tracks: {text}"
    );

    let before = session.project();
    ok(&client, "set_tempo", json!({ "bpm": 140 })).await;
    ok(&client, "track_delete", json!({ "track": "Pad" })).await;
    let undone = ok(&client, "undo", json!({ "steps": 2 })).await;
    assert_eq!(undone["undone"].as_array().unwrap().len(), 2);
    assert_eq!(*session.project(), *before);

    // File tools are not offered without the Files permission.
    let names: Vec<String> = client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    assert!(names.contains(&"notes_write".to_string()));
    assert!(!names.contains(&"export_loop".to_string()));
    assert!(!names.contains(&"project_save".to_string()));
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_clients_cannot_edit() {
    let (session, _) = Session::new(sky_model::templates::demo_project(), None);
    let server = SkywareMcp::new(session, Policy::new(Permission::ReadOnly, vec![]));
    let names = server.tool_names();
    assert!(names.contains(&"project_summary".to_string()));
    assert!(!names.contains(&"set_tempo".to_string()));
    let client = connect(server).await;
    let summary = ok(&client, "project_summary", json!({})).await;
    assert_eq!(summary["name"], "Demo loop");
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn notes_and_patterns_round_trip() {
    let (session, _) = Session::new(sky_model::templates::demo_project(), None);
    let client = connect(SkywareMcp::new(
        session.clone(),
        Policy::new(Permission::Edit, vec![]),
    ))
    .await;
    let written = ok(
        &client,
        "notes_write",
        json!({ "clip": "Bass line", "notes": "C2 1/4, r 1/4, G2 1/2 v70" }),
    )
    .await;
    // Rests come back as an @ position.
    assert_eq!(written["notes"], "C2 1/4, G2 1/2 v70 @2");
    ok(
        &client,
        "pattern_set",
        json!({ "clip": "Beat", "pattern": "bd sd, hh*4" }),
    )
    .await;
    let clip = ok(&client, "clip_get", json!({ "clip": "Beat" })).await;
    assert_eq!(clip["pattern"], "bd sd, hh*4");
    client.cancel().await.unwrap();
}

/// The phase 1 gate: an assistant builds a techno loop with ducked bass
/// and accented, swung hats from the built-in instruments, and it loops
/// cleanly.
#[tokio::test(flavor = "multi_thread")]
async fn an_assistant_builds_a_ducked_techno_loop() {
    let (session, _) = Session::new(empty_project("Techno"), None);
    let client = connect(SkywareMcp::new(
        session.clone(),
        Policy::new(Permission::Edit, vec![]),
    ))
    .await;
    ok(&client, "set_tempo", json!({ "bpm": 128 })).await;
    for (name, instrument, params) in [
        (
            "Kick",
            "drums",
            json!({ "style": "909", "kick_drive": 9, "kick_tune": -3 }),
        ),
        ("Hats", "drums", json!({ "style": "909" })),
        (
            "Bass",
            "synth",
            json!({ "cutoff": 600, "resonance": 0.4, "gain": -10 }),
        ),
    ] {
        ok(
            &client,
            "track_create",
            json!({ "name": name, "instrument": instrument, "params": params }),
        )
        .await;
    }
    ok(
        &client,
        "clip_create",
        json!({ "track": "Kick", "start_bar": 1, "length_bars": 4, "pattern": "bd*4" }),
    )
    .await;
    let hats = ok(
        &client,
        "clip_create",
        json!({
            "track": "Hats", "start_bar": 1, "length_bars": 4, "pattern": "hh*16",
            "name": "Hats", "controls": { "velocity": "[70 45 127 45]*4" }, "swing": 56,
        }),
    )
    .await;
    assert_eq!(hats["swing"]["amount"], 56, "{hats}");
    ok(
        &client,
        "clip_create",
        json!({
            "track": "Bass", "start_bar": 1, "length_bars": 4, "name": "Bassline",
            "pattern": "[~ a1 a1 a1]*4", "controls": { "cutoff": "[500 900]*8" },
        }),
    )
    .await;
    // A control the instrument does not have is caught.
    let (text, is_error) = call(
        &client,
        "clip_create",
        json!({
            "track": "Kick", "start_bar": 1, "length_bars": 1, "pattern": "bd",
            "controls": { "cutoff": "400" },
        }),
    )
    .await;
    assert!(
        is_error && text.contains("drums") && text.contains("kick_tune"),
        "{text}"
    );

    // A duck on the bass follows the first drum track by itself.
    let duck = ok(
        &client,
        "effect_insert",
        json!({ "channel": "Bass", "type": "duck", "params": { "depth": 0.85 } }),
    )
    .await;
    assert_eq!(duck["sidechain"], "Kick", "{duck}");
    let duck = ok(
        &client,
        "effect_set",
        json!({
            "channel": "Bass", "effect": "duck",
            "params": { "trigger": "kick only" }, "sidechain": "Hats",
        }),
    )
    .await;
    assert_eq!(duck["sidechain"], "Hats");
    ok(
        &client,
        "effect_set",
        json!({ "channel": "Bass", "effect": "duck", "sidechain": "Kick" }),
    )
    .await;

    // pattern_set changes some controls and keeps the rest.
    let hats = ok(
        &client,
        "pattern_set",
        json!({ "clip": "Hats", "controls": { "hat_decay": "0.6 0.6 2 0.6" }, "swing": 58 }),
    )
    .await;
    assert_eq!(hats["controls"]["velocity"], "[70 45 127 45]*4", "{hats}");
    assert_eq!(hats["controls"]["hat_decay"], "0.6 0.6 2 0.6");
    assert_eq!(hats["pattern"], "hh*16");
    assert_eq!(hats["swing"]["amount"], 58);

    let query = ok(
        &client,
        "pattern_query",
        json!({ "pattern": "100 -12 0.5", "numbers": true }),
    )
    .await;
    let values: Vec<f64> = query["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["value"].as_f64().unwrap())
        .collect();
    assert_eq!(values, [100.0, -12.0, 0.5]);

    // Strudel code: its controls by Strudel's names, and what it leaves out.
    let query = ok(
        &client,
        "pattern_query",
        json!({ "pattern": "note(\"c3 e3\").s(\"sawtooth\").lpf(800).room(.5)" }),
    )
    .await;
    let first = &query["events"][0];
    assert_eq!(first["sound"], "sawtooth", "{query}");
    assert_eq!(first["midi"], 48);
    assert_eq!(first["controls"], json!({ "note": 48.0, "lpf": 800.0 }));
    assert_eq!(
        query["warnings"],
        json!(["room was ignored: effects go on the track's inserts here"])
    );
    let hats = ok(
        &client,
        "pattern_set",
        json!({ "clip": "Hats", "pattern": "s(\"hh*16\").gain(\".6 1\").pan(sine)" }),
    )
    .await;
    assert_eq!(hats["pattern"], "s(\"hh*16\").gain(\".6 1\").pan(sine)");
    assert_eq!(
        hats["warnings"],
        json!(["pan was ignored: effects go on the track's inserts here"])
    );

    ok(&client, "set_loop", json!({ "start_bar": 1, "end_bar": 5 })).await;
    let seam = ok(&client, "loop_check_seam", json!({})).await;
    assert_eq!(seam["seam"]["verdict"], "seamless", "{seam}");

    let techno = ok(
        &client,
        "project_new",
        json!({ "template": "techno", "name": "Club", "discard_unsaved": true }),
    )
    .await;
    assert_eq!(techno["tracks"].as_array().unwrap().len(), 4);
    client.cancel().await.unwrap();
}

/// The phase 2 gate over MCP: an assistant writes a 12-step Acid bass line
/// with accents, slides and locks over a 16-step beat, and it loops.
#[tokio::test(flavor = "multi_thread")]
async fn an_assistant_writes_an_acid_line_in_steps() {
    let (session, _) = Session::new(empty_project("Acid"), None);
    let client = connect(SkywareMcp::new(
        session.clone(),
        Policy::new(Permission::Edit, vec![]),
    ))
    .await;
    ok(&client, "set_tempo", json!({ "bpm": 128 })).await;
    ok(
        &client,
        "track_create",
        json!({ "name": "Drums", "instrument": "drums", "params": { "style": "909" } }),
    )
    .await;
    ok(
        &client,
        "track_create",
        json!({ "name": "Acid", "instrument": "acid", "params": { "resonance": 0.8, "env_mod": 0.6 } }),
    )
    .await;
    let beat = ok(
        &client,
        "steps_write",
        json!({
            "track": "Drums", "length_bars": 4, "name": "Beat",
            "lanes": [
                { "note": "bd", "steps": "X . . . x . . . x . . . x . . ." },
                { "note": "cp", "steps": ". . . . x . . . . . . . x . . x@fill" },
                { "note": "hh", "steps": "x . x?50 . x . x*2 . x . x?50 . x . x>10 .",
                  "locks": { "hat_decay": ". . . . . . 2 . . . . . . . . ." } },
            ],
        }),
    )
    .await;
    assert_eq!(beat["type"], "steps", "{beat}");
    assert_eq!(beat["lanes"][0]["note"], "C2");
    assert_eq!(
        beat["lanes"][2]["locks"]["hat_decay"],
        ". . . . . . 2 . . . . . . . . ."
    );
    let line = ok(
        &client,
        "steps_write",
        json!({
            "track": "Acid", "length_bars": 4, "name": "Line",
            "lanes": [{ "note": "c2", "steps": "x x/ c3! x . d#2/ f2 x! - g1 x a#1/",
                        "locks": { "cutoff": "300 . 900 . . . . . . . . ." } }],
        }),
    )
    .await;
    assert_eq!(
        line["lanes"][0]["steps"], "x x/ c3! x . d#2/ f2 X - g1 x a#1/",
        "{line}"
    );
    // A lock the instrument does not have, and a bad step, are caught.
    let (text, is_error) = call(
        &client,
        "steps_write",
        json!({ "clip": "Line", "lanes": [{ "note": "c2", "steps": "x", "locks": { "kick_tune": "1" } }] }),
    )
    .await;
    assert!(
        is_error && text.contains("acid") && text.contains("env_mod"),
        "{text}"
    );
    let (text, is_error) = call(
        &client,
        "steps_write",
        json!({ "clip": "Line", "lanes": [{ "note": "c2", "steps": "x x*9" }] }),
    )
    .await;
    assert!(
        is_error && text.contains("lane 1") && text.contains("step 2"),
        "{text}"
    );
    // Changing settings keeps the lanes, and pattern tools refuse step clips.
    let swung = ok(
        &client,
        "steps_write",
        json!({ "clip": "Beat", "swing": 56 }),
    )
    .await;
    assert_eq!(swung["swing"]["amount"], 56);
    assert_eq!(swung["lanes"].as_array().unwrap().len(), 3);
    let (text, is_error) = call(
        &client,
        "pattern_set",
        json!({ "clip": "Beat", "pattern": "bd" }),
    )
    .await;
    assert!(is_error && text.contains("steps"), "{text}");

    ok(&client, "set_loop", json!({ "start_bar": 1, "end_bar": 5 })).await;
    let seam = ok(&client, "loop_check_seam", json!({})).await;
    assert_eq!(seam["seam"]["verdict"], "seamless", "{seam}");

    let acid = ok(
        &client,
        "project_new",
        json!({ "template": "acid", "name": "Acid club", "discard_unsaved": true }),
    )
    .await;
    assert_eq!(acid["tracks"].as_array().unwrap().len(), 2);
    client.cancel().await.unwrap();
}

async fn http_status(addr: std::net::SocketAddr, token: Option<&str>) -> String {
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#;
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let req = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        addr.port(),
        body.len()
    );
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = vec![0u8; 512];
    let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .unwrap()
        .unwrap();
    String::from_utf8_lossy(&buf[..n])
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn http_needs_the_token() {
    let (session, _) = Session::new(empty_project("Http"), None);
    let make = move || SkywareMcp::new(session.clone(), Policy::new(Permission::Edit, vec![]));
    let server = sky_mcp::serve_http(make, "127.0.0.1:0".parse().unwrap(), "secret-token".into())
        .await
        .unwrap();
    assert!(http_status(server.addr, None).await.contains("401"));
    assert!(
        http_status(server.addr, Some("wrong"))
            .await
            .contains("401")
    );
    let ok = http_status(server.addr, Some("secret-token")).await;
    assert!(ok.contains("200"), "{ok}");
    server.shutdown();
}

/// A pack folder with a `Kit` bank (bd with two variations, sd) and a
/// pitched `keys` sound.
fn write_pack(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).unwrap();
    for (name, level) in [("bd0", 0.5), ("bd1", 0.4), ("sd", 0.3), ("c4", 0.2)] {
        let audio = sky_dsp::AudioData {
            sample_rate: 48_000,
            channels: vec![vec![level; 4_800]],
        };
        sky_io::write_wav(
            &dir.join(format!("{name}.wav")),
            &audio,
            sky_io::WavFormat::Pcm16,
            false,
        )
        .unwrap();
    }
    std::fs::write(
        dir.join("strudel.json"),
        r#"{"Kit_bd": ["bd0.wav", "bd1.wav"], "Kit_sd": "sd.wav", "keys": {"C4": "c4.wav"}}"#,
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_assistant_loads_a_strudel_bank_and_exports_with_its_credits() {
    let tmp = tempfile::tempdir().unwrap();
    write_pack(&tmp.path().join("packs/kit"));
    let (session, _) = Session::new(empty_project("Banks"), None);
    session.set_library(sky_library::Library::new(tmp.path().join("cache")));
    let server = SkywareMcp::new(
        session.clone(),
        Policy::new(Permission::Files, vec![tmp.path().to_path_buf()]),
    );
    let client = connect(server).await;

    let packs = ok(&client, "sound_packs", json!({})).await;
    let ids: Vec<&str> = packs["packs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["pack"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"tidal-drum-machines") && ids.contains(&"vcsl"));

    let kit = ok(&client, "sound_packs", json!({ "pack": "packs/kit" })).await;
    assert_eq!(kit["banks"], json!({ "Kit": 2 }), "{kit}");
    assert_eq!(kit["sounds"], json!({ "keys": 1 }), "{kit}");
    assert_eq!(kit["license"], "unknown");
    let bank = ok(
        &client,
        "sound_packs",
        json!({ "pack": "packs/kit", "bank": "Kit" }),
    )
    .await;
    assert_eq!(bank["sounds"], json!({ "bd": 2, "sd": 1 }));

    ok(
        &client,
        "track_create",
        json!({ "name": "Beat", "instrument": "bank" }),
    )
    .await;
    let loaded = ok(
        &client,
        "bank_load",
        json!({ "track": "Beat", "pack": "packs/kit", "bank": "Kit", "license": "CC-BY-4.0" }),
    )
    .await;
    assert_eq!(loaded["sounds"], json!({ "bd": 2, "sd": 1 }), "{loaded}");
    assert_eq!(loaded["newFiles"], 3);
    assert_eq!(loaded["pattern"], "bd sd");
    let more = ok(
        &client,
        "bank_load",
        json!({ "track": "Beat", "pack": "packs/kit", "sounds": ["keys"], "add": true,
                "license": "CC-BY-4.0" }),
    )
    .await;
    assert_eq!(more["sounds"].as_object().unwrap().len(), 3);
    ok(
        &client,
        "instrument_set",
        json!({ "track": "Beat", "pitched": "keys" }),
    )
    .await;
    let track = ok(&client, "track_get", json!({ "track": "Beat" })).await;
    assert_eq!(track["instrument"]["bank"]["pitched"], "keys", "{track}");

    ok(
        &client,
        "clip_create",
        json!({ "track": "Beat", "start_bar": 1, "length_bars": 2, "pattern": "bd sd:1 bd:1 c4" }),
    )
    .await;
    ok(&client, "set_loop", json!({ "start_bar": 1, "end_bar": 3 })).await;
    let export = ok(
        &client,
        "export_loop",
        json!({ "name": "Beat", "dir": "export" }),
    )
    .await;
    assert_eq!(export["credits"], json!(["kit (CC BY 4.0)"]), "{export}");
    assert_eq!(export["licenseWarnings"], json!([]));
    let peak = export["asset"]["peakDbfs"].as_f64().unwrap();
    assert!(peak > -30.0, "the bank is silent: {peak} dBFS");

    // Known packs keep their license; outside folders stay closed.
    let (text, is_error) = call(
        &client,
        "bank_load",
        json!({ "track": "Beat", "pack": "vcsl", "license": "unknown" }),
    )
    .await;
    assert!(is_error && text.contains("cannot be changed"), "{text}");
    let (text, is_error) = call(
        &client,
        "bank_load",
        json!({ "track": "Beat", "pack": "/elsewhere/kit" }),
    )
    .await;
    assert!(
        is_error && text.contains("outside the allowed folders"),
        "{text}"
    );

    // Each load is one undo step: undoing the loop, the clip, the pitched
    // sound and the second load leaves the first load's three files.
    ok(&client, "undo", json!({ "steps": 4 })).await;
    let summary = ok(&client, "project_summary", json!({})).await;
    assert!(summary["tracks"][0]["instrument"]["bank"].is_object());
    assert_eq!(summary["audioPool"].as_array().unwrap().len(), 3);
}

/// A pack folder with plain names, as Dirt-Samples has: bd, sn and hh.
fn write_plain_pack(dir: &std::path::Path) {
    write_pack(dir);
    std::fs::write(
        dir.join("strudel.json"),
        r#"{"bd": ["bd0.wav", "bd1.wav"], "sn": "sd.wav", "hh": "c4.wav"}"#,
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_assistant_links_a_pack_with_a_samples_line() {
    let tmp = tempfile::tempdir().unwrap();
    write_plain_pack(&tmp.path().join("packs/dirt"));
    let (session, _) = Session::new(empty_project("Strudel"), None);
    session.set_library(sky_library::Library::new(tmp.path().join("cache")));
    let client = connect(SkywareMcp::new(
        session.clone(),
        Policy::new(Permission::Files, vec![tmp.path().to_path_buf()]),
    ))
    .await;
    ok(
        &client,
        "track_create",
        json!({ "name": "Beat", "instrument": "synth" }),
    )
    .await;
    ok(
        &client,
        "clip_create",
        json!({ "track": "Beat", "start_bar": 1, "length_bars": 1, "pattern": "c4" }),
    )
    .await;

    // Strudel's line links the pack; the sounds the pattern names load,
    // and the line is not kept.
    let set = ok(
        &client,
        "pattern_set",
        json!({ "clip": "Pattern", "pattern": "samples('packs/dirt')\nbd*2 sd" }),
    )
    .await;
    assert_eq!(set["pattern"], "bd*2 sd", "{set}");
    assert_eq!(set["sounds"]["linked"], json!(["dirt"]), "{set}");
    assert_eq!(
        set["sounds"]["loaded"],
        json!([["bd", "dirt"], ["sn", "dirt"]])
    );
    assert!(set["licenseNote"].is_string());
    let track = ok(&client, "track_get", json!({ "track": "Beat" })).await;
    let bank = &track["instrument"]["bank"];
    assert_eq!(bank["linked"][0]["pack"], "dirt", "{track}");
    assert_eq!(bank["linked"][0]["license"], "unknown");

    // A name the bank lacks loads from the linked pack later.
    let more = ok(
        &client,
        "pattern_set",
        json!({ "clip": "Pattern", "pattern": "bd hh zz" }),
    )
    .await;
    assert_eq!(more["sounds"]["loaded"], json!([["hh", "dirt"]]), "{more}");
    assert_eq!(more["sounds"]["missing"], json!(["zz"]));

    // The line has its place in pattern_set, and pattern_query reads past it.
    let (text, is_error) = call(
        &client,
        "clip_create",
        json!({ "track": "Beat", "start_bar": 2, "length_bars": 1, "pattern": "samples('packs/dirt')\nbd" }),
    )
    .await;
    assert!(is_error && text.contains("pattern_set"), "{text}");
    let query = ok(
        &client,
        "pattern_query",
        json!({ "pattern": "samples('github:a/b')\nbd sd" }),
    )
    .await;
    assert_eq!(query["events"].as_array().map(Vec::len), Some(2), "{query}");

    // Without the files permission nothing downloads.
    let edit = connect(SkywareMcp::new(
        session.clone(),
        Policy::new(Permission::Edit, vec![]),
    ))
    .await;
    let (text, is_error) = call(
        &edit,
        "pattern_set",
        json!({ "clip": "Pattern", "pattern": "samples('github:a/b')\nbd" }),
    )
    .await;
    assert!(is_error && text.contains("files permission"), "{text}");
    let plain = ok(
        &edit,
        "pattern_set",
        json!({ "clip": "Pattern", "pattern": "bd cp" }),
    )
    .await;
    assert!(
        plain["note"].as_str().is_some_and(|n| n.contains("cp")),
        "{plain}"
    );
    client.cancel().await.unwrap();
    edit.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_assistant_sets_up_game_assets_and_exports_them_for_the_engine() {
    let tmp = tempfile::tempdir().unwrap();
    let (session, _) = Session::new(sky_model::templates::demo_project(), None);
    let client = connect(SkywareMcp::new(
        session.clone(),
        Policy::new(Permission::Files, vec![tmp.path().to_path_buf()]),
    ))
    .await;
    ok(&client, "project_save", json!({ "path": "Game" })).await;

    // A loop with a one-bar intro, and a set of hits, one per beat.
    let battle = ok(
        &client,
        "game_asset_set",
        json!({
            "name": "battle", "kind": "loop", "start_bar": 2, "end_bar": 4,
            "intro_start_bar": 1, "tags": ["combat"]
        }),
    )
    .await;
    assert_eq!(battle["asset"]["introStartBar"], 1.0, "{battle}");
    assert_eq!(battle["asset"]["category"], "music");
    let hit = ok(
        &client,
        "game_asset_set",
        json!({
            "name": "hit", "kind": "sfx", "start_bar": 1, "end_bar": 2, "count": 4,
            "selection": "round_robin", "pitch_semitones": { "min": 2, "max": -2 },
            "tail_seconds": 0.25
        }),
    )
    .await;
    assert_eq!(hit["asset"]["variations"].as_array().unwrap().len(), 4);
    assert_eq!(hit["asset"]["selection"], "roundRobin");
    assert_eq!(hit["asset"]["pitchSemitones"], json!([-2.0, 2.0]));
    // Only what changes: split the same bar in two.
    let hit = ok(
        &client,
        "game_asset_set",
        json!({ "name": "hit", "count": 2 }),
    )
    .await;
    let variations = hit["asset"]["variations"].as_array().unwrap();
    assert_eq!(variations.len(), 2);
    assert_eq!(variations[1]["endBar"], 2.0);
    assert_eq!(hit["asset"]["selection"], "roundRobin");

    for (args, says) in [
        (json!({ "name": "hit", "intro_start_bar": 1 }), "for loops"),
        (json!({ "name": "battle", "count": 2 }), "for SFX sets"),
        (json!({ "name": "nope" }), "battle"),
        (
            json!({ "name": "Bad Name", "kind": "loop", "start_bar": 1, "end_bar": 2 }),
            "Bad Name",
        ),
        (
            json!({ "name": "hit", "variations": [{ "start_bar": 1, "end_bar": 1.5 }],
                    "start_bar": 1, "end_bar": 2 }),
            "not both",
        ),
    ] {
        let (text, is_error) = call(&client, "game_asset_set", args).await;
        assert!(is_error, "{text}");
        assert!(text.contains(says), "{text}");
    }

    // The game's folder sits next to the project folder.
    let settings = ok(
        &client,
        "game_export_settings",
        json!({ "dir": "../GameAudio", "bits": 16 }),
    )
    .await;
    assert_eq!(settings["exportDir"], "../GameAudio");
    assert_eq!(settings["bits"], 16);
    let (text, is_error) = call(
        &client,
        "game_export_settings",
        json!({ "dir": "../../outside" }),
    )
    .await;
    assert!(
        is_error && text.contains("outside the allowed folders"),
        "{text}"
    );

    let out = ok(&client, "game_export", json!({})).await;
    assert_eq!(out["verified"]["passed"], true, "{out}");
    assert_eq!(out["assets"].as_array().unwrap().len(), 4);
    let audio = tmp.path().join("GameAudio");
    for file in [
        "music/battle.wav",
        "sfx/hit_01.wav",
        "sfx/hit_02.wav",
        "sfx/demo_hits_04.wav",
    ] {
        assert!(audio.join(file).exists(), "{file}");
    }
    let wav = sky_io::read_audio(&audio.join("sfx/hit_01.wav")).unwrap();
    assert_eq!(wav.sample_rate, 48_000);

    // The engine side: sky-runtime plays what was exported, no edits.
    let mut rt = sky_runtime::Runtime::new(48_000);
    assert_eq!(rt.load_manifest(&audio).unwrap().len(), 4);
    let Some(sky_core::manifest::Asset::Loop(battle)) =
        rt.asset("battle").and_then(|h| rt.asset_info(h)).cloned()
    else {
        panic!("battle is a loop");
    };
    // One bar of intro at 120 BPM, then two bars of body.
    assert_eq!(battle.loop_start, 96_000);
    assert_eq!(battle.loop_end, 96_000 * 3);
    let music = rt
        .play_by_name("battle", sky_runtime::PlayParams::default())
        .unwrap();
    rt.play_by_name("hit", sky_runtime::PlayParams::default())
        .unwrap();
    let mut buf = vec![0.0f32; 2 * 48_000];
    rt.render(&mut buf);
    assert!(rt.is_playing(music));
    assert!(buf.iter().any(|s| s.abs() > 0.01));

    // Dropping an asset removes it and its files on the next export.
    ok(&client, "game_asset_remove", json!({ "name": "demo_hits" })).await;
    let out = ok(&client, "game_export", json!({ "verify": false })).await;
    assert_eq!(out["removed"], json!(["demo_hits"]));
    assert!(out.get("verified").is_none());
    assert!(!audio.join("sfx/demo_hits_01.wav").exists());
    assert!(audio.join("sfx/hit_01.wav").exists());
    let summary = ok(&client, "project_summary", json!({})).await;
    assert_eq!(summary["game"]["assets"].as_array().unwrap().len(), 3);
    client.cancel().await.unwrap();
}
