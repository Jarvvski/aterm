//! The Anthropic Messages-API client (T-5.2) - aterm's DEFAULT provider.
//!
//! A thin, hand-rolled typed client over `POST /v1/messages` with `stream:true`:
//! it serializes a provider-neutral [`TurnRequest`](super::TurnRequest) into the
//! Anthropic wire shape, POSTs over `reqwest`, and translates the SSE event
//! stream into the provider-neutral [`ProviderEvent`](super::ProviderEvent)
//! sequence the shared [`AgentEventMapper`](super::AgentEventMapper) folds.
//!
//! Locked decisions this honors (see `docs/research/06-agent-architecture.md`):
//!
//! - Call the Messages API directly; NO Agent SDK (Commercial-Terms / GPLv3
//!   conflict). We own the loop.
//! - `claude-opus-4-8` with `thinking:{type:"adaptive", display:"summarized"}` +
//!   `output_config:{effort:...}`. `budget_tokens` is NEVER sent (it 400s on
//!   Opus 4.8); a guard test asserts it is absent from every request body.
//! - Custom tools carry `"strict": true` as a SIBLING of name/description/
//!   input_schema (NOT on `tool_choice`).
//! - `pause_turn` is resumed by RE-SENDING the request with the assistant's
//!   accumulated content appended - never by injecting a synthetic "continue"
//!   user message.
//!
//! The SSE decoding is split into pure, headless-testable pieces (the
//! [`SseDecoder`] line/event framer and the [`StreamState`] event mapper) that
//! tests drive from byte fixtures with NO network; the HTTP path itself is
//! exercised against a loopback mock server (also no real network).

use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::sse::SseDecoder;
use super::{
    ContentBlock, LlmProvider, ProviderError, ProviderEvent, Role, StopReason, TurnRequest, Usage,
};
use crate::mcp::connector::{
    validate_connector_body, validate_servers, McpServer, MCP_CONNECTOR_BETA,
};

/// The production Messages-API base URL.
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
/// The pinned stable API-version header.
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Cap on `pause_turn` resumes within one logical turn (a backstop against a
/// server-tool loop that never settles).
const DEFAULT_MAX_RESUMES: u32 = 8;

/// The Anthropic Messages-API provider. Holds the API key, an HTTP client, and
/// the base URL (overridable for tests / proxies / gateway deployments).
#[derive(Clone)]
pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
    client: reqwest::Client,
    max_resumes: u32,
    /// Remote MCP servers consumed via the connector (T-6.1). Empty by default;
    /// when non-empty the request carries `mcp_servers` + a matching `mcp_toolset`
    /// per server and the `mcp-client-2025-11-20` beta header. This is an
    /// Anthropic-specific feature, so it lives on the provider - `TurnRequest`
    /// stays provider-neutral.
    mcp_servers: Vec<McpServer>,
}

// Manual Debug so the API key never lands in logs or panic output.
impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("api_key", &"<redacted>")
            .field("base_url", &self.base_url)
            .field("max_resumes", &self.max_resumes)
            .field("mcp_servers", &self.mcp_servers.len())
            .finish()
    }
}

impl AnthropicProvider {
    /// Build a provider with a key (custody is T-8.3; this just holds it).
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            client: reqwest::Client::new(),
            max_resumes: DEFAULT_MAX_RESUMES,
            mcp_servers: Vec::new(),
        }
    }

    /// Override the base URL (e.g. a loopback test server or a gateway).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Consume the given remote MCP servers via the connector (T-6.1). Each turn
    /// then carries `mcp_servers` + a matching `mcp_toolset` per server, gated by
    /// each server's deny-by-default [`McpToolPolicy`](crate::mcp::connector::McpToolPolicy),
    /// plus the `mcp-client-2025-11-20` beta header. NOT ZDR-eligible.
    #[must_use]
    pub fn with_mcp_servers(mut self, servers: Vec<McpServer>) -> Self {
        self.mcp_servers = servers;
        self
    }

    /// Borrow the configured HTTP client.
    #[must_use]
    pub fn http(&self) -> &reqwest::Client {
        &self.client
    }

    /// Serialize a neutral [`TurnRequest`] into the Anthropic Messages-API JSON
    /// body. `extra_assistant`, when present, is appended as one assistant
    /// message - the resume idiom for `pause_turn` (NOT a synthetic "continue").
    fn build_body(&self, request: &TurnRequest, extra_assistant: Option<&[Value]>) -> Value {
        let mut messages: Vec<Value> = request.messages.iter().map(wire_message).collect();
        if let Some(blocks) = extra_assistant {
            messages.push(json!({ "role": "assistant", "content": blocks }));
        }

        let mut body = json!({
            "model": request.model,
            "max_tokens": request.max_tokens,
            "stream": true,
            "messages": messages,
            // Adaptive thinking is the only on-mode for Opus 4.8; depth is the
            // effort knob, NOT budget_tokens.
            "thinking": { "type": "adaptive", "display": "summarized" },
            "output_config": { "effort": request.effort.as_str() },
        });

        if let Some(system) = &request.system {
            body["system"] = json!(system);
        }
        // Native custom tools + one `mcp_toolset` per connector server (T-6.1)
        // share the single `tools` array. The connector references each server by
        // exactly one toolset; `mcp_servers` declares them.
        let mut tools: Vec<Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                    "strict": t.strict,
                })
            })
            .collect();
        tools.extend(self.mcp_servers.iter().map(McpServer::toolset_json));
        if !tools.is_empty() {
            body["tools"] = json!(tools);
            body["tool_choice"] = json!({ "type": "auto" });
        }
        if !self.mcp_servers.is_empty() {
            body["mcp_servers"] = json!(self
                .mcp_servers
                .iter()
                .map(McpServer::server_json)
                .collect::<Vec<_>>());
        }
        body
    }
}

