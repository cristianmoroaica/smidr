//! Project directory CRUD operations.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub name: String,
    pub created: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Project {
    pub path: PathBuf,
    pub meta: ProjectMeta,
    pub sessions: Vec<SessionInfo>, // session directory names
}

/// Get the root storage directory: ~/Smidr/
pub fn root_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Smidr")
}

/// The pre-rebrand storage root (~/MiModel/), kept only so `ensure_root` can
/// perform a one-shot migration to ~/Smidr/ for existing installs.
fn legacy_root_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("MiModel")
}

/// One-shot migration of a pre-rebrand ~/MiModel/ to ~/Smidr/. Must run
/// before anything else touches the storage root: `create_project` and
/// `reference::ensure_references_dir` create ~/Smidr/ directly, and once it
/// exists the migration is permanently skipped, stranding ~/MiModel. The
/// server calls this once at startup; `ensure_root` also calls it for
/// defense in depth. Idempotent.
pub fn migrate_legacy_root() -> Result<(), String> {
    let root = root_dir();
    if !root.exists() {
        let legacy = legacy_root_dir();
        if legacy.exists() {
            std::fs::rename(&legacy, &root)
                .map_err(|e| format!("Failed to migrate ~/MiModel to ~/Smidr: {e}"))?;
        }
    }
    Ok(())
}

/// Ensure ~/Smidr/ exists. Creates with a default "Untitled" project if missing.
/// If a pre-rebrand ~/MiModel/ exists and ~/Smidr/ does not, migrates it in place.
pub fn ensure_root() -> Result<PathBuf, String> {
    migrate_legacy_root()?;
    let root = root_dir();
    if !root.exists() {
        std::fs::create_dir_all(&root)
            .map_err(|e| format!("Failed to create ~/Smidr/: {e}"))?;
        create_project("Untitled", "")?;
    }
    Ok(root)
}

/// List all projects in ~/Smidr/.
pub fn list_projects() -> Result<Vec<Project>, String> {
    let root = root_dir();
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut projects = Vec::new();
    let entries = std::fs::read_dir(&root)
        .map_err(|e| format!("Failed to read ~/Smidr/: {e}"))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() { continue; }

        // Skip non-project directories (references library, etc.)
        let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
        if dir_name == "references" || dir_name.starts_with('.') {
            continue;
        }

        let meta_path = path.join("project.json");
        let meta = if meta_path.exists() {
            let json = std::fs::read_to_string(&meta_path).unwrap_or_default();
            serde_json::from_str(&json).unwrap_or(ProjectMeta {
                name: path.file_name().unwrap_or_default().to_string_lossy().to_string(),
                created: String::new(),
                description: String::new(),
            })
        } else {
            ProjectMeta {
                name: path.file_name().unwrap_or_default().to_string_lossy().to_string(),
                created: String::new(),
                description: String::new(),
            }
        };

        // List session subdirectories
        let mut sessions = Vec::new();
        if let Ok(sub_entries) = std::fs::read_dir(&path) {
            for sub in sub_entries.flatten() {
                let sub_path = sub.path();
                if sub_path.is_dir() && sub_path.join("session.json").exists() {
                    if let Some(name) = sub_path.file_name() {
                        sessions.push(SessionInfo { name: name.to_string_lossy().to_string() });
                    }
                }
            }
        }
        sessions.sort_by(|a, b| a.name.cmp(&b.name));

        projects.push(Project { path, meta, sessions });
    }

    projects.sort_by(|a, b| a.meta.name.cmp(&b.meta.name));
    Ok(projects)
}

/// Create a new project directory with project.json.
pub fn create_project(name: &str, description: &str) -> Result<PathBuf, String> {
    let path = root_dir().join(name);
    std::fs::create_dir_all(&path)
        .map_err(|e| format!("Failed to create project dir: {e}"))?;

    let meta = ProjectMeta {
        name: name.to_string(),
        created: chrono::Utc::now().to_rfc3339(),
        description: description.to_string(),
    };
    let json = serde_json::to_string_pretty(&meta)
        .map_err(|e| format!("Failed to serialize project: {e}"))?;
    std::fs::write(path.join("project.json"), json)
        .map_err(|e| format!("Failed to write project.json: {e}"))?;

    Ok(path)
}

/// Delete a project and all its sessions.
pub fn delete_project(name: &str) -> Result<(), String> {
    let path = root_dir().join(name);
    if path.exists() {
        std::fs::remove_dir_all(&path)
            .map_err(|e| format!("Failed to delete project: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    use crate::test_util::HOME_LOCK;

    fn with_test_root(f: impl FnOnce()) {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());
        f();
        // Restore rather than unset: an unset HOME makes `dirs::home_dir()`
        // fall back to the passwd entry, i.e. the developer's real ~/Smidr.
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn test_ensure_root_creates_default_project() {
        with_test_root(|| {
            let root = ensure_root().unwrap();
            assert!(root.exists());
            assert!(root.join("Untitled/project.json").exists());
        });
    }

    #[test]
    fn test_create_and_list_projects() {
        with_test_root(|| {
            ensure_root().unwrap();
            create_project("Test Project", "A test").unwrap();
            let projects = list_projects().unwrap();
            assert!(projects.iter().any(|p| p.meta.name == "Test Project"));
        });
    }

    #[test]
    fn test_delete_project() {
        with_test_root(|| {
            ensure_root().unwrap();
            create_project("ToDelete", "").unwrap();
            delete_project("ToDelete").unwrap();
            let projects = list_projects().unwrap();
            assert!(!projects.iter().any(|p| p.meta.name == "ToDelete"));
        });
    }

    #[test]
    fn test_ensure_root_migrates_legacy_mimodel_dir() {
        with_test_root(|| {
            let home = dirs::home_dir().unwrap();
            std::fs::create_dir_all(home.join("MiModel/SomeProj")).unwrap();
            std::fs::write(
                home.join("MiModel/SomeProj/project.json"),
                r#"{"name":"SomeProj","created":"","description":""}"#,
            ).unwrap();

            ensure_root().unwrap();

            assert!(home.join("Smidr/SomeProj/project.json").exists());
            assert!(!home.join("MiModel").exists());
            assert!(!home.join("Smidr/Untitled").exists());
        });
    }

    #[test]
    fn test_ensure_root_is_idempotent_after_migration() {
        with_test_root(|| {
            let home = dirs::home_dir().unwrap();
            std::fs::create_dir_all(home.join("MiModel/SomeProj")).unwrap();
            std::fs::write(
                home.join("MiModel/SomeProj/project.json"),
                r#"{"name":"SomeProj","created":"","description":""}"#,
            ).unwrap();

            ensure_root().unwrap();
            ensure_root().unwrap();

            assert!(home.join("Smidr/SomeProj/project.json").exists());
        });
    }

    #[test]
    fn test_session_info_detection() {
        with_test_root(|| {
            ensure_root().unwrap();
            let project_path = create_project("TestProj", "").unwrap();

            // Create a new-format session
            let new_dir = project_path.join("new_session");
            std::fs::create_dir_all(&new_dir).unwrap();
            std::fs::write(
                new_dir.join("session.json"),
                r#"{"name":"new","created":"2026-03-16","phase":"Spec","current_component":null,"claude_sessions":{},"conversations":{},"component_states":[]}"#
            ).unwrap();

            let projects = list_projects().unwrap();
            let proj = projects.iter().find(|p| p.meta.name == "TestProj").unwrap();

            let session = proj.sessions.iter().find(|s| s.name == "new_session").unwrap();
            assert_eq!(session.name, "new_session");
        });
    }
}
