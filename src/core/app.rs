//! `AppCore` — UI-free application state and orchestration.
//!
//! Moved from `src/main.rs`'s `App` struct: everything that is not rendering
//! (phase state, session ops, prompt submission, ref handling, background
//! result processing). The ratatui TUI consumes this through the public API
//! at the bottom of this file (`new`, `submit_prompt`, `phase`,
//! `try_switch_phase`, `poll_events`) plus a handful of `pub(crate)` helpers
//! for the sibling `event_handler`/`phase_dispatch` modules.

use std::path::PathBuf;

use crate::claude_bridge::{self, BusyState, ClaudeBridge, ToolCall};
use crate::config::Config;
use crate::core::BackgroundResult;
use crate::image;
use crate::parser;
use crate::phase::Phase;
use crate::python;
use crate::reference;
use crate::session_manager::SessionManager;
use crate::storage::{self, Project};
use crate::usage;

#[derive(Debug, Clone)]
pub(crate) enum RenameTarget {
    Project { old_name: String },
    Session { project_idx: usize, old_name: String },
}

#[derive(Debug, Clone)]
pub(crate) enum DeleteTarget {
    Project { name: String },
    Session { project_idx: usize, name: String },
}

#[derive(Debug, Clone)]
struct PendingReference {
    name: String,
    raw_response: String,
}

/// Why a phase switch was refused.
#[derive(Debug, Clone, PartialEq)]
pub enum SwitchDenied {
    SamePhase,
    /// Forward switch attempted while `phase_gate` is on and the current
    /// phase has not been approved via `approve_phase()`.
    NotApproved,
}

/// Whether `AppCore::new_with` primes the usage monitor from the network at
/// startup. Production passes `Enabled`; tests pass `Disabled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageRefresh {
    Enabled,
    #[allow(dead_code)] // constructed by tests only
    Disabled,
}

/// Events surfaced by `AppCore::poll_events` for the TUI (or, later, a
/// server) to render.
///
/// The TUI currently ignores the `StreamDelta`/`ToolCall` payloads (it
/// re-reads `AppCore::streaming_text()` on every render instead), but the
/// exact shape is required by the Phase 2 server-side contract.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum CoreEvent {
    StreamDelta(String),
    ToolCall { name: String, detail: String },
    ResponseDone,
    BuildArtifact { stl: PathBuf },
    Error(String),
}

/// UI-free application state.
pub struct AppCore {
    pub(crate) phase: Phase,
    pub(crate) session: SessionManager,
    pub(crate) claude: ClaudeBridge,
    pub(crate) projects: Vec<Project>,
    pub(crate) python_path: String,
    pub(crate) build_timeout: u64,
    pub(crate) pending_images: Vec<PathBuf>,
    pub(crate) active_refs: Vec<String>,
    ref_confirm_pending: Option<PendingReference>,
    pub(crate) usage_monitor: usage::UsageMonitor,
    pub(crate) briefing_pending: bool,

    // Interaction-mode flags consumed by submit_prompt.
    pub(crate) new_session_pending: bool,
    pub(crate) new_project_pending: bool,
    pub(crate) rename_pending: Option<RenameTarget>,
    pub(crate) delete_pending: Option<DeleteTarget>,
    pub(crate) save_part_pending: bool,

    // Source-of-truth for what used to live on the TUI panes; the TUI
    // mirrors these into its own widgets each event-loop tick.
    pub(crate) spec_content: String,
    pub(crate) model_summary: String,
    pub(crate) refs_summary: String,

    /// Full conversation log (role, content) — persists across a reset.
    messages: Vec<(String, String)>,
    /// Messages appended since the last `take_new_messages()` call.
    new_messages: Vec<(String, String)>,
    /// Bumped whenever `messages` is wholesale replaced (new session,
    /// session load, project open) so the TUI knows to rebuild its pane
    /// from `messages()` instead of just draining `new_messages()`.
    pub(crate) reset_generation: u64,

    /// Events queued synchronously (e.g. by `undo_component`) that ride
    /// along with the next `poll_events()` call.
    pending_events: Vec<CoreEvent>,

    /// Set by `undo_component` when the working-copy STL should be
    /// refreshed without auto-launching the viewer (the pre-refactor
    /// `undo_component` called `viewer.update_working_stl` but never
    /// `viewer.show`). Consumed once via `take_stl_refresh`.
    pending_stl_refresh: Option<PathBuf>,
    /// Set when core changed something the UI renders but emitted no
    /// `CoreEvent` (busy-state transitions, `.open_viewer` with no STL yet).
    /// Consumed via `take_repaint_request`.
    repaint_requested: bool,

    /// Server-authoritative phase-approval gate (Task 2.2). When `false`
    /// (the TUI's setting, and the default), `try_switch_phase` treats
    /// every phase as pre-approved so Alt+1/2/3 free phase switching keeps
    /// working exactly as before this gate existed. The web server turns
    /// this on via `set_phase_gate(true)`, making `advance` require an
    /// explicit `approve_phase()` call for the current phase first.
    pub(crate) phase_gate: bool,
}

impl AppCore {
    // ---- construction -------------------------------------------------

    pub fn new(config: Config, briefing: Option<String>) -> Result<AppCore, String> {
        AppCore::new_with(config, briefing, UsageRefresh::Enabled)
    }

    /// Same constructor as [`AppCore::new`], with the startup usage refresh
    /// injected rather than compiled in. `maybe_refresh` spawns a detached
    /// thread that reads `~/.claude/.credentials.json` and calls the Anthropic
    /// API, which unit tests must not do; passing [`UsageRefresh::Disabled`]
    /// keeps tests on this exact code path instead of a `cfg(test)` variant.
    pub fn new_with(
        config: Config,
        briefing: Option<String>,
        usage_refresh: UsageRefresh,
    ) -> Result<AppCore, String> {
        let python_path = config.python_path();
        let build_timeout = config.defaults.build_timeout;
        let mut session = SessionManager::new(build_timeout, python_path.clone());

        // Ensure ~/MiModel/ exists and scan for projects
        let _ = storage::project::ensure_root();
        seed_references();
        let mut projects = storage::project::list_projects().unwrap_or_default();

        if let Some(ref content) = briefing {
            let name = briefing_name(content);

            // Create project, dedup name if exists
            let mut project_name = name.clone();
            let root = storage::project::root_dir();
            let mut suffix = 2;
            while root.join(&project_name).exists() {
                project_name = format!("{}_{}", name, suffix);
                suffix += 1;
            }

            let project_path = storage::project::create_project(&project_name, "")
                .map_err(|e| format!("Failed to create briefing project: {e}"))?;

            let session_dir = project_path.join(&project_name);
            session.create(session_dir.clone(), build_timeout, python_path.clone(), Some(content));
            session.active_name = Some(project_name.clone());
            session.active_dir = Some(session_dir.clone());

            // Re-scan projects so the tree includes the new one
            projects = storage::project::list_projects().unwrap_or_default();
        }

        Ok(AppCore {
            phase: Phase::Spec,
            session,
            claude: claude_bridge::ClaudeBridge::new(config.claude.model),
            projects,
            python_path,
            build_timeout,
            pending_images: Vec::new(),
            active_refs: Vec::new(),
            ref_confirm_pending: None,
            usage_monitor: {
                let m = usage::UsageMonitor::new();
                if usage_refresh == UsageRefresh::Enabled {
                    m.maybe_refresh(); // fetch once at startup
                }
                m
            },
            briefing_pending: briefing.is_some(),
            new_session_pending: false,
            new_project_pending: false,
            rename_pending: None,
            delete_pending: None,
            save_part_pending: false,
            spec_content: String::new(),
            model_summary: String::new(),
            refs_summary: String::new(),
            messages: Vec::new(),
            new_messages: Vec::new(),
            reset_generation: 0,
            pending_events: Vec::new(),
            pending_stl_refresh: None,
            repaint_requested: false,
            phase_gate: false,
        })
    }

    /// Turn the server-authoritative phase-approval gate on or off. See the
    /// `phase_gate` field doc comment for what each setting means.
    pub fn set_phase_gate(&mut self, on: bool) {
        self.phase_gate = on;
    }

    /// Whether `phase` has been approved in the current session's approval
    /// map. Always `false` when there is no active phase session (e.g. no
    /// project/session opened yet).
    pub fn is_phase_approved(&self, phase: Phase) -> bool {
        self.session
            .phase_session
            .as_ref()
            .map(|ps| ps.approved.get(phase.label()).copied().unwrap_or(false))
            .unwrap_or(false)
    }

    /// Mark the CURRENT phase approved and persist it.
    ///
    /// Approval is stored on the active `PhaseSession`. A project opened
    /// via `open_project_by_id` with no sessions yet has none — mirror
    /// `submit_prompt`'s lazy session auto-creation (same session-dir
    /// derivation) so "approve before ever prompting" (the web client's
    /// natural first action after connecting) has somewhere to persist to.
    pub fn approve_phase(&mut self) {
        if self.session.phase_session.is_none() {
            let dir = self.session.active_dir.clone().unwrap_or_else(|| {
                let project_path = self
                    .session
                    .project_idx
                    .and_then(|idx| self.projects.get(idx))
                    .map(|p| p.path.clone())
                    .unwrap_or_else(|| storage::project::root_dir().join("Untitled"));
                let session_dir = project_path.join("session");
                self.session.active_name = Some("session".to_string());
                self.session.active_dir = Some(session_dir.clone());
                session_dir
            });
            self.session.create(dir, self.build_timeout, self.python_path.clone(), None);
            self.refresh_projects();
        }

        let phase = self.phase;
        if let Some(ref mut ps) = self.session.phase_session {
            ps.approved.insert(phase.label().to_string(), true);
        }
        self.session.save(phase);
    }

    /// Look up a project by its directory id (the segment returned by
    /// `/api/projects` as `id`), open it, and — if it has any sessions —
    /// load the last one (sessions are name-sorted; "last" is the
    /// alphabetically-last name).
    pub(crate) fn open_project_by_id(&mut self, id: &str) -> Result<(), String> {
        self.refresh_projects();
        let idx = self
            .projects
            .iter()
            .position(|p| p.path.file_name().map(|n| n == id).unwrap_or(false))
            .ok_or_else(|| "unknown project".to_string())?;

        let last_session = self.projects[idx].sessions.last().map(|si| si.name.clone());

        self.open_project(idx);

        if let Some(session_name) = last_session {
            self.load_session(idx, session_name);
        }

        Ok(())
    }

