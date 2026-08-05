mod common;

/// Default build (no `embed-frontend` feature): the static handler must
/// still respond 200 text/html with guidance instead of failing/404ing,
/// and must never shadow the `/api/*` routes.
#[cfg(not(feature = "embed-frontend"))]
#[test]
fn root_returns_html_with_build_instructions() {
    let server = common::spawn();
    let resp = ureq::get(&format!("{}/", server.base))
        .call()
        .expect("GET / should succeed");
    assert_eq!(resp.status().as_u16(), 200);
    let content_type = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap_or_default().to_string())
        .unwrap_or_default();
    assert!(
        content_type.starts_with("text/html"),
        "expected text/html, got {content_type:?}"
    );
    let mut resp = resp;
    let body = resp.body_mut().read_to_string().expect("read body");
    assert!(
        body.contains("npm run build"),
        "body should mention npm run build, got: {body}"
    );
}

#[test]
fn spa_route_falls_back_to_html() {
    let server = common::spawn();
    let resp = ureq::get(&format!("{}/some/spa/route", server.base))
        .call()
        .expect("GET /some/spa/route should succeed");
    assert_eq!(resp.status().as_u16(), 200);
    let content_type = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap_or_default().to_string())
        .unwrap_or_default();
    assert!(
        content_type.starts_with("text/html"),
        "expected text/html, got {content_type:?}"
    );
}

#[test]
fn api_routes_still_work_under_static_fallback() {
    let server = common::spawn();
    let resp = ureq::get(&format!("{}/api/projects", server.base))
        .call()
        .expect("GET /api/projects should succeed");
    assert_eq!(resp.status().as_u16(), 200);
    let content_type = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap_or_default().to_string())
        .unwrap_or_default();
    assert!(
        content_type.starts_with("application/json"),
        "expected application/json, got {content_type:?}"
    );
    let mut resp = resp;
    let body: serde_json::Value = resp.body_mut().read_json().expect("read json");
    assert!(body.is_array(), "expected JSON array, got {body}");
}

/// With `embed-frontend` enabled and `frontend/dist` present in the tree,
/// a real embedded asset must be served with its correct content type, and
/// an unknown SPA route must fall back to the real embedded `index.html`
/// (not the `NOT_BUILT_HTML` placeholder).
#[cfg(feature = "embed-frontend")]
#[test]
fn embedded_asset_hit_returns_real_content_type() {
    // Find a real built asset filename (hashed by Vite, so discover it
    // rather than hardcoding).
    let assets_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/dist/assets");
    let entry = std::fs::read_dir(&assets_dir)
        .expect("frontend/dist/assets should exist for embed-frontend tests")
        .filter_map(|e| e.ok())
        .find(|e| e.path().extension().and_then(|s| s.to_str()) == Some("js"))
        .expect("expected at least one built .js asset");
    let filename = entry.file_name().into_string().unwrap();

    let server = common::spawn();
    let resp = ureq::get(&format!("{}/assets/{}", server.base, filename))
        .call()
        .expect("GET asset should succeed");
    assert_eq!(resp.status().as_u16(), 200);
    let content_type = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap_or_default().to_string())
        .unwrap_or_default();
    assert!(
        content_type.starts_with("text/javascript"),
        "expected text/javascript, got {content_type:?}"
    );
}

#[cfg(feature = "embed-frontend")]
#[test]
fn unknown_route_falls_back_to_real_index_html() {
    let index_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/dist/index.html");
    let expected_body = std::fs::read_to_string(&index_path).expect("read frontend/dist/index.html");

    let server = common::spawn();
    let resp = ureq::get(&format!("{}/some/spa/route", server.base))
        .call()
        .expect("GET /some/spa/route should succeed");
    assert_eq!(resp.status().as_u16(), 200);
    let mut resp = resp;
    let body = resp.body_mut().read_to_string().expect("read body");
    assert_eq!(body, expected_body, "expected the real embedded index.html");
    assert!(
        !body.contains("npm run build"),
        "should not fall back to NOT_BUILT_HTML when frontend is embedded"
    );
}
