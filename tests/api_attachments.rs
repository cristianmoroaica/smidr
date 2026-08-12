//! Black-box coverage for project-scoped browser image uploads.

mod common;

use serde_json::{json, Value};

const TINY_PNG: &[u8] = b"\x89PNG\r\n\x1a\ncontext";

fn create_project(base: &str, name: &str) -> String {
    let resp = ureq::post(&format!("{base}/api/projects"))
        .send_json(json!({"name": name}))
        .expect("create project should succeed");
    let mut resp = resp;
    let body: Value = resp.body_mut().read_json().unwrap();
    body["id"].as_str().unwrap().to_string()
}

#[test]
fn valid_image_is_staged_inside_the_project() {
    let server = common::spawn();
    let id = create_project(&server.base, "widget");
    let mut resp = ureq::post(&format!(
        "{}/api/projects/{}/attachments?filename=context.png",
        server.base, id
    ))
    .header("content-type", "image/png")
    .send(TINY_PNG)
    .expect("upload should succeed");

    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.body_mut().read_json().unwrap();
    let attachment_id = body["id"].as_str().expect("attachment id");
    assert_eq!(body["name"], "context.png");
    let staged = server
        .home
        .path()
        .join("Smidr/widget/.attachments")
        .join(attachment_id);
    assert_eq!(std::fs::read(staged).unwrap(), TINY_PNG);
}

#[test]
fn forged_image_content_is_rejected() {
    let server = common::spawn();
    let id = create_project(&server.base, "widget");
    let mut resp = ureq::post(&format!(
        "{}/api/projects/{}/attachments?filename=context.png",
        server.base, id
    ))
    .config()
    .http_status_as_error(false)
    .build()
    .header("content-type", "image/png")
    .send(b"this is not a png" as &[u8])
    .expect("request should complete");

    assert_eq!(resp.status().as_u16(), 415);
    let body: Value = resp.body_mut().read_json().unwrap();
    assert!(body["error"].as_str().unwrap().contains("does not match"));
}
