//! The OpenAI Responses-API client (T-5.3) - aterm's SECOND provider, making the
//! multi-provider seam real in v1.
//!
//! A thin, hand-rolled typed client over `POST /v1/responses` with `stream:true`:
//! it serializes a provider-neutral [`TurnRequest`](super::TurnRequest) into the
//! Responses wire shape and translates the Responses SSE event stream into the
//! SAME provider-neutral [`ProviderEvent`](super::ProviderEvent) sequence the
//! shared [`AgentEventMapper`](super::AgentEventMapper) folds - so the turn loop
//! (T-5.8) drives it with no provider-specific branching.
//!
//! Mapping notes (the Responses API differs from the Messages API in shape, not
//! in the neutral concepts):
//!
//! - Request: the neutral system prompt -> top-level `instructions`; the message
//!   list -> the `input` array of typed items (`input_text` for user/operator,
//!   `output_text` for echoed assistant text, `function_call` for an echoed tool
//!   call, `function_call_output` for each tool result keyed by `call_id`);
//!   `max_tokens` -> `max_output_tokens`; the effort knob -> `reasoning.effort`
//!   (OpenAI's analog of `output_config.effort` - NEVER `budget_tokens`). Tools
//!   are FLAT function defs (`type`/`name`/`description`/`parameters`/`strict`),
//!   not nested under a `function` key as in Chat Completions.
//! - `store:false`: we own the loop and re-send the full transcript each turn
//!   (mirrors the Anthropic client), so the server keeps no conversation state
//!   and tool calls thread purely by `call_id`.
//! - Stream: `response.created` -> `MessageStart`; `response.output_text.delta`
//!   -> `TextDelta`; `response.reasoning_summary_text.delta` -> `ThinkingDelta`
//!   (summarized reasoning is the closest neutral analog of summarized thinking);
//!   a `function_call` output item -> `ToolUseStart` / `ToolUseInputDelta` /
//!   `ToolUseStop`; `response.completed` / `response.incomplete` -> one
//!   `MessageDelta` (stop reason + usage) then `MessageStop`. Crucially, a turn
//!   that emitted ANY function call maps to [`StopReason::ToolUse`] regardless of
//!   the `completed` status string - the Responses API signals a tool turn via
//!   output items, not a stop string.
//!
//! The Responses API has no `pause_turn` analog in the streaming flow (a turn
//! `completes` or is `incomplete`), so - unlike the Anthropic client - there is no
//! resume loop. Reasoning-item replay across tool rounds is the same accepted
//! scope cut as the Anthropic thinking-signature cut: the neutral event stream
//! carries no reasoning-item id, so reasoning items are not threaded back on
//! re-send.
//!
//! The SSE decoding reuses the shared [`SseDecoder`](super::sse::SseDecoder); the
//! event-mapping ([`StreamState`]) and request-body builder are pure and
//! headless-testable from byte fixtures, and the HTTP path is exercised against a
//! loopback mock server (both with NO real network).

use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::sse::SseDecoder;
use super::{
    ContentBlock, Effort, LlmProvider, Message, ProviderError, ProviderEvent, Role, StopReason,
    TurnRequest, Usage,
};

/// The production Responses-API base URL.
const DEFAULT_BASE_URL: &str = "https://api.openai.com";

/// The OpenAI Responses-API provider. Holds the API key, an HTTP client, and the
/// base URL (overridable for tests / proxies / gateway deployments).
#[derive(Clone)]
pub struct OpenAiProvider {
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

// Manual Debug so the API key never lands in logs or panic output.
impl std::fmt::Debug for OpenAiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiProvider")
            .field("api_key", &"<redacted>")
            .field("base_url", &self.base_url)
            .finish()
    }
}

impl OpenAiProvider {
    /// Build a provider with a key (custody is T-8.3; this just holds it).
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            client: reqwest::Client::new(),
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

