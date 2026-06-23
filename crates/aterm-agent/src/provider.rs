//! `LlmProvider` trait + provider-neutral event types. Provider clients are
//! compiling STUBS: they implement the trait but return a clear "not yet
//! implemented" error rather than panicking, and make NO network calls.
//!
//! The real Anthropic Messages-API client (model `claude-opus-4-8`, adaptive
//! thinking + effort param, SSE streaming, loop on `stop_reason: "tool_use"`) is
//! EPIC-5. The trait surface here is deliberately provider-neutral so the
//! Anthropic and OpenAI clients can share the turn loop.

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

/// A tool call the model emitted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// A streaming delta from the provider (lower-level than [`AgentEvent`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderDelta {
    /// A chunk of model "thinking" (adaptive thinking output).
    Thinking(String),
    /// A chunk of assistant text.
    Text(String),
    /// A tool call became fully available.
    ToolCall(ToolCall),
    /// The provider finished a turn with this stop reason.
    Stop { reason: StopReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    /// Model wants to call a tool — the turn loop must execute it and continue.
    ToolUse,
    MaxTokens,
}

/// High-level, UI-facing agent event (what the timeline widget renders).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    Thinking(String),
    Assistant(String),
    /// A tool call is proposed (subject to the risk gate before execution).
    ToolProposed(ToolCall),
    /// A tool finished; carries the (already-sanitized) result text.
    ToolResult {
        id: String,
        output: String,
    },
    TurnComplete,
    Error(String),
}

/// Request to a provider for one turn.
#[derive(Debug, Clone)]
pub struct TurnRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    /// Adaptive-thinking effort knob (provider maps it to its own param).
    pub effort: Effort,
}

/// Effort / thinking budget knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effort {
    Low,
    Medium,
    High,
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

/// The provider seam. Implementors stream [`ProviderDelta`]s for one turn.
///
/// The scaffold's turn loop drives providers over an mpsc channel rather than a
/// poll-based `Stream`, so the real method is [`LlmProvider::stream_turn`].
#[allow(async_fn_in_trait)]
pub trait LlmProvider: Send + Sync {
    /// Provider name for logging.
    fn name(&self) -> &'static str;

    /// Default model id for this provider.
    fn default_model(&self) -> &'static str;

    /// Stream one turn's deltas into `sink`. Returns when the turn ends.
    async fn stream_turn(
        &self,
        request: TurnRequest,
        sink: mpsc::Sender<ProviderDelta>,
    ) -> Result<(), ProviderError>;
}

/// Anthropic provider. STUB — returns `NotImplemented`, makes no network calls.
#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    /// API key (held but unused until EPIC-5).
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

    /// Borrow the configured HTTP client (kept so the type is wired for EPIC-5).
    pub fn http(&self) -> &reqwest::Client {
        &self.client
    }
}

impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn default_model(&self) -> &'static str {
        // EPIC-5 target model.
        "claude-opus-4-8"
    }

    async fn stream_turn(
        &self,
        _request: TurnRequest,
        _sink: mpsc::Sender<ProviderDelta>,
    ) -> Result<(), ProviderError> {
        // TODO(ticket EPIC-5): POST /v1/messages with stream=true, parse SSE
        // (`content_block_delta` thinking/text, `tool_use` blocks), and loop the
        // caller on `stop_reason: "tool_use"`. Adaptive thinking + effort param.
        Err(ProviderError::NotImplemented(
            "AnthropicProvider::stream_turn — Messages API client is EPIC-5",
        ))
    }
}

/// OpenAI provider. STUB — returns `NotImplemented`, makes no network calls.
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
        _sink: mpsc::Sender<ProviderDelta>,
    ) -> Result<(), ProviderError> {
        // TODO(ticket EPIC-5): Responses/Chat Completions streaming client.
        Err(ProviderError::NotImplemented(
            "OpenAiProvider::stream_turn — client is EPIC-5",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            effort: Effort::Medium,
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
