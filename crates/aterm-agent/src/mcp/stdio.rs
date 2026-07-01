//! T-6.2: a local stdio MCP client - run our own MCP client in Rust for LOCAL
//! stdio servers (the common dev case: a filesystem server, a git server, a
//! project-specific server). We spawn the server process, speak JSON-RPC over its
//! stdin/stdout, `initialize` + `tools/list`, and register each tool as a native
//! tool (via [`crate::tools::ToolRegistry::with_mcp_tools`]) so the shared turn
//! loop calls it like any other. Provider-agnostic (works under either backend)
//! and fully ON-DEVICE (contrast the connector's not-ZDR-eligible remote path).
//!
//! # Dependency decision: hand-rolled, not `rmcp`
//!
//! The ticket asks us to evaluate `rmcp` (the official Rust MCP SDK) vs
//! hand-rolling. We **hand-roll** JSON-RPC over stdio, for the same reasons the
//! provider clients (Anthropic/OpenAI) and the SSE framer are hand-rolled here:
//! (1) the transport we need is tiny - newline-delimited JSON-RPC 2.0 with three
//! methods (`initialize`, `tools/list`, `tools/call`) - so a dependency's schema
//! types + transport machinery would be more surface than the ~200 lines it
//! replaces; (2) it keeps the crate's dependency graph and `cargo deny` license
//! surface minimal (a locked-decision value); (3) the [`McpTransport`] seam makes
//! the client logic pure and headless-testable with a mock, matching how the rest
//! of the agent spine is tested (no process, no network). If we later need MCP
//! resources/prompts or the Streamable-HTTP transport, revisit `rmcp` then.
//!
//! # Safety
//!
//! Every local MCP tool call flows through the SAME seams as a native tool: the
//! turn loop gates it ([`crate::turn::gate_tool`] over-approximates an MCP call to
//! RequireConfirm, since its args are opaque), sanitizes its output against the
//! single [`Secrets`](crate::secrets::Secrets) source, and records it in the
//! timeline. A crashed/exited server surfaces as an error result (never a hang):
//! a closed stream maps to [`McpError::Closed`] and a wedged server is bounded by
//! [`REQUEST_TIMEOUT`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::sandbox::Sandbox;
use crate::sink::Sinks;
use crate::tools::{McpToolCall, McpToolSpec, ToolDispatch, ToolInput, ToolOutcome};

/// The MCP protocol revision we advertise in `initialize`.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Per-request wall-clock cap: a server that accepts a request but never answers
/// (wedged, not crashed) is bounded here so a tool call can never hang the loop.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// An error talking to a local MCP server. Never a panic; the router maps these to
/// an `is_error` [`ToolOutcome`] so the model + user see a clean failure.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// A transport-level IO failure (spawn, write, or read).
    #[error("mcp transport io: {0}")]
    Io(String),
    /// The server closed its stdout (crashed or exited) before answering.
    #[error("mcp server closed the connection")]
    Closed,
    /// The server accepted the request but did not answer within [`REQUEST_TIMEOUT`].
    #[error("mcp request timed out")]
    Timeout,
    /// A malformed (non-JSON-RPC) message from the server.
    #[error("mcp protocol error: {0}")]
    Protocol(String),
    /// A JSON-RPC error object returned by the server.
    #[error("mcp server error {code}: {message}")]
    Server { code: i64, message: String },
}

// ---- JSON-RPC codec (pure) --------------------------------------------------

/// Encode a JSON-RPC 2.0 request line (no trailing newline; the transport frames).
#[must_use]
fn encode_request(id: u64, method: &str, params: &Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string()
}

/// Encode a JSON-RPC 2.0 notification line (no `id`, no response expected).
#[must_use]
fn encode_notification(method: &str, params: &Value) -> String {
    json!({ "jsonrpc": "2.0", "method": method, "params": params }).to_string()
}

/// A parsed response to one of our requests.
enum RpcResponse {
    Result(Value),
    Error { code: i64, message: String },
}

