//! Block model: the Warp-style "command + its output" unit. A `Block` is opened
//! by OSC-133 `B`/`C` (command typed / output begins) and closed by `D` (with an
//! exit code). `BlockList` holds the session's blocks; `BlockSegmenter` drives
//! segmentation from the typed [`Mark`]s produced by [`crate::osc`].

use std::ops::Range;
use std::time::Instant;

use crate::osc::{Mark, PromptKind};
use crate::terminal::SnapshotCell;

/// An immutable snapshot of one output row's cells, captured when a block finishes.
///
/// Finished blocks own their rows (a copy of the grid region) rather than pointing
/// into the live grid, so command history is immune to alacritty's reflow/eviction
/// when the window resizes - the prototype's key design choice (ticket T-2.4,
/// [`03-pty-vt-rust.md`] Recommendation 4). v1 stores rows directly; Warp's packed
/// `FlatStorage` is a deferred memory optimization for huge logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowSnapshot {
    pub cells: Vec<SnapshotCell>,
}

impl RowSnapshot {
    #[must_use]
    pub fn new(cells: Vec<SnapshotCell>) -> Self {
        Self { cells }
    }
}

/// A half-open byte span `[start, end)` into the session's logical output stream.
/// `end == None` means the span is still open (command running).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputSpan {
    pub start: usize,
    pub end: Option<usize>,
}

impl OutputSpan {
    fn open(start: usize) -> Self {
        Self { start, end: None }
    }
}

/// Default display policy: a finished block whose captured output exceeds this many
/// rows is collapsed in the timeline to its first `COLLAPSED_OUTPUT_ROWS` rows plus a
/// single "... +N lines" affordance row (ticket T-2.7, AC4). This caps a single
/// block's contribution to the scroll height, so one `cargo build` or `git log` cannot
/// dominate the timeline; the hidden rows stay in the immutable snapshot and are
/// revealed by the (future) expand affordance. A tuning default, NOT a protocol
/// constant - revisit against the T-7.x perf/UX matrix; the per-block expand toggle is
/// a follow-up (EPIC-3/4 input). Mirrors the precedent of [`Block::is_thin`] living in
/// core: the height that feeds the [`HeightIndex`] IS the renderer's geometry contract,
/// so the collapse decision belongs with it (one coordinate space).
pub const COLLAPSED_OUTPUT_ROWS: u64 = 16;

/// One command block: a shell command and the output it produced. The
/// [`Block::Command`] timeline variant wraps this; agent transcript steps
/// ([`Block::Agent`], ticket T-5.10) interleave with these in the same
/// wall-clock [`BlockList`].
#[derive(Debug, Clone)]
pub struct CommandBlock {
    /// The command line as typed (between OSC-133 `B` and `C`). May be empty if
    /// the shell did not report it or output started immediately.
    pub command: String,
    /// Byte span of this command's output in the logical stream.
    pub output_span: OutputSpan,
    /// Exit code from OSC-133 `D`, once the command finished.
    pub exit_code: Option<i32>,
    /// When the prompt for this block started (OSC-133 `A`).
    pub started_at: Instant,
    /// When the command finished (OSC-133 `D`).
    pub finished_at: Option<Instant>,
    /// Reported working directory at block start (from OSC-7), if known.
    pub cwd: Option<String>,
    /// Immutable snapshot of this block's output rows, captured when it finishes so
    /// it survives later grid reflow/eviction. Empty while the block is running and
    /// until the lifecycle driver (ticket T-2.5) copies the grid region in on `D`;
    /// once populated these are owned rows, never aliased to the live grid.
    pub output: Vec<RowSnapshot>,
    /// This block ran a full-screen (alt-screen) application - vim, htop, less - so
    /// it lives "outside" the normal scrollback: it is rendered as a compact
    /// "ran <cmd>" marker rather than a captured-output card, and no output rows are
    /// snapshotted (the alt screen is ephemeral). Set by the lifecycle driver when
    /// the alt screen activates while this block is running (ticket T-2.5).
    ///
    /// (This is the lightweight, non-breaking stand-in for the ticket's `Interactive`
    /// block VARIANT; the full `Block` enum redesign is the owner-confirm item from
    /// T-2.4, to be designed once alongside Epic-5's agent variants.)
    pub interactive: bool,
    /// This block was segmented by the heuristic fallback ([`HeuristicSegmenter`]),
    /// not by trusted OSC-133 marks (ticket T-2.6). Its boundaries are a best-effort
    /// guess (prompt-line detection), it has no authoritative command text, cwd, or
    /// exit code, and the renderer labels it as approximate so the user knows blocks
    /// are not integration-confirmed. Mark-driven blocks set this `false`.
    ///
    /// (Like [`Block::interactive`], a flag stand-in until the `Block` enum redesign.)
    pub approximate: bool,
}

impl CommandBlock {
    /// Is this block still running (no `D` seen)?
    pub fn is_running(&self) -> bool {
        self.finished_at.is_none()
    }

    /// Did the command succeed (exit 0)? `None` until it finishes.
    pub fn succeeded(&self) -> Option<bool> {
        self.exit_code.map(|c| c == 0)
    }

    /// Wall-clock duration in seconds, once the command has finished (`D` seen);
    /// `None` while still running. Drives the timeline block-meta's duration readout
    /// (ticket T-9.3). Fixed the moment the block finishes, so it is stable to fold
    /// into a render damage-signature.
    #[must_use]
    pub fn duration_secs(&self) -> Option<f64> {
        self.finished_at
            .map(|f| f.duration_since(self.started_at).as_secs_f64())
    }

    /// A finished, non-interactive command that produced NO output (e.g. `true`,
    /// `cd`): the renderer collapses these to a thin marker line rather than an empty
    /// output card (ticket T-2.5, AC4).
    ///
    /// Keyed off the captured `output` rows now that the engine snapshots them on `D`
    /// (ticket T-2.7): a command is thin exactly when it finished with no captured
    /// output rows. A command that emits only a zero-width control sequence (a bare
    /// cursor move or SGR reset) produces no visible rows and is correctly thin; one
    /// that prints anything visible is not. (Earlier this conservatively keyed off the
    /// `C`..`D` byte span, erring toward an empty card; the captured rows are the exact
    /// signal.)
    #[must_use]
    pub fn is_thin(&self) -> bool {
        !self.interactive && !self.is_running() && self.output.is_empty()
    }

    /// This block's RAW height in grid rows: one row for the command/prompt line
    /// plus every captured output row, uncollapsed. (A still-running block reports 1
    /// until its output is snapshotted on finish; the live tail block's on-screen
    /// height is the renderer's concern, ticket T-2.7.) The height the timeline
    /// actually reserves - and the [`HeightIndex`] tracks - is
    /// [`Self::display_height_rows`], which collapses long output.
    #[must_use]
    pub fn height_rows(&self) -> u64 {
        1 + self.output.len() as u64
    }

    /// The number of output rows actually shown when this block is rendered: capped
    /// at [`COLLAPSED_OUTPUT_ROWS`] for a long FINISHED block, else all of them.
    ///
    /// The currently-RUNNING (active) block is never collapsed - it shows all of its
    /// live output, like a live terminal, and the timeline pins to the bottom so the
    /// latest output stays on screen (ticket T-4.6). Collapse is only for SETTLED
    /// scrollback, so one finished `cargo build` cannot dominate the timeline (T-2.7).
    #[must_use]
    pub fn shown_output_rows(&self) -> u64 {
        let n = self.output.len() as u64;
        if self.is_running() {
            n
        } else {
            n.min(COLLAPSED_OUTPUT_ROWS)
        }
    }

    /// `Some(hidden_row_count)` when this (FINISHED) block's output is collapsed (more
    /// than [`COLLAPSED_OUTPUT_ROWS`] rows captured), else `None`. The running block is
    /// never collapsed (it shows its live tail), so this is always `None` while running.
    /// Drives the "... +N lines" affordance the timeline draws (ticket T-2.7, AC4).
    #[must_use]
    pub fn collapsed_hidden_rows(&self) -> Option<u64> {
        if self.is_running() {
            return None;
        }
        let n = self.output.len() as u64;
        (n > COLLAPSED_OUTPUT_ROWS).then(|| n - COLLAPSED_OUTPUT_ROWS)
    }

    /// This block's height in *display* rows - what the timeline reserves and what
    /// the [`HeightIndex`] tracks, so the virtualized renderer's scroll geometry
    /// matches what is drawn (ticket T-2.7). Equal to [`Self::height_rows`] until a
    /// FINISHED block's output exceeds [`COLLAPSED_OUTPUT_ROWS`], after which it
    /// collapses to: the command line + `COLLAPSED_OUTPUT_ROWS` shown rows + one
    /// "... +N lines" affordance row. A RUNNING block never collapses (it shows its
    /// full live output; ticket T-4.6), so its display height is always
    /// [`Self::height_rows`]. Collapsing HERE (not only in the renderer) keeps a single
    /// coordinate space: `block_at` / `blocks_in_viewport` / scroll-to-block all agree
    /// with the drawn layout.
    #[must_use]
    pub fn display_height_rows(&self) -> u64 {
        match self.collapsed_hidden_rows() {
            // command line + the shown (capped) rows + the "... +N lines" row.
            Some(_) => 1 + COLLAPSED_OUTPUT_ROWS + 1,
            None => self.height_rows(),
        }
    }
}

/// Which agent transcript step a [`Block::Agent`] timeline entry renders (ticket
/// T-5.10). Mirrors the locked `AgentStep` variant set (`06-agent-architecture.md`
/// section e): the rich step model - risk assessment, gate decision, raw tool
/// input - lives in `aterm-agent`'s transcript (`AgentStep`); THIS is the
/// agent-domain-FREE render projection that lives in the single wall-clock
/// timeline next to command blocks. Keeping it free of agent types is what
/// preserves the one-way crate arrow (`aterm-agent -> aterm-core`, never the
/// reverse): `aterm-core` must not name an LLM/agent type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentBlockKind {
    /// The user's request that opened the turn.
    UserPrompt,
    /// A chunk of the model's (summarized) thinking.
    Thinking,
    /// A chunk of assistant prose.
    AssistantText,
    /// A tool the model proposed (text is the glossed name + risk/decision).
    ToolCall,
    /// A tool's sanitized result.
    ToolResult,
    /// An approval prompt's resolution (auto / user-approved / user-declined).
    Approval,
}

