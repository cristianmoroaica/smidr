//! Black-box tests for local-model engines: `GET /api/engines`, the WS
//! `set_engine` message + `snapshot.engine` field, soft claude gating, and
//! a full-turn integration through a fake OpenAI-compatible endpoint driving
//! the real `mcp/server.py` in the spec phase.

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

const READ_TIMEOUT: Duration = Duration::from_secs(10);

// ---- shared small helpers (mirrors tests/api_ws.rs's style) --------------

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

fn read_json(ws: &mut WebSocket<MaybeTlsStream<TcpStream>>) -> Value {
    loop {
        match ws.read().expect("ws read should not time out or error") {
            Message::Text(text) => return serde_json::from_str(text.as_str()).expect("valid JSON"),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected non-text message: {other:?}"),
        }
    }
}

/// Write `engines.toml` into a fresh tempdir at `<tmp>/smidr/engines.toml`
/// and return the tempdir (kept alive by the caller) plus the
/// `XDG_CONFIG_HOME` value to pass to `spawn_with_env`.
fn write_engines_toml(contents: &str) -> (tempfile::TempDir, String) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let smidr_dir = tmp.path().join("smidr");
    std::fs::create_dir_all(&smidr_dir).unwrap();
    std::fs::write(smidr_dir.join("engines.toml"), contents).unwrap();
    let xdg = tmp.path().to_string_lossy().to_string();
    (tmp, xdg)
}

// ---- GET /api/engines -----------------------------------------------------

#[test]
fn engines_with_no_config_lists_only_claude() {
    // Neutralize XDG_CONFIG_HOME: left unset, `engine_config::engines_toml_path`
    // would prefer the developer's real `$XDG_CONFIG_HOME/smidr/engines.toml`
    // (or `~/.config/smidr/engines.toml` via `common::spawn`'s HOME override,
    // but XDG_CONFIG_HOME wins when set) over the sandboxed HOME, reading
    // real config and firing live discovery requests at real endpoints.
    let empty_config = tempfile::TempDir::new().expect("tempdir");
    let server = common::spawn_with_env(&[(
        "XDG_CONFIG_HOME",
        &empty_config.path().to_string_lossy(),
    )]);
    let mut resp = ureq::get(format!("{}/api/engines", server.base)).call().expect("request");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.body_mut().read_json().unwrap();
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], "claude");
    assert_eq!(arr[0]["name"], "Claude CLI");
}

/// A minimal `GET /api/tags`-only fake Ollama server: replies with a fixed
/// JSON body to every accepted connection, forever, until the listener is
/// dropped.
fn spawn_fake_ollama_tags(models: &[&str]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let models_json: Vec<Value> = models.iter().map(|m| json!({"name": m})).collect();
    let body = json!({"models": models_json}).to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            drain_request(&mut stream);
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });
    format!("http://{addr}")
}

fn drain_request(stream: &mut TcpStream) {
    let mut buf = [0u8; 4096];
    let mut acc: Vec<u8> = Vec::new();
    loop {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        acc.extend_from_slice(&buf[..n]);
        if acc.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
}

#[test]
fn engines_discovers_ollama_models_and_nulls_on_dead_endpoint() {
    let ollama_base = spawn_fake_ollama_tags(&["gpt-oss:120b", "llama3:8b"]);
    // A bound-then-dropped listener frees the port with nobody listening,
    // guaranteeing connection refused rather than a flaky "might still be
    // in TIME_WAIT" port reuse.
    let dead_listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let dead_addr = dead_listener.local_addr().expect("addr");
    drop(dead_listener);

    let toml = format!(
        "[[endpoint]]\nname = \"ollama\"\ntype = \"ollama\"\nbase_url = \"{ollama_base}/v1\"\n\n\
         [[endpoint]]\nname = \"dead\"\ntype = \"ollama\"\nbase_url = \"http://{dead_addr}/v1\"\n"
    );
    let (_tmp, xdg) = write_engines_toml(&toml);
    let server = common::spawn_with_env(&[("XDG_CONFIG_HOME", &xdg)]);

    let mut resp = ureq::get(format!("{}/api/engines", server.base)).call().expect("request");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.body_mut().read_json().unwrap();
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0]["id"], "claude");

    let ollama = arr.iter().find(|e| e["id"] == "ollama").expect("ollama entry");
    assert_eq!(
        ollama["models"],
        json!(["gpt-oss:120b", "llama3:8b"]),
        "ollama entry: {ollama}"
    );

    let dead = arr.iter().find(|e| e["id"] == "dead").expect("dead entry");
    assert_eq!(dead["models"], Value::Null, "dead entry: {dead}");
}

