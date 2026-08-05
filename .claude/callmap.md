# Callmap: AI3D (Smidr)   (arrows: caller → callee; `X ← Y` means Y calls X)

commit: b92a73c | generated: 2026-08-05 | resolution: lsp

LSP (rust-analyzer) answered incomingCalls for all hot symbols; edges it
missed cross-file are filled in via `rg` and marked (heuristic).

## Entry chains        (one call tree per entry point, ~3 levels deep)

### main() — src/main.rs:52
```
main
├─ Cli::parse
├─ [stdin not a tty] read piped stdin → briefing: Option<String>
├─ Config::load
├─ startup_checks                                    src/main.rs (heuristic)
│  ├─ claude::check_claude
│  └─ python::check_python
└─ server::run_blocking(config, port, briefing, on_bound)   src/server/mod.rs:55
   ├─ storage::project::migrate_legacy_root             src/storage/project.rs:47
   ├─ [if briefing.is_some()] AppCore::new(config, briefing)   src/core/app.rs:94
   ├─ ServerState{ config, cores, briefing_project } wrapped in Arc<Mutex<_>>
   ├─ routes::router(state)                              src/server/routes.rs:13
   │  ├─ GET/POST /api/projects → list_projects/create_project   src/server/routes.rs:41,146
   │  ├─ DELETE /api/projects/{id} → delete_project              src/server/routes.rs:166
   │  ├─ GET /api/refs → list_refs                                src/server/routes.rs:99
   │  ├─ .merge(ws::router)                                       src/server/ws.rs:30
   │  ├─ .merge(artifacts::router)                                src/server/artifacts.rs:22
   │  └─ .fallback(assets::static_handler)                        src/server/assets.rs:70
   └─ axum::serve(listener, app)
```

### ws::handle_socket() → AppCore (heuristic — cross-file edges rust-analyzer didn't resolve)
```
handle_socket (src/server/ws.rs:74)
├─ init_session                                       src/server/ws.rs:120
│  ├─ ServerState::core_for                            src/server/mod.rs:32
│  ├─ AppCore::open_project_by_id                      src/core/app.rs:203
│  └─ snapshot_value(core)                              src/server/ws.rs:280
├─ [on ws text] handle_client_message                  src/server/ws.rs:140
│  ├─ "prompt"        → AppCore::submit_prompt          src/core/app.rs:294
│  ├─ "approve_phase" → AppCore::approve_phase          src/core/app.rs:174
│  ├─ "advance"/"go_back" → AppCore::try_switch_phase   src/phase_dispatch.rs:210
│  │  └─ Err(SwitchDenied::NotApproved) → error_msg      src/server/ws.rs:335
│  └─ "cancel_stream" → AppCore::cancel                 src/core/app.rs:276
└─ [on 50ms tick] poll_core_events → AppCore::poll_events   src/server/ws.rs:206 → src/core/app.rs:1186
   (maps CoreEvent variants → JSON via snapshot_value/spec_value/phase_state_value/build_progress_value)
```
(`frontend/src/lib/ws.ts::connectSession` is the client-side protocol counterpart; not call-graphed.)

### artifacts::get_artifact() — src/server/artifacts.rs:56 (heuristic)
```
get_artifact → parse_artifact_file(file):41 → ServerState::core_for:mod.rs:32
             → reads session_dir/iteration_<n>.* from disk, served with content-type
```

### submit_prompt() → phase dispatch → Claude CLI (LSP-confirmed: real production caller is ws.rs:129,160; LSP incomingCalls on app.rs only surfaced same-file test callers)
```
AppCore::submit_prompt (src/core/app.rs:294; production caller: ws::handle_client_message "prompt", ws.rs:129/160)
├─ push_message                                      src/core/app.rs:225
├─ [by Phase] phase_dispatch::send_spec_prompt/send_build_prompt/send_refine_prompt   src/phase_dispatch.rs:17,109,120
│  (LSP-confirmed callers of ClaudeBridge::send_phase_prompt)
│  └─ ClaudeBridge::send_phase_prompt                src/claude_bridge.rs:156
│     └─ claude::send_with_phase_prompt              src/claude.rs:267  (LSP-confirmed caller)
│        └─ claude::send_prompt                       src/claude.rs:56
│           └─ [spawns `claude` CLI, streams stdout via mcp_config → mcp/server.py]
├─ import_step_file                                  src/core/app.rs:854
└─ SessionManager::create/reset/add_message/save; storage::project/session CRUD
```

