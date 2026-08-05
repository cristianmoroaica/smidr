//! `GET /api/artifacts/{project}/{file}` — serves per-iteration GLB exports
//! and their manifests from the open session's directory (Phase 3 Task 3.1).
//!
//! Only files matching `iteration_<digits>.glb` or
//! `iteration_<digits>.manifest.json` are ever served, and only from the
//! `AppCore` already open in `ServerState::cores` for `project` — no lazy
//! core creation and no path segment beyond the validated basename is ever
//! joined onto a filesystem path.
//!
//! New iteration artifacts land in `<session>/iterations/`; existing
//! sessions still have theirs at the session root. Both locations are
//! checked — `iterations/` first, falling back to the session root — so
//! lookups and discovery work transparently across old and new sessions.

use std::path::{Path, PathBuf};

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde_json::json;

use crate::server::routes::is_valid_project_name;
use crate::server::SharedState;

pub fn router(state: SharedState) -> Router {
    Router::new()
        .route("/api/artifacts/{project}/{file}", get(get_artifact))
        .with_state(state)
}

fn error_response(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(json!({"error": message}))).into_response()
}

/// Kind of artifact file a validated `iteration_<n>.*` name refers to.
enum ArtifactKind {
    Glb,
    Manifest,
}

/// Parse `iteration_<digits>.glb` or `iteration_<digits>.manifest.json`.
/// Returns `None` for anything else — including empty or non-ASCII-digit
/// iteration numbers.
fn parse_artifact_file(file: &str) -> Option<ArtifactKind> {
    let rest = file.strip_prefix("iteration_")?;
    let (digits, kind) = if let Some(d) = rest.strip_suffix(".glb") {
        (d, ArtifactKind::Glb)
    } else if let Some(d) = rest.strip_suffix(".manifest.json") {
        (d, ArtifactKind::Manifest)
    } else {
        return None;
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(kind)
}

async fn get_artifact(
    State(state): State<SharedState>,
    AxumPath((project, file)): AxumPath<(String, String)>,
) -> Response {
    if !is_valid_project_name(&project) {
        return error_response(StatusCode::BAD_REQUEST, "invalid project id");
    }
    let project = project.trim();

    let kind = match parse_artifact_file(&file) {
        Some(k) => k,
        None => return error_response(StatusCode::NOT_FOUND, "artifact not found"),
    };

    let session_dir = {
        let guard = match state.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        match guard.cores.get(project).and_then(|c| c.session_dir()) {
            Some(dir) => dir.to_path_buf(),
            None => return error_response(StatusCode::NOT_FOUND, "artifact not found"),
        }
    };

    let path = match resolve_artifact_path(&session_dir, &file) {
        Some(p) => p,
        None => return error_response(StatusCode::NOT_FOUND, "artifact not found"),
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => return error_response(StatusCode::NOT_FOUND, "artifact not found"),
    };

    let content_type = match kind {
        ArtifactKind::Glb => "model/gltf-binary",
        ArtifactKind::Manifest => "application/json",
    };

    (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, content_type)], bytes).into_response()
}

/// Resolve `file` (an already-`parse_artifact_file`-validated basename)
/// against a session directory, preferring `<session>/iterations/<file>`
/// and falling back to `<session>/<file>` (the legacy location). Returns
/// `None` if neither exists.
fn resolve_artifact_path(session_dir: &Path, file: &str) -> Option<PathBuf> {
    let in_iterations = session_dir.join("iterations").join(file);
    if in_iterations.is_file() {
        return Some(in_iterations);
    }
    let in_root = session_dir.join(file);
    if in_root.is_file() {
        return Some(in_root);
    }
    None
}

/// Scan a single directory (non-recursively) for `iteration_<digits>.glb`
/// files, returning the parsed digits as `u32`s. Returns an empty vec if
/// `dir` is missing or unreadable.
fn scan_glb_iterations(dir: &Path) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            let digits = name.strip_prefix("iteration_")?.strip_suffix(".glb")?;
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            digits.parse::<u32>().ok()
        })
        .collect()
}

