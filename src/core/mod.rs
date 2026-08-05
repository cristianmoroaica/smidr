//! UI-free application core.
//!
//! `AppCore` owns all non-rendering state and logic (phase state, session
//! management, Claude CLI interaction, reference handling, background-result
//! processing). The ratatui TUI in `src/main.rs` delegates to it and renders
//! whatever it observes via `AppCore::poll_events()` and its accessors.

pub mod app;

pub use app::{AppCore, CoreEvent, SwitchDenied};

// Re-exports so downstream code (Phase 2's server) can reach the surviving
// backend through `core` alone. Not all of these are consumed by the TUI
// yet — that's expected until Phase 2 lands.
#[allow(unused_imports)]
pub use crate::claude_bridge::{BusyState, ToolCall};
pub use crate::phase::Phase;
#[allow(unused_imports)]
pub use crate::session_manager::SessionManager;
#[allow(unused_imports)]
pub use crate::storage::{project, session, Project};

/// Results from background threads (Claude CLI calls, reference research).
///
/// Moved here from `src/tui/mod.rs` — core must not depend on `tui`.
pub enum BackgroundResult {
    ClaudeResponse {
        result: Result<String, String>,
        session_id: Option<String>,
    },
    ReferenceResearch {
        name: String,
        result: Result<String, String>,
    },
}
