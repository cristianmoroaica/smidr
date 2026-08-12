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

/// A fake `claude` script that emits a tool_use, then a `user` event carrying
/// a tool_result whose text contains BUILD_COMPONENT lines for two
/// components (one done, one failed), then the usual result event.
const FAKE_CLAUDE_BUILD_PROGRESS_SCRIPT: &str = r#"#!/bin/sh
printf '%s\n' '{"type":"assistant","session_id":"fake-1","message":{"content":[{"type":"tool_use","name":"mcp__smidr__write_file","input":{"path":"components/lid/code.py"}}]}}'
printf '%s\n' '{"type":"user","session_id":"fake-1","message":{"content":[{"type":"tool_result","content":[{"type":"text","text":"File written: components/lid/code.py\nBuild successful! Dimensions: 1x2x3mm.\nBUILD_COMPONENT: lid done\nBUILD_COMPONENT: base failed"}]}]}}'
printf '%s\n' '{"type":"result","session_id":"fake-1","is_error":false,"result":"build done"}'
"#;

/// A fake `claude` script that emits a tool_use for `mcp__smidr__ask_question`
/// with an `options` array, then the usual result event. Branches on the
/// *prompt text* passed via `-p` (not invocation count / a stateful marker
/// file): the test's initial "spec it" prompt gets the question; the
/// follow-up "40mm" answer prompt gets a plain text reply instead, so the
/// pending question actually resolves. Keying on prompt content — rather
/// than "first call wins" — means an unrelated claude invocation (e.g. a
/// future title-generation call) can't silently steal the one-shot question
/// branch and leave this test failing confusingly.
const FAKE_CLAUDE_ASK_QUESTION_SCRIPT: &str = r#"#!/bin/sh
prompt=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "-p" ]; then
    prompt="$arg"
  fi
  prev="$arg"
done
case "$prompt" in
  *"spec it"*)
    echo '{"type":"assistant","session_id":"fake-1","message":{"content":[{"type":"tool_use","name":"mcp__smidr__ask_question","input":{"question":"How wide should it be?","options":["20mm","40mm","60mm"]}}]}}'
    echo '{"type":"result","session_id":"fake-1","is_error":false,"result":"asked"}'
    ;;
  *)
    echo '{"type":"assistant","session_id":"fake-1","message":{"content":[{"type":"text","text":"got it, 40mm it is"}]}}'
    echo '{"type":"result","session_id":"fake-1","is_error":false,"result":"got it, 40mm it is"}'
    ;;
esac
"#;

/// A fake `claude` script that emits a tool_use for
/// `mcp__smidr__request_phase_change`, then the usual result event. Branches
/// on the *prompt text* passed via `-p`, same as
/// `FAKE_CLAUDE_ASK_QUESTION_SCRIPT`: the test's "make it prettier" prompt
/// gets the phase-change request; any other prompt gets a plain text reply.
const FAKE_CLAUDE_PHASE_SWITCH_SCRIPT: &str = r#"#!/bin/sh
prompt=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "-p" ]; then
    prompt="$arg"
  fi
  prev="$arg"
done
case "$prompt" in
  *"make it prettier"*)
    echo '{"type":"assistant","session_id":"fake-1","message":{"content":[{"type":"tool_use","name":"mcp__smidr__request_phase_change","input":{"target":"build","reason":"that is a functional change"}}]}}'
    echo '{"type":"result","session_id":"fake-1","is_error":false,"result":"requested"}'
    ;;
  *)
    echo '{"type":"assistant","session_id":"fake-1","message":{"content":[{"type":"text","text":"ok"}]}}'
    echo '{"type":"result","session_id":"fake-1","is_error":false,"result":"ok"}'
    ;;
esac
"#;

/// Write the given fake `claude` script into a fresh temp dir and return the
/// `PATH` value (fake dir prepended to the inherited `PATH`) that puts it
/// ahead of any real `claude` on the search path. Shared by every test that
/// needs `AppCore::submit_prompt` to drive a full turn without a real
/// Claude CLI or network access. The returned `TempDir` must stay alive for
/// as long as the server might invoke `claude`.
fn fake_claude_path_with_script(script: &str) -> (tempfile::TempDir, String) {
    let bin_dir = tempfile::TempDir::new().expect("tempdir");
    let claude_path = bin_dir.path().join("claude");
    std::fs::write(&claude_path, script).expect("write fake claude");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&claude_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&claude_path, perms).unwrap();
    }

    let inherited = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{}", bin_dir.path().display(), inherited);
    (bin_dir, path)
}

