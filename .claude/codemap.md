# Codemap: AI3D (Smiðr)

commit: fc99954 | generated: 2026-08-06 | source: jcodemunch

## Purpose
Smiðr (binary `smidr`, formerly mimodel/MiModel) — a web-only axum HTTP+WebSocket server fronting an embedded Svelte/three.js chat+viewer UI, driving the Claude CLI through a 3-phase pipeline (Spec→Build→Refine) via MCP tools to generate parametric CadQuery/OpenSCAD 3D models verified against an auto-generated `goal.md` checklist.
Python side (`python/ai3d_cad` + `mcp/server.py`) is the MCP tool server the spawned `claude` CLI subprocess calls to build/assemble/analyze/export geometry.

## Layout
- `src/main.rs` — clap `Cli` (`--port`/`--no-browser`; `--web` kept as a hidden deprecated no-op), reads piped-stdin briefing, always calls `server::run_blocking`. No TUI mode exists.
- `src/server/` — `mod.rs` (`ServerState`/`SharedState = Arc<Mutex<...>>`, `core_for`, `run_blocking`), `routes.rs` (`/api/projects` CRUD, export/open-folder/baseline endpoints, `/api/refs`), `ws.rs` (`/api/session` WebSocket), `artifacts.rs` (GET per-iteration `iteration_<n>.{glb,manifest.json}`, `iterations/`+root merge), `assets.rs` (rust-embed static files behind `embed-frontend` feature, else `NOT_BUILT_HTML`).
- `src/core/app.rs` — `AppCore`: single source of truth per project id (cached in `ServerState.cores`), all session/phase/Claude-interaction logic. `src/core/mod.rs` re-exports it + `CoreEvent`/`SwitchDenied`.
- `src/phase.rs` — 3-phase enum (Spec/Build/Refine) with legacy Decompose/Component/Assembly/Refinement deserialize aliases.
- `src/phase_dispatch.rs` — `impl AppCore`: per-phase prompt senders, gate-aware `try_switch_phase`, `export_artifacts`/`handle_export` (writes `<session>/exports/`), `handle_param_edit`.
- `src/claude.rs` / `src/claude_bridge.rs` — shells `claude` CLI (`send_prompt`/`send_with_phase_prompt`), builds per-phase MCP config (`generate_mcp_config`), locates `mcp/server.py` + its Python interpreter, parses `BUILD_COMPONENT:` progress lines.
- `src/prompt_builder.rs` — loads embedded `prompts/*.md` (rust-embed) as phase system prompts + engineering knowledge.
- `src/storage/` — `project.rs` (`~/Smidr` root, `migrate_legacy_root` one-shot ~/MiModel→~/Smidr migration, project CRUD), `session.rs` (session.json persistence).
- `src/spec.rs`, `src/component.rs`, `src/model_session.rs`, `src/reference.rs`/`reference_detect.rs`, `src/parser.rs`, `src/image.rs`, `src/config.rs`, `src/python.rs` — goal.md/spec handling, component iteration/undo, reference-part `/ref` library, clipboard/image ingestion, `~/.config/smidr` config, Python subprocess helpers.
- `frontend/` — Svelte 5 + TypeScript + Vite: `App.svelte` (shell) composes `lib/Viewer.svelte` (three.js GLB viewer; placement-driven instance scene, always-available Export button, open-folder button, Forging progress panel), `lib/ApproveModal.svelte` (export STL/STEP, inspect components, lock baseline+advance; export-only mode when opened from the viewer toolbar), `lib/PhaseSwitchModal.svelte` (approve/deny agent-issued `request_phase_change`), `lib/Timeline.svelte`, `lib/Chat.svelte` (streamed markdown + tool calls + structured clarifying-question option chips), `lib/SpecPanel.svelte`, `lib/RefPicker.svelte`, `lib/Stepper.svelte`. `lib/ws.ts::connectSession` — typed WS client mirroring `src/server/ws.rs` message shapes (hand-kept in sync, no codegen). Built to `frontend/dist`, embedded via rust-embed behind `embed-frontend`.
- `mcp/server.py` — MCP tool server (phase-gated tools: build, assemble, analyze, scan_model, import_step, `request_phase_change`, `ask_clarification`/`ask_question` with structured options, goal doc generation); emits `BUILD_COMPONENT: <name> <status>` lines consumed by `claude.rs`.
- `python/src/ai3d_cad/` — `builder.py` (CadQuery/OpenSCAD build+STL analysis), `assembler.py` (boolean assembly from manifest), `paramset.py` (locked-namespace param overrides), `glb_export.py` (instance-aware placement-driven GLB scene export backing `artifacts.rs`; `load_placements`/`apply_placements`/`build_scene_nodes`), `analyzer.py`, `openscad.py`.
- `prompts/` — phase system prompts (`spec.md`, `build.md`, `refine.md`; `build.md` pins the assembly child-naming contract `<component>_<n>` for instances) + `prompts/knowledge/*.md`; embedded via rust-embed.
- `references/*.toml` — verified real-world part specs (M3 SHCS, threaded inserts, etc.) used by `/ref` to avoid fabricating dimensions.
- `tests/` — black-box tests spawning the built binary: `api_projects.rs`, `api_refs.rs`, `api_ws.rs`, `api_artifacts.rs`, `api_assets.rs`, `integration.rs`, `tests/common/mod.rs` (sandboxed-HOME spawn harness).
- `python/tests/` — pytest suite for the CAD engine, incl. `test_assembly_placements.py`, `test_glb_export.py`, `test_mcp_server.py`.