// ---- set_engine over WS ----------------------------------------------------

#[test]
fn set_engine_round_trips_persists_and_validates() {
    let toml = "[[endpoint]]\nname = \"ollama\"\ntype = \"ollama\"\nbase_url = \"http://127.0.0.1:1/v1\"\n";
    let (_tmp, xdg) = write_engines_toml(toml);
    let server = common::spawn_with_env(&[("XDG_CONFIG_HOME", &xdg)]);
    let id = create_project(&server.base, "engine-proj");

    let mut ws = connect(&server, &id);
    let snapshot = read_json(&mut ws);
    assert_eq!(snapshot["engine"], "claude");

    send_json(&mut ws, json!({"type": "set_engine", "engine": "ollama:gpt-oss:120b"}));
    let reply = read_json(&mut ws);
    assert_eq!(reply["type"], "snapshot");
    assert_eq!(reply["engine"], "ollama:gpt-oss:120b");

    // Persisted to project.json on disk under the sandbox HOME.
    let project_json_path = server.home.path().join("Smidr").join(&id).join("project.json");
    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(&project_json_path).expect("read project.json"))
            .expect("valid json");
    assert_eq!(on_disk["engine"], "ollama:gpt-oss:120b");

    // Opening a project must APPLY the stored engine without rewriting
    // project.json — a rewrite would both be pointless and, via
    // `set_project_engine`'s fresh-meta fallback on an unreadable file,
    // destroy recoverable fields. An unknown key is the tripwire: serde
    // ignores it on read, so it only survives if nothing rewrote the file.
    let mut marked = on_disk.clone();
    marked["_test_marker"] = json!("keep-me");
    std::fs::write(&project_json_path, serde_json::to_string_pretty(&marked).unwrap()).unwrap();

    // Reconnecting yields the same snapshot value.
    let mut ws2 = connect(&server, &id);
    let snapshot2 = read_json(&mut ws2);
    assert_eq!(snapshot2["engine"], "ollama:gpt-oss:120b");

    // Unknown endpoint -> error, engine unchanged.
    send_json(&mut ws2, json!({"type": "set_engine", "engine": "nope:x"}));
    let err = read_json(&mut ws2);
    assert_eq!(err["type"], "error");

    let mut ws3 = connect(&server, &id);
    let snapshot3 = read_json(&mut ws3);
    assert_eq!(snapshot3["engine"], "ollama:gpt-oss:120b", "unknown endpoint must not change the engine");

    let after_opens: Value =
        serde_json::from_str(&std::fs::read_to_string(&project_json_path).expect("read project.json"))
            .expect("valid json");
    assert_eq!(
        after_opens["_test_marker"], "keep-me",
        "opening a project (twice) must not rewrite project.json: {after_opens}"
    );

    // "claude" resets.
    send_json(&mut ws3, json!({"type": "set_engine", "engine": "claude"}));
    let reply = read_json(&mut ws3);
    assert_eq!(reply["engine"], "claude");
}

/// Accept exactly one connection, wait `delay`, then reply with `body` as a
/// `text/event-stream`. The delay is what keeps a turn "in flight" long
/// enough for the test to interleave a `set_engine` message.
fn spawn_slow_fake_openai_endpoint(body: String, delay: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else { return };
        drain_request(&mut stream);
        std::thread::sleep(delay);
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
    });
    format!("http://{addr}/v1")
}