/// Parse one incoming line. Returns `Ok(None)` for a message that is NOT the
/// response to `expected_id` (a server notification/request, or a different id) so
/// the caller keeps reading; `Ok(Some(..))` for our response; `Err` for malformed
/// JSON.
fn parse_response(line: &str, expected_id: u64) -> Result<Option<RpcResponse>, McpError> {
    let value: Value = serde_json::from_str(line).map_err(|e| McpError::Protocol(e.to_string()))?;
    // A message with no `id`, or a different `id`, is not our response.
    match value.get("id").and_then(Value::as_u64) {
        Some(id) if id == expected_id => {}
        _ => return Ok(None),
    }
    if let Some(err) = value.get("error") {
        let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
            .to_string();
        return Ok(Some(RpcResponse::Error { code, message }));
    }
    Ok(Some(RpcResponse::Result(
        value.get("result").cloned().unwrap_or(Value::Null),
    )))
}

/// Flatten an MCP tool result's `content` array (`[{type:"text", text}]`) into one
/// string. A bare string is tolerated for forward-compat. UNTRUSTED - the turn
/// loop sanitizes it before it re-enters context.
#[must_use]
fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

// ---- transport seam ---------------------------------------------------------

/// A duplex line transport to an MCP server. Split out so the client logic is pure
/// and testable with an in-memory mock (no process). Messages are single lines of
/// JSON-RPC (the transport owns newline framing).
#[allow(async_fn_in_trait)]
pub trait McpTransport: Send {
    /// Send one JSON-RPC message (a single line; no embedded newline).
    async fn send(&mut self, line: String) -> Result<(), McpError>;
    /// Receive the next message line, or `None` at end-of-stream (server exited).
    async fn recv(&mut self) -> Result<Option<String>, McpError>;
}

/// How to launch a local stdio MCP server.
#[derive(Debug, Clone)]
pub struct StdioServerConfig {
    /// Logical server name (used to route tool calls back to this client).
    pub name: String,
    /// The executable to spawn.
    pub command: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Working directory (defaults to the process cwd).
    pub cwd: Option<PathBuf>,
    /// Extra environment variables.
    pub env: Vec<(String, String)>,
}

impl StdioServerConfig {
    /// A config with no cwd/env override.
    #[must_use]
    pub fn new(name: impl Into<String>, command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args,
            cwd: None,
            env: Vec::new(),
        }
    }
}

/// The real transport: a spawned child process spoken to over its stdin/stdout.
pub struct ProcessTransport {
    // Kept so the child is killed on drop (`kill_on_drop`); reaping it is the
    // Drop's job, so `_child` is intentionally not read after spawn.
    _child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
}

impl ProcessTransport {
    /// Spawn the configured server with piped stdio (stderr discarded). The child
    /// is killed on drop so a dropped client never leaks a server process.
    ///
    /// # Errors
    /// [`McpError::Io`] if the process cannot be spawned or its pipes are missing.
    pub async fn spawn(config: &StdioServerConfig) -> Result<Self, McpError> {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut cmd = tokio::process::Command::new(&config.command);
        cmd.args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(cwd) = &config.cwd {
            cmd.current_dir(cwd);
        }
        for (k, v) in &config.env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().map_err(|e| McpError::Io(e.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Io("child has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Io("child has no stdout".into()))?;
        let stdout = BufReader::new(stdout).lines();
        Ok(Self {
            _child: child,
            stdin,
            stdout,
        })
    }
}

impl McpTransport for ProcessTransport {
    async fn send(&mut self, mut line: String) -> Result<(), McpError> {
        use tokio::io::AsyncWriteExt;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| McpError::Io(e.to_string()))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| McpError::Io(e.to_string()))
    }

    async fn recv(&mut self) -> Result<Option<String>, McpError> {
        self.stdout
            .next_line()
            .await
            .map_err(|e| McpError::Io(e.to_string()))
    }
}

// ---- the client -------------------------------------------------------------

struct ClientState<T: McpTransport> {
    transport: T,
    next_id: u64,
}

/// A JSON-RPC client for one local stdio MCP server. Requests serialize through an
/// internal lock (the turn loop calls MCP tools one at a time anyway, since an MCP
/// call is never parallel-safe), so one exchange never interleaves with another.
pub struct StdioMcpClient<T: McpTransport> {
    name: String,
    state: Mutex<ClientState<T>>,
}

impl<T: McpTransport> StdioMcpClient<T> {
    /// Wrap an already-connected transport.
    #[must_use]
    pub fn new(name: impl Into<String>, transport: T) -> Self {
        Self {
            name: name.into(),
            state: Mutex::new(ClientState {
                transport,
                next_id: 1,
            }),
        }
    }

