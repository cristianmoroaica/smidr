# Codemap: AI3D (MiModel)
commit: 3bd2345 | generated: 2026-08-05 | source: jcodemunch

## Purpose
Ratatui TUI (`mimodel`) orchestrating the Claude CLI through a 3-phase pipeline (Spec→Build→Refine) to generate CadQuery/OpenSCAD 3D models from natural language, verified against an auto-generated `goal.md` checklist. Also runnable as an axum web server (`--web`) serving a Svelte chat UI over the same `AppCore` via REST + WebSocket.
Python side (`ai3d_cad` package + `mcp/server.py`) is the MCP tool server the spawned Claude CLI subprocess calls to build/assemble/analyze STL/STEP geometry.

## Layout
- `src/` — Rust binary (`mimodel`), 38 files, ~12.7k LOC. Entry: `src/main.rs` (682 lines) — CLI arg parsing (`Cli`: `--web`/`--port`/`--no-browser`) then either TUI (default) or `server::run_blocking`.
  - `src/core/app.rs` (2337 lines) — `AppCore`: all non-rendering state/logic (phase state + server-authoritative approval gate, session mgmt, Claude CLI interaction, refs, background-result processing), shared by both TUI and web server. `src/core/mod.rs` re-exports it plus `BackgroundResult`.
  - `src/server/` (new) — axum HTTP+WS API: `mod.rs` (`ServerState`/`SharedState` — `HashMap<project_id, AppCore>`, `run_blocking`), `routes.rs` (`/api/projects` CRUD, name validation, path-traversal guard), `ws.rs` (`/api/session` WS: pinned client→server `prompt`/`approve_phase`/`advance`/`go_back`/`cancel_stream` protocol; server pushes phase/build/stream events), `assets.rs` (rust-embed static-file serving behind `embed-frontend` Cargo feature, `Assets` struct + `static_handler` fallback).
  - `src/tui/` — ratatui widgets/panes: `conversation.rs`, `spec_panel.rs`, `right_panel.rs`, `model_panel.rs`, `project_tree.rs`, `input_bar.rs`, `layout.rs`, `status_bar.rs`.
  - `src/storage/` — on-disk project/session persistence (`project.rs`, `session.rs`).
  - `claude.rs`/`claude_bridge.rs` (CLI subprocess + streaming bridge), `phase_dispatch.rs` (`impl AppCore` — per-phase prompt senders + `try_switch_phase`, now gate-aware), `phase.rs` (3-phase enum: Spec/Build/Refine, legacy Decompose/Component/Assembly/Refinement deserialize aliases), `spec.rs`, `component.rs` (domain models), `parser.rs`, `prompt_builder.rs`, `python.rs` (subprocess into `ai3d_cad`), `session_manager.rs`, `preview.rs`/`render.rs`/`viewer.rs`/`stl.rs` (STL preview + external viewer, TUI-only), `reference.rs`/`reference_detect.rs` (`/ref` library), `usage.rs`, `image.rs`, `event_handler.rs` (`impl App` keybindings, delegates to `AppCore`).
  - `build.rs` — guards the `embed-frontend` feature against a missing `frontend/dist` build output.
- `frontend/` (new) — Svelte 5 + TypeScript + Vite chat UI, embedded into the binary via rust-embed when built with `--features embed-frontend`. `src/lib/ws.ts` — typed WS protocol client (mirrors `src/server/ws.rs` message shapes). `src/lib/Chat.svelte` (streamed markdown + collapsible tool calls), `src/lib/Stepper.svelte` (gated phase stepper), `src/App.svelte`, `src/main.ts`.
- `mcp/server.py` — MCP tool server exposing phase-gated tools to the Claude CLI subprocess (build, assemble, analyze, scan_model, import_step, goal doc generation).
- `python/src/ai3d_cad/` — CadQuery/OpenSCAD execution engine: `builder.py`, `assembler.py`, `openscad.py`, `paramset.py`, `analyzer.py`.
- `prompts/` — phase system prompts (`spec.md`, `build.md`, `refine.md`) + `prompts/knowledge/*.md` engineering reference docs injected into prompts.
- `references/*.toml` — hardware reference specs (screws, inserts) used by `/ref`.
- `python/tests/`, `tests/{api_projects,api_ws,api_assets,common}.rs` (new — integration tests drive a real server + fake `claude` binary), `src/**` inline `#[cfg(test)]` — colocated unit tests (`src/core/app.rs` alone carries ~40 inline tests, including the new phase-gate suite).