#[test]
fn set_engine_during_an_in_flight_turn_is_accepted_and_does_not_cancel_it() {
    // The spec pins: "a set_engine while a turn is in flight is accepted and
    // takes effect on the NEXT prompt (do not cancel anything)". The in-flight
    // worker owns a CLONE of its endpoint/model, so switching the engine
    // mid-turn cannot retarget or abort it — this test locks that in against a
    // future refactor to a shared/borrowed engine handle.
    let slow_base = spawn_slow_fake_openai_endpoint(
        sse_body(&[r#"{"choices":[{"delta":{"content":"from the slow engine"},"finish_reason":"stop"}]}"#]),
        Duration::from_millis(1500),
    );
    let toml = format!(
        "[[endpoint]]\nname = \"slow\"\ntype = \"openai\"\nbase_url = \"{slow_base}\"\n\n\
         [[endpoint]]\nname = \"other\"\ntype = \"openai\"\nbase_url = \"http://127.0.0.1:1/v1\"\n"
    );
    let (_tmp, xdg) = write_engines_toml(&toml);
    let server = common::spawn_with_env(&[("XDG_CONFIG_HOME", &xdg)]);
    let id = create_project(&server.base, "inflight-proj");

    let mut ws = connect(&server, &id);
    let _snapshot = read_json(&mut ws);

    send_json(&mut ws, json!({"type": "set_engine", "engine": "slow:m1"}));
    assert_eq!(read_json(&mut ws)["engine"], "slow:m1");

    send_json(&mut ws, json!({
        "type": "prompt",
        "text": "spec it",
        "part_refs": [],
        "lib_refs": [],
    }));
    // Switch engines while the slow turn is still waiting on its response.
    send_json(&mut ws, json!({"type": "set_engine", "engine": "other:m2"}));

    let mut switched = false;
    let mut saw_slow_delta = false;
    let mut frames = 0;
    const MAX_FRAMES: u32 = 400;
    while !(switched && saw_slow_delta) {
        frames += 1;
        assert!(
            frames <= MAX_FRAMES,
            "in-flight set_engine never settled (switched={switched} slow_delta={saw_slow_delta})"
        );
        let msg = read_json(&mut ws);
        match msg["type"].as_str() {
            // The set_engine ack: accepted immediately, mid-turn.
            Some("snapshot") if msg["engine"] == "other:m2" => switched = true,
            Some("stream_delta") => {
                if msg["text"].as_str().unwrap_or_default().contains("from the slow engine") {
                    // The in-flight turn ran to completion against the engine
                    // it started with — not cancelled, not retargeted.
                    saw_slow_delta = true;
                }
            }
            Some("error") => panic!("unexpected error during the in-flight turn: {msg}"),
            _ => {}
        }
    }

    // And the switch really took effect for the NEXT turn: reconnecting shows
    // the new engine, and it is the one persisted.
    let mut ws2 = connect(&server, &id);
    assert_eq!(read_json(&mut ws2)["engine"], "other:m2");
}

// ---- startup without claude -----------------------------------------------

#[test]
fn startup_without_claude_still_serves_ui_and_reports_unavailable() {
    // A tempdir on PATH containing no `claude` binary: `check_claude()` must
    // fail, but startup must not abort. Also neutralize XDG_CONFIG_HOME (see
    // `engines_with_no_config_lists_only_claude`) so `/api/engines` reads the
    // sandbox, not the developer's real engines.toml.
    let empty_bin = tempfile::TempDir::new().expect("tempdir");
    let empty_config = tempfile::TempDir::new().expect("tempdir");
    let server = common::spawn_with_env(&[
        ("PATH", &empty_bin.path().to_string_lossy()),
        ("XDG_CONFIG_HOME", &empty_config.path().to_string_lossy()),
    ]);

    let resp = ureq::get(&server.base).call().expect("GET / should succeed");
    assert_eq!(resp.status(), 200);

    let mut resp = ureq::get(format!("{}/api/engines", server.base)).call().expect("request");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.body_mut().read_json().unwrap();
    let arr = body.as_array().expect("array");
    assert_eq!(arr[0]["id"], "claude");
    assert_eq!(arr[0]["available"], false);
}

// ---- full-turn integration: fake OpenAI endpoint + real mcp/server.py -----

fn cadquery_python_available() -> bool {
    Path::new("./.venv-cadquery/bin/python3").exists()
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn sse_body(events: &[&str]) -> String {
    let mut s = String::new();
    for e in events {
        s.push_str("data: ");
        s.push_str(e);
        s.push_str("\n\n");
    }
    s.push_str("data: [DONE]\n\n");
    s
}

/// Serve `bodies[i]` (200 OK, `text/event-stream`) to the i-th accepted
/// connection in order, then stop accepting. Mirrors the pattern in
/// `src/openai_engine.rs`'s own unit tests. Also captures each request's
/// full JSON body and sends it on the returned channel (in request order),
/// so callers can assert on what the agent loop actually sent upstream —
/// e.g. that a `role:"tool"` message carries the REAL MCP server's
/// `ask_question` result rather than a swallowed-client-failure stand-in.
fn spawn_fake_openai_endpoint(bodies: Vec<String>) -> (String, std::sync::mpsc::Receiver<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    std::thread::spawn(move || {
        for body in bodies {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let mut acc: Vec<u8> = Vec::new();
            let mut content_length: usize = 0;
            let mut header_end = 0;
            loop {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                acc.extend_from_slice(&buf[..n]);
                if let Some(pos) = find_double_crlf(&acc) {
                    let head = String::from_utf8_lossy(&acc[..pos]);
                    for line in head.lines() {
                        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                            content_length = v.trim().parse().unwrap_or(0);
                        }
                    }
                    header_end = pos + 4;
                    let already = acc.len() - header_end;
                    let mut remaining = content_length.saturating_sub(already);
                    while remaining > 0 {
                        let n = stream.read(&mut buf).unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        acc.extend_from_slice(&buf[..n]);
                        remaining = remaining.saturating_sub(n);
                    }
                    break;
                }
            }
            let req_body = &acc[header_end..header_end + content_length.min(acc.len().saturating_sub(header_end))];
            if let Ok(v) = serde_json::from_slice::<Value>(req_body) {
                let _ = tx.send(v);
            }
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });
    (format!("http://{addr}/v1"), rx)
}

#[test]
fn full_turn_through_fake_openai_endpoint_and_real_mcp_server_asks_a_question() {
    if !cadquery_python_available() {
        eprintln!("skipping full_turn_through_fake_openai_endpoint_and_real_mcp_server_asks_a_question: ./.venv-cadquery/bin/python3 not present");
        return;
    }

    let round1 = sse_body(&[
        r#"{"choices":[{"delta":{"content":"Let me ask about width. "}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"ask_question","arguments":""}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"question\":\"How wide?\",\"options\":[\"20mm\",\"40mm\"]}"}}]},"finish_reason":"tool_calls"}]}"#,
    ]);
    let round2 = sse_body(&[
        r#"{"choices":[{"delta":{"content":"Got it, 40mm."},"finish_reason":"stop"}]}"#,
    ]);
    let (base_url, req_bodies) = spawn_fake_openai_endpoint(vec![round1, round2]);

    let toml = format!(
        "[[endpoint]]\nname = \"fake\"\ntype = \"openai\"\nbase_url = \"{base_url}\"\n"
    );
    let (_tmp, xdg) = write_engines_toml(&toml);
    let server = common::spawn_with_env(&[("XDG_CONFIG_HOME", &xdg)]);
    let id = create_project(&server.base, "fake-engine-proj");

    let mut ws = connect(&server, &id);
    let _snapshot = read_json(&mut ws);

    send_json(&mut ws, json!({"type": "set_engine", "engine": "fake:test-model"}));
    let reply = read_json(&mut ws);
    assert_eq!(reply["engine"], "fake:test-model");

    send_json(&mut ws, json!({
        "type": "prompt",
        "text": "spec it",
        "part_refs": [],
        "lib_refs": [],
    }));

    let mut saw_delta = false;
    let mut saw_tool_call = false;
    let mut saw_question = false;
    let mut saw_final_delta = false;
    let mut saw_final_snapshot = false;
    let mut frames = 0;
    const MAX_FRAMES: u32 = 400;
    // Drain until the whole turn has completed: `stream_delta` -> `tool_call`
    // -> `question` -> (round 2) another `stream_delta` carrying "Got it,
    // 40mm." -> the finalizing `snapshot`. `tool_call` and `question` land in
    // the same poll batch (the tool call handler queues `question` ahead of
    // the `tool_call` event it's describing — see `AppCore::poll_events`),
    // and round 2's delta can likewise share a batch with the question, so
    // this loop keeps draining past the first `question` frame rather than
    // stopping on it.
    while !(saw_delta && saw_tool_call && saw_question && saw_final_delta && saw_final_snapshot) {
        frames += 1;
        assert!(
            frames <= MAX_FRAMES,
            "did not see the full turn complete within {MAX_FRAMES} frames \
             (delta={saw_delta} tool_call={saw_tool_call} question={saw_question} \
             final_delta={saw_final_delta} final_snapshot={saw_final_snapshot})"
        );
        let msg = read_json(&mut ws);
        match msg["type"].as_str() {
            Some("stream_delta") => {
                saw_delta = true;
                if msg["text"].as_str() == Some("Got it, 40mm.") {
                    saw_final_delta = true;
                }
            }
            Some("tool_call") => saw_tool_call = true,
            Some("question") => {
                assert_eq!(msg["question"], "How wide?");
                assert_eq!(msg["options"], json!(["20mm", "40mm"]));
                saw_question = true;
            }
            Some("snapshot") => {
                assert!(saw_question, "finalizing snapshot arrived before the question message: {msg}");
                if saw_final_delta {
                    saw_final_snapshot = true;
                }
            }
            _ => {}
        }
    }

    // Non-vacuous: the round-2 request to the fake OpenAI endpoint must
    // carry a `role:"tool"` message whose content is the REAL mcp/server.py
    // `ask_question` output, not `openai_engine::run_turn`'s
    // "error: no MCP client available" stand-in for a swallowed
    // `McpClient::start` failure. This is what actually proves the turn
    // drove the real MCP server rather than merely echoing the tool-call
    // arguments straight into the `question` WS message.
    let round1_req = req_bodies.recv_timeout(Duration::from_secs(10)).expect("round 1 request captured");
    let round2_req = req_bodies.recv_timeout(Duration::from_secs(10)).expect("round 2 request captured");

    let round1_messages = round1_req["messages"].as_array().expect("round 1 messages array");
    assert!(
        round1_messages.iter().any(|m| m["role"] == "user" && m["content"].as_str() == Some("spec it")),
        "round 1 should carry the user's prompt: {round1_req}"
    );

    let round2_messages = round2_req["messages"].as_array().expect("round 2 messages array");
    let tool_msg = round2_messages
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap_or_else(|| panic!("round 2 should carry a tool-result message: {round2_req}"));
    let tool_content = tool_msg["content"].as_str().unwrap_or_default();
    assert_ne!(
        tool_content, "error: no MCP client available",
        "tool result must come from the real mcp/server.py, not a swallowed McpClient::start failure"
    );
    assert!(
        tool_content.contains("How wide?"),
        "tool result should echo the real ask_question output: {tool_content}"
    );
}
