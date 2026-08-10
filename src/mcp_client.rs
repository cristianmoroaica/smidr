//! Minimal synchronous JSON-RPC 2.0 client over newline-delimited stdio,
//! speaking to `mcp/server.py` (or any other MCP server) the same way the
//! `claude` CLI does today. Used by the OpenAI-compat engine's agent loop
//! (`openai_engine::run_turn`), which starts one client lazily on the first
//! tool call of a turn.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use wait_timeout::ChildExt;

/// One MCP tool definition, as returned by `tools/list`.
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// A synchronous MCP stdio client. One instance owns one child process.
pub struct McpClient {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: mpsc::Receiver<serde_json::Value>,
    next_id: u64,
    /// Per-request timeout. Defaults to 300s (builds legitimately take
    /// 60s+); tests shorten it to exercise the timeout path quickly.
    pub timeout: Duration,
}

impl McpClient {
    /// Locate and spawn our own `mcp/server.py` for `phase` (and optional
    /// `session_dir`), then perform the MCP `initialize` handshake.
    pub fn start(phase: &str, session_dir: Option<&Path>) -> Result<Self, String> {
        let server_path = crate::claude_bridge::find_mcp_server()?;
        let python = crate::claude_bridge::find_cadquery_python(&server_path);
        // Pin the interpreter to the native arch so an inherited Rosetta
        // exec-affinity can't flip a universal python to x86_64.
        let (program, mut args) = crate::python::native_arch_command(&python);

        args.extend([
            server_path.to_string_lossy().to_string(),
            "--phase".to_string(),
            phase.to_string(),
        ]);
        if let Some(dir) = session_dir {
            args.push("--session-dir".to_string());
            args.push(dir.to_string_lossy().to_string());
        }

        Self::start_with_command(&program, &args)
    }

    /// Spawn `program args...` with piped stdin/stdout and inherited stderr,
    /// then perform the MCP handshake. Split out from `start` so tests can
    /// drive a fake server script without touching real mcp/server.py
    /// discovery.
    pub(crate) fn start_with_command(program: &str, args: &[String]) -> Result<Self, String> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("failed to spawn MCP server {program}: {e}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "MCP server has no stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "MCP server has no stdout".to_string())?;

