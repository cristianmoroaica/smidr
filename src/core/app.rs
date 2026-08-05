//! `AppCore` — UI-free application state and orchestration.
//!
//! Everything that is not rendering (phase state, session ops, prompt
//! submission, ref handling, background result processing). The web server
//! (`src/server/**`) consumes this through the public API at the bottom of
//! this file (`new`, `submit_prompt`, `phase`, `try_switch_phase`,
//! `poll_events`) plus a handful of `pub(crate)` helpers for the sibling
//! `phase_dispatch` module.

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

/// Why a phase switch was refused.
#[derive(Debug, Clone, PartialEq)]
pub enum SwitchDenied {
    SamePhase,
    /// Forward switch attempted while `phase_gate` is on and the current
    /// phase has not been approved via `approve_phase()`.
    NotApproved,
}

/// Events surfaced by `AppCore::poll_events` for the web server to render.
///
/// The server translates each variant into a pinned WebSocket message; see
/// `server::ws` for the wire shapes.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum CoreEvent {
    StreamDelta(String),
    ToolCall { name: String, detail: String },
    ResponseDone,
    BuildArtifact { stl: PathBuf },
    BuildProgress { component: String, status: String },
    Question { question: String, options: Vec<String> },
    PhaseSwitchRequest { target: String, reason: String },
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
    pub(crate) briefing_pending: bool,

    // Interaction-mode flags consumed by submit_prompt.
    pub(crate) new_session_pending: bool,
    pub(crate) new_project_pending: bool,
    pub(crate) save_part_pending: bool,

    /// Spec narrative/goal text, surfaced to the web UI's Spec tab via
    /// `spec_content()`.
    pub(crate) spec_content: String,

    /// Full conversation log (role, content) — persists across a reset.
    messages: Vec<(String, String)>,

    /// Events queued synchronously (e.g. by `undo_component`) that ride
    /// along with the next `poll_events()` call.
    pending_events: Vec<CoreEvent>,

    /// Server-authoritative phase-approval gate (Task 2.2). When `false`
    /// (the TUI's setting, and the default), `try_switch_phase` treats
    /// every phase as pre-approved so Alt+1/2/3 free phase switching keeps
    /// working exactly as before this gate existed. The web server turns
    /// this on via `set_phase_gate(true)`, making `advance` require an
    /// explicit `approve_phase()` call for the current phase first.
    pub(crate) phase_gate: bool,

    /// A question surfaced by `ask_question`/`ask_clarification` that has
    /// not yet been resolved by a user answer. Mirrored to disk via
    /// `session.phase_session`'s `pending_question`. Cleared by any user
    /// prompt, a phase switch, or a wholesale conversation reset.
    pub(crate) pending_question: Option<(String, Vec<String>)>,

    /// A phase change the agent asked the user to make, awaiting the
    /// user's explicit consent (or denial) in the web UI. Mirrored to disk
    /// via `session.phase_session`'s `pending_phase_switch`. Cleared by any
    /// user prompt, a phase switch, a denial, or a wholesale conversation
    /// reset.
    pub(crate) pending_phase_switch: Option<(String, String)>,
}

impl AppCore {
    // ---- construction -------------------------------------------------

