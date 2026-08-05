mod common;

use serde_json::Value;

fn get_json(url: &str) -> (u16, Value) {
    let resp = ureq::get(url).call();
    match resp {
        Ok(mut r) => {
            let status = r.status().as_u16();
            let body: Value = r.body_mut().read_json().unwrap_or(Value::Null);
            (status, body)
        }
        Err(ureq::Error::StatusCode(code)) => (code, Value::Null),
        Err(e) => panic!("request failed: {e}"),
    }
}

#[test]
fn get_projects_returns_array_with_default_untitled() {
    let server = common::spawn();
    let (status, body) = get_json(&format!("{}/api/projects", server.base));
    assert_eq!(status, 200);
    let arr = body.as_array().expect("expected JSON array");
    assert!(!arr.is_empty(), "expected auto-created Untitled project");
    assert!(arr.iter().any(|p| p["name"] == "Untitled"));
    for p in arr {
        assert!(p["id"].is_string());
        assert!(p["name"].is_string());
        assert!(p["created"].is_string());
        assert!(p["phase"].is_string());
    }
}

#[test]
fn post_project_creates_and_lists_with_spec_phase() {
    let server = common::spawn();

    let resp = ureq::post(&format!("{}/api/projects", server.base))
        .send_json(serde_json::json!({"name": "widget"}));
    let mut resp = resp.expect("post should succeed");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.body_mut().read_json().unwrap();
    assert!(body["id"].is_string());

    let (status, list) = get_json(&format!("{}/api/projects", server.base));
    assert_eq!(status, 200);
    let arr = list.as_array().unwrap();
    let widget = arr.iter().find(|p| p["name"] == "widget").expect("widget project should be listed");
    assert_eq!(widget["phase"], "Spec");
}

#[test]
fn post_duplicate_project_name_returns_409() {
    let server = common::spawn();

    let _ = ureq::post(&format!("{}/api/projects", server.base))
        .send_json(serde_json::json!({"name": "widget"}))
        .unwrap();

    let resp = ureq::post(&format!("{}/api/projects", server.base))
        .send_json(serde_json::json!({"name": "widget"}));
    match resp {
        Err(ureq::Error::StatusCode(code)) => assert_eq!(code, 409),
        other => panic!("expected 409, got {other:?}"),
    }
}

#[test]
fn post_project_with_dotdot_name_returns_400() {
    let server = common::spawn();

    let resp = ureq::post(&format!("{}/api/projects", server.base))
        .send_json(serde_json::json!({"name": ".."}));
    match resp {
        Err(ureq::Error::StatusCode(code)) => assert_eq!(code, 400),
        other => panic!("expected 400, got {other:?}"),
    }
}

#[test]
fn delete_project_removes_it_and_unknown_id_is_404() {
    let server = common::spawn();

    let post_resp = ureq::post(&format!("{}/api/projects", server.base))
        .send_json(serde_json::json!({"name": "widget"}))
        .unwrap();
    let mut post_resp = post_resp;
    let post_body: Value = post_resp.body_mut().read_json().unwrap();
    let id = post_body["id"].as_str().unwrap().to_string();

    let resp = ureq::delete(&format!("{}/api/projects/{}", server.base, id)).call();
    let resp = resp.expect("delete should succeed");
    assert_eq!(resp.status().as_u16(), 204);

    let (_status, list) = get_json(&format!("{}/api/projects", server.base));
    let arr = list.as_array().unwrap();
    assert!(!arr.iter().any(|p| p["name"] == "widget"));

    let resp = ureq::delete(&format!("{}/api/projects/does-not-exist", server.base)).call();
    match resp {
        Err(ureq::Error::StatusCode(code)) => assert_eq!(code, 404),
        other => panic!("expected 404, got {other:?}"),
    }
}

#[test]
fn post_project_with_empty_or_whitespace_name_returns_400() {
    let server = common::spawn();

    for name in ["", "   "] {
        let resp = ureq::post(&format!("{}/api/projects", server.base))
            .send_json(serde_json::json!({"name": name}));
        match resp {
            Err(ureq::Error::StatusCode(code)) => assert_eq!(code, 400, "name={name:?}"),
            other => panic!("expected 400 for name={name:?}, got {other:?}"),
        }
    }
}

#[test]
fn post_project_trims_name_and_returned_id_matches_delete_id() {
    let server = common::spawn();

    let resp = ureq::post(&format!("{}/api/projects", server.base))
        .send_json(serde_json::json!({"name": "  widget  "}));
    let mut resp = resp.expect("post should succeed");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.body_mut().read_json().unwrap();
    let id = body["id"].as_str().expect("id should be a string");
    assert_eq!(id, "widget", "returned id should be trimmed, not raw");

    // The trimmed id must be the same one that later works for DELETE.
    let resp = ureq::delete(&format!("{}/api/projects/{}", server.base, id)).call();
    let resp = resp.expect("delete should succeed using the returned id");
    assert_eq!(resp.status().as_u16(), 204);
}

#[test]
fn delete_project_rejects_path_traversal_id() {
    let server = common::spawn();

    // Create a sibling directory of ~/Smidr that must survive untouched.
    let precious = server.home.path().join("precious");
    std::fs::create_dir_all(&precious).unwrap();
    std::fs::write(precious.join("secret.txt"), b"do not delete me").unwrap();

    // axum's `Path<String>` percent-decodes `%2F`/`%2E` back into `/`/`.`,
    // so this arrives at the handler as id == "../precious".
    let resp = ureq::delete(&format!("{}/api/projects/..%2Fprecious", server.base)).call();
    match resp {
        Err(ureq::Error::StatusCode(code)) => {
            assert!(code == 400 || code == 404, "expected 400/404, got {code}")
        }
        other => panic!("expected traversal id to be rejected, got {other:?}"),
    }

    assert!(
        precious.join("secret.txt").exists(),
        "path traversal must not delete a directory outside ~/Smidr"
    );
}
