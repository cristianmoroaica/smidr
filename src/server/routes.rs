//! `/api/projects` REST routes (Task 2.1).

use crate::core::AppCore;
use crate::server::SharedState;
use crate::storage;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub fn router(state: SharedState) -> Router {
    Router::new()
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/{id}", axum::routing::delete(delete_project))
        .route("/api/projects/{id}/export", post(export_project))
        .route("/api/projects/{id}/export/{file}", get(get_export_file))
        .route("/api/projects/{id}/open-folder", post(open_folder))
        .route("/api/projects/{id}/baseline", post(set_baseline))
        .route("/api/refs", get(list_refs))
        .with_state(state.clone())
        // Task 2.2: WebSocket session channel, defined in `server::ws`.
        .merge(crate::server::ws::router(state.clone()))
        // Task 3.1: per-iteration GLB/manifest artifact downloads.
        .merge(crate::server::artifacts::router(state.clone()))
        // Task 2.3: embedded frontend static assets / SPA fallback. Placed
        // last via `.fallback` so it never shadows `/api/*` or the
        // `/api/session` WebSocket route above.
        .fallback(crate::server::assets::static_handler)
}

#[derive(Debug, Serialize)]
struct ProjectSummary {
    id: String,
    name: String,
    created: String,
    phase: String,
}

fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

async fn list_projects(State(_state): State<SharedState>) -> Response {
    if let Err(e) = storage::project::ensure_root() {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, e);
    }

    let projects = match storage::project::list_projects() {
        Ok(p) => p,
        Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, e),
    };

    let summaries: Vec<ProjectSummary> = projects
        .into_iter()
        .map(|p| {
            let id = p
                .path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            // Pick the most recently created session (by session.json's
            // `created` timestamp, RFC3339-sortable), not the
            // alphabetically-last one `list_projects` happens to hand back.
            let phase = p
                .sessions
                .iter()
                .filter_map(|si| {
                    let session_path = p.path.join(&si.name);
                    match storage::session::session_status(&session_path) {
                        storage::session::SessionStatus::Ok { phase, created } => {
                            Some((created, phase))
                        }
                        _ => None,
                    }
                })
                .max_by(|a, b| a.0.cmp(&b.0))
                .map(|(_, phase)| phase)
                .unwrap_or_else(|| "Spec".to_string());
            ProjectSummary {
                id,
                name: p.meta.name,
                created: p.meta.created,
                phase,
            }
        })
        .collect();

    Json(summaries).into_response()
}

#[derive(Debug, Serialize)]
struct RefSummary {
    slug: String,
    name: String,
    category: String,
}

/// `GET /api/refs` — lists the reference library the same way the (removed)
/// TUI `/ref list` command did, so the frontend's ref picker (Task 4.1) has
/// something to autocomplete against.
async fn list_refs(State(_state): State<SharedState>) -> Response {
    let library = match crate::reference::load_library() {
        Ok(l) => l,
        Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, e),
    };

    let mut summaries: Vec<RefSummary> = library
        .into_iter()
        .map(|(comp, slug)| RefSummary {
            slug,
            name: comp.identity.name,
            category: comp.identity.category,
        })
        .collect();
    summaries.sort_by(|a, b| a.slug.cmp(&b.slug));

    Json(summaries).into_response()
}

#[derive(Debug, Deserialize)]
struct CreateProjectRequest {
    name: String,
}

/// Validate a project name/id that will become a literal directory segment
/// under `storage::project::root_dir()`. Rejects anything empty, containing
/// path separators or `..`, starting with `.` (hidden dirs `list_projects`
/// filters out), or the reserved `references` library directory name.
///
/// Applied identically to POST (name) and DELETE (id) — DELETE previously
/// skipped this check entirely, allowing a percent-encoded `..%2Ffoo`
/// segment (axum's `Path<String>` percent-decodes it back to `../foo`) to
/// escape `root_dir()` and recursively delete an arbitrary directory.
pub(crate) fn is_valid_project_name(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains("..") {
        return false;
    }
    if trimmed.starts_with('.') || trimmed == "references" {
        return false;
    }
    true
}

