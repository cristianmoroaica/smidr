//! Shared test infrastructure.

use std::sync::Mutex;

/// Process-wide lock for tests that mutate the HOME env var. Every test module
/// that touches HOME must guard through this single lock — per-module locks
/// still race each other across modules.
pub static HOME_LOCK: Mutex<()> = Mutex::new(());
