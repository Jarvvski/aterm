//! `LlmProvider` trait + the provider-neutral streaming event model (T-5.1).
//!
//! Two layers sit between a raw provider SSE stream and the timeline:
//!
//! 1. [`ProviderEvent`] - a low-level, provider-NEUTRAL event mirroring an
//!    Anthropic Messages-API SSE stream (and the OpenAI Responses stream)
//!    one-to-one. Each concrete provider (T-5.2 Anthropic, T-5.3 OpenAI) owns
//!    the SSE -> `ProviderEvent` translation; nothing provider-specific leaks
//!    past this boundary.
//! 2. [`AgentEvent`] - the high-level, timeline-facing event the UI renders.
//!    [`AgentEventMapper`] is the shared, provider-neutral reducer that folds a
//!    `ProviderEvent` stream into `AgentEvent`s: it buffers each tool call's
//!    streamed input JSON and emits one complete [`AgentEvent::ToolProposed`]
//!    per call.
//!
//! The real HTTP clients are T-5.2/T-5.3; here [`AnthropicProvider`] and
//! [`OpenAiProvider`] are compiling stubs that return `NotImplemented` and make
//! NO network calls, plus a scriptable [`MockProvider`] the turn loop and tests
//! drive.
//!
//! Divergences from the Kotlin prototype (deliberate, per aterm's locked
//! decisions): the prototype models a SINGLE tool (`propose_command` ->
//! `CommandProposal`); aterm is locked MULTI-TOOL, so the mapper stays generic
//! ([`ToolCall`] = `{id, name, input}`) and does not hardcode a tool name.
//! Thinking deltas are modeled here (the prototype dropped them) because the
//! T-5.1 acceptance criteria require the timeline to render thinking.

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// One message in a conversation, provider-neutral.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    /// Result of a tool the model requested.
    Tool,
}

/// A tool the model may call (provider-neutral schema).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's input.
    pub input_schema: serde_json::Value,
}

/// A fully-assembled tool call the model emitted: a stable id, the tool name,
/// and the reassembled+parsed input. Provider-neutral and tool-neutral.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// Adaptive-thinking effort knob. Maps to each provider's own control - on the
/// Anthropic Messages API this is `output_config.effort` (NOT `budget_tokens`,
/// which is rejected on `claude-opus-4-8`). Levels mirror the Opus 4.8 set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// Token accounting for one turn, provider-neutral. Fields default to `0` when a
/// provider does not report them.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_input_tokens: u32,
    pub cache_creation_input_tokens: u32,
}

/// Why a turn ended, mapped to a neutral set. [`StopReason::Other`] carries the
/// provider's raw reason verbatim so a new/unknown value never panics or maps
/// silently wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    /// The server paused an agentic turn; resume by RE-SENDING the request - do
    /// NOT inject a synthetic "continue" message (the turn loop, T-5.8, owns
    /// this).
    PauseTurn,
    Refusal,
    /// An unrecognized provider stop reason, kept verbatim for forward-compat.
    Other(String),
}

impl StopReason {
    /// Map an Anthropic Messages-API `stop_reason` to the neutral set.
    #[must_use]
    pub fn from_anthropic(raw: &str) -> StopReason {
        match raw {
            "end_turn" => StopReason::EndTurn,
            "max_tokens" => StopReason::MaxTokens,
            "stop_sequence" => StopReason::StopSequence,
            "tool_use" => StopReason::ToolUse,
            "pause_turn" => StopReason::PauseTurn,
            "refusal" => StopReason::Refusal,
            other => StopReason::Other(other.to_string()),
        }
    }

    /// Map an OpenAI Responses-API status / finish / `incomplete` reason to the
    /// neutral set. The Responses API surfaces completion via `status`
    /// (`completed` / `incomplete`) plus an `incomplete_details.reason`, and a
    /// tool turn via output items rather than a stop string - so this accepts
    /// the union of the strings a provider client (T-5.3) will feed it.
    #[must_use]
    pub fn from_openai(raw: &str) -> StopReason {
        match raw {
            "completed" | "stop" | "end_turn" => StopReason::EndTurn,
            "max_output_tokens" | "max_tokens" | "length" => StopReason::MaxTokens,
            "stop_sequence" => StopReason::StopSequence,
            "tool_calls" | "function_call" | "tool_use" => StopReason::ToolUse,
            "content_filter" | "refusal" => StopReason::Refusal,
            other => StopReason::Other(other.to_string()),
        }
    }
}

