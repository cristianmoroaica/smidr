//! OpenAI-compatible streaming agent loop with MCP tool execution.
//!
//! Runs on the worker thread `send_phase_prompt` spawns for
//! `EngineKind::OpenAiCompat`. Mirrors the Claude CLI path's channel
//! contract (`stream_tx`/`tool_tx`/`progress_tx`) but drives the model over
//! HTTP `/chat/completions` (SSE) and executes tools itself via
//! [`crate::mcp_client::McpClient`] instead of delegating to a `claude`
//! subprocess.
//!
//! Entry point: [`run_turn`], called from `ClaudeBridge::send_phase_prompt`'s
//! `EngineKind::OpenAiCompat` branch on a dedicated worker thread. The final
//! assistant text it returns is wrapped into `BackgroundResult::ClaudeResponse`
//! by the bridge (always with `session_id: None` — session.json history IS
//! this engine's memory).

use crate::claude_bridge::{BuildProgress, ToolCall};
use crate::engine_config::EndpointConfig;
use crate::mcp_client::{McpClient, ToolDef};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

/// One turn (one user prompt) to run against an OpenAI-compatible endpoint.
pub struct OpenAiTurn {
    pub endpoint: EndpointConfig,
    pub model: String,
    pub phase_name: String,
    pub system_prompt: String,
    /// (role, content) as persisted in session.json.
    pub history: Vec<(String, String)>,
    pub prompt: String,
    pub session_dir: Option<std::path::PathBuf>,
    pub cancel: Arc<AtomicBool>,
}

/// Run one full turn: build the message list, lazily start an MCP client,
/// loop POSTing `/chat/completions` and executing any requested tool calls,
/// and return the final assistant text.
pub fn run_turn(
    turn: OpenAiTurn,
    stream_tx: &Sender<String>,
    tool_tx: &Sender<ToolCall>,
    progress_tx: &Sender<BuildProgress>,
) -> Result<String, String> {
    let client = match McpClient::start(&turn.phase_name, turn.session_dir.as_deref()) {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("Warning: MCP client failed to start, continuing without tools: {e}");
            None
        }
    };
    run_turn_with_client(turn, stream_tx, tool_tx, progress_tx, client)
}

const MAX_ROUNDS: u32 = 32;
const MAX_CONSECUTIVE_MALFORMED: u32 = 3;

