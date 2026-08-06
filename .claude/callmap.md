# Callmap: AI3D (Smidr)   (arrows: caller → callee; `X ← Y` means Y calls X)

commit: fc99954 | generated: 2026-08-06 | resolution: lsp

LSP confirmed edges for hot symbols incl. export/open-folder/baseline.
Axum handlers wired only via `.route(...)` show 0 incoming calls by design
— marked (routed, not called).

## Entry chains        (one call tree per entry point, ~3 levels deep)
### main() — src/main.rs:52
```
main
├─ Cli::parse → read piped stdin → briefing: Option<String> → Config::load
├─ startup_checks (claude::check_claude, python::check_python)   (heuristic)
└─ server::run_blocking(config, port, briefing, on_bound)   src/server/mod.rs:55
   ├─ storage::project::migrate_legacy_root             src/storage/project.rs:47
   ├─ [if briefing] AppCore::new(config, briefing)       src/core/app.rs:94
   ├─ routes::router(state)   src/server/routes.rs:14 (LSP: 1 incoming call, run_blocking:98)
   │  ├─ /api/projects GET/POST/{id}DELETE → list_projects/create_project/delete_project  routes.rs:46,168,188
   │  ├─ POST .../export → export_project routes.rs:259 (routed); GET .../export/{file} → get_export_file routes.rs:303 (routed)
   │  ├─ POST .../open-folder → open_folder routes.rs:367 (routed); POST .../baseline → set_baseline routes.rs:436 (routed)
   │  ├─ GET /api/refs → list_refs routes.rs:101
   │  ├─ .merge(ws::router) src/server/ws.rs:30; .merge(artifacts::router) src/server/artifacts.rs:27 (LSP-confirmed)
   │  └─ .fallback(assets::static_handler) src/server/assets.rs:70
   └─ axum::serve(listener, app)
```
### export_project() — routes.rs:259 (LSP-confirmed outgoing calls)
```
export_project
├─ is_valid_project_name routes.rs:128 → ensure_project_open routes.rs:217
├─ AppCore::export_artifacts src/phase_dispatch.rs:171
│  (writes <session>/exports/assembly.{stl,step} + <component>.{stl,step})
├─ encode_path_segment routes.rs:235 (per download URL)
└─ error_response (404 on Err) routes.rs:42
```
Sibling handlers (heuristic): `get_export_file` (routes.rs:303) reads
`exports/<file>` directly; `open_folder` (routes.rs:367) → `ensure_project_open`
→ `xdg-open` (session root, or `exports/` if body `{"target":"exports"}`);
`set_baseline` (routes.rs:436) → `ensure_project_open` → `set_baseline_iteration`.
Frontend (heuristic): `ApproveModal.svelte doExport()` → POST `.../export` then
GET `.../export/{file}` per file; `Viewer.svelte` toolbar Export button opens
`ApproveModal` export-only mode; both have `doOpenFolder()` → POST `.../open-folder`.
### ws::handle_socket() → AppCore (heuristic — cross-file edges)
```
handle_socket (ws.rs:74)
├─ init_session:120 → core_for(mod.rs:32) → open_project_by_id(app.rs:203) → snapshot_value
├─ [ws text] handle_client_message:140 → "prompt"→submit_prompt(app.rs:294)
│  "approve_phase"→approve_phase(app.rs:174) "advance"/"go_back"→try_switch_phase(phase_dispatch.rs:280)
│  "cancel_stream"→cancel(app.rs:276)
└─ [50ms tick] poll_core_events:206 → poll_events(app.rs:1186)
   (CoreEvent→JSON incl. request_phase_change → PhaseSwitchModal trigger)
```
(`frontend/src/lib/ws.ts::connectSession` is the client counterpart; not graphed.)
### artifacts::get_artifact() — artifacts.rs:61 (LSP: router:27 only incoming call)
```
get_artifact → parse_artifact_file:46 → resolve_artifact_path:107 (iterations/ then root) → read bytes
```
### submit_prompt() → phase dispatch → Claude CLI (heuristic; caller ws.rs "prompt")
```
submit_prompt (app.rs:294) → push_message(app.rs:225) → [by Phase]
send_{spec,build,refine}_prompt (phase_dispatch.rs:17,109,120)
→ ClaudeBridge::send_phase_prompt (claude_bridge.rs:156)
→ claude::send_with_phase_prompt (claude.rs:267) → send_prompt:56
→ [spawns `claude` CLI, mcp_config → mcp/server.py]
also: import_step_file(app.rs:854); SessionManager/storage CRUD
```
### mcp/server.py::handle_tool_call() — line 1022 (heuristic, `rg` call sites)
```
handle_tool_call: "ask_question"/"ask_clarification"→option-chip payload:1025
"request_phase_change"→client prompt:1033 "build"/"assemble"/"analyze"/"paramset"
→ python -m ai3d_cad.* ; handle_import_step:610 ; run_cadquery_build:714
→ glb_export.export_iteration:116 → load_placements→apply_placements→build_scene_nodes
```

