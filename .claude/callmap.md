# Callmap: AI3D   (arrows: caller → callee; `X ← Y` means Y calls X)
commit: 5b0a29b | generated: 2026-08-05 | resolution: lsp

## Entry chains        (one call tree per entry point, ~3 levels deep)

### main() — src/main.rs:1964
```
main
├─ Config::load                          src/config.rs:63
├─ startup_checks                        src/main.rs:1857
├─ App::new                              src/main.rs:122
├─ make_fallback_app (on init failure)   src/main.rs:1876
├─ ratatui::init
├─ run_event_loop                        src/main.rs:2045
│  ├─ ClaudeBridge::drain_streaming      src/claude_bridge.rs:65
│  ├─ ClaudeBridge::try_recv_result      src/claude_bridge.rs:85
│  │  └─ App::handle_bg_result           src/main.rs:1031
│  │     └─ SessionManager::build        src/session_manager.rs:147
│  ├─ ClaudeBridge::drain_tool_calls     src/claude_bridge.rs:76
│  │  └─ App::handle_tool_call           src/main.rs:1704
│  ├─ event::poll / event::read          crossterm
│  │  └─ event_handler::handle_key       src/event_handler.rs:11
│  │  └─ App::handle_paste               src/main.rs:581
│  ├─ App::submit_prompt                 src/main.rs:363
│  └─ App::render                        src/main.rs:212
└─ App::cleanup                          src/main.rs:1804
```

### handle_key() — src/event_handler.rs:11
```
handle_key
├─ try_autocomplete                      src/event_handler.rs:460
├─ handle_input_key                      src/event_handler.rs:224
├─ handle_tree_key                       src/event_handler.rs:232
├─ handle_conversation_key               src/event_handler.rs:438
│  ├─ ConversationPane::scroll_up/down   src/tui/conversation.rs:37,42
│  └─ SessionManager::undo               src/session_manager.rs:178
└─ handle_right_panel_key                src/event_handler.rs:449
   └─ phase_dispatch::try_switch_phase   src/phase_dispatch.rs:436
```

### submit_prompt() → phase dispatch → Claude CLI
```
App::submit_prompt (src/main.rs:363)
└─ [routes by Phase to] phase_dispatch::send_spec_prompt (src/phase_dispatch.rs:19)
   ├─ App::build_ref_context             src/main.rs:912
   ├─ ClaudeBridge::generate_mcp_config  src/claude_bridge.rs:207
   └─ ClaudeBridge::send_phase_prompt    src/claude_bridge.rs:107
      └─ claude::send_with_phase_prompt  src/claude.rs:304
         ├─ prompt_builder::load_phase_system_prompt   src/prompt_builder.rs:14
         ├─ prompt_builder::load_engineering_knowledge src/prompt_builder.rs:51
         └─ claude::send_prompt (x3 call sites)         src/claude.rs:128
            └─ [spawns `claude` CLI subprocess, streams via mcp_config → mcp/server.py]
```
(same shape for `send_build_prompt` src/phase_dispatch.rs:203 and `send_refine_prompt` src/phase_dispatch.rs:214 — both call `try_switch_phase` then the same `ClaudeBridge::send_phase_prompt` chain.)

### handle_tool_call() — src/main.rs:1704
```
App::handle_tool_call
├─ SessionManager::add_message           src/session_manager.rs:74
├─ ConversationPane::add                 src/tui/conversation.rs:27
├─ RightPanel::set_spec / set_model      src/tui/right_panel.rs:52,60
├─ SpecPanel::set_content                src/tui/spec_panel.rs:16
└─ Viewer::is_running / show             src/viewer.rs:108,78
```

## Hot-symbol callers  (per codemap hot symbol)
- `main` ← (process entry, no callers)
- `run_event_loop` ← `main` (src/main.rs:2024)
- `submit_prompt` ← `run_event_loop` (src/main.rs:2112)
- `handle_tool_call` (main.rs) ← `run_event_loop` (src/main.rs:2068)
- `handle_bg_result` ← `run_event_loop` (src/main.rs:2061); calls `SessionManager::build` (src/main.rs:1058)
- `handle_key` ← `run_event_loop` (src/main.rs:2126)
- `send_spec_prompt` ← `App::handle_right_panel_key`/dispatch (src/event_handler.rs:658 via `try_switch_phase` path), `App::submit_prompt`
- `try_switch_phase` ← `event_handler::handle_right_panel_key` (src/event_handler.rs:167,171,175)
- `ClaudeBridge::send_phase_prompt` ← `phase_dispatch::send_spec_prompt` (src/phase_dispatch.rs:66)
- `ClaudeBridge::generate_mcp_config` ← `phase_dispatch::send_spec_prompt` (src/phase_dispatch.rs:63)
- `claude::send_with_phase_prompt` ← `ClaudeBridge::send_phase_prompt` (src/claude_bridge.rs:131)
- `claude::send_prompt` ← `send_with_phase_prompt` (src/claude.rs:331,340,347), `ClaudeBridge::send_raw_prompt` (src/claude_bridge.rs:184), `claude::send` (src/claude.rs:81)
- `SessionManager::build` ← `App::handle_bg_result` (src/main.rs:1058)
- `python::assemble` ← no in-repo callers found via LSP (dead/unreached from Rust call graph — likely invoked only via `mcp/server.py`, out of LSP's Rust index)
- `mcp/server.py::handle_tool_call`, `run_cadquery_build`, `builder.py::build` ← Python, no LSP server configured; resolve via `rg '\bhandle_tool_call\('`/`'\brun_cadquery_build\('` if edges needed (out of scope: called only from the external `claude` CLI subprocess, not from Rust)

## Nesting             (per file with non-trivial nesting)
### src/main.rs — impl App<'a>
```
new:122  render:212  submit_prompt:363  handle_ref_command:674  handle_multi_ref:774
save_pending_reference:826  build_ref_context:912  build_phase_context:948
handle_bg_result:1031  handle_build_result:1099  load_session:1154  open_project:1221
import_step_file:1346  refresh_projects:1455  refresh_refs_panel:1461
restore_right_panel:1483  find_latest_code_py:1572  build_component_context:1611
build_prior_builds_context:1669  handle_tool_call:1704  cleanup:1804
free fns: percent_decode:1811 find_system_prompt:1831 startup_checks:1857
which_exists:1866 make_fallback_app:1876 seed_references:1920 briefing_name:1940
main:1964 run_event_loop:2045
```
### src/event_handler.rs — impl App<'a> (event handling)
```
handle_key:11  handle_input_key:224  handle_tree_key:232  handle_conversation_key:438
handle_right_panel_key:449  handle_paste:583
free fn: try_autocomplete:460
```
### src/phase_dispatch.rs — impl App<'a> (phase-specific senders)
```
send_spec_prompt:19  handle_spec_response:69  parse_and_display_components:145
send_build_prompt:203  send_refine_prompt:214  handle_component_build_result:325
undo_component:406  try_switch_phase:436
```
### src/claude_bridge.rs — impl ClaudeBridge
```
drain_streaming:65  drain_tool_calls:76  try_recv_result:85  cancel:90
send_phase_prompt:107  send_raw_prompt:164
free fns: generate_mcp_config:207 find_mcp_server:235 find_cadquery_python:257
```