    /// The server's logical name (the routing key).
    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.name
    }

    /// Issue one request and await its matching response (skipping any interleaved
    /// notifications), bounded by [`REQUEST_TIMEOUT`].
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let mut guard = self.state.lock().await;
        let id = guard.next_id;
        guard.next_id += 1;
        guard
            .transport
            .send(encode_request(id, method, &params))
            .await?;
        let transport = &mut guard.transport;
        match tokio::time::timeout(REQUEST_TIMEOUT, recv_matching(transport, id)).await {
            Ok(result) => result,
            Err(_) => Err(McpError::Timeout),
        }
    }

    /// Send a fire-and-forget notification (no response awaited).
    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let mut guard = self.state.lock().await;
        guard
            .transport
            .send(encode_notification(method, &params))
            .await
    }

    /// Perform the MCP `initialize` handshake and send `notifications/initialized`.
    ///
    /// # Errors
    /// Propagates any transport error or a server JSON-RPC error.
    pub async fn initialize(&self) -> Result<(), McpError> {
        let params = json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "aterm", "version": env!("CARGO_PKG_VERSION") },
        });
        let _ = self.request("initialize", params).await?;
        self.notify("notifications/initialized", json!({})).await?;
        Ok(())
    }

    /// List the server's tools, tagged with this server's name.
    ///
    /// # Errors
    /// Propagates any transport error or a server JSON-RPC error.
    pub async fn list_tools(&self) -> Result<Vec<McpToolSpec>, McpError> {
        let result = self.request("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(tools
            .iter()
            .filter_map(|t| {
                let name = t.get("name").and_then(Value::as_str)?.to_string();
                let description = t
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let input_schema = t
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object" }));
                Some(McpToolSpec {
                    server: self.name.clone(),
                    name,
                    description,
                    input_schema,
                })
            })
            .collect())
    }

    /// Call a tool. A server-reported tool error (`isError:true`) is returned as an
    /// error [`ToolOutcome`], not an `Err` (the model should see and correct it); a
    /// transport/protocol failure is an `Err` the router turns into an error result.
    ///
    /// # Errors
    /// Transport, timeout, protocol, or JSON-RPC-error-object failures.
    pub async fn call_tool(&self, name: &str, args: Value) -> Result<ToolOutcome, McpError> {
        let params = json!({ "name": name, "arguments": args });
        let result = self.request("tools/call", params).await?;
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let output = content_text(result.get("content"));
        Ok(if is_error {
            ToolOutcome::error(output)
        } else {
            ToolOutcome::ok(output)
        })
    }
}

impl StdioMcpClient<ProcessTransport> {
    /// Spawn a local server and wrap it in a client (does NOT initialize).
    ///
    /// # Errors
    /// [`McpError::Io`] if the process cannot be spawned.
    pub async fn spawn(config: &StdioServerConfig) -> Result<Self, McpError> {
        let transport = ProcessTransport::spawn(config).await?;
        Ok(Self::new(&config.name, transport))
    }

    /// Spawn, `initialize`, and `tools/list` in one step - the usual entry point.
    /// Returns the live client plus its advertised tools (register them with
    /// [`crate::tools::ToolRegistry::with_mcp_tools`]).
    ///
    /// # Errors
    /// Any spawn/transport/server error along the way.
    pub async fn connect(config: &StdioServerConfig) -> Result<(Self, Vec<McpToolSpec>), McpError> {
        let client = Self::spawn(config).await?;
        client.initialize().await?;
        let tools = client.list_tools().await?;
        Ok((client, tools))
    }
}