    /// Serialize a neutral [`TurnRequest`] into the Responses-API JSON body.
    fn build_body(&self, request: &TurnRequest) -> Value {
        let mut body = json!({
            "model": request.model,
            "max_output_tokens": request.max_tokens,
            "stream": true,
            "store": false,
            "input": wire_input_items(&request.messages),
            // Depth is the effort knob, NOT budget_tokens; `summary:"auto"` asks
            // for summarized reasoning so the timeline can render thinking.
            "reasoning": {
                "effort": openai_effort(request.effort),
                "summary": "auto",
            },
        });

        if let Some(system) = &request.system {
            body["instructions"] = json!(system);
        }
        if !request.tools.is_empty() {
            let tools: Vec<Value> = request
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                        "strict": t.strict,
                    })
                })
                .collect();
            body["tools"] = json!(tools);
            body["tool_choice"] = json!("auto");
        }
        body
    }
}

/// Map the neutral effort knob to OpenAI's `reasoning.effort`. The documented
/// Responses set tops out at `xhigh` (not `high`), so `Effort::Xhigh` passes
/// through 1:1; the neutral `Effort::Max` (no OpenAI analog) clamps to that
/// documented ceiling. Per-model validity of a given level is a config concern
/// (T-8.3), not this mapping's.
fn openai_effort(effort: Effort) -> &'static str {
    match effort {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::Xhigh | Effort::Max => "xhigh",
    }
}

/// Map the neutral [`Message`] list to the Responses-API `input` array. A single
/// assistant message carrying both text and `tool_use` blocks expands to a text
/// message item PLUS one `function_call` item per tool call; a tool-results
/// message expands to one `function_call_output` item per result (the Responses
/// API has no "one message, many results" shape - results are flat input items).
fn wire_input_items(messages: &[Message]) -> Vec<Value> {
    let mut items = Vec::new();
    for message in messages {
        match message.role {
            Role::User => items.push(role_message("user", "input_text", &message.content)),
            // The inline operator channel maps to the `developer` role (OpenAI's
            // current high-priority instruction role for reasoning models).
            Role::System => items.push(role_message("developer", "input_text", &message.content)),
            Role::Assistant => {
                let mut text_parts = Vec::new();
                let mut calls = Vec::new();
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } => {
                            text_parts.push(json!({ "type": "output_text", "text": text }));
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            calls.push(json!({
                                "type": "function_call",
                                "call_id": id,
                                "name": name,
                                // The Responses API wants arguments as a STRING.
                                "arguments": serde_json::to_string(input)
                                    .unwrap_or_else(|_| "{}".to_string()),
                            }));
                        }
                        // A tool_result is never valid inside an assistant message.
                        ContentBlock::ToolResult { .. } => {}
                    }
                }
                if !text_parts.is_empty() {
                    items.push(json!({ "role": "assistant", "content": text_parts }));
                }
                items.extend(calls);
            }
            Role::Tool => {
                for block in &message.content {
                    if let ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } = block
                    {
                        // The Responses `function_call_output` has no is_error
                        // field; the turn loop folds the error text into `content`.
                        items.push(json!({
                            "type": "function_call_output",
                            "call_id": tool_use_id,
                            "output": content,
                        }));
                    }
                }
            }
        }
    }
    items
}

/// Build one input message item from the `Text` blocks of a neutral message.
/// `text_type` is `input_text` for user/operator messages and `output_text` for
/// echoed assistant text.
fn role_message(role: &str, text_type: &str, content: &[ContentBlock]) -> Value {
    let parts: Vec<Value> = content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(json!({ "type": text_type, "text": text })),
            _ => None,
        })
        .collect();
    json!({ "role": role, "content": parts })
}

impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn default_model(&self) -> &'static str {
        "gpt-5"
    }

    async fn stream_turn(
        &self,
        request: TurnRequest,
        sink: mpsc::Sender<ProviderEvent>,
    ) -> Result<(), ProviderError> {
        let url = format!("{}/v1/responses", self.base_url);
        let body = self.build_body(&request);

        let mut response = self
            .client
            .post(&url)
            .header("authorization", format!("Bearer {}", self.api_key))
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

        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| ProviderError::Http(e.to_string()))?
        {
            for payload in decoder.push(&chunk) {
                // The Responses API ends with `response.completed`, not a `[DONE]`
                // sentinel; guard against a gateway that injects one anyway (it is
                // not valid JSON and would otherwise be a spurious decode error).
                if payload.trim() == "[DONE]" {
                    continue;
                }
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
                    if sink.send(event).await.is_err() {
                        // Receiver dropped (turn aborted/cancelled).
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }
}

/// Stateful translator from decoded Responses SSE event JSON to the neutral
/// [`ProviderEvent`] sequence. Pure: tests drive it with `serde_json::Value`
/// fixtures, no network. Tracks the final usage plus two terminal-classification
/// signals: whether a function call fully CLOSED (a `function_call_arguments.done`
/// arrived - a usable tool call, distinct from one truncated mid-arguments) and
/// whether the model authored a refusal.
#[derive(Default)]
struct StreamState {
    usage: Usage,
    saw_completed_tool_call: bool,
    saw_refusal: bool,
}

impl StreamState {
    fn accept(&mut self, value: &Value) -> Vec<ProviderEvent> {
        match value.get("type").and_then(Value::as_str) {
            Some("response.created") => vec![ProviderEvent::MessageStart],
            Some("response.output_text.delta") => {
                vec![ProviderEvent::TextDelta(delta_str(value))]
            }
            Some("response.reasoning_summary_text.delta") => {
                vec![ProviderEvent::ThinkingDelta(delta_str(value))]
            }
            Some("response.output_item.added") => {
                let item = value.get("item");
                let itype = item
                    .and_then(|i| i.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if itype == "function_call" {
                    let id = str_at(item, "call_id");
                    let name = str_at(item, "name");
                    vec![ProviderEvent::ToolUseStart { id, name }]
                } else {
                    // `message` / `reasoning` items: their content streams via the
                    // dedicated text / reasoning delta events.
                    Vec::new()
                }
            }
            Some("response.function_call_arguments.delta") => {
                vec![ProviderEvent::ToolUseInputDelta {
                    json: delta_str(value),
                }]
            }
            Some("response.function_call_arguments.done") => {
                // The tool call's arguments are complete: a USABLE call (vs one
                // truncated mid-arguments, which never reaches `.done`).
                self.saw_completed_tool_call = true;
                vec![ProviderEvent::ToolUseStop]
            }
            // An authored content-policy refusal: the text streams here and the
            // turn still ends with `status:"completed"`, so the terminal status
            // string alone can't reveal it. Surface the prose as assistant text
            // (parity with the Anthropic refusal path) and flag the turn.
            Some("response.refusal.delta") => {
                self.saw_refusal = true;
                vec![ProviderEvent::TextDelta(delta_str(value))]
            }
            Some("response.refusal.done") => {
                self.saw_refusal = true;
                Vec::new()
            }
            Some("response.completed") | Some("response.incomplete") => {
                let resp = value.get("response");
                if let Some(u) = resp.and_then(|r| r.get("usage")) {
                    self.usage = parse_usage(u);
                }
                vec![
                    ProviderEvent::MessageDelta {
                        stop_reason: self.terminal_reason(resp),
                        usage: self.usage,
                    },
                    ProviderEvent::MessageStop,
                ]
            }
            Some("response.failed") => {
                let msg = value
                    .pointer("/response/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("response failed")
                    .to_string();
                vec![ProviderEvent::Error(msg)]
            }
            Some("error") => {
                let msg = value
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown provider error")
                    .to_string();
                vec![ProviderEvent::Error(msg)]
            }
            // `response.in_progress`, `response.output_item.done`,
            // `response.content_part.*`, `response.output_text.done`, etc. carry no
            // neutral information beyond what the deltas + terminal event already do.
            _ => Vec::new(),
        }
    }

    /// The neutral stop reason for a terminal event, in precedence order:
    ///
    /// 1. A model refusal is a definitive safety outcome - it must never be
    ///    masked as an ordinary end-turn (the `completed` status hides it).
    /// 2. A turn with at least one FULLY-CLOSED function call is a tool turn even
    ///    if a LATER output item then hit the token cap (`response.incomplete`):
    ///    the completed call is usable and must still be gated + run; any trailing
    ///    truncated call surfaces downstream as a malformed-args `is_error`.
    /// 3. Otherwise an `incomplete` turn reports its own reason (e.g.
    ///    `max_output_tokens` - a turn truncated with no usable tool call).
    /// 4. Otherwise map the terminal `status` string.
    fn terminal_reason(&self, resp: Option<&Value>) -> StopReason {
        if self.saw_refusal {
            return StopReason::Refusal;
        }
        if self.saw_completed_tool_call {
            return StopReason::ToolUse;
        }
        if let Some(reason) = resp
            .and_then(|r| r.get("incomplete_details"))
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str)
        {
            return StopReason::from_openai(reason);
        }
        let status = resp
            .and_then(|r| r.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("completed");
        StopReason::from_openai(status)
    }
}

/// The `delta` string field of a Responses streaming delta event.
fn delta_str(value: &Value) -> String {
    value
        .get("delta")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// A string field of an optional object (e.g. the streamed `item`).
fn str_at(obj: Option<&Value>, key: &str) -> String {
    obj.and_then(|o| o.get(key))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Map a Responses `usage` object to the neutral [`Usage`]. OpenAI reports cached
/// input under `input_tokens_details.cached_tokens`; it has no cache-CREATION
/// counter, so that neutral field stays `0`.
fn parse_usage(u: &Value) -> Usage {
    Usage {
        input_tokens: u_u32(u, "input_tokens"),
        output_tokens: u_u32(u, "output_tokens"),
        cache_read_input_tokens: u
            .get("input_tokens_details")
            .map(|d| u_u32(d, "cached_tokens"))
            .unwrap_or(0),
        cache_creation_input_tokens: 0,
    }
}

fn u_u32(obj: &Value, key: &str) -> u32 {
    obj.get(key)
        .and_then(Value::as_u64)
        .map(|n| n.min(u64::from(u32::MAX)) as u32)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::sse::find_subseq;
    use crate::provider::ToolSpec;
    use std::io::{Read, Write};

    // ---- request-body shape (pure; no network) -------------------------------

    fn req_with(messages: Vec<Message>, tools: Vec<ToolSpec>) -> TurnRequest {
        TurnRequest {
            model: "gpt-5".to_string(),
            system: Some("be terse".to_string()),
            messages,
            tools,
            effort: Effort::High,
            max_tokens: 4096,
        }
    }

    #[test]
    fn body_sets_responses_fields_reasoning_effort_and_never_budget_tokens() {
        let p = OpenAiProvider::new("sk-test");
        let body = p.build_body(&req_with(vec![Message::user("hi")], vec![]));
        assert_eq!(body["model"], "gpt-5");
        assert_eq!(body["max_output_tokens"], 4096);
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["reasoning"]["summary"], "auto");
        // The system prompt is `instructions`, not an input message.
        assert_eq!(body["instructions"], "be terse");
        // The neutral effort knob must NEVER serialize as budget_tokens.
        let serialized = serde_json::to_string(&body).unwrap();
        assert!(
            !serialized.contains("budget_tokens"),
            "budget_tokens must never be sent: {serialized}"
        );
        // The user message rides in the input array as input_text.
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["input"][0]["content"][0]["text"], "hi");
    }

    #[test]
    fn effort_levels_map_to_the_documented_responses_range() {
        assert_eq!(openai_effort(Effort::Low), "low");
        assert_eq!(openai_effort(Effort::Medium), "medium");
        assert_eq!(openai_effort(Effort::High), "high");
        // `xhigh` is the documented Responses maximum (NOT `high`): Xhigh passes
        // through 1:1, and the analog-less neutral Max clamps to that ceiling.
        assert_eq!(openai_effort(Effort::Xhigh), "xhigh");
        assert_eq!(openai_effort(Effort::Max), "xhigh");
    }

    #[test]
    fn tools_serialize_as_flat_function_defs_with_strict_and_auto_choice() {
        let p = OpenAiProvider::new("sk-test");
        let tool = ToolSpec {
            name: "read_file".into(),
            description: "read a file".into(),
            input_schema: json!({ "type": "object" }),
            strict: true,
        };
        let body = p.build_body(&req_with(vec![Message::user("go")], vec![tool]));
        // Responses function tools are FLAT (not nested under a `function` key).
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "read_file");
        assert_eq!(body["tools"][0]["description"], "read a file");
        assert!(body["tools"][0]["parameters"].is_object());
        assert_eq!(body["tools"][0]["strict"], true);
        assert!(body["tools"][0].get("function").is_none());
        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn no_tools_omits_tools_and_tool_choice() {
        let p = OpenAiProvider::new("sk-test");
        let body = p.build_body(&req_with(vec![Message::user("hi")], vec![]));
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn tool_result_continuation_maps_to_function_call_and_output_items() {
        // AC: a tool-result continuation produces a correctly-shaped follow-up
        // request. The assistant tool_use -> a `function_call` input item keyed by
        // call_id with arguments as a STRING; each result -> its own
        // `function_call_output` item keyed by the same call_id.
        let p = OpenAiProvider::new("sk-test");
        let messages = vec![
            Message::user("list and read"),
            Message::assistant_blocks(vec![
                ContentBlock::tool_use("call_a", "list_dir", json!({ "path": "." })),
                ContentBlock::tool_use("call_b", "read_file", json!({ "path": "nope" })),
            ]),
            Message::tool_results(vec![
                ContentBlock::tool_result("call_a", "a.rs\nb.rs", false),
                ContentBlock::tool_result("call_b", "ENOENT", true),
            ]),
        ];
        let body = p.build_body(&req_with(messages, vec![]));
        let input = body["input"].as_array().unwrap();

        // [0] user message, [1] function_call(call_a), [2] function_call(call_b),
        // [3] function_call_output(call_a), [4] function_call_output(call_b).
        assert_eq!(input.len(), 5);
        assert_eq!(input[0]["role"], "user");

        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["call_id"], "call_a");
        assert_eq!(input[1]["name"], "list_dir");
        // Arguments are a serialized JSON STRING, not an object.
        assert!(input[1]["arguments"].is_string());
        let args: Value = serde_json::from_str(input[1]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["path"], ".");

        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "call_b");

        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["call_id"], "call_a");
        assert_eq!(input[3]["output"], "a.rs\nb.rs");
        assert_eq!(input[4]["type"], "function_call_output");
        assert_eq!(input[4]["call_id"], "call_b");
        // The failed tool's error text rides in `output` (no is_error wire field).
        assert_eq!(input[4]["output"], "ENOENT");
    }

    #[test]
    fn assistant_text_and_tool_calls_split_into_separate_items() {
        let p = OpenAiProvider::new("sk-test");
        let messages = vec![Message::assistant_blocks(vec![
            ContentBlock::text("let me look"),
            ContentBlock::tool_use("call_1", "glob", json!({ "pattern": "*.rs" })),
        ])];
        let body = p.build_body(&req_with(messages, vec![]));
        let input = body["input"].as_array().unwrap();
        // One assistant message item (output_text) + one function_call item.
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["role"], "assistant");
        assert_eq!(input[0]["content"][0]["type"], "output_text");
        assert_eq!(input[0]["content"][0]["text"], "let me look");
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["name"], "glob");
    }

    #[test]
    fn operator_message_maps_to_a_developer_role_input_item() {
        let p = OpenAiProvider::new("sk-test");
        let body = p.build_body(&req_with(
            vec![Message::system("operator says hi"), Message::user("go")],
            vec![],
        ));
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], "developer");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[0]["content"][0]["text"], "operator says hi");
        assert_eq!(input[1]["role"], "user");
    }

    #[test]
    fn no_system_omits_instructions() {
        let p = OpenAiProvider::new("sk-test");
        let req = TurnRequest {
            model: "gpt-5".to_string(),
            system: None,
            messages: vec![Message::user("hi")],
            tools: vec![],
            effort: Effort::Medium,
            max_tokens: 256,
        };
        let body = p.build_body(&req);
        assert!(body.get("instructions").is_none());
    }

    // ---- pure SSE event mapping (byte fixtures; no network) -------------------

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
            ("response.created", json!({"type":"response.created"})),
            (
                "response.output_text.delta",
                json!({"type":"response.output_text.delta","delta":"Hel"}),
            ),
            (
                "response.output_text.delta",
                json!({"type":"response.output_text.delta","delta":"lo"}),
            ),
            (
                "response.completed",
                json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":12,"output_tokens":7}}}),
            ),
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
    fn tool_call_turn_maps_to_tool_use_sequence_and_tool_use_reason() {
        // The headline mapping nuance: status is "completed" but a function call
        // was emitted, so the neutral stop reason is ToolUse (the Responses API
        // signals a tool turn via output items, not a stop string).
        let mut state = StreamState::default();
        let mut d = SseDecoder::default();
        let raw = sse(&[
            ("response.created", json!({"type":"response.created"})),
            (
                "response.output_item.added",
                json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"fc_1","call_id":"call_9","name":"glob","arguments":""}}),
            ),
            (
                "response.function_call_arguments.delta",
                json!({"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"pattern\":"}),
            ),
            (
                "response.function_call_arguments.delta",
                json!({"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"\"*.rs\"}"}),
            ),
            (
                "response.function_call_arguments.done",
                json!({"type":"response.function_call_arguments.done","item_id":"fc_1","arguments":"{\"pattern\":\"*.rs\"}"}),
            ),
            (
                "response.completed",
                json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":5,"output_tokens":9}}}),
            ),
        ]);
        let out = drain(&mut state, &mut d, &raw);
        assert_eq!(
            out,
            vec![
                ProviderEvent::MessageStart,
                ProviderEvent::ToolUseStart {
                    id: "call_9".into(),
                    name: "glob".into(),
                },
                ProviderEvent::ToolUseInputDelta {
                    json: "{\"pattern\":".into(),
                },
                ProviderEvent::ToolUseInputDelta {
                    json: "\"*.rs\"}".into(),
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
    fn reasoning_summary_maps_to_thinking_delta() {
        let mut state = StreamState::default();
        let mut d = SseDecoder::default();
        let raw = sse(&[
            (
                "response.reasoning_summary_text.delta",
                json!({"type":"response.reasoning_summary_text.delta","delta":"considering"}),
            ),
            (
                "response.completed",
                json!({"type":"response.completed","response":{"status":"completed"}}),
            ),
        ]);
        let out = drain(&mut state, &mut d, &raw);
        assert_eq!(out[0], ProviderEvent::ThinkingDelta("considering".into()));
        assert_eq!(
            out.last(),
            Some(&ProviderEvent::MessageStop),
            "turn still terminates"
        );
    }

    #[test]
    fn incomplete_max_output_tokens_maps_to_max_tokens() {
        let mut state = StreamState::default();
        let mut d = SseDecoder::default();
        let raw = sse(&[(
            "response.incomplete",
            json!({"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":3,"output_tokens":256}}}),
        )]);
        let out = drain(&mut state, &mut d, &raw);
        assert_eq!(
            out,
            vec![
                ProviderEvent::MessageDelta {
                    stop_reason: StopReason::MaxTokens,
                    usage: Usage {
                        input_tokens: 3,
                        output_tokens: 256,
                        ..Usage::default()
                    },
                },
                ProviderEvent::MessageStop,
            ]
        );
    }

    #[test]
    fn incomplete_reason_wins_over_a_partial_tool_call() {
        // A turn truncated by the token cap mid-tool-call reports max_tokens, NOT
        // ToolUse - the truncated arguments are not a usable tool call.
        let mut state = StreamState::default();
        let mut d = SseDecoder::default();
        let raw = sse(&[
            (
                "response.output_item.added",
                json!({"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_x","name":"run_command","arguments":""}}),
            ),
            (
                "response.function_call_arguments.delta",
                json!({"type":"response.function_call_arguments.delta","delta":"{\"command\":[\"l"}),
            ),
            (
                "response.incomplete",
                json!({"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}}),
            ),
        ]);
        let out = drain(&mut state, &mut d, &raw);
        let reason = out.iter().find_map(|e| match e {
            ProviderEvent::MessageDelta { stop_reason, .. } => Some(stop_reason.clone()),
            _ => None,
        });
        assert_eq!(reason, Some(StopReason::MaxTokens));
    }

    #[test]
    fn completed_tool_call_then_incomplete_still_maps_to_tool_use() {
        // A function call CLOSES (`.done`), then a later item hits the token cap.
        // The completed call is usable, so the turn must report ToolUse (not
        // MaxTokens) - otherwise the turn loop would drop the valid call.
        let mut state = StreamState::default();
        let mut d = SseDecoder::default();
        let raw = sse(&[
            (
                "response.output_item.added",
                json!({"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_ok","name":"read_file","arguments":""}}),
            ),
            (
                "response.function_call_arguments.delta",
                json!({"type":"response.function_call_arguments.delta","delta":"{\"path\":\"a\"}"}),
            ),
            (
                "response.function_call_arguments.done",
                json!({"type":"response.function_call_arguments.done","arguments":"{\"path\":\"a\"}"}),
            ),
            (
                "response.incomplete",
                json!({"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}}),
            ),
        ]);
        let out = drain(&mut state, &mut d, &raw);
        // The completed call's neutral sequence is intact...
        assert!(out.contains(&ProviderEvent::ToolUseStop));
        // ...and the terminal reason is ToolUse, so the loop runs the call.
        let reason = out.iter().find_map(|e| match e {
            ProviderEvent::MessageDelta { stop_reason, .. } => Some(stop_reason.clone()),
            _ => None,
        });
        assert_eq!(reason, Some(StopReason::ToolUse));
    }

    #[test]
    fn authored_refusal_maps_to_refusal_reason() {
        // A content-policy refusal streams as response.refusal.delta and the turn
        // still ends `completed`; it must surface as StopReason::Refusal (parity
        // with the Anthropic path), not EndTurn, and the prose renders as text.
        let mut state = StreamState::default();
        let mut d = SseDecoder::default();
        let raw = sse(&[
            ("response.created", json!({"type":"response.created"})),
            (
                "response.refusal.delta",
                json!({"type":"response.refusal.delta","delta":"I can't help with that."}),
            ),
            (
                "response.refusal.done",
                json!({"type":"response.refusal.done","refusal":"I can't help with that."}),
            ),
            (
                "response.completed",
                json!({"type":"response.completed","response":{"status":"completed"}}),
            ),
        ]);
        let out = drain(&mut state, &mut d, &raw);
        assert!(out.contains(&ProviderEvent::TextDelta("I can't help with that.".into())));
        let reason = out.iter().find_map(|e| match e {
            ProviderEvent::MessageDelta { stop_reason, .. } => Some(stop_reason.clone()),
            _ => None,
        });
        assert_eq!(reason, Some(StopReason::Refusal));
        assert_eq!(out.last(), Some(&ProviderEvent::MessageStop));
    }

    #[test]
    fn cached_input_tokens_map_to_cache_read() {
        let mut state = StreamState::default();
        let mut d = SseDecoder::default();
        let raw = sse(&[(
            "response.completed",
            json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":100,"output_tokens":20,"input_tokens_details":{"cached_tokens":80}}}}),
        )]);
        let out = drain(&mut state, &mut d, &raw);
        let usage = out.iter().find_map(|e| match e {
            ProviderEvent::MessageDelta { usage, .. } => Some(*usage),
            _ => None,
        });
        assert_eq!(
            usage,
            Some(Usage {
                input_tokens: 100,
                output_tokens: 20,
                cache_read_input_tokens: 80,
                cache_creation_input_tokens: 0,
            })
        );
    }

    #[test]
    fn response_failed_maps_to_error() {
        let mut state = StreamState::default();
        let mut d = SseDecoder::default();
        let raw = sse(&[(
            "response.failed",
            json!({"type":"response.failed","response":{"status":"failed","error":{"code":"server_error","message":"boom"}}}),
        )]);
        let out = drain(&mut state, &mut d, &raw);
        assert_eq!(out, vec![ProviderEvent::Error("boom".into())]);
    }

    #[test]
    fn top_level_error_event_maps_to_error() {
        let mut state = StreamState::default();
        let mut d = SseDecoder::default();
        let raw = sse(&[(
            "error",
            json!({"type":"error","code":"rate_limit","message":"slow down"}),
        )]);
        let out = drain(&mut state, &mut d, &raw);
        assert_eq!(out, vec![ProviderEvent::Error("slow down".into())]);
    }

    #[test]
    fn unknown_and_noise_events_produce_nothing() {
        let mut state = StreamState::default();
        let mut d = SseDecoder::default();
        let raw = sse(&[
            (
                "response.in_progress",
                json!({"type":"response.in_progress"}),
            ),
            (
                "response.output_text.done",
                json!({"type":"response.output_text.done","text":"Hello"}),
            ),
            (
                "response.brand_new_event",
                json!({"type":"response.brand_new_event"}),
            ),
        ]);
        assert!(drain(&mut state, &mut d, &raw).is_empty());
    }

    // ---- loopback mock HTTP server (no real network egress) -------------------

    /// A blocking std-thread HTTP/1.1 server bound to loopback. Answers
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

    fn read_request_body(stream: &mut std::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 2048];
        loop {
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
        // AC: against a mocked HTTP server, a streamed Responses reply with a tool
        // call parses into the correct ProviderEvent sequence.
        let body = sse(&[
            ("response.created", json!({"type":"response.created"})),
            (
                "response.output_item.added",
                json!({"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_42","name":"read_file","arguments":""}}),
            ),
            (
                "response.function_call_arguments.delta",
                json!({"type":"response.function_call_arguments.delta","delta":"{\"path\":\"/etc/hosts\"}"}),
            ),
            (
                "response.function_call_arguments.done",
                json!({"type":"response.function_call_arguments.done","arguments":"{\"path\":\"/etc/hosts\"}"}),
            ),
            (
                "response.completed",
                json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":4,"output_tokens":6}}}),
            ),
        ]);
        let (base, server) = spawn_mock(vec![body]);
        let provider = OpenAiProvider::new("sk-test").with_base_url(base);
        let (tx, mut rx) = mpsc::channel(64);
        let req = req_with(vec![Message::user("read the hosts file")], vec![]);
        provider.stream_turn(req, tx).await.unwrap();

        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            got.push(ev);
        }
        let bodies = server.join().unwrap();

        assert_eq!(
            got,
            vec![
                ProviderEvent::MessageStart,
                ProviderEvent::ToolUseStart {
                    id: "call_42".into(),
                    name: "read_file".into(),
                },
                ProviderEvent::ToolUseInputDelta {
                    json: "{\"path\":\"/etc/hosts\"}".into(),
                },
                ProviderEvent::ToolUseStop,
                ProviderEvent::MessageDelta {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage {
                        input_tokens: 4,
                        output_tokens: 6,
                        ..Usage::default()
                    },
                },
                ProviderEvent::MessageStop,
            ]
        );

        // The request actually hit /v1/responses with a stream:true body.
        assert_eq!(bodies.len(), 1);
        let sent: Value = serde_json::from_str(&bodies[0]).unwrap();
        assert_eq!(sent["stream"], true);
        assert_eq!(sent["model"], "gpt-5");
    }

    #[tokio::test]
    async fn http_401_maps_to_auth_error() {
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
        let provider = OpenAiProvider::new("bad-key").with_base_url(base);
        let (tx, _rx) = mpsc::channel(8);
        let err = provider
            .stream_turn(req_with(vec![Message::user("hi")], vec![]), tx)
            .await
            .unwrap_err();
        server.join().unwrap();
        assert!(matches!(err, ProviderError::Auth));
    }

    #[test]
    fn identity_is_openai_gpt5() {
        let p = OpenAiProvider::new("sk-test");
        assert_eq!(p.name(), "openai");
        assert_eq!(p.default_model(), "gpt-5");
    }
}
