# CLI Briefing Pipe Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow piping conversation transcripts into `mimodel` via stdin to bootstrap sessions with pre-existing context.

**Architecture:** Detect piped stdin before TUI init, create a project/session from the content, inject it as briefing context into the Spec phase prompt. The Spec agent reads the briefing and pre-fills spec fields, asking only about gaps.

**Tech Stack:** Rust, ratatui, crossterm, serde_json

---

## File Structure

| File | Responsibility |
|------|---------------|
| `src/main.rs` | Stdin detection in `main()`, pass briefing to `App::new()`, one-shot auto-submit flag in event loop |
| `src/storage/session.rs` | Add `briefing` field to `PhaseSessionData` |
| `src/model_session.rs` | Accept briefing in constructor, save `briefing.md`, expose briefing path, include in save/load |
| `src/phase_dispatch.rs` | Read briefing from session dir and inject into `send_spec_prompt()` context |

---

### Task 1: Add `briefing` field to `PhaseSessionData` and fix all struct literals

**Files:**
- Modify: `src/storage/session.rs:19-28`
- Modify: `src/model_session.rs:110` (save method — PhaseSessionData construction)

This task adds the field AND fixes all existing `PhaseSessionData` struct literals so compilation never breaks between tasks.

- [ ] **Step 1: Add the field**

In `PhaseSessionData` struct (`src/storage/session.rs`), add after `component_states`:

```rust
#[serde(default)]
pub briefing: Option<String>,
```

- [ ] **Step 2: Fix PhaseSessionData construction in `model_session.rs:save()`**

In `PhaseSession::save()` (`src/model_session.rs:110`), add `briefing: None` to the `PhaseSessionData` struct literal (will be changed to `self.briefing.clone()` in Task 2):

```rust
component_states: self.components.clone(),
briefing: None,
```

- [ ] **Step 3: Update the serialization test**

In `test_serialize_phase_session_data` (`src/storage/session.rs` line 129), add `briefing: Some("briefing.md".into())` to the test data and assert the JSON contains `"briefing"`.

- [ ] **Step 4: Add backward-compat deserialization test**

Add a test that deserializes a JSON string *without* the `briefing` field and asserts it deserializes to `None`:

```rust
#[test]
fn test_deserialize_session_without_briefing() {
    let json = r#"{
        "name": "test",
        "created": "2026-03-16T12:00:00Z",
        "phase": "Spec",
        "current_component": null,
        "claude_sessions": {},
        "conversations": {},
        "component_states": []
    }"#;
    let data: PhaseSessionData = serde_json::from_str(json).unwrap();
    assert!(data.briefing.is_none());
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test`
Expected: All tests pass, no compilation errors anywhere in the project.

- [ ] **Step 6: Commit**

```bash
git add src/storage/session.rs src/model_session.rs
git commit -m "feat(session): add briefing field to PhaseSessionData"
```

---

### Task 2: Add briefing support to `PhaseSession`

**Files:**
- Modify: `src/model_session.rs:17-27` (struct), `src/model_session.rs:29-51` (new), `src/model_session.rs:104-133` (save), `src/model_session.rs:136-174` (load)

- [ ] **Step 1: Add `briefing` field to `PhaseSession` struct**

Add after `python_path`:

```rust
pub briefing: Option<String>,  // relative path to briefing.md
```

- [ ] **Step 2: Update `PhaseSession::new()` to accept optional briefing**

Change signature to:
```rust
pub fn new(base_dir: PathBuf, build_timeout: u64, python_path: String, briefing_content: Option<&str>) -> Self
```

Before the return statement, save briefing if present:
```rust
let briefing = if let Some(content) = briefing_content {
    let path = base_dir.join("briefing.md");
    fs::write(&path, content).expect("Failed to write briefing.md");
    Some("briefing.md".to_string())
} else {
    None
};
```

Include `briefing` in the returned struct.

- [ ] **Step 3: Update `save()` to include briefing**

In `save()` (line ~110), change the `briefing: None` placeholder (added in Task 1 Step 2) to `briefing: self.briefing.clone()`.

- [ ] **Step 4: Update `load()` to restore briefing**

In `load()` (line ~163), add `briefing: data.briefing` to the returned `PhaseSession`.

- [ ] **Step 5: Fix all call sites of `PhaseSession::new()`**

Search for ALL callers of `PhaseSession::new` and add `None` as the 4th argument. There are two groups:

