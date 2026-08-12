//! Static asset serving for the embedded Svelte frontend (Task 2.3).
//!
//! Behind the `embed-frontend` feature, the built `frontend/dist` directory
//! is embedded into the binary via `rust-embed` and served as a SPA
//! (unknown paths fall back to `index.html`). Without the feature (the
//! default), `lookup` always misses and `static_handler` serves a
//! `NOT_BUILT_HTML` page explaining how to build and enable it — this file
//! must compile and its tests must pass whether or not `frontend/dist`
//! exists on disk.

use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};

/// Runtime contract used by the Electron shell. A desktop package refuses to
/// open a backend that was built without the matching frontend.
pub const FRONTEND_EMBEDDED: bool = cfg!(feature = "embed-frontend");

pub const BUILD_ID: &str = match option_env!("SMIDR_BUILD_ID") {
    Some(id) => id,
    None => env!("CARGO_PKG_VERSION"),
};

/// Minimal standalone page shown when the frontend has not been built /
/// embedded (default build, no `embed-frontend` feature).
pub const NOT_BUILT_HTML: &str = r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>Smiðr</title></head>
<body>
<h1>Smiðr — frontend not built</h1>
<p>The web UI has not been built into this binary. Build it first:</p>
<pre>cd frontend && npm install && npm run build</pre>
<p>Then rebuild smidr with <code>--features embed-frontend</code>.</p>
</body>
</html>
"#;

/// Map a file extension to a content type. Defaults to
/// `application/octet-stream` for anything unrecognized.
///
/// Only used by the `embed-frontend` `lookup` below; the default build
/// never calls it (`lookup` always misses), hence the `allow`.
#[cfg_attr(not(feature = "embed-frontend"), allow(dead_code))]
fn content_type_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "wasm" => "application/wasm",
        "woff2" => "font/woff2",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

#[cfg(feature = "embed-frontend")]
#[derive(rust_embed::RustEmbed)]
#[folder = "frontend/dist"]
struct Assets;

#[cfg(feature = "embed-frontend")]
pub fn lookup(path: &str) -> Option<(Vec<u8>, &'static str)> {
    let file = Assets::get(path)?;
    Some((file.data.into_owned(), content_type_for(path)))
}

#[cfg(not(feature = "embed-frontend"))]
pub fn lookup(_path: &str) -> Option<(Vec<u8>, &'static str)> {
    None
}

/// Fallback handler: serves embedded static assets, falling back to
/// embedded `index.html` for SPA client-side routes, and to
/// `NOT_BUILT_HTML` when nothing is embedded at all.
pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    if let Some((bytes, content_type)) = lookup(path) {
        return (StatusCode::OK, [("content-type", content_type)], bytes).into_response();
    }

    // SPA fallback: unknown path, but index.html is embedded.
    if let Some((bytes, _)) = lookup("index.html") {
        return (
            StatusCode::OK,
            [("content-type", "text/html; charset=utf-8")],
            bytes,
        )
            .into_response();
    }

    // Nothing embedded at all (default build without embed-frontend).
    (
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        NOT_BUILT_HTML,
    )
        .into_response()
}