/// The agent-domain-FREE risk-gate verdict a tool-call step carries for the
/// timeline badge (ticket T-5.11). Mirrors the three risk-gate badge states
/// (`07-ia-design-language.md` §5) WITHOUT naming `aterm-agent`'s `Risk` /
/// `RiskAssessment` (the one-way crate arrow forbids the dependency):
/// `aterm-agent`'s `to_block()` maps a deterministic `RiskAssessment` + gate
/// decision onto this, and `aterm-ui` maps it onto its own local `RiskState` for
/// the chip styling. The auto-safe default means a proven-safe, non-shell-active
/// command is [`Auto`](AgentBadge::Auto); an escalated command is
/// [`NeedsApproval`](AgentBadge::NeedsApproval); a destructive (`Dangerous`)
/// verdict is [`Blocked`](AgentBadge::Blocked). Color is ALWAYS paired with a text
/// label downstream (color-blind safety); this enum is the label-bearing datum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentBadge {
    /// Auto-approved: proven `Safe` with no shell-active reason. Renders "auto".
    Auto,
    /// Escalated; needs explicit confirmation. Renders "APPROVE?".
    NeedsApproval,
    /// Destructive / blocked; requires an explicit override. Renders "BLOCKED".
    Blocked,
}

/// One agent transcript step projected into the timeline (ticket T-5.10).
///
/// It carries only what the renderer needs: a [`kind`](AgentBlockKind) tag, the
/// display `text` (already glossed/sanitized upstream by `aterm-agent` - this type
/// never sees a secret value or a raw `RiskAssessment`), the `tool_use_id` join
/// key (so a ToolCall/Approval/ToolResult triple can be correlated visually), a
/// wall-clock `started_at`, an `is_error` flag, and a `version` the streaming path
/// bumps on every text delta. The version is the 60fps lever: a streamed delta
/// mutates ONLY this entry's text + version, so the renderer's damage gate redraws
/// just this entry and never relays out the whole timeline per delta (ties to
/// T-2.7 / T-1.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentBlock {
    pub kind: AgentBlockKind,
    pub text: String,
    pub tool_use_id: Option<String>,
    pub started_at: Instant,
    pub is_error: bool,
    pub version: u64,
    /// The risk-gate verdict for a [`ToolCall`](AgentBlockKind::ToolCall) step
    /// (ticket T-5.11), used to draw its badge. `None` for non-tool-call kinds
    /// (a prompt / thinking / assistant prose / result has no gate verdict). This
    /// is the agent-domain-FREE projection of the deterministic gate decision -
    /// `aterm-agent` sets it via [`with_badge`](AgentBlock::with_badge); the
    /// renderer maps it to a chip without ever naming an agent type.
    pub badge: Option<AgentBadge>,
}

impl AgentBlock {
    /// A fresh agent entry at wall-clock `started_at`, version 0.
    #[must_use]
    pub fn new(kind: AgentBlockKind, text: impl Into<String>, started_at: Instant) -> Self {
        Self {
            kind,
            text: text.into(),
            tool_use_id: None,
            started_at,
            is_error: false,
            version: 0,
            badge: None,
        }
    }

    /// Attach the `tool_use_id` join key (chainable).
    #[must_use]
    pub fn with_tool_use_id(mut self, id: impl Into<String>) -> Self {
        self.tool_use_id = Some(id.into());
        self
    }

    /// Attach the risk-gate badge verdict (ticket T-5.11; chainable). Set by
    /// `aterm-agent` when projecting a `ToolCall` step so the renderer can draw the
    /// gate badge without naming an agent type.
    #[must_use]
    pub fn with_badge(mut self, badge: AgentBadge) -> Self {
        self.badge = Some(badge);
        self
    }

    /// Mark this entry as carrying an error result (chainable).
    #[must_use]
    pub fn with_error(mut self, is_error: bool) -> Self {
        self.is_error = is_error;
        self
    }

    /// Append streamed text and bump the version - the incremental-mutation path
    /// (ticket T-5.10 AC2). [`BlockList::append_agent_text`] re-derives this
    /// entry's display height afterward so the [`HeightIndex`] stays in step.
    pub fn push_text(&mut self, delta: &str) {
        self.text.push_str(delta);
        self.version = self.version.wrapping_add(1);
    }

    /// The number of text lines this step occupies (always >= 1). Agent steps are
    /// never collapsed in v1, so this IS the display height.
    #[must_use]
    pub fn line_count(&self) -> u64 {
        self.text.bytes().filter(|&b| b == b'\n').count() as u64 + 1
    }
}

/// One entry in the single wall-clock timeline. A [`BlockList`] interleaves human
/// command blocks ([`Block::Command`]) with agent transcript steps
/// ([`Block::Agent`], ticket T-5.10) in append (wall-clock) order - the locked
/// single-timeline design (`06-agent-architecture.md` section e). The renderer
/// virtualizes over the uniform display-height geometry exposed here; the variant
/// only changes WHAT is drawn, not how the timeline is laid out.
///
/// This replaces the former `Block` struct + `interactive`/`approximate` flag
/// stand-ins: the flags now live on the [`CommandBlock`] payload, and the agent
/// variant is the real thing the T-2.4 note deferred to "be designed alongside
/// Epic-5's agent variants".
#[derive(Debug, Clone)]
pub enum Block {
    /// A shell command and its output.
    Command(CommandBlock),
    /// An agent transcript step (ticket T-5.10).
    Agent(AgentBlock),
}

impl Block {
    /// Borrow the command payload, if this entry is a command block.
    #[must_use]
    pub fn as_command(&self) -> Option<&CommandBlock> {
        match self {
            Block::Command(c) => Some(c),
            Block::Agent(_) => None,
        }
    }

    /// Borrow the agent payload, if this entry is an agent step.
    #[must_use]
    pub fn as_agent(&self) -> Option<&AgentBlock> {
        match self {
            Block::Agent(a) => Some(a),
            Block::Command(_) => None,
        }
    }

    /// When this entry started, in wall-clock terms - the key the timeline orders
    /// by (insertion order IS wall-clock order; see [`BlockList`]).
    #[must_use]
    pub fn started_at(&self) -> Instant {
        match self {
            Block::Command(c) => c.started_at,
            Block::Agent(a) => a.started_at,
        }
    }

    /// Is this a still-running command (no `D` yet)? Agent steps are settled the
    /// moment they are recorded, so they are never "running".
    #[must_use]
    pub fn is_running(&self) -> bool {
        match self {
            Block::Command(c) => c.is_running(),
            Block::Agent(_) => false,
        }
    }

    /// This entry's RAW height in grid rows (uncollapsed).
    #[must_use]
    pub fn height_rows(&self) -> u64 {
        match self {
            Block::Command(c) => c.height_rows(),
            Block::Agent(a) => a.line_count(),
        }
    }

    /// This entry's height in *display* rows - what the timeline reserves and the
    /// [`HeightIndex`] tracks (collapsing long FINISHED command output, ticket
    /// T-2.7; agent steps never collapse).
    #[must_use]
    pub fn display_height_rows(&self) -> u64 {
        match self {
            Block::Command(c) => c.display_height_rows(),
            Block::Agent(a) => a.line_count(),
        }
    }
}

/// The ordered list of blocks for a session, with a [`HeightIndex`] giving
/// O(log n) viewport queries for the virtualized timeline renderer (ticket T-2.4).
///
/// The index mirrors each block's [`Block::display_height_rows`] (the collapsed
/// display height, ticket T-2.7); it is kept in step by [`Self::push`] (a new block)
/// and [`Self::set_block_output`] (a block's output snapshot lands, growing - and
/// possibly collapsing - its height). Mutating a block's cwd/exit/finished flags via
/// [`Self::last_mut`] does not change its height, so the index stays consistent
/// without an explicit update there.
///
/// `Clone` so the model thread can publish an immutable `Arc<BlockList>` snapshot to
/// the render thread (ticket T-2.7) - mirroring the grid `Snapshot` publish. Blocks
/// are small (the output capture that would make them heavy is a follow-up), and the
/// clone happens only when the list actually changes, not per frame.
#[derive(Debug, Default, Clone)]
pub struct BlockList {
    blocks: Vec<Block>,
    index: HeightIndex,
}

impl BlockList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Block> {
        self.blocks.iter()
    }

    /// Borrow block `i`, if present.
    #[must_use]
    pub fn get(&self, i: usize) -> Option<&Block> {
        self.blocks.get(i)
    }

    /// The most recent block, if any.
    pub fn last(&self) -> Option<&Block> {
        self.blocks.last()
    }

    /// Total height of all blocks in display rows (the timeline's scroll extent).
    #[must_use]
    pub fn total_height_rows(&self) -> u64 {
        self.index.total()
    }

    /// The display-row offset of block `i`'s top edge from the top of the timeline
    /// (the cumulative display height of all blocks before it) - O(log n) via the
    /// height index. The virtualized renderer uses this to place a block on screen
    /// relative to the scroll position (ticket T-2.7).
    #[must_use]
    pub fn block_top_row(&self, i: usize) -> u64 {
        self.index.prefix(i)
    }

    /// The half-open range of block indices intersecting a viewport of `viewport_h`
    /// rows whose top edge is `scroll_top` rows from the top of the timeline -
    /// O(log n) via the height index (ticket T-2.4). Empty range when there is
    /// nothing to show / the scroll is past the end.
    #[must_use]
    pub fn blocks_in_viewport(&self, scroll_top: u64, viewport_h: u64) -> Range<usize> {
        self.index.blocks_in_viewport(scroll_top, viewport_h)
    }

    /// Replace command block `i`'s output snapshot (the lifecycle driver, T-2.5,
    /// calls this on `D` with the captured grid rows) and update the height index to
    /// match its new *display* height (collapsing long output, ticket T-2.7). No-op
    /// if `i` is out of range or is an agent entry.
    pub fn set_block_output(&mut self, i: usize, rows: Vec<RowSnapshot>) {
        if let Some(Block::Command(c)) = self.blocks.get_mut(i) {
            c.output = rows;
            let h = self.blocks[i].display_height_rows();
            self.index.set(i, h);
        }
    }

    /// Replace the output snapshot of the most recent COMMAND entry (skipping any
    /// interleaved agent steps, ticket T-5.10) and update its display height.
    /// Returns whether a command entry was found.
    ///
    /// The engine's capture path targets the command it is currently capturing,
    /// which is always the *last command* in the list - agent steps pushed after it
    /// (in wall-clock order) do not move it, so this replaces the old "running block
    /// == last block" assumption that interleaving would break.
    pub fn set_last_command_output(&mut self, rows: Vec<RowSnapshot>) -> bool {
        let Some(i) = self
            .blocks
            .iter()
            .rposition(|b| matches!(b, Block::Command(_)))
        else {
            return false;
        };
        if let Block::Command(c) = &mut self.blocks[i] {
            c.output = rows;
        }
        let h = self.blocks[i].display_height_rows();
        self.index.set(i, h);
        true
    }

    /// Append an agent transcript step to the timeline (ticket T-5.10), returning
    /// its index. Wall-clock ordering is the append order, so the caller MUST push
    /// steps as they are emitted (not batch-insert later) to interleave correctly
    /// with concurrently-running command blocks.
    pub fn push_agent(&mut self, agent: AgentBlock) -> usize {
        let i = self.blocks.len();
        self.push(Block::Agent(agent));
        i
    }

    /// Append `delta` to agent entry `i`'s text and keep the height index in step
    /// (ticket T-5.10 AC2 - the incremental-mutation path). A point-update of one
    /// Fenwick node: only entry `i`'s height changes, never the whole timeline.
    /// Returns whether `i` is an agent entry.
    pub fn append_agent_text(&mut self, i: usize, delta: &str) -> bool {
        if let Some(Block::Agent(a)) = self.blocks.get_mut(i) {
            a.push_text(delta);
            let h = self.blocks[i].display_height_rows();
            self.index.set(i, h);
            true
        } else {
            false
        }
    }

    /// Append `delta` to the LAST agent entry's text (ticket T-5.11): the streaming
    /// path the engine's agent-injection mailbox uses, so a text/thinking delta
    /// extends the open agent block in place without the caller threading an index
    /// across the thread boundary. A point-update of one Fenwick node. Returns whether
    /// a trailing agent entry was found to extend.
    pub fn append_last_agent_text(&mut self, delta: &str) -> bool {
        match self
            .blocks
            .iter()
            .rposition(|b| matches!(b, Block::Agent(_)))
        {
            Some(i) => self.append_agent_text(i, delta),
            None => false,
        }
    }

    /// The most recent still-RUNNING command entry (skipping agent steps and
    /// finished commands). The segmenter mutates the open command through this, so
    /// an interleaved agent step that happens to be `last()` cannot be mistaken for
    /// the running command.
    fn last_running_command_mut(&mut self) -> Option<&mut CommandBlock> {
        self.blocks.iter_mut().rev().find_map(|b| match b {
            Block::Command(c) if c.is_running() => Some(c),
            _ => None,
        })
    }

    /// The most recent command entry regardless of run state (for setting the
    /// command text just after a block opens, when it is still running).
    fn last_command_mut(&mut self) -> Option<&mut CommandBlock> {
        self.blocks.iter_mut().rev().find_map(|b| match b {
            Block::Command(c) => Some(c),
            Block::Agent(_) => None,
        })
    }

    fn push(&mut self, block: Block) {
        self.index.push(block.display_height_rows());
        self.blocks.push(block);
    }
}

