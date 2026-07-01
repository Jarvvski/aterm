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
//! [`AnthropicProvider`] (the default) is the real Messages-API client - it
//! lives in [`anthropic`] (T-5.2); [`OpenAiProvider`] is the real Responses-API
//! client - it lives in [`openai`] (T-5.3). Both translate their provider SSE
//! stream into the same neutral [`ProviderEvent`] sequence, so the shared turn
//! loop drives either with no provider-specific branching. A scriptable
//! [`MockProvider`] drives the turn loop and tests with no network.
//!
//! Divergences from the Kotlin prototype (deliberate, per aterm's locked
//! decisions): the prototype models a SINGLE tool (`propose_command` ->
//! `CommandProposal`); aterm is locked MULTI-TOOL, so the mapper stays generic
//! ([`ToolCall`] = `{id, name, input}`) and does not hardcode a tool name.
//! Thinking deltas are modeled here (the prototype dropped them) because the
//! T-5.1 acceptance criteria require the timeline to render thinking.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub mod anthropic;
pub mod openai;
pub(crate) mod sse;

pub use anthropic::AnthropicProvider;
pub use openai::OpenAiProvider;

/// One message in a conversation, provider-neutral. Content is a list of typed
/// [`ContentBlock`]s so an agentic history can carry assistant `tool_use` blocks
/// and the matching user `tool_result` blocks (each block keyed by id), not just
/// plain text. A provider client maps these onto its own wire shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    /// A plain-text user message.
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::text(text)],
        }
    }

    /// A plain-text assistant message.
    #[must_use]
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::text(text)],
        }
    }

    /// An inline operator/system message (the non-spoofable operator channel).
    #[must_use]
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: vec![ContentBlock::text(text)],
        }
    }

    /// An assistant message made of arbitrary content blocks (e.g. text plus the
    /// `tool_use` blocks it emitted).
    #[must_use]
    pub fn assistant_blocks(content: Vec<ContentBlock>) -> Self {
        Self {
            role: Role::Assistant,
            content,
        }
    }

    /// One user message carrying ALL of a round's tool results. The turn loop
    /// builds exactly one of these per tool-use round (the provider maps it to a
    /// single `user` message with every `tool_result` block - the shape the
    /// Messages API requires).
    #[must_use]
    pub fn tool_results(results: Vec<ContentBlock>) -> Self {
        Self {
            role: Role::Tool,
            content: results,
        }
    }
}

/// A typed piece of a [`Message`]'s content, provider-neutral. Mirrors the small
/// set of Anthropic content-block types the agent loop produces and consumes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Assistant or user prose.
    Text { text: String },
    /// A tool call the assistant emitted (echoed back verbatim when continuing a
    /// turn so the model sees its own prior action).
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// The result of executing a tool, keyed to its `tool_use` by id. A failed
    /// tool sets `is_error` rather than being dropped.
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

impl ContentBlock {
    /// A text block.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock::Text { text: text.into() }
    }

    /// A tool-use block.
    #[must_use]
    pub fn tool_use(
        id: impl Into<String>,
        name: impl Into<String>,
        input: serde_json::Value,
    ) -> Self {
        ContentBlock::ToolUse {
            id: id.into(),
            name: name.into(),
            input,
        }
    }

    /// A tool-result block.
    #[must_use]
    pub fn tool_result(
        tool_use_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        ContentBlock::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error,
        }
    }
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

/// A tool the model may call (provider-neutral schema). Built by the
/// [`crate::tools`] registry for the custom typed tools and advertised to a
/// provider (T-5.2/T-5.3), which maps it onto that provider's wire shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's input.
    pub input_schema: serde_json::Value,
    /// Constrain the model's `tool_use.input` to validate exactly against
    /// `input_schema`. On the Anthropic Messages wire this is a sibling of
    /// name/description/input_schema (NOT on `tool_choice`); every custom typed
    /// tool sets it `true`. A provider client maps it into its own dialect.
    pub strict: bool,
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