## Entry points & data flow
1. `main()` (src/main.rs:52) → `Cli::parse` → read piped stdin as `briefing` → `Config::load` → `startup_checks` → `server::run_blocking(config, port, briefing, on_bound)`.
2. `run_blocking` (server/mod.rs:55) calls `migrate_legacy_root` first, optionally seeds an `AppCore` from the briefing, builds `routes::router` (project CRUD + export/open-folder/baseline, merges `ws::router` + `artifacts::router`, falls back to `assets::static_handler`), binds, serves.
3. Browser loads `/` → `assets::static_handler` serves the embedded Svelte SPA (or `NOT_BUILT_HTML`).
4. `GET /api/session?project=<id>` → `ws::handle_socket` → `init_session` → loop: `handle_client_message` dispatches `prompt`/`approve_phase`/`advance`/`go_back`/`cancel_stream`; 50ms tick → `poll_core_events` → `AppCore::poll_events`.
5. `submit_prompt` (core/app.rs:294) routes by `Phase` to `phase_dispatch::send_{spec,build,refine}_prompt` → `ClaudeBridge::send_phase_prompt` → `generate_mcp_config` → `claude::send_with_phase_prompt`/`send_prompt` (spawns `claude` CLI with the phase MCP config pointing at `mcp/server.py`).
6. `mcp/server.py::handle_tool_call` shells into `python -m ai3d_cad` (build/assemble/analyze/paramset/glb_export); `BUILD_COMPONENT:` stdout lines parsed back into `BuildProgress` events; agent-issued `request_phase_change` surfaces a `PhaseSwitchModal` consent prompt client-side (server approval gate untouched).
7. Viewer render: `GET /api/projects/{id}/artifacts/iteration_<n>.glb|manifest.json` (`artifacts::get_artifact`, checks `iterations/` then session root) → `Viewer.svelte` builds the scene per-placement from `manifest.json` (`build_scene_nodes`), one node per instance.
8. Export: `POST /api/projects/{id}/export` (`routes::export_project` → `AppCore::export_artifacts`) copies latest iteration + all built components into `<session>/exports/{assembly,<component>}.{stl,step}`, returns download URLs; `GET /api/projects/{id}/export/{file}` serves them. `POST /api/projects/{id}/open-folder` (body `{"target":"exports"}` optional) opens session root or `exports/` via `xdg-open` (`SMIDR_NO_OPEN=1` test hook). `POST /api/projects/{id}/baseline` `{"n":<u32>}` locks the Refine-phase ghost-diff baseline.

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
- Phase rails: 3 phases (Spec/Build/Refine), server-authoritative approval gate — `try_switch_phase` requires `approve_phase()` or returns `Err(SwitchDenied::NotApproved)`; persisted per-session in `session.json`. Agent-requested switches (`request_phase_change`) route through the same gate via a client consent modal, never auto-advance.
- `AppCore` is the single source of truth, one instance per project id, cached in `ServerState.cores` behind one `Mutex` (every WS message and REST call locks the whole map — no per-project lock granularity).
- `goal.md` is the source of truth for build verification; never fabricate real-world part dimensions — use `references/*.toml` or ask via structured `ask_clarification` option chips.
- Storage root is `~/Smidr` (migrated once from legacy `~/MiModel` via `migrate_legacy_root`, idempotent).
- Deliverables split: `<session>/exports/` = human-named final files (`assembly.stl/.step`, `<component>.stl/.step`), regenerated on demand by `POST .../export`; `<session>/iterations/` = numbered `iteration_<n>.glb/manifest.json` build history, legacy sessions still discovered/served from the session root too (iterations/ wins on numeric conflict).
- Assembly GLB scenes are placement-driven: `placements.json` (captured from `cq.Assembly` world transforms) drives one scene node per instance; assembly child names must be `<component-dir-name>` or `<component-dir-name>_<n>` for repeated instances (pinned in `prompts/build.md`).
- Every filename/id used in a filesystem path is validated before joining: `is_valid_project_name`, `is_valid_export_file_name`, `parse_artifact_file` — never trust a client-supplied path segment raw.
- WS wire protocol (message `type` values) is pinned by spec — see comment header in `src/server/ws.rs`; `frontend/src/lib/ws.ts` types are hand-kept in sync, no shared schema/codegen.
- `prompts/` and `frontend/dist` are embedded into the binary via rust-embed — source edits need a rebuild to take effect at runtime.