/// A Fenwick (binary-indexed) tree over per-block heights - the "SumTree" the
/// timeline renderer virtualizes over (ticket T-2.4). Append, point-update, total,
/// prefix-sum, and the "which block is at row Y" query are all O(log n).
///
/// Why a Fenwick tree rather than a flat prefix-sum `Vec`: the running (tail) block
/// grows every frame, and a flat prefix array costs O(n) per height change; Fenwick
/// makes both the update and the viewport query O(log n) against a 10k-block list.
/// It is the right fit for an append-mostly list (new blocks at the end, the tail
/// updated). The full Warp B+-tree `SumTree` - cheap arbitrary mid-insert + multiple
/// summary dimensions - is the upgrade path if mid-list insertion or extra summaries
/// are ever needed; flagged for the owner (see ticket notes). Removal (eviction)
/// rebuilds the tree, which is fine for the rare batch-evict-oldest case.
#[derive(Debug, Clone)]
pub struct HeightIndex {
    /// Per-block heights (rows) - the source of truth.
    heights: Vec<u64>,
    /// 1-indexed Fenwick tree; `tree[0]` is unused. `len == heights.len() + 1`.
    tree: Vec<u64>,
}

impl Default for HeightIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl HeightIndex {
    #[must_use]
    pub fn new() -> Self {
        // tree starts as `[0]` (the unused slot 0) so the 1-indexed invariant
        // `tree.len() == heights.len() + 1` holds from empty. Derived `Default`
        // would give an empty tree and break that invariant.
        Self {
            heights: Vec::new(),
            tree: vec![0],
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.heights.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.heights.is_empty()
    }

    /// Append a block of `height` rows. O(log n).
    ///
    /// Builds `tree[i]` from its already-present children: a Fenwick node at 1-based
    /// index `i` covers the range `(i - lowbit(i), i]`, so its sum is the new
    /// element plus the child subtrees `tree[i-1], tree[i-1-lowbit, ...]` down to
    /// `i - lowbit(i) + 1`. (We cannot use the usual upward point-update on append:
    /// it propagates to PARENT indices, which are larger and do not exist yet.)
    pub fn push(&mut self, height: u64) {
        self.heights.push(height);
        let i = self.heights.len(); // 1-based index of the new element
        let lowbit = i & i.wrapping_neg();
        let mut sum = height;
        let mut j = i - 1;
        while j > i - lowbit {
            sum += self.tree[j];
            j -= j & j.wrapping_neg();
        }
        self.tree.push(sum);
    }

    /// Set block `idx`'s height. O(log n). Panics if `idx` is out of range.
    pub fn set(&mut self, idx: usize, height: u64) {
        let delta = height as i64 - self.heights[idx] as i64;
        self.heights[idx] = height;
        self.fenwick_add(idx + 1, delta);
    }

    /// Remove block `idx`, rebuilding the tree (O(n) - the rare eviction path).
    pub fn remove(&mut self, idx: usize) {
        self.heights.remove(idx);
        self.rebuild();
    }

    fn rebuild(&mut self) {
        self.tree = vec![0; self.heights.len() + 1];
        for i in 0..self.heights.len() {
            self.fenwick_add(i + 1, self.heights[i] as i64);
        }
    }

    /// Fenwick point-update: add `delta` (signed) to 1-based index `i`.
    fn fenwick_add(&mut self, mut i: usize, delta: i64) {
        let n = self.heights.len();
        while i <= n {
            self.tree[i] = self.tree[i].wrapping_add_signed(delta);
            i += i & i.wrapping_neg(); // += lowest set bit
        }
    }

    /// Sum of the first `count` blocks' heights (the top edge of block `count`).
    #[must_use]
    pub fn prefix(&self, count: usize) -> u64 {
        let mut sum = 0u64;
        let mut i = count.min(self.heights.len());
        while i > 0 {
            sum += self.tree[i];
            i -= i & i.wrapping_neg();
        }
        sum
    }

    /// Total height of all blocks (rows).
    #[must_use]
    pub fn total(&self) -> u64 {
        self.prefix(self.heights.len())
    }

    /// The 0-based index of the block containing row `row` (i.e. the block whose
    /// `[prefix(k), prefix(k+1))` row range contains `row`), or `None` if `row` is
    /// at or past the total height. O(log n) via a Fenwick binary lift.
    #[must_use]
    pub fn block_at(&self, row: u64) -> Option<usize> {
        let n = self.heights.len();
        if n == 0 || row >= self.total() {
            return None;
        }
        // Largest `pos` whose cumulative height is <= row; that block contains row.
        let mut pos = 0usize;
        let mut acc = 0u64;
        // Highest power of two <= n. (usize::BITS and usize::leading_zeros are both
        // 64-bit here, so this is exact - a `n as u32` cast would mismatch the width
        // and seed an oversized step that just wastes ~32 loop iterations per query.)
        let mut step = 1usize << (usize::BITS - 1 - n.leading_zeros());
        while step != 0 {
            let next = pos + step;
            if next <= n && acc + self.tree[next] <= row {
                pos = next;
                acc += self.tree[next];
            }
            step >>= 1;
        }
        Some(pos)
    }

    /// The half-open block range intersecting `[scroll_top, scroll_top+viewport_h)`
    /// rows. O(log n). Empty when there are no blocks, `viewport_h == 0`, or the
    /// scroll is at/past the total height.
    #[must_use]
    pub fn blocks_in_viewport(&self, scroll_top: u64, viewport_h: u64) -> Range<usize> {
        let n = self.heights.len();
        if n == 0 || viewport_h == 0 {
            return 0..0;
        }
        let total = self.total();
        if scroll_top >= total {
            return n..n;
        }
        let start = self.block_at(scroll_top).unwrap_or(0);
        let last_row = scroll_top.saturating_add(viewport_h - 1).min(total - 1);
        let end = self.block_at(last_row).map_or(n, |e| e + 1);
        start..end.min(n)
    }
}

/// Coarse phase the segmenter is in, mirroring the OSC-133 lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Before any prompt, or after `D` and before the next `A`.
    Idle,
    /// Saw `A`: a prompt is being drawn; user is (about to be) typing.
    Prompt,
    /// Saw `B`: command input captured; awaiting `C`.
    Command,
    /// Saw `C`: command is running, output is accumulating.
    Output,
}

/// Drives [`BlockList`] from a stream of [`Mark`]s plus the running byte offset.
///
/// The caller advances `offset` as it appends bytes to the logical stream and
/// hands marks here in stream order. Command-text capture between `B` and `C`
/// is partial: we record the offset where it begins, and the app layer is
/// expected to fill `command` from the captured prompt-input bytes.
/// TODO(ticket EPIC-2): capture the literal command text between `B` and `C`
/// directly here once the input echo path feeds this module.
#[derive(Debug)]
pub struct BlockSegmenter {
    phase: Phase,
    /// Offset into the logical stream where the *current* command's input began
    /// (set at `B`), used to slice the command text later.
    command_start_offset: Option<usize>,
    /// CWD reported most recently via OSC-7, applied to the next opened block.
    pending_cwd: Option<String>,
    /// Authoritative command text decoded from a nonce-matched `C;cmdline=` (or OSC
    /// 633 `E`), staged by the [`Mark::CommandLine`] that precedes `OutputStart` and
    /// consumed when the block opens (ticket T-2.5).
    pending_command: Option<String>,
    /// Whether the alt screen is currently active. The caller (engine) sets this at
    /// FIRE TIME via [`Self::set_alt_screen`] - after draining the grid to the
    /// mark's offset - because the alt-screen-toggling CSI may still be unprocessed
    /// passthrough when a mark is first seen. While set, marks are suppressed (a
    /// full-screen TUI's own OSC-133 marks must not fabricate phantom blocks).
    alt_screen: bool,
}

impl Default for BlockSegmenter {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockSegmenter {
    pub fn new() -> Self {
        Self {
            phase: Phase::Idle,
            command_start_offset: None,
            pending_cwd: None,
            pending_command: None,
            alt_screen: false,
        }
    }

