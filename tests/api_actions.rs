//! Black-box tests for the export, open-folder, and baseline-iteration
//! REST actions (`POST /api/projects/{id}/export`,
//! `GET /api/projects/{id}/export/{file}`,
//! `POST /api/projects/{id}/open-folder`, `POST /api/projects/{id}/baseline`).
//!
//! `SMIDR_NO_OPEN=1` is the documented test hook that skips the actual
//! `xdg-open` spawn in `open_folder` (see `src/server/routes.rs`) — these
//! tests never assert any file-manager side effect, only the HTTP response.
//! It is set unconditionally by the shared harness (`tests/common/mod.rs`),
//! so no test can pop a file-manager window during `cargo test`.
//!
//! `ensure_project_open` (routes.rs) is deliberately read-only — it opens
//! an existing project but never lazily creates a session directory, so
//! these REST actions must not be used to *establish* a session. Tests
//! that need one first drive a real `approve_phase` over the WS channel
//! (`establish_session`), exactly as a user clicking Approve would.

mod common;

use std::net::TcpStream;
use std::time::Duration;

use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

const READ_TIMEOUT: Duration = Duration::from_secs(10);

fn spawn() -> common::Server {
    common::spawn()
}

fn create_project(base: &str, name: &str) -> String {
    let resp = ureq::post(&format!("{base}/api/projects")).send_json(json!({"name": name}));
    let mut resp = resp.expect("create project should succeed");
    let body: Value = resp.body_mut().read_json().unwrap();
    body["id"].as_str().unwrap().to_string()
}

/// POST JSON and return `(status, body)` for ANY status code, including
/// 4xx/5xx — needed to assert the `{"error": ...}` shape of error
/// responses, not just their status. `http_status_as_error(false)` stops
/// ureq's default behaviour of turning non-2xx into a bodyless `Err`.
fn post_json(url: &str, body: Value) -> (u16, Value) {
    let mut r = ureq::post(url)
        .config()
        .http_status_as_error(false)
        .build()
        .send_json(body)
        .expect("request should complete (even for a non-2xx status)");
    let status = r.status().as_u16();
    let body: Value = r.body_mut().read_json().unwrap_or(Value::Null);
    (status, body)
}

