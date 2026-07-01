//! aterm-agent — the agent's safety spine + LLM provider seam.
//!
//! Depends on `aterm-core`. The SAFETY SPINE is implemented for real and tested
//! hard ([`command`], [`risk`], [`secrets`], [`sanitizer`], [`policy`],
//! [`sandbox`]); the typed custom tool set + registry + dispatch seam live in
//! [`tools`]; the Anthropic (Messages API) and OpenAI (Responses API) providers
//! are both real streaming clients behind one `LlmProvider` trait ([`provider`]);
//! and the shared, provider-neutral agentic turn loop drives either provider
//! plan->act->observe, gating every tool call through the safety spine ([`turn`]).
//!
//! Design invariant: NEVER trust a model's self-reported risk. Every command a
//! model proposes is re-classified locally by [`risk::DefaultRiskClassifier`]
//! and gated by [`policy::ApprovalPolicy`] before it can run.

pub mod approval;
pub mod command;
pub mod mcp;
pub mod policy;
pub mod provider;
pub mod risk;
pub mod sandbox;
pub mod sanitizer;
pub mod secrets;
pub mod sink;
pub mod tools;
pub mod transcript;
pub mod turn;

// Root re-exports for the load-bearing public surface.
pub use approval::{ApprovalRequest, ChannelConfirmHandler};
pub use command::ShellCommand;
pub use mcp::connector::{
    classify_mcp_tool, validate_connector_body, validate_servers, McpConfigError, McpServer,
    McpToolPolicy, MCP_CONNECTOR_BETA,
};
pub use mcp::stdio::{
    McpError, McpToolRouter, McpTransport, ProcessTransport, StdioMcpClient, StdioServerConfig,
};
pub use policy::{Approval, ApprovalPolicy, AutonomyMode, AutonomyState};
pub use provider::{
    AgentEvent, AgentEventMapper, AnthropicProvider, ContentBlock, Effort, LlmProvider, Message,
    MockProvider, OpenAiProvider, ProviderError, ProviderEvent, Role, StopReason, ToolCall,
    ToolSpec, TurnRequest, Usage,
};
pub use risk::{gloss_for, DefaultRiskClassifier, RemoteContext, Risk, RiskAssessment, RiskReason};
pub use sandbox::{
    ConfinedCommand, ConfinedOutput, NoSandbox, ResourceLimits, Sandbox, SandboxError,
    SandboxPolicy, SandboxRunner, SeatbeltSandbox,
};
pub use sanitizer::OutputSanitizer;
pub use secrets::{Secrets, SENSITIVE_PATHS};
pub use sink::{CommandSink, FileSink, InjectDisposition, PtyInjectSink, Sinks};
pub use tools::{
    EditFile, Glob, Grep, ListDir, McpToolCall, McpToolSpec, ReadFile, RunCommand, ToolDispatch,
    ToolError, ToolInput, ToolKind, ToolOutcome, ToolRegistry, WriteFile,
};
pub use transcript::{AgentStep, AgentTranscript, ApprovalMode, ResolvedBy, TurnStatus};
pub use turn::{
    badge_for_approval, gate_tool, AgentTurn, CancelToken, ConfirmDecision, ConfirmHandler,
    ToolDisposition, TurnOutcome,
};