/// Same as [`run_turn`] but takes an already-constructed (or absent) MCP
/// client, so tests can inject a client wired to a fake stdio MCP server
/// instead of going through real `mcp/server.py` discovery.
pub(crate) fn run_turn_with_client(
    turn: OpenAiTurn,
    stream_tx: &Sender<String>,
    tool_tx: &Sender<ToolCall>,
    progress_tx: &Sender<BuildProgress>,
    mut client: Option<McpClient>,
) -> Result<String, String> {
    let tool_defs: Vec<ToolDef> = match &mut client {
        Some(c) => c.list_tools().unwrap_or_else(|e| {
            eprintln!("Warning: MCP list_tools failed, continuing without tools: {e}");
            vec![]
        }),
        None => vec![],
    };
    let tools_json = tool_defs_to_openai(&tool_defs);

    let mut messages = build_messages(&turn.system_prompt, &turn.history, &turn.prompt);
    let mut accumulated_text = String::new();
    let mut malformed_streak: u32 = 0;

    // http_status_as_error(false) so a non-2xx response comes back as an
    // Ok(Response) whose body we can still read, instead of being turned
    // into an `ureq::Error::StatusCode` that discards the body (which is
    // where OpenAI-compatible servers put the actionable error message).
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();

    let result = (|| -> Result<String, String> {
        for _round in 0..MAX_ROUNDS {
            if turn.cancel.load(Ordering::Relaxed) {
                return Err("cancelled".to_string());
            }

            let mut body = serde_json::json!({
                "model": turn.model,
                "messages": messages,
                "stream": true,
            });
            if !tool_defs.is_empty() {
                body["tools"] = tools_json.clone();
            }

            let url = format!("{}/chat/completions", turn.endpoint.base_url);
            let mut req = agent.post(&url).header("content-type", "application/json");
            if let Some(key) = &turn.endpoint.api_key {
                req = req.header("authorization", format!("Bearer {key}"));
            }
            let mut resp = req
                .send_json(&body)
                .map_err(|e| endpoint_err(&turn.endpoint, &e))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body_text = resp
                    .body_mut()
                    .with_config()
                    .limit(4096)
                    .read_to_string()
                    .unwrap_or_default();
                return Err(format!(
                    "{} ({}): http status {}: {}",
                    turn.endpoint.name,
                    turn.endpoint.base_url,
                    status.as_u16(),
                    body_text.trim()
                ));
            }

            let mut assembler = SseAssembler::default();
            let reader = BufReader::new(resp.body_mut().as_reader());
            for line in reader.lines() {
                if turn.cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".to_string());
                }
                let line = line.map_err(|e| {
                    format!(
                        "{} ({}): {}",
                        turn.endpoint.name, turn.endpoint.base_url, e
                    )
                })?;
                let line = line.trim_end();
                if line.is_empty()
                    || line.starts_with("event:")
                    || line.starts_with("id:")
                    || line.starts_with(':')
                {
                    continue;
                }
                let Some(rest) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = rest.strip_prefix(' ').unwrap_or(rest);
                for delta in assembler.push(data) {
                    let _ = stream_tx.send(delta);
                }
            }

            accumulated_text.push_str(assembler.text());
            let pending_calls = assembler.tool_calls();
            let is_tool_round = assembler.finish_reason() == Some("tool_calls")
                || (assembler.finish_reason().is_none() && !pending_calls.is_empty());

            if !is_tool_round {
                return Ok(accumulated_text);
            }

            let mut tool_messages: Vec<(String, String)> = Vec::new();
            for call in &pending_calls {
                if turn.cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".to_string());
                }

                let parsed: Result<serde_json::Value, _> = serde_json::from_str(&call.arguments);
                let args = match parsed {
                    Ok(v) => {
                        malformed_streak = 0;
                        v
                    }
                    Err(_) => {
                        malformed_streak += 1;
                        // Per plan Task 3: "each malformed one sends an error
                        // tool result back first" — push the tool message
                        // before checking whether the streak aborts the turn.
                        tool_messages.push((
                            call.id.clone(),
                            format!(
                                "error: arguments for tool \"{}\" were not valid JSON",
                                call.name
                            ),
                        ));
                        if malformed_streak >= MAX_CONSECUTIVE_MALFORMED {
                            return Err(
                                "engine sent 3 consecutive malformed tool-call arguments; aborting turn"
                                    .to_string(),
                            );
                        }
                        continue;
                    }
                };

                let _ = tool_tx.send(ToolCall {
                    name: call.name.clone(),
                    input: args.clone(),
                });

                let result_text = match &mut client {
                    Some(c) => match c.call_tool(&call.name, &args) {
                        Ok(t) => t,
                        Err(e) => e,
                    },
                    None => "error: no MCP client available".to_string(),
                };

                for line in result_text.lines() {
                    if let Some((component, status)) = crate::claude::parse_build_progress_line(line) {
                        let _ = progress_tx.send(BuildProgress { component, status });
                    }
                }

                tool_messages.push((call.id.clone(), result_text));
            }

            let assistant_content = if assembler.text().is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(assembler.text().to_string())
            };
            let tool_calls_json: Vec<serde_json::Value> = pending_calls
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "id": c.id,
                        "type": "function",
                        "function": {
                            "name": c.name,
                            "arguments": c.arguments,
                        },
                    })
                })
                .collect();
            messages.push(serde_json::json!({
                "role": "assistant",
                "content": assistant_content,
                "tool_calls": tool_calls_json,
            }));
            for (id, content) in tool_messages {
                messages.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": id,
                    "content": content,
                }));
            }
        }

        Err("engine exceeded 32 tool-call rounds; aborting turn".to_string())
    })();

    if let Some(c) = client.take() {
        c.shutdown();
    }
    result
}

fn endpoint_err(endpoint: &EndpointConfig, e: &ureq::Error) -> String {
    format!("{} ({}): {}", endpoint.name, endpoint.base_url, e)
}

/// Build the OpenAI `messages` array: system prompt, then persisted phase
/// history (role `question` maps to `assistant`; any other unrecognized
/// role is dropped), then the new user prompt.
pub(crate) fn build_messages(
    system: &str,
    history: &[(String, String)],
    prompt: &str,
) -> Vec<serde_json::Value> {
    let mut messages = vec![serde_json::json!({"role": "system", "content": system})];
    for (role, content) in history {
        let mapped = match role.as_str() {
            "user" => "user",
            "assistant" => "assistant",
            "question" => "assistant",
            _ => continue,
        };
        messages.push(serde_json::json!({"role": mapped, "content": content}));
    }
    messages.push(serde_json::json!({"role": "user", "content": prompt}));
    messages
}

