//! The agent timeline transcript model (T-5.10): the data model that records ONE
//! agent turn as timestamp-ordered steps, derives the provider API history from
//! it, and projects each step into the single wall-clock timeline as an agent
//! block ([`aterm_core::AgentBlock`]).
//!
//! Two representations, never conflated (the locked design, `06-agent-architecture.md`
//! section e):
//!
//! 1. **The API conversation history** - what is sent back to the provider:
//!    raw assistant `content` blocks (text + `tool_use`) and the matching
//!    data-role `tool_result` user messages. [`AgentTranscript::derive_history`]
//!    reproduces exactly the message sequence the turn loop (T-5.8) builds, so a
//!    derived history round-trips through any [`LlmProvider`](crate::LlmProvider).
//! 2. **The rendered timeline** - glossed risk reasons, approval state, and
//!    sanitized output, projected into [`aterm_core::AgentBlock`]s that interleave
//!    with human command blocks in one wall-clock [`BlockList`](aterm_core::BlockList).
//!
//! Streaming maps to INCREMENTAL mutation: a text/thinking delta appends to the
//! currently-open step rather than pushing a new one, so the renderer mutates only
//! the tail entry (the 60fps floor, ties to T-2.7 / T-1.8). The `tool_use_id` is
//! the join key correlating a [`AgentStep::ToolCall`], its [`AgentStep::Approval`],
//! and its [`AgentStep::ToolResult`].
//!
//! NOTE on the name: the locked vocabulary calls this `AgentTurn`, but that name is
//! taken by the turn-loop DRIVER ([`crate::AgentTurn`]). To keep both, the
//! transcript data model is [`AgentTranscript`]; the variant set ([`AgentStep`]) and
//! every field keep the locked names verbatim.

use std::time::Instant;

use serde_json::Value;

use aterm_core::{AgentBadge, AgentBlock, AgentBlockKind};

use crate::provider::{ContentBlock, Message, Usage};
use crate::risk::{gloss_for, Risk, RiskAssessment};
use crate::turn::ToolDisposition;

/// The lifecycle status of a recorded turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStatus {
    /// The turn is still streaming / executing.
    Running,
    /// The model ended the turn normally.
    Completed,
    /// The turn was cancelled mid-flight (Esc / interrupt).
    Cancelled,
    /// The turn ended on a transport/decode error or the round cap.
    Error,
}

/// The autonomy mode under which a gated call was resolved - one half of an
/// [`AgentStep::Approval`]. AUTO-SAFE auto-runs proved-safe calls; ASK-ALWAYS
/// confirms every call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalMode {
    AutoSafe,
    AskAlways,
}

/// How a gated call was actually resolved - the other half of an
/// [`AgentStep::Approval`]. The deterministic policy (`Auto`) or the human
/// (`UserApproved` / `UserDeclined`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedBy {
    /// Auto-approved by the deterministic policy (AUTO-SAFE; proved safe).
    Auto,
    /// The human confirmed a gated call.
    UserApproved,
    /// The human declined a gated call.
    UserDeclined,
}

/// One step of an agent turn, timestamp-stamped for wall-clock interleaving. The
/// variant set and field names are the locked vocabulary
/// (`06-agent-architecture.md` section e); every variant carries a `ts` so a
/// long-running [`ToolCall`](AgentStep::ToolCall) interleaves correctly with human
/// typing elsewhere in the timeline.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentStep {
    /// The user's request that opened the turn.
    UserPrompt { text: String, ts: Instant },
    /// A chunk of the model's (summarized) thinking. Accumulated in place while
    /// streaming; NOT echoed back into the API history (the turn loop does not
    /// re-send thinking - see [`AgentTranscript::derive_history`]).
    Thinking { summary: String, ts: Instant },
    /// A chunk of assistant prose. Accumulated in place while streaming.
    AssistantText { text: String, ts: Instant },
    /// A tool call the model proposed, with the DETERMINISTIC gate's assessment and
    /// decision (never the model's self-reported risk). `tool_use_id` joins it to
    /// its [`Approval`](AgentStep::Approval) and [`ToolResult`](AgentStep::ToolResult).
    ToolCall {
        tool_use_id: String,
        name: String,
        input: Value,
        risk: RiskAssessment,
        decision: ToolDisposition,
        ts: Instant,
    },
    /// The sanitized result of executing a tool, joined to its call by `tool_use_id`.
    ToolResult {
        tool_use_id: String,
        output: String,
        is_error: bool,
        ts: Instant,
    },
    /// How a gated tool call was resolved, joined to its call by `tool_use_id`.
    Approval {
        tool_use_id: String,
        mode: ApprovalMode,
        resolved_by: ResolvedBy,
        ts: Instant,
    },
}

