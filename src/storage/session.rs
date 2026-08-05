//! Session directory CRUD and serialization.

use crate::component::ComponentState;
use crate::phase::Phase;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Per-scope Claude session ID storage
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClaudeSessionMap {
    pub spec: Option<String>,
    pub decompose: Option<String>,
    #[serde(default)]
    pub components: HashMap<String, String>, // component_id -> session_id
}

/// New session.json format for phase-machine sessions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseSessionData {
    pub name: String,
    pub created: String,
    pub phase: Phase,
    pub current_component: Option<String>,
    pub claude_sessions: ClaudeSessionMap,
    pub conversations: HashMap<String, Vec<ConversationEntry>>,
    pub component_states: Vec<ComponentState>,
    #[serde(default)]
    pub briefing: Option<String>,
    /// Per-phase server-authoritative approval gate (Task 2.2). Key is
    /// `Phase::label()` ("Spec"/"Build"/"Refine"). Old session.json files
    /// predate this field and deserialize to an empty map — i.e. every
    /// phase unapproved, which is the safe default.
    #[serde(default)]
    pub approved: HashMap<String, bool>,
    /// A question awaiting a user answer, surfaced as an interactive card
    /// by the web client. Old session.json files predate this field and
    /// deserialize to `None`.
    #[serde(default)]
    pub pending_question: Option<PendingQuestion>,
    /// A phase change the agent asked the user to make, awaiting the user's
    /// explicit consent (or denial) in the web UI. Old session.json files
    /// predate this field and deserialize to `None`.
    #[serde(default)]
    pub pending_phase_switch: Option<PendingPhaseSwitch>,
    /// The iteration number locked as the Refine-phase ghost-diff baseline
    /// when the user chose "Lock as baseline & refine" in the Build-phase
    /// approve modal. Old session.json files predate this field and
    /// deserialize to `None`.
    #[serde(default)]
    pub baseline_iteration: Option<u32>,
}

/// A single conversation message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationEntry {
    pub role: String,
    pub content: String,
}

/// A question surfaced by `ask_question`/`ask_clarification` awaiting a
/// user answer, with optional suggested-answer chips.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingQuestion {
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
}

/// A phase change the agent asked the user to make, awaiting the user's
/// explicit consent (or denial) in the web UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingPhaseSwitch {
    pub target: String,
    pub reason: String,
}

/// Create a new session directory inside a project.
// Currently unused by the TUI; domain model the upcoming web layer will need.
#[allow(dead_code)]
pub fn create_session(project_path: &Path, name: &str) -> Result<PathBuf, String> {
    let path = project_path.join(name);
    std::fs::create_dir_all(&path)
        .map_err(|e| format!("Failed to create session dir: {e}"))?;
    Ok(path)
}

/// Return the status of a session directory (reads PhaseSessionData).
pub fn session_status(session_path: &Path) -> SessionStatus {
    let json_path = session_path.join("session.json");
    if !json_path.exists() {
        return SessionStatus::Empty;
    }
    match std::fs::read_to_string(&json_path) {
        Ok(json) => match serde_json::from_str::<PhaseSessionData>(&json) {
            Ok(data) => SessionStatus::Ok {
                phase: data.phase.label().to_string(),
                created: data.created,
            },
            Err(_) => SessionStatus::Corrupted,
        },
        Err(_) => SessionStatus::Corrupted,
    }
}

#[derive(Debug)]
pub enum SessionStatus {
    Ok { phase: String, created: String },
    Corrupted,
    Empty,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_create_session() {
        let tmp = TempDir::new().unwrap();
        let project_path = tmp.path();

        let session_path = create_session(project_path, "my-session").unwrap();
        assert!(session_path.exists());
    }

    #[test]
    fn test_session_status_empty() {
        let tmp = TempDir::new().unwrap();
        let session_path = create_session(tmp.path(), "empty").unwrap();
        assert!(matches!(session_status(&session_path), SessionStatus::Empty));
    }

    #[test]
    fn test_serialize_phase_session_data() {
        let data = PhaseSessionData {
            name: "test_session".into(),
            created: "2026-03-16T12:00:00Z".into(),
            phase: Phase::Spec,
            current_component: None,
            claude_sessions: ClaudeSessionMap::default(),
            conversations: std::collections::HashMap::new(),
            component_states: vec![],
            briefing: Some("briefing.md".into()),
            approved: HashMap::new(),
            pending_question: None,
            pending_phase_switch: None,
            baseline_iteration: None,
        };
        let json = serde_json::to_string_pretty(&data).unwrap();
        assert!(json.contains("\"phase\""));
        assert!(json.contains("Spec"));
        assert!(json.contains("\"briefing\""));
    }