/// Read lines until the response with `id` arrives (skipping notifications / other
/// ids), or the stream closes.
async fn recv_matching<T: McpTransport>(transport: &mut T, id: u64) -> Result<Value, McpError> {
    loop {
        match transport.recv().await? {
            None => return Err(McpError::Closed),
            Some(line) if line.trim().is_empty() => continue,
            Some(line) => match parse_response(&line, id)? {
                Some(RpcResponse::Result(v)) => return Ok(v),
                Some(RpcResponse::Error { code, message }) => {
                    return Err(McpError::Server { code, message })
                }
                None => continue,
            },
        }
    }
}

// ---- routing ----------------------------------------------------------------

/// A composite [`ToolDispatch`] that routes a [`ToolInput::Mcp`] call to the
/// owning MCP client and every native tool to the [`Sinks`] (gate + sandbox +
/// filesystem). This is the single dispatcher the turn loop drives when local MCP
/// servers are configured. A crashed server or unknown routing surfaces as an
/// error result - the loop never hangs.
pub struct McpToolRouter<S: Sandbox + Clone, T: McpTransport> {
    sinks: Sinks<S>,
    clients: HashMap<String, Arc<StdioMcpClient<T>>>,
}

impl<S: Sandbox + Clone, T: McpTransport> McpToolRouter<S, T> {
    /// A router that dispatches native tools to `sinks` and no MCP servers yet.
    #[must_use]
    pub fn new(sinks: Sinks<S>) -> Self {
        Self {
            sinks,
            clients: HashMap::new(),
        }
    }

    /// Register a connected MCP client under its [`server_name`](StdioMcpClient::server_name).
    #[must_use]
    pub fn with_server(mut self, client: Arc<StdioMcpClient<T>>) -> Self {
        self.clients
            .insert(client.server_name().to_string(), client);
        self
    }
}

