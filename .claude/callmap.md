# Callmap: AI3D   (arrows: caller → callee; `X ← Y` means Y calls X)
commit: 3bd2345 | generated: 2026-08-05 | resolution: heuristic

Note: LSP unavailable this pass (server disconnected) — edges are rg-heuristic
(call-site grep), not LSP-verified. TUI-path lines refreshed from outlines only.

## Entry chains        (one call tree per entry point, ~3 levels deep)

### main() — src/main.rs:479
```
main
├─ Cli::parse → if cli.web:
│  └─ server::run_blocking                           src/server/mod.rs:46
│     ├─ routes::router                                src/server/routes.rs:13
│     │  ├─ list_projects/create_project/delete_project  src/server/routes.rs:38,114,134
│     │  ├─ .merge(ws::router)                          src/server/ws.rs:25
│     │  │  └─ ws::upgrade → ws::handle_socket           src/server/ws.rs:36,44
│     │  └─ .fallback(assets::static_handler)            src/server/assets.rs:70
│     └─ axum::serve(listener, app)
├─ [else, TUI] Config::load → startup_checks           src/main.rs:430
├─ App::new                                           src/main.rs:106
│  └─ AppCore::new → AppCore::new_with                 src/core/app.rs:138,147
├─ make_fallback_app (on init failure)                 src/main.rs:449
├─ run_event_loop                                      src/main.rs:580
│  ├─ AppCore::poll_events                             src/core/app.rs:1784
│  │  ├─ ClaudeBridge::try_recv_result → AppCore::handle_bg_result    src/core/app.rs:1135
│  │  └─ ClaudeBridge::drain_tool_calls → AppCore::handle_tool_call   src/core/app.rs:1674
│  ├─ App::sync_from_core / handle_core_event           src/main.rs:287,378
│  ├─ event_handler::handle_key / App::handle_paste     src/event_handler.rs:13,574
│  ├─ App::submit → AppCore::submit_prompt              src/main.rs:338 → src/core/app.rs:493
│  └─ App::render                                       src/main.rs:139
└─ App::cleanup                                         src/main.rs:425
```

### ws::handle_socket() → AppCore (new, heuristic — grep-confirmed in src/server/ws.rs)
```
handle_socket (src/server/ws.rs:44)
├─ init_session                                       src/server/ws.rs:90
│  ├─ ServerState::core_for                            src/server/mod.rs:31  (lazily inserts AppCore per project id)
│  ├─ AppCore::set_phase_gate(true)                    src/core/app.rs:223
│  ├─ AppCore::open_project_by_id                      src/core/app.rs:274
│  └─ snapshot_value(core)                              src/server/ws.rs:197
├─ [on ws text] handle_client_message                  src/server/ws.rs:98
│  ├─ "prompt"        → AppCore::submit_prompt          src/core/app.rs:493
│  ├─ "approve_phase" → AppCore::approve_phase          src/core/app.rs:245
│  ├─ "advance"/"go_back" → AppCore::try_switch_phase   src/phase_dispatch.rs:234
│  │  └─ Err(SwitchDenied::NotApproved) → error_msg("phase not approved")  src/server/ws.rs:223
│  └─ "cancel_stream" → AppCore::cancel                 src/core/app.rs:416
└─ [on 50ms tick] poll_core_events → AppCore::poll_events   src/server/ws.rs:164 → src/core/app.rs:1784
   └─ maps CoreEvent::{StreamDelta,ToolCall,BuildArtifact,Error,ResponseDone} → JSON
```
(`frontend/src/lib/ws.ts` is the client-side protocol counterpart; not call-graphed.)

### handle_key() — src/event_handler.rs:13  (unchanged; try_switch_phase now gate-aware)
```
handle_key
├─ switch_phase → AppCore::try_switch_phase   src/event_handler.rs:209 → src/phase_dispatch.rs:234
├─ try_autocomplete / handle_input_key / handle_tree_key   src/event_handler.rs:452,217,225
├─ handle_conversation_key → ConversationPane::scroll_up/down, AppCore::undo   src/event_handler.rs:429
└─ handle_right_panel_key → AppCore::busy/cancel/save_session, App::sync_from_core   src/event_handler.rs:440
```

