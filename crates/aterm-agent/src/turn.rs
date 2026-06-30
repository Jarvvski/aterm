//! The shared, provider-neutral agentic turn loop (T-5.8): plan -> act -> observe
//! -> repeat.
//!
//! [`AgentTurn::run`] drives ONE user request to completion against any
//! [`LlmProvider`]: it streams the provider's [`ProviderEvent`]s through the
//! shared [`AgentEventMapper`], and whenever the model stops with
//! [`StopReason::ToolUse`] it runs each proposed tool call through the SAFETY
//! SPINE (risk gate -> approval policy -> output sanitizer) and feeds the results
//! back as structurally-separated `tool_result` blocks, looping until the model
//! ends the turn (or a non-tool stop, a cancel, or the round cap).
//!
//! Provider-neutral by construction: the loop only ever touches the
//! [`LlmProvider`] / [`ProviderEvent`] surface, so the SAME loop drives the
//! Anthropic and OpenAI providers unchanged (T-5.2 / T-5.3).
//!
//! The safety spine is NOT optional and NOT model-controlled: every tool call is
//! re-classified locally regardless of any risk the model self-reports, against
//! the SAME [`Secrets`] source the [`OutputSanitizer`] redacts from, so the two
//! defenses cannot drift. Tool RESULTS re-enter the conversation only as data-role
//! `tool_result` blocks (never as user instructions - the prompt-injection
//! structural separation), and every tool's raw output is sanitized before it
//! re-enters context.
//!
//! Execution itself is delegated to a [`ToolDispatch`] (T-5.9 owns the real
//! command/file sinks under the sandbox, T-5.7); this loop owns the
//! plan->act->observe structure, the FIRST gate decision + the confirmation flow,
//! the parallel/serial scheduling, sanitization, and the `tool_result`
//! round-trip. Read-only tools fan out concurrently; mutations serialize.
//!
//! `pause_turn` is resumed transparently inside the provider (T-5.2), so the loop
//! never sees it as a terminal stop reason.

use std::future::{pending, poll_fn, Future};
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

use tokio::sync::{mpsc, watch};

use aterm_core::AgentBadge;

use crate::policy::{Approval, ApprovalPolicy};
use crate::provider::{
    AgentEvent, AgentEventMapper, ContentBlock, LlmProvider, Message, ProviderError, ProviderEvent,
    StopReason, ToolCall, TurnRequest,
};
use crate::risk::{Risk, RiskAssessment, RiskReason};
use crate::sanitizer::OutputSanitizer;
use crate::secrets::Secrets;
use crate::tools::{ToolDispatch, ToolInput, ToolOutcome, ToolRegistry};

/// Default cap on tool rounds per turn - a backstop against a model that never
/// stops calling tools. A turn that genuinely needs more rounds is rare; hitting
/// the cap ends the turn with `Other("max_tool_rounds")`.
const DEFAULT_MAX_ROUNDS: u32 = 24;

/// Outcome of asking the user to confirm a gated tool call. The app/UI layer
/// (T-5.11) provides this; the loop only ever asks AFTER the deterministic policy
/// has demanded confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmDecision {
    Approved,
    Denied,
}

/// How a proposed tool call is resolved into an action by the deterministic gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolDisposition {
    /// Auto-approved by the policy (safe + not shell-active).
    AutoRun,
    /// Needs explicit confirmation; carries the human-readable reasons.
    NeedsConfirm(Vec<RiskReason>),
}

/// Resolves a gated (RequireConfirm) tool call into a [`ConfirmDecision`]. The
/// app/UI layer (T-5.11) implements this; the loop consults the DETERMINISTIC
/// policy first and only asks when the policy demands confirmation. An
/// auto-approved (AUTO-SAFE) call never reaches the handler.
#[allow(async_fn_in_trait)]
pub trait ConfirmHandler: Send + Sync {
    /// Confirm (or decline) a gated tool call, given the deterministic assessment
    /// that escalated it.
    async fn confirm(&self, call: &ToolCall, assessment: &RiskAssessment) -> ConfirmDecision;
}

/// How a full agentic turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnOutcome {
    /// The terminal stop reason: the reason of the last provider round that did
    /// not request another tool round. `Other("max_tool_rounds")` if the loop hit
    /// its round cap.
    pub stop_reason: StopReason,
    /// How many provider rounds ran (one per `stream_turn`).
    pub rounds: u32,
    /// Whether the loop was cancelled mid-flight (the Esc/cancel interrupt).
    pub cancelled: bool,
}

/// A cheap, cloneable cancellation signal for an in-flight turn (the Esc/cancel
/// interrupt; ties to T-3.3). Backed by a `watch` channel so a cancel wakes a loop
/// parked in `select!` promptly, not just at the next poll boundary.
#[derive(Debug, Clone)]
pub struct CancelToken {
    tx: Arc<watch::Sender<bool>>,
    rx: watch::Receiver<bool>,
}

impl CancelToken {
    #[must_use]
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(false);
        Self {
            tx: Arc::new(tx),
            rx,
        }
    }

    /// Signal cancellation. Idempotent; safe to call from any clone.
    pub fn cancel(&self) {
        // The token holds a receiver, so a send can never fail for lack of one.
        let _ = self.tx.send(true);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }

    /// Resolve once cancellation is requested. Returns immediately if it already
    /// has been; if the sender is somehow dropped without cancelling, it never
    /// resolves (so a `select!` arm waiting on it simply stays pending).
    async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let mut rx = self.rx.clone();
        while rx.changed().await.is_ok() {
            if *rx.borrow() {
                return;
            }
        }
        pending::<()>().await;
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Drives one agent turn against a provider, applying the safety spine. Borrows
/// the provider and the single [`Secrets`] source for the turn's lifetime; the
/// tool dispatcher, confirmation handler, and cancel token are supplied per
/// [`run`](Self::run).
pub struct AgentTurn<'a, P: LlmProvider> {
    provider: &'a P,
    policy: ApprovalPolicy,
    secrets: &'a Secrets,
    max_rounds: u32,
}

/// The deterministic gate decision for a typed tool call, as a free function so the
/// app can recompute the exact same verdict for a `ToolProposed` event without an
/// `AgentTurn` (ticket T-5.11). Never trusts the model. `run_command` goes through
/// the full argv risk gate (T-5.5) against the single [`Secrets`] source; file
/// MUTATIONS are never provably safe, so the gate over-approximates toward
/// RequireConfirm (the locked autonomy stance); read-only tools auto-run (any secret
/// VALUES in their output are redacted by the sanitizer before the result re-enters
/// context, and the file sink re-gates a sensitive-path read as defense in depth,
/// T-5.9).
///
/// This is the ONE place the per-tool-call disposition is decided, so the loop's
/// execution gate ([`AgentTurn::gate`]) and the UI's badge ([`badge_for_approval`])
/// can never disagree about whether a given call auto-runs, needs approval, or is
/// blocked.
#[must_use]
pub fn gate_tool(input: &ToolInput, policy: &ApprovalPolicy, secrets: &Secrets) -> Approval {
    match input {
        ToolInput::RunCommand(rc) => {
            policy.decide_command(&rc.command, rc.cwd.as_deref(), None, secrets)
        }
        ToolInput::EditFile(_) | ToolInput::WriteFile(_) => {
            Approval::RequireConfirm(RiskAssessment {
                level: Risk::Caution,
                reasons: vec![RiskReason::FileWrite],
            })
        }
        ToolInput::ReadFile(_)
        | ToolInput::ListDir(_)
        | ToolInput::Glob(_)
        | ToolInput::Grep(_) => Approval::AutoApprove,
    }
}

