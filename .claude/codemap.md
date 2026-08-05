# Codemap: AI3D (MiModel)
commit: 5b0a29b | generated: 2026-08-05 | source: jcodemunch

## Purpose
Ratatui TUI (`mimodel`) that orchestrates the Claude CLI through a 5-phase pipeline (Spec→Decompose→Component→Assembly→Refinement) to generate CadQuery/OpenSCAD 3D models from natural language, verified against an auto-generated `goal.md` checklist.
Python side (`ai3d_cad` package + `mcp/server.py`) is the MCP tool server the spawned Claude CLI subprocess calls to build/assemble/analyze STL/STEP geometry.

## Layout
- `src/` — Rust TUI binary (`mimodel`), 37 files, ~10.2k LOC. Entry: `src/main.rs`.
  - `src/tui/` — ratatui widgets/panes (conversation, spec_panel, right_panel, component_list/tree, project_tree, param_editor, input_bar, layout, status_bar).
  - `src/storage/` — on-disk project/session persistence (`project.rs`, `session.rs`).
  - Core: `claude.rs`/`claude_bridge.rs` (CLI subprocess + streaming bridge), `phase_dispatch.rs` (per-phase prompt senders), `phase.rs` (phase enum/rails), `spec.rs`/`assembly.rs`/`component.rs` (domain models), `parser.rs` (response parsing), `prompt_builder.rs` (system/user prompt assembly), `python.rs` (subprocess calls into `ai3d_cad`), `session_manager.rs` (build orchestration), `preview.rs`/`render.rs`/`viewer.rs`/`stl.rs` (STL preview & external viewer), `reference.rs`/`reference_detect.rs` (`/ref` component library), `usage.rs` (Claude usage stats), `image.rs` (clipboard/attachment handling), `event_handler.rs` (keybindings).
- `mcp/server.py` — MCP tool server exposing phase-gated tools to the Claude CLI subprocess (build, assemble, analyze, scan_model, import_step, goal doc generation).
- `python/src/ai3d_cad/` — CadQuery/OpenSCAD execution engine: `builder.py` (build/validate), `assembler.py` (boolean assembly from manifest), `openscad.py`, `paramset.py`, `analyzer.py` (STL dimension/topology info).
- `prompts/` — phase system prompts (`spec.md`, `decompose.md`, `component.md`, `assembly.md`, `refine.md`/`refinement.md`, `legacy.md`) + `prompts/knowledge/*.md` (engineering reference docs injected into prompts).
- `references/*.toml` — hardware reference specs (screws, inserts) used by `/ref`.
- `python/tests/`, `src/**` inline `#[cfg(test)]` — unit tests colocated with implementation.

## Entry points & data flow
1. `src/main.rs::main` → `Config::load` → `startup_checks` → `App::new` → `run_event_loop` (ratatui terminal loop).
2. `run_event_loop` polls crossterm events → `event_handler::handle_key` (dispatches to per-pane handlers) or `submit_prompt` on Enter.
3. `submit_prompt`/pane handlers → `phase_dispatch::send_{spec,build,refine}_prompt` / `try_switch_phase` → `claude_bridge::ClaudeBridge::send_phase_prompt` → `claude_bridge::generate_mcp_config` (writes per-phase MCP tool config pointing at `mcp/server.py`) → `claude::send_with_phase_prompt` → `claude::send_prompt` (spawns `claude` CLI subprocess, streams stdout).
4. Claude CLI subprocess calls back into `mcp/server.py::handle_tool_call`, which shells out to `python -m ai3d_cad` (`builder.build`/`validate`, `assembler.assemble`, `analyzer.info`, `paramset.paramset`) to run CadQuery/OpenSCAD and produce STL/STEP.
5. Streamed results land back in `main.rs` via `claude_bridge::drain_streaming`/`try_recv_result`/`drain_tool_calls`, handled by `App::handle_bg_result` / `handle_tool_call` / `handle_build_result`, which update `session_manager::SessionManager` and TUI panes (`right_panel`, `spec_panel`, `conversation`), and can invoke `src/python.rs::build/assemble/paramset` directly for local rebuilds.
6. `viewer.rs`/`preview.rs`/`stl.rs` render the resulting STL (braille preview pane + optional external f3d/viewer process); 360° goal-verification scans are driven from `mcp/server.py::scan_model`.

