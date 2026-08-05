# Codemap: AI3D (MiModel)
commit: 35b018b | generated: 2026-08-05 | source: jcodemunch

## Purpose
`mimodel` is now a web-only binary: an axum HTTP+WebSocket server (routes.rs projects/refs API, ws.rs session channel, artifacts.rs GLB downloads, assets.rs embedded static files) fronting a Svelte chat/viewer UI, driving Claude CLI through a 3-phase pipeline (Spec→Build→Refine) to generate CadQuery/OpenSCAD 3D models, verified against an auto-generated `goal.md` checklist.
Python side (`ai3d_cad` package incl. `glb_export.py` + `mcp/server.py`) is the MCP tool server the spawned Claude CLI subprocess calls to build/assemble/analyze/export geometry.

## Layout
- `src/` — Rust binary (`mimodel`), 26 files, ~6.4k LOC. Entry: `src/main.rs` (99 lines) — clap `Cli` (`--port`/`--no-browser`; `--web` kept as a hidden deprecated no-op), reads piped-stdin briefing, then always calls `server::run_blocking`. **TUI is gone**: `src/tui/`, `event_handler.rs`, `render.rs`, `viewer.rs`, `preview.rs`, `stl.rs`, `usage.rs` deleted; `ratatui`/`crossterm` dropped from `Cargo.toml`.
  - `src/core/app.rs` (1781 lines) — `AppCore`: all app state/logic (phase state + approval gate, session mgmt, Claude CLI interaction, refs, background-result processing). TUI-mirroring accessors (`busy`, `undo`, `usage_stats`, pending-image helpers) were removed with the TUI. `src/core/mod.rs` re-exports it + `BackgroundResult`.
  - `src/server/` — axum HTTP+WS API, the only front door: `mod.rs` (`ServerState`/`SharedState` = `HashMap<project_id, AppCore>` behind a `Mutex`, `run_blocking`), `routes.rs` (`/api/projects` CRUD + `/api/refs` list), `ws.rs` (`/api/session` WS: `prompt`/`approve_phase`/`advance`/`go_back`/`cancel_stream` in, `snapshot`/`stream_delta`/`tool_call`/`phase_state`/`build_progress`/`error` out; checks `Origin` header via `origin_allowed`), `artifacts.rs` (GET `iteration_<n>.{glb,manifest.json}` per project, `glb_iterations` dir scan), `assets.rs` (rust-embed static files behind `embed-frontend` Cargo feature).
  - `src/storage/` — on-disk project/session persistence (`project.rs`, `session.rs`).
  - `claude.rs` (390 lines, gained `parse_build_progress_line`)/`claude_bridge.rs` (293 lines, gained `BuildProgress`/`drain_build_progress` channel for `BUILD_COMPONENT:` lines), `phase_dispatch.rs` (`impl AppCore` — per-phase prompt senders + gate-aware `try_switch_phase`), `phase.rs` (3-phase enum, legacy deserialize aliases), `spec.rs`, `component.rs`, `parser.rs`, `prompt_builder.rs`, `python.rs` (subprocess into `ai3d_cad`), `session_manager.rs`, `reference.rs`/`reference_detect.rs` (`/ref` library), `image.rs`. **No** `usage.rs` — Claude usage stats were TUI-only and were removed.
  - `build.rs` — guards `embed-frontend` against a missing `frontend/dist`.
- `frontend/` — Svelte 5 + TypeScript + Vite chat UI, embedded via rust-embed when built with `--features embed-frontend`. `src/App.svelte` (325 lines, "Layout A" — the app shell) composes `lib/Viewer.svelte` (981 lines, three.js GLB viewer), `lib/Timeline.svelte` (iteration scrubber), `lib/Chat.svelte` (375 lines, streamed markdown + collapsible tool calls), `lib/SpecPanel.svelte`, `lib/RefPicker.svelte`, `lib/Stepper.svelte` (gated phase stepper). `src/lib/ws.ts::connectSession` — typed WS protocol client mirroring `src/server/ws.rs` message shapes. `src/lib/markdown.ts` — chat markdown rendering helper.
- `mcp/server.py` (1276 lines) — MCP tool server exposing phase-gated tools to the Claude CLI subprocess (build, assemble, analyze, scan_model, import_step, goal doc generation); emits `BUILD_COMPONENT: <name> <status>` progress lines (parsed by `claude.rs::parse_build_progress_line`) around line 658.
- `python/src/ai3d_cad/` — CadQuery/OpenSCAD execution engine: `builder.py`, `assembler.py`, `openscad.py`, `paramset.py`, `analyzer.py`, `glb_export.py` (111 lines, new — GLB export for the in-browser three.js viewer, backing `src/server/artifacts.rs`).
- `prompts/` — phase system prompts (`spec.md`, `build.md`, `refine.md`) + `prompts/knowledge/*.md`.
- `references/*.toml` — hardware reference specs used by `/ref`.
- `python/tests/`, `tests/{api_projects,api_ws,api_artifacts,api_assets,api_refs,integration}.rs` + `tests/common/mod.rs` (spawn harness — sandboxes `HOME`, spawns the real binary against a fake `claude`), `src/**` inline `#[cfg(test)]`.

