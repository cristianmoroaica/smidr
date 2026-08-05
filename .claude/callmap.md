# Callmap: AI3D   (arrows: caller → callee; `X ← Y` means Y calls X)
commit: 587e71e | generated: 2026-08-05 | resolution: lsp

## Entry chains        (one call tree per entry point, ~3 levels deep)

### main() — src/main.rs:459
```
main
├─ Config::load                          src/config.rs:63
├─ startup_checks                        src/main.rs:410
├─ App::new                              src/main.rs:86
│  └─ AppCore::new                       src/core/app.rs:127
│     └─ AppCore::new_with               src/core/app.rs:136
├─ make_fallback_app (on init failure)   src/main.rs:429
├─ ratatui::init
├─ run_event_loop                        src/main.rs:540
│  ├─ AppCore::poll_events               src/core/app.rs:1700
│  │  ├─ ClaudeBridge::drain_streaming   src/claude_bridge.rs:93
│  │  ├─ ClaudeBridge::try_recv_result   src/claude_bridge.rs:113
│  │  │  └─ AppCore::handle_bg_result    src/core/app.rs:1051
│  │  └─ ClaudeBridge::drain_tool_calls  src/claude_bridge.rs:104
│  │     └─ AppCore::handle_tool_call    src/core/app.rs:1589
│  ├─ App::sync_from_core                src/main.rs:266
│  ├─ App::handle_core_event             src/main.rs:358
│  ├─ event::poll / event::read          crossterm
│  │  └─ event_handler::handle_key       src/event_handler.rs:13
│  │  └─ App::handle_paste               src/event_handler.rs:572
│  ├─ App::submit                        src/main.rs:315
│  │  └─ AppCore::submit_prompt          src/core/app.rs:407
│  └─ App::render                        src/main.rs:119
└─ App::cleanup                          src/main.rs:404
```

### handle_key() — src/event_handler.rs:13  (impl App, delegates to AppCore)
```
handle_key
├─ switch_phase                          src/event_handler.rs:207
│  └─ AppCore::try_switch_phase          src/phase_dispatch.rs:228
├─ try_autocomplete                      src/event_handler.rs:451
├─ handle_input_key                      src/event_handler.rs:217
├─ handle_tree_key                       src/event_handler.rs:225
├─ handle_conversation_key               src/event_handler.rs:429
│  ├─ ConversationPane::scroll_up/down   src/tui/conversation.rs:37,42
│  └─ AppCore::undo                      src/core/app.rs:340
└─ handle_right_panel_key                src/event_handler.rs:440
   ├─ AppCore::busy/cancel/save_session  src/core/app.rs:258,332,336
   └─ App::sync_from_core                src/main.rs:266
```

### submit_prompt() → phase dispatch → Claude CLI
```
AppCore::submit_prompt (src/core/app.rs:407, called via App::submit src/main.rs:315)
├─ push_message (x30 call sites)                  src/core/app.rs:211
├─ [routes by Phase to] phase_dispatch::send_spec_prompt   src/phase_dispatch.rs:17
│  (send_build_prompt:111 / send_refine_prompt:122 same shape)
│  └─ ClaudeBridge::send_phase_prompt              src/claude_bridge.rs:107 (unchanged)
│     └─ claude::send_with_phase_prompt            src/claude.rs (unchanged)
│        └─ claude::send_prompt                    src/claude.rs:37
│           └─ [spawns `claude` CLI subprocess, streams via mcp_config → mcp/server.py]
├─ handle_ref_command / handle_multi_ref           src/core/app.rs:715,814
├─ save_pending_reference                          src/core/app.rs:864
├─ import_step_file                                src/core/app.rs:1320
├─ SessionManager::create/reset/add_message/save   src/session_manager.rs:99,113,74,89
└─ storage::project/session (create/delete/rename) src/storage/project.rs, src/storage/session.rs
```