impl AgentStep {
    /// This step's wall-clock timestamp.
    #[must_use]
    pub fn ts(&self) -> Instant {
        match self {
            AgentStep::UserPrompt { ts, .. }
            | AgentStep::Thinking { ts, .. }
            | AgentStep::AssistantText { ts, .. }
            | AgentStep::ToolCall { ts, .. }
            | AgentStep::ToolResult { ts, .. }
            | AgentStep::Approval { ts, .. } => *ts,
        }
    }

    /// The `tool_use_id` join key, if this step has one.
    #[must_use]
    pub fn tool_use_id(&self) -> Option<&str> {
        match self {
            AgentStep::ToolCall { tool_use_id, .. }
            | AgentStep::ToolResult { tool_use_id, .. }
            | AgentStep::Approval { tool_use_id, .. } => Some(tool_use_id),
            _ => None,
        }
    }

    /// Project this step into a render-facing timeline block (T-5.10). The text is
    /// pre-glossed/sanitized: a [`ToolCall`](AgentStep::ToolCall) renders its name +
    /// the gate decision (NOT the raw input), a [`ToolResult`](AgentStep::ToolResult)
    /// renders its already-sanitized output, and an [`Approval`](AgentStep::Approval)
    /// renders how it resolved - so `aterm_core` never sees a secret value or an
    /// agent-domain type (it names none of these types - the one-way crate arrow).
    #[must_use]
    pub fn to_block(&self) -> AgentBlock {
        match self {
            AgentStep::UserPrompt { text, ts } => {
                AgentBlock::new(AgentBlockKind::UserPrompt, text.clone(), *ts)
            }
            AgentStep::Thinking { summary, ts } => {
                AgentBlock::new(AgentBlockKind::Thinking, summary.clone(), *ts)
            }
            AgentStep::AssistantText { text, ts } => {
                AgentBlock::new(AgentBlockKind::AssistantText, text.clone(), *ts)
            }
            AgentStep::ToolCall {
                tool_use_id,
                name,
                risk,
                decision,
                ..
            } => AgentBlock::new(
                AgentBlockKind::ToolCall,
                render_tool_call(name, decision),
                self.ts(),
            )
            .with_tool_use_id(tool_use_id.clone())
            .with_badge(badge_for(risk, decision)),
            AgentStep::ToolResult {
                tool_use_id,
                output,
                is_error,
                ts,
            } => AgentBlock::new(AgentBlockKind::ToolResult, output.clone(), *ts)
                .with_tool_use_id(tool_use_id.clone())
                .with_error(*is_error),
            AgentStep::Approval {
                tool_use_id,
                mode,
                resolved_by,
                ts,
            } => AgentBlock::new(
                AgentBlockKind::Approval,
                render_approval(*mode, *resolved_by),
                *ts,
            )
            .with_tool_use_id(tool_use_id.clone()),
        }
    }
}

/// Map a tool call's deterministic gate verdict onto the agent-domain-FREE timeline
/// badge (ticket T-5.11). The gate decision drives whether it auto-ran; the risk
/// LEVEL distinguishes a confirmable escalation ([`AgentBadge::NeedsApproval`]) from
/// a destructive verdict ([`AgentBadge::Blocked`]). This is the ONLY place the
/// agent-side `Risk` is translated into the renderer's vocabulary, so `aterm-core`
/// and `aterm-ui` never name an agent type.
fn badge_for(risk: &RiskAssessment, decision: &ToolDisposition) -> AgentBadge {
    match decision {
        ToolDisposition::AutoRun => AgentBadge::Auto,
        ToolDisposition::NeedsConfirm(_) => {
            if risk.level == Risk::Dangerous {
                AgentBadge::Blocked
            } else {
                AgentBadge::NeedsApproval
            }
        }
    }
}