/// A low-level, provider-neutral streaming event. Mirrors an Anthropic Messages
/// SSE stream (and the OpenAI Responses stream) one-to-one. Producers emit these
/// in stream order: [`MessageStart`](ProviderEvent::MessageStart), then any
/// number of text / thinking / tool blocks, then
/// [`MessageDelta`](ProviderEvent::MessageDelta) (stop reason + final usage),
/// then [`MessageStop`](ProviderEvent::MessageStop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderEvent {
    /// The turn began (Anthropic `message_start`).
    MessageStart,
    /// A chunk of assistant text (`content_block_delta` / `text_delta`).
    TextDelta(String),
    /// A chunk of model thinking (`content_block_delta` / `thinking_delta`).
    ThinkingDelta(String),
    /// A tool-use block opened (`content_block_start` for a `tool_use` block).
    ToolUseStart { id: String, name: String },
    /// A fragment of the currently-open tool's input JSON (`content_block_delta`
    /// / `input_json_delta`). Fragments are ordered and concatenate to one valid
    /// JSON document.
    ToolUseInputDelta { json: String },
    /// The currently-open tool-use block closed (`content_block_stop`).
    ToolUseStop,
    /// The turn's stop reason and final usage (`message_delta`).
    MessageDelta {
        stop_reason: StopReason,
        usage: Usage,
    },
    /// The turn ended (`message_stop`).
    MessageStop,
    /// A transport/decode error from the provider. NOT a refusal - a refusal is
    /// a successful turn that ends in [`StopReason::Refusal`].
    Error(String),
}

/// A high-level, timeline-facing agent event. [`AgentEventMapper`] produces
/// these from a [`ProviderEvent`] stream; the turn loop (T-5.8) additionally
/// emits [`AgentEvent::ToolResult`] after it executes a gated tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    /// A chunk of model thinking to render.
    Thinking(String),
    /// A chunk of assistant text to render.
    Assistant(String),
    /// A complete, parsed tool call the model proposed - subject to the risk
    /// gate before it may run.
    ToolProposed(ToolCall),
    /// A tool finished; carries the (already-sanitized) result text. Emitted by
    /// the turn loop, not the mapper.
    ToolResult { id: String, output: String },
    /// Final token usage for the turn.
    Usage(Usage),
    /// The turn ended; carries the neutral stop reason so the turn loop can
    /// decide whether to loop (`ToolUse` / `PauseTurn`) or stop.
    TurnComplete { stop_reason: StopReason },
    /// A transport/decode error.
    Error(String),
}

/// A tool call whose input is still being streamed.
#[derive(Debug)]
struct OpenTool {
    id: String,
    name: String,
    json: String,
}

/// Provider-neutral reducer: folds a [`ProviderEvent`] stream into
/// [`AgentEvent`]s. Buffers each tool call's streamed input JSON and emits one
/// [`AgentEvent::ToolProposed`] per call when the block closes (or at
/// [`MessageStop`](ProviderEvent::MessageStop) if the stream ends without an
/// explicit close).
///
/// Stateful and single-use per turn: drive it with [`accept`](Self::accept) for
/// each event in order. After it emits [`AgentEvent::TurnComplete`] it is inert
/// (further events yield nothing).
#[derive(Debug, Default)]
pub struct AgentEventMapper {
    open_tool: Option<OpenTool>,
    stop_reason: Option<StopReason>,
    done: bool,
}

