//! Phase-specific dispatch methods for `AppCore`.
//!
//! Handles per-phase send, response, and phase-navigation methods. These
//! remain `AppCore` methods but live in a separate file, matching the
//! original layout when they were `impl App` methods in main.rs.

use std::path::PathBuf;

use crate::claude_bridge;
use crate::core::app::{AppCore, SwitchDenied};
use crate::phase::Phase;
use crate::{reference, reference_detect};

impl AppCore {
    /// Persisted conversation for `phase` as `(role, content)` pairs, for
    /// the OpenAI-compat engine's message history (the Claude CLI path
    /// ignores this parameter entirely). The just-pushed user prompt is
    /// dropped from the tail if present — `OpenAiTurn` appends `prompt`
    /// itself, so including it here would duplicate the final user turn.
    ///
    /// The persisted entry is the plain (`clean_text`) user message, while
    /// `prompt` handed to `send_phase_prompt` may be a decorated superstring
    /// of it (`<selected_part>` wrapping, spec briefing prefix, `[Reference
    /// context]` prefix — see `send_spec_prompt`/`AppCore::submit_prompt`).
    /// Dropping on `prompt.ends_with(&last.1)` rather than exact equality
    /// catches every decorated case, since the persisted text is always the
    /// tail of what actually gets sent. (Callers may equivalently pass the
    /// undecorated `text`, since every decoration is a *prefix*: whatever
    /// suffix relation holds for `prompt` holds for `text` too.)
    fn phase_history(&self, phase: Phase, prompt: &str) -> Vec<(String, String)> {
        let mut history: Vec<(String, String)> = self
            .session
            .conversations(phase)
            .iter()
            .map(|entry| (entry.role.clone(), entry.content.clone()))
            .collect();
        if let Some(last) = history.last() {
            if last.0 == "user" && !last.1.is_empty() && prompt.ends_with(last.1.as_str()) {
                history.pop();
            }
        }
        history
    }

    // -- Spec phase --

    pub(crate) fn send_spec_prompt(&mut self, text: &str, images: Vec<PathBuf>) {
        let ref_context = self.build_ref_context();
        let history = self.phase_history(Phase::Spec, text);

        // Whether this turn must carry the briefing block (and skip the
        // in-prompt "[Reference context]" prefix). The condition is
        // per-engine, because "does the model already know the briefing?"
        // means different things on each:
        //
        // * Claude CLI keeps conversation state server-side keyed by
        //   `session_id`, and `session_id == None` means the next invocation
        //   starts a BRAND NEW session with zero memory. That happens on
        //   every reconnect/restart (`open_project`) and every phase switch
        //   (`try_switch_phase`), not just on the literal first turn — so the
        //   briefing must be re-sent exactly then. This is the historical
        //   behavior and must stay byte-for-byte identical.
        // * The OpenAI-compat engine is stateless: every turn re-sends the
        //   reconstructed `history`, so "the model has seen prior turns" is
        //   exactly `!history.is_empty()`. It never sets `session_id`, so a
        //   session_id check there would re-prepend the briefing forever.
        let is_first_turn = match self.claude.engine {
            claude_bridge::EngineKind::ClaudeCli => self.claude.session_id.is_none(),
            claude_bridge::EngineKind::OpenAiCompat { .. } => history.is_empty(),
        };

        // Build briefing context if available (only on first message — subsequent turns skip this)
        let briefing_context = if is_first_turn {
            self.session.phase_session.as_ref()
                .and_then(|ps| ps.briefing.as_ref())
                .and_then(|rel_path| {
                    let dir = self.session.active_dir.as_ref()?;
                    let path = dir.join(rel_path);
                    std::fs::read_to_string(&path).ok()
                })
                .map(|content| {
                    format!(
                        "## Prior Conversation (Briefing)\n\n\
                         The user has provided a prior conversation that describes what they want to build.\n\
                         Use this to pre-fill spec fields where the information is clear.\n\
                         Ask about gaps or ambiguities — do not assume.\n\n\
                         <briefing>\n{}\n</briefing>",
                        content
                    )
                })
        } else {
            None
        };

        let prompt = if !is_first_turn {
            // Continuing session — only add ref context (briefing already sent on first message)
            if let Some(ref ctx) = ref_context {
                format!("[Reference context]\n{}\n\n{}", ctx, text)
            } else {
                text.to_string()
            }
        } else {
            // First message — include briefing if available, but NOT ref_context
            // (ref_context on first message is handled separately by send_phase_prompt)
            if let Some(ref bc) = briefing_context {
                format!("{}\n\n{}", bc, text)
            } else {
                text.to_string()
            }
        };

        let session_dir = self.session.active_dir.clone();
        let mcp_config = claude_bridge::generate_mcp_config(
            "spec", session_dir.as_deref()
        ).ok();
        self.claude.send_phase_prompt(
            "spec", &prompt, &images, ref_context.as_deref(), &history, session_dir, mcp_config,
        );
    }

