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

use super::{
    ContentBlock, LlmProvider, ProviderError, ProviderEvent, Role, StopReason, TurnRequest, Usage,
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
}

// Manual Debug so the API key never lands in logs or panic output.
impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("api_key", &"<redacted>")
            .field("base_url", &self.base_url)
            .field("max_resumes", &self.max_resumes)
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
        }
    }

    /// Override the base URL (e.g. a loopback test server or a gateway).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
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
        if !request.tools.is_empty() {
            let tools: Vec<Value> = request
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
            body["tools"] = json!(tools);
            body["tool_choice"] = json!({ "type": "auto" });
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

            let mut response = self
                .client
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("content-type", "application/json")
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

/// A minimal SSE framer: feed it raw response chunks (BYTES, as they come off the
/// socket), get back each event's concatenated `data:` payload (one `String` per
/// blank-line-delimited event). Pure and incremental.
///
/// It buffers RAW BYTES, never a per-chunk lossy decode: `reqwest`'s `chunk()`
/// splits on arbitrary transport boundaries, so a multibyte UTF-8 codepoint (in
/// streamed text or tool-input JSON) can straddle two chunks. We decode only a
/// COMPLETE event block - which always ends on the ASCII blank-line delimiter, so
/// it is a whole number of codepoints - and a partial trailing sequence stays
/// buffered as bytes until its continuation arrives. Both LF (`\n\n`) and CRLF
/// (`\r\n\r\n`) blank-line separators are recognized; `extract_data`'s
/// `str::lines()` then strips any per-line `\r`.
#[derive(Default)]
struct SseDecoder {
    buf: Vec<u8>,
}

impl SseDecoder {
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(end) = next_event_end(&self.buf) {
            let block: Vec<u8> = self.buf.drain(..end).collect();
            // The block ends on the ASCII blank-line delimiter, so it never cuts a
            // codepoint; lossy decode here can only ever be a true no-op.
            let text = String::from_utf8_lossy(&block);
            if let Some(data) = extract_data(&text) {
                out.push(data);
            }
        }
        out
    }
}

/// Index one past the end of the first complete SSE event in `buf` (including its
/// terminating blank line), recognizing both `\n\n` and `\r\n\r\n` separators.
fn next_event_end(buf: &[u8]) -> Option<usize> {
    let lf = find_subseq(buf, b"\n\n").map(|i| i + 2);
    let crlf = find_subseq(buf, b"\r\n\r\n").map(|i| i + 4);
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

fn find_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Pull the (possibly multi-line) `data:` payload out of one SSE event block.
fn extract_data(block: &str) -> Option<String> {
    let mut data = String::new();
    let mut found = false;
    for line in block.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            if found {
                data.push('\n');
            }
            // A single optional leading space after the colon is part of the
            // framing, not the payload.
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            found = true;
        }
    }
    found.then_some(data)
}

/// Which kind of content block is open at a given index (so `content_block_stop`
/// knows whether to emit a [`ProviderEvent::ToolUseStop`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    Thinking,
    ToolUse,
    Other,
}

/// Per-block accumulation, retained after the block closes so the full assistant
/// `content` can be reconstructed for a `pause_turn` resume.
struct BlockAccum {
    kind: BlockKind,
    id: String,
    name: String,
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
                self.blocks.insert(
                    index,
                    BlockAccum {
                        kind,
                        id: id.clone(),
                        name: name.clone(),
                        text: String::new(),
                        signature: String::new(),
                        json: String::new(),
                    },
                );
                if kind == BlockKind::ToolUse {
                    vec![ProviderEvent::ToolUseStart { id, name }]
                } else {
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
                        if let Some(b) = self.blocks.get_mut(&index) {
                            b.json.push_str(&j);
                        }
                        vec![ProviderEvent::ToolUseInputDelta { json: j }]
                    }
                    _ => Vec::new(),
                }
            }
            Some("content_block_stop") => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                if self.blocks.get(&index).map(|b| b.kind) == Some(BlockKind::ToolUse) {
                    vec![ProviderEvent::ToolUseStop]
                } else {
                    Vec::new()
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
                BlockKind::Other => None,
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

#[cfg(test)]
mod tests {
    use super::*;
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
    fn decoder_reassembles_an_event_split_across_chunks() {
        let mut d = SseDecoder::default();
        assert!(d.push(b"event: message_st").is_empty());
        assert!(d.push(b"art\ndata: {\"type\":\"mess").is_empty());
        let out = d.push(b"age_start\"}\n\n");
        assert_eq!(out, vec!["{\"type\":\"message_start\"}".to_string()]);
    }

    #[test]
    fn decoder_handles_crlf_and_multiple_events_in_one_chunk() {
        let mut d = SseDecoder::default();
        let raw = "event: ping\r\ndata: {\"type\":\"ping\"}\r\n\r\nevent: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n";
        let out = d.push(raw.as_bytes());
        assert_eq!(out.len(), 2);
        assert_eq!(out[1], "{\"type\":\"message_stop\"}");
    }

    #[test]
    fn decoder_preserves_a_multibyte_codepoint_split_across_chunks() {
        // A delta carrying a non-ASCII char ("é" = 0xC3 0xA9) whose bytes are torn
        // across two `chunk()` boundaries must NOT be corrupted into U+FFFD. This
        // is the case a per-chunk from_utf8_lossy silently destroyed.
        let payload = "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"café\"}}\n\n";
        let bytes = payload.as_bytes();
        // Find a split point INSIDE the 'é' (its first byte 0xC3).
        let split = bytes.iter().position(|&b| b == 0xC3).unwrap() + 1;
        let mut d = SseDecoder::default();
        assert!(
            d.push(&bytes[..split]).is_empty(),
            "partial codepoint must stay buffered"
        );
        let out = d.push(&bytes[split..]);
        assert_eq!(out.len(), 1);
        let v: Value = serde_json::from_str(&out[0]).unwrap();
        assert_eq!(v["delta"]["text"], "café");
        assert!(
            !out[0].contains('\u{FFFD}'),
            "no replacement char: {}",
            out[0]
        );
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