async fn create_project(
    State(_state): State<SharedState>,
    Json(req): Json<CreateProjectRequest>,
) -> Response {
    if !is_valid_project_name(&req.name) {
        return error_response(StatusCode::BAD_REQUEST, "invalid project name");
    }
    let name = req.name.trim();

    let project_dir = storage::project::root_dir().join(name);
    if project_dir.exists() {
        return error_response(StatusCode::CONFLICT, "project already exists");
    }

    match storage::project::create_project(name, "") {
        Ok(_) => Json(json!({ "id": name })).into_response(),
        Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn delete_project(State(state): State<SharedState>, Path(id): Path<String>) -> Response {
    if !is_valid_project_name(&id) {
        return error_response(StatusCode::BAD_REQUEST, "invalid project id");
    }
    let id = id.trim();

    let root = storage::project::root_dir();
    let project_dir = root.join(id);
    if !project_dir.exists() {
        return error_response(StatusCode::NOT_FOUND, "project not found");
    }
    // Belt-and-suspenders: even with `is_valid_project_name` passing,
    // confirm the resolved path's parent really is `root_dir()` before
    // deleting anything.
    if project_dir.parent() != Some(root.as_path()) {
        return error_response(StatusCode::BAD_REQUEST, "invalid project id");
    }

    if let Err(e) = storage::project::delete_project(id) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, e);
    }

    if let Ok(mut state) = state.lock() {
        state.cores.remove(id);
    }

    StatusCode::NO_CONTENT.into_response()
}

/// Make sure `core` has a project opened, WITHOUT creating anything on
/// disk. If this core was just lazily created by `core_for` and has never
/// been opened (`session_dir()` is `None`), load the project — picking up
/// its latest existing session, if any. This is read-only: it must never
/// fall back to lazily creating a session directory, or a purely
/// resolve-and-inspect action like "open this project's folder" would
/// materialize a phantom `session/` directory (and `session.json`) as a
/// side effect. Handlers that need `session_dir()` populated are expected
/// to 404 when it is still `None` after this call. A no-op when a session
/// is already active (e.g. this core is being driven live over the
/// WebSocket channel) so it never clobbers in-progress conversation/phase
/// state.
fn ensure_project_open(core: &mut AppCore, project_id: &str) {
    if core.session_dir().is_none() {
        let _ = core.open_project_by_id(project_id);
    }
}

/// Percent-encode `s` for use as a single URL path segment (RFC 3986
/// unreserved set kept verbatim, every other byte `%XX`-escaped).
///
/// Needed because `is_valid_project_name` only rejects path separators,
/// `..`, a leading `.` and `references` — so `?`, `#`, `%` and spaces are
/// all creatable project ids. Interpolating such an id raw into a download
/// URL yields e.g. `/api/projects/a?b/export/export.stl`, which a browser
/// resolves as path `/api/projects/a` with query `b/export/export.stl`, and
/// the user's STL download silently fails. Every client-side URL in the
/// frontend already uses `encodeURIComponent`; this is the server-generated
/// counterpart. Kept as a local helper so no new dependency is pulled in.
fn encode_path_segment(s: &str) -> String {
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

/// `POST /api/projects/{id}/export` — copies the latest iteration's
/// geometry into the session dir as `export.stl` (and `export.step` when a
/// `_buffer.step` exists), returning download URLs for the files actually
/// written. 404 when there is no session or nothing to export.
async fn export_project(State(state): State<SharedState>, Path(id): Path<String>) -> Response {
    if !is_valid_project_name(&id) {
        return error_response(StatusCode::BAD_REQUEST, "invalid project id");
    }
    let id = id.trim();

    let mut guard = match state.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    let core = match guard.core_for(id) {
        Ok(c) => c,
        Err(e) => return error_response(StatusCode::NOT_FOUND, e),
    };
    ensure_project_open(core, id);

    match core.export_artifacts() {
        Ok((dir, files)) => {
            let encoded_id = encode_path_segment(id);
            let files_json: Vec<Value> = files
                .iter()
                .map(|name| {
                    json!({
                        "name": name,
                        "url": format!("/api/projects/{encoded_id}/export/{name}"),
                    })
                })
                .collect();
            Json(json!({
                "dir": dir.to_string_lossy(),
                "files": files_json,
            }))
            .into_response()
        }
        Err(e) => error_response(StatusCode::NOT_FOUND, e),
    }
}

/// `GET /api/projects/{id}/export/{file}` — serves a previously exported
/// artifact. `file` must be exactly `export.stl` or `export.step`; no other
/// value is ever joined onto a filesystem path (mirrors the strictness of
/// `server::artifacts::get_artifact`). Reads the already-open core only (no
/// lazy project-opening) — same as `artifacts::get_artifact`.
///
/// Known inconsistency, deliberately left as-is: if the server process is
/// restarted (or this project's `AppCore` is otherwise evicted from
/// `ServerState.cores`) after `export.stl`/`export.step` were written to
/// disk, this handler 404s on a file that genuinely still exists, because
/// there is no cached core to resolve the session dir from. In practice
/// this is harmless — the frontend always `POST`s `/export` immediately
/// before following either download URL, which re-opens the core as a side
/// effect — but a page reload replaying a stored download link across a
/// server restart would hit this. Not fixed here to avoid giving a plain
/// file download an implicit, unauthenticated "open any project" lazy-load
/// path; if it becomes a real problem, prefer re-deriving the session dir
/// straight from `storage::project` without materializing a full `AppCore`.
async fn get_export_file(
    State(state): State<SharedState>,
    Path((id, file)): Path<(String, String)>,
) -> Response {
    if !is_valid_project_name(&id) {
        return error_response(StatusCode::BAD_REQUEST, "invalid project id");
    }
    let id = id.trim();

    if file != "export.stl" && file != "export.step" {
        return error_response(StatusCode::NOT_FOUND, "export file not found");
    }

    let session_dir = {
        let guard = match state.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        match guard.cores.get(id).and_then(|c| c.session_dir()) {
            Some(dir) => dir.to_path_buf(),
            None => return error_response(StatusCode::NOT_FOUND, "export file not found"),
        }
    };

    let path = session_dir.join(&file);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => return error_response(StatusCode::NOT_FOUND, "export file not found"),
    };

    (
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{file}\""),
            ),
        ],
        bytes,
    )
        .into_response()
}