## Commands
- Build: `cargo build --release` (binary `mimodel`, from `Cargo.toml`).
- Run: `cargo run` / installed `mimodel` binary.
- Rust tests: `cargo test`.
- Python env: `python/environment.yml` (conda) or `python/pyproject.toml` (pip, extras via `[project.optional-dependencies]`).
- Python tests: `pytest` from `python/` (`python/tests/test_*.py`).
- MCP server is not run standalone by users — spawned by `claude_bridge::generate_mcp_config` per phase.

## Conventions
- Phase rails enforced in code: each `Phase` exposes only its own MCP tool subset (`generate_mcp_config(phase_name, ...)`); no auto-advance — `try_switch_phase` requires explicit user command.
- Domain state (`SessionManager`, `ModelSession`, `ComponentManifest`) is persisted to disk under a project/session directory tree (`src/storage/`), reloadable via `ModelSession::load`.
- Rust functions favor free functions + small structs with `pub(crate)` visibility for TUI-internal dispatch (`event_handler.rs`, `phase_dispatch.rs`).
- Prompts are markdown files loaded at runtime (`prompt_builder::load_phase_system_prompt`), not compiled in — editing `prompts/*.md` changes agent behavior without a rebuild.
- Python build engine strictly separates CadQuery (`_build_cadquery`) vs OpenSCAD (`_build_openscad`) code paths inside `builder.py`.

## Gotchas
- `mcp/server.py::handle_tool_call` (134 lines, cyclomatic 5) and `generate_goal_document`/`run_cadquery_build` (cyclomatic 41 each) are the largest/most complex functions in the repo per hotspot analysis — touch with care.
- `src/main.rs` is 2188 lines — a monolithic `App` struct/impl holds nearly all TUI state and orchestration; most cross-cutting changes land here.
- `toml` has no jcodemunch extractor — import graph across `Cargo.toml`/`pyproject.toml`/reference `*.toml` is incomplete; don't trust dependency-graph tools for `.toml` edges.
- No CI/infra config detected (`get_project_intel` returned empty infra/ci/api/data) — no Dockerfile, no pipeline definitions in-repo.
- `python/src/ai3d_cad/__init__.py` defines `PROTOCOL_VERSION = 2`, consumed across the Rust↔MCP boundary; bump carefully if the tool schema changes.

## Hot symbols
- `src/main.rs:1964` — `fn main()`
- `src/main.rs:2045` — `fn run_event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> std::io::Result<()>`
- `src/main.rs:363` — `fn submit_prompt(&mut self, text: String)`
- `src/main.rs:1704` — `fn handle_tool_call(&mut self, tool: claude_bridge::ToolCall)`
- `src/main.rs:1031` — `fn handle_bg_result(&mut self, result: BackgroundResult)`
- `src/event_handler.rs:11` — `pub(crate) fn handle_key(&mut self, key: crossterm::event::KeyEvent)`
- `src/phase_dispatch.rs:19` — `pub(crate) fn send_spec_prompt(&mut self, text: &str, images: Vec<PathBuf>)`
- `src/phase_dispatch.rs:436` — `fn try_switch_phase(...)`
- `src/claude_bridge.rs:107` — `pub fn send_phase_prompt(&mut self, phase_name: &str, prompt: &str, images: &[PathBuf], ref_context: Option<&str>, mcp_config: Option<PathBuf>)`
- `src/claude_bridge.rs:207` — `pub fn generate_mcp_config(phase_name: &str, session_dir: Option<&Path>) -> Result<PathBuf, String>`
- `src/claude.rs:304` — `pub fn send_with_phase_prompt(...) -> Result<(String, Option<String>), String>`
- `src/claude.rs:128` — `pub fn send_prompt(...) -> Result<(String, Option<String>), String>`
- `src/session_manager.rs:147` — `pub fn build(&mut self, code: &str, engine: Engine) -> BuildResult`
- `src/python.rs:190` — `pub fn assemble(python: &str, manifest_path: &Path, output_path: &Path, step_path: Option<&Path>, timeout: Duration) -> BuildResult`
- `mcp/server.py:799` — `def handle_tool_call(name, arguments, session_dir)`
- `mcp/server.py:588` — `def run_cadquery_build(code, output_dir, session_root=None, label="build")`
- `python/src/ai3d_cad/builder.py:36` — `def build(code_path: str, output_path: str, engine: str, step_path: str | None = None) -> int`
