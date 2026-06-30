//! The live agent-turn runtime (ticket T-5.11): the glue that turns a submitted
//! agent prompt into a running [`aterm_agent::AgentTurn`] whose streamed steps land
//! in the single wall-clock timeline, gated by the deterministic risk gate and the
//! session's autonomy posture.
//!
//! This is the realization of the locked 3-thread architecture's agent seam
//! (CLAUDE.md): *"The agent runs on a tokio runtime off the render thread; SSE
//! deltas land by channel and mutate the current timeline entry incrementally."* The
//! channel is [`aterm_core::AgentInjector`] (the model-thread mailbox); the tokio
//! runtime here is that off-render-thread executor.
//!
//! Two pieces:
//!
//! 1. [`StreamProjector`] - a PURE, sink-generic mapper from [`AgentEvent`]s to
//!    timeline mutations. It coalesces consecutive thinking/assistant deltas into one
//!    in-place-extended block (the 60fps streaming path) and, for a proposed tool
//!    call, recomputes the SAME gate verdict the loop acts on (via
//!    [`aterm_agent::gate_tool`]) to draw the risk badge - so the badge can never
//!    drift from the execution decision. Unit-tested with a recording sink, no tokio,
//!    no window, no network.
//! 2. [`AgentRuntime`] / [`TurnHandle`] - the tokio runtime on dedicated worker
//!    threads, and a per-turn handle bridging the async approval/cancel seam back to
//!    the winit thread (an [`ApprovalRequest`] parked in a shared slot the keyboard
//!    resolves; a [`CancelToken`] for Esc; an `active` flag the router reads).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_agent::{
    badge_for_approval, gate_tool, gloss_for, AgentEvent, AgentTurn, AnthropicProvider, Approval,
    ApprovalPolicy, ApprovalRequest, CancelToken, ChannelConfirmHandler, Effort, LlmProvider,
    Message, MockProvider, OpenAiProvider, ProviderEvent, Secrets, Sinks, StopReason, ToolCall,
    ToolRegistry, TurnRequest, Usage,
};
use aterm_core::{AgentBadge, AgentBlock, AgentBlockKind, AgentInjector};
use tokio::sync::mpsc;

/// Depth of the turn-loop -> projector event channel. Generous; the projector drains
/// it as fast as the model thread accepts mailbox sends, and the turn loop produces
/// human-paced steps, so it never fills in practice.
const EVENT_CHANNEL_DEPTH: usize = 256;

/// Per-turn token ceiling. A pragmatic default until the EPIC-8 config loader; the
/// adaptive-thinking `effort` param (not `budget_tokens`) governs reasoning depth.
const DEFAULT_MAX_TOKENS: u32 = 8192;

/// The system prompt for a live turn. Concise: the safety spine (the gate + sandbox)
/// is enforced regardless of what the model is told, so this only sets the role.
const SYSTEM_PROMPT: &str =
    "You are aterm's built-in coding agent, operating inside a sandboxed macOS terminal. \
     Work in small steps: use the provided tools to inspect and modify the workspace, \
     and explain what you are doing. Every command you propose is independently risk-gated \
     and sandboxed by the terminal; destructive or shell-active commands require the user's \
     explicit approval.";

/// A timeline sink the [`StreamProjector`] writes through. Implemented for the real
/// [`AgentInjector`] (which relays to the model thread) and, in tests, for a recording
/// sink - so the projector's mapping logic is verifiable without a window or threads.
pub trait BlockSink {
    /// Append a new agent block to the timeline.
    fn push_block(&self, block: AgentBlock);
    /// Extend the trailing agent block's text in place (the streaming delta path).
    fn append_text(&self, delta: &str);
}

impl BlockSink for AgentInjector {
    fn push_block(&self, block: AgentBlock) {
        AgentInjector::push_block(self, block);
    }
    fn append_text(&self, delta: &str) {
        AgentInjector::append_text(self, delta.to_string());
    }
}

impl<T: BlockSink + ?Sized> BlockSink for &T {
    fn push_block(&self, block: AgentBlock) {
        (**self).push_block(block);
    }
    fn append_text(&self, delta: &str) {
        (**self).append_text(delta);
    }
}