/// Render a tool call's one-line gloss: its name plus the deterministic decision.
fn render_tool_call(name: &str, decision: &ToolDisposition) -> String {
    match decision {
        ToolDisposition::AutoRun => format!("{name} (auto)"),
        ToolDisposition::NeedsConfirm(reasons) => {
            if reasons.is_empty() {
                format!("{name} (needs confirmation)")
            } else {
                let glossed: Vec<&str> = reasons.iter().map(|r| gloss_for(*r)).collect();
                format!("{name} (needs confirmation: {})", glossed.join("; "))
            }
        }
    }
}

/// Render an approval's resolution gloss.
fn render_approval(mode: ApprovalMode, resolved_by: ResolvedBy) -> String {
    let mode = match mode {
        ApprovalMode::AutoSafe => "auto-safe",
        ApprovalMode::AskAlways => "ask-always",
    };
    match resolved_by {
        ResolvedBy::Auto => format!("auto-approved ({mode})"),
        ResolvedBy::UserApproved => format!("approved by you ({mode})"),
        ResolvedBy::UserDeclined => format!("declined by you ({mode})"),
    }
}

/// One recorded agent turn: an id, a wall-clock start, the ordered [`AgentStep`]s,
/// the lifecycle [`status`](TurnStatus), and the accumulated token [`usage`](Usage).
///
/// Built incrementally as the turn loop streams: text/thinking deltas append to the
/// open step (incremental mutation); tool calls, approvals, and results are recorded
/// with the deterministic gate's info the event stream does not carry. The two
/// derived views - [`derive_history`](Self::derive_history) (API) and
/// [`blocks`](Self::blocks) (timeline) - are computed from the steps, never stored
/// twice.
#[derive(Debug, Clone)]
pub struct AgentTranscript {
    pub id: String,
    pub started_at: Instant,
    pub steps: Vec<AgentStep>,
    pub status: TurnStatus,
    pub usage: Usage,
    /// Index of the currently-open streaming step ([`AgentStep::AssistantText`] /
    /// [`AgentStep::Thinking`]), if any - so a delta appends in place rather than
    /// pushing a new step. Cleared whenever any non-streaming step is recorded.
    open: Option<usize>,
}

impl AgentTranscript {
    /// A fresh, running transcript.
    #[must_use]
    pub fn new(id: impl Into<String>, started_at: Instant) -> Self {
        Self {
            id: id.into(),
            started_at,
            steps: Vec::new(),
            status: TurnStatus::Running,
            usage: Usage::default(),
            open: None,
        }
    }

    /// Record the user prompt that opened the turn.
    pub fn record_user_prompt(&mut self, text: impl Into<String>, ts: Instant) {
        self.open = None;
        self.steps.push(AgentStep::UserPrompt {
            text: text.into(),
            ts,
        });
    }

    /// Append a chunk of streamed assistant text - INCREMENTAL mutation: extends the
    /// open [`AgentStep::AssistantText`] in place if one is open, else opens a new
    /// one. Only the open step is touched (ticket T-5.10 AC2).
    pub fn push_assistant_delta(&mut self, delta: &str, ts: Instant) {
        if let Some(i) = self.open {
            if let Some(AgentStep::AssistantText { text, .. }) = self.steps.get_mut(i) {
                text.push_str(delta);
                return;
            }
        }
        self.steps.push(AgentStep::AssistantText {
            text: delta.to_string(),
            ts,
        });
        self.open = Some(self.steps.len() - 1);
    }

    /// Append a chunk of streamed thinking - INCREMENTAL mutation, mirroring
    /// [`push_assistant_delta`](Self::push_assistant_delta).
    pub fn push_thinking_delta(&mut self, delta: &str, ts: Instant) {
        if let Some(i) = self.open {
            if let Some(AgentStep::Thinking { summary, .. }) = self.steps.get_mut(i) {
                summary.push_str(delta);
                return;
            }
        }
        self.steps.push(AgentStep::Thinking {
            summary: delta.to_string(),
            ts,
        });
        self.open = Some(self.steps.len() - 1);
    }

    /// Record a proposed tool call with the DETERMINISTIC gate's assessment + decision.
    pub fn record_tool_call(
        &mut self,
        tool_use_id: impl Into<String>,
        name: impl Into<String>,
        input: Value,
        risk: RiskAssessment,
        decision: ToolDisposition,
        ts: Instant,
    ) {
        self.open = None;
        self.steps.push(AgentStep::ToolCall {
            tool_use_id: tool_use_id.into(),
            name: name.into(),
            input,
            risk,
            decision,
            ts,
        });
    }