    pub fn new(config: Config, briefing: Option<String>) -> Result<AppCore, String> {
        let python_path = config.python_path();
        let build_timeout = config.defaults.build_timeout;
        let mut session = SessionManager::new(build_timeout, python_path.clone());

        // Ensure ~/Smidr/ exists and scan for projects (this also seeds a
        // default "Untitled" project on first run, briefing or not).
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
            briefing_pending: briefing.is_some(),
            new_session_pending: false,
            new_project_pending: false,
            save_part_pending: false,
            spec_content: String::new(),
            messages: Vec::new(),
            pending_events: Vec::new(),
            phase_gate: false,
            pending_question: None,
            pending_phase_switch: None,
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
        self.ensure_session_dir();

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

    /// Append a message to the conversation log.
    pub(crate) fn push_message(&mut self, role: &str, content: &str) {
        self.messages.push((role.to_string(), content.to_string()));
    }

    /// Wholesale-replace the conversation log (new session, session load,
    /// project open).
    fn reset_conversation(&mut self, entries: Vec<(String, String)>) {
        self.messages = entries;
    }

    pub(crate) fn push_event(&mut self, event: CoreEvent) {
        self.pending_events.push(event);
    }

    /// Full conversation log, in order.
    pub fn messages(&self) -> &[(String, String)] {
        &self.messages
    }

    /// The current unresolved question, if any, as `(question, options)`.
    pub fn pending_question(&self) -> Option<&(String, Vec<String>)> {
        self.pending_question.as_ref()
    }

    /// Resolve any pending question: clears in-memory state, mirrors `None`
    /// into the phase session, and persists — but only when there was
    /// something to clear, to avoid gratuitous disk writes.
    pub(crate) fn clear_pending_question(&mut self) {
        if self.pending_question.take().is_none() {
            return;
        }
        if let Some(ref mut ps) = self.session.phase_session {
            ps.pending_question = None;
        }
        self.session.save(self.phase);
    }

    /// The current unresolved phase-switch request, if any, as
    /// `(target, reason)`.
    pub fn pending_phase_switch(&self) -> Option<&(String, String)> {
        self.pending_phase_switch.as_ref()
    }

    /// Resolve any pending phase-switch request: clears in-memory state,
    /// mirrors `None` into the phase session, and persists — but only when
    /// there was something to clear, to avoid gratuitous disk writes.
    pub(crate) fn clear_pending_phase_switch(&mut self) {
        if self.pending_phase_switch.take().is_none() {
            return;
        }
        if let Some(ref mut ps) = self.session.phase_session {
            ps.pending_phase_switch = None;
        }
        self.session.save(self.phase);
    }

    /// The iteration number locked as the Refine-phase ghost-diff baseline,
    /// if any (see "Lock as baseline & refine" in the Build-phase approve
    /// modal).
    pub fn baseline_iteration(&self) -> Option<u32> {
        self.session
            .phase_session
            .as_ref()
            .and_then(|ps| ps.baseline_iteration)
    }

    /// Lock `n` in as the Refine-phase ghost-diff baseline and persist it.
    pub fn set_baseline_iteration(&mut self, n: u32) {
        self.ensure_session_dir();
        if let Some(ref mut ps) = self.session.phase_session {
            ps.baseline_iteration = Some(n);
        }
        self.session.save(self.phase);
    }

    /// Ensure there is an active session directory, lazily creating one
    /// (mirroring `approve_phase`'s pre-existing lazy-creation behaviour)
    /// when the project has none yet. Returns the (now guaranteed) session
    /// dir.
    ///
    /// Guarded against data loss: `self.session.create` installs a brand
    /// new `PhaseSession` and the next `save()` overwrites whatever
    /// `session.json` is already on disk at that path. `phase_session`
    /// being `None` does NOT imply the directory is empty — `active_dir`
    /// can already be `Some` (e.g. a REST handler resolving a project that
    /// was only ever opened via `open_project_by_id`, never loaded into
    /// memory). So when the target directory already has a `session.json`,
    /// load it instead of blowing it away with a fresh one.
    pub(crate) fn ensure_session_dir(&mut self) -> std::path::PathBuf {
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
            if dir.join("session.json").is_file() {
                if self
                    .session
                    .load(&dir, self.build_timeout, self.python_path.clone())
                    .is_err()
                {
                    self.session.create(dir, self.build_timeout, self.python_path.clone(), None);
                } else {
                    // Sync the core-level mirrors of what we just loaded.
                    // Both callers (`approve_phase`, `set_baseline_iteration`)
                    // immediately `save(self.phase)`, so leaving a stale
                    // `self.phase` here would silently downgrade an on-disk
                    // Build/Refine session back to Spec — and then mark the
                    // wrong phase approved.
                    if let Some(ref ps) = self.session.phase_session {
                        self.phase = ps.phase;
                        self.pending_question = ps
                            .pending_question
                            .as_ref()
                            .map(|pq| (pq.question.clone(), pq.options.clone()));
                        self.pending_phase_switch = ps
                            .pending_phase_switch
                            .as_ref()
                            .map(|pps| (pps.target.clone(), pps.reason.clone()));
                    }
                }
            } else {
                self.session.create(dir, self.build_timeout, self.python_path.clone(), None);
            }
            self.refresh_projects();
        }
        self.session
            .active_dir
            .clone()
            .expect("ensure_session_dir always sets active_dir")
    }

    // ---- accessors consumed by the web server ---------------------------

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn spec_content(&self) -> &str {
        &self.spec_content
    }

