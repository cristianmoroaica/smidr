use crate::claude;
use crate::core::BackgroundResult;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

/// Which backend `send_phase_prompt` dispatches a turn to.
#[derive(Clone)]
pub enum EngineKind {
    /// The existing `claude` CLI subprocess path, unchanged.
    ClaudeCli,
    /// An OpenAI-compatible local endpoint, driven by the Rust agent loop in
    /// `src/openai_engine.rs`.
    OpenAiCompat {
        endpoint: crate::engine_config::EndpointConfig,
        model: String,
    },
}

/// Whether a background task is running.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BusyState {
    Idle,
    Thinking,
    Building,
}

/// An MCP tool call emitted by Claude during streaming.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub name: String,
    pub input: serde_json::Value,
}

/// A per-component build status line parsed from MCP tool output.
#[derive(Debug, Clone)]
pub struct BuildProgress {
    pub component: String,
    pub status: String,
}

/// A prompt dispatch recorded instead of executed (see [`Dispatch::Capture`]).
#[derive(Debug, Clone)]
#[allow(dead_code)] // read by tests only
pub struct CapturedPrompt {
    /// Phase name for `send_phase_prompt`; `None` for `send_raw_prompt`.
    pub phase_name: Option<String>,
    pub prompt: String,
    pub images: Vec<PathBuf>,
    pub ref_context: Option<String>,
    pub has_mcp: bool,
}

/// Where a dispatched prompt goes.
///
/// Production always uses `Subprocess`, which spawns the `claude` CLI on a
/// background thread. `Capture` records the dispatch instead, so tests can
/// drive the full prompt-submission path (including phase dispatch) without
/// launching a subprocess or making a network call. The variant is chosen at
/// runtime rather than by `cfg(test)` so both paths share one implementation.
pub enum Dispatch {
    Subprocess,
    #[allow(dead_code)] // constructed by tests only
    Capture(Vec<CapturedPrompt>),
}

/// Owns all Claude CLI interaction state: channels, PID tracking,
/// model selection, session continuity, and streaming text buffer.
pub struct ClaudeBridge {
    // Channels
    bg_tx: mpsc::Sender<BackgroundResult>,
    bg_rx: mpsc::Receiver<BackgroundResult>,
    stream_tx: mpsc::Sender<String>,
    stream_rx: mpsc::Receiver<String>,
    pub tool_tx: mpsc::Sender<ToolCall>,
    pub tool_rx: mpsc::Receiver<ToolCall>,
    progress_tx: mpsc::Sender<BuildProgress>,
    progress_rx: mpsc::Receiver<BuildProgress>,
    bg_pid: Arc<AtomicU32>,

    // State
    pub model: Option<String>,
    pub session_id: Option<String>,
    pub streaming_text: String,
    pub busy: BusyState,
    /// Dispatch backend for `send_phase_prompt` / `send_raw_prompt`.
    pub dispatch: Dispatch,
    /// Which engine `send_phase_prompt` dispatches to. Defaults to
    /// `ClaudeCli`; changed via `AppCore::set_engine`.
    pub engine: EngineKind,
    /// Cooperative cancellation flag for the OpenAI-compat engine loop,
    /// checked between SSE chunks and before each tool call. The Claude CLI
    /// path keeps using SIGTERM via `bg_pid`; `cancel()` sets both.
    cancel_flag: Arc<AtomicBool>,
}

