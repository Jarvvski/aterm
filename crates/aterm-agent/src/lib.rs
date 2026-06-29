//! aterm-agent — the agent's safety spine + LLM provider seam.
//!
//! Depends on `aterm-core`. The SAFETY SPINE is implemented for real and tested
//! hard ([`command`], [`risk`], [`secrets`], [`sanitizer`], [`policy`],
//! [`sandbox`]); the typed custom tool set + registry + dispatch seam live in
//! [`tools`]; the Anthropic provider is the real Messages-API client and the
//! OpenAI provider is a compiling stub ([`provider`]); and the shared,
//! provider-neutral agentic turn loop drives either provider plan->act->observe,
//! gating every tool call through the safety spine ([`turn`]).
//!
//! Design invariant: NEVER trust a model's self-reported risk. Every command a
//! model proposes is re-classified locally by [`risk::DefaultRiskClassifier`]
//! and gated by [`policy::ApprovalPolicy`] before it can run.

pub mod command;
pub mod policy;
pub mod provider;
pub mod risk;
pub mod sandbox;
pub mod sanitizer;
pub mod secrets;
pub mod tools;
pub mod turn;

// Root re-exports for the load-bearing public surface.
pub use command::ShellCommand;
pub use policy::{Approval, ApprovalPolicy};
pub use provider::{
    AgentEvent, AgentEventMapper, AnthropicProvider, ContentBlock, Effort, LlmProvider, Message,
    MockProvider, OpenAiProvider, ProviderError, ProviderEvent, Role, StopReason, ToolCall,
    ToolSpec, TurnRequest, Usage,
};
pub use risk::{gloss_for, DefaultRiskClassifier, RemoteContext, Risk, RiskAssessment, RiskReason};
pub use sandbox::{
    ConfinedCommand, NoSandbox, Sandbox, SandboxError, SandboxPolicy, SeatbeltSandbox,
};
pub use sanitizer::OutputSanitizer;
pub use secrets::{Secrets, SENSITIVE_PATHS};
pub use tools::{
    EditFile, Glob, Grep, ListDir, ReadFile, RunCommand, ToolDispatch, ToolError, ToolInput,
    ToolKind, ToolOutcome, ToolRegistry, WriteFile,
};
pub use turn::{
    AgentTurn, CancelToken, ConfirmDecision, ConfirmHandler, ToolDisposition, TurnOutcome,
};
