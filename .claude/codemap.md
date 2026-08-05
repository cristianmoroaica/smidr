# Codemap: AI3D (MiModel)
commit: 587e71e | generated: 2026-08-05 | source: jcodemunch

## Purpose
Ratatui TUI (`mimodel`) orchestrating the Claude CLI through a 3-phase pipeline (Spec→Build→Refine) to generate CadQuery/OpenSCAD 3D models from natural language, verified against an auto-generated `goal.md` checklist.
Python side (`ai3d_cad` package + `mcp/server.py`) is the MCP tool server the spawned Claude CLI subprocess calls to build/assemble/analyze STL/STEP geometry.

## Layout
- `src/` — Rust TUI binary (`mimodel`), 33 files, ~11.2k LOC. Entry: `src/main.rs` (642 lines, TUI-only after refactor).
  - `src/core/app.rs` (2158 lines) — `AppCore`: all non-rendering state/logic (phase state, session mgmt, Claude CLI interaction, refs, background-result processing), extracted from old `main.rs`. `src/core/mod.rs` re-exports it plus `BackgroundResult`.
  - `src/tui/` — ratatui widgets/panes: `conversation.rs`, `spec_panel.rs`, `right_panel.rs`, `model_panel.rs`, `project_tree.rs`, `input_bar.rs`, `layout.rs`, `status_bar.rs`.
  - `src/storage/` — on-disk project/session persistence (`project.rs`, `session.rs`).
  - `claude.rs`/`claude_bridge.rs` (CLI subprocess + streaming bridge), `phase_dispatch.rs` (`impl AppCore` — per-phase prompt senders, moved off `App`), `phase.rs` (3-phase enum: Spec/Build/Refine, with legacy Decompose/Component/Assembly/Refinement deserialize aliases), `spec.rs`, `component.rs` (domain models), `parser.rs` (response parsing), `prompt_builder.rs` (prompt assembly), `python.rs` (subprocess into `ai3d_cad`), `session_manager.rs` (build orchestration), `preview.rs`/`render.rs`/`viewer.rs`/`stl.rs` (STL preview + external viewer), `reference.rs`/`reference_detect.rs` (`/ref` library), `usage.rs`, `image.rs` (clipboard/attachments), `event_handler.rs` (`impl App` keybindings, delegates to `AppCore`).
- `mcp/server.py` — MCP tool server exposing phase-gated tools to the Claude CLI subprocess (build, assemble, analyze, scan_model, import_step, goal doc generation).
- `python/src/ai3d_cad/` — CadQuery/OpenSCAD execution engine: `builder.py` (build/validate), `assembler.py` (boolean assembly), `openscad.py`, `paramset.py`, `analyzer.py`.
- `prompts/` — phase system prompts (`spec.md`, `build.md`, `refine.md`) + `prompts/knowledge/*.md` engineering reference docs injected into prompts.
- `references/*.toml` — hardware reference specs (screws, inserts) used by `/ref`.
- `python/tests/`, `src/**` inline `#[cfg(test)]` — colocated unit tests (`src/core/app.rs` alone carries ~30 inline tests for `AppCore`).

## Entry points & data flow
1. `src/main.rs::main` → `Config::load` → `startup_checks` → `App::new` (wraps `AppCore::new`) → `run_event_loop` (ratatui terminal loop).
2. `run_event_loop` polls crossterm events → `event_handler::handle_key` (dispatches to per-pane handlers on `App`, which forward into `AppCore`) or `App::submit` on Enter.
3. `App::submit` → `AppCore::submit_prompt` → routes by `Phase` to `phase_dispatch::{send_spec_prompt, send_build_prompt, send_refine_prompt}` (`impl AppCore`) / `try_switch_phase` → `claude_bridge::ClaudeBridge::send_phase_prompt` → `claude_bridge::generate_mcp_config` (per-phase MCP tool config for `mcp/server.py`) → `claude::send_with_phase_prompt` → `claude::send_prompt` (spawns `claude` CLI subprocess, streams stdout).
4. Claude CLI subprocess calls back into `mcp/server.py::handle_tool_call`, shelling out to `python -m ai3d_cad` (`builder.build`/`validate`, `assembler.assemble`, `analyzer.info`, `paramset.paramset`) to run CadQuery/OpenSCAD and produce STL/STEP.
5. `AppCore::poll_events` drains `CoreEvent`s (streaming text, build artifacts, tool calls) → `App::handle_core_event`/`sync_from_core` (src/main.rs) mirror state into TUI panes (`right_panel`, `spec_panel`, `conversation`); `AppCore::handle_tool_call`/`handle_bg_result`/`handle_build_result` update `SessionManager` internally.
6. `viewer.rs`/`preview.rs`/`stl.rs` render the resulting STL (braille preview pane + optional external f3d/viewer process); 360° goal-verification scans driven from `mcp/server.py::scan_model`.

