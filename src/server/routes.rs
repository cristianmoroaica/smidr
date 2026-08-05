//! `/api/projects` REST routes (Task 2.1).

use crate::server::SharedState;
use crate::storage;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub fn router(state: SharedState) -> Router {
    Router::new()
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/{id}", axum::routing::delete(delete_project))
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