## Gotchas
- Never run the `smidr` binary or its test suite against a real `$HOME` — integration tests always spawn against a sandboxed HOME via `tests/common::spawn*`.
- Default `cargo build` does NOT include the web UI; use `cargo build --features embed-frontend` or `assets::static_handler` serves the `NOT_BUILT_HTML` page.
- Frontend must be rebuilt (`cd frontend && npm run build`) before `embed-frontend` picks up UI changes — stale `frontend/dist` gets baked in silently otherwise.
- rust-analyzer may show phantom `E0308` type errors here — false positives; trust `cargo check`/`cargo build`.
- `prompts/*.md` are embedded via rust-embed; a source edit needs a rebuild to be picked up by a running binary.
- `mcp/server.py` and `.venv-cadquery` are located relative to the checkout: `find_mcp_server` (claude_bridge.rs) walks cwd/exe-dir/parents, falling back to `CARGO_MANIFEST_DIR` for installed binaries; `find_cadquery_python` looks for `.venv-cadquery/bin/python3` (or `.venv/bin/python3`) next to `mcp/`. Python needs `.venv-cadquery` (3.11).
- `mcp/server.py` emits `BUILD_COMPONENT: <component> <status>` lines that `claude.rs::parse_build_progress_line` depends on verbatim — an unrecognized status is silently dropped.
- `--web` CLI flag is a hidden deprecated no-op (`let _ = cli.web;`); the server is now the only mode.
- `GET /api/projects/{id}/export/{file}` only resolves against an already-cached `AppCore` (no lazy re-open) — a page-reload download link replayed after a server restart 404s even though the file is still on disk (documented as deliberate in `routes.rs`).
- `open-folder`/export endpoints are safe unauthenticated only because the server always binds `127.0.0.1`.
- Unmatched GLB placement names are skipped with a warning (not fatal); unreferenced components still render once at identity so nothing vanishes silently from the viewer.
- README.md is TUI-era ("interactive terminal UI") and stale — actual UI is the axum+Svelte web app.

## Hot symbols
- `src/main.rs:52` — `fn main()`
- `src/server/mod.rs:55` — `pub fn run_blocking(config: Config, port: u16, briefing: Option<String>, on_bound: impl FnOnce(SocketAddr)) -> Result<(), String>`
- `src/server/mod.rs:32` — `pub fn ServerState::core_for(&mut self, project_id: &str) -> Result<&mut AppCore, String>`
- `src/server/routes.rs:14` — `pub fn router(state: SharedState) -> Router`
- `src/server/routes.rs:259` — `async fn export_project(State(state): State<SharedState>, Path(id): Path<String>) -> Response`
- `src/server/routes.rs:367` — `async fn open_folder(State(state): State<SharedState>, Path(id): Path<String>, body: String) -> Response`
- `src/server/ws.rs:74` — `async fn handle_socket(mut socket: WebSocket, state: SharedState, project_id: String)`
- `src/server/ws.rs:140` — `fn handle_client_message(state: &SharedState, project_id: &str, text: &str) -> Vec<Value>`
- `src/server/artifacts.rs:61` — `async fn get_artifact(...) -> Response`
- `src/server/artifacts.rs:107` — `fn resolve_artifact_path(session_dir: &Path, file: &str) -> Option<PathBuf>`
- `src/core/app.rs:294` — `pub fn submit_prompt(&mut self, text: &str, part_refs: &[String], lib_refs: &[String])`
- `src/core/app.rs:1186` — `pub fn poll_events(&mut self) -> Vec<CoreEvent>`
- `src/phase_dispatch.rs:171` — `pub(crate) fn export_artifacts(&mut self) -> Result<(PathBuf, Vec<String>), String>`
- `src/phase_dispatch.rs:280` — `pub fn try_switch_phase(&mut self, target: Phase) -> Result<(), SwitchDenied>`
- `python/src/ai3d_cad/glb_export.py:211` — `def build_scene_nodes(components: dict, placements: dict) -> tuple`
- `python/src/ai3d_cad/glb_export.py:172` — `def apply_placements(components: dict, placements: dict) -> dict`
- `mcp/server.py:1022` — `def handle_tool_call(name, arguments, session_dir)`
