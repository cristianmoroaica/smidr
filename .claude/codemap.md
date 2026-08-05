# Codemap: AI3D (Smiðr)

commit: b92a73c | generated: 2026-08-05 | source: jcodemunch

## Purpose
Smiðr (binary `smidr`, formerly mimodel/MiModel) — a web-only axum HTTP+WebSocket server fronting an embedded Svelte/three.js chat+viewer UI, driving the Claude CLI through a 3-phase pipeline (Spec→Build→Refine) via MCP tools to generate parametric CadQuery/OpenSCAD 3D models verified against an auto-generated `goal.md` checklist.
Python side (`python/ai3d_cad` + `mcp/server.py`) is the MCP tool server the spawned `claude` CLI subprocess calls to build/assemble/analyze/export geometry.

## Layout
- `src/main.rs` — clap `Cli` (`--port`/`--no-browser`; `--web` kept as a hidden deprecated no-op), reads piped-stdin briefing, always calls `server::run_blocking`. No TUI mode exists.
- `src/server/` — `mod.rs` (`ServerState`/`SharedState = Arc<Mutex<...>>`, `core_for`, `run_blocking`), `routes.rs` (`/api/projects` CRUD, `/api/refs`), `ws.rs` (`/api/session` WebSocket: upgrade/origin-check/handle_socket/init_session/handle_client_message/poll_core_events), `artifacts.rs` (GET `iteration_<n>.{glb,manifest.json}`), `assets.rs` (rust-embed static files behind `embed-frontend` feature, else `NOT_BUILT_HTML`).
- `src/core/app.rs` — `AppCore`: single source of truth per project id (cached in `ServerState.cores`), all session/phase/Claude-interaction logic. `src/core/mod.rs` re-exports it + `CoreEvent`/`SwitchDenied`.
- `src/phase.rs` — 3-phase enum (Spec/Build/Refine) with legacy Decompose/Component/Assembly/Refinement deserialize aliases.
- `src/phase_dispatch.rs` — `impl AppCore`: per-phase prompt senders (`send_spec_prompt`/`send_build_prompt`/`send_refine_prompt`) + gate-aware `try_switch_phase`.
- `src/claude.rs` / `src/claude_bridge.rs` — shells `claude` CLI (`send_prompt`/`send_with_phase_prompt`), builds per-phase MCP config (`generate_mcp_config`), locates `mcp/server.py` + its Python interpreter, parses `BUILD_COMPONENT:` progress lines.
- `src/prompt_builder.rs` — loads embedded `prompts/*.md` (rust-embed) as phase system prompts + engineering knowledge.
- `src/storage/` — `project.rs` (`~/Smidr` root, `migrate_legacy_root` one-shot ~/MiModel→~/Smidr migration, project CRUD), `session.rs` (session.json persistence).
- `src/spec.rs`, `src/component.rs`, `src/model_session.rs`, `src/reference.rs`/`reference_detect.rs`, `src/parser.rs`, `src/image.rs`, `src/config.rs`, `src/python.rs` — goal.md/spec handling, component iteration/undo, reference-part `/ref` library, clipboard/image ingestion, `~/.config/smidr` config, Python subprocess helpers.
- `frontend/` — Svelte 5 + TypeScript + Vite: `App.svelte` (shell) composes `lib/Viewer.svelte` (three.js GLB viewer), `lib/Timeline.svelte` (iteration scrubber), `lib/Chat.svelte` (streamed markdown + tool calls), `lib/SpecPanel.svelte`, `lib/RefPicker.svelte`, `lib/Stepper.svelte`. `lib/ws.ts::connectSession` — typed WS client mirroring `src/server/ws.rs` message shapes (hand-kept in sync, no codegen). Built to `frontend/dist`, embedded via rust-embed behind `embed-frontend`.
- `mcp/server.py` — MCP tool server (phase-gated tools: build, assemble, analyze, scan_model, import_step, goal doc generation); emits `BUILD_COMPONENT: <name> <status>` lines consumed by `claude.rs`.
- `python/src/ai3d_cad/` — `builder.py` (CadQuery/OpenSCAD build+STL analysis), `assembler.py` (boolean assembly from manifest), `paramset.py` (locked-namespace param overrides), `glb_export.py` (GLB export backing `artifacts.rs`), `analyzer.py`, `openscad.py`.
- `prompts/` — phase system prompts (`spec.md`, `build.md`, `refine.md`) + `prompts/knowledge/*.md` (tolerances, mounting, resin constraints); embedded via rust-embed.
- `references/*.toml` — verified real-world part specs (M3 SHCS, threaded inserts, etc.) used by `/ref` to avoid fabricating dimensions.
- `tests/` — black-box tests spawning the built binary: `api_projects.rs`, `api_refs.rs`, `api_ws.rs`, `api_artifacts.rs`, `api_assets.rs`, `integration.rs`, `tests/common/mod.rs` (sandboxed-HOME spawn harness).
- `python/tests/` — pytest suite for the CAD engine.