/// Translate MCP tool defs into OpenAI `tools` function-calling schemas.
/// Plain tool names — no `mcp__smidr__` prefix (that's a Claude-CLI artifact).
pub(crate) fn tool_defs_to_openai(defs: &[ToolDef]) -> serde_json::Value {
    serde_json::Value::Array(
        defs.iter()
            .map(|d| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": d.name,
                        "description": d.description,
                        "parameters": d.input_schema,
                    },
                })
            })
            .collect(),
    )
}

/// A tool call assembled from (possibly fragmented) SSE `delta.tool_calls`
/// entries, ready to execute.
#[derive(Debug, Clone, Default)]
pub(crate) struct PendingToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Default)]
struct AccumulatingToolCall {
    index: u64,
    id: String,
    name: String,
    arguments: String,
}

/// Incrementally assembles one SSE round's content text, tool calls, and
/// finish reason from `data: ` payload lines.
#[derive(Default)]
pub(crate) struct SseAssembler {
    text: String,
    finish_reason: Option<String>,
    tool_calls: Vec<AccumulatingToolCall>,
}

impl SseAssembler {
    /// Feed one SSE payload line (the text after `data: `). Returns any
    /// content deltas to forward to `stream_tx` (usually 0 or 1).
    pub(crate) fn push(&mut self, data: &str) -> Vec<String> {
        if data.is_empty() || data == "[DONE]" {
            return vec![];
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
            return vec![];
        };
        let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
            return vec![];
        };

        if let Some(fr) = choice.get("finish_reason").and_then(|f| f.as_str()) {
            self.finish_reason = Some(fr.to_string());
        }