/// Percent-encode a project id for use in a URL path segment or query
/// value. Project names may legally contain URL-significant characters
/// (`?`, `#`, `%`, spaces) — `is_valid_project_name` only rejects path
/// separators, `..`, leading `.` and `references` — so every id the test
/// interpolates into a URL has to be encoded, exactly as the frontend does
/// with `encodeURIComponent`.
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn connect(server: &common::Server, project_id: &str) -> WebSocket<MaybeTlsStream<TcpStream>> {
    let url = server.ws_url(&format!("/api/session?project={}", enc(project_id)));
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

/// Establish a real session directory the way a user would: connect, read
/// the initial snapshot, then approve the (Spec) phase. `AppCore::approve_phase`
/// lazily creates the session dir + `session.json` on disk as a documented
/// side effect of that explicit user action — unlike the REST actions under
/// test here, which must remain read-only with respect to session creation.
fn establish_session(server: &common::Server, id: &str) {
    let mut ws = connect(server, id);
    let _snapshot = read_json(&mut ws);
    send_json(&mut ws, json!({"type": "approve_phase"}));
    let _phase_state = read_json(&mut ws);
}

#[test]
fn open_folder_returns_existing_absolute_path_for_valid_project() {
    let server = spawn();
    let id = create_project(&server.base, "widget");
    establish_session(&server, &id);

    let (status, body) = post_json(&format!("{}/api/projects/{}/open-folder", server.base, id), Value::Null);
    assert_eq!(status, 200);
    let path = body["path"].as_str().expect("path should be a string");
    assert!(!path.is_empty());
    assert!(std::path::Path::new(path).is_absolute(), "path should be absolute: {path}");
    assert!(std::path::Path::new(path).exists(), "path should exist on disk: {path}");
    assert!(
        std::path::Path::new(path).starts_with(server.home.path()),
        "path should live under the sandboxed HOME: {path}"
    );
}

#[test]
fn open_folder_without_a_session_is_404_with_error_body() {
    let server = spawn();
    let id = create_project(&server.base, "widget");

    // No session ever established — the REST resolve must not create one.
    let (status, body) = post_json(&format!("{}/api/projects/{}/open-folder", server.base, id), Value::Null);
    assert_eq!(status, 404);
    assert!(body["error"].as_str().is_some_and(|s| !s.is_empty()));
}

#[test]
fn open_folder_unknown_project_is_4xx_with_error_body() {
    let server = spawn();

    let (status, body) = post_json(&format!("{}/api/projects/does-not-exist/open-folder", server.base), Value::Null);
    assert!((400..500).contains(&status), "expected 4xx, got {status}");
    assert!(body["error"].as_str().is_some_and(|s| !s.is_empty()));
}

#[test]
fn export_with_no_session_returns_404_with_error_body() {
    let server = spawn();
    let id = create_project(&server.base, "widget");

    // No session established — export must 404, not lazily create one.
    let (status, body) = post_json(&format!("{}/api/projects/{}/export", server.base, id), Value::Null);
    assert_eq!(status, 404);
    assert!(body["error"].as_str().is_some_and(|s| !s.is_empty()));
}

#[test]
fn export_with_session_but_no_geometry_returns_404() {
    let server = spawn();
    let id = create_project(&server.base, "widget");
    establish_session(&server, &id);

    let (status, body) = post_json(&format!("{}/api/projects/{}/export", server.base, id), Value::Null);
    assert_eq!(status, 404);
    assert!(body["error"].as_str().is_some_and(|s| !s.is_empty()));
}

#[test]
fn export_writes_stl_and_step_and_they_are_downloadable_with_correct_headers() {
    let server = spawn();
    let id = create_project(&server.base, "widget");
    establish_session(&server, &id);

    // Resolve the (now-existing) session dir purely for the test's own
    // filesystem setup; open-folder no longer creates anything here.
    let (status, body) = post_json(&format!("{}/api/projects/{}/open-folder", server.base, id), Value::Null);
    assert_eq!(status, 200);
    let session_dir = std::path::PathBuf::from(body["path"].as_str().unwrap());

    std::fs::write(session_dir.join("_buffer.stl"), b"fake stl bytes").unwrap();
    std::fs::write(session_dir.join("_buffer.step"), b"fake step bytes").unwrap();

    let (status, body) = post_json(&format!("{}/api/projects/{}/export", server.base, id), Value::Null);
    assert_eq!(status, 200);
    assert_eq!(body["dir"].as_str().unwrap(), session_dir.to_string_lossy());
    assert!(session_dir.join("export.stl").exists());
    assert!(session_dir.join("export.step").exists());

    let files = body["files"].as_array().expect("files should be an array");
    assert_eq!(files.len(), 2);
    let names: Vec<&str> = files.iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"export.stl"));
    assert!(names.contains(&"export.step"));

    for f in files {
        let name = f["name"].as_str().unwrap();
        let url = format!("{}{}", server.base, f["url"].as_str().unwrap());
        let resp = ureq::get(&url).call();
        let mut resp = resp.expect("export file download should succeed");
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(
            resp.headers().get("content-type").unwrap().to_str().unwrap(),
            "application/octet-stream"
        );
        assert_eq!(
            resp.headers().get("content-disposition").unwrap().to_str().unwrap(),
            format!("attachment; filename=\"{name}\"")
        );
        let body = resp.body_mut().read_to_string().expect("read body as string");
        let expected = if name == "export.stl" { "fake stl bytes" } else { "fake step bytes" };
        assert_eq!(body, expected);
    }
}

/// Regression: the download `url` the server hands back must be usable
/// verbatim. A project id containing a URL-significant character (`?` here —
/// `is_valid_project_name` allows it, and `POST /api/projects` happily
/// creates it) previously produced `/api/projects/a?b/export/export.stl`,
/// which a browser resolves as path `/api/projects/a` + query
/// `b/export/export.stl` — a 405, i.e. a silently broken download from the
/// approve modal's anchor.
#[test]
fn export_url_is_percent_encoded_and_downloadable_verbatim_for_url_significant_ids() {
    let server = spawn();
    let id = create_project(&server.base, "a?b");
    assert_eq!(id, "a?b");
    establish_session(&server, &id);

    let (status, body) =
        post_json(&format!("{}/api/projects/{}/open-folder", server.base, enc(&id)), Value::Null);
    assert_eq!(status, 200);
    let session_dir = std::path::PathBuf::from(body["path"].as_str().unwrap());
    std::fs::write(session_dir.join("_buffer.stl"), b"fake stl bytes").unwrap();

    let (status, body) =
        post_json(&format!("{}/api/projects/{}/export", server.base, enc(&id)), Value::Null);
    assert_eq!(status, 200);
    let files = body["files"].as_array().expect("files should be an array");
    assert_eq!(files.len(), 1);
    let url = files[0]["url"].as_str().unwrap();
    assert_eq!(url, "/api/projects/a%3Fb/export/export.stl", "id must be percent-encoded");

    // Follow the returned URL verbatim, as the frontend anchor does.
    let resp = ureq::get(&format!("{}{}", server.base, url)).call();
    let mut resp = resp.expect("returned export url should download verbatim");
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.body_mut().read_to_string().unwrap(), "fake stl bytes");
}