## Entry points & data flow
1. `main()` (src/main.rs:52) → `Cli::parse` → read piped stdin as `briefing` → `Config::load` → `startup_checks` (`claude::check_claude`, `python::check_python`, non-fatal) → `server::run_blocking(config, port, briefing, on_bound)`.
2. `run_blocking` (server/mod.rs:55) calls `migrate_legacy_root` first, optionally seeds an `AppCore` from the briefing, builds `routes::router` (merges `ws::router` + `artifacts::router`, falls back to `assets::static_handler`), binds, serves.
3. Browser loads `/` → `assets::static_handler` serves the embedded Svelte SPA (or `NOT_BUILT_HTML`).
4. `GET /api/session?project=<id>` → `ws::upgrade` (origin-checked) → `ws::handle_socket` → `init_session` (`ServerState::core_for` lazily creates/caches `AppCore` per project id) → loop: `handle_client_message` dispatches `prompt`→`AppCore::submit_prompt`, `approve_phase`→`AppCore::approve_phase`, `advance`/`go_back`→`try_switch_phase`, `cancel_stream`→`AppCore::cancel`; a 50ms tick calls `poll_core_events`→`AppCore::poll_events`, serialized via `snapshot_value`/`spec_value`/`phase_state_value`/`build_progress_value`.
5. `submit_prompt` (core/app.rs:294) routes by `Phase` to `phase_dispatch::send_spec_prompt`/`send_build_prompt`/`send_refine_prompt` → `ClaudeBridge::send_phase_prompt` → `generate_mcp_config` → `claude::send_with_phase_prompt` → `claude::send_prompt` (spawns `claude` CLI with the phase MCP config pointing at `mcp/server.py`).
6. `mcp/server.py::handle_tool_call` shells into `python -m ai3d_cad` (build/assemble/analyze/paramset/glb_export); `BUILD_COMPONENT:` stdout lines are parsed back into `BuildProgress` events and surfaced over the WS channel.
7. Browser fetches rendered geometry via `GET /api/projects/{id}/artifacts/iteration_<n>.glb` (`artifacts::get_artifact`) and renders it in `Viewer.svelte`.

## Commands
- Build (no UI assets): `cargo build` — binary `smidr`, server still runs, serves `NOT_BUILT_HTML`.
- Build with web UI: `cd frontend && npm install && npm run build && cd .. && cargo build --features embed-frontend` (otherwise the server just serves the NOT_BUILT page).
- Run: `./target/debug/smidr [--port N] [--no-browser]`.
- Rust tests: `cargo test` (black-box, spawns the real binary via `tests/common::spawn*` against a sandboxed HOME).
- Rust check (trust over rust-analyzer): `cargo check`.
- Frontend dev server: `cd frontend && npm run dev` (Vite).
- Python CAD engine tests: `cd python && pytest`.
- MCP server is never run standalone — spawned per-phase by `claude_bridge::generate_mcp_config`.

## Conventions
- Phase rails: 3 phases (Spec/Build/Refine), server-authoritative approval gate — `try_switch_phase` requires `approve_phase()` for the current phase or returns `Err(SwitchDenied::NotApproved)`; persisted per-session in `session.json`.
- `AppCore` is the single source of truth, one instance per project id, cached in `ServerState.cores` behind one `Mutex` (every WS message and REST call locks the whole map — no per-project lock granularity).
- `goal.md` is the source of truth for build verification (functional requirements before visual); never fabricate real-world part dimensions — use `references/*.toml` or ask the user.
- Storage root is `~/Smidr` (migrated once from legacy `~/MiModel` via `migrate_legacy_root`, idempotent, must run before any route/core creates the new root).
- WS wire protocol (message `type` values) is pinned by spec — see comment header in `src/server/ws.rs`; `frontend/src/lib/ws.ts` types are hand-kept in sync, no shared schema/codegen.
- `prompts/` and `frontend/dist` are embedded into the binary via rust-embed — source edits need a rebuild to take effect at runtime.