    /// Update the alt-screen state from the drained emulator (ticket T-2.5). On the
    /// transition into the alt screen, the currently-running command is the one that
    /// launched the full-screen app, so it becomes a compact `Interactive` block (no
    /// captured output) rather than a normal output card. Call this at fire time -
    /// after feeding the grid up to the mark's offset - so the flag is accurate.
    pub fn set_alt_screen(&mut self, alt_screen: bool, list: &mut BlockList) {
        if alt_screen && !self.alt_screen {
            // Entering the alt screen. The block that ran the TUI is the one whose
            // output is currently open (phase Output) - the `Phase::Output` guard
            // ensures we never flag a stale block left running by an earlier
            // missing-`D` (whose phase would not be Output) as interactive.
            if self.phase == Phase::Output {
                if let Some(c) = list.last_running_command_mut() {
                    c.interactive = true;
                }
            }
            // The launching command is now the TUI; drop any command line staged
            // for a not-yet-opened block so it cannot leak into a later one.
            self.pending_command = None;
        }
        self.alt_screen = alt_screen;
    }

    /// Apply one mark at byte `offset` in the logical stream, mutating `list`.
    pub fn apply(&mut self, mark: &Mark, offset: usize, list: &mut BlockList) {
        // Alt-screen suppression (ticket T-2.5): while a full-screen app owns the
        // screen, drop ALL marks so its own OSC-133 chatter cannot fabricate phantom
        // blocks. The launching command is already flagged Interactive (see
        // set_alt_screen); the real prompt cycle resumes once the app exits and the
        // alt screen is off (the closing `D` then fires with alt_screen == false).
        if self.alt_screen {
            return;
        }
        match mark {
            Mark::CommandLine(cmd) => {
                // Decoded, nonce-matched command line (OSC 133 `C;cmdline=` / 633
                // `E`); the scanner emits it just before `OutputStart`. Stage it for
                // the block that opens next.
                self.pending_command = Some(cmd.clone());
            }
            // The shell version (ticket T-2.3 AC2) is consumed by the engine for the
            // integration indicator, not by block segmentation - ignore it here.
            Mark::ShellVersion(_) => {}
            Mark::Cwd(path) => {
                self.pending_cwd = Some(path.clone());
                // If a command block is open, update its cwd too.
                if let Some(c) = list.last_running_command_mut() {
                    c.cwd = Some(path.clone());
                }
            }
            Mark::Prompt(PromptKind::PromptStart) => {
                // Missing-`D` recovery (AC3): a fresh prompt while a block is still
                // open means the command never reported `D` (Ctrl-C, a kill, a crash
                // mid-output). Auto-close the orphan with an UNKNOWN exit (None) so
                // it is finalized rather than dangling forever.
                if let Some(c) = list.last_running_command_mut() {
                    c.output_span.end = Some(offset);
                    c.finished_at = Some(Instant::now());
                    // exit_code stays None == unknown.
                }
                self.phase = Phase::Prompt;
                self.command_start_offset = None;
                self.pending_command = None;
            }
            Mark::Prompt(PromptKind::CommandStart) => {
                // `B`: command input begins.
                self.phase = Phase::Command;
                self.command_start_offset = Some(offset);
            }
            Mark::Prompt(PromptKind::OutputStart) => {
                // `C`: a new block opens; its output span starts here. An empty Enter
                // (A->B->A with no C) never reaches here, so no phantom empty block
                // is created (AC4); a command that runs but emits nothing yields a
                // block whose output span stays empty -> `Block::is_thin`.
                self.phase = Phase::Output;
                list.push(Block::Command(CommandBlock {
                    command: self.pending_command.take().unwrap_or_default(),
                    output_span: OutputSpan::open(offset),
                    exit_code: None,
                    started_at: Instant::now(),
                    finished_at: None,
                    cwd: self.pending_cwd.clone(),
                    // The immutable output snapshot lands on `D` via
                    // BlockList::set_block_output (grid-row capture is a follow-up).
                    output: Vec::new(),
                    interactive: false,
                    // Mark-driven: authoritative, not an approximation.
                    approximate: false,
                }));
            }
            Mark::Prompt(PromptKind::CommandDone { exit_code }) => {
                // `D`: close the current block.
                if let Some(c) = list.last_running_command_mut() {
                    c.output_span.end = Some(offset);
                    c.exit_code = *exit_code;
                    c.finished_at = Some(Instant::now());
                }
                self.phase = Phase::Idle;
                self.command_start_offset = None;
                self.pending_command = None;
            }
        }
    }

    /// Set the command text for the most recently opened (running) block. The app
    /// layer calls this once it has sliced the input echo between `B` and `C`.
    pub fn set_last_command(&self, list: &mut BlockList, command: impl Into<String>) {
        if let Some(c) = list.last_command_mut() {
            c.command = command.into();
        }
    }
}

/// The labeled-heuristic block detector (ticket T-2.6, ADR-0008,
/// [`04-shell-integration.md`] recommendation 7). Used ONLY when a supported shell
/// fails to confirm our nonce-matched OSC-133 marks (see
/// [`crate::integration::IntegrationMonitor::heuristic_active`]): rather than show a
/// broken/blank block UI, it approximates command boundaries and labels every block
/// it makes [`Block::approximate`] so the user knows they are not mark-confirmed.
///
/// **Signal: the dossier's structural "newline + cursor-at-col-0" prompt detection,
/// NOT a prompt-text/sigil match.** A sigil match (a line ending in `$`/`%`/`#`/`>`)
/// is fragile: it produces zero blocks for the very common `❯`/arrow prompts of
/// starship/powerlevel10k, and it false-fires on REPL sub-prompts, `PS2` line
/// continuations, and any output line that happens to end in such a glyph. Instead
/// this keys off structure the shell cannot avoid:
///
/// - **Quiescence + cursor not at column 0 = sitting at a prompt.** When output
///   settles (the engine's coalescing tick fires with nothing more to drain) and the
///   cursor rests mid-line (`col > 0`), the shell has typically drawn a prompt and is
///   waiting for input. A pause that rests at column 0 (the last line ended in a
///   newline) is an output pause, not a prompt. This holds for any prompt theme.
/// - **A newline since the last prompt = a command actually ran.** The engine feeds
///   every clean output byte through [`Self::observe_output`], which counts newlines.
///   Typing on the prompt line echoes characters but emits NO newline until Enter, so
///   re-sampling the same prompt while the user types adds no block (fixes the
///   "phantom block per keystroke" trap); a real command submits at least one newline.
///   Counting newlines (not comparing cursor rows) is also robust to scrollback.
/// - **A carriage-return-redrawn line is NOT a prompt.** A running command's in-place
///   progress bar (`Downloading 45%`, `npm install`, `cargo` spinner) redraws its line
///   with `\r` and stalls mid-line, which would otherwise look exactly like a settled
///   prompt. We track whether the current line was reached via a `\r` redraw and
///   suppress the prompt signal there, so a progress bar does not fragment its command
///   into spurious blocks.
///
/// Each detected command cycle is emitted as one **finished** approximate block
/// spanning `[previous prompt offset, this prompt offset)` - created retroactively at
/// the *next* prompt, so there is never a phantom empty/leading block and the
/// in-flight command is simply not bracketed until it completes. It carries no
/// command text, cwd, or exit code (those need real hooks).
///
/// **Inherent limit (degrade-honestly note).** A pure output-stream heuristic cannot
/// perfectly tell a settled shell prompt from a command that printed a partial line
/// (no `\r`, no trailing newline) and is *waiting* - a `Password:` prompt, `read -p`,
/// or a bespoke inline progress indicator. Such a command may get one extra labeled
/// approximate block. This is the open product question in [`04-shell-integration.md`]
/// (the labeled-heuristic fallback vs. an honest "no blocks" mode) and is flagged for
/// the owner; the blocks are always labeled `approximate`, never mark-confirmed.
///
/// The engine must drive it only while heuristic-active and NOT on the alt screen (a
/// full-screen TUI's cursor would otherwise fabricate boundaries, the same hazard the
/// mark-driven [`BlockSegmenter`] guards against). When marks later confirm, the
/// engine stops driving it and the authoritative segmenter takes over.
#[derive(Debug, Default)]
pub struct HeuristicSegmenter {
    /// Have we anchored the first prompt yet? Until then there is no interval to
    /// close, so a login banner before the first prompt creates no block.
    seen_prompt: bool,
    /// Clean-stream offset of the most recently anchored prompt - the open edge of
    /// the next approximate block's span.
    last_prompt_offset: usize,
    /// Newlines fed since the last anchored prompt. Zero means "the user is only
    /// typing at the prompt" (no command ran), so re-detecting the prompt is a no-op.
    newlines_since_prompt: usize,
    /// Whether the cursor's current line was reached by an in-place `\r` redraw (and
    /// not yet ended by a newline) - i.e. a progress-bar line, NOT a prompt. Set by
    /// a lone `\r`, cleared by the next `\n`. Suppresses the prompt signal so a
    /// stalled progress bar does not fabricate blocks.
    current_line_has_cr: bool,
}

impl HeuristicSegmenter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed the clean (passthrough) output bytes so the detector can track its two
    /// signals: the newline count ("a command actually ran") and whether the current
    /// line was `\r`-redrawn (a progress bar, not a prompt). One pass over the chunk;
    /// the engine calls this for every chunk it feeds to the grid while heuristic and
    /// off the alt screen.
    pub fn observe_output(&mut self, clean_bytes: &[u8]) {
        for &b in clean_bytes {
            match b {
                b'\n' => {
                    self.newlines_since_prompt += 1;
                    // A newline starts a fresh line - any prior `\r` redraw on the
                    // old line is irrelevant now (this also resolves `\r\n`, where
                    // the `\r` is just part of the line ending, to "not redrawn").
                    self.current_line_has_cr = false;
                }
                b'\r' => self.current_line_has_cr = true,
                _ => {}
            }
        }
    }