### AppCore::handle_tool_call() — src/core/app.rs:1072 (heuristic)
```
AppCore::handle_tool_call
├─ describe_tool_call                    src/core/app.rs:1168
└─ [dispatches build-artifact / spec / model events consumed by poll_events → ws::poll_core_events]
```

## Hot-symbol callers  (per codemap hot symbol, from LSP incomingCalls)
- `main` ← process entry only (no other caller)
- `AppCore::new` (app.rs:94) ← `run_blocking` (server/mod.rs:81), `core_for` (server/mod.rs:48), + `test_core`/`new_core_starts_in_spec_phase`/`new_with_briefing_...` tests
- `ServerState::core_for` (server/mod.rs:32) ← `run_blocking` (server/mod.rs:74)
- `routes::router` (routes.rs:13) ← `run_blocking` (server/mod.rs:98)
- `AppCore::submit_prompt` (app.rs:294) ← `ws::handle_client_message` (ws.rs:129,160, heuristic) + ~16 inline test fns in app.rs
- `AppCore::try_switch_phase` (phase_dispatch.rs:210) ← 6 phase-gate/dispatch tests in app.rs + phase_dispatch.rs:305 + `ws::handle_client_message` "advance"/"go_back" (heuristic)
- `phase_dispatch::send_{spec,build,refine}_prompt` ← `AppCore::submit_prompt` (app.rs:496,503,509,511); each in turn calls `ClaudeBridge::send_phase_prompt` (claude_bridge.rs:156, from phase_dispatch.rs:61,111,133) and `generate_mcp_config` (claude_bridge.rs:228, from phase_dispatch.rs:64,115,137)
- `claude::send_with_phase_prompt` (claude.rs:267) ← `ClaudeBridge::send_phase_prompt` (claude_bridge.rs:281)
- `prompt_builder::load_phase_system_prompt` (prompt_builder.rs:16) ← `send_with_phase_prompt` (claude.rs:281) + `test_load_system_prompt`
- `storage::project::migrate_legacy_root` (project.rs:47) ← `run_blocking` (server/mod.rs:74), `ensure_root` (project.rs:62)
- `mcp/server.py::handle_tool_call` ← Python, out of scope (called only from the `claude` CLI subprocess)

## Nesting             (per file with non-trivial nesting)
### src/core/app.rs — impl AppCore
```
new:94  set_phase_gate:152  is_phase_approved:159  approve_phase:174
open_project_by_id:203  push_message:225  reset_conversation:231  push_event:235
messages:240  pending_question:245  clear_pending_question:252  phase:264
spec_content:268  session_dir:272  cancel:276  briefing_pending:280
clear_briefing_pending:284  submit_prompt:294  build_ref_context:517
build_phase_context:552  handle_bg_result:620  handle_build_result:660
load_session:695  open_project:748  import_step_file:854  refresh_projects:945
restore_right_panel:950  find_latest_code_py:1002  build_prior_builds_context:1038
handle_tool_call:1072  describe_tool_call:1168  poll_events:1186
~60 inline #[cfg(test)] fns from 1297 on, incl. phase-gate suite ~1740-1810
```
### src/server/ (all files)
```
mod.rs:       core_for:32  run_blocking:55
routes.rs:    router:13  list_projects:41  list_refs:99  create_project:146  delete_project:166
ws.rs:        router:30  upgrade:41  origin_allowed:59  handle_socket:74  init_session:120
              handle_client_message:140  poll_core_events:206  snapshot_value:280
              pending_question_value:303  spec_value:314  phase_state_value:331  build_progress_value:341
artifacts.rs: router:22  parse_artifact_file:41  get_artifact:56  glb_iterations:98
assets.rs:    content_type_for:34  lookup (feature-gated x2):57,63  static_handler:70
```
### src/claude_bridge.rs / src/phase_dispatch.rs
```
claude_bridge.rs:  new:80  send_phase_prompt:156  generate_mcp_config:228  find_mcp_server:256
phase_dispatch.rs: send_spec_prompt:17  send_build_prompt:109  send_refine_prompt:120  try_switch_phase:210
```