    pub(crate) fn handle_spec_response(&mut self, response: &str) {
        // With MCP tools, code-block stripping and SPEC_COMPLETE detection are no longer
        // needed — structure is enforced by tool availability. This handler now just:
        // 1. Runs reference detection on Claude's freeform text
        // 2. Appends to the spec panel for visibility

        // Auto-detect external component references
        let known_slugs: Vec<String> = reference::load_library()
            .unwrap_or_default()
            .iter()
            .map(|(_, slug)| slug.clone())
            .collect();
        let detected = reference_detect::detect_references(response, &known_slugs);
        for det in &detected {
            if det.in_library {
                self.push_message("system",
                    &format!("Reference available: {} (add it with the /ref picker)", det.name));
            } else {
                self.push_message("system",
                    &format!("Detected component: {} — not in the reference library.", det.name));
            }
        }

        // Append Claude's text to the spec panel for visibility
        let mut spec_content = self.spec_content.clone();
        if !spec_content.is_empty() {
            spec_content.push_str("\n\n");
        }
        spec_content.push_str(response);
        self.spec_content = spec_content.clone();

        // Persist the full spec narrative so it survives session reloads.
        // goal.md only captures structured fields; this preserves the full
        // design discussion (dimensions, rationale, context).
        if let Some(ref dir) = self.session.active_dir {
            let narrative_path = dir.join("spec_narrative.md");
            let _ = std::fs::write(&narrative_path, &spec_content);
        }
    }

    // -- Build phase --

    pub(crate) fn send_build_prompt(&mut self, text: &str, images: Vec<PathBuf>) {
        let session_dir = self.session.active_dir.clone();
        let mcp_config = claude_bridge::generate_mcp_config(
            "build", session_dir.as_deref()
        ).ok();
        let ctx = self.build_phase_context();
        let history = self.phase_history(Phase::Build, text);
        self.claude.send_phase_prompt(
            "build", text, &images, ctx.as_deref(), &history, session_dir, mcp_config,
        );
    }

    // -- Refine phase --

    pub(crate) fn send_refine_prompt(&mut self, text: &str, images: Vec<PathBuf>) {
        let trimmed = text.trim().to_lowercase();

        if trimmed.starts_with("set ") {
            self.handle_param_edit(text);
            return;
        }
        if trimmed == "export" {
            self.handle_export();
            return;
        }

        let session_dir = self.session.active_dir.clone();
        let mcp_config = claude_bridge::generate_mcp_config(
            "refine", session_dir.as_deref()
        ).ok();
        let ctx = self.build_phase_context();
        let history = self.phase_history(Phase::Refine, text);
        self.claude.send_phase_prompt(
            "refine", text, &images, ctx.as_deref(), &history, session_dir, mcp_config,
        );
    }

    pub(crate) fn handle_param_edit(&mut self, text: &str) {
        // Parse "set PARAM_NAME value" format
        let parts: Vec<&str> = text.trim().splitn(3, ' ').collect();
        if parts.len() < 3 {
            self.push_message("system", "Usage: set PARAM_NAME value (e.g., 'set OUTER_DIAMETER 42.0')");
            return;
        }

        let param_name = parts[1].to_uppercase();
        let value: f64 = match parts[2].parse() {
            Ok(v) => v,
            Err(_) => {
                self.push_message("system", &format!("Invalid number: {}", parts[2]));
                return;
            }
        };

        self.push_message("system", &format!(
            "Parameter edit: {} = {} (zero-Claude rebuild)", param_name, value
        ));

        // In the future, this will:
        // 1. Write params JSON
        // 2. Call python::paramset()
        // 3. Rebuild assembly
        // 4. Update viewer
        // For now, just acknowledge the change
        self.push_message("system", "Parameter edit acknowledged. Full paramset integration pending PhaseSession wiring.");
    }