## Commands
- Build: `cargo build --release` (binary `mimodel`, from `Cargo.toml`).
- Run: `cargo run` / installed `mimodel` binary.
- Rust tests: `cargo test`.
- Python env: `python/environment.yml` (conda) or `python/pyproject.toml` (pip).
- Python tests: `pytest` from `python/` (`python/tests/test_*.py`).
- MCP server not run standalone — spawned by `claude_bridge::generate_mcp_config` per phase.

## Conventions
- Phase rails enforced in code: 3 phases (Spec/Build/Refine); each exposes only its own MCP tool subset (`generate_mcp_config(phase_name, ...)`); no auto-advance — `try_switch_phase` requires explicit user command.
- UI/logic split: `AppCore` (src/core/app.rs) owns all non-rendering state; `App` (src/main.rs) is TUI-only and must not hold business logic — mirrors core state via `sync_from_core`/`handle_core_event`.
- `phase_dispatch.rs` and other former `impl App` methods are now `impl AppCore`, callable/testable without a terminal.
- Domain state (`SessionManager`, `ModelSession`, `ComponentManifest`) persisted to disk under a project/session directory tree (`src/storage/`), reloadable via `ModelSession::load`.
- Prompts are markdown files loaded at runtime (`prompt_builder::load_phase_system_prompt`), not compiled in.
- Python build engine strictly separates CadQuery (`_build_cadquery`) vs OpenSCAD (`_build_openscad`) code paths inside `builder.py`.
- Legacy phase names (Decompose/Component/Assembly/Refinement) only survive as `serde` deserialize aliases in `phase.rs` for old session files — do not reintroduce them as active phases.

## Gotchas
- `src/core/app.rs::submit_prompt` (409-711, cyclomatic 55) is the single highest-risk hotspot in the repo — touch with care.
- `src/event_handler.rs::handle_tree_key`/`try_autocomplete` and `src/claude.rs::send_prompt` are next-highest complexity×churn hotspots.
- `mcp/server.py::handle_tool_call`/`generate_goal_document`/`run_cadquery_build` remain the largest Python functions; not re-measured this pass.
- `toml` has no jcodemunch extractor — import graph across `Cargo.toml`/`pyproject.toml`/reference `*.toml` is incomplete.
- No CI/infra config detected (`get_project_intel` returned empty infra/ci/api/data).
- `python/src/ai3d_cad/__init__.py::PROTOCOL_VERSION = 2` is consumed across the Rust↔MCP boundary; bump carefully if the tool schema changes.
- Dead 5-phase layer (Decompose/Component/Assembly as active phases, plus their prompt files) was purged this refactor — `prompts/` now only has `spec.md`/`build.md`/`refine.md`; don't resurrect old `phase_dispatch` variants from stale docs/context.

## Hot symbols
- `src/core/app.rs:409` — `pub fn submit_prompt(&mut self, text: &str, _part_refs: &[String], lib_refs: &[String])`
- `src/core/app.rs:136` — `pub fn new_with(config: Config, briefing: Option<String>, usage_refresh: UsageRefresh) -> Result<AppCore, String>`
- `src/core/app.rs:1700` — `pub fn poll_events(&mut self) -> Vec<CoreEvent>`
- `src/core/app.rs:1590` — `fn handle_tool_call(&mut self, tool: &ToolCall)`
- `src/core/app.rs:1214` — `pub(crate) fn open_project(&mut self, project_idx: usize)`
- `src/core/app.rs:715` — `pub(crate) fn handle_ref_command(&mut self, text: &str, attached_images: Vec<PathBuf>)`
- `src/main.rs:459` — `fn main()`
- `src/main.rs:540` — `fn run_event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> std::io::Result<()>`
- `src/main.rs:267` — `fn sync_from_core(&mut self)`
- `src/main.rs:358` — `fn handle_core_event(&mut self, event: CoreEvent)`
- `src/main.rs:318` — `fn submit(&mut self, text: &str, part_refs: &[String], lib_refs: &[String])`
- `src/event_handler.rs:13` — `pub(crate) fn handle_key(&mut self, key: crossterm::event::KeyEvent)`
- `src/event_handler.rs:225` — `pub(crate) fn handle_tree_key(&mut self, key: crossterm::event::KeyEvent)`
- `src/phase_dispatch.rs:228` — `pub fn try_switch_phase(&mut self, target: Phase) -> Result<(), SwitchDenied>` (impl AppCore)
- `src/claude_bridge.rs:107` — `pub fn send_phase_prompt(&mut self, phase_name: &str, prompt: &str, images: &[PathBuf], ref_context: Option<&str>, mcp_config: Option<PathBuf>)`
- `src/claude.rs:37` — `pub fn send_prompt(...) -> Result<(String, Option<String>), String>`