### submit_prompt() → phase dispatch → Claude CLI (unchanged shape; TUI + web share this path)
```
AppCore::submit_prompt (src/core/app.rs:493; callers: App::submit [TUI] and ws "prompt" [web])
├─ push_message (many sites)                        src/core/app.rs:297
├─ [by Phase] phase_dispatch::send_spec/build/refine_prompt   src/phase_dispatch.rs:17,111,122
│  └─ ClaudeBridge::send_phase_prompt → claude::send_with_phase_prompt → claude::send_prompt
│     └─ [spawns `claude` CLI, streams via mcp_config → mcp/server.py]
├─ handle_ref_command / handle_multi_ref / save_pending_reference   src/core/app.rs:799,898,948
├─ import_step_file                                  src/core/app.rs:1406
└─ SessionManager::create/reset/add_message/save; storage::project/session CRUD
```

## Hot-symbol callers  (per codemap hot symbol)
- `main` ← (process entry, no callers)
- `server::run_blocking` ← `main` (`cli.web` branch, new) — src/main.rs:486
- `run_event_loop` ← `main` (TUI branch only)
- `AppCore::submit_prompt` ← `App::submit` (TUI) AND `ws::handle_client_message` "prompt" (web, new); also ~10 inline test fns
- `AppCore::new`/`new_with` ← `App::new`/`make_fallback_app` (TUI) + test fns; web path uses `ServerState::core_for` instead (heuristic)
- `AppCore::poll_events` ← `run_event_loop` (TUI) AND `ws::poll_core_events` (web, new)
- `AppCore::try_switch_phase` ← `event_handler::switch_phase` (TUI) AND `ws` "advance"/"go_back" (web, new); also new gate test suite at src/core/app.rs:2259-2336
- `set_phase_gate`/`approve_phase`/`is_phase_approved`/`open_project_by_id` (all new) ← only `ws.rs` + inline tests; no TUI caller (gate off by default)
- `mcp/server.py::handle_tool_call` etc. ← Python, out of scope (called only from external `claude` CLI subprocess)

## Nesting             (per file with non-trivial nesting)
### src/core/app.rs — impl AppCore  (2337 lines; gained phase-gate methods)
```
new:138  new_with:147  set_phase_gate:223  is_phase_approved:230  approve_phase:245
open_project_by_id:274  push_message:297  submit_prompt:493  handle_ref_command:799
handle_multi_ref:898  save_pending_reference:948  build_phase_context:1067
handle_bg_result:1135  handle_build_result:1198  load_session:1245  open_project:1298
import_step_file:1406  restore_right_panel:1527  handle_tool_call:1674
describe_tool_call:1766  poll_events:1784  cleanup:485
~40 inline #[cfg(test)] fns from 1916 on, incl. phase-gate suite at 2247-2336
```
### src/server/ (new)
```
mod.rs:    core_for:31  run_blocking:46
routes.rs: router:13  list_projects:38  is_valid_project_name:100  create_project:114  delete_project:134
ws.rs:     router:25  upgrade:36  handle_socket:44  init_session:90  handle_client_message:98
           poll_core_events:164  snapshot_value:197  phase_state_value:219  error_msg:223
assets.rs: content_type_for:34  lookup (feature-gated x2):57,63  static_handler:70
```
### src/main.rs — impl App<'a>  (TUI-only, 682 lines; Cli/cli.web dispatch atop `main`)
```
new:106  render:139  sync_from_core:287  submit:338  open_project:347
load_session:354  handle_core_event:378  cleanup:425
free fns: startup_checks:430 which_exists:439 make_fallback_app:449
main:479 run_event_loop:580
```
### src/event_handler.rs (unchanged) / src/phase_dispatch.rs (try_switch_phase gate-aware)
```
event_handler.rs:  handle_key:13  switch_phase:209  handle_tree_key:225  handle_right_panel_key:440
phase_dispatch.rs: send_spec_prompt:17  send_build_prompt:111  send_refine_prompt:122  try_switch_phase:234
```