**Production code — `SessionManager::create()` in `session_manager.rs:100-103`:**

Update `SessionManager::create` signature:
```rust
pub fn create(&mut self, dir: PathBuf, build_timeout: u64, python_path: String, briefing: Option<&str>) {
    self.phase_session = Some(PhaseSession::new(dir.clone(), build_timeout, python_path, briefing));
    self.active_dir = Some(dir);
}
```

Then fix all callers of `session.create(...)` in `main.rs` to pass `None` for the briefing arg. Search for `.create(` calls on `self.session`.

**Test code — ALL `PhaseSession::new()` calls in `src/model_session.rs` tests (lines ~188, 200, 216, 230, 244, 260, 275, 299):**

Every test that calls `PhaseSession::new(path, timeout, python)` must be updated to `PhaseSession::new(path, timeout, python, None)`. There are approximately 8 such call sites in the test module at the bottom of `model_session.rs`. Update them all.

- [ ] **Step 6: Run tests**

Run: `cargo test`
Expected: All tests pass. No compilation errors.

- [ ] **Step 7: Commit**

```bash
git add src/model_session.rs src/session_manager.rs src/main.rs
git commit -m "feat(session): save and load briefing.md in PhaseSession"
```

---

### Task 3: Stdin detection and auto session creation in `main()`

**Files:**
- Modify: `src/main.rs:1910-1953` (main function), `src/main.rs:120-180` (App::new)

- [ ] **Step 1: Add stdin reading before TUI init**

In `main()`, after `startup_checks` and before `App::new`, add:

```rust
// Read piped stdin before TUI takes over
use std::io::{IsTerminal, Read};
let briefing: Option<String> = if !std::io::stdin().is_terminal() {
    let mut buf = Vec::new();
    let max_bytes: usize = 100 * 1024;
    std::io::stdin().lock().take(max_bytes as u64 + 1).read_to_end(&mut buf)
        .unwrap_or_else(|e| {
            eprintln!("Warning: failed to read piped input: {e}");
            0
        });
    let truncated = buf.len() > max_bytes;
    buf.truncate(max_bytes);
    let mut s = String::from_utf8_lossy(&buf).into_owned();
    if truncated {
        s.push_str("\n[...truncated at 100KB]");
    }
    if s.trim().is_empty() { None } else { Some(s) }
} else {
    None
};
```

- [ ] **Step 2: Add `briefing_name` helper function**

Add near the bottom of `main.rs` (before `main()`):

```rust
/// Extract a session/project name from briefing content.
fn briefing_name(content: &str) -> String {
    let line = content.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| {
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

- [ ] **Step 3: Update `App::new()` to accept briefing**

Change signature to `fn new(config: Config, briefing: Option<String>) -> Result<Self, String>`.

When `briefing` is `Some`, after creating `session` and `projects` but before returning:

```rust
if let Some(ref content) = briefing {
    let name = briefing_name(content);

    // Create project, dedup name if exists
    let mut project_name = name.clone();
    let root = storage::project::root_dir();
    let mut suffix = 2;
    while root.join(&project_name).exists() {
        project_name = format!("{}_{}", name, suffix);
        suffix += 1;
    }

    let project_path = storage::project::create_project(&project_name, "")
        .map_err(|e| format!("Failed to create briefing project: {e}"))?;

    let session_dir = project_path.join(&project_name);
    session.create(session_dir.clone(), build_timeout, python_path.clone(), Some(content));
    session.active_name = Some(project_name.clone());
    session.active_dir = Some(session_dir.clone());

    viewer.set_working_dir(&session_dir);

    // Re-scan projects so the tree includes the new one
    projects = storage::project::list_projects().unwrap_or_default();
    project_tree.refresh(&projects);
}
```

Also add a `briefing_pending` field to `App`:
```rust
briefing_pending: bool,
```
Set it to `briefing.is_some()` in the constructor.

- [ ] **Step 4: Update `main()` call sites**

Update the `App::new(config.clone())` call in `main()` to `App::new(config.clone(), briefing)`.

Update `make_fallback_app` (line ~1847): this function constructs an `App` struct literal directly (not via `App::new`), so:
1. Change its signature to accept `briefing: Option<String>` (or just ignore it — fallback apps don't need briefing)
2. Add `briefing_pending: false` to the `App` struct literal inside `make_fallback_app`
3. Update the call in `main()` to `make_fallback_app(config, &e)`

- [ ] **Step 5: Run compilation check**

Run: `cargo build`
Expected: Compiles without errors.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "feat: detect piped stdin and auto-create briefing session"
```