    /// Record how a gated tool call was resolved.
    pub fn record_approval(
        &mut self,
        tool_use_id: impl Into<String>,
        mode: ApprovalMode,
        resolved_by: ResolvedBy,
        ts: Instant,
    ) {
        self.open = None;
        self.steps.push(AgentStep::Approval {
            tool_use_id: tool_use_id.into(),
            mode,
            resolved_by,
            ts,
        });
    }

    /// Record a tool's (already-sanitized) result.
    pub fn record_tool_result(
        &mut self,
        tool_use_id: impl Into<String>,
        output: impl Into<String>,
        is_error: bool,
        ts: Instant,
    ) {
        self.open = None;
        self.steps.push(AgentStep::ToolResult {
            tool_use_id: tool_use_id.into(),
            output: output.into(),
            is_error,
            ts,
        });
    }

    /// Accumulate one round's token usage onto the turn (ticket T-5.10 AC5).
    pub fn add_usage(&mut self, usage: Usage) {
        self.usage.input_tokens = self.usage.input_tokens.saturating_add(usage.input_tokens);
        self.usage.output_tokens = self.usage.output_tokens.saturating_add(usage.output_tokens);
        self.usage.cache_read_input_tokens = self
            .usage
            .cache_read_input_tokens
            .saturating_add(usage.cache_read_input_tokens);
        self.usage.cache_creation_input_tokens = self
            .usage
            .cache_creation_input_tokens
            .saturating_add(usage.cache_creation_input_tokens);
    }

    /// Mark the turn finished with `status`.
    pub fn finish(&mut self, status: TurnStatus) {
        self.open = None;
        self.status = status;
    }

    // --- joins (ticket T-5.10 AC3) -----------------------------------------

    /// The [`AgentStep::ToolCall`] with this `tool_use_id`, if any.
    #[must_use]
    pub fn tool_call(&self, tool_use_id: &str) -> Option<&AgentStep> {
        self.steps
            .iter()
            .find(|s| matches!(s, AgentStep::ToolCall { tool_use_id: id, .. } if id == tool_use_id))
    }

    /// The [`AgentStep::Approval`] with this `tool_use_id`, if any.
    #[must_use]
    pub fn approval(&self, tool_use_id: &str) -> Option<&AgentStep> {
        self.steps
            .iter()
            .find(|s| matches!(s, AgentStep::Approval { tool_use_id: id, .. } if id == tool_use_id))
    }

    /// The [`AgentStep::ToolResult`] with this `tool_use_id`, if any.
    #[must_use]
    pub fn tool_result(&self, tool_use_id: &str) -> Option<&AgentStep> {
        self.steps.iter().find(
            |s| matches!(s, AgentStep::ToolResult { tool_use_id: id, .. } if id == tool_use_id),
        )
    }

    // --- derived views ------------------------------------------------------

    /// Project every step into a render-facing [`AgentBlock`] (ticket T-5.10 AC1).
    /// Pushed into a [`BlockList`](aterm_core::BlockList) in this order, they
    /// interleave with human command blocks by wall-clock (append order IS
    /// wall-clock order).
    #[must_use]
    pub fn blocks(&self) -> Vec<AgentBlock> {
        self.steps.iter().map(AgentStep::to_block).collect()
    }