/// Scan `dir` (a session directory) for `iteration_<digits>.glb` files in
/// both `dir/iterations` and `dir` itself, returning the merged, parsed
/// digits as `u32`s, ascending and deduped (a number present in both
/// locations appears once). Returns an empty vec if `dir` is `None` or
/// unreadable.
pub fn glb_iterations(dir: Option<&Path>) -> Vec<u32> {
    let Some(dir) = dir else {
        return Vec::new();
    };

    let mut iterations = scan_glb_iterations(dir);
    iterations.extend(scan_glb_iterations(&dir.join("iterations")));

    iterations.sort_unstable();
    iterations.dedup();
    iterations
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_artifact_file_accepts_glb_and_manifest() {
        assert!(matches!(parse_artifact_file("iteration_001.glb"), Some(ArtifactKind::Glb)));
        assert!(matches!(
            parse_artifact_file("iteration_042.manifest.json"),
            Some(ArtifactKind::Manifest)
        ));
    }

    #[test]
    fn parse_artifact_file_rejects_garbage() {
        assert!(parse_artifact_file("evil.txt").is_none());
        assert!(parse_artifact_file("iteration_.glb").is_none());
        assert!(parse_artifact_file("iteration_abc.glb").is_none());
        assert!(parse_artifact_file("iteration_1.glb.bak").is_none());
        assert!(parse_artifact_file("iteration_1x.manifest.json").is_none());
        assert!(parse_artifact_file("iteration_-1.glb").is_none());
    }

    #[test]
    fn glb_iterations_none_dir_is_empty() {
        assert_eq!(glb_iterations(None), Vec::<u32>::new());
    }

    #[test]
    fn glb_iterations_unreadable_dir_is_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert_eq!(glb_iterations(Some(&missing)), Vec::<u32>::new());
    }

    #[test]
    fn glb_iterations_sorted_and_deduped() {
        let dir = tempfile::TempDir::new().unwrap();
        for name in ["iteration_003.glb", "iteration_001.glb", "iteration_002.glb", "iteration_001.glb.tmp"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        // Non-GLB and manifest files must be ignored by this scanner.
        std::fs::write(dir.path().join("iteration_001.manifest.json"), b"{}").unwrap();
        std::fs::write(dir.path().join("evil.txt"), b"x").unwrap();

        assert_eq!(glb_iterations(Some(dir.path())), vec![1, 2, 3]);
    }

    #[test]
    fn glb_iterations_rejects_non_digit_names() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("iteration_abc.glb"), b"x").unwrap();
        std::fs::write(dir.path().join("iteration_.glb"), b"x").unwrap();
        assert_eq!(glb_iterations(Some(dir.path())), Vec::<u32>::new());
    }

    #[test]
    fn glb_iterations_merges_root_and_iterations_subdir() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("iteration_001.glb"), b"x").unwrap();
        std::fs::create_dir(dir.path().join("iterations")).unwrap();
        std::fs::write(dir.path().join("iterations").join("iteration_002.glb"), b"x").unwrap();

        assert_eq!(glb_iterations(Some(dir.path())), vec![1, 2]);
    }

    #[test]
    fn glb_iterations_dedupes_number_present_in_both_locations() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("iteration_001.glb"), b"root").unwrap();
        std::fs::create_dir(dir.path().join("iterations")).unwrap();
        std::fs::write(dir.path().join("iterations").join("iteration_001.glb"), b"new").unwrap();

        assert_eq!(glb_iterations(Some(dir.path())), vec![1]);
    }

    #[test]
    fn resolve_artifact_path_prefers_iterations_subdir() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("iteration_001.glb"), b"root").unwrap();
        std::fs::create_dir(dir.path().join("iterations")).unwrap();
        std::fs::write(dir.path().join("iterations").join("iteration_001.glb"), b"new").unwrap();

        let resolved = resolve_artifact_path(dir.path(), "iteration_001.glb").unwrap();
        assert_eq!(resolved, dir.path().join("iterations").join("iteration_001.glb"));
    }

    #[test]
    fn resolve_artifact_path_falls_back_to_root() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("iteration_001.glb"), b"root").unwrap();

        let resolved = resolve_artifact_path(dir.path(), "iteration_001.glb").unwrap();
        assert_eq!(resolved, dir.path().join("iteration_001.glb"));
    }

    #[test]
    fn resolve_artifact_path_none_when_missing_everywhere() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(resolve_artifact_path(dir.path(), "iteration_001.glb").is_none());
    }
}