    #[test]
    fn test_deserialize_session_without_approved_field() {
        // Old session.json files predate the `approved` field entirely —
        // must still deserialize, yielding an empty map (everything
        // unapproved) rather than failing.
        let json = r#"{
            "name": "test",
            "created": "2026-03-16T12:00:00Z",
            "phase": "Spec",
            "current_component": null,
            "claude_sessions": {},
            "conversations": {},
            "component_states": []
        }"#;
        let data: PhaseSessionData = serde_json::from_str(json).unwrap();
        assert!(data.approved.is_empty());
    }

    #[test]
    fn test_deserialize_session_without_briefing() {
        let json = r#"{
            "name": "test",
            "created": "2026-03-16T12:00:00Z",
            "phase": "Spec",
            "current_component": null,
            "claude_sessions": {},
            "conversations": {},
            "component_states": []
        }"#;
        let data: PhaseSessionData = serde_json::from_str(json).unwrap();
        assert!(data.briefing.is_none());
    }

    #[test]
    fn test_deserialize_session_without_pending_question_field() {
        // Old session.json files predate the `pending_question` field
        // entirely — must still deserialize, yielding `None`.
        let json = r#"{
            "name": "test",
            "created": "2026-03-16T12:00:00Z",
            "phase": "Spec",
            "current_component": null,
            "claude_sessions": {},
            "conversations": {},
            "component_states": []
        }"#;
        let data: PhaseSessionData = serde_json::from_str(json).unwrap();
        assert!(data.pending_question.is_none());
    }

    #[test]
    fn test_pending_question_round_trips_through_serialization() {
        let data = PhaseSessionData {
            name: "test_session".into(),
            created: "2026-03-16T12:00:00Z".into(),
            phase: Phase::Spec,
            current_component: None,
            claude_sessions: ClaudeSessionMap::default(),
            conversations: std::collections::HashMap::new(),
            component_states: vec![],
            briefing: None,
            approved: HashMap::new(),
            pending_question: Some(PendingQuestion {
                question: "How wide?".into(),
                options: vec!["20mm".into(), "40mm".into()],
            }),
            pending_phase_switch: None,
            baseline_iteration: None,
        };
        let json = serde_json::to_string_pretty(&data).unwrap();
        let round_tripped: PhaseSessionData = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped.pending_question, data.pending_question);
    }

    #[test]
    fn test_deserialize_session_without_pending_phase_switch_field() {
        // Old session.json files predate the `pending_phase_switch` field
        // entirely — must still deserialize, yielding `None`.
        let json = r#"{
            "name": "test",
            "created": "2026-03-16T12:00:00Z",
            "phase": "Spec",
            "current_component": null,
            "claude_sessions": {},
            "conversations": {},
            "component_states": []
        }"#;
        let data: PhaseSessionData = serde_json::from_str(json).unwrap();
        assert!(data.pending_phase_switch.is_none());
    }

    #[test]
    fn test_pending_phase_switch_round_trips_through_serialization() {
        let data = PhaseSessionData {
            name: "test_session".into(),
            created: "2026-03-16T12:00:00Z".into(),
            phase: Phase::Spec,
            current_component: None,
            claude_sessions: ClaudeSessionMap::default(),
            conversations: std::collections::HashMap::new(),
            component_states: vec![],
            briefing: None,
            approved: HashMap::new(),
            pending_question: None,
            pending_phase_switch: Some(PendingPhaseSwitch {
                target: "build".into(),
                reason: "that is a functional change".into(),
            }),
            baseline_iteration: None,
        };
        let json = serde_json::to_string_pretty(&data).unwrap();
        let round_tripped: PhaseSessionData = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped.pending_phase_switch, data.pending_phase_switch);
    }

    #[test]
    fn test_deserialize_session_without_baseline_iteration_field() {
        // Old session.json files predate the `baseline_iteration` field
        // entirely — must still deserialize, yielding `None`.
        let json = r#"{
            "name": "test",
            "created": "2026-03-16T12:00:00Z",
            "phase": "Spec",
            "current_component": null,
            "claude_sessions": {},
            "conversations": {},
            "component_states": []
        }"#;
        let data: PhaseSessionData = serde_json::from_str(json).unwrap();
        assert!(data.baseline_iteration.is_none());
    }

    #[test]
    fn test_baseline_iteration_round_trips_through_serialization() {
        let data = PhaseSessionData {
            name: "test_session".into(),
            created: "2026-03-16T12:00:00Z".into(),
            phase: Phase::Spec,
            current_component: None,
            claude_sessions: ClaudeSessionMap::default(),
            conversations: std::collections::HashMap::new(),
            component_states: vec![],
            briefing: None,
            approved: HashMap::new(),
            pending_question: None,
            pending_phase_switch: None,
            baseline_iteration: Some(3),
        };
        let json = serde_json::to_string_pretty(&data).unwrap();
        let round_tripped: PhaseSessionData = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped.baseline_iteration, Some(3));
    }

    #[test]
    fn test_deserialize_phase_session_data() {
        let json = r#"{
            "name": "test",
            "created": "2026-03-16T12:00:00Z",
            "phase": "Component",
            "current_component": "case_body",
            "claude_sessions": { "spec": "sid_123", "components": { "case_body": "sid_456" } },
            "conversations": {},
            "component_states": []
        }"#;
        let data: PhaseSessionData = serde_json::from_str(json).unwrap();
        assert_eq!(data.phase, Phase::Build);
        assert_eq!(data.current_component, Some("case_body".into()));
        assert_eq!(data.claude_sessions.spec, Some("sid_123".into()));
    }
}