## Entry points & data flow
1. `src/main.rs::main` parses `Cli` → `Config::load` → `startup_checks`. If `cli.web`: `server::run_blocking(config, cli.port, on_bound)` (opens browser unless `--no-browser`), else TUI: `App::new` (wraps `AppCore::new`) → `run_event_loop`.
2. **TUI path** (unchanged shape): `run_event_loop` polls crossterm → `event_handler::handle_key`/`App::submit` → `AppCore::submit_prompt` → `phase_dispatch::{send_spec_prompt, send_build_prompt, send_refine_prompt}` / `try_switch_phase` → `ClaudeBridge::send_phase_prompt` → `claude_bridge::generate_mcp_config` → `claude::send_with_phase_prompt` → `claude::send_prompt` (spawns `claude` CLI, streams stdout).
3. **Web path**: `server::ws::upgrade` (GET `/api/session?project_id=`) → `handle_socket` → `init_session` (`ServerState::core_for` lazily creates/caches an `AppCore` per project id, `set_phase_gate(true)`, `open_project_by_id`) → loop: `handle_client_message` (dispatches `prompt`→`submit_prompt`, `approve_phase`→`AppCore::approve_phase`, `advance`/`go_back`→`try_switch_phase`, denied with `SwitchDenied::NotApproved` if ungated) and `poll_core_events` (drains `AppCore::poll_events`, serializes via `snapshot_value`/`phase_state_value`/`build_progress_value` to JSON pushed over the socket). REST CRUD via `server::routes::router` (`list_projects`/`create_project`/`delete_project`, `is_valid_project_name` blocks path traversal).
4. Both paths converge on the same MCP tool loop: Claude CLI subprocess calls back into `mcp/server.py::handle_tool_call`, shelling out to `python -m ai3d_cad` (`builder.build`/`validate`, `assembler.assemble`, `analyzer.info`, `paramset.paramset`) to run CadQuery/OpenSCAD and produce STL/STEP.
5. TUI: `AppCore::poll_events` drained by `App::handle_core_event`/`sync_from_core` (src/main.rs) into TUI panes. Web: same `poll_events` drained by `ws::poll_core_events` into WS JSON frames consumed by `frontend/src/lib/ws.ts` → `Chat.svelte`/`Stepper.svelte`.
6. `viewer.rs`/`preview.rs`/`stl.rs` render STL for the TUI only (braille pane + optional external f3d/viewer); 360° goal-verification scans driven from `mcp/server.py::scan_model` in both modes.

## Commands
- Build (TUI only): `cargo build --release` (binary `mimodel`).
- Build with web UI: `cd frontend && npm install && npm run build` then `cargo build --release --features embed-frontend` (`build.rs` fails fast if `frontend/dist` is missing).
- Run web mode: `mimodel --web [--port N] [--no-browser]`.
- Rust tests: `cargo test` (unit + `tests/api_{projects,ws,assets}.rs` integration tests against a fake `claude` binary via `tests/common.rs`).
- Python env: `python/environment.yml` (conda) or `python/pyproject.toml` (pip); tests via `pytest` from `python/`.
- Frontend dev server: `cd frontend && npm run dev` (Vite).
- MCP server not run standalone — spawned by `claude_bridge::generate_mcp_config` per phase.

