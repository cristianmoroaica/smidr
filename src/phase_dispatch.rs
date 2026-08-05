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
    // -- Spec phase --

    pub(crate) fn send_spec_prompt(&mut self, text: &str, images: Vec<PathBuf>) {
        let ref_context = self.build_ref_context();

        // Build briefing context if available (only on first message — subsequent turns skip this)
        let briefing_context = if self.claude.session_id.is_none() {
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

        let prompt = if self.claude.session_id.is_some() {
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
        self.claude.send_phase_prompt("spec", &prompt, &images, ref_context.as_deref(), mcp_config);
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
                    &format!("Reference available: {} (use /ref {} to load)", det.name,
                        reference::slug_from_name(&det.name)));
            } else {
                self.push_message("system",
                    &format!("Detected component: {}. Use /ref {} to research and save.",
                        det.name, reference::slug_from_name(&det.name)));
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
        self.claude.send_phase_prompt("build", text, &images, ctx.as_deref(), mcp_config);
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
        self.claude.send_phase_prompt("refine", text, &images, ctx.as_deref(), mcp_config);
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


    pub(crate) fn handle_export(&mut self) {
        if let Some(ref session_dir) = self.session.active_dir {
            if let Some(stl_path) = self.session.latest_stl_path() {
                let export_stl = session_dir.join("export.stl");
                match std::fs::copy(&stl_path, &export_stl) {
                    Ok(_) => {
                        self.push_message("system", &format!("Exported to {}", export_stl.display()));
                    }
                    Err(e) => {
                        self.push_message("system", &format!("Export failed: {e}"));
                    }
                }
            } else {
                self.push_message("system", "No model to export.");
            }
        } else {
            self.push_message("system", "No active session directory for export.");
        }
    }

    pub(crate) fn undo_component(&mut self) {
        if self.session.undo() {
            self.push_message("system", "Undid last component iteration.");
            if let Some(meta) = self.session.current_metadata.clone() {
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
                // Pre-refactor `undo_component` only refreshed the viewer's
                // working-copy STL — it never called `viewer.show()`. Queue a
                // refresh-only signal rather than `CoreEvent::BuildArtifact`
                // (which the TUI treats as "update AND auto-launch").
                if let Some(src) = stl_path {
                    self.queue_stl_refresh(src);
                }
            }
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