#[test]
fn export_file_rejects_arbitrary_and_traversal_names_while_real_export_succeeds() {
    let server = spawn();
    let id = create_project(&server.base, "widget");
    establish_session(&server, &id);

    let (status, body) = post_json(&format!("{}/api/projects/{}/open-folder", server.base, id), Value::Null);
    assert_eq!(status, 200);
    let session_dir = std::path::PathBuf::from(body["path"].as_str().unwrap());
    std::fs::write(session_dir.join("_buffer.stl"), b"fake stl bytes").unwrap();

    // Materialize a real, downloadable export.stl so the guard is proven
    // against an actually-present core + file, not an absent one.
    let (status, _body) = post_json(&format!("{}/api/projects/{}/export", server.base, id), Value::Null);
    assert_eq!(status, 200);

    let resp = ureq::get(&format!("{}/api/projects/{}/export/evil.txt", server.base, id)).call();
    match resp {
        Err(ureq::Error::StatusCode(code)) => assert_eq!(code, 404),
        other => panic!("expected 404, got {other:?}"),
    }

    let resp = ureq::get(&format!("{}/api/projects/{}/export/..%2Fsecret", server.base, id)).call();
    match resp {
        Err(ureq::Error::StatusCode(code)) => assert_eq!(code, 404),
        other => panic!("expected 404, got {other:?}"),
    }

    // Sanity: the exact same core/session state genuinely serves the real
    // file, so the 404s above are the guard rejecting the name, not some
    // unrelated failure (e.g. a missing core).
    let resp = ureq::get(&format!("{}/api/projects/{}/export/export.stl", server.base, id)).call();
    assert_eq!(resp.expect("export.stl should download").status().as_u16(), 200);
}

#[test]
fn baseline_survives_and_is_visible_in_a_fresh_ws_snapshot() {
    let server = spawn();
    let id = create_project(&server.base, "widget");
    establish_session(&server, &id);

    // A fresh snapshot defaults to null.
    let mut ws = connect(&server, &id);
    let snapshot = read_json(&mut ws);
    assert_eq!(snapshot["baseline_iteration"], Value::Null);
    drop(ws);

    let (status, body) = post_json(&format!("{}/api/projects/{}/baseline", server.base, id), json!({"n": 3}));
    assert_eq!(status, 200);
    assert_eq!(body["baseline_iteration"], 3);

    let mut ws2 = connect(&server, &id);
    let snapshot2 = read_json(&mut ws2);
    assert_eq!(snapshot2["baseline_iteration"], 3);
}

#[test]
fn baseline_without_a_session_is_404_with_error_body() {
    let server = spawn();
    let id = create_project(&server.base, "widget");

    let (status, body) = post_json(&format!("{}/api/projects/{}/baseline", server.base, id), json!({"n": 1}));
    assert_eq!(status, 404);
    assert!(body["error"].as_str().is_some_and(|s| !s.is_empty()));
}

#[test]
fn baseline_missing_or_invalid_n_is_400() {
    let server = spawn();
    let id = create_project(&server.base, "widget");
    establish_session(&server, &id);

    let (status, _) = post_json(&format!("{}/api/projects/{}/baseline", server.base, id), json!({}));
    assert_eq!(status, 400);

    let (status, _) = post_json(&format!("{}/api/projects/{}/baseline", server.base, id), json!({"n": "nope"}));
    assert_eq!(status, 400);
}

#[test]
fn baseline_unknown_project_is_404_with_error_body() {
    let server = spawn();

    let (status, body) = post_json(&format!("{}/api/projects/does-not-exist/baseline", server.base), json!({"n": 1}));
    assert_eq!(status, 404);
    assert!(body["error"].as_str().is_some_and(|s| !s.is_empty()));
}