    // ---- conversation log helpers --------------------------------------

    /// Append a message to the conversation log (mirrors the old
    /// `self.conversation.add(role, text)` on the TUI pane).
    pub(crate) fn push_message(&mut self, role: &str, content: &str) {
        self.messages.push((role.to_string(), content.to_string()));
        self.new_messages.push((role.to_string(), content.to_string()));
    }

    /// Wholesale-replace the conversation log (mirrors `conversation.clear()`
    /// followed by re-population). Bumps `reset_generation` so the TUI knows
    /// to rebuild its pane from `messages()` rather than drain incrementally.
    fn reset_conversation(&mut self, entries: Vec<(String, String)>) {
        self.messages = entries;
        self.new_messages.clear();
        self.reset_generation = self.reset_generation.wrapping_add(1);
    }

    pub(crate) fn push_event(&mut self, event: CoreEvent) {
        self.pending_events.push(event);
    }

    /// Queue a working-copy STL refresh (no viewer auto-launch) for the TUI
    /// to pick up via `take_stl_refresh`.
    pub(crate) fn queue_stl_refresh(&mut self, path: PathBuf) {
        self.pending_stl_refresh = Some(path);
    }

    /// Full conversation log, in order.
    pub fn messages(&self) -> &[(String, String)] {
        &self.messages
    }