/// Map a neutral [`Message`](super::Message) to a wire message object. Tool
/// results ride in a `user` message (the Messages-API shape); the inline
/// operator channel maps to a `system` message.
fn wire_message(message: &super::Message) -> Value {
    let role = match message.role {
        Role::User | Role::Tool => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
    };
    let content: Vec<Value> = message.content.iter().map(wire_block).collect();
    json!({ "role": role, "content": content })
}

/// Map a neutral [`ContentBlock`] to its wire object.
fn wire_block(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text { text } => json!({ "type": "text", "text": text }),
        ContentBlock::ToolUse { id, name, input } => {
            json!({ "type": "tool_use", "id": id, "name": name, "input": input })
        }
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": content,
            "is_error": is_error,
        }),
    }
}

impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn default_model(&self) -> &'static str {
        "claude-opus-4-8"
    }

    async fn stream_turn(
        &self,
        request: TurnRequest,
        sink: mpsc::Sender<ProviderEvent>,
    ) -> Result<(), ProviderError> {
        let url = format!("{}/v1/messages", self.base_url);
        // Validate the connector config up front so a malformed toolset becomes a
        // local error, never a wasted round-trip that 400s (T-6.1 AC).
        if !self.mcp_servers.is_empty() {
            validate_servers(&self.mcp_servers)
                .map_err(|e| ProviderError::Invalid(e.to_string()))?;
        }
        // Cumulative assistant content across pause/resume hops (so a re-send
        // carries everything the model produced so far, not just the last hop).
        let mut accumulated: Vec<Value> = Vec::new();
        let mut resumes: u32 = 0;
        let mut first_response = true;
        // Token accounting folded across pause/resume hops: output_tokens is the
        // SUM over hops; input/cache tokens are taken from the FIRST hop only (a
        // resume re-sends accumulated content, so summing input would double-count
        // the context). Surfaced on the single final, non-suppressed MessageDelta.
        let mut acc_output: u32 = 0;
        let mut base_usage: Option<Usage> = None;

        loop {
            let extra = (!accumulated.is_empty()).then_some(accumulated.as_slice());
            let body = self.build_body(&request, extra);

            // Belt-and-suspenders: assert the 1:1 mcp_servers<->mcp_toolset
            // invariant on the assembled body before send (the API 400s otherwise).
            if !self.mcp_servers.is_empty() {
                validate_connector_body(&body)
                    .map_err(|e| ProviderError::Invalid(e.to_string()))?;
            }

            let mut builder = self
                .client
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("content-type", "application/json");
            if !self.mcp_servers.is_empty() {
                builder = builder.header("anthropic-beta", MCP_CONNECTOR_BETA);
            }
            let mut response = builder
                .json(&body)
                .send()
                .await
                .map_err(|e| ProviderError::Http(e.to_string()))?;

            let status = response.status();
            if !status.is_success() {
                if status.as_u16() == 401 || status.as_u16() == 403 {
                    return Err(ProviderError::Auth);
                }
                let detail = response.text().await.unwrap_or_default();
                return Err(ProviderError::Http(format!("{status}: {detail}")));
            }

            let mut decoder = SseDecoder::default();
            let mut state = StreamState::default();
            // True once this response stops with `pause_turn` and we have resume
            // budget left - we suppress its terminal events and re-send.
            let mut pausing = false;

            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|e| ProviderError::Http(e.to_string()))?
            {
                for payload in decoder.push(&chunk) {
                    let value: Value = match serde_json::from_str(&payload) {
                        Ok(v) => v,
                        Err(e) => {
                            let _ = sink
                                .send(ProviderEvent::Error(format!("SSE decode error: {e}")))
                                .await;
                            return Err(ProviderError::Decode(e.to_string()));
                        }
                    };
                    for event in state.accept(&value) {
                        // Collapse multiple HTTP responses into ONE logical turn:
                        // a resumed response's `MessageStart` is dropped, and a
                        // `pause_turn` stop (plus its trailing `MessageStop`) is
                        // suppressed so the consumer sees a single continuous turn.
                        match event {
                            ProviderEvent::MessageStart if !first_response => continue,
                            ProviderEvent::MessageDelta { stop_reason, usage } => {
                                // First hop sets the input/cache baseline for the turn.
                                if base_usage.is_none() {
                                    base_usage = Some(usage);
                                }
                                if stop_reason == StopReason::PauseTurn
                                    && resumes < self.max_resumes
                                {
                                    acc_output += usage.output_tokens;
                                    pausing = true;
                                    continue;
                                }
                                // Final delta: surface the folded turn total.
                                let mut total = base_usage.unwrap_or(usage);
                                total.output_tokens = acc_output + usage.output_tokens;
                                if sink
                                    .send(ProviderEvent::MessageDelta {
                                        stop_reason,
                                        usage: total,
                                    })
                                    .await
                                    .is_err()
                                {
                                    return Ok(());
                                }
                            }
                            ProviderEvent::MessageStop if pausing => {
                                // Terminal of a paused hop: stop reading and resume.
                                break;
                            }
                            other => {
                                if sink.send(other).await.is_err() {
                                    // Receiver dropped (turn aborted/cancelled).
                                    return Ok(());
                                }
                            }
                        }
                    }
                    if pausing {
                        break;
                    }
                }
                if pausing {
                    break;
                }
            }

            if pausing {
                resumes += 1;
                first_response = false;
                accumulated.extend(state.assistant_content());
                continue;
            }
            return Ok(());
        }
    }
}

