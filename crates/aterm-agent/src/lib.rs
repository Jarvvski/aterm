//! aterm-agent — the agent's safety spine + LLM provider seam.
//!
//! Depends on `aterm-core`. The SAFETY SPINE is implemented for real and tested
//! hard ([`command`], [`risk`], [`secrets`], [`sanitizer`], [`policy`],
//! [`sandbox`]); the typed custom tool set + registry + dispatch seam live in
//! [`tools`]; the LLM provider clients are compiling stubs that return errors and
//! make no network calls ([`provider`]), and the turn loop is a plan→act→observe
//! skeleton over them ([`turn`]).
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
pub use turn::{AgentTurn, ConfirmDecision, ToolDisposition};
