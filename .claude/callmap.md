# Callmap: AI3D   (arrows: caller → callee; `X ← Y` means Y calls X)
commit: 35b018b | generated: 2026-08-05 | resolution: heuristic

Note: LSP unavailable this pass — edges are rg-heuristic. TUI-only chains from
the prior pass are gone (event_handler.rs, viewer.rs, render.rs etc. deleted).

## Entry chains        (one call tree per entry point, ~3 levels deep)

### main() — src/main.rs:52  (only entry point now — no TUI branch)
```
main
├─ Cli::parse
├─ [stdin not a tty] read piped stdin → briefing: Option<String>
├─ Config::load
├─ startup_checks                                    src/main.rs:46
│  ├─ claude::check_claude
│  └─ python::check_python
└─ server::run_blocking(config, port, briefing, on_bound)   src/server/mod.rs:61
   ├─ [if briefing.is_some()] AppCore::new(config, briefing)   src/core/app.rs:87
   │  (project_id derived from core.session_dir() parent dir name; seeded into ServerState.cores)
   ├─ ServerState{ config, cores, briefing_project } wrapped in Arc<Mutex<_>>
   ├─ routes::router(state)                            src/server/routes.rs:13
   │  ├─ GET/POST /api/projects → list_projects/create_project   src/server/routes.rs:41,146
   │  ├─ DELETE /api/projects/{id} → delete_project              src/server/routes.rs:166
   │  ├─ GET /api/refs → list_refs                                src/server/routes.rs:99
   │  ├─ .merge(ws::router)                                       src/server/ws.rs:26
   │  ├─ .merge(artifacts::router)                                src/server/artifacts.rs:22
   │  └─ .fallback(assets::static_handler)                        src/server/assets.rs:70
   └─ axum::serve(listener, app)
```
`on_bound` prints `listening on http://{addr}` and opens a browser unless `--no-browser`.

### ws::handle_socket() → AppCore (heuristic — grep-confirmed call sites in src/server/ws.rs)
```
handle_socket (src/server/ws.rs:70)
├─ init_session                                       src/server/ws.rs:116
│  ├─ ServerState::core_for                            src/server/mod.rs:37  (lazily inserts AppCore per project id)
│  ├─ AppCore::set_phase_gate(true)                    src/core/app.rs:144
│  ├─ AppCore::open_project_by_id                      src/core/app.rs:195
│  └─ snapshot_value(core)                              src/server/ws.rs:247
├─ [on ws text] handle_client_message                  src/server/ws.rs:136
│  ├─ "prompt"        → AppCore::submit_prompt          src/core/app.rs:268   (text, part_refs, lib_refs from JSON)
│  ├─ "approve_phase" → AppCore::approve_phase          src/core/app.rs:166
│  ├─ "advance"/"go_back" → AppCore::try_switch_phase   src/phase_dispatch.rs:210
│  │  └─ Err(SwitchDenied::NotApproved) → error_msg("phase not approved")   src/server/ws.rs:290
│  └─ "cancel_stream" → AppCore::cancel                 src/core/app.rs:250
└─ [on 50ms tick] poll_core_events → AppCore::poll_events   src/server/ws.rs:202 → src/core/app.rs:1136
   └─ maps CoreEvent variants → JSON via snapshot_value/spec_value/phase_state_value/build_progress_value
```
(`frontend/src/lib/ws.ts::connectSession` is the client-side protocol counterpart; not call-graphed.)

### artifacts::get_artifact() — src/server/artifacts.rs:56 (heuristic)
```
get_artifact
├─ parse_artifact_file(file)                          src/server/artifacts.rs:41   (validates iteration_<n>.{glb,manifest.json})
├─ ServerState::core_for / cores lookup                src/server/mod.rs:37
└─ reads bytes from session_dir/iteration_<n>.* on disk, served with content-type
```