    /// Derive the provider API conversation history from the turn (ticket T-5.10
    /// AC4). Reproduces the message sequence the turn loop (T-5.8) builds, so it is
    /// a valid provider conversation that round-trips through any provider:
    ///
    /// - a [`UserPrompt`](AgentStep::UserPrompt) becomes a `user` message;
    /// - a contiguous run of assistant content ([`AssistantText`](AgentStep::AssistantText)
    ///   + [`ToolCall`](AgentStep::ToolCall)) becomes one assistant message of `text`
    ///   + `tool_use` blocks;
    /// - the [`ToolResult`](AgentStep::ToolResult)s answering that run become ONE
    ///   data-role `tool_result` message (the Messages-API shape).
    ///
    /// [`Thinking`](AgentStep::Thinking) and [`Approval`](AgentStep::Approval) steps
    /// are render-only and are NOT echoed into the API history (matching the loop,
    /// which re-sends neither).
    #[must_use]
    pub fn derive_history(&self) -> Vec<Message> {
        let mut messages = Vec::new();
        let mut i = 0;
        while i < self.steps.len() {
            match &self.steps[i] {
                AgentStep::UserPrompt { text, .. } => {
                    messages.push(Message::user(text.clone()));
                    i += 1;
                }
                // Render-only steps never enter the API history.
                AgentStep::Thinking { .. } | AgentStep::Approval { .. } => {
                    i += 1;
                }
                AgentStep::AssistantText { .. } | AgentStep::ToolCall { .. } => {
                    // Gather one contiguous assistant turn: text + tool_use blocks,
                    // transparently skipping interleaved render-only steps.
                    let mut text = String::new();
                    let mut blocks: Vec<ContentBlock> = Vec::new();
                    let mut tool_uses: Vec<ContentBlock> = Vec::new();
                    while i < self.steps.len() {
                        match &self.steps[i] {
                            AgentStep::AssistantText { text: t, .. } => text.push_str(t),
                            AgentStep::ToolCall {
                                tool_use_id,
                                name,
                                input,
                                ..
                            } => tool_uses.push(ContentBlock::tool_use(
                                tool_use_id.clone(),
                                name.clone(),
                                input.clone(),
                            )),
                            AgentStep::Thinking { .. } | AgentStep::Approval { .. } => {}
                            // A UserPrompt or ToolResult ends the assistant turn.
                            AgentStep::UserPrompt { .. } | AgentStep::ToolResult { .. } => break,
                        }
                        i += 1;
                    }
                    if !text.is_empty() {
                        blocks.push(ContentBlock::text(text));
                    }
                    blocks.extend(tool_uses);
                    messages.push(Message::assistant_blocks(blocks));
                }
                AgentStep::ToolResult { .. } => {
                    // Gather the contiguous tool results into one data-role message.
                    let mut results: Vec<ContentBlock> = Vec::new();
                    while let Some(AgentStep::ToolResult {
                        tool_use_id,
                        output,
                        is_error,
                        ..
                    }) = self.steps.get(i)
                    {
                        results.push(ContentBlock::tool_result(
                            tool_use_id.clone(),
                            output.clone(),
                            *is_error,
                        ));
                        i += 1;
                    }
                    messages.push(Message::tool_results(results));
                }
            }
        }
        messages
    }
}

