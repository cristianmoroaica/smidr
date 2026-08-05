//! Black-box tests for `GET /api/artifacts/{project}/{file}` and the
//! `iteration_added`/snapshot `iterations` wiring driven by on-disk GLB
//! artifacts (Phase 3 Task 3.1, Rust side).

mod common;

use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

const READ_TIMEOUT: Duration = Duration::from_secs(10);

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

/// `approve_phase` lazily creates a session (see `AppCore::approve_phase`)
/// when none exists yet, which is the cheapest way to get a real session
/// directory on disk without a fake `claude` binary. Send it and drain the
/// resulting `phase_state` reply.
fn force_session_creation(ws: &mut WebSocket<MaybeTlsStream<TcpStream>>) {
    send_json(ws, json!({"type": "approve_phase"}));
    let reply = read_json(ws);
    assert_eq!(reply["type"], "phase_state");
}

/// Walk `<HOME>/Smidr/<project>` looking for the session directory
/// `force_session_creation` (via `approve_phase`) produced — the first
/// subdirectory containing a `session.json`, found via recursive search
/// since the exact nesting is an implementation detail.
fn find_session_dir(home: &std::path::Path, project: &str) -> PathBuf {
    let project_root = home.join("Smidr").join(project);
    fn search(dir: &std::path::Path) -> Option<PathBuf> {
        let entries = std::fs::read_dir(dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.join("session.json").is_file() {
                    return Some(path);
                }
                if let Some(found) = search(&path) {
                    return Some(found);
                }
            }
        }
        None
    }
    search(&project_root).unwrap_or_else(|| {
        panic!("no session dir with session.json found under {}", project_root.display())
    })
}

#[test]
fn missing_artifact_is_404() {
    let server = common::spawn();
    let id = create_project(&server.base, "widget-art-1");

    let mut ws = connect(&server, &id);
    let _snapshot = read_json(&mut ws);

    let resp = ureq::get(&format!("{}/api/artifacts/{}/iteration_001.glb", server.base, id)).call();
    let err = resp.expect_err("missing artifact should 404");
    match err {
        ureq::Error::StatusCode(404) => {}
        other => panic!("expected 404, got {other:?}"),
    }
}

#[test]
fn existing_artifact_is_served_with_correct_content_type() {
    let server = common::spawn();
    let id = create_project(&server.base, "widget-art-2");

    let mut ws = connect(&server, &id);
    let _snapshot = read_json(&mut ws);
    force_session_creation(&mut ws);

    let session_dir = find_session_dir(server.home.path(), &id);

    let glb_bytes = b"glTF\0\0\0\0".to_vec();
    std::fs::write(session_dir.join("iteration_001.glb"), &glb_bytes).expect("write glb");
    let manifest_json = json!({"components": [], "dimensions": {}});
    std::fs::write(
        session_dir.join("iteration_001.manifest.json"),
        serde_json::to_vec(&manifest_json).unwrap(),
    )
    .expect("write manifest");

    let mut resp = ureq::get(&format!("{}/api/artifacts/{}/iteration_001.glb", server.base, id))
        .call()
        .expect("glb should be served");
    assert_eq!(resp.status().as_u16(), 200);
    let content_type = resp.headers().get("content-type").unwrap().to_str().unwrap().to_string();
    assert_eq!(content_type, "model/gltf-binary");
    let body = resp.body_mut().read_to_string().expect("read glb body as string");
    assert_eq!(body.into_bytes(), glb_bytes);

    let mut resp = ureq::get(&format!(
        "{}/api/artifacts/{}/iteration_001.manifest.json",
        server.base, id
    ))
    .call()
    .expect("manifest should be served");
    assert_eq!(resp.status().as_u16(), 200);
    let content_type = resp.headers().get("content-type").unwrap().to_str().unwrap().to_string();
    assert_eq!(content_type, "application/json");
    let body: Value = resp.body_mut().read_json().unwrap();
    assert_eq!(body, manifest_json);
}

#[test]
fn traversal_and_garbage_requests_are_rejected() {
    let server = common::spawn();
    let id = create_project(&server.base, "widget-art-3");

    let mut ws = connect(&server, &id);
    let _snapshot = read_json(&mut ws);

    // Percent-encoded traversal in the project segment: never 200.
    let resp = ureq::get(&format!("{}/api/artifacts/..%2Fetc/iteration_001.glb", server.base)).call();
    match resp {
        Ok(r) => panic!("traversal must not succeed, got status {}", r.status()),
        Err(ureq::Error::StatusCode(400)) | Err(ureq::Error::StatusCode(404)) => {}
        Err(other) => panic!("expected 400 or 404, got {other:?}"),
    }

    // Not a valid iteration filename pattern.
    let resp = ureq::get(&format!("{}/api/artifacts/{}/evil.txt", server.base, id)).call();
    match resp {
        Ok(r) => panic!("garbage filename must not succeed, got status {}", r.status()),
        Err(ureq::Error::StatusCode(404)) => {}
        Err(other) => panic!("expected 404, got {other:?}"),
    }

    // Non-digit iteration number.
    let resp = ureq::get(&format!("{}/api/artifacts/{}/iteration_abc.glb", server.base, id)).call();
    match resp {
        Ok(r) => panic!("non-digit iteration must not succeed, got status {}", r.status()),
        Err(ureq::Error::StatusCode(404)) => {}
        Err(other) => panic!("expected 404, got {other:?}"),
    }
}

#[test]
fn snapshot_iterations_reflect_glb_artifacts_on_disk() {
    let server = common::spawn();
    let id = create_project(&server.base, "widget-art-4");

    let mut ws = connect(&server, &id);
    let _snapshot = read_json(&mut ws);
    force_session_creation(&mut ws);

    let session_dir = find_session_dir(server.home.path(), &id);
    std::fs::write(session_dir.join("iteration_001.glb"), b"glTF\0\0\0\0").expect("write glb");

    // A fresh connection's snapshot should now report iteration 1.
    let mut ws2 = connect(&server, &id);
    let snapshot2 = read_json(&mut ws2);
    assert_eq!(snapshot2["type"], "snapshot");
    assert_eq!(snapshot2["iterations"], json!([1]));
}