/// `POST /api/projects/{id}/open-folder` — resolves the project's current
/// session directory and asks the desktop file manager to open it via
/// `xdg-open`, detached (spawn errors are ignored; the response is 200
/// regardless of whether a file manager is actually available). Safe to
/// expose unauthenticated because the server always binds `127.0.0.1` on
/// the user's own machine, never reachable from another host.
///
/// Test hook: set the environment variable `SMIDR_NO_OPEN=1` to skip the
/// `xdg-open` spawn entirely — used by the sandboxed-HOME test harness,
/// which must not pop file-manager windows while running `cargo test`.
async fn open_folder(State(state): State<SharedState>, Path(id): Path<String>) -> Response {
    if !is_valid_project_name(&id) {
        return error_response(StatusCode::BAD_REQUEST, "invalid project id");
    }
    let id = id.trim();

    let dir = {
        let mut guard = match state.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        let core = match guard.core_for(id) {
            Ok(c) => c,
            Err(e) => return error_response(StatusCode::NOT_FOUND, e),
        };
        ensure_project_open(core, id);
        match core.session_dir() {
            Some(dir) => dir.to_path_buf(),
            None => return error_response(StatusCode::NOT_FOUND, "no session"),
        }
    };

    if std::env::var("SMIDR_NO_OPEN").as_deref() != Ok("1") {
        let _ = std::process::Command::new("xdg-open")
            .arg(&dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    Json(json!({"path": dir.to_string_lossy()})).into_response()
}

/// `POST /api/projects/{id}/baseline` — locks the given iteration number in
/// as the Refine-phase ghost-diff baseline. Body: `{"n": <u32>}`.
async fn set_baseline(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if !is_valid_project_name(&id) {
        return error_response(StatusCode::BAD_REQUEST, "invalid project id");
    }
    let id = id.trim();

    let n = match body.get("n").and_then(|v| v.as_u64()).and_then(|v| u32::try_from(v).ok()) {
        Some(n) => n,
        None => return error_response(StatusCode::BAD_REQUEST, "missing or invalid 'n'"),
    };

    let mut guard = match state.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    let core = match guard.core_for(id) {
        Ok(c) => c,
        Err(e) => return error_response(StatusCode::NOT_FOUND, e),
    };
    ensure_project_open(core, id);
    if core.session_dir().is_none() {
        return error_response(StatusCode::NOT_FOUND, "no session");
    }
    core.set_baseline_iteration(n);

    Json(json!({"baseline_iteration": n})).into_response()
}