    /// Note that the terminal has gone idle, with the cursor at column `cursor_col`
    /// and the clean stream at byte `offset`. Call at each *idle* publish (the
    /// coalescing flush), while heuristic-active and off the alt screen.
    ///
    /// A column-0 cursor (a fresh line / output pause) or a `\r`-redrawn current line
    /// (a progress bar) is not a settled prompt and is ignored. Otherwise the shell is
    /// sitting at a prompt: if a newline has been seen since the last prompt a command
    /// cycle completed, so emit the finished approximate block for
    /// `[last_prompt_offset, offset)` and re-anchor here. The first prompt only anchors
    /// (no preceding command to bracket).
    pub fn note_prompt_if_idle(&mut self, cursor_col: usize, offset: usize, list: &mut BlockList) {
        if cursor_col == 0 || self.current_line_has_cr {
            // A fresh line (output pause) or an in-place `\r` redraw (progress bar) -
            // not a settled prompt.
            return;
        }
        if !self.seen_prompt {
            self.seen_prompt = true;
            self.last_prompt_offset = offset;
            self.newlines_since_prompt = 0;
            return;
        }
        if self.newlines_since_prompt == 0 {
            return; // same prompt, the user is only typing - no command ran
        }
        // A command cycle ran in [last_prompt_offset, offset). Emit it, labeled
        // approximate and already finished (the heuristic has no running tail).
        let now = Instant::now();
        list.push(Block::Command(CommandBlock {
            command: String::new(),
            output_span: OutputSpan {
                start: self.last_prompt_offset,
                end: Some(offset),
            },
            exit_code: None,
            started_at: now,
            finished_at: Some(now),
            cwd: None,
            output: Vec::new(),
            interactive: false,
            approximate: true,
        }));
        self.last_prompt_offset = offset;
        self.newlines_since_prompt = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a() -> Mark {
        Mark::Prompt(PromptKind::PromptStart)
    }
    fn b() -> Mark {
        Mark::Prompt(PromptKind::CommandStart)
    }
    fn c() -> Mark {
        Mark::Prompt(PromptKind::OutputStart)
    }
    fn d(code: i32) -> Mark {
        Mark::Prompt(PromptKind::CommandDone {
            exit_code: Some(code),
        })
    }

    #[test]
    fn full_lifecycle_creates_one_closed_block() {
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();

        seg.apply(&a(), 0, &mut list);
        seg.apply(&b(), 10, &mut list); // command input at 10
        seg.apply(&c(), 17, &mut list); // output starts at 17
        seg.apply(&d(0), 42, &mut list); // done at 42

        assert_eq!(list.len(), 1);
        let blk = list.last().unwrap();
        let c0 = blk.as_command().unwrap();
        assert_eq!(c0.output_span.start, 17);
        assert_eq!(c0.output_span.end, Some(42));
        assert_eq!(c0.exit_code, Some(0));
        assert_eq!(c0.succeeded(), Some(true));
        assert!(!blk.is_running());
    }

    #[test]
    fn block_is_running_before_done() {
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        seg.apply(&a(), 0, &mut list);
        seg.apply(&b(), 5, &mut list);
        seg.apply(&c(), 8, &mut list);
        let blk = list.last().unwrap();
        assert!(blk.is_running());
        let c0 = blk.as_command().unwrap();
        assert_eq!(c0.output_span.end, None);
        assert_eq!(c0.succeeded(), None);
    }

    #[test]
    fn two_commands_make_two_blocks() {
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        for (off, m) in [
            (0, a()),
            (3, b()),
            (6, c()),
            (20, d(0)),
            (21, a()),
            (24, b()),
            (27, c()),
            (50, d(1)),
        ] {
            seg.apply(&m, off, &mut list);
        }
        assert_eq!(list.len(), 2);
        assert_eq!(
            list.iter().next().unwrap().as_command().unwrap().exit_code,
            Some(0)
        );
        assert_eq!(
            list.iter().nth(1).unwrap().as_command().unwrap().exit_code,
            Some(1)
        );
        assert_eq!(
            list.iter()
                .nth(1)
                .unwrap()
                .as_command()
                .unwrap()
                .succeeded(),
            Some(false)
        );
    }

    #[test]
    fn cwd_mark_applied_to_open_block() {
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        seg.apply(&Mark::Cwd("/home/me".into()), 0, &mut list);
        seg.apply(&a(), 0, &mut list);
        seg.apply(&b(), 2, &mut list);
        seg.apply(&c(), 4, &mut list);
        assert_eq!(
            list.last().unwrap().as_command().unwrap().cwd.as_deref(),
            Some("/home/me")
        );
    }

    #[test]
    fn set_last_command_fills_text() {
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        seg.apply(&a(), 0, &mut list);
        seg.apply(&b(), 2, &mut list);
        seg.apply(&c(), 4, &mut list);
        seg.set_last_command(&mut list, "ls -la");
        assert_eq!(list.last().unwrap().as_command().unwrap().command, "ls -la");
    }

    // --- HeightIndex (SumTree) -------------------------------------------------

    /// Naive O(n) references the Fenwick index must agree with exactly.
    fn naive_prefix(h: &[u64], count: usize) -> u64 {
        h[..count.min(h.len())].iter().sum()
    }
    fn naive_block_at(h: &[u64], row: u64) -> Option<usize> {
        let mut acc = 0u64;
        for (k, &x) in h.iter().enumerate() {
            if row < acc + x {
                return Some(k);
            }
            acc += x;
        }
        None
    }

    fn index_of(heights: &[u64]) -> HeightIndex {
        let mut idx = HeightIndex::new();
        for &h in heights {
            idx.push(h);
        }
        idx
    }

    /// Cross-check the index against the naive reference for prefix, total, and
    /// block_at over EVERY row, for a height vector with varied + zero entries.
    fn assert_matches_naive(heights: &[u64]) {
        let idx = index_of(heights);
        let total: u64 = heights.iter().sum();
        assert_eq!(idx.total(), total, "total for {heights:?}");
        assert_eq!(idx.len(), heights.len());
        for count in 0..=heights.len() {
            assert_eq!(
                idx.prefix(count),
                naive_prefix(heights, count),
                "prefix({count}) for {heights:?}"
            );
        }
        // Every row in [0, total) plus a couple past-the-end probes.
        for row in 0..total + 2 {
            assert_eq!(
                idx.block_at(row),
                naive_block_at(heights, row),
                "block_at({row}) for {heights:?}"
            );
        }
    }

    #[test]
    fn height_index_matches_naive_for_varied_heights() {
        assert_matches_naive(&[]);
        assert_matches_naive(&[1]);
        assert_matches_naive(&[3, 1, 4, 1, 5, 9, 2, 6]);
        // Zero-height blocks (empty output) must be skipped, never "contain" a row.
        assert_matches_naive(&[0, 2, 0, 0, 3, 0, 1]);
        assert_matches_naive(&[5, 0, 0, 0, 7]);
    }

    #[test]
    fn height_index_set_and_remove_stay_consistent() {
        let mut idx = index_of(&[2, 3, 5, 1, 4]);
        let mut model = vec![2u64, 3, 5, 1, 4];

        // Grow the tail (the common running-block case), shrink a middle block.
        idx.set(4, 10);
        model[4] = 10;
        idx.set(2, 1);
        model[2] = 1;
        for row in 0..model.iter().sum::<u64>() {
            assert_eq!(
                idx.block_at(row),
                naive_block_at(&model, row),
                "after set, row {row}"
            );
        }
        assert_eq!(idx.total(), model.iter().sum::<u64>());

        // Evict the oldest (front) and a middle block.
        idx.remove(0);
        model.remove(0);
        idx.remove(1);
        model.remove(1);
        assert_eq!(idx.len(), model.len());
        assert_eq!(idx.total(), model.iter().sum::<u64>());
        for row in 0..model.iter().sum::<u64>() {
            assert_eq!(
                idx.block_at(row),
                naive_block_at(&model, row),
                "after remove, row {row}"
            );
        }
    }

    #[test]
    fn blocks_in_viewport_ranges_are_correct() {
        // Heights: blocks at rows [0,2) [2,5) [5,6) [6,10).
        let idx = index_of(&[2, 3, 1, 4]); // total 10
        assert_eq!(
            idx.blocks_in_viewport(0, 3),
            0..2,
            "top of 2 spills into block 1"
        );
        assert_eq!(idx.blocks_in_viewport(2, 3), 1..2, "exactly block 1's rows");
        // rows [4,7): row 4 -> block 1, row 5 -> block 2, row 6 -> block 3.
        assert_eq!(
            idx.blocks_in_viewport(4, 3),
            1..4,
            "spans blocks 1, 2 and 3"
        );
        assert_eq!(idx.blocks_in_viewport(0, 10), 0..4, "whole content");
        assert_eq!(
            idx.blocks_in_viewport(0, 100),
            0..4,
            "viewport taller than content"
        );
        // Degenerate inputs.
        assert_eq!(idx.blocks_in_viewport(0, 0), 0..0, "zero-height viewport");
        assert_eq!(
            idx.blocks_in_viewport(10, 5),
            4..4,
            "scrolled exactly to the end"
        );
        assert_eq!(idx.blocks_in_viewport(50, 5), 4..4, "scrolled past the end");
        assert_eq!(
            HeightIndex::new().blocks_in_viewport(0, 5),
            0..0,
            "empty list"
        );
    }

    #[test]
    fn height_index_scales_to_10k_blocks() {
        // AC3: O(log n) viewport queries against 10k blocks. We assert correctness
        // at scale (the Fenwick path is structurally O(log n)) plus a very generous
        // timing sanity check (NOT a hard gate) - a quadratic regression would blow
        // far past it.
        let n = 10_000usize;
        let heights: Vec<u64> = (0..n as u64).map(|i| (i % 7) + 1).collect();
        let idx = index_of(&heights);
        let total: u64 = heights.iter().sum();
        assert_eq!(idx.total(), total);
        // Spot-check block_at against naive at sampled rows (full naive over 10k
        // rows x 10k blocks would itself be slow).
        for &row in &[0u64, 1, total / 4, total / 2, total - 1] {
            assert_eq!(
                idx.block_at(row),
                naive_block_at(&heights, row),
                "row {row}"
            );
        }
        let start = Instant::now();
        let mut acc = 0usize;
        for q in 0..100_000u64 {
            acc += idx.blocks_in_viewport(q % total, 50).len();
        }
        assert!(acc > 0);
        assert!(
            start.elapsed().as_secs() < 5,
            "100k viewport queries over 10k blocks should be ~instant for O(log n)"
        );
    }

    // --- immutable output snapshots --------------------------------------------

    fn row(width: usize) -> RowSnapshot {
        RowSnapshot::new(vec![crate::terminal::SnapshotCell::default(); width])
    }

    #[test]
    fn set_block_output_updates_height_index() {
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        // Two command cycles -> two blocks, each initially height 1 (no output).
        for base in [0usize, 100] {
            seg.apply(&a(), base, &mut list);
            seg.apply(&b(), base + 2, &mut list);
            seg.apply(&c(), base + 4, &mut list);
            seg.apply(&d(0), base + 40, &mut list);
        }
        assert_eq!(list.len(), 2);
        assert_eq!(
            list.total_height_rows(),
            2,
            "two blocks, 1 row each before output"
        );

        // The lifecycle driver lands block 0's output snapshot (3 rows) -> height 4.
        list.set_block_output(0, vec![row(80), row(80), row(80)]);
        assert_eq!(list.get(0).unwrap().height_rows(), 4);
        assert_eq!(
            list.total_height_rows(),
            4 + 1,
            "block0=4 rows + block1=1 row"
        );
        // Viewport at the very top now starts in block 0 and reaches block 1.
        assert_eq!(list.blocks_in_viewport(0, 5), 0..2);
        assert_eq!(
            list.blocks_in_viewport(4, 1),
            1..2,
            "row 4 is block 1's only row"
        );
    }

    #[test]
    fn display_height_collapses_long_output() {
        // AC4 (T-2.7): a block whose captured output exceeds COLLAPSED_OUTPUT_ROWS
        // collapses to command + CAP shown rows + one "... +N lines" affordance row;
        // a short block shows every row uncollapsed (display == raw height).
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();

        // A short block (3 output rows) is never collapsed.
        seg.apply(&a(), 0, &mut list);
        seg.apply(&c(), 4, &mut list);
        seg.apply(&d(0), 40, &mut list);
        list.set_block_output(0, vec![row(80); 3]);
        let short = list.get(0).unwrap();
        let sc = short.as_command().unwrap();
        assert_eq!(sc.collapsed_hidden_rows(), None);
        assert_eq!(sc.shown_output_rows(), 3);
        assert_eq!(short.display_height_rows(), short.height_rows());
        assert_eq!(short.display_height_rows(), 1 + 3);

        // A long block: CAP + 25 rows of output collapses.
        let long_rows = (COLLAPSED_OUTPUT_ROWS + 25) as usize;
        seg.apply(&a(), 100, &mut list);
        seg.apply(&c(), 104, &mut list);
        seg.apply(&d(0), 200, &mut list);
        list.set_block_output(1, vec![row(80); long_rows]);
        let long = list.get(1).unwrap();
        let lc = long.as_command().unwrap();
        assert_eq!(lc.shown_output_rows(), COLLAPSED_OUTPUT_ROWS);
        assert_eq!(lc.collapsed_hidden_rows(), Some(25));
        // command + CAP shown + 1 affordance row, NOT 1 + long_rows.
        assert_eq!(long.display_height_rows(), 1 + COLLAPSED_OUTPUT_ROWS + 1);
        assert!(long.display_height_rows() < long.height_rows());
    }

    #[test]
    fn running_block_shows_full_live_output_without_collapsing() {
        // T-4.6: the RUNNING (active) block is never collapsed - it shows all of its
        // live output so the bottom-pinned timeline keeps the latest output on screen
        // (watching `cargo build` shows the tail, not a frozen head + a growing counter).
        // The SAME output on a FINISHED block collapses (settled scrollback).
        let long_rows = (COLLAPSED_OUTPUT_ROWS + 50) as usize;

        // A running block: prompt + output start, NO command-done.
        let mut running = BlockList::new();
        let mut rs = BlockSegmenter::new();
        rs.apply(&a(), 0, &mut running);
        rs.apply(&c(), 4, &mut running);
        running.set_block_output(0, vec![row(80); long_rows]);
        let rb = running.get(0).unwrap();
        assert!(rb.is_running(), "no command-done -> still running");
        let rc = rb.as_command().unwrap();
        assert_eq!(
            rc.collapsed_hidden_rows(),
            None,
            "a running block never collapses"
        );
        assert_eq!(
            rc.shown_output_rows(),
            long_rows as u64,
            "a running block shows ALL its live output"
        );
        assert_eq!(
            rb.display_height_rows(),
            rb.height_rows(),
            "a running block's display height is its full height"
        );

        // The same output on a finished block collapses to the cap + affordance.
        let mut finished = BlockList::new();
        let mut fs = BlockSegmenter::new();
        fs.apply(&a(), 0, &mut finished);
        fs.apply(&c(), 4, &mut finished);
        fs.apply(&d(0), 40, &mut finished);
        finished.set_block_output(0, vec![row(80); long_rows]);
        let fb = finished.get(0).unwrap();
        assert!(!fb.is_running());
        let fc = fb.as_command().unwrap();
        assert_eq!(fc.shown_output_rows(), COLLAPSED_OUTPUT_ROWS);
        assert_eq!(fc.collapsed_hidden_rows(), Some(50));
        assert_eq!(fb.display_height_rows(), 1 + COLLAPSED_OUTPUT_ROWS + 1);
    }

    #[test]
    fn height_index_tracks_collapsed_display_height() {
        // The SumTree must virtualize over DISPLAY heights so scroll geometry matches
        // the drawn (collapsed) layout - one coordinate space (ticket T-2.7).
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        seg.apply(&a(), 0, &mut list);
        seg.apply(&c(), 4, &mut list);
        seg.apply(&d(0), 40, &mut list);

        let long_rows = (COLLAPSED_OUTPUT_ROWS + 100) as usize;
        list.set_block_output(0, vec![row(80); long_rows]);

        let collapsed_h = 1 + COLLAPSED_OUTPUT_ROWS + 1;
        assert_eq!(list.get(0).unwrap().display_height_rows(), collapsed_h);
        assert_eq!(
            list.total_height_rows(),
            collapsed_h,
            "the index tracks the collapsed display height, not the raw output count"
        );
        assert_eq!(list.block_top_row(0), 0);
        // A viewport far taller than the collapsed block still sees exactly one block,
        // and the index never reports a row past the collapsed extent.
        assert_eq!(list.blocks_in_viewport(0, 1000), 0..1);
        assert_eq!(
            list.block_top_row(1),
            collapsed_h,
            "the next block's top edge is after the collapsed block's display height"
        );
    }

    #[test]
    fn finished_block_output_is_immutable_under_later_activity() {
        // AC2: a finished block's stored rows are owned copies, so later activity
        // (more blocks, a simulated reflow that mutates the source rows) cannot
        // change them.
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        seg.apply(&a(), 0, &mut list);
        seg.apply(&c(), 4, &mut list);
        seg.apply(&d(0), 40, &mut list);

        let mut source = vec![row(80), row(80)];
        list.set_block_output(0, source.clone());
        let snapshot = list.get(0).unwrap().as_command().unwrap().output.clone();

        // "Reflow": mutate the source rows + push another block. The finished
        // block's stored output must be unchanged (it owns its copy).
        source[0].cells.clear();
        source.push(row(40));
        seg.apply(&a(), 100, &mut list);
        seg.apply(&c(), 104, &mut list);

        assert_eq!(
            list.get(0).unwrap().as_command().unwrap().output,
            snapshot,
            "a finished block's snapshot must survive later reflow/activity unchanged"
        );
        assert_eq!(
            list.get(0).unwrap().as_command().unwrap().output.len(),
            2,
            "still 2 rows, 80 wide each"
        );
    }

    #[test]
    fn ac1_command_cycle_yields_block_with_command_exit_cwd_and_snapshot() {
        // AC1: an A/B/C/D cycle (with cwd + command) yields one block carrying the
        // command text, exit code, cwd, and an immutable output snapshot.
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        seg.apply(&Mark::Cwd("/home/me/project".into()), 0, &mut list);
        seg.apply(&a(), 0, &mut list);
        seg.apply(&b(), 2, &mut list);
        seg.apply(&c(), 8, &mut list);
        seg.set_last_command(&mut list, "cargo build");
        seg.apply(&d(0), 50, &mut list);
        list.set_block_output(0, vec![row(80), row(80)]);

        assert_eq!(list.len(), 1);
        let b0 = list.get(0).unwrap();
        let c0 = b0.as_command().unwrap();
        assert_eq!(c0.command, "cargo build");
        assert_eq!(c0.exit_code, Some(0));
        assert_eq!(c0.succeeded(), Some(true));
        assert_eq!(c0.cwd.as_deref(), Some("/home/me/project"));
        assert!(!b0.is_running());
        assert_eq!(c0.output.len(), 2, "immutable output snapshot captured");
        assert_eq!(b0.height_rows(), 3);
    }

    // --- T-2.5 lifecycle state machine + alt-screen suppression ----------------

    #[test]
    fn ac1_normal_cycle_finalizes_one_block_and_resumes() {
        // AC1: a normal A/B/C/D cycle yields one finalized block; a second cycle
        // then yields a second (the state machine returned to Idle).
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        for base in [0usize, 100] {
            seg.apply(&a(), base, &mut list);
            seg.apply(&b(), base + 2, &mut list);
            seg.apply(&c(), base + 4, &mut list);
            seg.apply(&d(0), base + 40, &mut list);
        }
        assert_eq!(list.len(), 2);
        assert!(list.iter().all(|b| !b.is_running()));
        assert_eq!(
            list.get(0).unwrap().as_command().unwrap().exit_code,
            Some(0)
        );
    }

    #[test]
    fn ac1_cmdline_mark_sets_block_command() {
        // The scanner emits CommandLine(X) just before OutputStart for a nonce'd
        // `C;cmdline=X`; the segmenter stages it onto the opening block.
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        seg.apply(&a(), 0, &mut list);
        seg.apply(&b(), 2, &mut list);
        seg.apply(&Mark::CommandLine("git status".into()), 4, &mut list);
        seg.apply(&c(), 4, &mut list);
        seg.apply(&d(0), 20, &mut list);
        assert_eq!(
            list.get(0).unwrap().as_command().unwrap().command,
            "git status"
        );
    }

    #[test]
    fn ac2_alt_screen_yields_one_interactive_block_no_phantoms() {
        // AC2: running a TUI (vim) -> one Interactive block, and the TUI's own
        // OSC-133 marks while in the alt screen create NO phantom blocks.
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        // vim launches: a normal block opens.
        seg.apply(&a(), 0, &mut list);
        seg.apply(&b(), 2, &mut list);
        seg.apply(&Mark::CommandLine("vim".into()), 4, &mut list);
        seg.apply(&c(), 4, &mut list);
        assert_eq!(list.len(), 1);
        // vim enters the alt screen -> the launching block becomes Interactive.
        seg.set_alt_screen(true, &mut list);
        assert!(list.get(0).unwrap().as_command().unwrap().interactive);
        // Phantom marks the TUI emits while in the alt screen are suppressed.
        seg.apply(&a(), 50, &mut list);
        seg.apply(&b(), 52, &mut list);
        seg.apply(&c(), 54, &mut list);
        seg.apply(&d(0), 56, &mut list);
        assert_eq!(list.len(), 1, "no phantom blocks from alt-screen chatter");
        // vim exits -> alt screen off -> the shell's D closes the interactive block.
        seg.set_alt_screen(false, &mut list);
        seg.apply(&d(0), 100, &mut list);
        let b0 = list.get(0).unwrap();
        let c0 = b0.as_command().unwrap();
        assert_eq!(list.len(), 1);
        assert!(c0.interactive);
        assert!(
            !b0.is_running(),
            "the interactive block is finalized on exit"
        );
        assert_eq!(c0.command, "vim");
    }

    #[test]
    fn altscreen_entry_clears_staged_command_line() {
        // Hardening (review): a command line staged via `CommandLine` whose block
        // never opened before the alt screen activates must not leak into a later
        // block. Entering the alt screen drops the staged command.
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        seg.apply(&Mark::CommandLine("staged".into()), 0, &mut list);
        assert_eq!(seg.pending_command.as_deref(), Some("staged"));
        seg.set_alt_screen(true, &mut list);
        assert!(
            seg.pending_command.is_none(),
            "alt-screen entry drops the staged command line"
        );
    }

    #[test]
    fn altscreen_entry_only_flags_the_open_output_block() {
        // Hardening (review): entering the alt screen flags ONLY the block whose
        // output is currently open (phase Output, still running) as interactive -
        // never a finished block (the `is_running` guard) and never a stale block
        // left dangling by an earlier missing `D` (whose phase is not Output, so the
        // `Phase::Output` guard rejects it).
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        seg.apply(&a(), 0, &mut list);
        seg.apply(&b(), 2, &mut list);
        seg.apply(&c(), 4, &mut list);
        seg.apply(&d(0), 20, &mut list); // finished; phase -> Idle
        seg.set_alt_screen(true, &mut list);
        assert!(
            !list.get(0).unwrap().as_command().unwrap().interactive,
            "a finished (non-Output) block is not retroactively made interactive"
        );
    }

    #[test]
    fn ac3_missing_d_recovery_auto_closes_with_unknown_exit() {
        // AC3: a Ctrl-C'd command (no D before the next prompt) is auto-closed with
        // an UNKNOWN exit when the next A arrives.
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        seg.apply(&a(), 0, &mut list);
        seg.apply(&b(), 2, &mut list);
        seg.apply(&c(), 4, &mut list); // command running...
        assert!(list.get(0).unwrap().is_running());
        // No D - the next prompt arrives (user hit Ctrl-C).
        seg.apply(&a(), 30, &mut list);
        let b0 = list.get(0).unwrap();
        assert!(!b0.is_running(), "orphaned block auto-closed");
        assert_eq!(
            b0.as_command().unwrap().exit_code,
            None,
            "auto-closed exit is unknown"
        );
        // The recovery did not spawn a phantom block.
        assert_eq!(list.len(), 1);
        // A real next command still segments normally.
        seg.apply(&b(), 32, &mut list);
        seg.apply(&c(), 34, &mut list);
        seg.apply(&d(0), 60, &mut list);
        assert_eq!(list.len(), 2);
        assert_eq!(
            list.get(1).unwrap().as_command().unwrap().exit_code,
            Some(0)
        );
    }

    #[test]
    fn ac4_empty_enter_makes_no_block_and_no_output_is_thin() {
        // AC4: an empty Enter (A->B->A, never reaching C) creates no block.
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        seg.apply(&a(), 0, &mut list);
        seg.apply(&b(), 2, &mut list);
        seg.apply(&a(), 4, &mut list); // empty Enter -> new prompt, no command ran
        assert_eq!(list.len(), 0, "an empty Enter must not create a block");

        // A command that runs but emits nothing (C and D adjacent) -> a thin block.
        seg.apply(&b(), 6, &mut list);
        seg.apply(&c(), 8, &mut list);
        seg.apply(&d(0), 8, &mut list); // no output bytes between C and D
        assert_eq!(list.len(), 1);
        assert!(
            list.get(0).unwrap().as_command().unwrap().is_thin(),
            "no-output command collapses to a thin marker"
        );
    }

    #[test]
    fn ac5_nonce_mismatch_marks_never_reach_the_segmenter() {
        // AC5: a nested un-integrated shell (or an attacker) emitting OSC-133 with
        // the WRONG nonce produces NO marks from the nonce-armed scanner, so the
        // outer block list is never mutated. (The gate is T-2.1's; this confirms the
        // integration that protects the lifecycle.)
        use crate::osc::OscScanner;
        let mut scanner = OscScanner::with_nonce("realsession");
        let forged = b"\x1b]133;A;aterm_nonce=attacker\x07\
\x1b]133;C;aterm_nonce=attacker\x07\x1b]133;D;0;aterm_nonce=attacker\x07";
        let scan = scanner.scan(forged);
        assert!(
            scan.marks.is_empty(),
            "wrong-nonce marks must be dropped by the scanner"
        );

        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        for (offset, mark) in &scan.marks {
            seg.apply(mark, *offset, &mut list);
        }
        assert_eq!(list.len(), 0, "no block from forged marks");

        // The same sequence with the CORRECT nonce DOES produce marks (sanity).
        let good = b"\x1b]133;A;aterm_nonce=realsession\x07\
\x1b]133;C;aterm_nonce=realsession\x07\x1b]133;D;0;aterm_nonce=realsession\x07";
        let scan = scanner.scan(good);
        assert!(
            !scan.marks.is_empty(),
            "correctly-nonced marks pass the gate"
        );
    }

    // --- T-2.6 heuristic fallback block detector --------------------------------

    #[test]
    fn heuristic_progress_bar_redraw_does_not_fabricate_blocks() {
        // Regression (review): a running command's in-place `\r`-redrawn progress bar
        // stalls mid-line (cursor col > 0, output quiet) and would otherwise look like
        // a settled prompt - fragmenting the single command into spurious blocks. The
        // `\r`-redraw guard suppresses every redraw stall; the command yields exactly
        // one block, at its real next prompt.
        let mut list = BlockList::new();
        let mut h = HeuristicSegmenter::new();
        h.note_prompt_if_idle(2, 0, &mut list); // P0 anchored at the prompt

        h.observe_output(b"wget url\r\n"); // submit: a newline, `\r\n` is not a redraw
                                           // Each progress update redraws the line in place with a leading `\r`, then the
                                           // command stalls (network wait) - an idle publish samples the cursor mid-line.
        for chunk in [
            b"\rDownloading   0%".as_slice(),
            b"\rDownloading  50%".as_slice(),
            b"\rDownloading 100%".as_slice(),
        ] {
            h.observe_output(chunk);
            h.note_prompt_if_idle(16, 40, &mut list); // stalled mid-redraw -> suppressed
        }
        assert_eq!(
            list.len(),
            0,
            "a `\\r`-redrawn progress bar must not fabricate blocks mid-command"
        );

        // The command finishes and the real prompt redraws on a fresh line.
        h.observe_output(b"\ndone\nP> ");
        h.note_prompt_if_idle(2, 60, &mut list);
        assert_eq!(
            list.len(),
            1,
            "exactly one block for the whole wget command"
        );
        assert_eq!(
            list.get(0).unwrap().as_command().unwrap().output_span.start,
            0
        );
    }

    #[test]
    fn heuristic_segments_one_finished_block_per_command_cycle() {
        // The structural signal: idle-at-prompt (cursor col > 0) bracketed by a
        // command that emitted at least one newline. Each cycle becomes one finished,
        // labeled-approximate block spanning [prev prompt, this prompt).
        let mut list = BlockList::new();
        let mut h = HeuristicSegmenter::new();

        h.note_prompt_if_idle(2, 0, &mut list); // P0: anchor only, no block
        assert_eq!(list.len(), 0, "the first prompt alone creates no block");

        h.observe_output(b"ls -la\r\nfile1\nfile2\n"); // echo + output -> 3 newlines
        h.note_prompt_if_idle(2, 30, &mut list); // P1: a command ran -> emit [0,30)
        assert_eq!(list.len(), 1);
        let b0 = list.get(0).unwrap();
        assert!(
            !b0.is_running(),
            "heuristic blocks are emitted already finished"
        );
        assert_eq!(
            b0.as_command().unwrap().output_span,
            OutputSpan {
                start: 0,
                end: Some(30)
            }
        );

        h.observe_output(b"pwd\r\n/home/me\n"); // 2 newlines
        h.note_prompt_if_idle(2, 50, &mut list); // P2 -> emit [30,50)
        assert_eq!(list.len(), 2);
        assert_eq!(
            list.get(1).unwrap().as_command().unwrap().output_span,
            OutputSpan {
                start: 30,
                end: Some(50)
            }
        );
    }

    #[test]
    fn heuristic_does_not_fabricate_a_block_while_only_typing() {
        // The phantom-block-per-keystroke trap: typing on the prompt line echoes
        // characters but emits NO newline, so re-detecting the same prompt while the
        // cursor advances must create nothing. Only a submitted command (a newline)
        // makes a block.
        let mut list = BlockList::new();
        let mut h = HeuristicSegmenter::new();
        h.note_prompt_if_idle(2, 0, &mut list); // P0 anchored

        h.observe_output(b"git st"); // typing echo, no newline
        h.note_prompt_if_idle(8, 6, &mut list); // re-idle at the same prompt
        h.observe_output(b"atus"); // more typing
        h.note_prompt_if_idle(12, 10, &mut list);
        assert_eq!(list.len(), 0, "typing must not fabricate a block");

        h.observe_output(b"\r\non branch main\n"); // Enter + output
        h.note_prompt_if_idle(2, 30, &mut list); // next prompt -> one block
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn heuristic_treats_col0_idle_as_an_output_pause_not_a_prompt() {
        // A mid-output pause rests at column 0 (the last line ended in a newline);
        // that is NOT a settled prompt and must not split the command's output.
        let mut list = BlockList::new();
        let mut h = HeuristicSegmenter::new();
        h.note_prompt_if_idle(2, 0, &mut list); // P0
        h.observe_output(b"line1\nline2\n");
        h.note_prompt_if_idle(0, 12, &mut list); // idle at col 0 -> output pause, ignored
        assert_eq!(list.len(), 0, "an output pause is not a command boundary");
        h.observe_output(b"line3\n");
        h.note_prompt_if_idle(2, 18, &mut list); // the real next prompt
        assert_eq!(list.len(), 1);
        assert_eq!(
            list.get(0).unwrap().as_command().unwrap().output_span,
            OutputSpan {
                start: 0,
                end: Some(18)
            },
            "the whole output run is one block, not split at the pause"
        );
    }

    #[test]
    fn heuristic_works_for_non_sigil_modern_prompts() {
        // AC2 for the common case: starship/powerlevel10k prompts end in `❯`/arrows,
        // never `$`/`%`/`#`/`>`. The structural detector keys off cursor-not-at-col-0,
        // not a sigil, so these segment normally (the old sigil match produced zero
        // blocks here - a silent AC2 failure).
        let mut list = BlockList::new();
        let mut h = HeuristicSegmenter::new();
        h.note_prompt_if_idle(2, 0, &mut list); // "❯ " - cursor at col 2
        h.observe_output(b"echo hi\r\nhi\n");
        h.note_prompt_if_idle(2, 20, &mut list);
        assert_eq!(list.len(), 1, "a non-sigil prompt still produces a block");
    }

    #[test]
    fn heuristic_blocks_are_labeled_approximate_and_lack_authoritative_fields() {
        // AC2: heuristic blocks must be clearly labeled approximate, and (having no
        // real hooks) carry no exit code / command text / cwd.
        let mut list = BlockList::new();
        let mut h = HeuristicSegmenter::new();
        h.note_prompt_if_idle(2, 0, &mut list);
        h.observe_output(b"true\r\n");
        h.note_prompt_if_idle(2, 10, &mut list);

        let b = list.get(0).unwrap().as_command().unwrap();
        assert!(b.approximate, "heuristic blocks are labeled approximate");
        assert_eq!(b.exit_code, None, "no authoritative exit code");
        assert!(b.command.is_empty(), "no authoritative command text");
        assert!(b.cwd.is_none(), "no authoritative cwd");
        assert!(!b.interactive);
    }

    #[test]
    fn heuristic_ignores_output_before_the_first_prompt() {
        // A login banner / motd printed before the first prompt must NOT fabricate a
        // leading block; segmentation begins only once the first prompt is anchored.
        let mut list = BlockList::new();
        let mut h = HeuristicSegmenter::new();
        h.observe_output(b"Welcome to the machine\nLast login: today\n");
        h.note_prompt_if_idle(2, 40, &mut list); // first prompt: anchors, no block
        assert_eq!(list.len(), 0, "pre-prompt banner output creates no block");

        h.observe_output(b"hi\r\nhi\n");
        h.note_prompt_if_idle(2, 60, &mut list);
        assert_eq!(list.len(), 1, "the first real command cycle then segments");
        assert_eq!(
            list.get(0).unwrap().as_command().unwrap().output_span.start,
            40,
            "the block starts at the first prompt, not in the banner"
        );
    }

    #[test]
    fn heuristic_idle_resampling_at_the_same_prompt_is_a_no_op() {
        // The engine notes idle on every coalescing flush; repeated idle samples at
        // the same prompt (no intervening command/newline) must not duplicate blocks.
        let mut list = BlockList::new();
        let mut h = HeuristicSegmenter::new();
        h.note_prompt_if_idle(2, 0, &mut list);
        h.observe_output(b"ls\r\nout\n");
        for off in [30, 31, 32] {
            h.note_prompt_if_idle(2, off, &mut list); // resampled idle at the new prompt
        }
        assert_eq!(list.len(), 1, "one block despite repeated idle samples");
        assert_eq!(
            list.get(0).unwrap().as_command().unwrap().output_span.end,
            Some(30),
            "emitted at the first idle sample of the new prompt"
        );
    }

    // --- agent steps in the timeline (ticket T-5.10) ---------------------------

    fn agent(kind: AgentBlockKind, text: &str) -> AgentBlock {
        AgentBlock::new(kind, text, Instant::now())
    }

    #[test]
    fn agent_block_carries_an_agent_domain_free_badge() {
        // T-5.11: a tool-call step's gate verdict rides on the block as a
        // label-bearing datum (AgentBadge), NOT an aterm-agent type - so the
        // renderer draws the badge without crossing the one-way crate arrow. A
        // fresh block (and any non-tool kind) carries no badge.
        let plain = agent(AgentBlockKind::AssistantText, "hi");
        assert_eq!(plain.badge, None);

        let gated =
            agent(AgentBlockKind::ToolCall, "run_command").with_badge(AgentBadge::NeedsApproval);
        assert_eq!(gated.badge, Some(AgentBadge::NeedsApproval));

        // The verdict is part of the block's identity, so a transition (an approval
        // flipping NeedsApproval -> Auto) is observable for the renderer's damage gate.
        let approved = agent(AgentBlockKind::ToolCall, "run_command").with_badge(AgentBadge::Auto);
        assert_ne!(gated.badge, approved.badge);
    }

    #[test]
    fn append_last_agent_text_extends_the_trailing_agent_block_only() {
        // T-5.11: the engine's streaming-inject path extends the OPEN (last) agent
        // block in place, without the caller threading an index across threads. It
        // targets the last agent entry even when a command block trails it is absent,
        // and is a no-op (returns false) when there is no agent block.
        let mut list = BlockList::new();
        assert!(
            !list.append_last_agent_text("x"),
            "no agent block -> nothing to extend"
        );

        let i = list.push_agent(agent(AgentBlockKind::AssistantText, "Hel"));
        assert!(list.append_last_agent_text("lo"));
        assert_eq!(list.get(i).unwrap().as_agent().unwrap().text, "Hello");

        // A second agent block: the append targets the LAST one, and the height index
        // tracks the new line count (a two-line append spans two rows).
        let j = list.push_agent(agent(AgentBlockKind::ToolResult, "one"));
        assert!(list.append_last_agent_text("\ntwo"));
        assert_eq!(list.get(j).unwrap().as_agent().unwrap().text, "one\ntwo");
        assert_eq!(list.get(i).unwrap().as_agent().unwrap().text, "Hello"); // first untouched
        assert_eq!(list.total_height_rows(), 1 + 2, "1 row + 2-line tail");
    }

    #[test]
    fn agent_steps_interleave_with_command_blocks_in_append_order() {
        // T-5.10 AC1: agent steps and human command blocks live in ONE wall-clock
        // timeline; append order IS wall-clock order, so the segmenter's commands and
        // the agent's pushed steps interleave by the order they happen.
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();

        // A human command runs and finishes (block 0).
        seg.apply(&a(), 0, &mut list);
        seg.apply(&c(), 4, &mut list);
        seg.apply(&d(0), 40, &mut list);
        // Then an agent turn: prompt + assistant text (blocks 1, 2).
        list.push_agent(agent(AgentBlockKind::UserPrompt, "fix the test"));
        list.push_agent(agent(AgentBlockKind::AssistantText, "On it."));
        // Then a second human command (block 3).
        seg.apply(&a(), 100, &mut list);
        seg.apply(&c(), 104, &mut list);
        seg.apply(&d(0), 140, &mut list);

        assert_eq!(list.len(), 4);
        let kinds: Vec<&str> = list
            .iter()
            .map(|b| match b {
                Block::Command(_) => "cmd",
                Block::Agent(a) => match a.kind {
                    AgentBlockKind::UserPrompt => "prompt",
                    AgentBlockKind::AssistantText => "assistant",
                    _ => "agent",
                },
            })
            .collect();
        assert_eq!(kinds, vec!["cmd", "prompt", "assistant", "cmd"]);

        // The height index accounts for the agent entries too (each 1 line here).
        assert_eq!(list.total_height_rows(), 4);
        // Viewport queries span across the interleave without confusion.
        assert_eq!(list.blocks_in_viewport(0, 4), 0..4);
        assert_eq!(
            list.block_top_row(2),
            2,
            "the assistant step is the 3rd entry"
        );
    }

    #[test]
    fn append_agent_text_is_a_point_update_touching_only_the_tail() {
        // T-5.10 AC2: streaming a delta into an agent step mutates ONLY that entry -
        // its version + height - and leaves every earlier entry's geometry untouched
        // (no full-timeline relayout per delta; the 60fps floor).
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        seg.apply(&a(), 0, &mut list);
        seg.apply(&c(), 4, &mut list);
        seg.apply(&d(0), 40, &mut list);
        let agent_idx = list.push_agent(agent(AgentBlockKind::AssistantText, "Hel"));
        assert_eq!(agent_idx, 1);

        let top_of_command = list.block_top_row(0);
        let top_of_agent = list.block_top_row(agent_idx);
        let total_before = list.total_height_rows();
        assert_eq!(list.get(agent_idx).unwrap().as_agent().unwrap().version, 0);

        // One delta that does NOT add a line: same height, version bumps.
        assert!(list.append_agent_text(agent_idx, "lo, world"));
        assert_eq!(
            list.get(agent_idx).unwrap().as_agent().unwrap().text,
            "Hello, world"
        );
        assert_eq!(list.get(agent_idx).unwrap().as_agent().unwrap().version, 1);
        assert_eq!(
            list.total_height_rows(),
            total_before,
            "no new line, no height change"
        );

        // A delta that adds a line grows ONLY this entry's height by exactly one row.
        assert!(list.append_agent_text(agent_idx, "\nsecond line"));
        assert_eq!(list.get(agent_idx).unwrap().as_agent().unwrap().version, 2);
        assert_eq!(list.total_height_rows(), total_before + 1);

        // The earlier command entry is geometrically untouched by the deltas.
        assert_eq!(list.block_top_row(0), top_of_command);
        assert_eq!(list.block_top_row(agent_idx), top_of_agent);
        assert_eq!(list.get(0).unwrap().height_rows(), 1);

        // Appending to a non-agent (command) entry is a no-op that reports false.
        assert!(!list.append_agent_text(0, "nope"));
    }

    #[test]
    fn set_last_command_output_targets_the_command_under_a_trailing_agent_step() {
        // The engine's capture path must land on the running COMMAND even when an
        // agent step (T-5.10) is the LAST entry - the interleaving-correctness
        // invariant that the old "running block == last block" assumption broke.
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        seg.apply(&a(), 0, &mut list); // prompt
        seg.apply(&c(), 4, &mut list); // command opens at index 0, running
                                       // An agent step interleaves AFTER the command opened (index 1 is now last()).
        list.push_agent(agent(AgentBlockKind::Thinking, "considering"));
        assert!(matches!(list.last().unwrap(), Block::Agent(_)));

        // The capture lands on the COMMAND (index 0), not the trailing agent step.
        assert!(list.set_last_command_output(vec![row(80), row(80)]));
        assert_eq!(
            list.get(0).unwrap().as_command().unwrap().output.len(),
            2,
            "the command block received the captured rows"
        );
        assert_eq!(
            list.get(1).unwrap().as_agent().unwrap().text,
            "considering",
            "the trailing agent step is untouched"
        );

        // With no command entry at all, it reports false (no-op).
        let mut empty = BlockList::new();
        empty.push_agent(agent(AgentBlockKind::AssistantText, "hi"));
        assert!(!empty.set_last_command_output(vec![row(80)]));
    }
}