impl Effort {
    /// The wire token for this effort level (Anthropic `output_config.effort`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }
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
    /// A REMOTE MCP tool call the model made via the connector (T-6.1), assembled
    /// whole at `content_block_stop`. Executed SERVER-SIDE by Anthropic - it is
    /// NOT dispatched locally and never enters the risk-gate/execute path; it is
    /// render-only. `server` is the connector `server_name`.
    McpToolUse {
        id: String,
        name: String,
        server: String,
        input: serde_json::Value,
    },
    /// The result of a connector MCP tool call (server-side), keyed by `id`. The
    /// content is UNTRUSTED (a prompt-injection vector) and MUST be sanitized
    /// before it is rendered or re-used.
    McpToolResult {
        id: String,
        output: String,
        is_error: bool,
    },
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
    /// A tool finished; carries the (already-sanitized) result text and whether it
    /// was an error result (a declined/rejected/failed call, fed back with
    /// `is_error`). Emitted by the turn loop, not the mapper; `is_error` lets the
    /// transcript (T-5.10) record a faithful `ToolResult` step / `tool_result`
    /// block without re-deriving it.
    ToolResult {
        id: String,
        output: String,
        is_error: bool,
    },
    /// A tool call the model emitted whose streamed input JSON was malformed (e.g.
    /// a truncated stream). Carries the originating `tool_use` id so the turn loop
    /// can feed an `is_error` tool_result back keyed to it - the call is surfaced
    /// and recoverable, never silently dropped.
    ToolProposalFailed {
        id: String,
        name: String,
        error: String,
    },
    /// A REMOTE MCP tool call the model made via the connector (T-6.1), executed
    /// SERVER-SIDE. Render-only: the turn loop forwards it to the timeline but
    /// never dispatches it (contrast [`ToolProposed`](AgentEvent::ToolProposed),
    /// which is gated + run locally). `server` is the connector `server_name`.
    McpToolUse {
        id: String,
        name: String,
        server: String,
        input: serde_json::Value,
    },
    /// The (already-sanitized) result of a connector MCP tool call, keyed by `id`.
    McpToolResult {
        id: String,
        output: String,
        is_error: bool,
    },
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
            // Connector MCP blocks bypass the open-tool buffer entirely: they are
            // assembled whole by the provider and never become a locally-run
            // `ToolProposed`. Pass them straight through to the timeline.
            ProviderEvent::McpToolUse {
                id,
                name,
                server,
                input,
            } => vec![AgentEvent::McpToolUse {
                id,
                name,
                server,
                input,
            }],
            ProviderEvent::McpToolResult {
                id,
                output,
                is_error,
            } => vec![AgentEvent::McpToolResult {
                id,
                output,
                is_error,
            }],
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
/// A blank buffer means a no-argument tool (`{}`); a malformed buffer surfaces as
/// an [`AgentEvent::ToolProposalFailed`] (carrying the call's id so the turn loop
/// can feed an `is_error` tool_result back) rather than silently dropping the call.
fn flush_tool(t: OpenTool) -> Vec<AgentEvent> {
    let trimmed = t.json.trim();
    let input: serde_json::Value = if trimmed.is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                return vec![AgentEvent::ToolProposalFailed {
                    id: t.id,
                    name: t.name,
                    error: format!("malformed input JSON: {e}"),
                }];
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
    /// Hard cap on output tokens for this turn. The Anthropic Messages API
    /// REQUIRES `max_tokens`; OpenAI maps it to `max_output_tokens`.
    pub max_tokens: u32,
}

/// Errors a provider can return. Never panics for "not implemented".
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("provider not yet implemented: {0}")]
    NotImplemented(&'static str),
    #[error("authentication failed")]
    Auth,
    /// The request was rejected locally before sending (e.g. an invalid MCP
    /// connector config that would otherwise 400). See T-6.1.
    #[error("invalid request: {0}")]
    Invalid(String),
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

/// A provider that replays scripted [`ProviderEvent`] sequences with no network -
/// one script per round. The turn loop (T-5.8) calls [`stream_turn`](LlmProvider::stream_turn)
/// once per provider round, so a multi-round agentic loop is driven by
/// [`MockProvider::scripted`] with one script per round; it also records every
/// [`TurnRequest`] it receives so a test can assert the follow-up request shape
/// (the `tool_result` round-trip).
#[derive(Debug, Clone)]
pub struct MockProvider {
    name: &'static str,
    model: &'static str,
    scripts: Vec<Vec<ProviderEvent>>,
    /// Which script the NEXT `stream_turn` call replays (shared so a `Clone`
    /// observes the same progress). Interior-mutable because `stream_turn` is
    /// `&self`.
    cursor: Arc<Mutex<usize>>,
    /// Every request received, in call order.
    requests: Arc<Mutex<Vec<TurnRequest>>>,
}

impl MockProvider {
    /// Replay a SINGLE round's script (back-compat). For a multi-round agentic
    /// loop use [`MockProvider::scripted`].
    #[must_use]
    pub fn new(script: Vec<ProviderEvent>) -> Self {
        Self::scripted(vec![script])
    }

    /// Replay one script per round, in order. When the loop asks for more rounds
    /// than were scripted, the extra calls send NOTHING (the channel just closes),
    /// which the turn loop reads as an empty `end_turn` round - so a mis-scripted
    /// test terminates instead of looping forever.
    #[must_use]
    pub fn scripted(scripts: Vec<Vec<ProviderEvent>>) -> Self {
        Self {
            name: "mock",
            model: "mock-model",
            scripts,
            cursor: Arc::new(Mutex::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Override the reported provider name + model (to prove the shared loop is
    /// provider-neutral - the same loop driving two distinct identities).
    #[must_use]
    pub fn with_identity(mut self, name: &'static str, model: &'static str) -> Self {
        self.name = name;
        self.model = model;
        self
    }

    /// The [`TurnRequest`]s this mock has received, in call order.
    #[must_use]
    pub fn requests(&self) -> Vec<TurnRequest> {
        self.requests.lock().unwrap().clone()
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
        request: TurnRequest,
        sink: mpsc::Sender<ProviderEvent>,
    ) -> Result<(), ProviderError> {
        self.requests.lock().unwrap().push(request);
        let idx = {
            let mut cursor = self.cursor.lock().unwrap();
            let i = *cursor;
            *cursor += 1;
            i
        };
        // An exhausted script (idx out of range) sends nothing; the loop reads the
        // closed channel as an empty `end_turn` round.
        if let Some(script) = self.scripts.get(idx) {
            for event in script {
                // Stop early if the receiver was dropped (turn aborted/cancelled).
                if sink.send(event.clone()).await.is_err() {
                    break;
                }
            }
        }
        Ok(())
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
    fn connector_mcp_events_pass_through_and_never_become_tool_proposed() {
        // T-6.1: connector blocks map 1:1 to render-only AgentEvents; they never
        // enter the open-tool buffer, so they can never surface as ToolProposed
        // (which the loop would try to dispatch locally).
        let mut m = AgentEventMapper::new();
        let out = drive(
            &mut m,
            vec![
                ProviderEvent::McpToolUse {
                    id: "mcp_1".into(),
                    name: "search".into(),
                    server: "docs".into(),
                    input: serde_json::json!({ "q": "rust" }),
                },
                ProviderEvent::McpToolResult {
                    id: "mcp_1".into(),
                    output: "a doc".into(),
                    is_error: false,
                },
            ],
        );
        assert_eq!(
            out,
            vec![
                AgentEvent::McpToolUse {
                    id: "mcp_1".into(),
                    name: "search".into(),
                    server: "docs".into(),
                    input: serde_json::json!({ "q": "rust" }),
                },
                AgentEvent::McpToolResult {
                    id: "mcp_1".into(),
                    output: "a doc".into(),
                    is_error: false,
                },
            ]
        );
        assert!(!out.iter().any(|e| matches!(e, AgentEvent::ToolProposed(_))));
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
    fn malformed_tool_json_surfaces_a_failed_proposal_keyed_to_the_id() {
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
        // Not a successful proposal, and not silently dropped: it surfaces as a
        // failed proposal that still carries the id (so the turn loop can feed an
        // is_error tool_result back) - never a bare Error.
        assert!(proposed(&out).is_empty());
        assert!(out.iter().any(|e| matches!(
            e,
            AgentEvent::ToolProposalFailed { id, name, .. }
                if id == "toolu_bad" && name == "edit_file"
        )));
        assert!(!out.iter().any(|e| matches!(e, AgentEvent::Error(_))));
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
            max_tokens: 1024,
        };
        let handle = tokio::spawn(async move { provider.stream_turn(req, tx).await });
        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            got.push(ev);
        }
        handle.await.unwrap().unwrap();
        assert_eq!(got, script);
    }

    #[test]
    fn effort_levels_map_to_wire_tokens() {
        assert_eq!(Effort::Low.as_str(), "low");
        assert_eq!(Effort::Medium.as_str(), "medium");
        assert_eq!(Effort::High.as_str(), "high");
        assert_eq!(Effort::Xhigh.as_str(), "xhigh");
        assert_eq!(Effort::Max.as_str(), "max");
    }

    #[test]
    fn message_constructors_build_typed_content_blocks() {
        assert_eq!(
            Message::user("hi").content,
            vec![ContentBlock::Text { text: "hi".into() }]
        );
        let results =
            Message::tool_results(vec![ContentBlock::tool_result("toolu_1", "ok", false)]);
        assert_eq!(results.role, Role::Tool);
        assert_eq!(
            results.content,
            vec![ContentBlock::ToolResult {
                tool_use_id: "toolu_1".into(),
                content: "ok".into(),
                is_error: false,
            }]
        );
    }
}