/// Maps the agent turn's [`AgentEvent`] stream onto timeline mutations over a
/// [`BlockSink`] (ticket T-5.11). Pure and synchronous: no I/O, no async, no shared
/// state beyond the open-streaming-block cursor - so it is exhaustively unit-testable.
///
/// Coalescing: consecutive [`AgentEvent::Thinking`] (or consecutive
/// [`AgentEvent::Assistant`]) deltas extend the same open block in place via
/// [`BlockSink::append_text`]; any other event closes the open block so the next text
/// delta starts a fresh one. This is the 60fps streaming contract - a long stream
/// mutates only the tail entry.
pub struct StreamProjector<S: BlockSink> {
    sink: S,
    policy: ApprovalPolicy,
    secrets: Secrets,
    registry: ToolRegistry,
    /// The kind of the currently-open streaming block (`Thinking` / `AssistantText`),
    /// or `None` when the last emitted block is not text-extendable.
    open: Option<AgentBlockKind>,
}

impl<S: BlockSink> StreamProjector<S> {
    /// A projector writing through `sink`, computing tool-call badges against the same
    /// `policy` and `secrets` the turn was gated with, so the badge matches the loop's
    /// execution decision.
    pub fn new(sink: S, policy: ApprovalPolicy, secrets: Secrets) -> Self {
        Self {
            sink,
            policy,
            secrets,
            registry: ToolRegistry::with_default_tools(),
            open: None,
        }
    }

    /// Project one event onto the timeline.
    pub fn apply(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Thinking(delta) => self.stream(AgentBlockKind::Thinking, &delta),
            AgentEvent::Assistant(delta) => self.stream(AgentBlockKind::AssistantText, &delta),
            AgentEvent::ToolProposed(call) => {
                self.open = None;
                let (text, badge) = self.describe_tool(&call);
                let mut block = AgentBlock::new(AgentBlockKind::ToolCall, text, Instant::now())
                    .with_tool_use_id(call.id);
                if let Some(badge) = badge {
                    block = block.with_badge(badge);
                }
                self.sink.push_block(block);
            }
            AgentEvent::ToolResult {
                id,
                output,
                is_error,
            } => {
                self.open = None;
                self.sink.push_block(
                    AgentBlock::new(AgentBlockKind::ToolResult, output, Instant::now())
                        .with_tool_use_id(id)
                        .with_error(is_error),
                );
            }
            AgentEvent::ToolProposalFailed { id, name, error } => {
                self.open = None;
                self.sink.push_block(
                    AgentBlock::new(
                        AgentBlockKind::ToolResult,
                        format!("{name}: {error}"),
                        Instant::now(),
                    )
                    .with_tool_use_id(id)
                    .with_error(true),
                );
            }
            AgentEvent::Error(message) => {
                self.open = None;
                self.sink.push_block(
                    AgentBlock::new(
                        AgentBlockKind::AssistantText,
                        format!("[error] {message}"),
                        Instant::now(),
                    )
                    .with_error(true),
                );
            }
            // Token accounting and the turn boundary are not timeline steps; the
            // `active` flag (not a block) reflects turn completion.
            AgentEvent::Usage(_) | AgentEvent::TurnComplete { .. } => {}
        }
    }

    /// Append a streamed text/thinking delta: extend the open block of the same kind
    /// in place, else open a new one.
    fn stream(&mut self, kind: AgentBlockKind, delta: &str) {
        if self.open == Some(kind) {
            self.sink.append_text(delta);
        } else {
            self.sink
                .push_block(AgentBlock::new(kind, delta, Instant::now()));
            self.open = Some(kind);
        }
    }

    /// The display text + risk badge for a proposed tool call. Parses the call, gates
    /// it through [`gate_tool`] (the crown-jewel decision, shared with the loop), and
    /// maps the verdict to an [`AgentBadge`]. A call whose arguments do not parse gets
    /// a plain label and no badge (its failure surfaces as the `is_error` result the
    /// loop feeds back).
    fn describe_tool(&self, call: &ToolCall) -> (String, Option<AgentBadge>) {
        match self.registry.parse(call) {
            Ok(input) => {
                let approval = gate_tool(&input, &self.policy, &self.secrets);
                let badge = badge_for_approval(&approval);
                (describe_call(&call.name, &approval), Some(badge))
            }
            Err(error) => (
                format!("{} (could not parse arguments: {error})", call.name),
                None,
            ),
        }
    }
}

