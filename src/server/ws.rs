//! `GET /api/session?project={id}` — WebSocket session channel (Task 2.2).
//!
//! Wire protocol is pinned by the plan: the exact JSON shapes below (field
//! names, message `type` values) must not change without updating the spec.
//!
//! client→server: `prompt` / `approve_phase` / `advance` / `go_back` / `cancel_stream`
//! server→client: `snapshot` / `stream_delta` / `tool_call` / `phase_state` /
//!                 `iteration_added` / `build_progress` / `error`

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
    core.open_project_by_id(project_id)?;
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

    core.poll_events()
        .into_iter()
        .map(|ev| match ev {
            CoreEvent::StreamDelta(text) => json!({"type": "stream_delta", "text": text}),
            CoreEvent::ToolCall { name, detail } => {
                json!({"type": "tool_call", "name": name, "detail": detail})
            }
            CoreEvent::BuildArtifact { .. } => {
                json!({"type": "iteration_added", "n": core.iteration()})
            }
            CoreEvent::Error(message) => json!({"type": "error", "message": message}),
            CoreEvent::ResponseDone => snapshot_value(core),
        })
        .collect()
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
    let iterations: Vec<u32> = (1..=core.iteration()).collect();
    let spec = if core.spec_content().is_empty() {
        Value::Null
    } else {
        Value::String(core.spec_content().to_string())
    };
    json!({
        "type": "snapshot",
        "phase": core.phase().label(),
        "approved": core.is_phase_approved(core.phase()),
        "conversation": conversation,
        "iterations": iterations,
        "spec": spec,
    })
}

fn phase_state_value(phase: Phase, approved: bool) -> Value {
    json!({"type": "phase_state", "phase": phase.label(), "approved": approved})
}

fn error_msg(message: &str) -> Value {
    json!({"type": "error", "message": message})
}

/// Serializer for `build_progress` — no producer exists yet (build/refine
/// per-component progress tracking lands in Phase 4). Kept here so the wire
/// shape is pinned and ready for that phase to wire up.
#[allow(dead_code)]
fn build_progress_value(component: &str, status: &str) -> Value {
    json!({"type": "build_progress", "component": component, "status": status})
}