/// Project a gate [`Approval`] onto the agent-domain-FREE [`AgentBadge`] the timeline
/// draws (ticket T-5.11). The auto-safe default means an [`Approval::AutoApprove`] is
/// [`AgentBadge::Auto`]; an escalation is [`AgentBadge::Blocked`] when the underlying
/// risk is `Dangerous` (a destructive verdict needs an explicit override) and
/// [`AgentBadge::NeedsApproval`] otherwise. This mirrors `transcript::badge_for` (the
/// `ToolDisposition` path) so the live-stream badge and the recorded-step badge agree.
#[must_use]
pub fn badge_for_approval(approval: &Approval) -> AgentBadge {
    match approval {
        Approval::AutoApprove => AgentBadge::Auto,
        Approval::RequireConfirm(assessment) => {
            if assessment.level == Risk::Dangerous {
                AgentBadge::Blocked
            } else {
                AgentBadge::NeedsApproval
            }
        }
    }
}

impl<'a, P: LlmProvider> AgentTurn<'a, P> {
    /// The default turn: AUTO-SAFE policy, default round cap.
    pub fn new(provider: &'a P, secrets: &'a Secrets) -> Self {
        Self {
            provider,
            policy: ApprovalPolicy::new(),
            secrets,
            max_rounds: DEFAULT_MAX_ROUNDS,
        }
    }

    /// A turn with an explicit approval policy (e.g. the ask-always tier).
    pub fn with_policy(provider: &'a P, secrets: &'a Secrets, policy: ApprovalPolicy) -> Self {
        Self {
            provider,
            policy,
            secrets,
            max_rounds: DEFAULT_MAX_ROUNDS,
        }
    }

    /// Override the tool-round cap (chainable).
    #[must_use]
    pub fn with_max_rounds(mut self, max_rounds: u32) -> Self {
        self.max_rounds = max_rounds;
        self
    }

    /// Classify a proposed tool call's command line deterministically (the input
    /// box / string path). The model's own risk claim is intentionally ignored.
    pub fn disposition_for_command(&self, command_line: &str) -> ToolDisposition {
        // The gate classifies against the SAME `Secrets` the sanitizer redacts
        // from (`self.secrets`) - one source, so the two defenses cannot drift.
        // `decide` routes through the multi-line buffer gate, so an embedded `\n`
        // cannot smuggle a dangerous second command past a head-keyed rule.
        match self.policy.decide(command_line, self.secrets) {
            Approval::AutoApprove => ToolDisposition::AutoRun,
            Approval::RequireConfirm(a) => ToolDisposition::NeedsConfirm(a.reasons),
        }
    }

    /// Sanitize a tool's raw output before it is fed back to the model or shown.
    pub fn sanitize_observation(&self, raw: &str, max_len: Option<usize>) -> String {
        OutputSanitizer::new(self.secrets).sanitize(raw, max_len)
    }

    /// The deterministic gate decision for a typed tool call. Never trusts the
    /// model. Delegates to the free [`gate_tool`] so the SAME verdict the loop acts
    /// on can be recomputed elsewhere (the UI's `ToolProposed` badge, ticket T-5.11)
    /// against this turn's policy + single [`Secrets`] source - no crown-jewel
    /// divergence.
    fn gate(&self, input: &ToolInput) -> Approval {
        gate_tool(input, &self.policy, self.secrets)
    }

    /// Run the full agentic loop: plan -> act -> observe -> repeat, until the model
    /// ends the turn (or a non-tool stop, a cancel, or the round cap). Streams
    /// timeline events on `events`; returns how the turn ended.
    ///
    /// `events` receives every mapper event (thinking / assistant text / proposed
    /// tools / per-round usage) plus a [`AgentEvent::ToolResult`] per executed tool
    /// and exactly ONE final [`AgentEvent::TurnComplete`] (the per-round ones are
    /// swallowed so a consumer sees a single turn boundary, not one per tool
    /// round). On cancel, no `TurnComplete` is emitted.
    pub async fn run<D, A>(
        &self,
        mut request: TurnRequest,
        registry: &ToolRegistry,
        dispatch: &D,
        approver: &A,
        cancel: &CancelToken,
        events: mpsc::Sender<AgentEvent>,
    ) -> Result<TurnOutcome, ProviderError>
    where
        D: ToolDispatch,
        A: ConfirmHandler,
    {
        let mut rounds = 0u32;
        let mut final_stop = StopReason::EndTurn;
        let mut cancelled = false;

        loop {
            if cancel.is_cancelled() {
                cancelled = true;
                break;
            }
            if rounds >= self.max_rounds {
                final_stop = StopReason::Other("max_tool_rounds".to_string());
                break;
            }
            rounds += 1;

            // --- stream one provider round (cancellable) ---
            let round = tokio::select! {
                biased;
                () = cancel.cancelled() => { cancelled = true; break; }
                r = self.stream_round(request.clone(), &events) => match r {
                    Ok(round) => round,
                    Err(e) => {
                        let _ = events.send(AgentEvent::Error(e.to_string())).await;
                        return Err(e);
                    }
                },
            };

            // A round that did not request tools (or asked but produced no tool
            // activity at all - neither a valid nor a malformed call) is terminal.
            // `pause_turn` is handled inside the provider, so it never reaches here.
            let had_tool_activity = !round.proposed.is_empty() || !round.failed_results.is_empty();
            if round.stop != StopReason::ToolUse || !had_tool_activity {
                final_stop = round.stop;
                break;
            }

            // --- act: gate + execute the proposed calls (cancellable) ---
            let mut result_blocks = tokio::select! {
                biased;
                () = cancel.cancelled() => { cancelled = true; break; }
                blocks = self.execute_round(&round.proposed, registry, dispatch, approver, &events) => blocks,
            };
            // Malformed calls never reach the dispatcher; their placeholder tool_use
            // is already in `round.tool_uses`, so pair each with its is_error result.
            result_blocks.extend(round.failed_results);

            // --- observe: append the assistant turn + the tool results, then loop ---
            let mut assistant_blocks: Vec<ContentBlock> = Vec::new();
            if !round.text.is_empty() {
                assistant_blocks.push(ContentBlock::text(round.text));
            }
            assistant_blocks.extend(round.tool_uses);
            request
                .messages
                .push(Message::assistant_blocks(assistant_blocks));
            request.messages.push(Message::tool_results(result_blocks));
        }

        if !cancelled {
            let _ = events
                .send(AgentEvent::TurnComplete {
                    stop_reason: final_stop.clone(),
                })
                .await;
        }
        Ok(TurnOutcome {
            stop_reason: final_stop,
            rounds,
            cancelled,
        })
    }