### submit_prompt() → phase dispatch → Claude CLI (sole caller now: ws "prompt")
```
AppCore::submit_prompt (src/core/app.rs:268; caller: ws::handle_client_message "prompt")
├─ push_message (many sites)                        src/core/app.rs:217
├─ [by Phase] phase_dispatch::send_spec/build/refine_prompt   src/phase_dispatch.rs:17,109,120
│  └─ ClaudeBridge::send_phase_prompt                src/claude_bridge.rs:156
│     └─ claude::send_with_phase_prompt → claude::send_prompt   src/claude.rs
│        ├─ [spawns `claude` CLI, streams stdout via mcp_config → mcp/server.py]
│        └─ parse_build_progress_line(line) on `BUILD_COMPONENT:` lines   src/claude.rs:42
│           └─ ClaudeBridge progress_tx.send(BuildProgress{..})   src/claude_bridge.rs:65
│              └─ drained by ClaudeBridge::drain_build_progress → AppCore::poll_events → ws build_progress event
├─ handle_ref_command / handle_multi_ref / save_pending_reference   src/core/app.rs (unchanged shape)
├─ import_step_file                                  src/core/app.rs:820
└─ SessionManager::create/reset/add_message/save; storage::project/session CRUD
```

### AppCore::handle_tool_call() — src/core/app.rs:1038
```
AppCore::handle_tool_call
├─ describe_tool_call                    src/core/app.rs:1118
└─ [dispatches build-artifact / spec / model events consumed by poll_events → ws::poll_core_events]
```

## Hot-symbol callers  (per codemap hot symbol)
- `main` ← (process entry, only entry point — no TUI branch to compare against anymore)
- `server::run_blocking` ← `main`, unconditionally — src/main.rs:85
- `AppCore::new` ← `run_blocking` (briefing-seeded project only, src/server/mod.rs:75) AND `ServerState::core_for` (lazy per-project creation, src/server/mod.rs, heuristic) + inline test fns
- `AppCore::submit_prompt` ← `ws::handle_client_message` "prompt" case ONLY (src/server/ws.rs:154) + ~40 inline test fns in src/core/app.rs; no other caller exists now
- `AppCore::poll_events` ← `ws::poll_core_events` (src/server/ws.rs:225) ONLY
- `AppCore::try_switch_phase` ← `ws::handle_client_message` "advance"/"go_back" (src/server/ws.rs:161,175) + phase-gate test suite (src/core/app.rs:1691-1780)
- `set_phase_gate`/`approve_phase`/`is_phase_approved`/`open_project_by_id` ← `ws::init_session`/`handle_client_message` + their own inline tests; gate is now unconditionally on for every session (no TUI to leave it off)
- `claude::parse_build_progress_line` ← `claude::send_prompt` (src/claude.rs:220, inside the stdout-line loop) + its own unit tests (src/claude.rs:365-387)
- `mcp/server.py::handle_tool_call` etc. ← Python, out of scope (only called from the external `claude` CLI subprocess)

## Nesting             (per file with non-trivial nesting)
### src/core/app.rs — impl AppCore  (1781 lines; TUI accessors removed)
```
new:87  set_phase_gate:144  is_phase_approved:151  approve_phase:166
open_project_by_id:195  push_message:217  submit_prompt:268  build_phase_context:523
handle_bg_result:591  handle_build_result:631  load_session:666  open_project:715
import_step_file:820  restore_right_panel:916  handle_tool_call:1038
describe_tool_call:1118  poll_events:1136
~50 inline #[cfg(test)] fns from 1247 on, incl. phase-gate suite at 1691-1780
```
### src/server/ (all files)
```
mod.rs:      core_for:37  run_blocking:61
routes.rs:   router:13  list_projects:41  list_refs:99  is_valid_project_name:132  create_project:146  delete_project:166
ws.rs:       router:26  upgrade:37  origin_allowed:55  handle_socket:70  init_session:116
             handle_client_message:136  poll_core_events:202  snapshot_value:247  spec_value:269
artifacts.rs: router:22  parse_artifact_file:41  get_artifact:56  glb_iterations:98
assets.rs:   content_type_for:34  lookup (feature-gated x2):57,63  static_handler:70
```
### src/claude_bridge.rs (gained BuildProgress channel) / src/phase_dispatch.rs (unchanged)
```
claude_bridge.rs:  new:80  drain_build_progress:126  cancel:140  send_phase_prompt:156
                   free fns: generate_mcp_config:228  find_mcp_server:256  find_cadquery_python:278
phase_dispatch.rs: send_spec_prompt:17  send_build_prompt:109  send_refine_prompt:120  try_switch_phase:210
```