impl AgentEventMapper {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one provider event, returning zero or more timeline events.
    pub fn accept(&mut self, event: ProviderEvent) -> Vec<AgentEvent> {
        if self.done {
            return Vec::new();
        }
        match event {
            ProviderEvent::MessageStart => Vec::new(),
            ProviderEvent::TextDelta(t) => vec![AgentEvent::Assistant(t)],
            ProviderEvent::ThinkingDelta(t) => vec![AgentEvent::Thinking(t)],
            ProviderEvent::ToolUseStart { id, name } => {
                // Defensive: providers emit content blocks sequentially, but if a
                // previous tool block never closed, flush it before opening the
                // next so a dropped `ToolUseStop` can't swallow a tool call.
                let out = self.open_tool.take().map(flush_tool).unwrap_or_default();
                self.open_tool = Some(OpenTool {
                    id,
                    name,
                    json: String::new(),
                });
                out
            }
            ProviderEvent::ToolUseInputDelta { json } => {
                if let Some(t) = self.open_tool.as_mut() {
                    t.json.push_str(&json);
                }
                Vec::new()
            }
            ProviderEvent::ToolUseStop => self.open_tool.take().map(flush_tool).unwrap_or_default(),
            ProviderEvent::MessageDelta { stop_reason, usage } => {
                self.stop_reason = Some(stop_reason);
                vec![AgentEvent::Usage(usage)]
            }
            ProviderEvent::MessageStop => {
                // Flush an unterminated tool call (a stream that ends at
                // `tool_use` without an explicit `content_block_stop`).
                let mut out = self.open_tool.take().map(flush_tool).unwrap_or_default();
                let reason = self.stop_reason.take().unwrap_or(StopReason::EndTurn);
                out.push(AgentEvent::TurnComplete {
                    stop_reason: reason,
                });
                self.done = true;
                out
            }
            ProviderEvent::Error(e) => vec![AgentEvent::Error(e)],
        }
    }
}

/// Parse a finished tool's buffered JSON into a [`AgentEvent::ToolProposed`].
/// A blank buffer means a no-argument tool (`{}`); a malformed buffer surfaces
/// as an [`AgentEvent::Error`] rather than silently dropping the call.
fn flush_tool(t: OpenTool) -> Vec<AgentEvent> {
    let trimmed = t.json.trim();
    let input: serde_json::Value = if trimmed.is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                return vec![AgentEvent::Error(format!(
                    "tool `{}` (id {}): malformed input JSON: {e}",
                    t.name, t.id
                ))];
            }
        }
    };
    vec![AgentEvent::ToolProposed(ToolCall {
        id: t.id,
        name: t.name,
        input,
    })]
}

/// Request to a provider for one turn.
#[derive(Debug, Clone)]
pub struct TurnRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    /// Adaptive-thinking effort knob (the provider maps it to its own param).
    pub effort: Effort,
}

/// Errors a provider can return. Never panics for "not implemented".
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("provider not yet implemented: {0}")]
    NotImplemented(&'static str),
    #[error("authentication failed")]
    Auth,
    #[error("http error: {0}")]
    Http(String),
    #[error("decode error: {0}")]
    Decode(String),
}

/// The provider seam. Implementors stream [`ProviderEvent`]s for one turn into
/// `sink`. The shared turn loop drives providers over an mpsc channel (a bounded
/// channel gives backpressure) rather than a poll-based `Stream`.
#[allow(async_fn_in_trait)]
pub trait LlmProvider: Send + Sync {
    /// Provider name for logging.
    fn name(&self) -> &'static str;

    /// Default model id for this provider.
    fn default_model(&self) -> &'static str;

    /// Stream one turn's events into `sink`. Returns when the turn ends.
    async fn stream_turn(
        &self,
        request: TurnRequest,
        sink: mpsc::Sender<ProviderEvent>,
    ) -> Result<(), ProviderError>;
}

/// A provider that replays a scripted [`ProviderEvent`] sequence with no
/// network. The turn loop's tests (and T-5.8) drive the shared loop with this.
#[derive(Debug, Clone)]
pub struct MockProvider {
    name: &'static str,
    model: &'static str,
    script: Vec<ProviderEvent>,
}

impl MockProvider {
    #[must_use]
    pub fn new(script: Vec<ProviderEvent>) -> Self {
        Self {
            name: "mock",
            model: "mock-model",
            script,
        }
    }
}

impl LlmProvider for MockProvider {
    fn name(&self) -> &'static str {
        self.name
    }

    fn default_model(&self) -> &'static str {
        self.model
    }

    async fn stream_turn(
        &self,
        _request: TurnRequest,
        sink: mpsc::Sender<ProviderEvent>,
    ) -> Result<(), ProviderError> {
        for event in &self.script {
            // Stop early if the receiver was dropped (turn aborted/cancelled).
            if sink.send(event.clone()).await.is_err() {
                break;
            }
        }
        Ok(())
    }
}

