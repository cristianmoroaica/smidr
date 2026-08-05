//! Black-box test for `GET /api/refs` — lists the reference library the
//! same way the (removed) TUI `/ref list` command did.

mod common;

use serde_json::Value;

#[test]
fn get_api_refs_returns_seeded_m3_shcs() {
    let server = common::spawn();

    // `AppCore` seeds ~/Smidr/references on construction, but construction
    // is lazy — it happens the first time a project's `AppCore` is created,
    // which is on the first WebSocket connect for that project (see
    // `ServerState::core_for`), not on `POST /api/projects`. Create a
    // project, then open (and immediately drop) a session WS to force that
    // lazy seeding before listing the library.
    let resp = ureq::post(&format!("{}/api/projects", server.base))
        .send_json(serde_json::json!({"name": "widget"}));
    let mut resp = resp.expect("create project should succeed");
    let body: Value = resp.body_mut().read_json().unwrap();
    let project_id = body["id"].as_str().unwrap().to_string();

    let url = server.ws_url(&format!("/api/session?project={project_id}"));
    let (mut ws, _resp) = tungstenite::connect(&url).expect("ws connect should succeed");
    // Read the initial snapshot so the connection is fully established
    // before we tear it down.
    let _ = ws.read().expect("snapshot read should not fail");
    let _ = ws.close(None);
    drop(ws);

    let resp = ureq::get(&format!("{}/api/refs", server.base)).call();
    let mut resp = resp.expect("GET /api/refs should succeed");
    assert_eq!(resp.status(), 200);

    let body: Value = resp.body_mut().read_json().unwrap();
    let refs = body.as_array().expect("response is a JSON array");

    let m3 = refs
        .iter()
        .find(|r| r["slug"] == "m3_shcs")
        .unwrap_or_else(|| panic!("expected m3_shcs in refs: {refs:?}"));

    assert!(
        m3["name"].as_str().is_some_and(|s| !s.is_empty()),
        "m3_shcs name must be non-empty: {m3}"
    );
    assert!(
        m3["category"].as_str().is_some_and(|s| !s.is_empty()),
        "m3_shcs category must be non-empty: {m3}"
    );
}