        let mut deltas = Vec::new();
        let Some(delta) = choice.get("delta") else {
            return deltas;
        };

        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
            self.text.push_str(content);
            deltas.push(content.to_string());
        }
        // delta.reasoning / delta.reasoning_content: intentionally dropped.

        if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tcs {
                let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                let pos = self.tool_calls.iter().position(|c| c.index == index);
                let pos = pos.unwrap_or_else(|| {
                    self.tool_calls.push(AccumulatingToolCall {
                        index,
                        ..Default::default()
                    });
                    self.tool_calls.len() - 1
                });
                let entry = &mut self.tool_calls[pos];
                if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                    entry.id.push_str(id);
                }
                if let Some(func) = tc.get("function") {
                    if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                        entry.name.push_str(name);
                    }
                    if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                        entry.arguments.push_str(args);
                    }
                }
            }
        }

        deltas
    }

    pub(crate) fn finish_reason(&self) -> Option<&str> {
        self.finish_reason.as_deref()
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn tool_calls(&self) -> Vec<PendingToolCall> {
        let mut sorted = self.tool_calls.clone();
        sorted.sort_by_key(|c| c.index);
        sorted
            .into_iter()
            .map(|c| PendingToolCall {
                id: c.id,
                name: c.name,
                arguments: c.arguments,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_config::{EndpointConfig, EndpointKind};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::sync::Mutex;

    // ---- build_messages ----

    #[test]
    fn build_messages_orders_system_first_and_prompt_last() {
        let history = vec![
            ("user".to_string(), "hi".to_string()),
            ("assistant".to_string(), "hello".to_string()),
        ];
        let msgs = build_messages("SYS", &history, "PROMPT");
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "SYS");
        assert_eq!(msgs.last().unwrap()["role"], "user");
        assert_eq!(msgs.last().unwrap()["content"], "PROMPT");
    }

    #[test]
    fn build_messages_maps_question_to_assistant_and_drops_unknown_roles() {
        let history = vec![
            ("question".to_string(), "which color?".to_string()),
            ("system".to_string(), "chatter".to_string()),
            ("user".to_string(), "red".to_string()),
        ];
        let msgs = build_messages("SYS", &history, "next");
        // system + question(->assistant) + user + final user prompt = 4
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"], "which color?");
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"], "red");
    }

    // ---- tool_defs_to_openai ----

    #[test]
    fn tool_defs_to_openai_shape_and_plain_names() {
        let defs = vec![ToolDef {
            name: "echo".to_string(),
            description: "Echoes".to_string(),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        }];
        let v = tool_defs_to_openai(&defs);
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "function");
        assert_eq!(arr[0]["function"]["name"], "echo");
        assert_eq!(arr[0]["function"]["description"], "Echoes");
        assert_eq!(
            arr[0]["function"]["parameters"],
            serde_json::json!({"type": "object", "properties": {}})
        );
    }

    // ---- SseAssembler ----

    #[test]
    fn sse_assembler_accumulates_content() {
        let mut a = SseAssembler::default();
        let d1 = a.push(r#"{"choices":[{"delta":{"content":"Hel"}}]}"#);
        let d2 = a.push(r#"{"choices":[{"delta":{"content":"lo"}}]}"#);
        assert_eq!(d1, vec!["Hel".to_string()]);
        assert_eq!(d2, vec!["lo".to_string()]);
        assert_eq!(a.text(), "Hello");
    }

    #[test]
    fn sse_assembler_drops_reasoning() {
        let mut a = SseAssembler::default();
        let d = a.push(r#"{"choices":[{"delta":{"reasoning":"thinking..."}}]}"#);
        assert!(d.is_empty());
        let d = a.push(r#"{"choices":[{"delta":{"reasoning_content":"more thinking"}}]}"#);
        assert!(d.is_empty());
        assert_eq!(a.text(), "");
    }

    #[test]
    fn sse_assembler_reassembles_fragmented_tool_call() {
        let mut a = SseAssembler::default();
        a.push(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"ec","arguments":""}}]}}]}"#);
        a.push(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"ho"}}]}}]}"#);
        a.push(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"a\""}}]}}]}"#);
        a.push(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":1}"}}]}}]}"#);
        let calls = a.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "echo");
        assert_eq!(calls[0].arguments, r#"{"a":1}"#);
    }

    #[test]
    fn sse_assembler_keeps_parallel_tool_calls_separate() {
        let mut a = SseAssembler::default();
        a.push(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c0","function":{"name":"foo","arguments":"{}"}}]}}]}"#);
        a.push(r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"c1","function":{"name":"bar","arguments":"{}"}}]}}]}"#);
        let calls = a.tool_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "foo");
        assert_eq!(calls[1].name, "bar");
    }

    #[test]
    fn sse_assembler_tolerates_done_and_junk() {
        let mut a = SseAssembler::default();
        assert!(a.push("[DONE]").is_empty());
        assert!(a.push("").is_empty());
        assert!(a.push("not json at all").is_empty());
        assert_eq!(a.text(), "");
    }

    #[test]
    fn sse_assembler_exposes_finish_reason() {
        let mut a = SseAssembler::default();
        assert_eq!(a.finish_reason(), None);
        a.push(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#);
        assert_eq!(a.finish_reason(), Some("stop"));
    }

    // ---- run_turn_with_client against a scripted fake HTTP server ----

    /// Spawn a fake OpenAI-compatible SSE endpoint on 127.0.0.1:0. `bodies`
    /// yields one SSE response body (already framed as `data: ...\n\n`
    /// lines terminated by `data: [DONE]\n\n`) per accepted connection, in
    /// order. Returns the bound address plus a shared log of each request's
    /// JSON body (in arrival order) so tests can assert on what was POSTed.
    fn spawn_fake_endpoint(bodies: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
        spawn_fake_endpoint_with_status(bodies.into_iter().map(|b| (200, b)).collect())
    }

    /// Same as [`spawn_fake_endpoint`] but each response carries its own
    /// HTTP status code, so tests can script non-2xx responses.
    fn spawn_fake_endpoint_with_status(
        responses: Vec<(u16, String)>,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let requests_writer = Arc::clone(&requests);
        std::thread::spawn(move || {
            for (status, body) in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                // Drain the request (headers + body) up to a blank line,
                // then keep reading until we've consumed Content-Length
                // bytes so the client's write doesn't block on us.
                let mut buf = [0u8; 4096];
                let mut acc: Vec<u8> = Vec::new();
                let mut content_length: usize = 0;
                let mut request_body = String::new();
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    acc.extend_from_slice(&buf[..n]);
                    if let Some(pos) = find_double_crlf(&acc) {
                        let head = String::from_utf8_lossy(&acc[..pos]);
                        for line in head.lines() {
                            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                                content_length = v.trim().parse().unwrap_or(0);
                            }
                        }
                        let already = acc.len() - (pos + 4);
                        request_body.push_str(&String::from_utf8_lossy(&acc[pos + 4..]));
                        let mut remaining = content_length.saturating_sub(already);
                        while remaining > 0 {
                            let n = stream.read(&mut buf).unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            request_body.push_str(&String::from_utf8_lossy(&buf[..n]));
                            remaining = remaining.saturating_sub(n);
                        }
                        break;
                    }
                }
                requests_writer.lock().unwrap().push(request_body);
                let status_line = match status {
                    200 => "200 OK",
                    404 => "404 Not Found",
                    500 => "500 Internal Server Error",
                    _ => "400 Bad Request",
                };
                let content_type = if status == 200 {
                    "text/event-stream"
                } else {
                    "application/json"
                };
                let resp = format!(
                    "HTTP/1.1 {status_line}\r\ncontent-type: {content_type}\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len(),
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}/v1"), requests)
    }

    fn find_double_crlf(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n")
    }

    fn sse_body(events: &[&str]) -> String {
        let mut s = String::new();
        for e in events {
            s.push_str("data: ");
            s.push_str(e);
            s.push_str("\n\n");
        }
        s.push_str("data: [DONE]\n\n");
        s
    }

    fn test_endpoint(base_url: String) -> EndpointConfig {
        EndpointConfig {
            name: "test-endpoint".to_string(),
            kind: EndpointKind::OpenAi,
            base_url,
            api_key: None,
        }
    }

    fn test_turn(endpoint: EndpointConfig) -> OpenAiTurn {
        OpenAiTurn {
            endpoint,
            model: "test-model".to_string(),
            phase_name: "spec".to_string(),
            system_prompt: "system prompt".to_string(),
            history: vec![],
            prompt: "hello".to_string(),
            session_dir: None,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn single_round_text_only_returns_ok_and_streams_deltas() {
        let body = sse_body(&[
            r#"{"choices":[{"delta":{"content":"Hi"}}]}"#,
            r#"{"choices":[{"delta":{"content":" there"},"finish_reason":"stop"}]}"#,
        ]);
        let (base_url, _requests) = spawn_fake_endpoint(vec![body]);
        let turn = test_turn(test_endpoint(base_url));
        let (stream_tx, stream_rx) = mpsc::channel();
        let (tool_tx, _tool_rx) = mpsc::channel();
        let (progress_tx, _progress_rx) = mpsc::channel();

        let result = run_turn_with_client(turn, &stream_tx, &tool_tx, &progress_tx, None);
        assert_eq!(result, Ok("Hi there".to_string()));
        let deltas: Vec<String> = stream_rx.try_iter().collect();
        assert_eq!(deltas, vec!["Hi".to_string(), " there".to_string()]);
    }

    #[test]
    fn cancel_before_call_returns_cancelled() {
        let (base_url, _requests) = spawn_fake_endpoint(vec![]);
        let turn = test_turn(test_endpoint(base_url));
        turn.cancel.store(true, Ordering::Relaxed);
        let (stream_tx, _) = mpsc::channel();
        let (tool_tx, _) = mpsc::channel();
        let (progress_tx, _) = mpsc::channel();
        let result = run_turn_with_client(turn, &stream_tx, &tool_tx, &progress_tx, None);
        assert_eq!(result, Err("cancelled".to_string()));
    }

    #[test]
    fn refused_connection_error_contains_endpoint_name_and_base_url() {
        // Bind then immediately drop, freeing the port without anyone
        // listening on it -> connection refused.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        let base_url = format!("http://{addr}/v1");
        let turn = test_turn(test_endpoint(base_url.clone()));
        let (stream_tx, _) = mpsc::channel();
        let (tool_tx, _) = mpsc::channel();
        let (progress_tx, _) = mpsc::channel();
        let err = run_turn_with_client(turn, &stream_tx, &tool_tx, &progress_tx, None)
            .expect_err("should fail to connect");
        assert!(err.contains("test-endpoint"), "error was: {err}");
        assert!(err.contains(&base_url), "error was: {err}");
    }

    // ---- MCP-backed loop tests (skipped if python3 unavailable) ----

    const FAKE_MCP_SERVER_PY: &str = r#"
import sys
import json

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
                    }
                ]
            },
        })
    elif method == "tools/call":
        params = msg.get("params", {})
        name = params.get("name")
        args = params.get("arguments", {})
        if name == "echo":
            send({
                "jsonrpc": "2.0",
                "id": msg_id,
                "result": {"content": [{"type": "text", "text": "BUILD_COMPONENT: widget done\nechoed: " + json.dumps(args)}]},
            })
        else:
            send({"jsonrpc": "2.0", "id": msg_id, "error": {"code": -32601, "message": "unknown tool"}})
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

    fn write_fake_mcp_server() -> Option<tempfile::TempDir> {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().ok()?;
        let path = dir.path().join("fake_mcp_server.py");
        let mut f = std::fs::File::create(&path).ok()?;
        f.write_all(FAKE_MCP_SERVER_PY.as_bytes()).ok()?;
        let mut perms = std::fs::metadata(&path).ok()?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).ok()?;
        Some(dir)
    }

    #[test]
    fn tool_call_round_then_text_round_executes_tool_and_returns_final_text() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = write_fake_mcp_server().expect("tempdir");
        let script = dir
            .path()
            .join("fake_mcp_server.py")
            .to_string_lossy()
            .to_string();
        let client = McpClient::start_with_command("python3", &[script]).expect("start mcp");

        let round1 = sse_body(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"echo","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"text\":\"hi\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        ]);
        let round2 = sse_body(&[
            r#"{"choices":[{"delta":{"content":"done"},"finish_reason":"stop"}]}"#,
        ]);
        let (base_url, requests) = spawn_fake_endpoint(vec![round1, round2]);
        let turn = test_turn(test_endpoint(base_url));
        let (stream_tx, _stream_rx) = mpsc::channel();
        let (tool_tx, tool_rx) = mpsc::channel();
        let (progress_tx, progress_rx) = mpsc::channel();

        let result = run_turn_with_client(turn, &stream_tx, &tool_tx, &progress_tx, Some(client));
        assert_eq!(result, Ok("done".to_string()));

        let tool_call = tool_rx.try_recv().expect("tool_tx row");
        assert_eq!(tool_call.name, "echo");
        assert_eq!(tool_call.input, serde_json::json!({"text": "hi"}));

        let progress = progress_rx.try_recv().expect("progress row");
        assert_eq!(progress.component, "widget");
        assert_eq!(progress.status, "done");

        // The single most load-bearing behavior of the loop: round 2's
        // request must carry forward the assistant tool_calls message and a
        // matching role:"tool" message with the echoed result.
        let seen = requests.lock().unwrap();
        assert_eq!(seen.len(), 2, "expected two requests, got: {seen:?}");
        let round2_body: serde_json::Value =
            serde_json::from_str(&seen[1]).expect("round 2 body is valid JSON");
        let msgs = round2_body["messages"].as_array().expect("messages array");
        let assistant_msg = msgs
            .iter()
            .find(|m| m["role"] == "assistant" && m["tool_calls"].is_array() && !m["tool_calls"].as_array().unwrap().is_empty())
            .expect("assistant tool_calls message present in round 2 body");
        assert_eq!(assistant_msg["tool_calls"][0]["id"], "call_1");
        let tool_msg = msgs
            .iter()
            .find(|m| m["role"] == "tool" && m["tool_call_id"] == "call_1")
            .expect("role:tool message with tool_call_id call_1 present");
        assert!(
            tool_msg["content"].as_str().unwrap_or_default().contains("echoed"),
            "tool message content was: {}",
            tool_msg["content"]
        );
    }

    #[test]
    fn malformed_tool_call_arguments_send_error_tool_message_and_recover() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = write_fake_mcp_server().expect("tempdir");
        let script = dir
            .path()
            .join("fake_mcp_server.py")
            .to_string_lossy()
            .to_string();
        let client = McpClient::start_with_command("python3", &[script]).expect("start mcp");

        let round1 = sse_body(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_bad","function":{"name":"echo","arguments":"{not json"}}]},"finish_reason":"tool_calls"}]}"#,
        ]);
        let round2 = sse_body(&[
            r#"{"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
        ]);
        let (base_url, requests) = spawn_fake_endpoint(vec![round1, round2]);
        let turn = test_turn(test_endpoint(base_url));
        let (stream_tx, _stream_rx) = mpsc::channel();
        let (tool_tx, tool_rx) = mpsc::channel();
        let (progress_tx, _progress_rx) = mpsc::channel();

        let result = run_turn_with_client(turn, &stream_tx, &tool_tx, &progress_tx, Some(client));
        assert_eq!(result, Ok("ok".to_string()));
        assert!(tool_rx.try_recv().is_err(), "malformed call must not execute / emit tool_tx");

        let seen = requests.lock().unwrap();
        let round2_body: serde_json::Value =
            serde_json::from_str(&seen[1]).expect("round 2 body is valid JSON");
        let msgs = round2_body["messages"].as_array().expect("messages array");
        let tool_msg = msgs
            .iter()
            .find(|m| m["role"] == "tool" && m["tool_call_id"] == "call_bad")
            .expect("error tool message present for malformed call");
        assert!(
            tool_msg["content"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase()
                .contains("json"),
            "tool message content was: {}",
            tool_msg["content"]
        );
    }

    #[test]
    fn three_consecutive_malformed_tool_calls_abort_turn() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = write_fake_mcp_server().expect("tempdir");
        let script = dir
            .path()
            .join("fake_mcp_server.py")
            .to_string_lossy()
            .to_string();
        let client = McpClient::start_with_command("python3", &[script]).expect("start mcp");

        let bad_call = |id: &str| {
            sse_body(&[&format!(
                r#"{{"choices":[{{"delta":{{"tool_calls":[{{"index":0,"id":"{id}","function":{{"name":"echo","arguments":"{{not json"}}}}]}},"finish_reason":"tool_calls"}}]}}"#
            )])
        };
        let (base_url, _requests) = spawn_fake_endpoint(vec![
            bad_call("call_1"),
            bad_call("call_2"),
            bad_call("call_3"),
        ]);
        let turn = test_turn(test_endpoint(base_url));
        let (stream_tx, _stream_rx) = mpsc::channel();
        let (tool_tx, _tool_rx) = mpsc::channel();
        let (progress_tx, _progress_rx) = mpsc::channel();

        let err = run_turn_with_client(turn, &stream_tx, &tool_tx, &progress_tx, Some(client))
            .expect_err("three consecutive malformed calls must abort");
        assert!(
            err.contains("3 consecutive malformed"),
            "error was: {err}"
        );
    }

    #[test]
    fn exceeding_max_rounds_aborts_turn() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = write_fake_mcp_server().expect("tempdir");
        let script = dir
            .path()
            .join("fake_mcp_server.py")
            .to_string_lossy()
            .to_string();
        let client = McpClient::start_with_command("python3", &[script]).expect("start mcp");

        // 33 tool-call rounds so the loop never sees a "stop": the 32-round
        // cap must trip before the server runs out of scripted responses.
        let tool_round = || {
            sse_body(&[
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_x","function":{"name":"echo","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
            ])
        };
        let bodies: Vec<String> = (0..33).map(|_| tool_round()).collect();
        let (base_url, _requests) = spawn_fake_endpoint(bodies);
        let turn = test_turn(test_endpoint(base_url));
        let (stream_tx, _stream_rx) = mpsc::channel();
        let (tool_tx, _tool_rx) = mpsc::channel();
        let (progress_tx, _progress_rx) = mpsc::channel();

        let err = run_turn_with_client(turn, &stream_tx, &tool_tx, &progress_tx, Some(client))
            .expect_err("32-round cap must trip");
        assert!(
            err.contains("32 tool-call rounds"),
            "error was: {err}"
        );
    }

    #[test]
    fn non_2xx_status_includes_status_code_and_body_text() {
        let (base_url, _requests) = spawn_fake_endpoint_with_status(vec![(
            404,
            r#"{"error":"model 'gpt-oss:120b' not found"}"#.to_string(),
        )]);
        let turn = test_turn(test_endpoint(base_url));
        let (stream_tx, _) = mpsc::channel();
        let (tool_tx, _) = mpsc::channel();
        let (progress_tx, _) = mpsc::channel();
        let err = run_turn_with_client(turn, &stream_tx, &tool_tx, &progress_tx, None)
            .expect_err("non-2xx must fail the turn");
        assert!(err.contains("test-endpoint"), "error was: {err}");
        assert!(err.contains("404"), "error was: {err}");
        assert!(err.contains("gpt-oss:120b"), "error was: {err}");
    }
}