## Conventions
- Phase rails enforced in code: 3 phases (Spec/Build/Refine); each exposes only its own MCP tool subset; no auto-advance.
- Server-authoritative approval gate (new): `AppCore::set_phase_gate(true)` (web sessions only, set in `ws::init_session`) makes `try_switch_phase` require `approve_phase()` for the *current* phase before advancing, else `Err(SwitchDenied::NotApproved)`; gate is off by default (TUI), persisted per-session in `session.json`, old sessions load unapproved.
- UI/logic split: `AppCore` (src/core/app.rs) owns all non-rendering state, consumed by *both* the TUI (`src/main.rs`, direct field access) and the server (`src/server/`, one `AppCore` per project id behind a `Mutex`); `App` (TUI) mirrors core state via `sync_from_core`/`handle_core_event`, `ws.rs` mirrors it via JSON snapshots.
- `phase_dispatch.rs` methods are `impl AppCore`, callable/testable without a terminal or socket.
- `frontend/src/lib/ws.ts` types must be kept in sync by hand with the Rust message shapes in `src/server/ws.rs` — no shared schema/codegen.
- `embed-frontend` is opt-in (`Cargo.toml` `[features] embed-frontend = []`); without it `assets::lookup` returns `None` and the server has no static UI.
- Domain state (`SessionManager`, `ModelSession`, `ComponentManifest`) persisted to disk under a project/session directory tree, reloadable via `ModelSession::load`.
- Prompts are markdown files loaded at runtime, not compiled in.
- Legacy phase names (Decompose/Component/Assembly/Refinement) only survive as `serde` deserialize aliases in `phase.rs`.

## Gotchas
- `src/core/app.rs::submit_prompt` (493-795, previously measured cyclomatic 55) remains the single highest-risk hotspot — untouched by this refactor but now also reachable from `ws::handle_client_message`.
- New concurrency surface: `ServerState.cores: HashMap<String, AppCore>` behind one `Mutex` — every WS message and REST call locks the whole map; check `src/server/mod.rs::core_for` before assuming per-project isolation.
- `SwitchDenied::NotApproved` is a *new* variant — any old exhaustive `match` on `SwitchDenied` outside this refactor's touched files will now fail to compile; grep before adding new match sites.
- `frontend/` is a separate npm project (`package.json`/`package-lock.json`) not covered by `cargo build` — CI/build tooling must run both `npm run build` and `cargo build --features embed-frontend`, in that order (`build.rs` checks for `frontend/dist`).
- `toml`/`css` have no jcodemunch extractor — import graph across `Cargo.toml`, `*.toml` references, and `frontend/src/app.css` is incomplete.
- No CI/infra config detected (`get_project_intel` still returns empty infra/ci/api/data — this repo has no in-repo pipeline definitions despite the new server).
- `python/src/ai3d_cad/__init__.py::PROTOCOL_VERSION = 2` is consumed across the Rust↔MCP boundary; unrelated to the new REST/WS protocol versioning (there is none yet) — don't conflate the two.

## Hot symbols
- `src/core/app.rs:493` — `pub fn submit_prompt(&mut self, text: &str, _part_refs: &[String], lib_refs: &[String])`
- `src/core/app.rs:223` — `pub fn set_phase_gate(&mut self, on: bool)`
- `src/core/app.rs:230` — `pub fn is_phase_approved(&self, phase: Phase) -> bool`
- `src/core/app.rs:245` — `pub fn approve_phase(&mut self)`
- `src/core/app.rs:274` — `pub(crate) fn open_project_by_id(&mut self, id: &str) -> Result<(), String>`
- `src/core/app.rs:1784` — `pub fn poll_events(&mut self) -> Vec<CoreEvent>`
- `src/core/app.rs:1674` — `fn handle_tool_call(&mut self, tool: &ToolCall)`
- `src/server/mod.rs:31` — `pub fn core_for(&mut self, project_id: &str) -> Result<&mut AppCore, String>`
- `src/server/mod.rs:46` — `pub fn run_blocking(config: Config, port: u16, on_bound: impl FnOnce(std::net::SocketAddr)) -> Result<(), String>`
- `src/server/ws.rs:44` — `async fn handle_socket(mut socket: WebSocket, state: SharedState, project_id: String)`
- `src/server/ws.rs:98` — `fn handle_client_message(state: &SharedState, project_id: &str, text: &str) -> Vec<Value>`
- `src/server/routes.rs:13` — `pub fn router(state: SharedState) -> Router`
- `src/main.rs:479` — `fn main()`
- `src/main.rs:580` — `fn run_event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> std::io::Result<()>`
- `src/phase_dispatch.rs:234` — `pub fn try_switch_phase(&mut self, target: Phase) -> Result<(), SwitchDenied>` (now gate-aware)