/// Anthropic provider. STUB - returns `NotImplemented`, makes no network calls.
/// The real Messages-API client is T-5.2.
#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    /// API key (held but unused until T-5.2).
    _api_key: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            _api_key: api_key.into(),
            client: reqwest::Client::new(),
        }
    }

    /// Borrow the configured HTTP client (kept so the type is wired for T-5.2).
    #[must_use]
    pub fn http(&self) -> &reqwest::Client {
        &self.client
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
        _request: TurnRequest,
        _sink: mpsc::Sender<ProviderEvent>,
    ) -> Result<(), ProviderError> {
        // TODO(T-5.2): POST /v1/messages with stream=true, translate SSE
        // (`content_block_delta` thinking/text, `tool_use` blocks, `message_delta`
        // stop_reason/usage) into ProviderEvent, and loop on `tool_use`.
        Err(ProviderError::NotImplemented(
            "AnthropicProvider::stream_turn - Messages API client is T-5.2",
        ))
    }
}

/// OpenAI provider. STUB - returns `NotImplemented`, makes no network calls. The
/// real Responses-API client is T-5.3.
#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    _api_key: String,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            _api_key: api_key.into(),
            client: reqwest::Client::new(),
        }
    }

    #[must_use]
    pub fn http(&self) -> &reqwest::Client {
        &self.client
    }
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
        _request: TurnRequest,
        _sink: mpsc::Sender<ProviderEvent>,
    ) -> Result<(), ProviderError> {
        // TODO(T-5.3): Responses API streaming client -> ProviderEvent.
        Err(ProviderError::NotImplemented(
            "OpenAiProvider::stream_turn - client is T-5.3",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- stop-reason mapping (AC: exhaustive for both providers' raw reasons) ----

    #[test]
    fn anthropic_stop_reasons_map_exhaustively() {
        assert_eq!(StopReason::from_anthropic("end_turn"), StopReason::EndTurn);
        assert_eq!(
            StopReason::from_anthropic("max_tokens"),
            StopReason::MaxTokens
        );
        assert_eq!(
            StopReason::from_anthropic("stop_sequence"),
            StopReason::StopSequence
        );
        assert_eq!(StopReason::from_anthropic("tool_use"), StopReason::ToolUse);
        assert_eq!(
            StopReason::from_anthropic("pause_turn"),
            StopReason::PauseTurn
        );
        assert_eq!(StopReason::from_anthropic("refusal"), StopReason::Refusal);
    }

    #[test]
    fn unknown_anthropic_reason_is_kept_verbatim() {
        assert_eq!(
            StopReason::from_anthropic("brand_new_reason"),
            StopReason::Other("brand_new_reason".to_string())
        );
    }

    #[test]
    fn openai_stop_reasons_map_exhaustively() {
        assert_eq!(StopReason::from_openai("completed"), StopReason::EndTurn);
        assert_eq!(StopReason::from_openai("stop"), StopReason::EndTurn);
        assert_eq!(
            StopReason::from_openai("max_output_tokens"),
            StopReason::MaxTokens
        );
        assert_eq!(StopReason::from_openai("length"), StopReason::MaxTokens);
        assert_eq!(StopReason::from_openai("tool_calls"), StopReason::ToolUse);
        assert_eq!(
            StopReason::from_openai("function_call"),
            StopReason::ToolUse
        );
        assert_eq!(
            StopReason::from_openai("content_filter"),
            StopReason::Refusal
        );
    }

    #[test]
    fn unknown_openai_reason_is_kept_verbatim() {
        assert_eq!(
            StopReason::from_openai("mystery"),
            StopReason::Other("mystery".to_string())
        );
    }

    // ---- AgentEventMapper ----

    fn proposed(events: &[AgentEvent]) -> Vec<&ToolCall> {
        events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolProposed(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    fn drive(mapper: &mut AgentEventMapper, script: Vec<ProviderEvent>) -> Vec<AgentEvent> {
        script.into_iter().flat_map(|e| mapper.accept(e)).collect()
    }

    #[test]
    fn plain_text_turn_emits_assistant_usage_complete() {
        let mut m = AgentEventMapper::new();
        let out = drive(
            &mut m,
            vec![
                ProviderEvent::MessageStart,
                ProviderEvent::TextDelta("Hel".into()),
                ProviderEvent::TextDelta("lo".into()),
                ProviderEvent::MessageDelta {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 2,
                        ..Usage::default()
                    },
                },
                ProviderEvent::MessageStop,
            ],
        );
        assert_eq!(
            out,
            vec![
                AgentEvent::Assistant("Hel".into()),
                AgentEvent::Assistant("lo".into()),
                AgentEvent::Usage(Usage {
                    input_tokens: 10,
                    output_tokens: 2,
                    ..Usage::default()
                }),
                AgentEvent::TurnComplete {
                    stop_reason: StopReason::EndTurn
                },
            ]
        );
    }

    #[test]
    fn thinking_delta_maps_to_thinking() {
        let mut m = AgentEventMapper::new();
        let out = m.accept(ProviderEvent::ThinkingDelta("hmm".into()));
        assert_eq!(out, vec![AgentEvent::Thinking("hmm".into())]);
    }

    #[test]
    fn tool_call_reassembles_streamed_json() {
        let mut m = AgentEventMapper::new();
        let out = drive(
            &mut m,
            vec![
                ProviderEvent::MessageStart,
                ProviderEvent::ToolUseStart {
                    id: "toolu_1".into(),
                    name: "read_file".into(),
                },
                ProviderEvent::ToolUseInputDelta {
                    json: "{\"path\":".into(),
                },
                ProviderEvent::ToolUseInputDelta {
                    json: "\"/etc/hosts\"}".into(),
                },
                ProviderEvent::ToolUseStop,
                ProviderEvent::MessageDelta {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage::default(),
                },
                ProviderEvent::MessageStop,
            ],
        );
        let calls = proposed(&out);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "toolu_1");
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].input["path"], "/etc/hosts");
        assert_eq!(
            out.last(),
            Some(&AgentEvent::TurnComplete {
                stop_reason: StopReason::ToolUse
            })
        );
    }

    #[test]
    fn unterminated_tool_call_is_flushed_at_message_stop() {
        // Stream ends at tool_use with no explicit ToolUseStop.
        let mut m = AgentEventMapper::new();
        let out = drive(
            &mut m,
            vec![
                ProviderEvent::ToolUseStart {
                    id: "toolu_x".into(),
                    name: "glob".into(),
                },
                ProviderEvent::ToolUseInputDelta {
                    json: "{\"pattern\":\"*.rs\"}".into(),
                },
                ProviderEvent::MessageDelta {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage::default(),
                },
                ProviderEvent::MessageStop,
            ],
        );
        let calls = proposed(&out);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].input["pattern"], "*.rs");
    }

    #[test]
    fn empty_argument_tool_yields_empty_object_not_error() {
        let mut m = AgentEventMapper::new();
        let out = drive(
            &mut m,
            vec![
                ProviderEvent::ToolUseStart {
                    id: "toolu_e".into(),
                    name: "list_dir".into(),
                },
                ProviderEvent::ToolUseStop,
                ProviderEvent::MessageStop,
            ],
        );
        let calls = proposed(&out);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].input, serde_json::json!({}));
        assert!(!out.iter().any(|e| matches!(e, AgentEvent::Error(_))));
    }

    #[test]
    fn malformed_tool_json_surfaces_error_not_silent_drop() {
        let mut m = AgentEventMapper::new();
        let out = drive(
            &mut m,
            vec![
                ProviderEvent::ToolUseStart {
                    id: "toolu_bad".into(),
                    name: "edit_file".into(),
                },
                ProviderEvent::ToolUseInputDelta {
                    json: "{not valid json".into(),
                },
                ProviderEvent::ToolUseStop,
                ProviderEvent::MessageStop,
            ],
        );
        assert!(proposed(&out).is_empty());
        assert!(out.iter().any(|e| matches!(e, AgentEvent::Error(_))));
    }

    #[test]
    fn two_tool_calls_in_one_turn_emit_two_proposals() {
        let mut m = AgentEventMapper::new();
        let out = drive(
            &mut m,
            vec![
                ProviderEvent::ToolUseStart {
                    id: "a".into(),
                    name: "read_file".into(),
                },
                ProviderEvent::ToolUseInputDelta {
                    json: "{\"path\":\"a\"}".into(),
                },
                ProviderEvent::ToolUseStop,
                ProviderEvent::ToolUseStart {
                    id: "b".into(),
                    name: "read_file".into(),
                },
                ProviderEvent::ToolUseInputDelta {
                    json: "{\"path\":\"b\"}".into(),
                },
                ProviderEvent::ToolUseStop,
                ProviderEvent::MessageDelta {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage::default(),
                },
                ProviderEvent::MessageStop,
            ],
        );
        let calls = proposed(&out);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "a");
        assert_eq!(calls[1].id, "b");
    }

    #[test]
    fn refusal_turn_completes_with_refusal_reason() {
        let mut m = AgentEventMapper::new();
        let out = drive(
            &mut m,
            vec![
                ProviderEvent::TextDelta("I can't help with that.".into()),
                ProviderEvent::MessageDelta {
                    stop_reason: StopReason::Refusal,
                    usage: Usage::default(),
                },
                ProviderEvent::MessageStop,
            ],
        );
        assert_eq!(
            out.first(),
            Some(&AgentEvent::Assistant("I can't help with that.".into()))
        );
        assert_eq!(
            out.last(),
            Some(&AgentEvent::TurnComplete {
                stop_reason: StopReason::Refusal
            })
        );
    }

    #[test]
    fn events_after_turn_complete_are_ignored() {
        let mut m = AgentEventMapper::new();
        let _ = drive(&mut m, vec![ProviderEvent::MessageStop]);
        // Mapper is inert now.
        assert!(m.accept(ProviderEvent::TextDelta("late".into())).is_empty());
        assert!(m.accept(ProviderEvent::MessageStop).is_empty());
    }

    #[test]
    fn transport_error_maps_to_error_event() {
        let mut m = AgentEventMapper::new();
        let out = m.accept(ProviderEvent::Error("connection reset".into()));
        assert_eq!(out, vec![AgentEvent::Error("connection reset".into())]);
    }

    // ---- providers ----

    #[tokio::test]
    async fn mock_provider_replays_its_script() {
        let script = vec![
            ProviderEvent::MessageStart,
            ProviderEvent::TextDelta("hi".into()),
            ProviderEvent::MessageDelta {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            },
            ProviderEvent::MessageStop,
        ];
        let provider = MockProvider::new(script.clone());
        // Small channel proves the send path applies backpressure / drains fine.
        let (tx, mut rx) = mpsc::channel(2);
        let req = TurnRequest {
            model: provider.default_model().to_string(),
            system: None,
            messages: vec![],
            tools: vec![],
            effort: Effort::Medium,
        };
        let handle = tokio::spawn(async move { provider.stream_turn(req, tx).await });
        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            got.push(ev);
        }
        handle.await.unwrap().unwrap();
        assert_eq!(got, script);
    }

    #[tokio::test]
    async fn anthropic_stub_returns_not_implemented() {
        let p = AnthropicProvider::new("sk-test");
        assert_eq!(p.name(), "anthropic");
        assert_eq!(p.default_model(), "claude-opus-4-8");
        let (tx, _rx) = mpsc::channel(4);
        let req = TurnRequest {
            model: p.default_model().to_string(),
            system: None,
            messages: vec![],
            tools: vec![],
            effort: Effort::High,
        };
        let err = p.stream_turn(req, tx).await.unwrap_err();
        assert!(matches!(err, ProviderError::NotImplemented(_)));
    }

    #[tokio::test]
    async fn openai_stub_returns_not_implemented() {
        let p = OpenAiProvider::new("sk-test");
        let (tx, _rx) = mpsc::channel(4);
        let req = TurnRequest {
            model: p.default_model().to_string(),
            system: None,
            messages: vec![],
            tools: vec![],
            effort: Effort::Low,
        };
        assert!(p.stream_turn(req, tx).await.is_err());
    }
}