## Hot-symbol callers  (per codemap hot symbol)
- `main` ← process entry only; `run_blocking` ← `main`
- `routes::router` ← `run_blocking` (LSP, 1 incoming call)
- `artifacts::router` ← `routes::router` (LSP, `.merge` routes.rs:27→29:55)
- `export_project`/`open_folder` ← routed by `routes::router` (routed, not called)
- `AppCore::export_artifacts` ← `export_project` (routes.rs:281, LSP) + `handle_export` (phase_dispatch.rs:260, LSP)
- `AppCore::submit_prompt` ← `ws::handle_client_message` (heuristic) + inline test fns
- `AppCore::try_switch_phase` ← phase-gate tests (phase_dispatch.rs:344-448) + ws "advance"/"go_back" (heuristic)
- `get_artifact` ← `artifacts::router` (LSP, only incoming call)
- `send_{spec,build,refine}_prompt` ← `submit_prompt` (heuristic) → `send_phase_prompt`(claude_bridge.rs:156), `generate_mcp_config`(claude_bridge.rs:228)
- `glb_export::build_scene_nodes` ← `export_iteration` (glb_export.py:116, heuristic)
- `mcp/server.py::handle_tool_call` ← Python, out of scope (called only from `claude` CLI subprocess)

## Nesting             (per file with non-trivial nesting)
### src/core/app.rs — impl AppCore
```
new:94 approve_phase:174 open_project_by_id:203 push_message:225 cancel:276
submit_prompt:294 import_step_file:854 handle_tool_call:1072
describe_tool_call:1168 poll_events:1186 + ~60 inline #[cfg(test)] fns
```
### src/phase_dispatch.rs — impl AppCore
```
send_spec_prompt:17 handle_spec_response:67 send_build_prompt:109
send_refine_prompt:120 handle_param_edit:140 export_artifacts:171
handle_export:259 undo_component:270 try_switch_phase:280
mod tests:314 (phase-switch-request suite:344-448)
```
### src/server/ (all files)
```
mod.rs:       core_for:32  run_blocking:55
routes.rs:    router:14  list_projects:46  list_refs:101  create_project:168  delete_project:188
              ensure_project_open:217  encode_path_segment:235  export_project:259
              get_export_file:303  open_folder:367  set_baseline:436
ws.rs:        router:30  handle_socket:74  init_session:120  handle_client_message:140
              poll_core_events:206  snapshot_value:280
artifacts.rs: router:27  parse_artifact_file:46  get_artifact:61  resolve_artifact_path:107  glb_iterations:146
assets.rs:    static_handler:70
```
### python/src/ai3d_cad/glb_export.py & mcp/server.py
```
glb_export.py: write_manifest:49 export_iteration:116 load_placements:153 apply_placements:172 build_scene_nodes:211
server.py: request_phase_change:251 ask_clarification:320,364 handle_import_step:610 run_cadquery_build:714 handle_tool_call:1022
```