## Gotchas
- Never run the `smidr` binary or its test suite against a real `$HOME` — integration tests always spawn against a sandboxed HOME via `tests/common::spawn*`.
- Default `cargo build` does NOT include the web UI; use `cargo build --features embed-frontend` or `assets::static_handler` serves the `NOT_BUILT_HTML` page.
- rust-analyzer may show phantom `E0308` type errors here — false positives; trust `cargo check`/`cargo build`.
- `prompts/*.md` are embedded via rust-embed; a source edit needs a rebuild to be picked up by a running binary.
- `mcp/server.py` and `.venv-cadquery` are located relative to the checkout: `find_mcp_server` (claude_bridge.rs) walks cwd/exe-dir/parents, falling back to `CARGO_MANIFEST_DIR` for installed binaries; `find_cadquery_python` looks for `.venv-cadquery/bin/python3` (or `.venv/bin/python3`) next to `mcp/`.
- `mcp/server.py` emits `BUILD_COMPONENT: <component> <status>` lines that `claude.rs::parse_build_progress_line` depends on verbatim — an unrecognized status is silently dropped.
- `--web` CLI flag is a hidden deprecated no-op (`let _ = cli.web;`); the server is now the only mode.
- README.md is TUI-era ("interactive terminal UI") and stale — actual UI is the axum+Svelte web app.

## Hot symbols
- `src/main.rs:52` — `fn main()`
- `src/server/mod.rs:55` — `pub fn run_blocking(config: Config, port: u16, briefing: Option<String>, on_bound: impl FnOnce(SocketAddr)) -> Result<(), String>`
- `src/server/mod.rs:32` — `pub fn ServerState::core_for(&mut self, project_id: &str) -> Result<&mut AppCore, String>`
- `src/server/routes.rs:13` — `pub fn router(state: SharedState) -> Router`
- `src/server/ws.rs:74` — `async fn handle_socket(mut socket: WebSocket, state: SharedState, project_id: String)`
- `src/server/ws.rs:140` — `fn handle_client_message(state: &SharedState, project_id: &str, text: &str) -> Vec<Value>`
- `src/server/ws.rs:206` — `fn poll_core_events(state: &SharedState, project_id: &str) -> Vec<Value>`
- `src/server/artifacts.rs:56` — `async fn get_artifact(...) -> Response`
- `src/core/app.rs:94` — `pub fn AppCore::new(config: Config, briefing: Option<String>) -> Result<AppCore, String>`
- `src/core/app.rs:174` — `pub fn approve_phase(&mut self)`
- `src/core/app.rs:294` — `pub fn submit_prompt(&mut self, text: &str, part_refs: &[String], lib_refs: &[String])`
- `src/core/app.rs:1186` — `pub fn poll_events(&mut self) -> Vec<CoreEvent>`
- `src/core/app.rs:1072` — `fn handle_tool_call(&mut self, tool: &ToolCall)`
- `src/phase_dispatch.rs:210` — `pub fn try_switch_phase(&mut self, target: Phase) -> Result<(), SwitchDenied>`
- `src/phase_dispatch.rs:17,109,120` — `pub(crate) fn send_spec_prompt/send_build_prompt/send_refine_prompt(&mut self, text: &str, images: Vec<PathBuf>)`
- `src/claude_bridge.rs:156` — `pub fn send_phase_prompt(&mut self, phase_name: &str, prompt: &str, images: &[PathBuf], ref_context: Option<&str>, mcp_config: Option<PathBuf>)`
- `src/claude_bridge.rs:228` — `pub fn generate_mcp_config(phase_name: &str, session_dir: Option<&Path>) -> Result<PathBuf, String>`
- `src/claude_bridge.rs:256` — `fn find_mcp_server() -> Result<PathBuf, String>`
- `src/claude.rs:56` — `pub fn send_prompt(...) -> Result<(String, Option<String>), String>`
- `src/storage/project.rs:47` — `pub fn migrate_legacy_root() -> Result<(), String>`
- `mcp/server.py:871` — `def handle_tool_call(name, arguments, session_dir)`