    /// Stream one provider round: drive `stream_turn` and the shared mapper
    /// concurrently (a bounded channel gives backpressure and avoids a deadlock if
    /// a burst exceeds the channel), forward every event EXCEPT the per-round
    /// `TurnComplete`, and collect what the loop needs to continue.
    async fn stream_round(
        &self,
        request: TurnRequest,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Result<RoundData, ProviderError> {
        let (tx, mut rx) = mpsc::channel::<ProviderEvent>(64);
        let mut mapper = AgentEventMapper::new();
        let mut text = String::new();
        let mut proposed: Vec<ToolCall> = Vec::new();
        let mut tool_uses: Vec<ContentBlock> = Vec::new();
        let mut failed_results: Vec<ContentBlock> = Vec::new();
        let mut stop = StopReason::EndTurn;

        let producer = self.provider.stream_turn(request, tx);
        let consumer = async {
            while let Some(pe) = rx.recv().await {
                for ae in mapper.accept(pe) {
                    match &ae {
                        AgentEvent::Assistant(t) => text.push_str(t),
                        AgentEvent::ToolProposed(call) => {
                            proposed.push(call.clone());
                            tool_uses.push(ContentBlock::tool_use(
                                call.id.clone(),
                                call.name.clone(),
                                call.input.clone(),
                            ));
                        }
                        AgentEvent::ToolProposalFailed { id, name, error } => {
                            // The call's streamed JSON was malformed (e.g. a
                            // truncated stream). Reconstruct a placeholder tool_use
                            // so the assistant message carries a block the is_error
                            // tool_result can pair with, and pre-build that error
                            // result. The call never runs; the model sees the error
                            // and can re-issue it.
                            tool_uses.push(ContentBlock::tool_use(
                                id.clone(),
                                name.clone(),
                                serde_json::json!({}),
                            ));
                            failed_results.push(ContentBlock::tool_result(
                                id.clone(),
                                error.clone(),
                                true,
                            ));
                        }
                        AgentEvent::TurnComplete { stop_reason } => stop = stop_reason.clone(),
                        _ => {}
                    }
                    // Swallow the per-round TurnComplete; `run` emits one final.
                    if !matches!(ae, AgentEvent::TurnComplete { .. }) {
                        let _ = events.send(ae).await;
                    }
                }
            }
        };

        let (producer_result, ()) = tokio::join!(producer, consumer);
        producer_result?;
        Ok(RoundData {
            proposed,
            text,
            tool_uses,
            failed_results,
            stop,
        })
    }

    /// Gate, (confirm,) execute, and sanitize a round's proposed tool calls,
    /// returning the `tool_result` blocks in proposal order. Read-only tools fan
    /// out concurrently; mutations serialize. A parse-rejected or user-declined
    /// call yields an `is_error` result rather than being dropped, so the model
    /// can see and correct it.
    async fn execute_round<D, A>(
        &self,
        proposed: &[ToolCall],
        registry: &ToolRegistry,
        dispatch: &D,
        approver: &A,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Vec<ContentBlock>
    where
        D: ToolDispatch,
        A: ConfirmHandler,
    {
        // Slot per proposal index: an immediate result (parse-rejected / declined)
        // or, after gating, a scheduled execution.
        let mut results: Vec<Option<ContentBlock>> = (0..proposed.len()).map(|_| None).collect();
        let mut to_run: Vec<(usize, String, ToolInput)> = Vec::new();

        for (idx, call) in proposed.iter().enumerate() {
            match registry.parse(call) {
                Err(e) => {
                    let msg = format!("tool input rejected: {e}");
                    Self::emit_result(events, &call.id, &msg, true).await;
                    results[idx] = Some(ContentBlock::tool_result(call.id.clone(), msg, true));
                }
                Ok(input) => match self.gate(&input) {
                    Approval::AutoApprove => to_run.push((idx, call.id.clone(), input)),
                    Approval::RequireConfirm(assessment) => {
                        match approver.confirm(call, &assessment).await {
                            ConfirmDecision::Approved => {
                                to_run.push((idx, call.id.clone(), input));
                            }
                            ConfirmDecision::Denied => {
                                let msg = "tool call declined by user".to_string();
                                Self::emit_result(events, &call.id, &msg, true).await;
                                results[idx] =
                                    Some(ContentBlock::tool_result(call.id.clone(), msg, true));
                            }
                        }
                    }
                },
            }
        }

        // Read-only (parallel-safe) tools fan out concurrently; mutations serialize.
        let (parallel, serial): (Vec<_>, Vec<_>) = to_run
            .into_iter()
            .partition(|(_, _, input)| input.parallel_safe());

        let parallel_futs: Vec<_> = parallel
            .into_iter()
            .map(move |(idx, id, input)| async move { (idx, id, dispatch.dispatch(input).await) })
            .collect();
        let parallel_results = join_all_concurrent(parallel_futs).await;

        let mut serial_results: Vec<(usize, String, ToolOutcome)> = Vec::new();
        for (idx, id, input) in serial {
            let outcome = dispatch.dispatch(input).await;
            serial_results.push((idx, id, outcome));
        }

        for (idx, id, outcome) in parallel_results.into_iter().chain(serial_results) {
            let clean = self.sanitize_observation(&outcome.output, None);
            Self::emit_result(events, &id, &clean, outcome.is_error).await;
            results[idx] = Some(ContentBlock::tool_result(id, clean, outcome.is_error));
        }

        // Every slot is filled by construction (executed, declined, or rejected).
        results.into_iter().flatten().collect()
    }

    /// Forward a tool result to the timeline. `is_error` mirrors the `tool_result`
    /// block's flag (a declined / parse-rejected / failed call), so the transcript
    /// (T-5.10) records a faithful step without re-deriving it.
    async fn emit_result(
        events: &mpsc::Sender<AgentEvent>,
        id: &str,
        output: &str,
        is_error: bool,
    ) {
        let _ = events
            .send(AgentEvent::ToolResult {
                id: id.to_string(),
                output: output.to_string(),
                is_error,
            })
            .await;
    }
}

/// What one streamed provider round yielded that the loop needs to continue.
struct RoundData {
    proposed: Vec<ToolCall>,
    text: String,
    /// Assistant `tool_use` blocks for EVERY call the model emitted this round -
    /// valid ones plus a placeholder for each malformed one - so every fed-back
    /// `tool_result` has a matching `tool_use` to pair with.
    tool_uses: Vec<ContentBlock>,
    /// Pre-built `is_error` `tool_result` blocks for malformed (un-parseable) tool
    /// calls; appended to the executed results so a malformed call is reported back
    /// rather than silently dropped.
    failed_results: Vec<ContentBlock>,
    stop: StopReason,
}

/// Poll a dynamic set of futures CONCURRENTLY on the current task - no
/// `tokio::spawn` (so the futures may borrow non-`'static` state, e.g. the
/// dispatcher) and no `futures` dependency. Each still-pending future is polled on
/// every wake; for the small tool batches here (a handful of read-only calls) the
/// O(n) re-poll is irrelevant, and it is what lets the reads make progress
/// concurrently - a barrier reached by one is observed by the rest in the same
/// poll pass. Outputs are returned in input order.
async fn join_all_concurrent<F: Future>(futures: Vec<F>) -> Vec<F::Output> {
    let mut slots: Vec<Option<Pin<Box<F>>>> =
        futures.into_iter().map(|f| Some(Box::pin(f))).collect();
    let mut outputs: Vec<Option<F::Output>> = (0..slots.len()).map(|_| None).collect();
    poll_fn(move |cx| {
        let mut pending_any = false;
        for (i, slot) in slots.iter_mut().enumerate() {
            if let Some(fut) = slot.as_mut() {
                match fut.as_mut().poll(cx) {
                    Poll::Ready(v) => {
                        outputs[i] = Some(v);
                        *slot = None;
                    }
                    Poll::Pending => pending_any = true,
                }
            }
        }
        if pending_any {
            Poll::Pending
        } else {
            Poll::Ready(outputs.iter_mut().map(|o| o.take().unwrap()).collect())
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Effort, MockProvider, Usage};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    // ---- shared test fixtures ----------------------------------------------

    fn req() -> TurnRequest {
        TurnRequest {
            model: "claude-opus-4-8".to_string(),
            system: None,
            messages: vec![Message::user("do the thing")],
            tools: ToolRegistry::default().specs(),
            effort: Effort::Medium,
            max_tokens: 1024,
        }
    }

    #[test]
    fn gate_tool_and_badge_for_approval_agree_across_the_three_verdicts() {
        // T-5.11: the app recomputes the verdict for a `ToolProposed` badge via the
        // SAME `gate_tool` the loop's execution gate uses, then maps it to a badge.
        // This pins the auto-run / needs-approval / blocked partition so the
        // crown-jewel gate and the timeline badge can never drift.
        use crate::tools::{ReadFile, RunCommand, WriteFile};
        let secrets = Secrets::new();
        let policy = ApprovalPolicy::new(); // AUTO-SAFE default
        let cmd = |args: &[&str]| {
            ToolInput::RunCommand(RunCommand {
                command: args.iter().map(|s| (*s).to_string()).collect(),
                cwd: None,
            })
        };

        // A proven-safe, non-shell-active command auto-runs -> Auto.
        let safe = gate_tool(&cmd(&["ls", "-la"]), &policy, &secrets);
        assert_eq!(safe, Approval::AutoApprove);
        assert_eq!(badge_for_approval(&safe), AgentBadge::Auto);

        // A destructive command is RequireConfirm(Dangerous) -> Blocked.
        let danger = gate_tool(&cmd(&["rm", "-rf", "/"]), &policy, &secrets);
        assert!(
            matches!(&danger, Approval::RequireConfirm(a) if a.level == Risk::Dangerous),
            "rm -rf / must escalate as Dangerous, got {danger:?}"
        );
        assert_eq!(badge_for_approval(&danger), AgentBadge::Blocked);

        // A file MUTATION is always RequireConfirm(Caution) -> NeedsApproval.
        let write = gate_tool(
            &ToolInput::WriteFile(WriteFile {
                path: "out.txt".into(),
                content: "hi".into(),
            }),
            &policy,
            &secrets,
        );
        assert!(matches!(&write, Approval::RequireConfirm(a) if a.level == Risk::Caution));
        assert_eq!(badge_for_approval(&write), AgentBadge::NeedsApproval);

        // A read-only tool auto-runs -> Auto.
        let read = gate_tool(
            &ToolInput::ReadFile(ReadFile {
                path: "in.txt".into(),
                range: None,
            }),
            &policy,
            &secrets,
        );
        assert_eq!(read, Approval::AutoApprove);
        assert_eq!(badge_for_approval(&read), AgentBadge::Auto);
    }

    /// One provider round that proposes a single tool call and stops on ToolUse.
    fn tool_round(id: &str, name: &str, input_json: &str) -> Vec<ProviderEvent> {
        vec![
            ProviderEvent::MessageStart,
            ProviderEvent::ToolUseStart {
                id: id.to_string(),
                name: name.to_string(),
            },
            ProviderEvent::ToolUseInputDelta {
                json: input_json.to_string(),
            },
            ProviderEvent::ToolUseStop,
            ProviderEvent::MessageDelta {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
            },
            ProviderEvent::MessageStop,
        ]
    }

    /// One provider round that emits text and ends the turn.
    fn end_round(text: &str) -> Vec<ProviderEvent> {
        vec![
            ProviderEvent::MessageStart,
            ProviderEvent::TextDelta(text.to_string()),
            ProviderEvent::MessageDelta {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            },
            ProviderEvent::MessageStop,
        ]
    }

    fn drain(rx: &mut mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    fn has_tool_result(messages: &[Message], want_error: Option<bool>) -> bool {
        messages.iter().flat_map(|m| &m.content).any(|b| match b {
            ContentBlock::ToolResult { is_error, .. } => want_error.is_none_or(|w| *is_error == w),
            _ => false,
        })
    }

    // ---- test dispatchers + approvers --------------------------------------

    #[derive(Default)]
    struct RecordingDispatch {
        seen: Arc<Mutex<Vec<ToolInput>>>,
    }
    impl RecordingDispatch {
        fn count(&self) -> usize {
            self.seen.lock().unwrap().len()
        }
    }
    impl ToolDispatch for RecordingDispatch {
        async fn dispatch(&self, input: ToolInput) -> ToolOutcome {
            self.seen.lock().unwrap().push(input.clone());
            match &input {
                ToolInput::RunCommand(rc) => {
                    ToolOutcome::ok(format!("ran: {}", rc.command.join(" ")))
                }
                ToolInput::ReadFile(rf) => ToolOutcome::ok(format!("contents of {}", rf.path)),
                ToolInput::EditFile(_) | ToolInput::WriteFile(_) => ToolOutcome::ok("written"),
                _ => ToolOutcome::ok("ok"),
            }
        }
    }

    struct ApproveAll;
    impl ConfirmHandler for ApproveAll {
        async fn confirm(&self, _call: &ToolCall, _a: &RiskAssessment) -> ConfirmDecision {
            ConfirmDecision::Approved
        }
    }

    struct DenyAll;
    impl ConfirmHandler for DenyAll {
        async fn confirm(&self, _call: &ToolCall, _a: &RiskAssessment) -> ConfirmDecision {
            ConfirmDecision::Denied
        }
    }

    /// Records how many times it was consulted, so a test can prove an AUTO-SAFE
    /// call never reaches the approver.
    #[derive(Default)]
    struct CountingApprover {
        calls: Arc<AtomicUsize>,
    }
    impl ConfirmHandler for CountingApprover {
        async fn confirm(&self, _call: &ToolCall, _a: &RiskAssessment) -> ConfirmDecision {
            self.calls.fetch_add(1, Ordering::SeqCst);
            // Deny - so if a "safe" call ever did reach here, it would NOT run and
            // the test would catch it.
            ConfirmDecision::Denied
        }
    }

    // ---- AC#1: end-to-end loop ---------------------------------------------

    #[tokio::test]
    async fn loop_runs_a_gated_tool_then_completes_on_end_turn() {
        let provider = MockProvider::scripted(vec![
            tool_round("toolu_1", "run_command", r#"{"command":["ls","-la"]}"#),
            end_round("all done"),
        ]);
        let secrets = Secrets::new();
        let dispatch = RecordingDispatch::default();
        let turn = AgentTurn::new(&provider, &secrets);
        let (etx, mut erx) = mpsc::channel(256);

        let outcome = turn
            .run(
                req(),
                &ToolRegistry::default(),
                &dispatch,
                &ApproveAll,
                &CancelToken::new(),
                etx,
            )
            .await
            .unwrap();

        assert_eq!(outcome.stop_reason, StopReason::EndTurn);
        assert_eq!(outcome.rounds, 2);
        assert!(!outcome.cancelled);
        // The auto-safe tool actually executed.
        assert_eq!(dispatch.count(), 1);

        // The follow-up request fed the tool_result back as one tool message.
        let reqs = provider.requests();
        assert_eq!(reqs.len(), 2);
        assert!(has_tool_result(&reqs[1].messages, None));

        // Timeline: a proposal, a result, and EXACTLY ONE final TurnComplete.
        let events = drain(&mut erx);
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolProposed(_))));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolResult { .. })));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AgentEvent::TurnComplete { .. }))
                .count(),
            1
        );
        assert_eq!(
            events.last(),
            Some(&AgentEvent::TurnComplete {
                stop_reason: StopReason::EndTurn
            })
        );
    }

    // ---- T-5.10 AC4: the transcript's API history round-trips --------------

    /// Fold a real turn's emitted [`AgentEvent`]s into a transcript, mirroring how the
    /// app (T-5.11) will record one. Risk/decision are render-only and not carried by
    /// the event stream, so a placeholder is used (the derived API history ignores
    /// them; the round-trip below exercises only the message shape).
    fn transcript_from_events(
        user: &str,
        events: &[AgentEvent],
    ) -> crate::transcript::AgentTranscript {
        use crate::transcript::{AgentTranscript, TurnStatus};
        let now = std::time::Instant::now();
        let mut tr = AgentTranscript::new("turn", now);
        tr.record_user_prompt(user, now);
        for e in events {
            match e {
                AgentEvent::Assistant(t) => tr.push_assistant_delta(t, now),
                AgentEvent::Thinking(t) => tr.push_thinking_delta(t, now),
                AgentEvent::ToolProposed(call) => tr.record_tool_call(
                    call.id.clone(),
                    call.name.clone(),
                    call.input.clone(),
                    RiskAssessment {
                        level: Risk::Safe,
                        reasons: Vec::new(),
                    },
                    ToolDisposition::AutoRun,
                    now,
                ),
                AgentEvent::ToolResult {
                    id,
                    output,
                    is_error,
                } => tr.record_tool_result(id, output, *is_error, now),
                AgentEvent::Usage(u) => tr.add_usage(*u),
                AgentEvent::TurnComplete { .. } => tr.finish(TurnStatus::Completed),
                _ => {}
            }
        }
        tr
    }

    #[tokio::test]
    async fn transcript_derived_history_reproduces_and_round_trips_through_the_mock() {
        // Drive a real turn, fold its events into a transcript, and prove the derived
        // API history (1) reproduces EXACTLY what the loop sent the provider on the
        // follow-up round, and (2) round-trips: fed back into a fresh provider it is
        // accepted verbatim as a valid conversation, tool_use/tool_result join intact.
        let provider = MockProvider::scripted(vec![
            tool_round("toolu_1", "run_command", r#"{"command":["ls","-la"]}"#),
            end_round("all done"),
        ]);
        let secrets = Secrets::new();
        let dispatch = RecordingDispatch::default();
        let (etx, mut erx) = mpsc::channel(256);
        AgentTurn::new(&provider, &secrets)
            .run(
                req(),
                &ToolRegistry::default(),
                &dispatch,
                &ApproveAll,
                &CancelToken::new(),
                etx,
            )
            .await
            .unwrap();

        let events = drain(&mut erx);
        let tr = transcript_from_events("do the thing", &events);
        let history = tr.derive_history();

        // (1) The tool-bearing prefix equals the loop's accumulated follow-up request
        //     ([user, assistant(tool_use), tool_results]); the derived history then adds
        //     the final assistant turn the loop never re-sends.
        let sent = &provider.requests()[1].messages;
        assert_eq!(
            &history[..sent.len()],
            &sent[..],
            "derived history reproduces what the loop actually sent the provider"
        );
        assert!(
            history.len() > sent.len(),
            "the full transcript also carries the closing assistant turn"
        );

        // (2) Round-trip: feed the derived history into a fresh provider as the start of
        //     a new turn; it is recorded verbatim, so it is a valid provider conversation.
        let mock2 = MockProvider::scripted(vec![end_round("ok")]);
        let mut req2 = req();
        req2.messages = history.clone();
        let (tx2, _rx2) = mpsc::channel(64);
        AgentTurn::new(&mock2, &secrets)
            .run(
                req2,
                &ToolRegistry::default(),
                &RecordingDispatch::default(),
                &ApproveAll,
                &CancelToken::new(),
                tx2,
            )
            .await
            .unwrap();
        let got = &mock2.requests()[0].messages;
        assert_eq!(
            got, &history,
            "the mock received the derived history verbatim"
        );
        assert!(
            has_tool_result(got, Some(false)),
            "the round-tripped history carries the joined (non-error) tool_result"
        );
    }

    // ---- AC#2: gate decision respected -------------------------------------

    #[tokio::test]
    async fn dangerous_tool_is_not_executed_when_confirmation_is_denied() {
        let provider = MockProvider::scripted(vec![
            tool_round("toolu_x", "run_command", r#"{"command":["rm","-rf","/"]}"#),
            end_round("stopped"),
        ]);
        let secrets = Secrets::new();
        let dispatch = RecordingDispatch::default();
        let turn = AgentTurn::new(&provider, &secrets);
        let (etx, _erx) = mpsc::channel(256);

        let outcome = turn
            .run(
                req(),
                &ToolRegistry::default(),
                &dispatch,
                &DenyAll,
                &CancelToken::new(),
                etx,
            )
            .await
            .unwrap();

        assert_eq!(outcome.stop_reason, StopReason::EndTurn);
        assert_eq!(
            dispatch.count(),
            0,
            "a denied dangerous tool must never reach the dispatcher"
        );
        // An is_error result was still fed back so the model learns it was refused.
        let reqs = provider.requests();
        assert!(has_tool_result(&reqs[1].messages, Some(true)));
    }

    #[tokio::test]
    async fn dangerous_tool_runs_only_after_explicit_confirmation() {
        let provider = MockProvider::scripted(vec![
            tool_round("toolu_x", "run_command", r#"{"command":["rm","-rf","/"]}"#),
            end_round("done"),
        ]);
        let secrets = Secrets::new();
        let dispatch = RecordingDispatch::default();
        let turn = AgentTurn::new(&provider, &secrets);
        let (etx, _erx) = mpsc::channel(256);

        turn.run(
            req(),
            &ToolRegistry::default(),
            &dispatch,
            &ApproveAll,
            &CancelToken::new(),
            etx,
        )
        .await
        .unwrap();

        assert_eq!(
            dispatch.count(),
            1,
            "an approved dangerous tool runs exactly once"
        );
    }

    #[tokio::test]
    async fn caution_command_parks_on_the_channel_seam_until_the_ui_answers() {
        // AC2 (T-5.11) at the loop level: a Caution command does NOT auto-run; the
        // loop parks on the ChannelConfirmHandler until the UI answers the surfaced
        // request. Approving runs the tool; the two halves are driven concurrently so
        // the loop genuinely waits on the channel (it can only proceed once we reply).
        use crate::approval::ChannelConfirmHandler;

        let provider = MockProvider::scripted(vec![
            tool_round(
                "toolu_1",
                "run_command",
                r#"{"command":["brew","install","wget"]}"#,
            ),
            end_round("done"),
        ]);
        let secrets = Secrets::new();
        let dispatch = RecordingDispatch::default();
        let turn = AgentTurn::new(&provider, &secrets);
        let (handler, mut rx) = ChannelConfirmHandler::new();
        let (etx, _erx) = mpsc::channel(256);
        let registry = ToolRegistry::default();
        let cancel = CancelToken::new();

        let run = turn.run(req(), &registry, &dispatch, &handler, &cancel, etx);
        let ui = async {
            let r = rx
                .recv()
                .await
                .expect("a Caution command must surface for approval");
            assert_eq!(r.assessment.level, Risk::Caution, "brew install is Caution");
            assert_eq!(r.call.name, "run_command");
            r.approve();
        };
        let (outcome, ()) = tokio::join!(run, ui);

        assert_eq!(outcome.unwrap().stop_reason, StopReason::EndTurn);
        assert_eq!(dispatch.count(), 1, "the approved Caution command ran once");
    }

    #[tokio::test]
    async fn caution_command_denied_over_the_channel_seam_is_never_run() {
        // AC2: denying over the same seam feeds back an is_error result and the tool
        // never reaches the dispatcher.
        use crate::approval::ChannelConfirmHandler;

        let provider = MockProvider::scripted(vec![
            tool_round(
                "toolu_1",
                "run_command",
                r#"{"command":["brew","install","wget"]}"#,
            ),
            end_round("stopped"),
        ]);
        let secrets = Secrets::new();
        let dispatch = RecordingDispatch::default();
        let turn = AgentTurn::new(&provider, &secrets);
        let (handler, mut rx) = ChannelConfirmHandler::new();
        let (etx, _erx) = mpsc::channel(256);
        let registry = ToolRegistry::default();
        let cancel = CancelToken::new();

        let run = turn.run(req(), &registry, &dispatch, &handler, &cancel, etx);
        let ui = async {
            rx.recv().await.expect("request surfaces").deny();
        };
        let (outcome, ()) = tokio::join!(run, ui);

        assert_eq!(outcome.unwrap().stop_reason, StopReason::EndTurn);
        assert_eq!(dispatch.count(), 0, "a denied command never runs");
        assert!(has_tool_result(
            &provider.requests()[1].messages,
            Some(true)
        ));
    }

    #[tokio::test]
    async fn safe_tool_auto_runs_without_consulting_the_approver() {
        let provider = MockProvider::scripted(vec![
            tool_round("toolu_s", "run_command", r#"{"command":["ls","-la"]}"#),
            end_round("done"),
        ]);
        let secrets = Secrets::new();
        let dispatch = RecordingDispatch::default();
        let approver = CountingApprover::default();
        let turn = AgentTurn::new(&provider, &secrets);
        let (etx, _erx) = mpsc::channel(256);

        turn.run(
            req(),
            &ToolRegistry::default(),
            &dispatch,
            &approver,
            &CancelToken::new(),
            etx,
        )
        .await
        .unwrap();

        // AUTO-SAFE: the safe command ran AND the approver was never consulted.
        assert_eq!(dispatch.count(), 1);
        assert_eq!(approver.calls.load(Ordering::SeqCst), 0);
    }

    // ---- AC#3: parallel reads concurrent, mutations serialize --------------

    struct BarrierDispatch {
        barrier: Arc<tokio::sync::Barrier>,
    }
    impl ToolDispatch for BarrierDispatch {
        async fn dispatch(&self, _input: ToolInput) -> ToolOutcome {
            // Only completes if another read reaches the barrier concurrently.
            self.barrier.wait().await;
            ToolOutcome::ok("read")
        }
    }

    #[tokio::test]
    async fn read_only_tools_run_concurrently() {
        let mut round = vec![ProviderEvent::MessageStart];
        for (id, path) in [("r1", "a"), ("r2", "b")] {
            round.push(ProviderEvent::ToolUseStart {
                id: id.to_string(),
                name: "read_file".to_string(),
            });
            round.push(ProviderEvent::ToolUseInputDelta {
                json: format!(r#"{{"path":"{path}"}}"#),
            });
            round.push(ProviderEvent::ToolUseStop);
        }
        round.push(ProviderEvent::MessageDelta {
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        });
        round.push(ProviderEvent::MessageStop);

        let provider = MockProvider::scripted(vec![round, end_round("done")]);
        let secrets = Secrets::new();
        let dispatch = BarrierDispatch {
            barrier: Arc::new(tokio::sync::Barrier::new(2)),
        };
        let turn = AgentTurn::new(&provider, &secrets);
        let (etx, _erx) = mpsc::channel(256);

        // If the two reads serialized, the barrier of 2 never releases and this
        // times out. Completing proves they ran concurrently.
        let res = tokio::time::timeout(
            Duration::from_secs(5),
            turn.run(
                req(),
                &ToolRegistry::default(),
                &dispatch,
                &ApproveAll,
                &CancelToken::new(),
                etx,
            ),
        )
        .await;
        assert!(res.is_ok(), "two read-only tools must run concurrently");
        assert_eq!(res.unwrap().unwrap().rounds, 2);
    }

    #[derive(Default)]
    struct SerialAssertDispatch {
        in_flight: Arc<AtomicUsize>,
        max_in_flight: Arc<AtomicUsize>,
        calls: Arc<AtomicUsize>,
    }
    impl ToolDispatch for SerialAssertDispatch {
        async fn dispatch(&self, _input: ToolInput) -> ToolOutcome {
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(now, Ordering::SeqCst);
            self.calls.fetch_add(1, Ordering::SeqCst);
            // Yield so a buggy concurrent scheduler would overlap two mutations.
            tokio::task::yield_now().await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            ToolOutcome::ok("written")
        }
    }

    #[tokio::test]
    async fn mutations_serialize() {
        let mut round = vec![ProviderEvent::MessageStart];
        for (id, path) in [("w1", "a"), ("w2", "b")] {
            round.push(ProviderEvent::ToolUseStart {
                id: id.to_string(),
                name: "write_file".to_string(),
            });
            round.push(ProviderEvent::ToolUseInputDelta {
                json: format!(r#"{{"path":"{path}","content":"x"}}"#),
            });
            round.push(ProviderEvent::ToolUseStop);
        }
        round.push(ProviderEvent::MessageDelta {
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        });
        round.push(ProviderEvent::MessageStop);

        let provider = MockProvider::scripted(vec![round, end_round("done")]);
        let secrets = Secrets::new();
        let dispatch = SerialAssertDispatch::default();
        let turn = AgentTurn::new(&provider, &secrets);
        let (etx, _erx) = mpsc::channel(256);

        // write_file is a mutation -> RequireConfirm; approve so both execute.
        tokio::time::timeout(
            Duration::from_secs(5),
            turn.run(
                req(),
                &ToolRegistry::default(),
                &dispatch,
                &ApproveAll,
                &CancelToken::new(),
                etx,
            ),
        )
        .await
        .expect("must not hang")
        .unwrap();

        assert_eq!(dispatch.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            dispatch.max_in_flight.load(Ordering::SeqCst),
            1,
            "mutations must never overlap"
        );
    }

    // ---- AC#4: provider-neutral --------------------------------------------

    async fn run_identity(
        name: &'static str,
        model: &'static str,
        scripts: Vec<Vec<ProviderEvent>>,
    ) -> (TurnOutcome, Vec<AgentEvent>) {
        let provider = MockProvider::scripted(scripts).with_identity(name, model);
        let secrets = Secrets::new();
        let dispatch = RecordingDispatch::default();
        let turn = AgentTurn::new(&provider, &secrets);
        let (etx, mut erx) = mpsc::channel(256);
        let outcome = turn
            .run(
                req(),
                &ToolRegistry::default(),
                &dispatch,
                &ApproveAll,
                &CancelToken::new(),
                etx,
            )
            .await
            .unwrap();
        (outcome, drain(&mut erx))
    }

    #[tokio::test]
    async fn the_same_loop_drives_both_provider_identities_unchanged() {
        let scripts = vec![
            tool_round("t", "run_command", r#"{"command":["echo","hi"]}"#),
            end_round("done"),
        ];
        let (anthropic, ev_a) = run_identity("anthropic", "claude-opus-4-8", scripts.clone()).await;
        let (openai, ev_o) = run_identity("openai", "gpt-5", scripts.clone()).await;
        assert_eq!(anthropic, openai);
        assert_eq!(ev_a, ev_o);
    }

    // ---- AC#5: output sanitized before re-entering context -----------------

    struct LeakyDispatch;
    impl ToolDispatch for LeakyDispatch {
        async fn dispatch(&self, _input: ToolInput) -> ToolOutcome {
            ToolOutcome::ok("file says sk-LEAK-CANARY-123456 then stops")
        }
    }

    #[tokio::test]
    async fn tool_output_is_sanitized_before_re_entering_context() {
        let mut secrets = Secrets::new();
        secrets.add_value("sk-LEAK-CANARY-123456");
        let provider = MockProvider::scripted(vec![
            tool_round("t", "read_file", r#"{"path":"creds"}"#),
            end_round("done"),
        ]);
        let dispatch = LeakyDispatch;
        let turn = AgentTurn::new(&provider, &secrets);
        let (etx, mut erx) = mpsc::channel(256);

        turn.run(
            req(),
            &ToolRegistry::default(),
            &dispatch,
            &ApproveAll,
            &CancelToken::new(),
            etx,
        )
        .await
        .unwrap();

        // The emitted timeline result is redacted ...
        let events = drain(&mut erx);
        let shown = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::ToolResult { output, .. } => Some(output.clone()),
                _ => None,
            })
            .unwrap();
        assert!(!shown.contains("LEAK-CANARY"));

        // ... and so is the tool_result fed back into the next request.
        let reqs = provider.requests();
        let fed_back = reqs[1]
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .find_map(|b| match b {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
            .unwrap();
        assert!(!fed_back.contains("LEAK-CANARY"));
    }

    // ---- AC#6: cancel aborts cleanly ---------------------------------------

    struct BlockingDispatch {
        started: mpsc::Sender<()>,
    }
    impl ToolDispatch for BlockingDispatch {
        async fn dispatch(&self, _input: ToolInput) -> ToolOutcome {
            let _ = self.started.send(()).await;
            // Never completes; only a cancel can end the turn.
            pending::<ToolOutcome>().await
        }
    }

    #[tokio::test]
    async fn cancel_aborts_the_loop_cleanly() {
        let provider = MockProvider::scripted(vec![
            tool_round("t", "run_command", r#"{"command":["ls"]}"#),
            end_round("unreached"),
        ]);
        let secrets = Secrets::new();
        let (started_tx, mut started_rx) = mpsc::channel::<()>(1);
        let dispatch = BlockingDispatch {
            started: started_tx,
        };
        let turn = AgentTurn::new(&provider, &secrets);
        let cancel = CancelToken::new();
        let registry = ToolRegistry::default();
        let (etx, _erx) = mpsc::channel(256);

        let result = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                turn.run(req(), &registry, &dispatch, &ApproveAll, &cancel, etx),
                async {
                    // Cancel once the (auto-safe) tool is mid-dispatch.
                    let _ = started_rx.recv().await;
                    cancel.cancel();
                }
            )
        })
        .await;

        assert!(result.is_ok(), "cancel must abort promptly, not hang");
        let (outcome, ()) = result.unwrap();
        let outcome = outcome.unwrap();
        assert!(outcome.cancelled);
        // The second round (end_round) was never requested.
        assert_eq!(provider.requests().len(), 1);
    }

    // ---- robustness: an unknown tool name is fed back as an error ----------

    #[tokio::test]
    async fn unknown_tool_is_reported_as_error_and_the_loop_continues() {
        let provider = MockProvider::scripted(vec![
            tool_round("t", "definitely_not_a_tool", r#"{"x":1}"#),
            end_round("recovered"),
        ]);
        let secrets = Secrets::new();
        let dispatch = RecordingDispatch::default();
        let turn = AgentTurn::new(&provider, &secrets);
        let (etx, _erx) = mpsc::channel(256);

        let outcome = turn
            .run(
                req(),
                &ToolRegistry::default(),
                &dispatch,
                &ApproveAll,
                &CancelToken::new(),
                etx,
            )
            .await
            .unwrap();

        assert_eq!(outcome.stop_reason, StopReason::EndTurn);
        assert_eq!(dispatch.count(), 0, "unknown tool is never dispatched");
        let reqs = provider.requests();
        assert!(has_tool_result(&reqs[1].messages, Some(true)));
    }

    // ---- robustness: a malformed tool call is reported, not dropped --------

    fn malformed_tool_round(id: &str, name: &str, bad_json: &str) -> Vec<ProviderEvent> {
        vec![
            ProviderEvent::MessageStart,
            ProviderEvent::ToolUseStart {
                id: id.to_string(),
                name: name.to_string(),
            },
            ProviderEvent::ToolUseInputDelta {
                json: bad_json.to_string(),
            },
            ProviderEvent::ToolUseStop,
            ProviderEvent::MessageDelta {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
            },
            ProviderEvent::MessageStop,
        ]
    }

    fn has_paired_tool_use(messages: &[Message], id: &str) -> bool {
        messages
            .iter()
            .flat_map(|m| &m.content)
            .any(|b| matches!(b, ContentBlock::ToolUse { id: i, .. } if i == id))
    }

    #[tokio::test]
    async fn malformed_only_round_feeds_back_an_error_and_continues() {
        let provider = MockProvider::scripted(vec![
            malformed_tool_round("toolu_bad", "edit_file", "{not valid json"),
            end_round("recovered"),
        ]);
        let secrets = Secrets::new();
        let dispatch = RecordingDispatch::default();
        let turn = AgentTurn::new(&provider, &secrets);
        let (etx, _erx) = mpsc::channel(256);

        let outcome = turn
            .run(
                req(),
                &ToolRegistry::default(),
                &dispatch,
                &ApproveAll,
                &CancelToken::new(),
                etx,
            )
            .await
            .unwrap();

        // The turn did NOT end prematurely on ToolUse: it looped to the follow-up.
        assert_eq!(outcome.stop_reason, StopReason::EndTurn);
        assert_eq!(outcome.rounds, 2);
        assert_eq!(
            dispatch.count(),
            0,
            "a malformed call never reaches the dispatcher"
        );
        // An is_error tool_result keyed to the malformed call was fed back, paired
        // with a placeholder tool_use of the same id (a valid round-trip).
        let reqs = provider.requests();
        assert!(has_tool_result(&reqs[1].messages, Some(true)));
        assert!(
            has_paired_tool_use(&reqs[1].messages, "toolu_bad"),
            "the placeholder tool_use must pair with the error result"
        );
    }

    #[tokio::test]
    async fn mixed_round_runs_the_valid_call_and_reports_the_malformed_one() {
        let mut round = vec![ProviderEvent::MessageStart];
        round.push(ProviderEvent::ToolUseStart {
            id: "ok".into(),
            name: "read_file".into(),
        });
        round.push(ProviderEvent::ToolUseInputDelta {
            json: r#"{"path":"a"}"#.into(),
        });
        round.push(ProviderEvent::ToolUseStop);
        round.push(ProviderEvent::ToolUseStart {
            id: "bad".into(),
            name: "edit_file".into(),
        });
        round.push(ProviderEvent::ToolUseInputDelta {
            json: "{broken".into(),
        });
        round.push(ProviderEvent::ToolUseStop);
        round.push(ProviderEvent::MessageDelta {
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        });
        round.push(ProviderEvent::MessageStop);

        let provider = MockProvider::scripted(vec![round, end_round("done")]);
        let secrets = Secrets::new();
        let dispatch = RecordingDispatch::default();
        let turn = AgentTurn::new(&provider, &secrets);
        let (etx, _erx) = mpsc::channel(256);

        turn.run(
            req(),
            &ToolRegistry::default(),
            &dispatch,
            &ApproveAll,
            &CancelToken::new(),
            etx,
        )
        .await
        .unwrap();

        // The valid read still runs; the malformed call is reported, not dropped.
        assert_eq!(dispatch.count(), 1);
        let reqs = provider.requests();
        assert!(
            has_tool_result(&reqs[1].messages, Some(false)),
            "the valid call has a success result"
        );
        assert!(
            has_tool_result(&reqs[1].messages, Some(true)),
            "the malformed call has an error result"
        );
        assert!(has_paired_tool_use(&reqs[1].messages, "bad"));
    }

    // ---- AC#6 (cont.): cancel also aborts during the streaming phase -------

    struct BlockingStreamProvider {
        started: mpsc::Sender<()>,
    }
    impl LlmProvider for BlockingStreamProvider {
        fn name(&self) -> &'static str {
            "blocking-stream"
        }
        fn default_model(&self) -> &'static str {
            "blocking-model"
        }
        async fn stream_turn(
            &self,
            _request: TurnRequest,
            sink: mpsc::Sender<ProviderEvent>,
        ) -> Result<(), ProviderError> {
            let _ = sink.send(ProviderEvent::MessageStart).await;
            let _ = self.started.send(()).await;
            // Never sends MessageStop; only a cancel can end the round.
            pending::<()>().await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn cancel_aborts_during_the_streaming_phase() {
        let (started_tx, mut started_rx) = mpsc::channel::<()>(1);
        let provider = BlockingStreamProvider {
            started: started_tx,
        };
        let secrets = Secrets::new();
        let dispatch = RecordingDispatch::default();
        let turn = AgentTurn::new(&provider, &secrets);
        let cancel = CancelToken::new();
        let registry = ToolRegistry::default();
        let (etx, _erx) = mpsc::channel(256);

        let result = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                turn.run(req(), &registry, &dispatch, &ApproveAll, &cancel, etx),
                async {
                    // Cancel while the provider round is still streaming.
                    let _ = started_rx.recv().await;
                    cancel.cancel();
                }
            )
        })
        .await;

        assert!(result.is_ok(), "stream-phase cancel must abort promptly");
        let (outcome, ()) = result.unwrap();
        assert!(outcome.unwrap().cancelled);
        assert_eq!(dispatch.count(), 0, "no tool ran - cancel hit mid-stream");
    }

    // ---- the pure helpers still hold ---------------------------------------

    #[test]
    fn dangerous_command_needs_confirm_regardless_of_model_claim() {
        let secrets = Secrets::new();
        let provider = MockProvider::new(vec![]);
        let turn = AgentTurn::new(&provider, &secrets);
        match turn.disposition_for_command("rm -rf ~") {
            ToolDisposition::NeedsConfirm(reasons) => {
                assert!(reasons.contains(&RiskReason::Destructive));
            }
            ToolDisposition::AutoRun => panic!("rm -rf ~ must never auto-run"),
        }
    }

    #[test]
    fn safe_command_auto_runs() {
        let secrets = Secrets::new();
        let provider = MockProvider::new(vec![]);
        let turn = AgentTurn::new(&provider, &secrets);
        assert_eq!(
            turn.disposition_for_command("ls -la"),
            ToolDisposition::AutoRun
        );
    }

    #[test]
    fn gate_and_sanitizer_cannot_drift_single_secrets_source() {
        // ONE `Secrets` instance feeds BOTH the risk gate and the sanitizer.
        let mut secrets = Secrets::new();
        secrets.add_sensitive_path("vault-keys");
        secrets.add_value("sk-live-DRIFT-CANARY-0987654321");
        let provider = MockProvider::new(vec![]);
        let turn = AgentTurn::new(&provider, &secrets);

        match turn.disposition_for_command("cat vault-keys") {
            ToolDisposition::NeedsConfirm(reasons) => {
                assert!(
                    reasons.contains(&RiskReason::SecretAccess),
                    "the registered sensitive path must drive a secret-path escalation"
                );
            }
            ToolDisposition::AutoRun => {
                panic!("a path added to the single Secrets source must never auto-run")
            }
        }

        let clean = turn.sanitize_observation("leak=sk-live-DRIFT-CANARY-0987654321 end", None);
        assert!(!clean.contains("DRIFT-CANARY"));
    }

    #[test]
    fn observation_is_sanitized() {
        let mut secrets = Secrets::new();
        secrets.add_value("sk-secret-value-xyz");
        let provider = MockProvider::new(vec![]);
        let turn = AgentTurn::new(&provider, &secrets);
        let clean = turn.sanitize_observation("token=sk-secret-value-xyz done", None);
        assert!(!clean.contains("sk-secret-value-xyz"));
    }

    #[test]
    fn file_mutations_require_confirmation_under_auto_safe() {
        let secrets = Secrets::new();
        let provider = MockProvider::new(vec![]);
        let turn = AgentTurn::new(&provider, &secrets);
        // A write is never provably safe: the gate over-approximates to confirm.
        let approval = turn.gate(&ToolInput::WriteFile(crate::tools::WriteFile {
            path: "a.txt".into(),
            content: "x".into(),
        }));
        assert!(matches!(approval, Approval::RequireConfirm(_)));
        // A read is auto-safe (output is sanitized before re-entering context).
        let read = turn.gate(&ToolInput::ReadFile(crate::tools::ReadFile {
            path: "a.txt".into(),
            range: None,
        }));
        assert!(matches!(read, Approval::AutoApprove));
    }

    #[tokio::test]
    async fn provider_error_is_surfaced_without_panic() {
        struct FailProvider;
        impl LlmProvider for FailProvider {
            fn name(&self) -> &'static str {
                "fail"
            }
            fn default_model(&self) -> &'static str {
                "fail-model"
            }
            async fn stream_turn(
                &self,
                _request: TurnRequest,
                _sink: mpsc::Sender<ProviderEvent>,
            ) -> Result<(), ProviderError> {
                Err(ProviderError::Http("boom".into()))
            }
        }

        let secrets = Secrets::new();
        let provider = FailProvider;
        let dispatch = RecordingDispatch::default();
        let turn = AgentTurn::new(&provider, &secrets);
        let (etx, mut erx) = mpsc::channel(16);
        let result = turn
            .run(
                req(),
                &ToolRegistry::default(),
                &dispatch,
                &ApproveAll,
                &CancelToken::new(),
                etx,
            )
            .await;
        assert!(result.is_err());
        assert!(drain(&mut erx)
            .iter()
            .any(|e| matches!(e, AgentEvent::Error(_))));
    }
}