/// Write the default fake `claude` executable into a fresh temp dir. See
/// `fake_claude_path_with_script` for details.
fn fake_claude_path() -> (tempfile::TempDir, String) {
    fake_claude_path_with_script(FAKE_CLAUDE_SCRIPT)
}

/// Spawn the server with a fake `claude` executable running `script` on
/// `PATH`. See `fake_claude_path_with_script` for details.
fn spawn_with_fake_claude_script(script: &str) -> (common::Server, tempfile::TempDir) {
    let (bin_dir, path) = fake_claude_path_with_script(script);
    let server = common::spawn_with_env(&[("PATH", &path)]);
    (server, bin_dir)
}

/// Spawn the server with the default fake `claude` executable on `PATH`.
fn spawn_with_fake_claude() -> (common::Server, tempfile::TempDir) {
    spawn_with_fake_claude_script(FAKE_CLAUDE_SCRIPT)
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

/// Browsers don't apply CORS to WebSockets — the server must reject upgrade
/// requests whose Origin doesn't match the Host (a hostile webpage opening
/// ws://127.0.0.1:<port>).
#[test]
fn cross_origin_websocket_is_rejected() {
    use tungstenite::client::IntoClientRequest;

    let server = common::spawn();
    let id = create_project(&server.base, "ws-evil-origin");

    let mut req = server
        .ws_url(&format!("/api/session?project={id}"))
        .into_client_request()
        .expect("client request");
    req.headers_mut()
        .insert("Origin", "http://evil.example".parse().unwrap());

    let err = tungstenite::connect(req).expect_err("cross-origin upgrade must fail");
    match err {
        tungstenite::Error::Http(resp) => assert_eq!(resp.status(), 403),
        other => panic!("expected HTTP 403 rejection, got: {other:?}"),
    }
}

/// Same-origin browser connections (Origin matching Host) must still work.
#[test]
fn same_origin_websocket_is_accepted() {
    use tungstenite::client::IntoClientRequest;

    let server = common::spawn();
    let id = create_project(&server.base, "ws-same-origin");

    let host = server.base.strip_prefix("http://").unwrap().to_string();
    let mut req = server
        .ws_url(&format!("/api/session?project={id}"))
        .into_client_request()
        .expect("client request");
    req.headers_mut()
        .insert("Origin", format!("http://{host}").parse().unwrap());

    let (mut ws, _) = tungstenite::connect(req).expect("same-origin upgrade must succeed");
    if let MaybeTlsStream::Plain(stream) = ws.get_ref() {
        stream.set_read_timeout(Some(READ_TIMEOUT)).unwrap();
    }
    let snapshot = read_json(&mut ws);
    assert_eq!(snapshot["type"], "snapshot");
}

/// Unknown project ids must not allocate server state: the connection gets an
/// error message instead of a lazily-created AppCore.
#[test]
fn unknown_project_gets_error_not_a_core() {
    let server = common::spawn();
    let mut ws = connect(&server, "no-such-project");
    let msg = read_json(&mut ws);
    assert_eq!(msg["type"], "error");
    assert!(
        msg["message"].as_str().unwrap_or("").contains("unknown project"),
        "unexpected error payload: {msg}"
    );
}

/// Piped stdin at startup (the `git diff | smidr` briefing flow) must
/// create a project from the briefing content (alongside the default
/// "Untitled" project `AppCore::new` always seeds), and the FIRST WebSocket
/// connection to that briefing project must auto-submit the synthetic
/// "review the briefing" prompt that used to be fired by the (now-deleted)
/// TUI event loop's first tick. A second connection must see the same
/// message exactly once — never re-submitted.
#[test]
fn briefing_stdin_creates_project_and_first_connect_auto_submits() {
    let (bin_dir, path) = fake_claude_path();
    let stdin_text = "User: build me a 40mm bracket.\nAssistant: understood.\n";
    let server = common::spawn_with_env_and_stdin(&[("PATH", &path)], stdin_text);
    let _bin_dir = bin_dir; // keep alive for as long as the server might invoke `claude`

    let resp = ureq::get(&format!("{}/api/projects", server.base)).call();
    let mut resp = resp.expect("list projects should succeed");
    let projects: Value = resp.body_mut().read_json().unwrap();
    let projects = projects.as_array().expect("projects is an array");
    let briefing_projects: Vec<&Value> = projects
        .iter()
        .filter(|p| p["id"].as_str() != Some("Untitled"))
        .collect();
    assert_eq!(
        briefing_projects.len(),
        1,
        "piped stdin must create exactly one non-default project: {projects:?}"
    );
    let project_id = briefing_projects[0]["id"].as_str().expect("project id").to_string();

    const SYNTHETIC_PROMPT: &str =
        "Please review the attached conversation and begin extracting spec fields.";

    let mut ws = connect(&server, &project_id);
    let snapshot = read_json(&mut ws);
    assert_eq!(snapshot["type"], "snapshot");
    assert_eq!(snapshot["phase"], "Spec");
    let conversation = snapshot["conversation"].as_array().expect("conversation array");
    let synthetic_count = conversation
        .iter()
        .filter(|m| m["role"] == "user" && m["content"] == SYNTHETIC_PROMPT)
        .count();
    assert_eq!(
        synthetic_count, 1,
        "first connect must auto-submit the synthetic briefing prompt exactly once: {conversation:?}"
    );

    // A second connection must NOT re-submit — briefing_pending was already
    // cleared by the first connect.
    let mut ws2 = connect(&server, &project_id);
    let snapshot2 = read_json(&mut ws2);
    let conversation2 = snapshot2["conversation"].as_array().expect("conversation array");
    let synthetic_count2 = conversation2
        .iter()
        .filter(|m| m["role"] == "user" && m["content"] == SYNTHETIC_PROMPT)
        .count();
    assert_eq!(
        synthetic_count2, 1,
        "second connect must see the synthetic prompt exactly once, not re-submitted: {conversation2:?}"
    );
}

/// MCP tool output arrives on `user` events as `tool_result` blocks. Each
/// `BUILD_COMPONENT: <name> <status>` line inside that text must become a
/// pinned `{"type":"build_progress",...}` WS message.
#[test]
fn build_progress_lines_become_build_progress_messages() {
    let (server, _bin_dir) = spawn_with_fake_claude_script(FAKE_CLAUDE_BUILD_PROGRESS_SCRIPT);
    let id = create_project(&server.base, "widget");

    let mut ws = connect(&server, &id);
    let _snapshot = read_json(&mut ws);

    send_json(&mut ws, json!({
        "type": "prompt",
        "text": "build it",
        "part_refs": [],
        "lib_refs": [],
    }));

    let mut saw_lid_done = false;
    let mut saw_base_failed = false;
    let mut frames = 0;
    const MAX_FRAMES: u32 = 200;

    while !(saw_lid_done && saw_base_failed) {
        frames += 1;
        assert!(frames <= MAX_FRAMES, "did not see both build_progress messages within {MAX_FRAMES} frames");

        let msg = read_json(&mut ws);
        if msg["type"] == "build_progress" {
            if msg["component"] == "lid" && msg["status"] == "done" {
                saw_lid_done = true;
            }
            if msg["component"] == "base" && msg["status"] == "failed" {
                saw_base_failed = true;
            }
        }
        if msg["type"] == "snapshot" && frames > 1 {
            // Finalizing snapshot arrived; keep looping only if we haven't
            // seen both messages yet (they should have arrived by now).
            if !(saw_lid_done && saw_base_failed) {
                panic!("finalizing snapshot arrived before both build_progress messages: seen lid={saw_lid_done} base={saw_base_failed}");
            }
        }
    }
}

/// A `mcp__smidr__ask_question` tool call with an `options` array must
/// become a pinned `{"type":"question","question":..,"options":[..]}` WS
/// message — arriving before the finalizing snapshot.
#[test]
fn ask_question_tool_call_emits_question_message_with_options() {
    let (server, _bin_dir) = spawn_with_fake_claude_script(FAKE_CLAUDE_ASK_QUESTION_SCRIPT);
    let id = create_project(&server.base, "widget");

    let mut ws = connect(&server, &id);
    let _snapshot = read_json(&mut ws);

    send_json(&mut ws, json!({
        "type": "prompt",
        "text": "spec it",
        "part_refs": [],
        "lib_refs": [],
    }));

    let mut frames = 0;
    const MAX_FRAMES: u32 = 200;
    loop {
        frames += 1;
        assert!(frames <= MAX_FRAMES, "did not see a question message within {MAX_FRAMES} frames");

        let msg = read_json(&mut ws);
        match msg["type"].as_str() {
            Some("question") => {
                assert_eq!(msg["question"], "How wide should it be?");
                assert_eq!(msg["options"], json!(["20mm", "40mm", "60mm"]));
                break;
            }
            Some("snapshot") => {
                panic!("finalizing snapshot arrived before the question message: {msg}");
            }
            _ => {}
        }
    }
}

/// `snapshot.pending_question` must carry the outstanding question (and
/// survive a fresh connection / reload) until the user answers it, at which
/// point it goes back to `null`. The question must also land in
/// `conversation` with `role == "question"`, never duplicated as an
/// `assistant` message (the duplication fix).
#[test]
fn snapshot_carries_pending_question_until_answered() {
    let (server, _bin_dir) = spawn_with_fake_claude_script(FAKE_CLAUDE_ASK_QUESTION_SCRIPT);
    let id = create_project(&server.base, "widget");

    let mut ws = connect(&server, &id);
    let _snapshot = read_json(&mut ws);

    send_json(&mut ws, json!({
        "type": "prompt",
        "text": "spec it",
        "part_refs": [],
        "lib_refs": [],
    }));

    let mut frames = 0;
    const MAX_FRAMES: u32 = 200;
    let mut saw_question = false;
    let final_snapshot = loop {
        frames += 1;
        assert!(frames <= MAX_FRAMES, "did not see the finalizing snapshot within {MAX_FRAMES} frames");

        let msg = read_json(&mut ws);
        match msg["type"].as_str() {
            Some("question") => saw_question = true,
            Some("snapshot") => {
                assert!(saw_question, "snapshot arrived before the question message");
                break msg;
            }
            _ => {}
        }
    };

    assert_eq!(final_snapshot["pending_question"]["question"], "How wide should it be?");
    assert_eq!(final_snapshot["pending_question"]["options"], json!(["20mm", "40mm", "60mm"]));

    let conversation = final_snapshot["conversation"].as_array().expect("conversation array");
    let has_question_entry = conversation.iter().any(|entry| {
        entry["role"] == "question" && entry["content"] == "How wide should it be?"
    });
    assert!(has_question_entry, "conversation should contain a role:question entry: {conversation:?}");
    let has_duplicate_assistant = conversation.iter().any(|entry| {
        entry["role"] == "assistant" && entry["content"] == "How wide should it be?"
    });
    assert!(!has_duplicate_assistant, "question must not also appear as an assistant message: {conversation:?}");

    // A second connection to the same project must see the same
    // pending_question — reload keeps the interactive card.
    let mut ws2 = connect(&server, &id);
    let snapshot2 = read_json(&mut ws2);
    assert_eq!(snapshot2["pending_question"]["question"], "How wide should it be?");
    assert_eq!(snapshot2["pending_question"]["options"], json!(["20mm", "40mm", "60mm"]));

    // Close the second connection before answering. Both connections share
    // one `AppCore`, and `poll_core_events` DRAINS `core.poll_events()` on
    // whichever connection's poll loop happens to run first — if `ws2`
    // stayed open, the post-answer finalizing snapshot could be delivered to
    // it instead of `ws`, leaving `ws`'s read loop below waiting forever
    // (flaky `WouldBlock` timeouts). Give the server-side task a moment to
    // notice the close and exit before sending the answer.
    drop(ws2);
    std::thread::sleep(Duration::from_millis(200));

    // Answering (any prompt) resolves the pending question.
    send_json(&mut ws, json!({
        "type": "prompt",
        "text": "40mm",
        "part_refs": [],
        "lib_refs": [],
    }));

    let mut frames2 = 0;
    loop {
        frames2 += 1;
        assert!(frames2 <= MAX_FRAMES, "did not see a snapshot after answering within {MAX_FRAMES} frames");
        let msg = read_json(&mut ws);
        if msg["type"] == "snapshot" {
            assert_eq!(msg["pending_question"], Value::Null);
            break;
        }
    }
}

/// A project with no outstanding question must report `pending_question` as
/// JSON `null` in its initial snapshot.
#[test]
fn snapshot_pending_question_is_null_by_default() {
    let server = common::spawn();
    let id = create_project(&server.base, "widget");

    let mut ws = connect(&server, &id);
    let snapshot = read_json(&mut ws);

    assert_eq!(snapshot["type"], "snapshot");
    assert_eq!(snapshot["pending_question"], Value::Null);
}

/// A `mcp__smidr__request_phase_change` tool call must become a pinned
/// `{"type":"phase_switch_request","target":..,"reason":..}` WS message —
/// arriving before the finalizing snapshot.
#[test]
fn request_phase_change_tool_call_emits_phase_switch_request_message() {
    let (server, _bin_dir) = spawn_with_fake_claude_script(FAKE_CLAUDE_PHASE_SWITCH_SCRIPT);
    let id = create_project(&server.base, "widget");

    let mut ws = connect(&server, &id);
    let _snapshot = read_json(&mut ws);

    send_json(&mut ws, json!({
        "type": "prompt",
        "text": "make it prettier",
        "part_refs": [],
        "lib_refs": [],
    }));

    let mut frames = 0;
    const MAX_FRAMES: u32 = 200;
    loop {
        frames += 1;
        assert!(frames <= MAX_FRAMES, "did not see a phase_switch_request message within {MAX_FRAMES} frames");

        let msg = read_json(&mut ws);
        match msg["type"].as_str() {
            Some("phase_switch_request") => {
                assert_eq!(msg["target"], "build");
                assert_eq!(msg["reason"], "that is a functional change");
                break;
            }
            Some("snapshot") => {
                panic!("finalizing snapshot arrived before the phase_switch_request message: {msg}");
            }
            _ => {}
        }
    }
}

/// `snapshot.pending_phase_switch` must carry the outstanding request (and
/// survive a fresh connection / reload) until the user denies it, at which
/// point it goes back to `null` and the phase is unchanged.
#[test]
fn snapshot_carries_pending_phase_switch_until_denied() {
    let (server, _bin_dir) = spawn_with_fake_claude_script(FAKE_CLAUDE_PHASE_SWITCH_SCRIPT);
    let id = create_project(&server.base, "widget");

    let mut ws = connect(&server, &id);
    let _snapshot = read_json(&mut ws);

    send_json(&mut ws, json!({
        "type": "prompt",
        "text": "make it prettier",
        "part_refs": [],
        "lib_refs": [],
    }));

    let mut frames = 0;
    const MAX_FRAMES: u32 = 200;
    let mut saw_request = false;
    let final_snapshot = loop {
        frames += 1;
        assert!(frames <= MAX_FRAMES, "did not see the finalizing snapshot within {MAX_FRAMES} frames");

        let msg = read_json(&mut ws);
        match msg["type"].as_str() {
            Some("phase_switch_request") => saw_request = true,
            Some("snapshot") => {
                assert!(saw_request, "snapshot arrived before the phase_switch_request message");
                break msg;
            }
            _ => {}
        }
    };

    assert_eq!(final_snapshot["pending_phase_switch"]["target"], "build");
    assert_eq!(final_snapshot["pending_phase_switch"]["reason"], "that is a functional change");

    // A second connection to the same project must see the same
    // pending_phase_switch — reload keeps the modal alive (persistence).
    let mut ws2 = connect(&server, &id);
    let snapshot2 = read_json(&mut ws2);
    assert_eq!(snapshot2["pending_phase_switch"]["target"], "build");
    assert_eq!(snapshot2["pending_phase_switch"]["reason"], "that is a functional change");

    // Close the second connection before denying, same rationale as
    // `snapshot_carries_pending_question_until_answered`: both connections
    // share one `AppCore`, and `poll_core_events` drains events on whichever
    // connection's poll loop runs first.
    drop(ws2);
    std::thread::sleep(Duration::from_millis(200));

    send_json(&mut ws, json!({"type": "deny_phase_switch"}));
    let reply = read_json(&mut ws);
    assert_eq!(reply["type"], "snapshot");
    assert_eq!(reply["pending_phase_switch"], Value::Null);
    assert_eq!(reply["phase"], "Spec");
}

/// The phase must remain unchanged after a `deny_phase_switch` — denying a
/// phase-switch request must never itself trigger a phase change.
#[test]
fn deny_phase_switch_does_not_change_phase() {
    let (server, _bin_dir) = spawn_with_fake_claude_script(FAKE_CLAUDE_PHASE_SWITCH_SCRIPT);
    let id = create_project(&server.base, "widget");

    let mut ws = connect(&server, &id);
    let _snapshot = read_json(&mut ws);

    send_json(&mut ws, json!({
        "type": "prompt",
        "text": "make it prettier",
        "part_refs": [],
        "lib_refs": [],
    }));

    let mut frames = 0;
    const MAX_FRAMES: u32 = 200;
    loop {
        frames += 1;
        assert!(frames <= MAX_FRAMES, "did not see the finalizing snapshot within {MAX_FRAMES} frames");
        let msg = read_json(&mut ws);
        if msg["type"] == "snapshot" {
            break;
        }
    }

    send_json(&mut ws, json!({"type": "deny_phase_switch"}));
    let reply = read_json(&mut ws);
    assert_eq!(reply["type"], "snapshot");
    assert_eq!(reply["phase"], "Spec");
    assert_eq!(reply["pending_phase_switch"], Value::Null);

    // A fresh connection must also report Spec, unchanged.
    let mut ws2 = connect(&server, &id);
    let snapshot2 = read_json(&mut ws2);
    assert_eq!(snapshot2["phase"], "Spec");
}