impl<S, T> ToolDispatch for McpToolRouter<S, T>
where
    S: Sandbox + Clone + Send + Sync + 'static,
    T: McpTransport + Send + Sync + 'static,
{
    async fn dispatch(&self, input: ToolInput) -> ToolOutcome {
        match input {
            ToolInput::Mcp(McpToolCall { server, name, args }) => match self.clients.get(&server) {
                Some(client) => client
                    .call_tool(&name, args)
                    .await
                    .unwrap_or_else(|e| ToolOutcome::error(format!("mcp `{name}`: {e}"))),
                None => {
                    ToolOutcome::error(format!("no MCP client registered for server `{server}`"))
                }
            },
            other => self.sinks.dispatch(other).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    // ---- pure codec ---------------------------------------------------------

    #[test]
    fn encodes_request_and_notification() {
        let req: Value =
            serde_json::from_str(&encode_request(7, "tools/list", &json!({}))).unwrap();
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["id"], 7);
        assert_eq!(req["method"], "tools/list");
        let note: Value = serde_json::from_str(&encode_notification(
            "notifications/initialized",
            &json!({}),
        ))
        .unwrap();
        assert_eq!(note["method"], "notifications/initialized");
        assert!(note.get("id").is_none(), "a notification has no id");
    }

    #[test]
    fn parse_response_skips_other_ids_and_notifications() {
        // A notification (no id) is skipped.
        assert!(parse_response(r#"{"jsonrpc":"2.0","method":"x"}"#, 1)
            .unwrap()
            .is_none());
        // A response to a different id is skipped.
        assert!(parse_response(r#"{"jsonrpc":"2.0","id":2,"result":{}}"#, 1)
            .unwrap()
            .is_none());
        // Malformed JSON is a protocol error, not a panic.
        assert!(matches!(
            parse_response("not json", 1),
            Err(McpError::Protocol(_))
        ));
    }

    #[test]
    fn content_text_flattens_blocks() {
        let c = json!([{ "type": "text", "text": "a" }, { "type": "text", "text": "b" }]);
        assert_eq!(content_text(Some(&c)), "a\nb");
        assert_eq!(content_text(Some(&json!("plain"))), "plain");
        assert_eq!(content_text(None), "");
    }

    // ---- client over a mock transport (no process) --------------------------

    /// A scripted transport: `recv` pops the next queued line (an exhausted queue
    /// returns `None`, i.e. the server closed); `send` records the outgoing line.
    struct MockTransport {
        incoming: VecDeque<String>,
        sent: Vec<String>,
    }

    impl MockTransport {
        fn new(incoming: Vec<&str>) -> Self {
            Self {
                incoming: incoming.into_iter().map(String::from).collect(),
                sent: Vec::new(),
            }
        }
    }

    impl McpTransport for MockTransport {
        async fn send(&mut self, line: String) -> Result<(), McpError> {
            self.sent.push(line);
            Ok(())
        }
        async fn recv(&mut self) -> Result<Option<String>, McpError> {
            Ok(self.incoming.pop_front())
        }
    }

    #[tokio::test]
    async fn initialize_list_and_call_round_trip() {
        // T-6.2 AC: a server's tools are listed and one is invoked. ids: init=1,
        // list=2, call=3.
        let client = StdioMcpClient::new(
            "fs",
            MockTransport::new(vec![
                r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18"}}"#,
                r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"read_text","description":"read a file","inputSchema":{"type":"object"}}]}}"#,
                r#"{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"hello"}],"isError":false}}"#,
            ]),
        );
        client.initialize().await.unwrap();
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "read_text");
        assert_eq!(tools[0].server, "fs");
        let out = client
            .call_tool("read_text", json!({ "path": "x" }))
            .await
            .unwrap();
        assert_eq!(out.output, "hello");
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn server_tool_error_is_surfaced_not_hidden() {
        let client = StdioMcpClient::new(
            "fs",
            MockTransport::new(vec![
                r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"nope"}],"isError":true}}"#,
            ]),
        );
        let out = client.call_tool("boom", json!({})).await.unwrap();
        assert!(out.is_error);
        assert_eq!(out.output, "nope");
    }

    #[tokio::test]
    async fn jsonrpc_error_object_maps_to_server_error() {
        let client = StdioMcpClient::new(
            "fs",
            MockTransport::new(vec![
                r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"method not found"}}"#,
            ]),
        );
        let err = client.initialize().await.unwrap_err();
        assert!(
            matches!(err, McpError::Server { code: -32601, .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn server_crash_is_surfaced_cleanly_not_hung() {
        // T-6.2 AC: server crash/exit is handled cleanly (no hang). An empty
        // incoming queue models a closed stdout (the server exited).
        let client = StdioMcpClient::new("fs", MockTransport::new(vec![]));
        let err = client.initialize().await.unwrap_err();
        assert!(matches!(err, McpError::Closed), "{err:?}");
    }

    #[tokio::test]
    async fn a_leading_notification_is_skipped_before_the_response() {
        let client = StdioMcpClient::new(
            "fs",
            MockTransport::new(vec![
                r#"{"jsonrpc":"2.0","method":"notifications/message","params":{}}"#,
                r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#,
            ]),
        );
        // initialize's request (id 1) resolves past the interleaved notification.
        client.initialize().await.unwrap();
    }

    // ---- routing + turn-loop integration ------------------------------------

    fn no_sandbox_sinks() -> Sinks<crate::sandbox::NoSandbox> {
        use crate::sandbox::{NoSandbox, SandboxRunner};
        use crate::secrets::Secrets;
        Sinks::with_sandbox(
            SandboxRunner::new(NoSandbox),
            std::env::temp_dir(),
            Secrets::new(),
        )
    }

    #[tokio::test]
    async fn router_sends_mcp_to_the_client_and_reports_unknown_servers() {
        let client = Arc::new(StdioMcpClient::new(
            "fs",
            MockTransport::new(vec![
                r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"routed"}],"isError":false}}"#,
            ]),
        ));
        let router = McpToolRouter::new(no_sandbox_sinks()).with_server(client);

        let ok = router
            .dispatch(ToolInput::Mcp(McpToolCall {
                server: "fs".into(),
                name: "read_text".into(),
                args: json!({}),
            }))
            .await;
        assert_eq!(ok.output, "routed");

        let missing = router
            .dispatch(ToolInput::Mcp(McpToolCall {
                server: "ghost".into(),
                name: "x".into(),
                args: json!({}),
            }))
            .await;
        assert!(missing.is_error);
        assert!(missing.output.contains("ghost"));
    }

    #[tokio::test]
    async fn mcp_tool_runs_through_the_turn_loop_gated_and_confirmed() {
        // T-6.2 AC: an MCP tool is invoked through the turn loop, gated exactly
        // like a native tool. The registry routes the name to ToolInput::Mcp, the
        // gate over-approximates to RequireConfirm (so the approver is consulted),
        // and the router dispatches it to the client.
        use crate::provider::{MockProvider, ProviderEvent, StopReason, ToolCall, Usage};
        use crate::secrets::Secrets;
        use crate::tools::ToolRegistry;
        use crate::turn::{AgentTurn, CancelToken, ConfirmDecision, ConfirmHandler};
        use tokio::sync::mpsc;

        struct ApproveAll;
        impl ConfirmHandler for ApproveAll {
            async fn confirm(
                &self,
                _call: &ToolCall,
                _a: &crate::risk::RiskAssessment,
            ) -> ConfirmDecision {
                ConfirmDecision::Approved
            }
        }

        // Register one MCP tool "notes_search" from server "notes".
        let registry = ToolRegistry::with_default_tools().with_mcp_tools(vec![McpToolSpec {
            server: "notes".into(),
            name: "notes_search".into(),
            description: "search notes".into(),
            input_schema: json!({ "type": "object" }),
        }]);

        // The model proposes that MCP tool, then ends the turn.
        let provider = MockProvider::scripted(vec![
            vec![
                ProviderEvent::MessageStart,
                ProviderEvent::ToolUseStart {
                    id: "t1".into(),
                    name: "notes_search".into(),
                },
                ProviderEvent::ToolUseInputDelta {
                    json: r#"{"q":"rust"}"#.into(),
                },
                ProviderEvent::ToolUseStop,
                ProviderEvent::MessageDelta {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage::default(),
                },
                ProviderEvent::MessageStop,
            ],
            vec![
                ProviderEvent::MessageStart,
                ProviderEvent::TextDelta("done".into()),
                ProviderEvent::MessageDelta {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                },
                ProviderEvent::MessageStop,
            ],
        ]);

        let client = Arc::new(StdioMcpClient::new(
            "notes",
            MockTransport::new(vec![
                r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"found 3 notes"}],"isError":false}}"#,
            ]),
        ));
        let router = McpToolRouter::new(no_sandbox_sinks()).with_server(client);
        let secrets = Secrets::new();

        let request = crate::provider::TurnRequest {
            model: "mock".into(),
            system: None,
            messages: vec![crate::provider::Message::user("search my notes")],
            tools: registry.specs(),
            effort: crate::provider::Effort::Medium,
            max_tokens: 1024,
        };
        let (etx, mut erx) = mpsc::channel(256);
        let outcome = AgentTurn::new(&provider, &secrets)
            .run(
                request,
                &registry,
                &router,
                &ApproveAll,
                &CancelToken::new(),
                etx,
            )
            .await
            .unwrap();

        assert_eq!(outcome.stop_reason, StopReason::EndTurn);
        // The MCP tool's result rode back into the timeline.
        let mut events = Vec::new();
        while let Ok(e) = erx.try_recv() {
            events.push(e);
        }
        assert!(
            events.iter().any(|e| matches!(
                e,
                crate::provider::AgentEvent::ToolResult { output, .. } if output.contains("found 3 notes")
            )),
            "the MCP tool result should reach the timeline: {events:?}"
        );
    }

    /// A real spawned process that exits immediately must surface as `Closed`, not
    /// hang - proves `ProcessTransport` EOF handling end to end.
    #[cfg(unix)]
    #[tokio::test]
    async fn real_process_that_exits_is_closed_not_hung() {
        let config = StdioServerConfig::new("dead", "true", vec![]);
        let client = StdioMcpClient::spawn(&config).await.unwrap();
        let err = client.initialize().await.unwrap_err();
        assert!(
            matches!(err, McpError::Closed | McpError::Io(_)),
            "a process that exits should close cleanly, got {err:?}"
        );
    }
}