/// Whether `level` is shown as a heightened-risk badge in the timeline (a tiny
/// convenience for the render layer; `Caution`/`Dangerous` are flagged).
#[must_use]
pub fn is_elevated(level: Risk) -> bool {
    level != Risk::Safe
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Role, StopReason};
    use crate::risk::RiskReason;
    use serde_json::json;
    use std::time::{Duration, Instant};

    fn t0() -> Instant {
        Instant::now()
    }

    fn safe() -> RiskAssessment {
        RiskAssessment {
            level: Risk::Safe,
            reasons: Vec::new(),
        }
    }

    fn dangerous() -> RiskAssessment {
        RiskAssessment {
            level: Risk::Dangerous,
            reasons: vec![RiskReason::Destructive],
        }
    }

    /// A small end-to-end transcript: prompt -> assistant text + one tool call ->
    /// its result -> closing assistant text.
    fn sample(base: Instant) -> AgentTranscript {
        let mut tr = AgentTranscript::new("turn-1", base);
        tr.record_user_prompt("list the files", base);
        tr.push_assistant_delta("Sure, ", base + Duration::from_millis(1));
        tr.push_assistant_delta("running it.", base + Duration::from_millis(2));
        tr.record_tool_call(
            "toolu_1",
            "run_command",
            json!({ "command": ["ls", "-la"] }),
            safe(),
            ToolDisposition::AutoRun,
            base + Duration::from_millis(3),
        );
        tr.record_approval(
            "toolu_1",
            ApprovalMode::AutoSafe,
            ResolvedBy::Auto,
            base + Duration::from_millis(4),
        );
        tr.record_tool_result(
            "toolu_1",
            "a.txt\nb.txt",
            false,
            base + Duration::from_millis(5),
        );
        tr.push_assistant_delta("Two files.", base + Duration::from_millis(6));
        tr.add_usage(Usage {
            input_tokens: 100,
            output_tokens: 20,
            ..Usage::default()
        });
        tr.finish(TurnStatus::Completed);
        tr
    }

    // ---- AC2: streaming mutates only the current entry ----------------------

    #[test]
    fn streaming_deltas_extend_the_open_step_not_push_new_ones() {
        let base = t0();
        let mut tr = AgentTranscript::new("t", base);
        tr.record_user_prompt("hi", base);
        let after_prompt = tr.steps.clone();

        tr.push_assistant_delta("Hel", base);
        assert_eq!(tr.steps.len(), 2, "first delta opens ONE assistant step");
        tr.push_assistant_delta("lo, ", base);
        tr.push_assistant_delta("world", base);
        assert_eq!(
            tr.steps.len(),
            2,
            "subsequent deltas append in place, never push new steps"
        );

        // The earlier (UserPrompt) step is byte-identical - untouched by the deltas.
        assert_eq!(&tr.steps[0], &after_prompt[0]);
        match &tr.steps[1] {
            AgentStep::AssistantText { text, .. } => assert_eq!(text, "Hello, world"),
            other => panic!("expected AssistantText, got {other:?}"),
        }
    }

    #[test]
    fn a_non_streaming_step_closes_the_open_one() {
        let base = t0();
        let mut tr = AgentTranscript::new("t", base);
        tr.push_assistant_delta("part one", base);
        tr.record_tool_call(
            "toolu_x",
            "read_file",
            json!({ "path": "a" }),
            safe(),
            ToolDisposition::AutoRun,
            base,
        );
        // A delta after the tool call opens a SEPARATE assistant step, not a
        // re-extension of the pre-tool one.
        tr.push_assistant_delta("part two", base);
        let assistant: Vec<&String> = tr
            .steps
            .iter()
            .filter_map(|s| match s {
                AgentStep::AssistantText { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(assistant, vec!["part one", "part two"]);
    }

    // ---- AC3: tool_use_id join ---------------------------------------------

    #[test]
    fn tool_call_approval_and_result_join_by_tool_use_id() {
        let tr = sample(t0());
        assert!(tr.tool_call("toolu_1").is_some());
        assert!(tr.approval("toolu_1").is_some());
        assert!(tr.tool_result("toolu_1").is_some());
        assert!(tr.tool_call("nope").is_none());

        // The three steps share the join key and nothing else claims it.
        let ids: Vec<&str> = tr.steps.iter().filter_map(AgentStep::tool_use_id).collect();
        assert_eq!(ids, vec!["toolu_1", "toolu_1", "toolu_1"]);
    }

    // ---- AC4: derived API history is a valid provider conversation ---------

    #[test]
    fn derive_history_reproduces_the_loop_message_shape() {
        let tr = sample(t0());
        let msgs = tr.derive_history();

        // [ user, assistant(text + tool_use), tool_results, assistant(text) ]
        assert_eq!(
            msgs.len(),
            4,
            "one user, one assistant+tool, one results, one closing assistant"
        );
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[1].role, Role::Assistant);
        assert_eq!(msgs[2].role, Role::Tool);
        assert_eq!(msgs[3].role, Role::Assistant);

        // The assistant turn carries the streamed text THEN the tool_use block.
        match &msgs[1].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Sure, running it."),
            other => panic!("expected leading text, got {other:?}"),
        }
        match &msgs[1].content[1] {
            ContentBlock::ToolUse { id, name, .. } => {
                assert_eq!(id, "toolu_1");
                assert_eq!(name, "run_command");
            }
            other => panic!("expected tool_use, got {other:?}"),
        }
        // The result message joins back by tool_use_id.
        match &msgs[2].content[0] {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                assert_eq!(tool_use_id, "toolu_1");
                assert_eq!(content, "a.txt\nb.txt");
                assert!(!is_error);
            }
            other => panic!("expected tool_result, got {other:?}"),
        }
    }

    #[test]
    fn derived_history_is_structurally_valid() {
        let tr = sample(t0());
        let msgs = tr.derive_history();
        assert!(
            is_valid_provider_conversation(&msgs),
            "every tool_use must be answered and every tool_result must name a prior tool_use"
        );
    }

    /// The Messages-API structural invariant: starts with a user message, and every
    /// `tool_result.tool_use_id` names a `tool_use.id` that appeared in an EARLIER
    /// assistant message.
    fn is_valid_provider_conversation(messages: &[Message]) -> bool {
        if messages.first().map(|m| m.role) != Some(Role::User) {
            return false;
        }
        let mut seen_tool_use: std::collections::HashSet<String> = std::collections::HashSet::new();
        for m in messages {
            for b in &m.content {
                match b {
                    ContentBlock::ToolUse { id, .. } => {
                        seen_tool_use.insert(id.clone());
                    }
                    ContentBlock::ToolResult { tool_use_id, .. } => {
                        if !seen_tool_use.contains(tool_use_id) {
                            return false;
                        }
                    }
                    ContentBlock::Text { .. } => {}
                }
            }
        }
        true
    }

    // ---- AC5: usage attribution --------------------------------------------

    #[test]
    fn usage_accumulates_onto_the_turn() {
        let base = t0();
        let mut tr = AgentTranscript::new("t", base);
        tr.add_usage(Usage {
            input_tokens: 10,
            output_tokens: 3,
            ..Usage::default()
        });
        tr.add_usage(Usage {
            input_tokens: 5,
            output_tokens: 7,
            cache_read_input_tokens: 2,
            ..Usage::default()
        });
        assert_eq!(tr.usage.input_tokens, 15);
        assert_eq!(tr.usage.output_tokens, 10);
        assert_eq!(tr.usage.cache_read_input_tokens, 2);
    }

    // ---- projection (AC1 data side) ----------------------------------------

    #[test]
    fn projection_carries_kind_join_key_and_glossed_text() {
        let tr = sample(t0());
        let blocks = tr.blocks();
        assert_eq!(blocks.len(), tr.steps.len());

        // The tool-call block renders the decision, not the raw input, and carries
        // the join key.
        let call = blocks
            .iter()
            .find(|b| b.kind == AgentBlockKind::ToolCall)
            .unwrap();
        assert_eq!(call.tool_use_id.as_deref(), Some("toolu_1"));
        assert!(call.text.contains("run_command"));
        assert!(call.text.contains("auto"));
        assert!(
            !call.text.contains("ls"),
            "raw argv must not leak into the gloss"
        );
        // T-5.11: an auto-run call carries the `Auto` badge (the renderer draws "auto").
        assert_eq!(call.badge, Some(AgentBadge::Auto));

        // A dangerous call glosses its reasons AND carries the `Blocked` badge.
        let danger = AgentStep::ToolCall {
            tool_use_id: "x".into(),
            name: "run_command".into(),
            input: json!({}),
            risk: dangerous(),
            decision: ToolDisposition::NeedsConfirm(vec![RiskReason::Destructive]),
            ts: t0(),
        };
        let danger_block = danger.to_block();
        assert!(danger_block.text.contains("deletes or overwrites files"));
        assert_eq!(danger_block.badge, Some(AgentBadge::Blocked));

        // A Caution (non-destructive) escalation carries the confirmable
        // `NeedsApproval` badge, not `Blocked`.
        let caution = AgentStep::ToolCall {
            tool_use_id: "y".into(),
            name: "run_command".into(),
            input: json!({}),
            risk: RiskAssessment {
                level: Risk::Caution,
                reasons: vec![RiskReason::PackageMutator],
            },
            decision: ToolDisposition::NeedsConfirm(vec![RiskReason::PackageMutator]),
            ts: t0(),
        };
        assert_eq!(caution.to_block().badge, Some(AgentBadge::NeedsApproval));
    }

    #[test]
    fn approval_projection_renders_resolution() {
        let base = t0();
        let mut tr = AgentTranscript::new("t", base);
        tr.record_approval(
            "id",
            ApprovalMode::AskAlways,
            ResolvedBy::UserDeclined,
            base,
        );
        let b = &tr.blocks()[0];
        assert_eq!(b.kind, AgentBlockKind::Approval);
        assert!(b.text.contains("declined by you"));
    }

    #[test]
    fn status_lifecycle() {
        let mut tr = AgentTranscript::new("t", t0());
        assert_eq!(tr.status, TurnStatus::Running);
        tr.finish(TurnStatus::Cancelled);
        assert_eq!(tr.status, TurnStatus::Cancelled);
        // StopReason is unused here but the enum is in scope for the wiring tests.
        let _ = StopReason::EndTurn;
    }
}
