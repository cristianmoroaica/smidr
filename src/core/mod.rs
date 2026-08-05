//! UI-free application core.
//!
//! `AppCore` owns all non-rendering state and logic (phase state, session
//! management, Claude CLI interaction, reference handling, background-result
//! processing). The axum web server in `src/server/**` consumes it via
//! `AppCore::poll_events()` and its accessors.

pub mod app;

pub use app::{AppCore, CoreEvent, SwitchDenied};

// Re-exports so downstream code (the web server) can reach the surviving
// backend through `core` alone. Not all of these are consumed by the server
// yet.
#[allow(unused_imports)]
pub use crate::claude_bridge::{BusyState, ToolCall};
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
}
