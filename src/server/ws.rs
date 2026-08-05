//! `GET /api/session?project={id}` — WebSocket session channel (Task 2.2).
//!
//! Wire protocol is pinned by the plan: the exact JSON shapes below (field
//! names, message `type` values) must not change without updating the spec.
//!
//! client→server: `prompt` / `approve_phase` / `advance` / `go_back` / `cancel_stream`
//! server→client: `snapshot` / `stream_delta` / `tool_call` / `question` /
//!                 `phase_state` / `iteration_added` / `build_progress` / `error`
//!
//! `snapshot` additionally carries a `pending_question` field (see
//! `pending_question_value`) so a reconnecting/reloading client keeps the
//! interactive question card until it's answered.

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures_util::SinkExt;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::core::{AppCore, CoreEvent, SwitchDenied};
use crate::phase::Phase;
use crate::server::SharedState;

pub fn router(state: SharedState) -> Router {
    Router::new()
        .route("/api/session", get(upgrade))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
struct SessionQuery {
    project: String,
}

async fn upgrade(
    ws: WebSocketUpgrade,
    State(state): State<SharedState>,
    Query(q): Query<SessionQuery>,
    headers: HeaderMap,
) -> Response {
    // Browsers don't apply CORS to WebSockets: without this check any webpage
    // could open ws://127.0.0.1:<port> and drive the session. Same-origin
    // only; requests without an Origin header (tests, CLI clients) pass.
    if !origin_allowed(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin websocket rejected").into_response();
    }
    ws.max_message_size(1 << 20)
        .on_upgrade(move |socket| handle_socket(socket, state, q.project))
}

/// If an `Origin` header is present, its scheme must be http and its
/// host:port must equal the request's `Host` header (i.e. same-origin).
fn origin_allowed(headers: &HeaderMap) -> bool {
    let origin = match headers.get("origin").and_then(|v| v.to_str().ok()) {
        None => return true,
        Some(o) => o,
    };
    let host = match headers.get("host").and_then(|v| v.to_str().ok()) {
        None => return false,
        Some(h) => h,
    };
    match origin.strip_prefix("http://") {
        Some(origin_host) => origin_host == host,
        None => false,
    }
}

async fn handle_socket(mut socket: WebSocket, state: SharedState, project_id: String) {
    let snapshot = match init_session(&state, &project_id) {
        Ok(v) => v,
        Err(e) => {
            let _ = socket.send(Message::Text(error_msg(&e).to_string().into())).await;
            let _ = socket.close().await;
            return;
        }
    };

    if socket.send(Message::Text(snapshot.to_string().into())).await.is_err() {
        return;
    }

    let mut tick = tokio::time::interval(Duration::from_millis(50));

    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        for out in handle_client_message(&state, &project_id, text.as_str()) {
                            if socket.send(Message::Text(out.to_string().into())).await.is_err() {
                                return;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => return,
                    Some(Ok(_)) => {} // ignore ping/pong/binary
                    Some(Err(_)) => return,
                }
            }
            _ = tick.tick() => {
                for out in poll_core_events(&state, &project_id) {
                    if socket.send(Message::Text(out.to_string().into())).await.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

/// Get/create the core for `project_id`, turn the approval gate on, and open
/// the project (loading its last session, if any). Entirely synchronous —
/// must not be called while holding the lock across an `.await`.
fn init_session(state: &SharedState, project_id: &str) -> Result<Value, String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    let core = guard.core_for(project_id)?;
    core.set_phase_gate(true);
    if core.briefing_pending() {
        // The briefing session is already open in memory (created from piped
        // stdin at server startup); re-opening by id would clobber it. Fire
        // the synthetic first prompt exactly once instead.
        core.clear_briefing_pending();
        core.submit_prompt(
            "Please review the attached conversation and begin extracting spec fields.",
            &[],
            &[],
        );
    } else {
        core.open_project_by_id(project_id)?;
    }
    Ok(snapshot_value(core))
}

fn handle_client_message(state: &SharedState, project_id: &str, text: &str) -> Vec<Value> {
    let parsed: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return vec![error_msg("malformed message")],
    };

    let mut guard = match state.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    let core = match guard.cores.get_mut(project_id) {
        Some(c) => c,
        None => return vec![error_msg("unknown session")],
    };

    match parsed.get("type").and_then(|t| t.as_str()) {
        Some("prompt") => {
            let text = parsed.get("text").and_then(|t| t.as_str()).unwrap_or("");
            let part_refs = string_array(&parsed, "part_refs");
            let lib_refs = string_array(&parsed, "lib_refs");
            core.submit_prompt(text, &part_refs, &lib_refs);
            Vec::new()
        }
        Some("approve_phase") => {
            core.approve_phase();
            vec![phase_state_value(core.phase(), true)]
        }
        Some("advance") => match Phase::from_index(core.phase().index() + 1) {
            None => vec![error_msg("no next phase")],
            Some(target) => match core.try_switch_phase(target) {
                Ok(()) => vec![
                    phase_state_value(core.phase(), core.is_phase_approved(core.phase())),
                    snapshot_value(core),
                ],
                Err(SwitchDenied::NotApproved) => vec![error_msg("phase not approved")],
                Err(SwitchDenied::SamePhase) => vec![error_msg("phase not approved")],
            },
        },
        Some("go_back") => {
            let target = match parsed.get("target").and_then(|t| t.as_str()) {
                Some("spec") => Some(Phase::Spec),
                Some("build") => Some(Phase::Build),
                _ => None,
            };
            match target {
                None => vec![error_msg("invalid go_back target")],
                Some(t) if t.index() >= core.phase().index() => {
                    vec![error_msg("cannot go back to that phase")]
                }
                Some(t) => match core.try_switch_phase(t) {
                    Ok(()) => vec![
                        phase_state_value(core.phase(), core.is_phase_approved(core.phase())),
                        snapshot_value(core),
                    ],
                    Err(_) => vec![error_msg("cannot go back to that phase")],
                },
            }
        }
        Some("cancel_stream") => {
            core.cancel();
            Vec::new()
        }
        _ => vec![error_msg("unknown message type")],
    }
}

fn poll_core_events(state: &SharedState, project_id: &str) -> Vec<Value> {
    let mut guard = match state.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    let core = match guard.cores.get_mut(project_id) {
        Some(c) => c,
        None => return Vec::new(),
    };

    // `AppCore::poll_events` (src/core/app.rs) drains the background-result
    // channel — which can produce `ResponseDone` — before draining queued
    // tool calls, so it can (rarely, when a fast tool-call/result pair lands
    // within the same poll tick) yield a `ResponseDone` ahead of the
    // `Question`/other events the tool call itself produced, even though the
    // tool call happened first in wall-clock time. The root cause lives in
    // app.rs's drain order and is out of scope here (owned by a sibling
    // spec); this is a deliberate WS-layer invariant to compensate: the
    // finalizing `snapshot` must never precede events describing what
    // happened during the turn it's summarizing, so `ResponseDone`-derived
    // snapshots are held back and flushed only after every other event in
    // this batch, preserving their relative order. `StreamDelta` and
    // `ToolCall`/`Question` events for the SAME turn always precede its
    // `ResponseDone` within `poll_events`'s own per-source ordering, and
    // `Error` and `ResponseDone` are mutually exclusive per turn, so this
    // reorder only ever moves a snapshot later, never drops or duplicates an
    // event.
    let mut out = Vec::new();
    let mut snapshots = Vec::new();

    for ev in core.poll_events() {
        match ev {
            CoreEvent::StreamDelta(text) => {
                out.push(json!({"type": "stream_delta", "text": text}));
            }
            CoreEvent::ToolCall { name, detail } => {
                out.push(json!({"type": "tool_call", "name": name, "detail": detail}));
            }
            CoreEvent::BuildArtifact { .. } => {
                // The iteration number is derived from the GLB artifacts
                // actually written to disk, not from `core.iteration()`
                // (which counts build attempts, not successful exports). If
                // no GLB exists yet (e.g. the export step hasn't run), emit
                // nothing for this event.
                if let Some(n) = crate::server::artifacts::glb_iterations(core.session_dir()).last() {
                    out.push(json!({"type": "iteration_added", "n": n}));
                }
            }
            CoreEvent::BuildProgress { component, status } => {
                out.push(build_progress_value(&component, &status));
            }
            CoreEvent::Question { question, options } => {
                out.push(json!({"type": "question", "question": question, "options": options}));
            }
            CoreEvent::Error(message) => {
                out.push(json!({"type": "error", "message": message}));
            }
            CoreEvent::ResponseDone => {
                snapshots.push(snapshot_value(core));
            }
        }
    }

    out.extend(snapshots);
    out
}

fn string_array(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default()
}

fn snapshot_value(core: &AppCore) -> Value {
    let conversation: Vec<Value> = core
        .messages()
        .iter()
        .map(|(role, content)| json!({"role": role, "content": content}))
        .collect();
    let iterations: Vec<u32> = crate::server::artifacts::glb_iterations(core.session_dir());
    let spec = spec_value(core);
    let pending_question = pending_question_value(core);
    json!({
        "type": "snapshot",
        "phase": core.phase().label(),
        "approved": core.is_phase_approved(core.phase()),
        "conversation": conversation,
        "iterations": iterations,
        "spec": spec,
        "pending_question": pending_question,
    })
}

/// `pending_question` for the snapshot: `{"question":<str>,"options":[<str>...]}`
/// when a question is awaiting an answer, else JSON `null`. Pinned shape —
/// keeps a reloading/reconnecting client's interactive question card alive.
fn pending_question_value(core: &AppCore) -> Value {
    match core.pending_question() {
        Some((question, options)) => json!({"question": question, "options": options}),
        None => Value::Null,
    }
}

/// `spec` for the snapshot: prefer the in-memory spec narrative (populated
/// during a live Spec turn), falling back to what's on disk for a session
/// that hasn't had a live turn since the process started (restart/reconnect)
/// — `spec_narrative.md` first, then `goal.md`.
fn spec_value(core: &AppCore) -> Value {
    if !core.spec_content().is_empty() {
        return Value::String(core.spec_content().to_string());
    }
    let Some(dir) = core.session_dir() else {
        return Value::Null;
    };
    for name in ["spec_narrative.md", "goal.md"] {
        if let Ok(content) = std::fs::read_to_string(dir.join(name)) {
            if !content.is_empty() {
                return Value::String(content);
            }
        }
    }
    Value::Null
}

fn phase_state_value(phase: Phase, approved: bool) -> Value {
    json!({"type": "phase_state", "phase": phase.label(), "approved": approved})
}

fn error_msg(message: &str) -> Value {
    json!({"type": "error", "message": message})
}

/// Serializer for `build_progress`. The emitted shape is pinned:
/// `{"type":"build_progress","component":<str>,"status":<str>}`.
fn build_progress_value(component: &str, status: &str) -> Value {
    json!({"type": "build_progress", "component": component, "status": status})
}