impl ClaudeBridge {
    /// Create a new bridge with channels and initial state.
    pub fn new(model: Option<String>) -> Self {
        let (bg_tx, bg_rx) = mpsc::channel::<BackgroundResult>();
        let (stream_tx, stream_rx) = mpsc::channel::<String>();
        let (tool_tx, tool_rx) = mpsc::channel::<ToolCall>();
        let (progress_tx, progress_rx) = mpsc::channel::<BuildProgress>();
        let bg_pid = Arc::new(AtomicU32::new(0));

        ClaudeBridge {
            bg_tx,
            bg_rx,
            stream_tx,
            stream_rx,
            tool_tx,
            tool_rx,
            progress_tx,
            progress_rx,
            bg_pid,
            model,
            session_id: None,
            streaming_text: String::new(),
            busy: BusyState::Idle,
            dispatch: Dispatch::Subprocess,
            engine: EngineKind::ClaudeCli,
            cancel_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Drain stream_rx via try_recv loop, appending each chunk to
    /// streaming_text and returning the chunks in arrival order.
    pub fn drain_streaming(&mut self) -> Vec<String> {
        let mut chunks = Vec::new();
        while let Ok(chunk) = self.stream_rx.try_recv() {
            self.streaming_text.push_str(&chunk);
            chunks.push(chunk);
        }
        chunks
    }

    /// Drain tool_rx via try_recv loop, returning all pending tool calls.
    pub fn drain_tool_calls(&self) -> Vec<ToolCall> {
        let mut calls = Vec::new();
        while let Ok(tc) = self.tool_rx.try_recv() {
            calls.push(tc);
        }
        calls
    }

    /// Drain progress_rx via try_recv loop, returning all pending build progress updates.
    pub fn drain_build_progress(&self) -> Vec<BuildProgress> {
        let mut updates = Vec::new();
        while let Ok(bp) = self.progress_rx.try_recv() {
            updates.push(bp);
        }
        updates
    }

    /// Non-blocking check for a completed background result.
    pub fn try_recv_result(&self) -> Option<BackgroundResult> {
        self.bg_rx.try_recv().ok()
    }

    /// Send SIGTERM to the background Claude subprocess (if any), and set
    /// the cooperative cancellation flag the OpenAI-compat engine loop
    /// checks between SSE chunks and before each tool call.
    pub fn cancel(&self) {
        let pid = self.bg_pid.load(Ordering::SeqCst);
        if pid != 0 {
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
        self.cancel_flag.store(true, Ordering::SeqCst);
    }

    /// Spawn a background thread that calls `claude::send_with_phase_prompt`
    /// and sends the result via bg_tx.
    ///
    /// This replaces the duplicated thread-spawn pattern across all send methods.
    /// When `mcp_config` is provided, MCP tool flags are passed to the CLI and
    /// tool_use content blocks are forwarded via `tool_tx`.
    #[allow(clippy::too_many_arguments)]
    pub fn send_phase_prompt(
        &mut self,
        phase_name: &str,
        prompt: &str,
        images: &[PathBuf],
        ref_context: Option<&str>,
        history: &[(String, String)],
        session_dir: Option<PathBuf>,
        mcp_config: Option<PathBuf>,
    ) {
        self.busy = BusyState::Thinking;
        self.streaming_text.clear();

        let phase_name = phase_name.to_string();
        let prompt = prompt.to_string();
        let images = images.to_vec();
        let ref_context = ref_context.map(|s| s.to_string());
        let has_mcp = mcp_config.is_some();

        if let Dispatch::Capture(ref mut log) = self.dispatch {
            log.push(CapturedPrompt {
                phase_name: Some(phase_name.clone()),
                prompt: prompt.clone(),
                images: images.clone(),
                ref_context: ref_context.clone(),
                has_mcp,
            });
            return;
        }

        match &self.engine {
            EngineKind::ClaudeCli => {
                let model = self.model.clone();
                let session_id = self.session_id.clone();
                let tx = self.bg_tx.clone();
                let stream_tx = self.stream_tx.clone();
                let tool_tx = self.tool_tx.clone();
                let progress_tx = self.progress_tx.clone();
                let bg_pid = Arc::clone(&self.bg_pid);

                std::thread::spawn(move || {
                    let result = claude::send_with_phase_prompt(
                        &model,
                        &phase_name,
                        session_id.as_deref(),
                        &prompt,
                        &images,
                        Some(&stream_tx),
                        Some(&bg_pid),
                        ref_context.as_deref(),
                        if has_mcp { Some(&tool_tx) } else { None },
                        mcp_config.as_deref(),
                        has_mcp,
                        Some(&progress_tx),
                    );
                    bg_pid.store(0, Ordering::SeqCst);
                    match result {
                        Ok((response, new_sid)) => {
                            let _ = tx.send(BackgroundResult::ClaudeResponse {
                                result: Ok(response),
                                session_id: new_sid.or(session_id),
                            });
                        }
                        Err(e) => {
                            let _ = tx.send(BackgroundResult::ClaudeResponse {
                                result: Err(e),
                                session_id: None,
                            });
                        }
                    }
                });
            }
            EngineKind::OpenAiCompat { endpoint, model } => {
                if !images.is_empty() {
                    let _ = self.bg_tx.send(BackgroundResult::ClaudeResponse {
                        result: Err("images are not supported on local engines (v1)".to_string()),
                        session_id: None,
                    });
                    return;
                }

                self.cancel_flag.store(false, Ordering::SeqCst);

                let mut system_prompt = match crate::prompt_builder::load_phase_system_prompt(&phase_name) {
                    Ok(p) => p,
                    Err(e) => {
                        let _ = self.bg_tx.send(BackgroundResult::ClaudeResponse {
                            result: Err(e),
                            session_id: None,
                        });
                        return;
                    }
                };
                if matches!(
                    phase_name.as_str(),
                    "build" | "component" | "assembly" | "refinement" | "refine" | "lead"
                ) {
                    system_prompt.push_str(&crate::prompt_builder::load_engineering_knowledge());
                }
                if let Some(ctx) = &ref_context {
                    system_prompt.push_str("\n\n");
                    system_prompt.push_str(ctx);
                }

                let turn = crate::openai_engine::OpenAiTurn {
                    endpoint: endpoint.clone(),
                    model: model.clone(),
                    phase_name: phase_name.clone(),
                    system_prompt,
                    history: history.to_vec(),
                    prompt,
                    session_dir,
                    cancel: Arc::clone(&self.cancel_flag),
                };

                let tx = self.bg_tx.clone();
                let stream_tx = self.stream_tx.clone();
                let tool_tx = self.tool_tx.clone();
                let progress_tx = self.progress_tx.clone();

                std::thread::spawn(move || {
                    let result = crate::openai_engine::run_turn(turn, &stream_tx, &tool_tx, &progress_tx);
                    let _ = tx.send(BackgroundResult::ClaudeResponse {
                        result,
                        session_id: None,
                    });
                });
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::BackgroundResult;
    use crate::engine_config::{EndpointConfig, EndpointKind};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    fn fake_endpoint(base_url: &str) -> EndpointConfig {
        EndpointConfig {
            name: "fake".to_string(),
            kind: EndpointKind::OpenAi,
            base_url: base_url.to_string(),
            api_key: None,
        }
    }

    /// Poll `try_recv_result` until it yields something or `timeout` elapses.
    fn recv_result_within(bridge: &ClaudeBridge, timeout: Duration) -> Option<BackgroundResult> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(r) = bridge.try_recv_result() {
                return Some(r);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A bound-then-dropped listener frees the port with nobody listening,
    /// guaranteeing "connection refused" rather than a flaky "might still be
    /// in TIME_WAIT" port reuse — mirrors the pattern in tests/api_engines.rs.
    fn dead_base_url() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        format!("http://{addr}/v1")
    }

    #[test]
    fn capture_dispatch_ignores_engine_and_history_and_does_not_touch_bg_channel() {
        let mut bridge = ClaudeBridge::new(None);
        bridge.dispatch = Dispatch::Capture(Vec::new());
        bridge.engine = EngineKind::OpenAiCompat {
            endpoint: fake_endpoint("http://127.0.0.1:1/v1"),
            model: "test-model".to_string(),
        };
        let history = vec![("user".to_string(), "earlier turn".to_string())];

        bridge.send_phase_prompt("spec", "hello", &[], None, &history, None, None);

        match &bridge.dispatch {
            Dispatch::Capture(log) => {
                assert_eq!(log.len(), 1);
                assert_eq!(log[0].phase_name.as_deref(), Some("spec"));
                assert_eq!(log[0].prompt, "hello");
                assert!(log[0].images.is_empty());
                assert_eq!(log[0].ref_context, None);
                assert!(!log[0].has_mcp);
            }
            Dispatch::Subprocess => panic!("dispatch should still be Capture"),
        }

        // Capture returns before ever touching the OpenAI engine or spawning
        // a thread — nothing should show up on the background-result channel.
        assert!(recv_result_within(&bridge, Duration::from_millis(200)).is_none());
    }

    #[test]
    fn openai_compat_dispatch_with_dead_endpoint_sends_err_promptly() {
        let mut bridge = ClaudeBridge::new(None);
        bridge.engine = EngineKind::OpenAiCompat {
            endpoint: fake_endpoint(&dead_base_url()),
            model: "test-model".to_string(),
        };

        bridge.send_phase_prompt("spec", "hello", &[], None, &[], None, None);

        let result = recv_result_within(&bridge, Duration::from_secs(10))
            .expect("dead endpoint should produce a prompt Err, not hang");
        match result {
            BackgroundResult::ClaudeResponse { result, session_id } => {
                assert!(result.is_err(), "expected Err from a dead endpoint, got {result:?}");
                assert!(session_id.is_none());
            }
        }
    }

    #[test]
    fn openai_compat_dispatch_rejects_images() {
        let mut bridge = ClaudeBridge::new(None);
        bridge.engine = EngineKind::OpenAiCompat {
            endpoint: fake_endpoint("http://127.0.0.1:1/v1"),
            model: "test-model".to_string(),
        };

        bridge.send_phase_prompt(
            "spec",
            "hello",
            &[PathBuf::from("/tmp/does-not-matter.png")],
            None,
            &[],
            None,
            None,
        );

        let result = recv_result_within(&bridge, Duration::from_secs(2))
            .expect("images rejection should be synchronous, not require a thread");
        match result {
            BackgroundResult::ClaudeResponse { result, session_id } => {
                assert_eq!(result, Err("images are not supported on local engines (v1)".to_string()));
                assert!(session_id.is_none());
            }
        }
    }

    #[test]
    fn openai_compat_dispatch_sends_err_on_prompt_load_failure() {
        let mut bridge = ClaudeBridge::new(None);
        bridge.engine = EngineKind::OpenAiCompat {
            endpoint: fake_endpoint("http://127.0.0.1:1/v1"),
            model: "test-model".to_string(),
        };

        // No embedded prompt named "nonexistent_phase.md" exists, so
        // `prompt_builder::load_phase_system_prompt` errors before any
        // thread is spawned or network call attempted.
        bridge.send_phase_prompt("nonexistent_phase", "hello", &[], None, &[], None, None);

        let result = recv_result_within(&bridge, Duration::from_secs(2))
            .expect("a prompt-load failure should send an Err synchronously");
        match result {
            BackgroundResult::ClaudeResponse { result, session_id } => {
                assert!(result.is_err(), "expected Err from a missing phase prompt, got {result:?}");
                assert!(session_id.is_none());
            }
        }
    }

    #[test]
    fn cancel_sets_the_cooperative_cancel_flag() {
        let bridge = ClaudeBridge::new(None);
        assert!(!bridge.cancel_flag.load(Ordering::SeqCst));
        bridge.cancel();
        assert!(bridge.cancel_flag.load(Ordering::SeqCst));
    }

    #[test]
    fn openai_compat_dispatch_resets_cancel_flag_at_the_start_of_a_new_turn() {
        let mut bridge = ClaudeBridge::new(None);
        bridge.cancel_flag.store(true, Ordering::SeqCst);
        bridge.engine = EngineKind::OpenAiCompat {
            endpoint: fake_endpoint(&dead_base_url()),
            model: "test-model".to_string(),
        };

        bridge.send_phase_prompt("spec", "hello", &[], None, &[], None, None);

        assert!(!bridge.cancel_flag.load(Ordering::SeqCst));
        // Drain the background result so the spawned thread doesn't outlive
        // useful assertions/log noise in the test run.
        let _ = recv_result_within(&bridge, Duration::from_secs(10));
    }
}

/// Generate an MCP config JSON file for the given phase and return its path.
/// The config points the Claude CLI at our MCP server with appropriate args.
pub fn generate_mcp_config(phase_name: &str, session_dir: Option<&Path>) -> Result<PathBuf, String> {
    let server_path = find_mcp_server()?;
    let python_cmd = find_cadquery_python(&server_path);
    // Pin the interpreter to the native arch so an inherited Rosetta
    // exec-affinity can't flip a universal python to x86_64.
    let (command, mut args) = crate::python::native_arch_command(&python_cmd);
    args.extend([
        server_path.to_string_lossy().to_string(),
        "--phase".to_string(),
        phase_name.to_string(),
    ]);
    if let Some(dir) = session_dir {
        args.push("--session-dir".to_string());
        args.push(dir.to_string_lossy().to_string());
    }
    let config = serde_json::json!({
        "mcpServers": {
            "smidr": {
                "command": command,
                "args": args
            }
        }
    });
    let tmp_path = std::env::temp_dir().join(format!("smidr_mcp_{}.json", std::process::id()));
    std::fs::write(&tmp_path, config.to_string())
        .map_err(|e| format!("Failed to write MCP config: {e}"))?;
    Ok(tmp_path)
}

/// Locate the MCP server script (mcp/server.py).
/// Searches cwd, binary dir, and walks up from cwd.
pub(crate) fn find_mcp_server() -> Result<PathBuf, String> {
    if let Some(configured) = std::env::var_os("SMIDR_MCP_SERVER") {
        let configured = PathBuf::from(configured);
        if configured.is_file() {
            return Ok(configured);
        }
        return Err(format!(
            "SMIDR_MCP_SERVER does not point to a file: {}",
            configured.display()
        ));
    }
    let candidates = [
        std::env::current_dir().ok().map(|d| d.join("mcp/server.py")),
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("mcp/server.py"))),
    ];
    for c in candidates.into_iter().flatten() {
        if c.exists() { return Ok(c); }
    }
    // Walk up from cwd
    if let Ok(mut dir) = std::env::current_dir() {
        loop {
            let candidate = dir.join("mcp/server.py");
            if candidate.exists() { return Ok(candidate); }
            if !dir.pop() { break; }
        }
    }
    // Installed-binary fallback: the repo this binary was compiled from.
    // The Python side (mcp/server.py + .venv-cadquery) can't be embedded,
    // so an installed smidr keeps using the checkout it was built at.
    let built_from = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("mcp/server.py");
    if built_from.exists() {
        return Ok(built_from);
    }
    Err("mcp/server.py not found".to_string())
}

/// Find the Python interpreter that has CadQuery+OCP installed.
/// Looks for .venv-cadquery/bin/python3 relative to the project root
/// (same directory tree as mcp/server.py). Falls back to "python3".
pub(crate) fn find_cadquery_python(server_path: &Path) -> String {
    if let Ok(configured) = std::env::var("SMIDR_PYTHON") {
        if !configured.trim().is_empty() {
            return configured;
        }
    }
    // server_path is like /path/to/project/mcp/server.py
    // project root is the parent of mcp/
    if let Some(project_root) = server_path.parent().and_then(|p| p.parent()) {
        let venv_python = project_root.join(".venv-cadquery/bin/python3");
        if venv_python.exists() {
            return venv_python.to_string_lossy().to_string();
        }
        // Also check .venv/bin/python3
        let venv_python = project_root.join(".venv/bin/python3");
        if venv_python.exists() {
            return venv_python.to_string_lossy().to_string();
        }
    }
    "python3".to_string()
}