---

### Task 4: Auto-submit synthetic prompt on first tick

**Files:**
- Modify: `src/main.rs:1955-2089` (run_event_loop)

- [ ] **Step 1: Add briefing auto-submit in event loop**

In `run_event_loop`, after the `.open_viewer` signal block (around line 2015) and before the render block, add:

```rust
// Auto-submit briefing prompt after first render (tick_count > 0 ensures one frame rendered first)
if app.briefing_pending && tick_count > 0 {
    app.briefing_pending = false;
    app.focus = Focus::Input;
    let synthetic = "Please review the attached conversation and begin extracting spec fields.".to_string();
    app.submit_prompt(synthetic);
    app.dirty = true;
}
```

- [ ] **Step 2: Run compilation check**

Run: `cargo build`
Expected: Compiles without errors.

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat: auto-submit synthetic prompt for briefing sessions"
```

---

### Task 5: Inject briefing context in `send_spec_prompt`

**Files:**
- Modify: `src/phase_dispatch.rs:19-37`

- [ ] **Step 1: Read briefing and inject into prompt context**

In `send_spec_prompt()`, after the `ref_context` is built (line 20) and before the `prompt` construction (line 22), add:

```rust
// Build briefing context if available
let briefing_context = self.session.phase_session.as_ref()
    .and_then(|ps| ps.briefing.as_ref())
    .and_then(|rel_path| {
        let dir = self.session.active_dir.as_ref()?;
        let path = dir.join(rel_path);
        std::fs::read_to_string(&path).ok()
    })
    .map(|content| {
        format!(
            "## Prior Conversation (Briefing)\n\n\
             The user has provided a prior conversation that describes what they want to build.\n\
             Use this to pre-fill spec fields where the information is clear.\n\
             Ask about gaps or ambiguities — do not assume.\n\n\
             <briefing>\n{}\n</briefing>",
            content
        )
    });
```

Then combine briefing with the prompt. Replace the existing `let prompt = if self.claude.session_id.is_some() { ... }` block with:

```rust
let prompt = if self.claude.session_id.is_some() {
    // Continuing session — only add ref context (briefing already sent on first message)
    if let Some(ref ctx) = ref_context {
        format!("[Reference context]\n{}\n\n{}", ctx, text)
    } else {
        text.to_string()
    }
} else {
    // First message — include briefing if available, but NOT ref_context
    // (ref_context on first message is handled separately by send_phase_prompt)
    if let Some(ref bc) = briefing_context {
        format!("{}\n\n{}", bc, text)
    } else {
        text.to_string()
    }
};
```

Note: The existing behavior is preserved — `ref_context` is only included in the prompt text for continuing sessions (`session_id.is_some()`). For first messages, `ref_context` is passed separately to `send_phase_prompt` (line 36). The briefing is only added on the first message.

- [ ] **Step 2: Run full test suite**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 3: Manual test**

Create a test briefing file and pipe it:
```bash
echo "I want to build a simple dice with rounded edges, about 20mm on each side, with recessed pips" | cargo run
```

Expected: TUI opens with a session named something like `I_want_to_build_a_simple_dice`, the Spec agent receives the briefing context and begins extracting spec fields.

- [ ] **Step 4: Commit**

```bash
git add src/phase_dispatch.rs
git commit -m "feat: inject briefing context into Spec phase prompt"
```

---

### Task 6: End-to-end verification

- [ ] **Step 1: Test with realistic conversation transcript**

Create `~/test_briefing.md` with a multi-turn conversation transcript and pipe it:
```bash
cat ~/test_briefing.md | cargo run
```

Verify:
- Project and session created in `~/MiModel/`
- `briefing.md` exists in session directory
- `session.json` has `"briefing": "briefing.md"`
- Spec agent references the conversation content
- Normal interactive flow works after initial response

- [ ] **Step 2: Test edge cases**

```bash
# Empty pipe — should start normally
echo "" | cargo run

# No pipe — should start normally
cargo run

# Large file — should truncate
dd if=/dev/urandom bs=1024 count=200 | base64 | cargo run
```

- [ ] **Step 3: Test session reload**

Open an existing briefing session from the project tree. Verify the briefing context is still available on the first Spec prompt.

- [ ] **Step 4: Final commit**

If any fixes were needed, commit them.
