mod common;

#[test]
fn health_reports_build_identity_and_frontend_mode() {
    let server = common::spawn();
    let mut response = ureq::get(&format!("{}/api/health", server.base))
        .call()
        .expect("GET /api/health should succeed");
    let body: serde_json::Value = response.body_mut().read_json().expect("health JSON");

    assert_eq!(body["status"], "ok");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    assert!(body["build_id"].as_str().is_some_and(|id| !id.is_empty()));
    assert_eq!(body["os"], std::env::consts::OS);
    assert_eq!(body["arch"], std::env::consts::ARCH);
    assert_eq!(body["frontend_embedded"], cfg!(feature = "embed-frontend"));
}
