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
        json!({ "track": "Bass", "volume_db": -4, "pan": -0.2 }),
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