    /// Drain messages appended since the last call.
    pub fn take_new_messages(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.new_messages)
    }

    /// Bumped whenever `messages()` was wholesale-replaced.
    pub fn reset_generation(&self) -> u64 {
        self.reset_generation
    }

    // ---- accessors used by the TUI -------------------------------------

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn busy(&self) -> BusyState {
        self.claude.busy
    }

    pub fn streaming_text(&self) -> &str {
        &self.claude.streaming_text
    }

    pub fn spec_content(&self) -> &str {
        &self.spec_content
    }

    pub fn model_summary(&self) -> &str {
        &self.model_summary
    }

    pub fn refs_summary(&self) -> &str {
        &self.refs_summary
    }

    pub fn usage_stats(&self) -> usage::UsageStats {
        self.usage_monitor.stats()
    }

    pub fn pending_images(&self) -> &[PathBuf] {
        &self.pending_images
    }

    pub fn push_pending_image(&mut self, path: PathBuf) {
        self.pending_images.push(path);
    }

    /// Clear pending images, returning how many were cleared.
    pub fn clear_pending_images(&mut self) -> usize {
        let n = self.pending_images.len();
        self.pending_images.clear();
        n
    }

    pub fn projects(&self) -> &[Project] {
        &self.projects
    }

    pub fn session_dir(&self) -> Option<&std::path::Path> {
        self.session.active_dir.as_deref()
    }

    /// Name of the currently active session, if any.
    pub fn session_name(&self) -> Option<&str> {
        self.session.active_name.as_deref()
    }

    /// Session-scoped temp directory, used as a fallback attachment
    /// destination when no session directory is active yet.
    pub fn temp_dir(&self) -> &std::path::Path {
        self.session.temp_dir()
    }

    pub fn model_metadata(&self) -> Option<python::ModelMetadata> {
        self.session.current_metadata.clone()
    }

    pub fn iteration(&self) -> u32 {
        self.session.iteration()
    }

    pub fn latest_stl_path(&self) -> Option<PathBuf> {
        self.session.latest_stl_path()
    }

    pub fn export(&self, dest: &std::path::Path) -> Result<(), String> {
        self.session.export(dest)
    }

    pub fn cancel(&self) {
        self.claude.cancel();
    }

    pub fn save_session(&mut self) {
        self.session.save(self.phase);
    }

    /// Undo the last build iteration, mirroring `model_summary` to match
    /// (bug-for-bug, "Iterations: 0" is the pre-refactor text verbatim).
    /// Does NOT touch the viewer — callers that need the working-copy STL
    /// refreshed should use `take_stl_refresh` after a successful undo.
    pub fn undo(&mut self) -> bool {
        if self.session.undo() {
            if let Some(ref meta) = self.session.current_metadata {
                self.model_summary = format!(
                    "{:.1} x {:.1} x {:.1} mm\nIterations: 0\nEngine: {}\nWatertight: {}",
                    meta.dimensions.x, meta.dimensions.y, meta.dimensions.z,
                    meta.engine.as_str(),
                    if meta.watertight { "yes" } else { "no" }
                );
            }
            true
        } else {
            false
        }
    }

    /// Drain a pending working-copy STL refresh queued by `undo_component`.
    /// The caller should call `viewer.update_working_stl` on the returned
    /// path WITHOUT launching the viewer (matches pre-refactor behavior).
    pub fn take_stl_refresh(&mut self) -> Option<PathBuf> {
        self.pending_stl_refresh.take()
    }

    /// Drain the "repaint needed but no event to show for it" flag.
    pub fn take_repaint_request(&mut self) -> bool {
        std::mem::take(&mut self.repaint_requested)
    }

    pub fn briefing_pending(&self) -> bool {
        self.briefing_pending
    }

    pub fn clear_briefing_pending(&mut self) {
        self.briefing_pending = false;
    }

    pub fn request_new_session(&mut self) {
        self.new_session_pending = true;
    }

    pub fn request_new_project(&mut self) {
        self.new_project_pending = true;
    }

    pub fn request_save_part(&mut self) {
        self.save_part_pending = true;
    }

    pub(crate) fn request_rename(&mut self, target: RenameTarget) {
        self.rename_pending = Some(target);
    }

    pub(crate) fn request_delete(&mut self, target: DeleteTarget) {
        self.delete_pending = Some(target);
    }

    /// Kill any running Claude subprocess on app exit.
    pub fn cleanup(&self) {
        self.claude.cancel();
    }

    // ---- prompt submission ---------------------------------------------

    /// Non-empty `part_refs` are prepended to the dispatched prompt as
    /// `<selected_part>` lines (one per ref) before it is sent to Claude.
    /// They are NOT part of the conversation-visible user message — the
    /// pushed/stored history always shows the user's raw text untagged.
    pub fn submit_prompt(&mut self, text: &str, part_refs: &[String], lib_refs: &[String]) {
        for r in lib_refs {
            if !self.active_refs.contains(r) {
                self.active_refs.push(r.clone());
            }
        }

        let text = text.to_string();

        if self.claude.busy != BusyState::Idle {
            self.push_message("system", "Please wait for the current operation to finish.");
            return;
        }

        // Handle save part
        if self.save_part_pending {
            self.save_part_pending = false;
            let part_name: String = text.chars().take(50).collect();
            let part_name = part_name.trim().to_string();
            if part_name.is_empty() {
                self.push_message("system", "Save cancelled (empty name).");
                return;
            }
            if let Some(ref stl_src) = self.session.latest_stl_path() {
                let dest_dir = self.session.active_dir
                    .as_ref()
                    .and_then(|d| d.parent().map(|p| p.to_path_buf()))
                    .unwrap_or_else(|| storage::project::root_dir().join("Untitled"));
                let dest = dest_dir.join(format!("{part_name}.stl"));
                let _ = std::fs::create_dir_all(&dest_dir);
                match std::fs::copy(stl_src, &dest) {
                    Ok(_) => {
                        self.push_message("system", &format!("Saved part '{part_name}.stl' to {}", dest_dir.display()));
                        let code = self.session.current_code.clone()
                            .or_else(|| self.find_latest_code_py());
                        if let Some(code) = code {
                            let code_dest = dest_dir.join(format!("{part_name}.py"));
                            let _ = std::fs::write(&code_dest, code);
                        }
                    }
                    Err(e) => self.push_message("system", &format!("Save failed: {e}")),
                }
            } else {
                self.push_message("system", "No model to save.");
            }
            return;
        }

        // Handle delete confirmation
        if let Some(target) = self.delete_pending.take() {
            if text.trim().eq_ignore_ascii_case("yes") {
                match target {
                    DeleteTarget::Project { name, .. } => {
                        match storage::project::delete_project(&name) {
                            Ok(()) => self.push_message("system", &format!("Deleted project '{name}'.")),
                            Err(e) => self.push_message("system", &format!("Delete failed: {e}")),
                        }
                    }
                    DeleteTarget::Session { project_idx, name } => {
                        if let Some(project) = self.projects.get(project_idx) {
                            let session_path = project.path.join(&name);
                            match storage::session::delete_session(&session_path) {
                                Ok(()) => self.push_message("system", &format!("Deleted session '{name}'.")),
                                Err(e) => self.push_message("system", &format!("Delete failed: {e}")),
                            }
                        }
                    }
                }
                self.refresh_projects();
            } else {
                self.push_message("system", "Delete cancelled.");
            }
            return;
        }

        // Handle rename pending
        if let Some(target) = self.rename_pending.take() {
            let new_name: String = text.chars().take(50).collect();
            let new_name = new_name.trim().to_string();
            if new_name.is_empty() {
                self.push_message("system", "Rename cancelled (empty name).");
                return;
            }
            match target {
                RenameTarget::Project { old_name, .. } => {
                    match storage::project::rename_project(&old_name, &new_name) {
                        Ok(()) => self.push_message("system", &format!("Renamed project '{old_name}' to '{new_name}'.")),
                        Err(e) => self.push_message("system", &format!("Rename failed: {e}")),
                    }
                }
                RenameTarget::Session { project_idx, old_name } => {
                    if let Some(project) = self.projects.get(project_idx) {
                        let session_path = project.path.join(&old_name);
                        match storage::session::rename_session(&session_path, &new_name) {
                            Ok(_) => self.push_message("system", &format!("Renamed session '{old_name}' to '{new_name}'.")),
                            Err(e) => self.push_message("system", &format!("Rename failed: {e}")),
                        }
                    }
                }
            }
            self.refresh_projects();
            return;
        }

        // Handle new project pending
        if self.new_project_pending {
            self.new_project_pending = false;
            let project_name: String = text.chars().take(30).collect();
            let project_name = project_name.trim().to_string();
            if !project_name.is_empty() {
                match storage::project::create_project(&project_name, "") {
                    Ok(_) => {
                        self.push_message("system", &format!("Created project '{project_name}'."));
                        self.refresh_projects();
                    }
                    Err(e) => {
                        self.push_message("system", &format!("Failed to create project: {e}"));
                    }
                }
            }
            return;
        }

        // Handle new session pending
        if self.new_session_pending {
            self.new_session_pending = false;
            self.session.reset();
            self.reset_conversation(Vec::new());
            self.claude.session_id = None;
            self.phase = Phase::Spec;
            self.active_refs.clear();
            self.ref_confirm_pending = None;
            self.push_message("system", "New session started.");
        }

        // Handle reference save confirmation
        if let Some(pending) = self.ref_confirm_pending.take() {
            if text.trim().eq_ignore_ascii_case("yes") {
                self.save_pending_reference(pending);
            } else {
                self.push_message("system", "Reference not saved.");
            }
            return;
        }

        // Handle /ref commands — extract attached images first since we return early
        if text.starts_with("/ref") {
            let (_clean, mut images) = image::extract_attachment_paths(&text);
            images.extend(self.pending_images.drain(..));

            let parts: Vec<&str> = text.split("/ref")
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().trim_matches(',').trim())
                .collect();

            if parts.len() > 1 {
                self.handle_multi_ref(parts, images);
            } else {
                self.handle_ref_command(&text, images);
            }
            return;
        }

        // Handle /attach command — explicit file attachment (works in tmux where drag-drop doesn't)
        if text.starts_with("/attach") {
            let paths_str = text.strip_prefix("/attach").unwrap_or("").trim();
            if paths_str.is_empty() {
                self.push_message("system", "Usage: /attach <path> [path2 ...]\nPaste or type file paths to attach images/PDFs.");
                return;
            }
            let (_, files) = image::extract_attachment_paths(paths_str);
            if files.is_empty() {
                self.push_message("system", "No valid image/PDF files found in the provided paths.");
            } else {
                for path in &files {
                    let kind = if image::is_pdf(path) { "PDF" } else { "image" };
                    let size_kb = std::fs::metadata(path).map(|m| m.len() / 1024).unwrap_or(0);
                    self.push_message("system", &format!("Attached {kind} ({size_kb}KB): {}", path.display()));
                }
                self.pending_images.extend(files);
            }
            return;
        }

        // Handle /import command — import a STEP file into the session
        if text.starts_with("/import") {
            let args = text.strip_prefix("/import").unwrap_or("").trim();
            if args.is_empty() {
                self.push_message("system", "Usage: /import <path/to/file.step>");
                return;
            }
            let path_str = {
                let lower = args.to_lowercase();
                let end = [".step", ".stp"].iter()
                    .filter_map(|ext| lower.find(ext).map(|pos| pos + ext.len()))
                    .min();
                match end {
                    Some(pos) => args[..pos].to_string(),
                    None => {
                        self.push_message("system", &format!("No .step/.stp file found in: {args}"));
                        return;
                    }
                }
            };
            let path_str = if path_str.starts_with("~/") {
                dirs::home_dir()
                    .map(|h| h.join(&path_str[2..]).to_string_lossy().to_string())
                    .unwrap_or(path_str)
            } else {
                path_str
            };
            let source = std::path::Path::new(&path_str);
            if !source.exists() {
                self.push_message("system", &format!("File not found: {path_str}"));
                return;
            }
            let source = source.to_path_buf();
            self.import_step_file(&source);
            return;
        }

        // Auto-create session name from first prompt if none active
        if self.session.active_name.is_none() {
            let session_name: String = text.chars()
                .filter(|c| c.is_alphanumeric() || *c == ' ')
                .take(30)
                .collect();
            let session_name = session_name.trim().replace(' ', "_");
            if !session_name.is_empty() {
                let project_path = self.session.project_idx
                    .and_then(|idx| self.projects.get(idx))
                    .map(|p| p.path.clone())
                    .unwrap_or_else(|| storage::project::root_dir().join("Untitled"));
                let session_dir = project_path.join(&session_name);
                self.session.active_name = Some(session_name);
                self.session.active_dir = Some(session_dir);
                self.refresh_projects();
            }
        }

        // Create PhaseSession if we have a session dir but no phase session yet
        if self.session.phase_session.is_none() {
            if let Some(dir) = self.session.active_dir.clone() {
                self.session.create(dir, self.build_timeout, self.python_path.clone(), None);
                self.refresh_projects();
            }
        }

        // Extract attachment paths (images + PDFs) from text
        let (clean_text, mut extracted_images) = image::extract_attachment_paths(&text);
        for path in &extracted_images {
            let kind = if image::is_pdf(path) { "PDF" } else { "image" };
            let size_kb = std::fs::metadata(path).map(|m| m.len() / 1024).unwrap_or(0);
            self.push_message("system", &format!("Attached {kind} ({size_kb}KB): {}", path.display()));
        }
        extracted_images.extend(self.pending_images.drain(..));
        let all_images = extracted_images;

        // Add user message to conversation
        self.push_message("user", &clean_text);
        self.session.add_message(self.phase, "user", &clean_text);
        self.session.save(self.phase);

        // Handle 'advance' command to move between phases
        if clean_text.trim().eq_ignore_ascii_case("advance") {
            match self.phase {
                Phase::Spec => {
                    self.phase = Phase::Build;
                    self.claude.session_id = None;
                    self.push_message("system", "Advanced to Build phase.");
                    self.session.save(self.phase);
                }
                Phase::Build => {
                    self.phase = Phase::Refine;
                    self.claude.session_id = None;
                    self.push_message("system", "Advanced to Refine phase. Functionality is locked — focus on aesthetics.");
                    self.session.save(self.phase);
                }
                Phase::Refine => {
                    self.push_message("system", "Already in the final phase.");
                }
            }
            return;
        }

        // Build the dispatch text: non-empty part_refs are prepended as
        // <selected_part> lines. This is what gets sent to Claude — the
        // conversation-visible clean_text above is left untouched.
        let dispatch_text = {
            let wrapped: Vec<String> = part_refs.iter()
                .map(|r| r.trim())
                .filter(|r| !r.is_empty())
                .map(|r| r.replace(['<', '>'], ""))
                .filter(|r| !r.is_empty())
                .map(|r| format!("<selected_part>{r}</selected_part>"))
                .collect();
            if wrapped.is_empty() {
                clean_text.clone()
            } else {
                format!("{}\n\n{}", wrapped.join("\n"), clean_text)
            }
        };

        // Phase-specific dispatch
        match self.phase {
            Phase::Spec => {
                self.send_spec_prompt(&dispatch_text, all_images);
            }
            Phase::Build => {
                let trimmed = clean_text.trim().to_lowercase();
                if trimmed == "undo" {
                    self.undo_component();
                } else {
                    self.send_build_prompt(&dispatch_text, all_images);
                }
            }
            Phase::Refine => {
                let t = clean_text.trim().to_lowercase();
                if t.starts_with("set ") || t == "export" {
                    self.send_refine_prompt(&clean_text, all_images);
                } else {
                    self.send_refine_prompt(&dispatch_text, all_images);
                }
            }
        }
    }

    // ---- reference handling ---------------------------------------------

    pub(crate) fn handle_ref_command(&mut self, text: &str, attached_images: Vec<PathBuf>) {
        let args = text.strip_prefix("/ref").unwrap_or("").trim();

        if args.is_empty() || args == "list" {
            match reference::load_library() {
                Ok(library) if library.is_empty() => {
                    self.push_message("system", "Reference library is empty.");
                }
                Ok(library) => {
                    let list: Vec<String> = library.iter()
                        .map(|(c, s)| format!("  {} — {} [{}]", s, c.identity.name, c.identity.category))
                        .collect();
                    self.push_message("system", &format!("References:\n{}", list.join("\n")));
                }
                Err(e) => self.push_message("system", &format!("Error: {e}")),
            }
            return;
        }

        if let Some(name) = args.strip_prefix("remove ") {
            let name = name.trim();
            let slug = reference::slug_from_name(name);
            let path = reference::references_dir().join(format!("{slug}.toml"));
            if path.exists() {
                if let Err(e) = std::fs::remove_file(&path) {
                    self.push_message("system", &format!("Failed to remove: {e}"));
                } else {
                    self.active_refs.retain(|s| s != &slug);
                    self.push_message("system", &format!("Removed reference '{slug}'."));
                }
            } else {
                self.push_message("system", &format!("Reference '{slug}' not found."));
            }
            return;
        }

        let is_refresh = args.starts_with("refresh ");
        let query = if is_refresh {
            args.strip_prefix("refresh ").unwrap().trim()
        } else {
            args
        };

        if !is_refresh {
            match reference::load_one(query) {
                Ok((comp, slug)) => {
                    if !self.active_refs.contains(&slug) {
                        self.active_refs.push(slug.clone());
                    }
                    let summary = reference::summarize_for_prompt(&[&comp]);
                    self.push_message("system", &format!("Loaded reference:\n{summary}"));
                    self.refresh_refs_panel();
                    return;
                }
                Err(e) if e.contains("Multiple matches") => {
                    self.push_message("system", &e);
                    return;
                }
                Err(_) => {} // Not found — fall through to research
            }
        }

        // Research new component via Claude
        self.push_message("system", &format!("Researching '{query}'..."));

        let name = query.to_string();
        let research_prompt = format!(
            "Research the component: {name}\n\
             Find official datasheet or technical drawing.\n\
             Extract ALL mechanical dimensions in millimeters and key constraints.\n\
             Return the data as a TOML block in this exact format:\n\
             ```toml\n\
             [identity]\n\
             name = \"full component name\"\n\
             manufacturer = \"...\"\n\
             part_number = \"...\"\n\
             category = \"motor|fastener|bearing|connector|other\"\n\
             created = \"\"\n\
             updated = \"\"\n\n\
             [dimensions]\n\
             units = \"mm\"\n\
             key_name = value\n\n\
             [constraints]\n\
             key_with_unit_suffix = value\n\n\
             [sources]\n\
             urls = [\"...\"]\n\
             notes = \"...\"\n\
             ```\n\
             Return ONLY the TOML block, nothing else."
        );

        self.claude.send_raw_prompt(
            "You are a technical reference researcher. Search for component datasheets and extract precise mechanical specifications.",
            &research_prompt,
            &attached_images,
            &name,
        );
    }

    pub(crate) fn handle_multi_ref(&mut self, names: Vec<&str>, images: Vec<PathBuf>) {
        let mut loaded = Vec::new();
        let mut to_research = Vec::new();

        for name in &names {
            match reference::load_one(name) {
                Ok((comp, slug)) => {
                    if !self.active_refs.contains(&slug) {
                        self.active_refs.push(slug.clone());
                    }
                    loaded.push(comp.identity.name.clone());
                }
                Err(_) => {
                    to_research.push(name.to_string());
                }
            }
        }

        if !loaded.is_empty() {
            self.push_message("system",
                &format!("Loaded {} references: {}", loaded.len(), loaded.join(", ")));
            self.refresh_refs_panel();
        }

        if to_research.is_empty() {
            return;
        }

        self.push_message("system",
            &format!("Researching {} components: {}...", to_research.len(), to_research.join(", ")));

        let research_prompt = format!(
            "Research these components and return a TOML block for EACH one:\n- {}\n\n\
             For each component, output a separate ```toml fenced block with [identity], [dimensions], [constraints], [sources] sections.\n\
             Use the exact format: name, manufacturer, part_number, category, created=\"\", updated=\"\" in [identity].\n\
             All dimensions in mm. Constraints with unit suffixes (_g, _a, _nm, _c, _kn).\n\
             Separate each component's TOML block clearly.",
            to_research.join("\n- ")
        );

        let result_name = to_research.join(",");

        self.claude.send_raw_prompt(
            "You are a technical reference researcher. Search for component datasheets and extract precise mechanical specifications.",
            &research_prompt,
            &images,
            &result_name,
        );
    }

    fn save_pending_reference(&mut self, pending: PendingReference) {
        let is_batch = pending.name.contains(',');

        if is_batch {
            let mut saved = Vec::new();
            let mut failed = Vec::new();
            let now = chrono::Utc::now().to_rfc3339();

            for block in pending.raw_response.split("```toml") {
                if let Some(end) = block.find("```") {
                    let toml_str = block[..end].trim();
                    if toml_str.is_empty() {
                        continue;
                    }
                    match toml::from_str::<reference::ReferenceComponent>(toml_str) {
                        Ok(mut comp) => {
                            if comp.identity.created.is_empty() {
                                comp.identity.created = now.clone();
                            }
                            if comp.identity.updated.is_empty() {
                                comp.identity.updated = now.clone();
                            }
                            let name = comp.identity.name.clone();
                            match reference::save(&comp) {
                                Ok(slug) => {
                                    if !self.active_refs.contains(&slug) {
                                        self.active_refs.push(slug);
                                    }
                                    saved.push(name);
                                }
                                Err(e) => failed.push(format!("{}: {}", name, e)),
                            }
                        }
                        Err(e) => failed.push(format!("parse error: {}", e)),
                    }
                }
            }

            if !saved.is_empty() {
                self.push_message("system",
                    &format!("Saved {} references: {}", saved.len(), saved.join(", ")));
                self.refresh_refs_panel();
            }
            if !failed.is_empty() {
                self.push_message("system",
                    &format!("Failed: {}", failed.join("; ")));
            }
        } else {
            let toml_str = if let Ok(extracted) = parser::parse_toml_response(&pending.raw_response) {
                extracted
            } else {
                pending.raw_response.clone()
            };

            let now = chrono::Utc::now().to_rfc3339();

            match toml::from_str::<reference::ReferenceComponent>(&toml_str) {
                Ok(mut comp) => {
                    if comp.identity.created.is_empty() {
                        comp.identity.created = now.clone();
                    }
                    if comp.identity.updated.is_empty() {
                        comp.identity.updated = now;
                    }
                    match reference::save(&comp) {
                        Ok(saved_slug) => {
                            if !self.active_refs.contains(&saved_slug) {
                                self.active_refs.push(saved_slug.clone());
                            }
                            self.push_message("system",
                                &format!("Saved reference '{}' as {saved_slug}.toml", comp.identity.name));
                            self.refresh_refs_panel();
                        }
                        Err(e) => self.push_message("system", &format!("Failed to save: {e}")),
                    }
                }
                Err(e) => {
                    self.push_message("system",
                        &format!("Failed to parse reference TOML: {e}\nTry `/ref refresh {}` to retry.", pending.name));
                }
            }
        }
    }

    pub(crate) fn build_ref_context(&self) -> Option<String> {
        let library = reference::load_library().unwrap_or_default();
        if library.is_empty() && self.active_refs.is_empty() {
            return None;
        }

        let mut parts = Vec::new();

        if !self.active_refs.is_empty() {
            let active: Vec<&reference::ReferenceComponent> = library.iter()
                .filter(|(_, slug)| self.active_refs.contains(slug))
                .map(|(comp, _)| comp)
                .collect();
            if !active.is_empty() {
                parts.push(format!(
                    "## Active Reference Components (use these dimensions)\n{}",
                    reference::summarize_for_prompt(&active)
                ));
            }
        }

        let all_refs: Vec<&reference::ReferenceComponent> = library.iter()
            .map(|(comp, _)| comp)
            .collect();
        if !all_refs.is_empty() {
            parts.push(format!(
                "## Available in Reference Library\n{}",
                reference::list_names(&all_refs)
            ));
        }

        if parts.is_empty() { None } else { Some(parts.join("\n\n")) }
    }

    /// Build full context for non-Spec phases: spec data + reference context.
    pub(crate) fn build_phase_context(&self) -> Option<String> {
        let mut parts = Vec::new();

        let spec = self.spec_content.clone();
        if !spec.is_empty() {
            parts.push(format!("## Design Specification\n{spec}"));
        }

        let spec_conversation = self.session.conversations(Phase::Spec);
        if !spec_conversation.is_empty() {
            let summary: Vec<String> = spec_conversation.iter()
                .filter(|e| e.role == "user" || e.role == "assistant")
                .take(20)
                .map(|e| format!("{}: {}", e.role, e.content))
                .collect();
            if !summary.is_empty() {
                parts.push(format!("## Spec Conversation Summary\n{}", summary.join("\n")));
            }
        }

        if let Some(ref dir) = self.session.active_dir {
            let goal_path = dir.join("goal.md");
            if goal_path.exists() {
                if let Ok(goal) = std::fs::read_to_string(&goal_path) {
                    parts.push(format!("## Verification Checklist (goal.md)\n{goal}"));
                }
            }
            let narrative_path = dir.join("spec_narrative.md");
            if narrative_path.exists() {
                if let Ok(narrative) = std::fs::read_to_string(&narrative_path) {
                    if !narrative.is_empty() {
                        parts.push(format!("## Full Spec Narrative\n{narrative}"));
                    }
                }
            }
        }

        if let Some(ref_ctx) = self.build_ref_context() {
            parts.push(ref_ctx);
        }

        if matches!(self.phase, Phase::Build | Phase::Refine) {
            if let Some(ref dir) = self.session.active_dir {
                let layout_path = dir.join("assembly").join("layout.py");
                if layout_path.exists() {
                    if let Ok(layout) = std::fs::read_to_string(&layout_path) {
                        parts.push(format!(
                            "## Spatial Layout (assembly/layout.py)\n\
                             This layout assembly establishes component positions. \
                             Detail pass MUST preserve bounding boxes and mounting interfaces.\n\
                             ```python\n{layout}\n```"
                        ));
                    }
                }
            }
        }

        if matches!(self.phase, Phase::Build | Phase::Refine) {
            if let Some(build_ctx) = self.build_prior_builds_context() {
                parts.push(build_ctx);
            }
        }

        if parts.is_empty() { None } else { Some(parts.join("\n\n")) }
    }

    // ---- background result / build handling -----------------------------

    fn handle_bg_result(&mut self, result: BackgroundResult) {
        self.usage_monitor.maybe_refresh();
        match result {
            BackgroundResult::ClaudeResponse { result, session_id } => {
                if let Some(sid) = session_id {
                    self.claude.session_id = Some(sid);
                }

                match result {
                    Ok(response) => {
                        self.push_message("assistant", &response);
                        self.session.add_message(self.phase, "assistant", &response);
                        self.session.save(self.phase);

                        match self.phase {
                            Phase::Spec => {
                                self.handle_spec_response(&response);
                                self.claude.busy = BusyState::Idle;
                            }
                            Phase::Build | Phase::Refine => {
                                let parsed = parser::parse_response(&response);
                                if let Some(code_block) = parsed.code {
                                    self.claude.busy = BusyState::Building;
                                    let result = self.session.build(&code_block.code, code_block.engine);
                                    self.handle_build_result(result);
                                } else {
                                    self.claude.busy = BusyState::Idle;
                                }
                            }
                        }
                        self.push_event(CoreEvent::ResponseDone);
                    }
                    Err(e) => {
                        self.claude.busy = BusyState::Idle;
                        self.push_event(CoreEvent::Error(format!("Claude error: {e}")));
                    }
                }
            }
            BackgroundResult::ReferenceResearch { name, result } => {
                match result {
                    Ok(response) => {
                        self.push_message("assistant", &response);
                        if name.contains(',') {
                            self.push_message("system", "Save all references? (yes/no)");
                        } else {
                            self.push_message("system", "Save as reference? (yes/no)");
                        }
                        self.ref_confirm_pending = Some(PendingReference {
                            name,
                            raw_response: response,
                        });
                        self.claude.busy = BusyState::Idle;
                        self.push_event(CoreEvent::ResponseDone);
                    }
                    Err(e) => {
                        self.claude.busy = BusyState::Idle;
                        self.push_event(CoreEvent::Error(format!("Research failed: {e}")));
                    }
                }
            }
        }
    }

    pub(crate) fn handle_build_result(&mut self, build_result: python::BuildResult) {
        match build_result {
            python::BuildResult::Success(ref meta) => {
                let dims_msg = format!(
                    "Built successfully\n{:.1} x {:.1} x {:.1} mm",
                    meta.dimensions.x, meta.dimensions.y, meta.dimensions.z
                );
                let features_str = if meta.features.is_empty() {
                    String::new()
                } else {
                    format!("\n{}", meta.features.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n"))
                };
                self.push_message("system", &format!("{dims_msg}{features_str}"));

                let stl_path = self.session.latest_stl_path();
                let iteration = self.session.iteration();
                let model_summary = format!(
                    "{:.1} x {:.1} x {:.1} mm\nIterations: {}\nEngine: {}\nWatertight: {}{}",
                    meta.dimensions.x, meta.dimensions.y, meta.dimensions.z,
                    iteration,
                    meta.engine.as_str(),
                    if meta.watertight { "yes" } else { "no" },
                    if meta.features.is_empty() { String::new() } else {
                        format!("\n\nFeatures:\n{}", meta.features.iter().map(|f| format!("  - {f}")).collect::<Vec<_>>().join("\n"))
                    }
                );
                self.model_summary = model_summary;

                if let Some(ref src) = stl_path {
                    self.push_event(CoreEvent::BuildArtifact { stl: src.clone() });
                }

                self.session.save(self.phase);
                self.refresh_projects();
            }
            python::BuildResult::BuildError(e) | python::BuildResult::SyntaxError(e) => {
                self.push_message("system", &format!("Build error: {}", e.error));
            }
            python::BuildResult::Timeout => {
                self.push_message("system", "Build timed out.");
            }
        }
        self.claude.busy = BusyState::Idle;
    }

    // ---- session / project navigation ------------------------------------

    pub(crate) fn load_session(&mut self, project_idx: usize, session_name: String) {
        if let Some(project) = self.projects.get(project_idx) {
            let session_dir = project.path.join(&session_name);

            match self.session.load(&session_dir, self.build_timeout, self.python_path.clone()) {
                Ok(()) => {
                    self.new_project_pending = false;
                    self.new_session_pending = false;
                    self.save_part_pending = false;
                    self.rename_pending = None;
                    self.delete_pending = None;
                    self.ref_confirm_pending = None;

                    let phase = self.session.phase_session.as_ref()
                        .map(|ps| ps.phase)
                        .unwrap_or(Phase::Spec);

                    self.phase = phase;

                    let entries: Vec<(String, String)> = self.session.conversations(phase)
                        .iter()
                        .map(|e| (e.role.clone(), e.content.clone()))
                        .collect();
                    self.reset_conversation(entries);
                    self.push_message("system", &format!(
                        "Resumed session '{}' in {} phase.", session_name, phase.label()
                    ));

                    if phase == Phase::Build {
                        self.push_message("system",
                            "Tip: If the last build was interrupted, type 'undo' to restore the previous state.");
                    }

                    self.restore_right_panel(&session_dir);

                    self.session.project_idx = Some(project_idx);
                    self.session.active_name = Some(session_name.clone());

                    self.refresh_projects();

                    // No `BuildArtifact` event here: pre-refactor `load_session`
                    // only pointed the viewer at the session dir and launched it
                    // when `_buffer.stl` already existed (no STL copy, no model
                    // panel update). The TUI reproduces that from
                    // `session_dir()` after `load_session` returns.
                }
                Err(e) => {
                    self.push_message("system", &format!("Failed to load session: {e}"));
                }
            }
        }
    }

    pub(crate) fn open_project(&mut self, project_idx: usize) {
        if let Some(project) = self.projects.get(project_idx).cloned() {
            self.new_project_pending = false;
            self.new_session_pending = false;
            self.save_part_pending = false;
            self.rename_pending = None;
            self.delete_pending = None;
            self.ref_confirm_pending = None;

            self.session.project_idx = Some(project_idx);
            self.session.active_name = None;
            self.session.active_dir = None;
            self.session.phase_session = None;
            self.claude.session_id = None;

            self.reset_conversation(Vec::new());

            let name = &project.meta.name;
            let desc = if project.meta.description.is_empty() {
                String::new()
            } else {
                format!("\n{}", project.meta.description)
            };
            self.push_message("system", &format!("Project: {name}{desc}"));

            if project.sessions.is_empty() {
                self.push_message("system", "No sessions yet. Type a prompt to start building.");
            } else {
                let mut session_info = String::from("Sessions:");
                for si in &project.sessions {
                    let sname = &si.name;
                    let session_path = project.path.join(sname);
                    let status = storage::session::session_status(&session_path);
                    let detail = match status {
                        storage::session::SessionStatus::Ok { phase, created } => {
                            let date = created.split('T').next().unwrap_or(&created);
                            format!("  {sname}  ({phase}, {date})")
                        }
                        storage::session::SessionStatus::Empty => {
                            format!("  {sname}  (empty)")
                        }
                        storage::session::SessionStatus::Corrupted => {
                            format!("  {sname}  (corrupted)")
                        }
                    };
                    session_info.push_str(&format!("\n{detail}"));
                }
                self.push_message("system", &session_info);
            }

            let mut parts: Vec<String> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&project.path) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("stl") {
                        if let Some(name) = path.file_stem().and_then(|n| n.to_str()) {
                            parts.push(name.to_string());
                        }
                    }
                }
            }
            if !parts.is_empty() {
                parts.sort();
                let parts_list = parts.iter().map(|p| format!("  {p}.stl")).collect::<Vec<_>>().join("\n");
                self.push_message("system", &format!("Saved parts:\n{parts_list}"));
            }

            let doc_names = ["README.md", "readme.md", "NOTES.md", "notes.md", "notes.txt", "docs.md"];
            for doc_name in &doc_names {
                let doc_path = project.path.join(doc_name);
                if doc_path.exists() {
                    if let Ok(content) = std::fs::read_to_string(&doc_path) {
                        let preview: String = content.lines().take(20).collect::<Vec<_>>().join("\n");
                        self.push_message("system", &format!("{doc_name}:\n{preview}"));
                    }
                }
            }

            // Open latest STL in the viewer if any session has one.
            let mut latest_stl: Option<PathBuf> = None;
            for si in project.sessions.iter().rev() {
                let sname = &si.name;
                let session_path = project.path.join(sname);
                if let Ok(entries) = std::fs::read_dir(&session_path) {
                    let mut stls: Vec<PathBuf> = entries.flatten()
                        .map(|e| e.path())
                        .filter(|p| {
                            p.file_name()
                                .and_then(|n| n.to_str())
                                .map(|n| n.starts_with("iter_") && n.ends_with(".stl"))
                                .unwrap_or(false)
                        })
                        .collect();
                    stls.sort();
                    if let Some(stl) = stls.last() {
                        latest_stl = Some(stl.clone());
                        break;
                    }
                }
            }
            if let Some(stl) = latest_stl {
                self.push_event(CoreEvent::BuildArtifact { stl });
            }
        }
    }

    /// Import a STEP file: ensure session exists, copy into it, run MCP import,
    /// display results in conversation.
    pub(crate) fn import_step_file(&mut self, source: &std::path::Path) {
        let filename = source.file_name().unwrap_or_default().to_string_lossy().to_string();

        if self.session.active_name.is_none() {
            let stem = source.file_stem().unwrap_or_default().to_string_lossy();
            let session_name: String = stem.chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .take(30)
                .collect();
            let session_name = if session_name.is_empty() { "imported".to_string() } else { session_name };
            let project_path = self.session.project_idx
                .and_then(|idx| self.projects.get(idx))
                .map(|p| p.path.clone())
                .unwrap_or_else(|| storage::project::root_dir().join("Untitled"));
            let session_dir = project_path.join(&session_name);
            self.session.active_name = Some(session_name);
            self.session.active_dir = Some(session_dir);
        }

        if self.session.phase_session.is_none() {
            if let Some(dir) = self.session.active_dir.clone() {
                self.session.create(dir, self.build_timeout, self.python_path.clone(), None);
            }
        }

        let session_dir = match self.session.active_dir {
            Some(ref d) => d.clone(),
            None => {
                self.push_message("system", "No session directory available.");
                return;
            }
        };

        let target_dir = session_dir.join("imported");
        let _ = std::fs::create_dir_all(&target_dir);
        let dest_step = target_dir.join("imported.step");
        if let Err(e) = std::fs::copy(source, &dest_step) {
            self.push_message("system", &format!("Failed to copy STEP: {e}"));
            return;
        }

        self.push_message("system", &format!("Importing {filename}..."));

        let build_code = format!(
            "import cadquery as cq\nresult = cq.importers.importStep(\"{}\")",
            dest_step.to_string_lossy().replace('\\', "/")
        );

        let stl_path = target_dir.join("result.stl");
        let step_path = target_dir.join("result.step");
        let export_code = format!(
            "{build_code}\n\nimport cadquery as cq\ncq.exporters.export(result, \"{}\")\ncq.exporters.export(result, \"{}\")\nbb = result.val().BoundingBox()\nprint(f\"DIMS:{{bb.xlen:.2f}}x{{bb.ylen:.2f}}x{{bb.zlen:.2f}}\")",
            stl_path.to_string_lossy().replace('\\', "/"),
            step_path.to_string_lossy().replace('\\', "/"),
        );

        let proc = std::process::Command::new(&self.python_path)
            .arg("-c")
            .arg(&export_code)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output();

        match proc {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let dims = stdout.lines()
                    .find(|l| l.starts_with("DIMS:"))
                    .map(|l| &l[5..])
                    .unwrap_or("unknown");

                if stl_path.exists() {
                    self.push_event(CoreEvent::BuildArtifact { stl: stl_path.clone() });
                }

                self.push_message("system", &format!(
                    "Imported {filename} ({dims}mm)\nCopied to imported/imported.step\nModel loaded in viewer.\n\nYou can now describe changes, or type 'advance' to work on it."
                ));

                self.phase = Phase::Build;

                self.session.save(self.phase);
                self.refresh_projects();
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                self.push_message("system", &format!("Import build failed:\n{}", &stderr[..stderr.len().min(500)]));
            }
            Err(e) => {
                self.push_message("system", &format!("Failed to run Python: {e}"));
            }
        }
    }

    pub(crate) fn refresh_projects(&mut self) {
        self.projects = storage::project::list_projects().unwrap_or_default();
    }

    /// Rebuild the Refs tab content from the current active_refs list.
    pub(crate) fn refresh_refs_panel(&mut self) {
        let library = reference::load_library().unwrap_or_default();
        if self.active_refs.is_empty() {
            self.refs_summary = "No references loaded. Use /ref <name> to load.".to_string();
            return;
        }
        let mut lines = Vec::new();
        lines.push(format!("Active references ({}):", self.active_refs.len()));
        for slug in &self.active_refs {
            if let Some((comp, _)) = library.iter().find(|(_, s)| s == slug) {
                lines.push(format!(
                    "  {} — {} [{}]",
                    slug, comp.identity.name, comp.identity.category
                ));
            } else {
                lines.push(format!("  {slug} (not in library)"));
            }
        }
        self.refs_summary = lines.join("\n");
    }

    /// Restore the right panel tabs (Spec, Refs, Model) from session files on disk.
    fn restore_right_panel(&mut self, session_dir: &std::path::Path) {
        let narrative_path = session_dir.join("spec_narrative.md");
        let goal_path = session_dir.join("goal.md");
        let spec_path = session_dir.join("spec.toml");
        if narrative_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&narrative_path) {
                self.spec_content = content;
            }
        } else if goal_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&goal_path) {
                self.spec_content = content;
            }
        } else if spec_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&spec_path) {
                self.spec_content = content;
            }
        }

        self.active_refs.clear();
        let ref_dir = reference::references_dir();
        if ref_dir.exists() {
            if let Some(ref ps) = self.session.phase_session {
                for (_, entries) in &ps.conversations {
                    for entry in entries {
                        if entry.role == "system" && entry.content.contains("Loaded reference") {
                            if let Ok(library) = reference::load_library() {
                                for (_, slug) in &library {
                                    if entry.content.contains(slug) && !self.active_refs.contains(slug) {
                                        self.active_refs.push(slug.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let all_convos = self.session.conversations(Phase::Spec);
            for entry in all_convos {
                if entry.content.starts_with("/ref ") {
                    let name = entry.content.strip_prefix("/ref ").unwrap_or("").trim();
                    if let Ok((_, slug)) = reference::load_one(name) {
                        if !self.active_refs.contains(&slug) {
                            self.active_refs.push(slug);
                        }
                    }
                }
            }
        }
        self.refresh_refs_panel();

        if let Some(stl_path) = self.session.latest_stl_path() {
            let size_kb = std::fs::metadata(&stl_path).map(|m| m.len() / 1024).unwrap_or(0);
            let mut model_info = format!("Latest build: {} ({size_kb}KB)", stl_path.file_name().unwrap_or_default().to_string_lossy());

            if let Some(code) = self.find_latest_code_py() {
                let line_count = code.lines().count();
                let params: Vec<&str> = code.lines()
                    .filter(|l| {
                        let trimmed = l.trim();
                        trimmed.contains('=') && !trimmed.starts_with('#') && {
                            let name = trimmed.split('=').next().unwrap_or("").trim();
                            name == name.to_uppercase() && name.len() > 1 && name.chars().all(|c| c.is_alphanumeric() || c == '_')
                        }
                    })
                    .collect();
                model_info.push_str(&format!("\nCode: {line_count} lines, {} parameters", params.len()));
                if !params.is_empty() {
                    for p in params.iter().take(10) {
                        model_info.push_str(&format!("\n  {}", p.trim()));
                    }
                }
            }
            self.model_summary = model_info;
        }
    }

    /// Find the latest code.py in the session (refinement > assembly > components).
    fn find_latest_code_py(&self) -> Option<String> {
        let dir = self.session.active_dir.as_ref()?;
        for subdir in &["refinement", "assembly"] {
            let code_path = dir.join(subdir).join("code.py");
            if code_path.exists() {
                return std::fs::read_to_string(&code_path).ok();
            }
        }
        let comp_dir = dir.join("components");
        if comp_dir.is_dir() {
            let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
            if let Ok(entries) = std::fs::read_dir(&comp_dir) {
                for entry in entries.flatten() {
                    let code_path = entry.path().join("code.py");
                    if code_path.exists() {
                        if let Ok(meta) = std::fs::metadata(&code_path) {
                            let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                            if best.as_ref().map_or(true, |(t, _)| mtime > *t) {
                                best = Some((mtime, code_path));
                            }
                        }
                    }
                }
            }
            if let Some((_, path)) = best {
                return std::fs::read_to_string(&path).ok();
            }
        }
        let imported = dir.join("imported").join("code.py");
        if imported.exists() {
            return std::fs::read_to_string(&imported).ok();
        }
        None
    }

    /// Build context about prior component builds for Assembly/Refinement phases.
    fn build_prior_builds_context(&self) -> Option<String> {
        let session_dir = self.session.active_dir.as_ref()?;
        let comp_dir = session_dir.join("components");
        if !comp_dir.exists() { return None; }

        let mut lines = vec!["## Built Components".to_string()];
        let mut found = false;

        if let Ok(entries) = std::fs::read_dir(&comp_dir) {
            let mut dirs: Vec<_> = entries.flatten().filter(|e| e.path().is_dir()).collect();
            dirs.sort_by_key(|e| e.file_name());

            for entry in dirs {
                let path = entry.path();
                let id = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                let step = path.join("result.step");
                let code = path.join("code.py");
                if step.exists() {
                    found = true;
                    lines.push(format!("  {id}:"));
                    lines.push(format!("    STEP: components/{id}/result.step"));
                    if code.exists() {
                        lines.push(format!("    Code: components/{id}/code.py"));
                    }
                }
            }
        }

        if !found { return None; }
        lines.push("\nUse read_file to examine component code for exact dimensions and positioning.".to_string());
        Some(lines.join("\n"))
    }

    /// Dispatch an MCP tool call from Claude's stream to the appropriate handler.
    fn handle_tool_call(&mut self, tool: &ToolCall) {
        let name = tool.name.strip_prefix("mcp__mimodel__").unwrap_or(&tool.name);

        match name {
            "ask_question" | "ask_clarification" => {
                if let Some(q) = tool.input.get("question").and_then(|v| v.as_str()) {
                    self.session.add_message(self.phase, "assistant", q);
                    self.push_message("assistant", q);
                }
            }
            "record_spec_field" => {
                let cat = tool.input.get("category").and_then(|v| v.as_str()).unwrap_or("");
                let key = tool.input.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let val = tool.input.get("value").and_then(|v| v.as_str()).unwrap_or("");
                let unit = tool.input.get("unit").and_then(|v| v.as_str()).unwrap_or("");
                let entry = format!("[{}] {} = {} {}", cat, key, val, unit);
                let mut content = self.spec_content.clone();
                if !content.is_empty() { content.push('\n'); }
                content.push_str(&entry);
                self.spec_content = content;
            }
            "mark_spec_complete" => {
                if let Some(ref dir) = self.session.active_dir {
                    let goal_path = dir.join("goal.md");
                    if goal_path.exists() {
                        if let Ok(goal) = std::fs::read_to_string(&goal_path) {
                            let narrative = self.spec_content.clone();
                            let combined = if narrative.is_empty() {
                                goal
                            } else {
                                format!("{}\n\n---\n\n## Spec Discussion\n{}", goal, narrative)
                            };
                            self.spec_content = combined.clone();
                            let narrative_path = dir.join("spec_narrative.md");
                            let _ = std::fs::write(&narrative_path, &combined);
                        }
                    }
                }
                self.push_message("system", "Spec complete. Type 'advance' to move to Decompose phase.");
                self.session.add_message(self.phase, "system", "Spec complete.");
            }
            "propose_component_tree" => {
                if let Some(components) = tool.input.get("components").and_then(|v| v.as_array()) {
                    let mut lines = Vec::new();
                    for c in components {
                        let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                        let cname = c.get("name").and_then(|v| v.as_str()).unwrap_or(id);
                        let op = c.get("assembly_op").and_then(|v| v.as_str()).unwrap_or("union");
                        lines.push(format!("  {} -- {} [{}]", id, cname, op));
                    }
                    self.push_message("system",
                        &format!("Component tree proposed:\n{}\nType 'approve' to accept, or describe changes.",
                            lines.join("\n")));
                }
            }
            "write_file" => {
                // Viewer launch (if not already running) is handled by the TUI
                // from the `CoreEvent::ToolCall{name: "write_file", ..}` event
                // that `poll_events` always emits alongside this dispatch — the
                // pre-refactor handler only ever called `viewer.show()` here,
                // never `update_working_stl`, so no CoreEvent is queued.
                if let Some(path) = tool.input.get("path").and_then(|v| v.as_str()) {
                    if path.ends_with(".py") && (path.starts_with("components/") || path.starts_with("assembly/") || path.starts_with("refinement/")) {
                        self.model_summary = "Build complete -- check 3D viewer".to_string();
                    }
                }
            }
            "request_approval" => {
                if let Some(summary) = tool.input.get("summary").and_then(|v| v.as_str()) {
                    self.push_message("system",
                        &format!("Review model in viewer. {}\nType 'approve' or describe changes.", summary));
                }
            }
            "update_parameter" => {
                let pname = tool.input.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let new_val = tool.input.get("new_value").and_then(|v| v.as_str()).unwrap_or("");
                let mut content = self.spec_content.clone();
                content.push_str(&format!("\nUpdated: {} = {}", pname, new_val));
                self.spec_content = content;
            }
            "open_viewer" => {
                // Pre-refactor behavior: unconditionally `viewer.show()` if not
                // already running, regardless of whether a session STL is
                // registered. Handled by the TUI off the `CoreEvent::ToolCall`
                // event `poll_events` always emits for this dispatch.
            }
            _ => {} // Unknown tool -- ignore
        }
    }

    /// Render a short human-readable detail string for a tool call, used by
    /// `poll_events`'s `CoreEvent::ToolCall`.
    fn describe_tool_call(tool: &ToolCall) -> String {
        match serde_json::to_string(&tool.input) {
            Ok(s) if s != "null" && s != "{}" => s,
            _ => String::new(),
        }
    }

    // ---- phase-specific senders (moved from src/phase_dispatch.rs) ------
    // (see src/phase_dispatch.rs for send_spec_prompt/send_build_prompt/
    // send_refine_prompt/handle_spec_response/handle_param_edit/
    // handle_export/undo_component/try_switch_phase)

    // ---- event polling ----------------------------------------------------

    /// Drain everything that happened since the last call: queued synchronous
    /// events, streamed text, tool calls, and a completed background result.
    /// Also polls the `.building` busy-state signal and the `.open_viewer`
    /// file signal that the MCP server writes.
    pub fn poll_events(&mut self) -> Vec<CoreEvent> {
        let mut events: Vec<CoreEvent> = self.pending_events.drain(..).collect();

        for chunk in self.claude.drain_streaming() {
            events.push(CoreEvent::StreamDelta(chunk));
        }

        if let Some(result) = self.claude.try_recv_result() {
            self.claude.streaming_text.clear();
            self.handle_bg_result(result);
            events.extend(self.pending_events.drain(..));
        }

        let tool_calls = self.claude.drain_tool_calls();
        for tc in tool_calls {
            let name = tc.name.strip_prefix("mcp__mimodel__").unwrap_or(&tc.name).to_string();
            let detail = Self::describe_tool_call(&tc);
            self.handle_tool_call(&tc);
            events.extend(self.pending_events.drain(..));
            events.push(CoreEvent::ToolCall { name, detail });
        }

        // Poll .building file for BusyState transitions. These produce no
        // `CoreEvent` but must still repaint the spinner, exactly as the
        // pre-refactor loop's `app.dirty = true` did.
        if self.claude.busy == BusyState::Thinking {
            if let Some(ref dir) = self.session.active_dir {
                if dir.join(".building").exists() {
                    self.claude.busy = BusyState::Building;
                    self.repaint_requested = true;
                }
            }
        } else if self.claude.busy == BusyState::Building {
            if let Some(ref dir) = self.session.active_dir {
                if !dir.join(".building").exists() {
                    self.claude.busy = BusyState::Thinking;
                    self.repaint_requested = true;
                }
            }
        }

        // Poll .open_viewer signal from the MCP server.
        if let Some(ref dir) = self.session.active_dir {
            let signal = dir.join(".open_viewer");
            if signal.exists() {
                let _ = std::fs::remove_file(&signal);
                let working_stl = dir.join("_buffer.stl");
                if working_stl.exists() {
                    events.push(CoreEvent::BuildArtifact { stl: working_stl });
                }
                // Repaint unconditionally, even when no STL exists yet —
                // matches the pre-refactor `app.dirty = true` on this path.
                self.repaint_requested = true;
            }
        }

        events
    }
}

/// Decode percent-encoded URI path (e.g. %20 -> space).
pub(crate) fn percent_decode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                result.push(byte as char);
            } else {
                result.push('%');
                result.push_str(&hex);
            }
        } else {
            result.push(ch);
        }
    }
    result
}

/// Seed ~/MiModel/references/ with common components on first run.
fn seed_references() {
    let dir = reference::references_dir();
    if dir.exists() && std::fs::read_dir(&dir).map(|mut d| d.next().is_some()).unwrap_or(false) {
        return; // Already has references
    }
    let _ = reference::ensure_references_dir();

    let seeds: &[(&str, &str)] = &[
        ("m3_shcs.toml", include_str!("../../references/m3_shcs.toml")),
        ("m3x5x4_threaded_insert.toml", include_str!("../../references/m3x5x4_threaded_insert.toml")),
    ];
    for (name, content) in seeds {
        let path = dir.join(name);
        if !path.exists() {
            let _ = std::fs::write(&path, content);
        }
    }
}

/// Extract a session/project name from briefing content.
fn briefing_name(content: &str) -> String {
    let line = content.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| {
            for prefix in &["User:", "Human:", "Assistant:", "AI:"] {
                if let Some(rest) = l.strip_prefix(prefix) {
                    return rest.trim();
                }
            }
            l
        })
        .find(|l| !l.is_empty())
        .unwrap_or("briefing");

    let name: String = line.chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ')
        .take(30)
        .collect();
    let name = name.trim().replace(' ', "_");
    if name.is_empty() { "briefing".to_string() } else { name }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_bridge::{CapturedPrompt, Dispatch};
    use tempfile::TempDir;

    use crate::test_util::HOME_LOCK;

    fn with_test_home<T>(f: impl FnOnce() -> T) -> T {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());
        let result = f();
        // Restore rather than unset: an unset HOME makes `dirs::home_dir()`
        // fall back to the passwd entry, i.e. the developer's real ~/MiModel.
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        result
    }

    /// Build a core that never touches the network: the usage refresh is
    /// disabled and prompt dispatch is captured instead of spawning the
    /// `claude` CLI. Both are ordinary runtime parameters, so these tests run
    /// the same constructor and dispatch code the binary does.
    fn test_core(briefing: Option<String>) -> AppCore {
        let mut core = AppCore::new_with(Config::load(), briefing, UsageRefresh::Disabled)
            .expect("AppCore::new_with should succeed");
        core.claude.dispatch = Dispatch::Capture(Vec::new());
        core
    }

    fn captured(core: &AppCore) -> &[CapturedPrompt] {
        match core.claude.dispatch {
            Dispatch::Capture(ref log) => log,
            Dispatch::Subprocess => panic!("core was not built with a capturing dispatch"),
        }
    }

    #[test]
    fn new_core_starts_in_spec_phase() {
        with_test_home(|| {
            let core = AppCore::new_with(Config::load(), None, UsageRefresh::Disabled)
                .expect("AppCore::new_with should succeed");
            assert_eq!(core.phase(), Phase::Spec);
        });
    }

    #[test]
    fn try_switch_phase_switches_and_returns_ok() {
        with_test_home(|| {
            let mut core = test_core(None);
            let result = core.try_switch_phase(Phase::Build);
            assert_eq!(result, Ok(()));
            assert_eq!(core.phase(), Phase::Build);
        });
    }

    #[test]
    fn try_switch_phase_resets_session_id_and_logs_the_switch() {
        with_test_home(|| {
            let mut core = test_core(None);
            core.claude.session_id = Some("sid-123".to_string());
            let before = core.messages().len();

            assert_eq!(core.try_switch_phase(Phase::Refine), Ok(()));

            assert_eq!(core.claude.session_id, None, "phase switch starts a fresh Claude session");
            let messages = core.messages();
            assert_eq!(messages.len(), before + 1);
            assert_eq!(messages.last().unwrap().0, "system");
            assert_eq!(messages.last().unwrap().1, "Switched to Refine phase");
        });
    }

    #[test]
    fn try_switch_phase_same_phase_is_denied() {
        with_test_home(|| {
            let mut core = test_core(None);
            core.claude.session_id = Some("sid-123".to_string());
            let current = core.phase();
            let before = core.messages().len();

            let result = core.try_switch_phase(current);

            assert_eq!(result, Err(SwitchDenied::SamePhase));
            assert_eq!(core.phase(), current);
            assert_eq!(core.claude.session_id, Some("sid-123".to_string()));
            assert_eq!(core.messages().len(), before, "denied switch logs nothing");
        });
    }

    #[test]
    fn poll_events_on_idle_core_is_empty() {
        with_test_home(|| {
            let mut core = test_core(None);
            let events = core.poll_events();
            assert!(events.is_empty());
        });
    }

    #[test]
    fn poll_events_open_viewer_signal_emits_build_artifact_and_repaint() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));
            let dir = core.session_dir().unwrap().to_path_buf();
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("_buffer.stl"), b"solid x\nendsolid x\n").unwrap();
            std::fs::write(dir.join(".open_viewer"), b"").unwrap();

            let events = core.poll_events();

            assert!(!dir.join(".open_viewer").exists(), "signal file is consumed");
            match events.as_slice() {
                [CoreEvent::BuildArtifact { stl }] => assert_eq!(stl, &dir.join("_buffer.stl")),
                other => panic!("expected one BuildArtifact, got {other:?}"),
            }
            assert!(core.take_repaint_request());
        });
    }

    #[test]
    fn poll_events_open_viewer_signal_without_stl_still_requests_repaint() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));
            let dir = core.session_dir().unwrap().to_path_buf();
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(".open_viewer"), b"").unwrap();

            let events = core.poll_events();

            assert!(events.is_empty(), "no STL yet, so nothing to show the viewer");
            assert!(core.take_repaint_request(), "the signal must still repaint the UI");
        });
    }

    #[test]
    fn new_with_briefing_creates_project_and_starts_in_spec_phase() {
        with_test_home(|| {
            let core = AppCore::new_with(
                Config::load(),
                Some("User: Build me a bracket.".to_string()),
                UsageRefresh::Disabled,
            )
            .expect("AppCore::new_with with briefing should succeed");
            assert_eq!(core.phase(), Phase::Spec);
            assert!(core.session_dir().is_some());
            let dir = core.session_dir().unwrap();
            assert!(dir.exists());
        });
    }

    #[test]
    fn submit_prompt_dispatches_the_spec_phase_prompt() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));

            core.submit_prompt("40mm wide, 3mm thick", &[], &[]);

            let log = captured(&core);
            assert_eq!(log.len(), 1, "one spec-phase dispatch");
            assert_eq!(log[0].phase_name.as_deref(), Some("spec"));
            assert!(
                log[0].prompt.contains("40mm wide, 3mm thick"),
                "user text reaches the dispatched prompt: {}",
                log[0].prompt
            );
            assert_eq!(core.claude.busy, BusyState::Thinking);

            let messages = core.messages();
            assert_eq!(messages.last().unwrap().0, "user");
            assert_eq!(messages.last().unwrap().1, "40mm wide, 3mm thick");
        });
    }

    #[test]
    fn submit_prompt_attaches_extracted_image_paths() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));
            let dir = core.session_dir().unwrap().to_path_buf();
            let img = dir.join("sketch.png");
            std::fs::write(&img, b"not-really-a-png").unwrap();

            core.submit_prompt(&format!("match this {}", img.display()), &[], &[]);

            let log = captured(&core);
            assert_eq!(log.len(), 1);
            assert_eq!(log[0].images, vec![img.clone()]);
            assert!(
                !log[0].prompt.contains("sketch.png"),
                "the attachment path is stripped from the prompt text"
            );
        });
    }

    #[test]
    fn submit_prompt_in_build_phase_dispatches_the_build_prompt() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));
            assert_eq!(core.try_switch_phase(Phase::Build), Ok(()));

            core.submit_prompt("make it 5mm thick", &[], &[]);

            let log = captured(&core);
            assert_eq!(log.len(), 1);
            assert_eq!(log[0].phase_name.as_deref(), Some("build"));
            assert!(log[0].prompt.contains("make it 5mm thick"));
        });
    }

    #[test]
    fn open_project_bumps_reset_generation_and_replaces_the_log() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));
            core.push_message("user", "prior turn");
            let before = core.reset_generation();

            core.open_project(0);

            assert_ne!(core.reset_generation(), before, "TUI must rebuild its pane");
            let messages = core.messages();
            assert!(
                !messages.iter().any(|(_, c)| c == "prior turn"),
                "the previous conversation is cleared, not appended to"
            );
            assert!(messages.iter().any(|(role, c)| role == "system" && c.starts_with("Project: ")));
        });
    }

    #[test]
    fn new_session_bumps_reset_generation() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));
            core.push_message("user", "prior turn");
            let before = core.reset_generation();
            core.request_new_session();

            // Bare "/attach" returns right after the new-session reset block.
            core.submit_prompt("/attach", &[], &[]);

            assert_ne!(core.reset_generation(), before);
            assert!(captured(&core).is_empty(), "no prompt dispatched by the reset");
        });
    }

    #[test]
    fn submit_prompt_lib_refs_are_deduped_into_active_refs() {
        with_test_home(|| {
            let mut core = test_core(None);
            // Force the "please wait" early return (checked right after the
            // lib_refs dedupe loop) so we never reach phase dispatch.
            core.claude.busy = BusyState::Thinking;
            let refs = vec!["m3_shcs".to_string(), "m3_shcs".to_string(), "bearing_608".to_string()];
            core.submit_prompt("hello", &[], &refs);
            assert_eq!(
                core.active_refs,
                vec!["m3_shcs".to_string(), "bearing_608".to_string()],
                "lib_refs must be deduped into active_refs regardless of dispatch path"
            );
        });
    }

    #[test]
    fn submit_prompt_lib_refs_dedupe_against_existing_active_refs() {
        with_test_home(|| {
            let mut core = test_core(None);
            core.active_refs.push("m3_shcs".to_string());
            core.claude.busy = BusyState::Thinking;
            core.submit_prompt("hello", &[], &["m3_shcs".to_string()]);
            assert_eq!(core.active_refs, vec!["m3_shcs".to_string()]);
        });
    }

    #[test]
    fn submit_prompt_wraps_part_refs_for_dispatch() {
        with_test_home(|| {
            let mut core = test_core(None);

            let part_refs = vec!["lid".to_string(), "base".to_string()];
            core.submit_prompt("make it wider", &part_refs, &[]);

            let log = captured(&core);
            assert_eq!(log.len(), 1, "exactly one captured dispatch");
            assert!(
                log[0].prompt.starts_with(
                    "<selected_part>lid</selected_part>\n<selected_part>base</selected_part>\n\n"
                ),
                "dispatched prompt starts with the wrapped part refs: {}",
                log[0].prompt
            );
            assert!(
                log[0].prompt.contains("make it wider"),
                "dispatched prompt still contains the user's text: {}",
                log[0].prompt
            );

            let messages = core.messages();
            assert_eq!(messages.last().unwrap().0, "user");
            assert_eq!(
                messages.last().unwrap().1,
                "make it wider",
                "conversation-visible message has no <selected_part> tags"
            );

            assert!(core.active_refs.is_empty());
        });
    }

    #[test]
    fn submit_prompt_without_part_refs_is_unchanged() {
        with_test_home(|| {
            let mut core = test_core(None);

            core.submit_prompt("hello", &[], &[]);

            let log = captured(&core);
            assert_eq!(log.len(), 1, "exactly one captured dispatch");
            assert!(log[0].prompt.contains("hello"));
            assert!(
                !log[0].prompt.contains("<selected_part>"),
                "no part_refs means no wrapping: {}",
                log[0].prompt
            );
        });
    }

    #[test]
    fn submit_prompt_refine_export_with_part_refs_still_runs_export_not_dispatch() {
        with_test_home(|| {
            let mut core = test_core(None);
            assert_eq!(core.try_switch_phase(Phase::Refine), Ok(()));

            core.submit_prompt("export", &["lid".to_string()], &[]);

            let log = captured(&core);
            assert!(
                log.is_empty(),
                "export must run handle_export, not be dispatched to Claude: {log:?}"
            );
        });
    }

    #[test]
    fn submit_prompt_refine_set_with_part_refs_still_runs_param_edit_not_dispatch() {
        with_test_home(|| {
            let mut core = test_core(None);
            assert_eq!(core.try_switch_phase(Phase::Refine), Ok(()));

            core.submit_prompt("set WIDTH 42", &["lid".to_string()], &[]);

            let log = captured(&core);
            assert!(
                log.is_empty(),
                "set commands must run handle_param_edit, not be dispatched to Claude: {log:?}"
            );
        });
    }

    #[test]
    fn submit_prompt_new_session_pending_resets_state() {
        with_test_home(|| {
            let mut core = test_core(None);
            let _ = core.try_switch_phase(Phase::Build);
            core.push_message("user", "some prior message");
            core.request_new_session();

            // Bare "/attach" hits its own early-return ("Usage: ...") right
            // after the new_session_pending block runs, so dispatch is never
            // reached while still exercising the reset logic end to end.
            core.submit_prompt("/attach", &[], &[]);

            assert_eq!(core.phase(), Phase::Spec);
            assert!(!core.new_session_pending);
            let messages = core.messages();
            assert_eq!(messages.len(), 2);
            assert_eq!(messages[0].0, "system");
            assert_eq!(messages[0].1, "New session started.");
        });
    }

    #[test]
    fn submit_prompt_busy_core_defers_and_warns() {
        with_test_home(|| {
            let mut core = test_core(None);
            core.claude.busy = BusyState::Thinking;
            core.submit_prompt("hello while busy", &[], &[]);
            let messages = core.messages();
            assert_eq!(messages.last().unwrap().0, "system");
            assert!(messages.last().unwrap().1.contains("wait"));
        });
    }

    #[test]
    fn submit_prompt_save_part_pending_with_no_model_reports_failure() {
        with_test_home(|| {
            let mut core = test_core(None);
            core.request_save_part();
            core.submit_prompt("my_part_name", &[], &[]);
            let messages = core.messages();
            assert_eq!(messages.last().unwrap().1, "No model to save.");
            assert!(!core.save_part_pending);
        });
    }

    // ---- Task 2.2: phase approval gate ---------------------------------

    /// A `test_core` with an active phase session, so `approve_phase` /
    /// `is_phase_approved` have somewhere to read and write.
    fn gated_core(tmp: &TempDir) -> AppCore {
        let mut core = test_core(None);
        core.session.create(
            tmp.path().join("sess"),
            core.build_timeout,
            core.python_path.clone(),
            None,
        );
        core
    }

    #[test]
    fn phase_gate_off_allows_forward_switch_unapproved() {
        with_test_home(|| {
            let tmp = TempDir::new().unwrap();
            let mut core = gated_core(&tmp);
            assert!(!core.phase_gate);
            assert_eq!(core.try_switch_phase(Phase::Build), Ok(()));
            assert_eq!(core.phase(), Phase::Build);
        });
    }

    #[test]
    fn phase_gate_on_unapproved_forward_switch_is_denied() {
        with_test_home(|| {
            let tmp = TempDir::new().unwrap();
            let mut core = gated_core(&tmp);
            core.set_phase_gate(true);

            let result = core.try_switch_phase(Phase::Build);

            assert_eq!(result, Err(SwitchDenied::NotApproved));
            assert_eq!(core.phase(), Phase::Spec, "phase must not change on denial");
        });
    }

    #[test]
    fn phase_gate_on_approved_forward_switch_succeeds() {
        with_test_home(|| {
            let tmp = TempDir::new().unwrap();
            let mut core = gated_core(&tmp);
            core.set_phase_gate(true);

            core.approve_phase();
            assert!(core.is_phase_approved(Phase::Spec));

            let result = core.try_switch_phase(Phase::Build);
            assert_eq!(result, Ok(()));
            assert_eq!(core.phase(), Phase::Build);
        });
    }

    #[test]
    fn phase_gate_on_backward_switch_allowed_while_unapproved() {
        with_test_home(|| {
            let tmp = TempDir::new().unwrap();
            let mut core = gated_core(&tmp);
            core.set_phase_gate(true);

            // Get to Refine without the gate, then flip it on and go back.
            core.set_phase_gate(false);
            assert_eq!(core.try_switch_phase(Phase::Build), Ok(()));
            assert_eq!(core.try_switch_phase(Phase::Refine), Ok(()));
            core.set_phase_gate(true);

            assert!(!core.is_phase_approved(Phase::Refine));
            let result = core.try_switch_phase(Phase::Spec);
            assert_eq!(result, Ok(()));
            assert_eq!(core.phase(), Phase::Spec);
        });
    }

    #[test]
    fn phase_gate_approval_survives_phase_session_save_and_load() {
        with_test_home(|| {
            let tmp = TempDir::new().unwrap();
            let mut core = gated_core(&tmp);
            core.approve_phase();

            let dir = core.session.active_dir.clone().unwrap();
            let build_timeout = core.build_timeout;
            let python_path = core.python_path.clone();
            core.session.phase_session = None;
            core.session
                .load(&dir, build_timeout, python_path)
                .expect("reload should succeed");

            assert!(core.is_phase_approved(Phase::Spec));
        });
    }
}