    pub fn session_dir(&self) -> Option<&std::path::Path> {
        self.session.active_dir.as_deref()
    }

    pub fn cancel(&self) {
        self.claude.cancel();
    }

    pub fn briefing_pending(&self) -> bool {
        self.briefing_pending
    }

    pub fn clear_briefing_pending(&mut self) {
        self.briefing_pending = false;
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
            self.pending_question = None;
            self.pending_phase_switch = None;
            self.push_message("system", "New session started.");
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

        // Add user message to conversation; any user answer resolves a
        // pending question.
        self.clear_pending_question();
        self.clear_pending_phase_switch();
        self.push_message("user", &clean_text);
        self.session.add_message(self.phase, "user", &clean_text);
        self.session.save(self.phase);

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
                .filter(|e| e.role == "user" || e.role == "assistant" || e.role == "question")
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

                    let phase = self.session.phase_session.as_ref()
                        .map(|ps| ps.phase)
                        .unwrap_or(Phase::Spec);

                    self.phase = phase;

                    self.pending_question = self.session.phase_session.as_ref()
                        .and_then(|ps| ps.pending_question.as_ref())
                        .map(|pq| (pq.question.clone(), pq.options.clone()));

                    self.pending_phase_switch = self.session.phase_session.as_ref()
                        .and_then(|ps| ps.pending_phase_switch.as_ref())
                        .map(|pps| (pps.target.clone(), pps.reason.clone()));

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

                    // No `BuildArtifact` event here: loading a session only
                    // makes its directory current. The client picks up any
                    // existing geometry from the snapshot it receives after
                    // `load_session` returns.
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

            self.session.project_idx = Some(project_idx);
            self.session.active_name = None;
            self.session.active_dir = None;
            self.session.phase_session = None;
            self.claude.session_id = None;
            self.pending_question = None;
            self.pending_phase_switch = None;

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
                    "Imported {filename} ({dims}mm)\nCopied to imported/imported.step\nModel loaded in viewer.\n\nYou can now describe changes."
                ));

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

    /// Restore the Spec tab content and active_refs from session files on disk.
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
    pub(crate) fn handle_tool_call(&mut self, tool: &ToolCall) {
        let name = tool.name.strip_prefix("mcp__smidr__").unwrap_or(&tool.name);

        match name {
            "ask_question" | "ask_clarification" => {
                if let Some(q) = tool.input.get("question").and_then(|v| v.as_str()) {
                    let options: Vec<String> = tool.input.get("options")
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                        .unwrap_or_default();

                    self.session.add_message(self.phase, "question", q);
                    self.push_message("question", q);

                    self.pending_question = Some((q.to_string(), options.clone()));
                    if let Some(ref mut ps) = self.session.phase_session {
                        ps.pending_question = Some(crate::storage::session::PendingQuestion {
                            question: q.to_string(),
                            options: options.clone(),
                        });
                    }
                    self.session.save(self.phase);

                    self.push_event(CoreEvent::Question { question: q.to_string(), options });
                }
            }
            "request_phase_change" => {
                let target = tool.input.get("target").and_then(|v| v.as_str()).unwrap_or("");
                let reason = tool.input.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                let target_lower = target.to_lowercase();
                let reason_trimmed = reason.trim();
                let valid_target = matches!(target_lower.as_str(), "spec" | "build" | "refine");
                if valid_target && !reason_trimmed.is_empty() {
                    self.pending_phase_switch = Some((target_lower.clone(), reason_trimmed.to_string()));
                    if let Some(ref mut ps) = self.session.phase_session {
                        ps.pending_phase_switch = Some(crate::storage::session::PendingPhaseSwitch {
                            target: target_lower.clone(),
                            reason: reason_trimmed.to_string(),
                        });
                    }
                    self.session.save(self.phase);

                    self.push_event(CoreEvent::PhaseSwitchRequest {
                        target: target_lower,
                        reason: reason_trimmed.to_string(),
                    });
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
                self.push_message("system", "Spec complete. Approve the spec, then advance to the Build phase.");
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
                // No-op: the viewer is always on screen in the web UI, so
                // there is nothing to launch. `poll_events` still emits the
                // `CoreEvent::ToolCall` for this dispatch so the call shows
                // up in the conversation log.
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
            let name = tc.name.strip_prefix("mcp__smidr__").unwrap_or(&tc.name).to_string();
            let detail = Self::describe_tool_call(&tc);
            self.handle_tool_call(&tc);
            events.extend(self.pending_events.drain(..));
            events.push(CoreEvent::ToolCall { name, detail });
        }

        for bp in self.claude.drain_build_progress() {
            events.push(CoreEvent::BuildProgress { component: bp.component, status: bp.status });
        }

        // Poll .building file for BusyState transitions. These produce no
        // `CoreEvent` but must still repaint the spinner, exactly as the
        // pre-refactor loop's `app.dirty = true` did.
        if self.claude.busy == BusyState::Thinking {
            if let Some(ref dir) = self.session.active_dir {
                if dir.join(".building").exists() {
                    self.claude.busy = BusyState::Building;
                }
            }
        } else if self.claude.busy == BusyState::Building {
            if let Some(ref dir) = self.session.active_dir {
                if !dir.join(".building").exists() {
                    self.claude.busy = BusyState::Thinking;
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
            }
        }

        events
    }
}

/// Seed ~/Smidr/references/ with common components on first run.
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
        // fall back to the passwd entry, i.e. the developer's real ~/Smidr.
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        result
    }

    /// Build a core with prompt dispatch captured instead of spawning the
    /// `claude` CLI, so these tests never touch the network. Otherwise this
    /// runs the same constructor and dispatch code the binary does.
    fn test_core(briefing: Option<String>) -> AppCore {
        let mut core =
            AppCore::new(Config::load(), briefing).expect("AppCore::new should succeed");
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
            let core = AppCore::new(Config::load(), None).expect("AppCore::new should succeed");
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
        });
    }

    #[test]
    fn poll_events_open_viewer_signal_without_stl_emits_nothing() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));
            let dir = core.session_dir().unwrap().to_path_buf();
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(".open_viewer"), b"").unwrap();

            let events = core.poll_events();

            assert!(events.is_empty(), "no STL yet, so nothing to show the viewer");
            assert!(!dir.join(".open_viewer").exists(), "signal file is still consumed");
        });
    }

    #[test]
    fn new_with_briefing_creates_project_and_starts_in_spec_phase() {
        with_test_home(|| {
            let core = AppCore::new(
                Config::load(),
                Some("User: Build me a bracket.".to_string()),
            )
            .expect("AppCore::new with briefing should succeed");
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
    fn open_project_replaces_the_conversation_log() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));
            core.push_message("user", "prior turn");

            core.open_project(0);

            let messages = core.messages();
            assert!(
                !messages.iter().any(|(_, c)| c == "prior turn"),
                "the previous conversation is cleared, not appended to"
            );
            assert!(messages.iter().any(|(role, c)| role == "system" && c.starts_with("Project: ")));
        });
    }

    #[test]
    fn new_session_pending_resets_the_conversation_log() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));
            core.push_message("user", "prior turn");
            core.new_session_pending = true;

            // Bare "/attach" returns right after the new-session reset block.
            core.submit_prompt("/attach", &[], &[]);

            let messages = core.messages();
            assert!(
                !messages.iter().any(|(_, c)| c == "prior turn"),
                "the previous conversation is cleared, not appended to"
            );
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
            core.new_session_pending = true;

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
    fn submit_prompt_lib_refs_feed_reference_summary_into_the_prompt() {
        with_test_home(|| {
            let mut core = test_core(None);

            core.submit_prompt("make a mount", &[], &["m3_shcs".to_string()]);

            let log = captured(&core);
            assert_eq!(log.len(), 1, "exactly one captured dispatch");
            let ref_context = log[0]
                .ref_context
                .as_ref()
                .expect("lib_refs must populate ref_context");
            assert!(
                ref_context.contains("## Active Reference Components"),
                "ref_context missing active-components header: {ref_context}"
            );
            let (m3_component, _slug) =
                reference::load_one("m3_shcs").expect("seeded m3_shcs reference");
            assert!(
                ref_context.contains(&m3_component.identity.name),
                "ref_context missing m3_shcs component name: {ref_context}"
            );
        });
    }

    #[test]
    fn submit_prompt_advance_text_is_dispatched_not_a_phase_change() {
        with_test_home(|| {
            let mut core = test_core(None);
            assert_eq!(core.phase(), Phase::Spec);

            core.submit_prompt("advance", &[], &[]);

            assert_eq!(core.phase(), Phase::Spec, "'advance' text no longer changes phase");
            let log = captured(&core);
            assert_eq!(log.len(), 1, "exactly one captured dispatch");
            assert!(
                log[0].prompt.contains("advance"),
                "the literal text 'advance' is dispatched to Claude like any other prompt: {}",
                log[0].prompt
            );
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
            core.save_part_pending = true;
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

    #[test]
    fn ask_question_tool_call_records_a_question_and_sets_pending_question() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));

            let tool = ToolCall {
                name: "mcp__smidr__ask_question".to_string(),
                input: serde_json::json!({
                    "question": "How tall?",
                    "options": ["10mm", "20mm"],
                }),
            };
            core.handle_tool_call(&tool);

            assert_eq!(
                core.pending_question(),
                Some(&("How tall?".to_string(), vec!["10mm".to_string(), "20mm".to_string()]))
            );

            let messages = core.messages();
            assert!(
                messages.iter().any(|(role, content)| role == "question" && content == "How tall?"),
                "expected a 'question' role message, got: {messages:?}"
            );
            assert!(
                !messages.iter().any(|(role, content)| role == "assistant" && content == "How tall?"),
                "the question text must not also land as an 'assistant' message: {messages:?}"
            );
        });
    }

    #[test]
    fn ask_question_tool_call_persists_and_survives_reload() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));

            let tool = ToolCall {
                name: "mcp__smidr__ask_question".to_string(),
                input: serde_json::json!({
                    "question": "How tall?",
                    "options": ["10mm", "20mm"],
                }),
            };
            core.handle_tool_call(&tool);

            // Mirrored into the in-memory phase session...
            let mirrored = core
                .session
                .phase_session
                .as_ref()
                .and_then(|ps| ps.pending_question.as_ref())
                .expect("pending_question should be mirrored onto phase_session");
            assert_eq!(mirrored.question, "How tall?");
            assert_eq!(mirrored.options, vec!["10mm".to_string(), "20mm".to_string()]);

            // ...and written to disk, restorable via a fresh load.
            let dir = core.session.active_dir.clone().expect("active session dir");
            let reloaded = crate::model_session::PhaseSession::load(
                &dir,
                core.build_timeout,
                core.python_path.clone(),
            )
            .expect("session should reload");
            let reloaded_pq = reloaded
                .pending_question
                .expect("pending_question should survive save/load");
            assert_eq!(reloaded_pq.question, "How tall?");
            assert_eq!(reloaded_pq.options, vec!["10mm".to_string(), "20mm".to_string()]);

            // And load_session (the AppCore-level reload path) restores the
            // in-memory `(question, options)` mirror too.
            core.session.phase_session = None;
            core.pending_question = None;
            let idx = core.session.project_idx.unwrap_or(0);
            let name = core.session.active_name.clone().expect("active session name");
            core.load_session(idx, name);
            assert_eq!(
                core.pending_question(),
                Some(&("How tall?".to_string(), vec!["10mm".to_string(), "20mm".to_string()]))
            );
        });
    }

    #[test]
    fn ask_question_tool_call_without_options_yields_empty_options() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));

            let tool = ToolCall {
                name: "mcp__smidr__ask_question".to_string(),
                input: serde_json::json!({ "question": "How tall?" }),
            };
            core.handle_tool_call(&tool);

            assert_eq!(
                core.pending_question(),
                Some(&("How tall?".to_string(), Vec::<String>::new()))
            );
        });
    }

    #[test]
    fn submit_prompt_resolves_a_pending_question() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));

            let tool = ToolCall {
                name: "mcp__smidr__ask_question".to_string(),
                input: serde_json::json!({
                    "question": "How tall?",
                    "options": ["10mm", "20mm"],
                }),
            };
            core.handle_tool_call(&tool);
            assert!(core.pending_question().is_some());

            core.submit_prompt("20mm", &[], &[]);

            assert!(core.pending_question().is_none());
        });
    }

    #[test]
    fn request_phase_change_tool_call_sets_pending_phase_switch() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));

            let tool = ToolCall {
                name: "mcp__smidr__request_phase_change".to_string(),
                input: serde_json::json!({
                    "target": "Build",
                    "reason": "that is a functional change",
                }),
            };
            core.handle_tool_call(&tool);

            assert_eq!(
                core.pending_phase_switch(),
                Some(&("build".to_string(), "that is a functional change".to_string()))
            );

            let mirrored = core
                .session
                .phase_session
                .as_ref()
                .and_then(|ps| ps.pending_phase_switch.as_ref())
                .expect("pending_phase_switch should be mirrored onto phase_session");
            assert_eq!(mirrored.target, "build");
            assert_eq!(mirrored.reason, "that is a functional change");
        });
    }

    #[test]
    fn request_phase_change_tool_call_rejects_an_invalid_phase_switch_target() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));

            let tool = ToolCall {
                name: "mcp__smidr__request_phase_change".to_string(),
                input: serde_json::json!({
                    "target": "foo",
                    "reason": "that is a functional change",
                }),
            };
            core.handle_tool_call(&tool);

            assert!(core.pending_phase_switch().is_none());
            assert!(
                core.session
                    .phase_session
                    .as_ref()
                    .and_then(|ps| ps.pending_phase_switch.as_ref())
                    .is_none()
            );
        });
    }

    #[test]
    fn request_phase_change_tool_call_rejects_an_empty_or_whitespace_phase_switch_reason() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));

            let tool = ToolCall {
                name: "mcp__smidr__request_phase_change".to_string(),
                input: serde_json::json!({ "target": "build", "reason": "" }),
            };
            core.handle_tool_call(&tool);
            assert!(core.pending_phase_switch().is_none());

            let tool = ToolCall {
                name: "mcp__smidr__request_phase_change".to_string(),
                input: serde_json::json!({ "target": "build", "reason": "   " }),
            };
            core.handle_tool_call(&tool);
            assert!(core.pending_phase_switch().is_none());
        });
    }

    #[test]
    fn request_phase_change_tool_call_normalizes_phase_switch_target_and_reason_on_the_wire() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));

            let tool = ToolCall {
                name: "mcp__smidr__request_phase_change".to_string(),
                input: serde_json::json!({
                    "target": "Build",
                    "reason": "  that is a functional change  ",
                }),
            };
            core.handle_tool_call(&tool);

            let events: Vec<CoreEvent> = core.poll_events();
            let found = events.iter().any(|e| matches!(
                e,
                CoreEvent::PhaseSwitchRequest { target, reason }
                    if target == "build" && reason == "that is a functional change"
            ));
            assert!(found, "expected a case-normalized, trimmed PhaseSwitchRequest event, got: {events:?}");
        });
    }

    #[test]
    fn request_phase_change_tool_call_pending_phase_switch_persists_and_survives_reload() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));

            let tool = ToolCall {
                name: "mcp__smidr__request_phase_change".to_string(),
                input: serde_json::json!({
                    "target": "build",
                    "reason": "that is a functional change",
                }),
            };
            core.handle_tool_call(&tool);

            let dir = core.session.active_dir.clone().expect("active session dir");
            let reloaded = crate::model_session::PhaseSession::load(
                &dir,
                core.build_timeout,
                core.python_path.clone(),
            )
            .expect("session should reload");
            let reloaded_ps = reloaded
                .pending_phase_switch
                .expect("pending_phase_switch should survive save/load");
            assert_eq!(reloaded_ps.target, "build");
            assert_eq!(reloaded_ps.reason, "that is a functional change");

            // load_session (the AppCore-level reload path) restores the
            // in-memory (target, reason) mirror too.
            core.session.phase_session = None;
            core.pending_phase_switch = None;
            let idx = core.session.project_idx.unwrap_or(0);
            let name = core.session.active_name.clone().expect("active session name");
            core.load_session(idx, name);
            assert_eq!(
                core.pending_phase_switch(),
                Some(&("build".to_string(), "that is a functional change".to_string()))
            );
        });
    }

    #[test]
    fn submit_prompt_resolves_a_pending_phase_switch() {
        with_test_home(|| {
            let mut core = test_core(Some("User: Build me a bracket.".to_string()));

            let tool = ToolCall {
                name: "mcp__smidr__request_phase_change".to_string(),
                input: serde_json::json!({
                    "target": "build",
                    "reason": "that is a functional change",
                }),
            };
            core.handle_tool_call(&tool);
            assert!(core.pending_phase_switch().is_some());

            core.submit_prompt("keep going in spec", &[], &[]);

            assert!(core.pending_phase_switch().is_none());
        });
    }
}