## Entry points & data flow
1. `src/main.rs::main` → `Cli::parse` → read piped stdin as `briefing` → `Config::load` → `startup_checks` (`claude::check_claude`, `python::check_python`) → `server::run_blocking(config, port, briefing, on_bound)` (opens browser unless `--no-browser`). There is no other mode.
2. `run_blocking` builds the axum `Router` (`routes::router` merges `ws::router`, falls back to `assets::static_handler`, adds `artifacts::router`) and serves it; `on_bound` prints the listening URL.
3. Browser connects `GET /api/session?project=<id>` → `ws::upgrade` (origin-checked) → `ws::handle_socket` → `init_session` (`ServerState::core_for` lazily creates/caches an `AppCore` per project id, `set_phase_gate(true)`, `open_project_by_id`) → loop: `handle_client_message` (`prompt`→`AppCore::submit_prompt`, `approve_phase`→`AppCore::approve_phase`, `advance`/`go_back`→`try_switch_phase`, denied with `SwitchDenied::NotApproved` if ungated, `cancel_stream`→`AppCore::cancel`) and a 50ms-tick `poll_core_events` (drains `AppCore::poll_events` → JSON via `snapshot_value`/`spec_value`/`phase_state_value`/`build_progress_value`).
4. `AppCore::submit_prompt` routes by `Phase` to `phase_dispatch::{send_spec_prompt, send_build_prompt, send_refine_prompt}` → `ClaudeBridge::send_phase_prompt` → `claude_bridge::generate_mcp_config` → `claude::send_with_phase_prompt` → `claude::send_prompt` (spawns `claude` CLI, streams stdout; `BUILD_COMPONENT:` lines parsed into `BuildProgress` and pushed to the client as `build_progress` events).
5. Claude CLI calls back into `mcp/server.py::handle_tool_call`, shelling into `python -m ai3d_cad` (`builder.build`/`validate`, `assembler.assemble`, `analyzer.info`, `paramset.paramset`, `glb_export`) to produce STL/STEP/GLB.
6. Browser fetches rendered geometry via `GET /api/projects/{id}/artifacts/iteration_<n>.glb` (`src/server/artifacts.rs::get_artifact`, iterations enumerated by `glb_iterations`) and renders it in `Viewer.svelte`; `Timeline.svelte` drives which iteration is shown.

## Commands
- Build (no UI assets): `cargo build --release` (binary `mimodel`; server still runs, `assets::lookup` returns `None`, serves a "not built" page).
- Build with web UI: `cd frontend && npm install && npm run build` then `cargo build --release --features embed-frontend` (`build.rs` fails fast if `frontend/dist` is missing).
- Run: `mimodel [--port N] [--no-browser]` (always starts the server; `--web` is accepted but ignored).
- Rust tests: `cargo test` (unit + `tests/api_{projects,ws,artifacts,assets,refs}.rs`, `tests/integration.rs`, via `tests/common::spawn*`).
- Python: `python/environment.yml` / `python/pyproject.toml`; tests via `pytest` from `python/`.
- Frontend dev server: `cd frontend && npm run dev` (Vite).
- MCP server not run standalone — spawned by `claude_bridge::generate_mcp_config` per phase.