### AppCore::handle_tool_call() — src/core/app.rs:1589
```
AppCore::handle_tool_call
├─ describe_tool_call                    src/core/app.rs:1680
└─ [dispatches build-artifact / spec / model events consumed by poll_events → App::handle_core_event]
```
(Note: `SessionManager::add_message`, `RightPanel`/`SpecPanel` mutation now happen through `CoreEvent`s drained by `App::handle_core_event`/`sync_from_core` in src/main.rs, not directly inside `handle_tool_call` as in the pre-refactor version.)

## Hot-symbol callers  (per codemap hot symbol; LSP-verified)
- `main` ← (process entry, no callers)
- `run_event_loop` ← `main` (src/main.rs:519)
- `AppCore::submit_prompt` ← `App::submit` (src/main.rs:319); also called from 10 inline `#[cfg(test)]` fns in src/core/app.rs
- `AppCore::new_with` ← `AppCore::new` (src/core/app.rs:128); `AppCore::new` ← `App::new` (src/main.rs:87) + 3 test fns
- `AppCore::poll_events` ← `run_event_loop` (src/main.rs:547)
- `AppCore::handle_tool_call` ← `AppCore::poll_events` (src/core/app.rs:1717)
- `AppCore::handle_bg_result` ← `AppCore::poll_events` (src/core/app.rs:1709)
- `handle_key` ← `run_event_loop` (src/main.rs:580)
- `AppCore::try_switch_phase` (src/phase_dispatch.rs:228) ← `event_handler::switch_phase` (src/event_handler.rs:210); also 3 inline test fns
- `phase_dispatch::send_spec_prompt` ← `AppCore::submit_prompt` (src/core/app.rs:697); `send_build_prompt`/`send_refine_prompt` similarly at 704/708
- `ClaudeBridge::send_phase_prompt` / `claude::send_with_phase_prompt` / `claude::send_prompt` — call chain unchanged from prior refactor, now originating in `phase_dispatch.rs` (`impl AppCore`) instead of `main.rs`
- `mcp/server.py::handle_tool_call`, `run_cadquery_build`, `builder.py::build` ← Python, no LSP server configured; resolve via `rg '\bhandle_tool_call\('` if edges needed (called only from the external `claude` CLI subprocess, not from Rust)

## Nesting             (per file with non-trivial nesting)
### src/core/app.rs — impl AppCore  (extracted from old main.rs `impl App`)
```
new:127  new_with:136  push_message:211  submit_prompt:407  handle_ref_command:715
handle_multi_ref:814  save_pending_reference:864  build_ref_context:948
build_phase_context:983  handle_bg_result:1051  handle_build_result:1114
load_session:1161  open_project:1214  import_step_file:1320  refresh_projects:1416
refresh_refs_panel:1421  restore_right_panel:1443  find_latest_code_py:1519
build_prior_builds_context:1556  handle_tool_call:1589  describe_tool_call:1680
poll_events:1700  cleanup:401
free fns: percent_decode:1761 seed_references:1781 briefing_name:1801
~30 inline #[cfg(test)] fns from line 1832 on
```
### src/main.rs — impl App<'a>  (TUI-only, post-refactor, 642 lines total)
```
new:86  render:119  sync_from_core:267  submit:318  open_project:327
load_session:334  handle_core_event:358  cleanup:405
free fns: startup_checks:410 which_exists:419 make_fallback_app:429
main:459 run_event_loop:540
```
### src/event_handler.rs — impl App<'a> (event handling, delegates to AppCore)
```
handle_key:13  switch_phase:209  handle_input_key:217  handle_tree_key:225
handle_conversation_key:429  handle_right_panel_key:440  handle_paste:574
free fns: try_autocomplete:452  longest_common_prefix:617
```
### src/phase_dispatch.rs — impl AppCore (moved from `impl App`)
```
send_spec_prompt:17  handle_spec_response:67  send_build_prompt:111
send_refine_prompt:122  handle_param_edit:142  handle_export:173
undo_component:193  try_switch_phase:228
```