/// The human-facing one-line description of a gated tool call: the tool name plus the
/// verdict, glossing the parsed risk reasons for a non-auto decision (ticket T-5.11
/// AC3). Mirrors `transcript::render_tool_call` so the live and recorded views read
/// the same.
fn describe_call(name: &str, approval: &Approval) -> String {
    match approval {
        Approval::AutoApprove => format!("{name} (auto)"),
        Approval::RequireConfirm(assessment) => {
            if assessment.reasons.is_empty() {
                format!("{name} (needs approval)")
            } else {
                let glossed: Vec<&str> = assessment.reasons.iter().map(|r| gloss_for(*r)).collect();
                format!("{name} (needs approval: {})", glossed.join("; "))
            }
        }
    }
}

/// A handle to one in-flight agent turn (ticket T-5.11), owned by the [`Session`] on
/// the winit thread. It bridges the async turn (running on the [`AgentRuntime`]'s
/// tokio threads) back to the keyboard:
///
/// - `active` flips to `false` when the turn finishes, so the router knows whether Esc
///   should interrupt.
/// - `pending` holds the [`ApprovalRequest`] the loop is currently parked on (if any),
///   put there by a bridge task; the keyboard resolves it via [`Self::approve_pending`]
///   / [`Self::deny_pending`].
/// - `cancel` is the Esc/interrupt signal.
///
/// [`Session`]: crate::session::Session
pub struct TurnHandle {
    cancel: CancelToken,
    pending: Arc<Mutex<Option<ApprovalRequest>>>,
    active: Arc<AtomicBool>,
}

impl TurnHandle {
    /// Whether the turn is still running (the router's `agent_turn_active`).
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// Whether the loop is currently parked awaiting an approval decision.
    #[must_use]
    pub fn has_pending_approval(&self) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Approve the pending gated call (it runs). Returns whether there was one.
    pub fn approve_pending(&self) -> bool {
        if let Some(req) = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            req.approve();
            true
        } else {
            false
        }
    }

    /// Deny the pending gated call (it is fed back as an error result; the turn
    /// continues). Returns whether there was one.
    pub fn deny_pending(&self) -> bool {
        if let Some(req) = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            req.deny();
            true
        } else {
            false
        }
    }

    /// Interrupt the whole turn (Esc): signal cancellation and fail-closed deny any
    /// approval the loop is parked on, so it unblocks promptly instead of waiting on a
    /// decision that will never come.
    pub fn cancel(&self) {
        self.cancel.cancel();
        if let Some(req) = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            req.deny();
        }
    }
}

/// The off-render-thread agent executor (ticket T-5.11): a tokio runtime on dedicated
/// worker threads plus the per-session workspace root and [`Secrets`] source the turn
/// is gated and sandboxed against. One is owned by the [`Session`].
///
/// The runtime is held in an `Option` so it can be shut down explicitly at teardown
/// (see [`Self::shutdown`]) BEFORE the engine's model thread is joined - a live turn's
/// task holds an [`AgentInjector`] (a clone of the engine's model-mailbox sender), and
/// that clone must be released first or `Engine::drop`'s join would hang.
pub struct AgentRuntime {
    rt: Option<tokio::runtime::Runtime>,
    root: PathBuf,
    secrets: Secrets,
}