    /// Copy everything a user takes away from a session into
    /// `<session>/exports/`: the latest iteration's geometry as
    /// `assembly.stl` (and, when a `_buffer.step` is present,
    /// `assembly.step`), plus one `<component>.stl`/`<component>.step` pair
    /// per `components/<name>/` subdirectory (from that directory's
    /// `result.stl`/`result.step`, whichever exist). Returns the exports dir
    /// and the list of file names actually written (basenames, deduped,
    /// `assembly.stl`/`assembly.step` first, then components in directory
    /// name order). `Err` when there is no active session dir or no STL to
    /// export.
    ///
    /// A component literally named `assembly` will overwrite
    /// `assembly.stl`/`assembly.step` — acceptable for this personal tool.
    pub(crate) fn export_artifacts(&mut self) -> Result<(PathBuf, Vec<String>), String> {
        let session_dir = self
            .session
            .active_dir
            .clone()
            .ok_or_else(|| "No active session directory for export.".to_string())?;
        let stl_path = self
            .session
            .latest_stl_path()
            .ok_or_else(|| "No model to export.".to_string())?;

        let exports_dir = session_dir.join("exports");
        std::fs::create_dir_all(&exports_dir).map_err(|e| format!("Export failed: {e}"))?;

        let assembly_stl = exports_dir.join("assembly.stl");
        std::fs::copy(&stl_path, &assembly_stl).map_err(|e| format!("Export failed: {e}"))?;
        let mut written = vec!["assembly.stl".to_string()];

        let buffer_step = session_dir.join("_buffer.step");
        if buffer_step.exists() {
            let assembly_step = exports_dir.join("assembly.step");
            match std::fs::copy(&buffer_step, &assembly_step) {
                Ok(_) => written.push("assembly.step".to_string()),
                // A STEP that exists but cannot be copied is omitted from
                // `written` (callers only ever advertise files actually on
                // disk) — log it so the omission is diagnosable rather than
                // silent.
                Err(e) => eprintln!("export: failed to copy {}: {e}", buffer_step.display()),
            }
        }

        let components_dir = session_dir.join("components");
        if let Ok(entries) = std::fs::read_dir(&components_dir) {
            let mut names: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|name| {
                    !name.is_empty()
                        && !name.starts_with('.')
                        && !name.contains('/')
                        && !name.contains('\\')
                })
                .collect();
            names.sort();

            for name in names {
                let comp_dir = components_dir.join(&name);

                let result_stl = comp_dir.join("result.stl");
                if result_stl.exists() {
                    let dest = exports_dir.join(format!("{name}.stl"));
                    match std::fs::copy(&result_stl, &dest) {
                        Ok(_) => written.push(format!("{name}.stl")),
                        Err(e) => eprintln!("export: failed to copy {}: {e}", result_stl.display()),
                    }
                }

                let result_step = comp_dir.join("result.step");
                if result_step.exists() {
                    let dest = exports_dir.join(format!("{name}.step"));
                    match std::fs::copy(&result_step, &dest) {
                        Ok(_) => written.push(format!("{name}.step")),
                        Err(e) => eprintln!("export: failed to copy {}: {e}", result_step.display()),
                    }
                }
            }
        }

        let mut seen = std::collections::HashSet::new();
        written.retain(|name| seen.insert(name.clone()));