/// Which kind of content block is open at a given index (so `content_block_stop`
/// knows whether to emit a [`ProviderEvent::ToolUseStop`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    Thinking,
    ToolUse,
    /// A connector `mcp_tool_use` block (T-6.1): its input JSON streams in like a
    /// tool_use, but it is assembled into one [`ProviderEvent::McpToolUse`] at
    /// close and NEVER dispatched locally.
    McpToolUse,
    /// A connector `mcp_tool_result` block (server-side result, delivered whole at
    /// `content_block_start`).
    McpToolResult,
    Other,
}

/// Per-block accumulation, retained after the block closes so the full assistant
/// `content` can be reconstructed for a `pause_turn` resume.
struct BlockAccum {
    kind: BlockKind,
    id: String,
    name: String,
    /// Connector `server_name` for an `mcp_tool_use` block; empty otherwise.
    server: String,
    text: String,
    signature: String,
    json: String,
}

/// Stateful translator from decoded Anthropic SSE event JSON to the neutral
/// [`ProviderEvent`] sequence. Also reconstructs the assistant `content` array
/// (text / thinking-with-signature / tool_use) for resume re-sends. Pure: tests
/// drive it with `serde_json::Value` fixtures, no network.
#[derive(Default)]
struct StreamState {
    blocks: std::collections::BTreeMap<u64, BlockAccum>,
    usage: Usage,
}