impl AgentRuntime {
    /// Build the runtime rooted at `root` (the workspace the agent's writes are
    /// confined to and its commands default their cwd to) with the single `secrets`
    /// deny-set that feeds the gate, the sandbox, and the sanitizer alike.
    pub fn new(root: PathBuf, secrets: Secrets) -> std::io::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("aterm-agent")
            .build()?;
        Ok(Self {
            rt: Some(rt),
            root,
            secrets,
        })
    }

    /// Shut the runtime down (bounded), dropping any in-flight turn task so its
    /// [`AgentInjector`] clone is released. Called from [`Session`]'s drop BEFORE the
    /// engine is dropped, so the model thread can then observe its mailbox disconnect
    /// and join cleanly (aterm-core's zero-hang shutdown invariant). Bounded via
    /// `shutdown_timeout` so a sandboxed command still executing in a blocking task
    /// cannot stall process exit (the command self-terminates via its own kill-timeout).
    ///
    /// [`Session`]: crate::session::Session
    pub fn shutdown(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_timeout(Duration::from_millis(250));
        }
    }

    /// Start a turn for `prompt` under `policy`, streaming its steps into the timeline
    /// through `injector`. Returns immediately with a [`TurnHandle`]; the turn runs on
    /// the tokio threads. The user's prompt is pushed as the opening timeline block so
    /// the turn is anchored even before the first model delta.
    pub fn start_turn(
        &self,
        prompt: String,
        policy: ApprovalPolicy,
        injector: AgentInjector,
    ) -> TurnHandle {
        let cancel = CancelToken::new();
        let pending: Arc<Mutex<Option<ApprovalRequest>>> = Arc::new(Mutex::new(None));
        let active = Arc::new(AtomicBool::new(true));

        // Anchor the turn with the user's prompt block (wall-clock first).
        injector.push_block(AgentBlock::new(
            AgentBlockKind::UserPrompt,
            prompt.clone(),
            Instant::now(),
        ));

        // Live for the whole session; only `None` transiently during teardown, when no
        // new turn is ever started.
        let rt = self
            .rt
            .as_ref()
            .expect("agent runtime is live while the session runs");

        let (handler, approvals_rx) = ChannelConfirmHandler::new();
        // Bridge approval requests from the loop into the shared slot the keyboard reads.
        rt.spawn(bridge_approvals(approvals_rx, Arc::clone(&pending)));

        let secrets = self.secrets.clone();
        let root = self.root.clone();
        let cancel_task = cancel.clone();
        let active_task = Arc::clone(&active);
        let pending_task = Arc::clone(&pending);
        rt.spawn(async move {
            run_turn(
                select_provider(),
                prompt,
                policy,
                secrets,
                root,
                injector,
                handler,
                &cancel_task,
            )
            .await;
            // The turn has ended (completed, errored, or cancelled): fail-closed deny
            // any approval still parked (the loop is gone), THEN clear the active flag -
            // in that order so a winit-thread reader never observes `active == false`
            // alongside a live pending request.
            if let Some(req) = pending_task
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
            {
                req.deny();
            }
            active_task.store(false, Ordering::SeqCst);
        });

        TurnHandle {
            cancel,
            pending,
            active,
        }
    }
}

/// Drain approval requests from the loop's [`ChannelConfirmHandler`] into the shared
/// slot the winit thread resolves. The loop awaits each confirm before proposing the
/// next tool, so at most one is ever pending; a defensive double-park denies the older.
async fn bridge_approvals(
    mut rx: mpsc::Receiver<ApprovalRequest>,
    pending: Arc<Mutex<Option<ApprovalRequest>>>,
) {
    while let Some(req) = rx.recv().await {
        let prev = pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(req);
        if let Some(old) = prev {
            old.deny();
        }
    }
}

/// The provider chosen for a turn. Selected from the environment: a real client when
/// an API key is present, else the keyless [`MockProvider`] demo so the whole UX (auto
/// badge, approval card, interleaving, cancel) is exercisable with zero setup. Key
/// custody is T-8.3 (out of scope); reading the env directly is the interim.
enum SelectedProvider {
    Anthropic(AnthropicProvider),
    OpenAi(OpenAiProvider),
    Mock(MockProvider),
}

/// Pick a provider from `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`, falling back to the
/// keyless demo mock.
fn select_provider() -> SelectedProvider {
    if let Some(key) = nonempty_env("ANTHROPIC_API_KEY") {
        log::info!("agent: using the Anthropic provider");
        return SelectedProvider::Anthropic(AnthropicProvider::new(key));
    }
    if let Some(key) = nonempty_env("OPENAI_API_KEY") {
        log::info!("agent: using the OpenAI provider");
        return SelectedProvider::OpenAi(OpenAiProvider::new(key));
    }
    log::info!("agent: no API key set; using the keyless mock provider (demo)");
    SelectedProvider::Mock(MockProvider::scripted(demo_script()))
}