        Ok((exports_dir, written))
    }

    pub(crate) fn handle_export(&mut self) {
        match self.export_artifacts() {
            Ok((exports_dir, _written)) => {
                self.push_message("system", &format!("Exported to {}", exports_dir.display()));
            }
            Err(e) => {
                self.push_message("system", &e);
            }
        }
    }

    pub(crate) fn undo_component(&mut self) {
        if self.session.undo() {
            self.push_message("system", "Undid last component iteration.");
        } else {
            self.push_message("system", "Nothing to undo.");
        }
    }

    // -- Phase navigation --

    /// Attempt to switch to a different phase.
    ///
    /// Same phase is always denied. Moving to a LOWER phase index (going
    /// back) is always allowed. Moving to a HIGHER phase index is allowed
    /// only when the approval gate is off (`phase_gate == false`, the TUI's
    /// setting) or the current phase has been approved
    /// (`is_phase_approved`); otherwise it is denied with `NotApproved` and
    /// NO state is mutated (phase unchanged, no session reset, no message,
    /// no save).
    pub fn try_switch_phase(&mut self, target: Phase) -> Result<(), SwitchDenied> {
        if target == self.phase {
            return Err(SwitchDenied::SamePhase);
        }
        if target.index() > self.phase.index()
            && self.phase_gate
            && !self.is_phase_approved(self.phase)
        {
            return Err(SwitchDenied::NotApproved);
        }
        self.phase = target;
        self.clear_pending_question();
        self.clear_pending_phase_switch();
        // Force fresh Claude session so phase-specific system prompt and context
        // (spec conversation, goal.md, references) are re-injected. Without this,
        // --resume would continue the previous phase's session and silently drop
        // the new phase's context.
        self.claude.session_id = None;
        // Add system message about phase change
        self.push_message("system", &format!("Switched to {} phase", target.label()));
        self.session.save(self.phase);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_bridge::Dispatch;
    use crate::config::Config;
    use crate::test_util::HOME_LOCK;
    use tempfile::TempDir;

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

    fn test_core(briefing: Option<String>) -> AppCore {
        let mut core =
            AppCore::new(Config::load(), briefing).expect("AppCore::new should succeed");
        core.claude.dispatch = Dispatch::Capture(Vec::new());
        core
    }

    #[test]
    fn phase_history_drops_a_decorated_trailing_user_entry() {
        with_test_home(|| {
            let mut core = test_core(None);
            core.ensure_session_dir();
            core.session.add_message(Phase::Spec, "user", "how wide should it be?");

            // The dispatched prompt is a decorated superstring of the
            // persisted plain text — e.g. wrapped in <selected_part> lines,
            // or prefixed with the spec briefing / "[Reference context]"
            // block (see `AppCore::submit_prompt` / `send_spec_prompt`).
            let decorated = format!(
                "<selected_part>bracket</selected_part>\n\nhow wide should it be?"
            );
            let history = core.phase_history(Phase::Spec, &decorated);

            assert!(
                history.is_empty(),
                "the trailing user entry must be dropped so OpenAiTurn doesn't send it twice: {history:?}"
            );
        });
    }

    #[test]
    fn phase_history_keeps_prior_turns_and_only_drops_the_matching_tail() {
        with_test_home(|| {
            let mut core = test_core(None);
            core.ensure_session_dir();
            core.session.add_message(Phase::Spec, "user", "first question");
            core.session.add_message(Phase::Spec, "assistant", "first answer");
            core.session.add_message(Phase::Spec, "user", "second question");

            let history = core.phase_history(Phase::Spec, "second question");

            assert_eq!(
                history,
                vec![
                    ("user".to_string(), "first question".to_string()),
                    ("assistant".to_string(), "first answer".to_string()),
                ]
            );
        });
    }

    #[test]
    fn phase_history_keeps_the_trailing_entry_when_prompt_does_not_end_with_it() {
        with_test_home(|| {
            let mut core = test_core(None);
            core.ensure_session_dir();
            core.session.add_message(Phase::Spec, "user", "unrelated stored text");

            // `prompt` here is NOT a superstring of the stored entry (this
            // should not happen in practice, but the method must not drop
            // history it can't prove is a duplicate).
            let history = core.phase_history(Phase::Spec, "a completely different prompt");

            assert_eq!(history, vec![("user".to_string(), "unrelated stored text".to_string())]);
        });
    }

    /// Set up a core with a briefing file wired in and Capture dispatch.
    /// (Bypasses the `AppCore::new` briefing-project bootstrap, which isn't
    /// needed to exercise `send_spec_prompt`'s gating logic.)
    fn briefed_core() -> AppCore {
        let mut core = test_core(None);
        core.ensure_session_dir();
        core.claude.dispatch = Dispatch::Capture(Vec::new());
        let dir = core.session.active_dir.clone().expect("active dir");
        std::fs::write(dir.join("briefing.md"), "A widget briefing").unwrap();
        if let Some(ref mut ps) = core.session.phase_session {
            ps.briefing = Some("briefing.md".to_string());
        }
        core
    }

    fn captured(core: &AppCore) -> &[crate::claude_bridge::CapturedPrompt] {
        let Dispatch::Capture(log) = &core.claude.dispatch else {
            panic!("dispatch should still be Capture");
        };
        log
    }

    #[test]
    fn claude_cli_briefing_gating_stays_session_id_based() {
        with_test_home(|| {
            let mut core = briefed_core();
            assert!(matches!(core.claude.engine, claude_bridge::EngineKind::ClaudeCli));

            core.send_spec_prompt("first question", Vec::new());
            // The real turn records the conversation and a claude session id.
            core.session.add_message(Phase::Spec, "user", "first question");
            core.session.add_message(Phase::Spec, "assistant", "some reply");
            core.claude.session_id = Some("sess-1".to_string());

            core.send_spec_prompt("second question", Vec::new());

            // Reconnect / phase switch: `open_project` and `try_switch_phase`
            // both clear session_id, so the NEXT claude invocation starts a
            // brand new session with zero memory — the briefing must be
            // re-sent, exactly as it was before local engines existed.
            core.claude.session_id = None;
            core.send_spec_prompt("third question", Vec::new());

            let log = captured(&core);
            assert_eq!(log.len(), 3);
            assert!(log[0].prompt.contains("Prior Conversation (Briefing)"));
            assert!(
                !log[1].prompt.contains("Prior Conversation (Briefing)"),
                "a resumed claude session already has the briefing: {:?}",
                log[1].prompt
            );
            assert!(
                log[2].prompt.contains("Prior Conversation (Briefing)"),
                "session_id was reset (reconnect / phase switch) so claude starts a \
                 fresh session with no memory — the briefing MUST be re-sent: {:?}",
                log[2].prompt
            );
        });
    }

    #[test]
    fn claude_cli_does_not_duplicate_ref_context_into_the_prompt_after_a_reset() {
        with_test_home(|| {
            let mut core = briefed_core();
            core.session.add_message(Phase::Spec, "user", "first question");
            core.session.add_message(Phase::Spec, "assistant", "some reply");
            // session_id stays None (fresh claude session after reconnect).

            core.send_spec_prompt("next question", Vec::new());

            let log = captured(&core);
            // `ref_context` is passed to `send_phase_prompt` separately and
            // lands in the SYSTEM prompt; the user prompt must never also
            // carry a "[Reference context]" block on a fresh claude session.
            assert!(
                !log[0].prompt.contains("[Reference context]"),
                "ref context must not be duplicated into the user prompt: {:?}",
                log[0].prompt
            );
        });
    }

    #[test]
    fn openai_engine_briefing_gating_is_history_based() {
        with_test_home(|| {
            let mut core = briefed_core();
            core.claude.engine = claude_bridge::EngineKind::OpenAiCompat {
                endpoint: crate::engine_config::EndpointConfig {
                    name: "local".to_string(),
                    kind: crate::engine_config::EndpointKind::OpenAi,
                    base_url: "http://127.0.0.1:1/v1".to_string(),
                    api_key: None,
                },
                model: "gpt-oss".to_string(),
            };
            // The OpenAI-compat engine never sets claude.session_id, so a
            // session_id check could never distinguish first from Nth turn.
            assert!(core.claude.session_id.is_none());

            core.send_spec_prompt("first question", Vec::new());
            core.session.add_message(Phase::Spec, "user", "first question");
            core.session.add_message(Phase::Spec, "assistant", "some reply");

            core.send_spec_prompt("second question", Vec::new());

            let log = captured(&core);
            assert_eq!(log.len(), 2);
            assert!(
                log[0].prompt.contains("Prior Conversation (Briefing)"),
                "empty history ⇒ the model has never seen the briefing: {:?}",
                log[0].prompt
            );
            assert!(
                !log[1].prompt.contains("Prior Conversation (Briefing)"),
                "the reconstructed history already carries the prior turns, so the \
                 briefing block must not be re-prepended every message: {:?}",
                log[1].prompt
            );
        });
    }

    #[test]
    fn successful_phase_switch_clears_a_pending_question() {
        with_test_home(|| {
            let mut core = test_core(None);
            core.pending_question = Some(("How tall?".to_string(), vec!["10mm".to_string()]));

            assert_eq!(core.try_switch_phase(Phase::Build), Ok(()));

            assert!(core.pending_question().is_none());
        });
    }

    #[test]
    fn successful_phase_switch_clears_pending_question_on_disk() {
        with_test_home(|| {
            let mut core = test_core(Some("A widget briefing".to_string()));
            core.pending_question = Some(("How tall?".to_string(), vec!["10mm".to_string()]));
            if let Some(ref mut ps) = core.session.phase_session {
                ps.pending_question = Some(crate::storage::session::PendingQuestion {
                    question: "How tall?".to_string(),
                    options: vec!["10mm".to_string()],
                });
            }
            core.session.save(core.phase);

            assert_eq!(core.try_switch_phase(Phase::Build), Ok(()));

            // In-memory mirror cleared...
            assert!(core.pending_question().is_none());
            // ...and the disk copy too: reload the session and check.
            let dir = core.session.active_dir.clone().expect("active session dir");
            let reloaded = crate::model_session::PhaseSession::load(
                &dir,
                core.build_timeout,
                core.python_path.clone(),
            )
            .expect("session should reload");
            assert!(reloaded.pending_question.is_none());
        });
    }

    #[test]
    fn denied_phase_switch_preserves_a_pending_question() {
        with_test_home(|| {
            let mut core = test_core(None);
            core.pending_question = Some(("How tall?".to_string(), vec!["10mm".to_string()]));
            core.set_phase_gate(true); // current phase (Spec) is not approved

            let result = core.try_switch_phase(Phase::Build);

            assert_eq!(result, Err(SwitchDenied::NotApproved));
            assert_eq!(
                core.pending_question(),
                Some(&("How tall?".to_string(), vec!["10mm".to_string()]))
            );
        });
    }

    #[test]
    fn successful_phase_switch_request_clears_a_pending_phase_switch() {
        with_test_home(|| {
            let mut core = test_core(None);
            core.pending_phase_switch =
                Some(("build".to_string(), "that is a functional change".to_string()));

            assert_eq!(core.try_switch_phase(Phase::Build), Ok(()));

            assert!(core.pending_phase_switch().is_none());
        });
    }

    #[test]
    fn successful_phase_switch_request_clears_pending_phase_switch_on_disk() {
        with_test_home(|| {
            let mut core = test_core(Some("A widget briefing".to_string()));
            // Go through the actual tool-call handler so the mirror-into-
            // phase_session + session.save path is exercised, not just the
            // clear path.
            let tool = crate::claude_bridge::ToolCall {
                name: "mcp__smidr__request_phase_change".to_string(),
                input: serde_json::json!({
                    "target": "build",
                    "reason": "that is a functional change",
                }),
            };
            core.handle_tool_call(&tool);
            assert!(core.pending_phase_switch().is_some());

            assert_eq!(core.try_switch_phase(Phase::Build), Ok(()));

            // In-memory mirror cleared...
            assert!(core.pending_phase_switch().is_none());
            // ...and the disk copy too: reload the session and check.
            let dir = core.session.active_dir.clone().expect("active session dir");
            let reloaded = crate::model_session::PhaseSession::load(
                &dir,
                core.build_timeout,
                core.python_path.clone(),
            )
            .expect("session should reload");
            assert!(reloaded.pending_phase_switch.is_none());
        });
    }

    #[test]
    fn denied_phase_switch_request_preserves_a_pending_phase_switch() {
        with_test_home(|| {
            let mut core = test_core(None);
            core.pending_phase_switch =
                Some(("build".to_string(), "that is a functional change".to_string()));
            core.set_phase_gate(true); // current phase (Spec) is not approved

            let result = core.try_switch_phase(Phase::Build);

            assert_eq!(result, Err(SwitchDenied::NotApproved));
            assert_eq!(
                core.pending_phase_switch(),
                Some(&("build".to_string(), "that is a functional change".to_string()))
            );
        });
    }
}
