# CLI Briefing Pipe — Design Spec

**Date:** 2026-03-23
**Status:** Draft

## Problem

Starting a new mimodel session requires interactively answering Spec phase questions from scratch, even when the user has already explored the design in a prior conversation with another agent. There's no way to bootstrap a session with pre-existing context.

## Solution

Allow piping free-text content (conversation transcripts, notes, briefs) into `mimodel` via stdin. The app detects piped input, creates a project/session automatically, and injects the content as briefing context into the Spec phase. The Spec agent uses the briefing to pre-fill spec fields where information is clear and asks about gaps — the normal Spec flow, but with a head start.

**Invocation:**
```bash
cat conversation.md | mimodel
pbpaste | mimodel
```

## Design

### 1. Stdin Detection & Reading

At the top of `main()`, before TUI initialization, check if stdin is a terminal using `std::io::IsTerminal`. If stdin is not a terminal (i.e., content is piped), read all content into a `String`.

This must happen before ratatui takes over the terminal. Crossterm's `tty_fd()` falls back to opening `/dev/tty` when `isatty(STDIN_FILENO)` returns false, so ratatui's raw mode and event polling work correctly after stdin has been consumed.

```rust
use std::io::{IsTerminal, Read};

let briefing: Option<String> = if !std::io::stdin().is_terminal() {
    let mut buf = Vec::new();
    let mut handle = std::io::stdin().lock();
    let max_bytes = 100 * 1024; // 100KB cap
    handle.take(max_bytes as u64 + 1).read_to_end(&mut buf)
        .map_err(|e| format!("Failed to read piped input: {e}"))?;
    let truncated = buf.len() > max_bytes;
    buf.truncate(max_bytes);
    // Use from_utf8_lossy to handle truncation splitting a multi-byte char
    let mut s = String::from_utf8_lossy(&buf).into_owned();
    if truncated {
        s.push_str("\n[...truncated at 100KB]");
    }
    if s.trim().is_empty() { None } else { Some(s) }
} else {
    None
};
```

**Constraints:**
- Empty input is ignored (app starts normally)
- Input capped at 100KB to avoid blowing up Claude's context window; truncated with a note
- Non-UTF-8 bytes are replaced with U+FFFD (lossy conversion) rather than rejecting the input
- No format requirements — plain text, markdown, chat transcripts with role prefixes, anything

### 2. Auto Session Creation

When briefing content is present, create the project and session before entering the TUI event loop. The briefing is passed into `App::new()` so it can populate `session.active_name`, `session.active_dir`, and `session.phase_session` before the event loop starts — satisfying the guards in `submit_prompt()` so the auto-create logic is skipped.

**Name generation:** Use the same approach as the existing `submit_prompt()` auto-naming: filter to alphanumeric + spaces, take first 30 chars, trim, replace spaces with underscores. The input is the first non-empty line of the briefing (after stripping leading role prefixes like `User:`, `Human:`, `Assistant:`, `AI:`). Fallback name: `"briefing"`.

```rust
fn briefing_name(content: &str) -> String {
    let line = content.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| {
            // Strip common role prefixes
            for prefix in &["User:", "Human:", "Assistant:", "AI:"] {
                if let Some(rest) = l.strip_prefix(prefix) {
                    return rest.trim();
                }
            }
            l
        })
        .find(|l| !l.is_empty())
        .unwrap_or("briefing");

    let name: String = line.chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ')
        .take(30)
        .collect();
    let name = name.trim().replace(' ', "_");
    if name.is_empty() { "briefing".to_string() } else { name }
}
```

**Creation flow:**
1. Generate name from briefing content
2. Create project directory: `~/MiModel/{name}/` with `project.json` via `storage::project::create_project()` — same as manual project creation. If a project with that name already exists, append a numeric suffix (`{name}_2`, `{name}_3`, etc.)
3. Create session directory: `~/MiModel/{name}/{name}/` (session named same as project)
4. Initialize `PhaseSession` in `Phase::Spec`
5. Save raw content to `briefing.md` in the session directory
6. Set `session.active_name`, `session.active_dir`, `session.phase_session` on the `App` struct so `submit_prompt()` sees an active session

**PhaseSessionData field addition:**
```rust
#[serde(default)]
pub briefing: Option<String>,  // Relative path to briefing.md, or None
```

The `#[serde(default)]` ensures existing `session.json` files without this field deserialize correctly.

### 3. Prompt Injection

The briefing content is injected into the Spec phase prompt in `phase_dispatch.rs` within `send_spec_prompt()`. This is where reference context and MCP config are already assembled — the briefing is read from disk and included in the same flow.

When `PhaseSession` has a briefing, `send_spec_prompt()` reads `briefing.md` from the session directory and prepends it to the context passed to `claude_bridge::send_phase_prompt()`.

**Injected context block:**
```markdown
## Prior Conversation (Briefing)

The user has provided a prior conversation that describes what they want to build.
Use this to pre-fill spec fields where the information is clear.
Ask about gaps or ambiguities — do not assume.

<briefing>
{contents of briefing.md}
</briefing>
```

**Position in context hierarchy:**
1. Phase system prompt (`prompts/spec.md`) — unchanged
2. **Briefing context** — injected as additional context alongside reference context
3. Active references — unchanged
4. User message — synthetic first message: `"Please review the attached conversation and begin extracting spec fields."`

**Lifecycle:**
- Briefing is only injected during the Spec phase
- Once the user advances to Build, the briefing is no longer included — `spec.toml` and `goal.md` carry the information forward
- The `briefing.md` file remains in the session directory for reference but is not re-read after Spec

### 4. TUI Behavior After Pipe

After reading stdin and creating the session:
1. Ratatui initializes normally (reclaiming `/dev/tty` for terminal interaction)
2. The TUI opens with the new session active in the project tree
3. The synthetic first prompt is submitted via the normal `submit_prompt()` path — since `active_name`, `active_dir`, and `phase_session` are already set, the auto-create guards are skipped and it dispatches directly to `send_spec_prompt()`
4. The user sees the agent's first response — proposed spec fields and any clarifying questions
5. Normal interactive flow continues from here

## Files Changed

| File | Change |
|------|--------|
| `src/main.rs` | Stdin detection before TUI init, briefing passed to `App::new()`, auto session/project creation when briefing present, auto-submit synthetic prompt on first tick |
| `src/storage/session.rs` | Add `#[serde(default)] pub briefing: Option<String>` field to `PhaseSessionData` |
| `src/phase_dispatch.rs` | In `send_spec_prompt()`, read `briefing.md` from session dir and include as context when present |
| `src/model_session.rs` | Save `briefing.md` to session dir, expose briefing path, accept briefing in constructor |

## Not In Scope

- No new CLI arguments or subcommands
- No changes to `spec.md` prompt — the briefing block provides sufficient instruction
- No changes to Build or Refine phases
- No special parsing of conversation formats — the content is passed as-is
- No Claude API call for name generation — heuristic only