/// A trimmed, non-empty environment variable, or `None`.
fn nonempty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Run the selected provider's turn to completion (monomorphized per provider).
#[allow(clippy::too_many_arguments)]
async fn run_turn(
    provider: SelectedProvider,
    prompt: String,
    policy: ApprovalPolicy,
    secrets: Secrets,
    root: PathBuf,
    injector: AgentInjector,
    handler: ChannelConfirmHandler,
    cancel: &CancelToken,
) {
    match provider {
        SelectedProvider::Anthropic(p) => {
            drive(&p, prompt, policy, secrets, root, injector, handler, cancel).await;
        }
        SelectedProvider::OpenAi(p) => {
            drive(&p, prompt, policy, secrets, root, injector, handler, cancel).await;
        }
        SelectedProvider::Mock(p) => {
            drive(&p, prompt, policy, secrets, root, injector, handler, cancel).await;
        }
    }
}

/// The generic turn driver: build the dispatch sinks + request, run the loop, and
/// pump its events through the projector into the timeline - concurrently, so a
/// streamed delta is projected as it arrives.
#[allow(clippy::too_many_arguments)]
async fn drive<P: LlmProvider>(
    provider: &P,
    prompt: String,
    policy: ApprovalPolicy,
    secrets: Secrets,
    root: PathBuf,
    injector: AgentInjector,
    handler: ChannelConfirmHandler,
    cancel: &CancelToken,
) {
    let registry = ToolRegistry::with_default_tools();
    let sinks = Sinks::seatbelt(root, secrets.clone());
    let turn = AgentTurn::with_policy(provider, &secrets, policy.clone());

    let request = TurnRequest {
        model: provider.default_model().to_string(),
        system: Some(SYSTEM_PROMPT.to_string()),
        messages: vec![Message::user(prompt)],
        tools: registry.specs(),
        effort: Effort::Medium,
        max_tokens: DEFAULT_MAX_TOKENS,
    };

    let (events_tx, mut events_rx) = mpsc::channel::<AgentEvent>(EVENT_CHANNEL_DEPTH);
    let mut projector = StreamProjector::new(injector, policy, secrets.clone());

    let pump = async {
        while let Some(event) = events_rx.recv().await {
            projector.apply(event);
        }
    };
    let run = turn.run(request, &registry, &sinks, &handler, cancel, events_tx);

    let (result, ()) = tokio::join!(run, pump);
    match result {
        Ok(outcome) => log::info!(
            "agent turn finished: {:?} ({} rounds, cancelled={})",
            outcome.stop_reason,
            outcome.rounds,
            outcome.cancelled
        ),
        Err(e) => log::warn!("agent turn error: {e}"),
    }
}