impl StreamState {
    fn accept(&mut self, value: &Value) -> Vec<ProviderEvent> {
        match value.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(u) = value.pointer("/message/usage") {
                    self.usage.input_tokens = u_u32(u, "input_tokens");
                    self.usage.cache_read_input_tokens = u_u32(u, "cache_read_input_tokens");
                    self.usage.cache_creation_input_tokens =
                        u_u32(u, "cache_creation_input_tokens");
                    self.usage.output_tokens = u_u32(u, "output_tokens");
                }
                vec![ProviderEvent::MessageStart]
            }
            Some("content_block_start") => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                let block = value.get("content_block");
                let btype = block
                    .and_then(|b| b.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let kind = match btype {
                    "text" => BlockKind::Text,
                    "thinking" => BlockKind::Thinking,
                    "tool_use" => BlockKind::ToolUse,
                    "mcp_tool_use" => BlockKind::McpToolUse,
                    "mcp_tool_result" => BlockKind::McpToolResult,
                    _ => BlockKind::Other,
                };
                let id = block
                    .and_then(|b| b.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let name = block
                    .and_then(|b| b.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                // Connector `server_name` (on mcp_tool_use blocks only).
                let server = block
                    .and_then(|b| b.get("server_name"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                // A connector result carries its content inline at start (the
                // server already executed it) - emit it whole and keep no open
                // block (close is a no-op).
                if kind == BlockKind::McpToolResult {
                    let tool_use_id = block
                        .and_then(|b| b.get("tool_use_id"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let is_error = block
                        .and_then(|b| b.get("is_error"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let output = mcp_result_text(block.and_then(|b| b.get("content")));
                    self.blocks.insert(
                        index,
                        BlockAccum {
                            kind,
                            id: tool_use_id.clone(),
                            name: String::new(),
                            server: String::new(),
                            text: String::new(),
                            signature: String::new(),
                            json: String::new(),
                        },
                    );
                    return vec![ProviderEvent::McpToolResult {
                        id: tool_use_id,
                        output,
                        is_error,
                    }];
                }
                self.blocks.insert(
                    index,
                    BlockAccum {
                        kind,
                        id: id.clone(),
                        name: name.clone(),
                        server: server.clone(),
                        text: String::new(),
                        signature: String::new(),
                        json: String::new(),
                    },
                );
                if kind == BlockKind::ToolUse {
                    vec![ProviderEvent::ToolUseStart { id, name }]
                } else {
                    // mcp_tool_use assembles at close; other kinds emit nothing.
                    Vec::new()
                }
            }
            Some("content_block_delta") => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                let delta = value.get("delta");
                let dtype = delta
                    .and_then(|d| d.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                match dtype {
                    "text_delta" => {
                        let t = delta_str(delta, "text");
                        if let Some(b) = self.blocks.get_mut(&index) {
                            b.text.push_str(&t);
                        }
                        vec![ProviderEvent::TextDelta(t)]
                    }
                    "thinking_delta" => {
                        let t = delta_str(delta, "thinking");
                        if let Some(b) = self.blocks.get_mut(&index) {
                            b.text.push_str(&t);
                        }
                        vec![ProviderEvent::ThinkingDelta(t)]
                    }
                    "signature_delta" => {
                        let s = delta_str(delta, "signature");
                        if let Some(b) = self.blocks.get_mut(&index) {
                            b.signature.push_str(&s);
                        }
                        Vec::new()
                    }
                    "input_json_delta" => {
                        let j = delta_str(delta, "partial_json");
                        let is_mcp =
                            self.blocks.get(&index).map(|b| b.kind) == Some(BlockKind::McpToolUse);
                        if let Some(b) = self.blocks.get_mut(&index) {
                            b.json.push_str(&j);
                        }
                        // A connector mcp_tool_use assembles at close; do not leak a
                        // ToolUseInputDelta that the mapper would misroute.
                        if is_mcp {
                            Vec::new()
                        } else {
                            vec![ProviderEvent::ToolUseInputDelta { json: j }]
                        }
                    }
                    _ => Vec::new(),
                }
            }
            Some("content_block_stop") => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                match self.blocks.get(&index).map(|b| b.kind) {
                    Some(BlockKind::ToolUse) => vec![ProviderEvent::ToolUseStop],
                    Some(BlockKind::McpToolUse) => {
                        let b = &self.blocks[&index];
                        let input: Value = if b.json.trim().is_empty() {
                            json!({})
                        } else {
                            serde_json::from_str(&b.json).unwrap_or_else(|_| json!({}))
                        };
                        vec![ProviderEvent::McpToolUse {
                            id: b.id.clone(),
                            name: b.name.clone(),
                            server: b.server.clone(),
                            input,
                        }]
                    }
                    _ => Vec::new(),
                }
            }
            Some("message_delta") => {
                let stop_reason = value
                    .pointer("/delta/stop_reason")
                    .and_then(Value::as_str)
                    .map(StopReason::from_anthropic)
                    .unwrap_or(StopReason::EndTurn);
                if let Some(u) = value.get("usage") {
                    // message_delta carries the final cumulative output token count.
                    let out = u_u32(u, "output_tokens");
                    if out > 0 {
                        self.usage.output_tokens = out;
                    }
                }
                vec![ProviderEvent::MessageDelta {
                    stop_reason,
                    usage: self.usage,
                }]
            }
            Some("message_stop") => vec![ProviderEvent::MessageStop],
            Some("error") => {
                let msg = value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown provider error")
                    .to_string();
                vec![ProviderEvent::Error(msg)]
            }
            // `ping` and anything unrecognized produce no neutral event.
            _ => Vec::new(),
        }
    }

    /// Reconstruct the assistant `content` blocks seen so far, in index order, to
    /// echo back on a `pause_turn` resume. Thinking blocks keep their signature
    /// (the Messages API requires them echoed UNCHANGED on the same model).
    fn assistant_content(&self) -> Vec<Value> {
        self.blocks
            .values()
            .filter_map(|b| match b.kind {
                BlockKind::Text => Some(json!({ "type": "text", "text": b.text })),
                BlockKind::Thinking => Some(json!({
                    "type": "thinking",
                    "thinking": b.text,
                    "signature": b.signature,
                })),
                BlockKind::ToolUse => {
                    let input: Value = if b.json.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&b.json).unwrap_or_else(|_| json!({}))
                    };
                    Some(json!({
                        "type": "tool_use",
                        "id": b.id,
                        "name": b.name,
                        "input": input,
                    }))
                }
                // Connector blocks execute server-side and are not echoed back on a
                // pause_turn resume (v1 limitation; connector turns rarely pause,
                // and re-sending a half-formed mcp pair risks a 400).
                BlockKind::McpToolUse | BlockKind::McpToolResult | BlockKind::Other => None,
            })
            .collect()
    }
}

fn u_u32(obj: &Value, key: &str) -> u32 {
    obj.get(key)
        .and_then(Value::as_u64)
        .map(|n| n.min(u64::from(u32::MAX)) as u32)
        .unwrap_or(0)
}

fn delta_str(delta: Option<&Value>, key: &str) -> String {
    delta
        .and_then(|d| d.get(key))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Flatten an `mcp_tool_result` block's `content` into a single string. The
/// Messages API delivers it as an array of `{type:"text", text}` blocks (a bare
/// string is tolerated for forward-compat). The text is UNTRUSTED - the turn loop
/// sanitizes it before it reaches the timeline.
fn mcp_result_text(content: Option<&Value>) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::sse::find_subseq;
    use crate::provider::{Effort, Message, ToolSpec};
    use std::io::{Read, Write};

    // ---- request-body shape (pure; no network) -------------------------------

    fn req_with(messages: Vec<Message>, tools: Vec<ToolSpec>) -> TurnRequest {
        TurnRequest {
            model: "claude-opus-4-8".to_string(),
            system: Some("be terse".to_string()),
            messages,
            tools,
            effort: Effort::High,
            max_tokens: 4096,
        }
    }

    #[test]
    fn body_sets_adaptive_thinking_effort_and_never_budget_tokens() {
        let p = AnthropicProvider::new("sk-test");
        let body = p.build_body(&req_with(vec![Message::user("hi")], vec![]), None);
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["thinking"]["display"], "summarized");
        assert_eq!(body["output_config"]["effort"], "high");
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["stream"], true);
        assert_eq!(body["system"], "be terse");
        // The whole serialized body must NEVER contain budget_tokens (400s on 4.8).
        let serialized = serde_json::to_string(&body).unwrap();
        assert!(
            !serialized.contains("budget_tokens"),
            "budget_tokens must never be sent: {serialized}"
        );
    }

    #[test]
    fn tools_serialize_with_strict_sibling_and_auto_choice() {
        let p = AnthropicProvider::new("sk-test");
        let tool = ToolSpec {
            name: "read_file".into(),
            description: "read a file".into(),
            input_schema: json!({ "type": "object" }),
            strict: true,
        };
        let body = p.build_body(&req_with(vec![Message::user("go")], vec![tool]), None);
        assert_eq!(body["tools"][0]["name"], "read_file");
        // strict is a SIBLING of name/description/input_schema, not on tool_choice.
        assert_eq!(body["tools"][0]["strict"], true);
        assert!(body["tools"][0]["input_schema"].is_object());
        assert_eq!(body["tool_choice"]["type"], "auto");
        assert!(body["tool_choice"].get("strict").is_none());
    }

    #[test]
    fn no_tools_omits_tools_and_tool_choice() {
        let p = AnthropicProvider::new("sk-test");
        let body = p.build_body(&req_with(vec![Message::user("hi")], vec![]), None);
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn connector_body_carries_mcp_servers_and_matching_toolset() {
        // T-6.1 AC: a remote MCP server's tools are listed/callable through a
        // Messages request; the toolset rides in `tools` next to native tools and
        // `mcp_servers` declares the server 1:1.
        use crate::mcp::connector::{McpServer, McpToolPolicy};
        let server = McpServer::new("docs", "https://mcp.example.com")
            .with_tool_policy(McpToolPolicy::allow_only(["search"]).deny(["delete"]));
        let p = AnthropicProvider::new("sk-test").with_mcp_servers(vec![server]);
        let native = ToolSpec {
            name: "read_file".into(),
            description: "read a file".into(),
            input_schema: json!({ "type": "object" }),
            strict: true,
        };
        let body = p.build_body(&req_with(vec![Message::user("go")], vec![native]), None);

        // mcp_servers declared, url-typed.
        assert_eq!(body["mcp_servers"][0]["type"], "url");
        assert_eq!(body["mcp_servers"][0]["name"], "docs");

        // The tools array holds the native tool AND the mcp_toolset.
        let tools = body["tools"].as_array().unwrap();
        assert!(tools.iter().any(|t| t["name"] == "read_file"));
        let toolset = tools
            .iter()
            .find(|t| t["type"] == "mcp_toolset")
            .expect("mcp_toolset present");
        assert_eq!(toolset["mcp_server_name"], "docs");
        // Deny-by-default: search enabled, delete disabled - "gated, not run".
        assert_eq!(toolset["default_config"]["enabled"], false);
        assert_eq!(toolset["configs"]["search"]["enabled"], true);
        assert_eq!(toolset["configs"]["delete"]["enabled"], false);

        // The assembled body passes the 1:1 invariant guard.
        assert!(crate::mcp::connector::validate_connector_body(&body).is_ok());
    }

    #[test]
    fn no_connector_omits_mcp_servers() {
        let p = AnthropicProvider::new("sk-test");
        let body = p.build_body(&req_with(vec![Message::user("hi")], vec![]), None);
        assert!(body.get("mcp_servers").is_none());
    }

    #[test]
    fn tool_result_round_trip_is_one_user_message_with_all_results() {
        // AC: the follow-up body is ONE user message carrying ALL tool_result
        // blocks; a failed tool sets is_error:true (not dropped).
        let p = AnthropicProvider::new("sk-test");
        let messages = vec![
            Message::user("list and read"),
            Message::assistant_blocks(vec![
                ContentBlock::tool_use("toolu_a", "list_dir", json!({ "path": "." })),
                ContentBlock::tool_use("toolu_b", "read_file", json!({ "path": "nope" })),
            ]),
            Message::tool_results(vec![
                ContentBlock::tool_result("toolu_a", "a.rs\nb.rs", false),
                ContentBlock::tool_result("toolu_b", "ENOENT", true),
            ]),
        ];
        let body = p.build_body(&req_with(messages, vec![]), None);
        let wire = &body["messages"];

        // The assistant tool_use blocks survive verbatim.
        assert_eq!(wire[1]["role"], "assistant");
        assert_eq!(wire[1]["content"][0]["type"], "tool_use");
        assert_eq!(wire[1]["content"][0]["id"], "toolu_a");

        // Both results ride in ONE user message.
        assert_eq!(wire[2]["role"], "user");
        let results = wire[2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r["type"] == "tool_result"));
        assert_eq!(results[0]["tool_use_id"], "toolu_a");
        assert_eq!(results[0]["is_error"], false);
        // The failed tool is is_error:true, not dropped.
        assert_eq!(results[1]["tool_use_id"], "toolu_b");
        assert_eq!(results[1]["is_error"], true);
        assert_eq!(results[1]["content"], "ENOENT");
    }

    #[test]
    fn resume_appends_assistant_content_not_a_continue_message() {
        // The resume idiom: original messages + an appended assistant message
        // holding the prior content. NO synthetic user "continue" anywhere.
        let p = AnthropicProvider::new("sk-test");
        let extra = vec![json!({ "type": "text", "text": "partial" })];
        let body = p.build_body(&req_with(vec![Message::user("go")], vec![]), Some(&extra));
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][0]["text"], "partial");
        let serialized = serde_json::to_string(&body).unwrap();
        assert!(
            !serialized.contains("continue"),
            "resume must not inject a 'continue' message: {serialized}"
        );
    }

    // ---- pure SSE framing + event mapping (byte fixtures; no network) ---------

    fn sse(events: &[(&str, Value)]) -> String {
        let mut s = String::new();
        for (name, data) in events {
            s.push_str(&format!("event: {name}\ndata: {data}\n\n"));
        }
        s
    }

    fn drain(state: &mut StreamState, decoder: &mut SseDecoder, raw: &str) -> Vec<ProviderEvent> {
        decoder
            .push(raw.as_bytes())
            .iter()
            .flat_map(|p| {
                let v: Value = serde_json::from_str(p).unwrap();
                state.accept(&v)
            })
            .collect()
    }

    #[test]
    fn text_turn_maps_to_neutral_sequence() {
        let mut state = StreamState::default();
        let mut d = SseDecoder::default();
        let raw = sse(&[
            (
                "message_start",
                json!({"type":"message_start","message":{"usage":{"input_tokens":12,"output_tokens":1}}}),
            ),
            (
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            ),
            (
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}),
            ),
            (
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}),
            ),
            (
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ),
            (
                "message_delta",
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}),
            ),
            ("message_stop", json!({"type":"message_stop"})),
        ]);
        let out = drain(&mut state, &mut d, &raw);
        assert_eq!(
            out,
            vec![
                ProviderEvent::MessageStart,
                ProviderEvent::TextDelta("Hel".into()),
                ProviderEvent::TextDelta("lo".into()),
                ProviderEvent::MessageDelta {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage {
                        input_tokens: 12,
                        output_tokens: 7,
                        ..Usage::default()
                    },
                },
                ProviderEvent::MessageStop,
            ]
        );
    }

    #[test]
    fn tool_use_turn_maps_start_input_stop_and_tool_use_reason() {
        let mut state = StreamState::default();
        let mut d = SseDecoder::default();
        let raw = sse(&[
            (
                "message_start",
                json!({"type":"message_start","message":{"usage":{"input_tokens":5}}}),
            ),
            (
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file","input":{}}}),
            ),
            (
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}),
            ),
            (
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"/etc/hosts\"}"}}),
            ),
            (
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ),
            (
                "message_delta",
                json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}),
            ),
            ("message_stop", json!({"type":"message_stop"})),
        ]);
        let out = drain(&mut state, &mut d, &raw);
        assert_eq!(
            out,
            vec![
                ProviderEvent::MessageStart,
                ProviderEvent::ToolUseStart {
                    id: "toolu_1".into(),
                    name: "read_file".into()
                },
                ProviderEvent::ToolUseInputDelta {
                    json: "{\"path\":".into()
                },
                ProviderEvent::ToolUseInputDelta {
                    json: "\"/etc/hosts\"}".into()
                },
                ProviderEvent::ToolUseStop,
                ProviderEvent::MessageDelta {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage {
                        input_tokens: 5,
                        output_tokens: 9,
                        ..Usage::default()
                    },
                },
                ProviderEvent::MessageStop,
            ]
        );
    }

    #[test]
    fn thinking_block_does_not_emit_a_stop_event_and_is_kept_for_resume() {
        let mut state = StreamState::default();
        let mut d = SseDecoder::default();
        let raw = sse(&[
            (
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            ),
            (
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"reason"}}),
            ),
            (
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig123"}}),
            ),
            (
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ),
        ]);
        let out = drain(&mut state, &mut d, &raw);
        // A thinking block emits ThinkingDelta but NO ToolUseStop on close.
        assert_eq!(out, vec![ProviderEvent::ThinkingDelta("reason".into())]);
        // For resume, the thinking block is reconstructed WITH its signature.
        let content = state.assistant_content();
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "reason");
        assert_eq!(content[0]["signature"], "sig123");
    }

    #[test]
    fn connector_mcp_blocks_map_to_neutral_events() {
        // T-6.1 AC: mcp_tool_use / mcp_tool_result blocks map into the neutral
        // stream. mcp_tool_use assembles at close (its input streams like tool_use,
        // but emits no ToolUseInputDelta); mcp_tool_result is delivered whole at
        // start with its content flattened.
        let mut state = StreamState::default();
        let mut d = SseDecoder::default();
        let raw = sse(&[
            (
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"mcp_tool_use","id":"mcp_1","name":"search","server_name":"docs","input":{}}}),
            ),
            (
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"q\":\"rust\"}"}}),
            ),
            (
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ),
            (
                "content_block_start",
                json!({"type":"content_block_start","index":1,"content_block":{"type":"mcp_tool_result","tool_use_id":"mcp_1","is_error":false,"content":[{"type":"text","text":"a doc"},{"type":"text","text":"another"}]}}),
            ),
            (
                "content_block_stop",
                json!({"type":"content_block_stop","index":1}),
            ),
        ]);
        let out = drain(&mut state, &mut d, &raw);
        assert_eq!(
            out,
            vec![
                ProviderEvent::McpToolUse {
                    id: "mcp_1".into(),
                    name: "search".into(),
                    server: "docs".into(),
                    input: json!({ "q": "rust" }),
                },
                ProviderEvent::McpToolResult {
                    id: "mcp_1".into(),
                    output: "a doc\nanother".into(),
                    is_error: false,
                },
            ]
        );
    }

    #[test]
    fn error_event_maps_to_provider_error() {
        let mut state = StreamState::default();
        let mut d = SseDecoder::default();
        let raw = sse(&[(
            "error",
            json!({"type":"error","error":{"type":"overloaded_error","message":"overloaded"}}),
        )]);
        let out = drain(&mut state, &mut d, &raw);
        assert_eq!(out, vec![ProviderEvent::Error("overloaded".into())]);
    }

    #[test]
    fn ping_events_produce_nothing() {
        let mut state = StreamState::default();
        let mut d = SseDecoder::default();
        let raw = sse(&[("ping", json!({"type":"ping"}))]);
        assert!(drain(&mut state, &mut d, &raw).is_empty());
    }

    // ---- loopback mock HTTP server (no real network egress) -------------------

    /// A blocking std-thread HTTP/1.1 server bound to loopback. It answers
    /// `responses.len()` requests in order, captures each request body, and
    /// returns the captured bodies when joined. Uses `std::net` so it needs no
    /// extra tokio features; loopback only - never a real network call.
    fn spawn_mock(responses: Vec<String>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut captured = Vec::new();
            for body in responses {
                let (mut stream, _) = listener.accept().unwrap();
                captured.push(read_request_body(&mut stream));
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(resp.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
            captured
        });
        (base, handle)
    }

    /// Read one HTTP request off the socket and return its body as a string,
    /// honoring the `Content-Length` header.
    fn read_request_body(stream: &mut std::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 2048];
        loop {
            // Have we got the full header block yet?
            if let Some(hpos) = find_subseq(&buf, b"\r\n\r\n") {
                let header_end = hpos + 4;
                let len = content_length(&buf[..hpos]);
                while buf.len() < header_end + len {
                    let n = stream.read(&mut tmp).unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let end = (header_end + len).min(buf.len());
                return String::from_utf8_lossy(&buf[header_end..end]).into_owned();
            }
            let n = stream.read(&mut tmp).unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        String::new()
    }

    fn content_length(headers: &[u8]) -> usize {
        let text = String::from_utf8_lossy(headers);
        for line in text.lines() {
            if let Some((name, value)) = line.split_once(':') {
                if name.trim().eq_ignore_ascii_case("content-length") {
                    return value.trim().parse().unwrap_or(0);
                }
            }
        }
        0
    }

    #[tokio::test]
    async fn streams_a_tool_use_response_into_the_neutral_sequence() {
        // AC: against a mocked HTTP server, a streamed tool_use response parses
        // into the correct ProviderEvent sequence.
        let body = sse(&[
            (
                "message_start",
                json!({"type":"message_start","message":{"usage":{"input_tokens":3}}}),
            ),
            (
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_9","name":"glob","input":{}}}),
            ),
            (
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"pattern\":\"*.rs\"}"}}),
            ),
            (
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ),
            (
                "message_delta",
                json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":4}}),
            ),
            ("message_stop", json!({"type":"message_stop"})),
        ]);
        let (base, server) = spawn_mock(vec![body]);
        let provider = AnthropicProvider::new("sk-test").with_base_url(base);
        let (tx, mut rx) = mpsc::channel(64);
        let req = req_with(vec![Message::user("find rust files")], vec![]);
        provider.stream_turn(req, tx).await.unwrap();

        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            got.push(ev);
        }
        let _ = server.join().unwrap();
        assert_eq!(
            got,
            vec![
                ProviderEvent::MessageStart,
                ProviderEvent::ToolUseStart {
                    id: "toolu_9".into(),
                    name: "glob".into()
                },
                ProviderEvent::ToolUseInputDelta {
                    json: "{\"pattern\":\"*.rs\"}".into()
                },
                ProviderEvent::ToolUseStop,
                ProviderEvent::MessageDelta {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage {
                        input_tokens: 3,
                        output_tokens: 4,
                        ..Usage::default()
                    },
                },
                ProviderEvent::MessageStop,
            ]
        );
    }

    #[tokio::test]
    async fn pause_turn_resumes_by_resending_without_a_continue_message() {
        // AC: pause_turn triggers a resume re-send (not a "continue" message).
        // First response: text then pause_turn. Second response: end_turn.
        let first = sse(&[
            (
                "message_start",
                json!({"type":"message_start","message":{"usage":{"input_tokens":2}}}),
            ),
            (
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            ),
            (
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"searching"}}),
            ),
            (
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ),
            (
                "message_delta",
                json!({"type":"message_delta","delta":{"stop_reason":"pause_turn"},"usage":{"output_tokens":3}}),
            ),
            ("message_stop", json!({"type":"message_stop"})),
        ]);
        let second = sse(&[
            (
                "message_start",
                json!({"type":"message_start","message":{"usage":{"input_tokens":9}}}),
            ),
            (
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            ),
            (
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" done"}}),
            ),
            (
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ),
            (
                "message_delta",
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}),
            ),
            ("message_stop", json!({"type":"message_stop"})),
        ]);
        let (base, server) = spawn_mock(vec![first, second]);
        let provider = AnthropicProvider::new("sk-test").with_base_url(base);
        let (tx, mut rx) = mpsc::channel(64);
        let req = req_with(vec![Message::user("go")], vec![]);
        provider.stream_turn(req, tx).await.unwrap();

        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            got.push(ev);
        }
        let bodies = server.join().unwrap();

        // ONE continuous turn: a single MessageStart, both text deltas, no
        // pause_turn surfaced, ending in exactly one end_turn + MessageStop.
        assert_eq!(
            got.iter()
                .filter(|e| matches!(e, ProviderEvent::MessageStart))
                .count(),
            1,
            "resumed turn must expose only one MessageStart"
        );
        assert!(got.contains(&ProviderEvent::TextDelta("searching".into())));
        assert!(got.contains(&ProviderEvent::TextDelta(" done".into())));
        assert!(!got.iter().any(|e| matches!(
            e,
            ProviderEvent::MessageDelta {
                stop_reason: StopReason::PauseTurn,
                ..
            }
        )));
        assert_eq!(
            got.last(),
            Some(&ProviderEvent::MessageStop),
            "the turn ends once, after the resume"
        );

        // Usage is folded across hops: output_tokens summed (3 + 5 = 8), input
        // taken from the FIRST hop only (2) - the resume's larger input is not
        // double-counted.
        let final_usage = got.iter().find_map(|e| match e {
            ProviderEvent::MessageDelta { usage, .. } => Some(*usage),
            _ => None,
        });
        assert_eq!(
            final_usage,
            Some(Usage {
                input_tokens: 2,
                output_tokens: 8,
                ..Usage::default()
            })
        );

        // The resume body re-sends with the assistant's content appended and NO
        // synthetic "continue" user message.
        assert_eq!(bodies.len(), 2);
        let resume: Value = serde_json::from_str(&bodies[1]).unwrap();
        let messages = resume["messages"].as_array().unwrap();
        assert_eq!(messages.last().unwrap()["role"], "assistant");
        assert_eq!(messages.last().unwrap()["content"][0]["text"], "searching");
        assert!(
            !bodies[1].contains("continue"),
            "resume must not inject a 'continue' message"
        );
    }

    #[tokio::test]
    async fn http_401_maps_to_auth_error() {
        // A non-loopback-friendly path: a server that returns 401.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_request_body(&mut stream);
            let resp =
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(resp.as_bytes()).unwrap();
            stream.flush().unwrap();
        });
        let provider = AnthropicProvider::new("bad-key").with_base_url(base);
        let (tx, _rx) = mpsc::channel(8);
        let err = provider
            .stream_turn(req_with(vec![Message::user("hi")], vec![]), tx)
            .await
            .unwrap_err();
        server.join().unwrap();
        assert!(matches!(err, ProviderError::Auth));
    }
}