## Conventions
- Phase rails: 3 phases (Spec/Build/Refine); server-authoritative approval gate — `AppCore::set_phase_gate(true)` (always on now, set in `ws::init_session`) makes `try_switch_phase` require `approve_phase()` for the current phase, else `Err(SwitchDenied::NotApproved)`; persisted per-session in `session.json`, old sessions load unapproved.
- `AppCore` (src/core/app.rs) is the single source of truth, one instance per project id cached in `ServerState.cores`; there is no other consumer now that the TUI is gone.
- `phase_dispatch.rs` methods are `impl AppCore`, independently testable.
- `frontend/src/lib/ws.ts` types are hand-kept in sync with `src/server/ws.rs` message shapes — no shared schema/codegen.
- `embed-frontend` is opt-in (`Cargo.toml` `[features] embed-frontend = []`); without it the server has no static UI.
- Domain state (`SessionManager`, `ModelSession`, `ComponentManifest`) persisted to disk under a project/session directory tree, reloadable via `ModelSession::load`.
- Prompts are markdown files loaded at runtime, not compiled in.
- Legacy phase names (Decompose/Component/Assembly/Refinement) survive only as `serde` deserialize aliases in `phase.rs`.

## Gotchas
- **NEVER run the `mimodel` binary or its tests without a sandboxed `HOME`** — briefing/test runs create real projects in `~/MiModel` (use `tests/common::spawn*`, which sets `HOME` to a tempdir before launching the binary).
- `src/core/app.rs::submit_prompt` (268-486) remains the largest/highest-risk function in the repo — untouched in shape by the TUI removal but now the sole entry point from `ws::handle_client_message`.
- `ServerState.cores: HashMap<String, AppCore>` behind one `Mutex` — every WS message and REST call locks the whole map; no per-project lock granularity.
- The TUI removal deleted `usage.rs`, `viewer.rs`, `preview.rs`, `stl.rs`, `render.rs`, `event_handler.rs`, and `src/tui/*` along with several now-dead `AppCore` accessors (`busy`, `undo`, `usage_stats`, pending-image helpers) — don't resurrect calls to them from stale docs/context.
- `--web` flag is a hidden deprecated no-op (`let _ = cli.web;`) kept only for old scripts/muscle memory; it has no effect.
- `toml`/`css` have no jcodemunch extractor — import graph across `Cargo.toml`, reference `*.toml`, and `frontend/src/app.css` is incomplete.
- No CI/infra config detected (`get_project_intel` still returns empty infra/ci/api/data).
- `mcp/server.py` emits `BUILD_COMPONENT: <component> <status>` lines (~line 658) that `claude::parse_build_progress_line` depends on verbatim — status must be one of the known values or the line is silently dropped.

## Hot symbols
- `src/core/app.rs:268` — `pub fn submit_prompt(&mut self, text: &str, part_refs: &[String], lib_refs: &[String])`
- `src/core/app.rs:144` — `pub fn set_phase_gate(&mut self, on: bool)`
- `src/core/app.rs:166` — `pub fn approve_phase(&mut self)`
- `src/core/app.rs:1136` — `pub fn poll_events(&mut self) -> Vec<CoreEvent>`
- `src/core/app.rs:1038` — `fn handle_tool_call(&mut self, tool: &ToolCall)`
- `src/server/mod.rs:37` — `pub fn core_for(&mut self, project_id: &str) -> Result<&mut AppCore, String>`
- `src/server/mod.rs:61` — `pub fn run_blocking(config: Config, port: u16, briefing: Option<String>, on_bound: impl FnOnce(std::net::SocketAddr)) -> Result<(), String>`
- `src/server/ws.rs:70` — `async fn handle_socket(mut socket: WebSocket, state: SharedState, project_id: String)`
- `src/server/ws.rs:136` — `fn handle_client_message(state: &SharedState, project_id: &str, text: &str) -> Vec<Value>`
- `src/server/artifacts.rs:56` — `async fn get_artifact(State(state): State<SharedState>, AxumPath((project, file)): AxumPath<(String, String)>) -> Response`
- `src/claude_bridge.rs:156` — `pub fn send_phase_prompt(&mut self, phase_name: &str, prompt: &str, images: &[PathBuf], ref_context: Option<&str>, mcp_config: Option<PathBuf>)`
- `src/claude.rs:42` — `pub fn parse_build_progress_line(line: &str) -> Option<(String, String)>`
- `src/phase_dispatch.rs:210` — `pub fn try_switch_phase(&mut self, target: Phase) -> Result<(), SwitchDenied>`
- `src/main.rs:52` — `fn main()`
- `frontend/src/lib/ws.ts:42` — `export function connectSession(projectId: string, handlers: SessionHandlers): SessionClient`