/// The keyless demo script (no API key): three provider rounds that exercise the full
/// approval UX - a Safe read-only tool that auto-runs (the `auto` badge + a result), a
/// file write that parks on approval (the `APPROVE?` badge + the parsed gloss, awaiting
/// the keyboard), then a closing message. Each inner vec is one `stream_turn` round.
fn demo_script() -> Vec<Vec<ProviderEvent>> {
    fn tool_round(
        thinking: Option<&str>,
        text: &str,
        id: &str,
        name: &str,
        json: &str,
    ) -> Vec<ProviderEvent> {
        let mut events = vec![ProviderEvent::MessageStart];
        if let Some(t) = thinking {
            events.push(ProviderEvent::ThinkingDelta(t.to_string()));
        }
        events.push(ProviderEvent::TextDelta(text.to_string()));
        events.push(ProviderEvent::ToolUseStart {
            id: id.to_string(),
            name: name.to_string(),
        });
        events.push(ProviderEvent::ToolUseInputDelta {
            json: json.to_string(),
        });
        events.push(ProviderEvent::ToolUseStop);
        events.push(ProviderEvent::MessageDelta {
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        });
        events.push(ProviderEvent::MessageStop);
        events
    }

    vec![
        // Round 1: a Safe, read-only tool - auto-runs, no approval needed.
        tool_round(
            Some("Let me get my bearings by looking at the current directory."),
            "I'll list the files here first.",
            "demo-list",
            "list_dir",
            r#"{"path":"."}"#,
        ),
        // Round 2: a file write - always Caution, so it parks on the keyboard.
        tool_round(
            None,
            "Now I'd like to save a short note to disk.",
            "demo-write",
            "write_file",
            r#"{"path":"aterm-demo-note.txt","content":"Hello from the aterm agent demo.\n"}"#,
        ),
        // Round 3: wrap up and end the turn.
        vec![
            ProviderEvent::MessageStart,
            ProviderEvent::TextDelta("All done - that's the keyless demo turn.".to_string()),
            ProviderEvent::MessageDelta {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            },
            ProviderEvent::MessageStop,
        ],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;

    /// A synchronous recording sink that mirrors the engine's append-in-place
    /// semantics, so the projector's coalescing is testable without threads.
    #[derive(Default)]
    struct RecordingSink {
        blocks: RefCell<Vec<AgentBlock>>,
    }

    impl BlockSink for RecordingSink {
        fn push_block(&self, block: AgentBlock) {
            self.blocks.borrow_mut().push(block);
        }
        fn append_text(&self, delta: &str) {
            if let Some(last) = self.blocks.borrow_mut().last_mut() {
                last.push_text(delta);
            }
        }
    }

    fn projector(sink: &RecordingSink) -> StreamProjector<&RecordingSink> {
        StreamProjector::new(sink, ApprovalPolicy::new(), Secrets::new())
    }

    #[test]
    fn consecutive_text_deltas_coalesce_into_one_extended_block() {
        // The 60fps streaming contract: a run of assistant deltas extends ONE block in
        // place rather than pushing a block per delta.
        let sink = RecordingSink::default();
        let mut p = projector(&sink);
        p.apply(AgentEvent::Assistant("Hello ".into()));
        p.apply(AgentEvent::Assistant("there ".into()));
        p.apply(AgentEvent::Assistant("world".into()));
        let blocks = sink.blocks.borrow();
        assert_eq!(blocks.len(), 1, "three deltas should be one block");
        assert_eq!(blocks[0].kind, AgentBlockKind::AssistantText);
        assert_eq!(blocks[0].text, "Hello there world");
        assert!(blocks[0].version >= 2, "two appends bump the version");
    }

    #[test]
    fn thinking_then_assistant_open_separate_blocks() {
        // A change of kind closes the open block, so thinking and prose never merge.
        let sink = RecordingSink::default();
        let mut p = projector(&sink);
        p.apply(AgentEvent::Thinking("hmm".into()));
        p.apply(AgentEvent::Assistant("answer".into()));
        let blocks = sink.blocks.borrow();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].kind, AgentBlockKind::Thinking);
        assert_eq!(blocks[1].kind, AgentBlockKind::AssistantText);
    }

    #[test]
    fn safe_tool_call_gets_the_auto_badge_and_joins_its_result() {
        // AC1: a Safe (read-only) tool auto-runs and appears with the `auto` badge.
        let sink = RecordingSink::default();
        let mut p = projector(&sink);
        p.apply(AgentEvent::Assistant("listing".into()));
        p.apply(AgentEvent::ToolProposed(ToolCall {
            id: "t1".into(),
            name: "list_dir".into(),
            input: json!({ "path": "." }),
        }));
        p.apply(AgentEvent::ToolResult {
            id: "t1".into(),
            output: "a\nb".into(),
            is_error: false,
        });
        let blocks = sink.blocks.borrow();
        assert_eq!(blocks.len(), 3);
        // The tool-call block closed the open assistant block (not coalesced).
        assert_eq!(blocks[1].kind, AgentBlockKind::ToolCall);
        assert_eq!(blocks[1].badge, Some(AgentBadge::Auto));
        assert_eq!(blocks[1].tool_use_id.as_deref(), Some("t1"));
        assert!(blocks[1].text.contains("auto"), "text: {}", blocks[1].text);
        assert_eq!(blocks[2].kind, AgentBlockKind::ToolResult);
        assert_eq!(blocks[2].tool_use_id.as_deref(), Some("t1"));
        assert!(!blocks[2].is_error);
    }

    #[test]
    fn file_write_tool_needs_approval_and_shows_the_gloss() {
        // AC2/AC3: a file mutation parks on approval, badged NeedsApproval, with the
        // parsed risk gloss in its label.
        let sink = RecordingSink::default();
        let mut p = projector(&sink);
        p.apply(AgentEvent::ToolProposed(ToolCall {
            id: "w".into(),
            name: "write_file".into(),
            input: json!({ "path": "note.txt", "content": "x" }),
        }));
        let blocks = sink.blocks.borrow();
        assert_eq!(blocks[0].badge, Some(AgentBadge::NeedsApproval));
        assert!(
            blocks[0].text.contains("writes a file to disk"),
            "gloss should show, got: {}",
            blocks[0].text
        );
    }

    #[test]
    fn dangerous_command_is_blocked() {
        // A destructive command is badged Blocked (the strongest verdict).
        let sink = RecordingSink::default();
        let mut p = projector(&sink);
        p.apply(AgentEvent::ToolProposed(ToolCall {
            id: "rm".into(),
            name: "run_command".into(),
            input: json!({ "command": ["rm", "-rf", "/"] }),
        }));
        let blocks = sink.blocks.borrow();
        assert_eq!(blocks[0].badge, Some(AgentBadge::Blocked));
    }

    #[test]
    fn unparseable_tool_call_renders_without_a_badge() {
        // A malformed argument set cannot be gated; it surfaces plainly (its failure
        // arrives as the loop's is_error result), never silently dropped.
        let sink = RecordingSink::default();
        let mut p = projector(&sink);
        p.apply(AgentEvent::ToolProposed(ToolCall {
            id: "bad".into(),
            name: "run_command".into(),
            input: json!({ "wrong_field": 1 }),
        }));
        let blocks = sink.blocks.borrow();
        assert_eq!(blocks[0].kind, AgentBlockKind::ToolCall);
        assert_eq!(blocks[0].badge, None);
    }

    #[test]
    fn tool_proposal_failed_becomes_an_error_result_block() {
        let sink = RecordingSink::default();
        let mut p = projector(&sink);
        p.apply(AgentEvent::ToolProposalFailed {
            id: "x".into(),
            name: "run_command".into(),
            error: "truncated json".into(),
        });
        let blocks = sink.blocks.borrow();
        assert_eq!(blocks[0].kind, AgentBlockKind::ToolResult);
        assert!(blocks[0].is_error);
        assert_eq!(blocks[0].tool_use_id.as_deref(), Some("x"));
    }

    #[test]
    fn usage_and_turn_complete_emit_no_block() {
        let sink = RecordingSink::default();
        let mut p = projector(&sink);
        p.apply(AgentEvent::Usage(Usage::default()));
        p.apply(AgentEvent::TurnComplete {
            stop_reason: StopReason::EndTurn,
        });
        assert!(sink.blocks.borrow().is_empty());
    }

    #[test]
    fn agent_runtime_shutdown_is_bounded_and_idempotent() {
        // The teardown path (ticket T-5.11): shutdown() drops the runtime so an
        // in-flight turn's AgentInjector clone is released before the engine is joined.
        // Guard that it drops the runtime and is safe to call more than once (Session's
        // Drop calls it; a double call must not panic).
        let mut rt = AgentRuntime::new(PathBuf::from("."), Secrets::new()).expect("runtime builds");
        assert!(rt.rt.is_some());
        rt.shutdown();
        assert!(rt.rt.is_none(), "shutdown drops the runtime");
        rt.shutdown(); // idempotent
    }

    #[test]
    fn demo_script_has_three_rounds_and_a_terminal_end_turn() {
        // The keyless demo must terminate (an EndTurn round), or the loop would run to
        // its round cap.
        let script = demo_script();
        assert_eq!(script.len(), 3);
        let last = script.last().unwrap();
        assert!(last.iter().any(|e| matches!(
            e,
            ProviderEvent::MessageDelta {
                stop_reason: StopReason::EndTurn,
                ..
            }
        )));
    }
}