        let (tx, rx) = mpsc::channel::<serde_json::Value>();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                    if tx.send(v).is_err() {
                        break;
                    }
                }
                // Non-JSON lines are ignored (never fatal).
            }
        });

        let mut client = McpClient {
            child,
            stdin: Some(stdin),
            rx,
            next_id: 1,
            timeout: Duration::from_secs(300),
        };

        client.request(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "smidr", "version": env!("CARGO_PKG_VERSION")},
            }),
        )?;
        client.notify("notifications/initialized", serde_json::json!({}))?;

        Ok(client)
    }

    /// `tools/list` -> the server's tool definitions.
    pub fn list_tools(&mut self) -> Result<Vec<ToolDef>, String> {
        let result = self.request("tools/list", serde_json::json!({}))?;
        let tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(tools
            .into_iter()
            .map(|t| ToolDef {
                name: t
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                description: t
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                input_schema: t.get("inputSchema").cloned().unwrap_or_else(|| {
                    serde_json::json!({"type": "object", "properties": {}})
                }),
            })
            .collect())
    }

    /// `tools/call` -> joined text content blocks. If the server reports
    /// `isError`, the joined text is still returned, but as `Err`.
    pub fn call_tool(&mut self, name: &str, args: &serde_json::Value) -> Result<String, String> {
        let result = self.request(
            "tools/call",
            serde_json::json!({"name": name, "arguments": args}),
        )?;
        let content = result
            .get("content")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();
        let text = content
            .iter()
            .filter(|block| block.get("type").and_then(|v| v.as_str()) == Some("text"))
            .filter_map(|block| block.get("text").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
        let is_error = result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_error {
            Err(text)
        } else {
            Ok(text)
        }
    }

    /// Close stdin (EOF), give the child a brief grace period, then kill and
    /// reap. Never panics, never hangs. (`Drop` does the actual work — this
    /// just makes the intent explicit at call sites and consumes `self`.)
    pub fn shutdown(self) {}

    /// Write one JSON-RPC request line, flush, and wait for the matching
    /// response id (discarding any non-matching messages, e.g.
    /// server-initiated notifications) up to `self.timeout`.
    fn request(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_line(&msg)?;

        let deadline = std::time::Instant::now() + self.timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                self.kill();
                return Err(format!(
                    "MCP {method} timed out after {}s",
                    self.timeout.as_secs()
                ));
            }
            let msg = match self.rx.recv_timeout(remaining) {
                Ok(m) => m,
                Err(_) => {
                    self.kill();
                    return Err(format!(
                        "MCP {method} timed out after {}s",
                        self.timeout.as_secs()
                    ));
                }
            };
            if msg.get("id").and_then(|v| v.as_u64()) != Some(id) {
                continue;
            }
            if let Some(err) = msg.get("error") {
                let message = err
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                return Err(format!("MCP {method} error: {message}"));
            }
            return Ok(msg.get("result").cloned().unwrap_or(serde_json::Value::Null));
        }
    }

    /// Write a JSON-RPC notification (no id, no response awaited).
    fn notify(&mut self, method: &str, params: serde_json::Value) -> Result<(), String> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_line(&msg)
    }

    fn write_line(&mut self, msg: &serde_json::Value) -> Result<(), String> {
        let mut line = serde_json::to_string(msg).map_err(|e| e.to_string())?;
        line.push('\n');
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| "MCP server stdin already closed".to_string())?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.flush())
            .map_err(|e| format!("failed to write to MCP server stdin: {e}"))
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // Drop stdin first to signal EOF, then give the child a brief grace
        // period to exit on its own before force-killing. Never panics,
        // never hangs.
        self.stdin.take();
        match self
            .child
            .wait_timeout(Duration::from_secs(2))
            .unwrap_or(None)
        {
            Some(_status) => {}
            None => {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const FAKE_SERVER_PY: &str = r#"
import sys
import json
import time

def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except Exception:
        continue
    method = msg.get("method")
    msg_id = msg.get("id")

    if method == "initialize":
        send({"jsonrpc": "2.0", "id": msg_id, "result": {"protocolVersion": "2024-11-05"}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        send({
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": {
                "tools": [
                    {
                        "name": "echo",
                        "description": "Echoes arguments back",
                        "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
                    },
                    {"name": "slow", "description": "Sleeps"},
                ]
            },
        })
    elif method == "tools/call":
        params = msg.get("params", {})
        name = params.get("name")
        args = params.get("arguments", {})
        if name == "slow":
            time.sleep(5)
            send({"jsonrpc": "2.0", "id": msg_id, "result": {"content": [{"type": "text", "text": "done"}]}})
        else:
            text = json.dumps(args)
            send({"jsonrpc": "2.0", "id": msg_id, "result": {"content": [{"type": "text", "text": text}]}})
    else:
        send({"jsonrpc": "2.0", "id": msg_id, "error": {"code": -32601, "message": f"unknown method {method}"}})
"#;

    fn python3_available() -> bool {
        std::process::Command::new("which")
            .arg("python3")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn write_fake_server() -> Option<tempfile::TempDir> {
        let dir = tempfile::TempDir::new().ok()?;
        let path = dir.path().join("fake_mcp_server.py");
        let mut f = std::fs::File::create(&path).ok()?;
        f.write_all(FAKE_SERVER_PY.as_bytes()).ok()?;
        let mut perms = std::fs::metadata(&path).ok()?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).ok()?;
        Some(dir)
    }

    #[test]
    fn handshake_and_list_tools() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = write_fake_server().expect("tempdir");
        let script = dir.path().join("fake_mcp_server.py").to_string_lossy().to_string();
        let mut client = McpClient::start_with_command("python3", &[script]).expect("start");
        let tools = client.list_tools().expect("list_tools");
        let echo = tools.iter().find(|t| t.name == "echo").expect("echo tool");
        assert_eq!(echo.description, "Echoes arguments back");
        assert_eq!(
            echo.input_schema.get("type").and_then(|v| v.as_str()),
            Some("object")
        );
    }

    #[test]
    fn call_tool_round_trips_arguments() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = write_fake_server().expect("tempdir");
        let script = dir.path().join("fake_mcp_server.py").to_string_lossy().to_string();
        let mut client = McpClient::start_with_command("python3", &[script]).expect("start");
        let result = client
            .call_tool("echo", &serde_json::json!({"text": "hello"}))
            .expect("call_tool");
        let parsed: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(parsed.get("text").and_then(|v| v.as_str()), Some("hello"));
    }

    #[test]
    fn call_tool_timeout_kills_child() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = write_fake_server().expect("tempdir");
        let script = dir.path().join("fake_mcp_server.py").to_string_lossy().to_string();
        let mut client = McpClient::start_with_command("python3", &[script]).expect("start");
        client.timeout = Duration::from_millis(200);
        let err = client
            .call_tool("slow", &serde_json::json!({}))
            .expect_err("should time out");
        assert!(err.contains("timed out"), "unexpected error: {err}");
        // The child should be gone (killed by the timeout path).
        let status = client.child.try_wait().expect("try_wait");
        assert!(status.is_some(), "child should have been killed");
    }

    #[test]
    fn drop_of_live_client_does_not_hang() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = write_fake_server().expect("tempdir");
        let script = dir.path().join("fake_mcp_server.py").to_string_lossy().to_string();
        let client = McpClient::start_with_command("python3", &[script]).expect("start");
        drop(client);
        // If we got here without the test hanging, Drop worked.
    }
}
