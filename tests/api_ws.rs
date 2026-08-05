//! Black-box tests for the `GET /api/session` WebSocket channel and the
//! server-authoritative phase-approval gate (Task 2.2).

mod common;

use std::net::TcpStream;
use std::time::Duration;

use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

const READ_TIMEOUT: Duration = Duration::from_secs(10);

const FAKE_CLAUDE_SCRIPT: &str = r#"#!/bin/sh
echo '{"type":"assistant","session_id":"fake-1","message":{"content":[{"type":"text","text":"hello from fake claude"}]}}'
echo '{"type":"result","session_id":"fake-1","is_error":false,"result":"hello from fake claude"}'
"#;

/// Spawn the server with a fake `claude` executable on `PATH` (ahead of the
/// inherited `PATH`) so `AppCore::submit_prompt` can drive a full turn
/// without a real Claude CLI or network access. The returned `TempDir` must
/// stay alive for as long as the server might invoke `claude`.
fn spawn_with_fake_claude() -> (common::Server, tempfile::TempDir) {
    let bin_dir = tempfile::TempDir::new().expect("tempdir");
    let claude_path = bin_dir.path().join("claude");
    std::fs::write(&claude_path, FAKE_CLAUDE_SCRIPT).expect("write fake claude");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&claude_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&claude_path, perms).unwrap();
    }

    let inherited = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{}", bin_dir.path().display(), inherited);
    let server = common::spawn_with_env(&[("PATH", &path)]);
    (server, bin_dir)
}

fn create_project(base: &str, name: &str) -> String {
    let resp = ureq::post(&format!("{base}/api/projects")).send_json(json!({"name": name}));
    let mut resp = resp.expect("create project should succeed");
    let body: Value = resp.body_mut().read_json().unwrap();
    body["id"].as_str().unwrap().to_string()
}

fn connect(server: &common::Server, project_id: &str) -> WebSocket<MaybeTlsStream<TcpStream>> {
    let url = server.ws_url(&format!("/api/session?project={project_id}"));
    let (ws, _resp) = tungstenite::connect(&url).expect("ws connect should succeed");
    if let MaybeTlsStream::Plain(stream) = ws.get_ref() {
        stream.set_read_timeout(Some(READ_TIMEOUT)).expect("set read timeout");
    }
    ws
}

fn send_json(ws: &mut WebSocket<MaybeTlsStream<TcpStream>>, v: Value) {
    ws.send(Message::Text(v.to_string().into())).expect("send should succeed");
}

/// Read the next text frame and parse it as JSON. Panics (rather than
/// hanging) if no frame arrives within `READ_TIMEOUT`, thanks to the read
/// timeout set on the underlying `TcpStream` in `connect`.
fn read_json(ws: &mut WebSocket<MaybeTlsStream<TcpStream>>) -> Value {
    loop {
        match ws.read().expect("ws read should not time out or error") {
            Message::Text(text) => return serde_json::from_str(text.as_str()).expect("valid JSON"),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected non-text message: {other:?}"),
        }
    }
}

#[test]
fn connect_sends_initial_snapshot_with_spec_unapproved() {
    let server = common::spawn();
    let id = create_project(&server.base, "widget");

    let mut ws = connect(&server, &id);
    let snapshot = read_json(&mut ws);

    assert_eq!(snapshot["type"], "snapshot");
    assert_eq!(snapshot["phase"], "Spec");
    assert_eq!(snapshot["approved"], false);
}

#[test]
fn advance_without_approval_is_denied_and_state_is_unchanged() {
    let server = common::spawn();
    let id = create_project(&server.base, "widget");

    let mut ws = connect(&server, &id);
    let _snapshot = read_json(&mut ws);

    send_json(&mut ws, json!({"type": "advance"}));
    let reply = read_json(&mut ws);
    assert_eq!(reply, json!({"type": "error", "message": "phase not approved"}));

    // A fresh connection must still see the unapproved Spec phase — nothing
    // was mutated by the denied advance.
    let mut ws2 = connect(&server, &id);
    let snapshot2 = read_json(&mut ws2);
    assert_eq!(snapshot2["phase"], "Spec");
    assert_eq!(snapshot2["approved"], false);
}

#[test]
fn approve_then_advance_moves_to_build_and_resets_approval() {
    let server = common::spawn();
    let id = create_project(&server.base, "widget");

    let mut ws = connect(&server, &id);
    let _snapshot = read_json(&mut ws);

    send_json(&mut ws, json!({"type": "approve_phase"}));
    let phase_state = read_json(&mut ws);
    assert_eq!(phase_state, json!({"type": "phase_state", "phase": "Spec", "approved": true}));

    send_json(&mut ws, json!({"type": "advance"}));
    let phase_state = read_json(&mut ws);
    assert_eq!(phase_state["type"], "phase_state");
    assert_eq!(phase_state["phase"], "Build");
    let snapshot = read_json(&mut ws);
    assert_eq!(snapshot["type"], "snapshot");
    assert_eq!(snapshot["phase"], "Build");

    // A fresh connection reports Build, freshly unapproved.
    let mut ws2 = connect(&server, &id);
    let snapshot2 = read_json(&mut ws2);
    assert_eq!(snapshot2["phase"], "Build");
    assert_eq!(snapshot2["approved"], false);
}

#[test]
fn prompt_streams_deltas_from_fake_claude_and_finalizes_into_snapshot() {
    let (server, _bin_dir) = spawn_with_fake_claude();
    let id = create_project(&server.base, "widget");

    let mut ws = connect(&server, &id);
    let _snapshot = read_json(&mut ws);

    send_json(&mut ws, json!({
        "type": "prompt",
        "text": "hello",
        "part_refs": [],
        "lib_refs": [],
    }));

    // Drain frames until we see the streamed delta, then until the
    // finalizing snapshot whose conversation contains the full text.
    let mut saw_delta = false;
    loop {
        let msg = read_json(&mut ws);
        match msg["type"].as_str() {
            Some("stream_delta") => {
                if msg["text"].as_str().unwrap_or("").contains("hello from fake claude") {
                    saw_delta = true;
                }
            }
            Some("snapshot") => {
                assert!(saw_delta, "snapshot arrived before any stream_delta");
                let conversation = msg["conversation"].as_array().expect("conversation array");
                let found = conversation.iter().any(|entry| {
                    entry["content"].as_str().unwrap_or("").contains("hello from fake claude")
                });
                assert!(found, "conversation should contain the assistant's reply: {conversation:?}");
                break;
            }
            Some("tool_call") => {} // fake claude emits none, but tolerate it
            other => panic!("unexpected message type: {other:?}"),
        }
    }
}
