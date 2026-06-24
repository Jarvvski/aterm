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

/// One command block.
#[derive(Debug, Clone)]
pub struct Block {
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
}

impl Block {
    /// Is this block still running (no `D` seen)?
    pub fn is_running(&self) -> bool {
        self.finished_at.is_none()
    }

    /// Did the command succeed (exit 0)? `None` until it finishes.
    pub fn succeeded(&self) -> Option<bool> {
        self.exit_code.map(|c| c == 0)
    }

    /// This block's height in grid rows for the height index: one row for the
    /// command/prompt line plus its captured output rows. (A still-running block
    /// reports 1 until its output is snapshotted on finish; the live tail block's
    /// on-screen height is the renderer's concern, ticket T-2.7.)
    #[must_use]
    pub fn height_rows(&self) -> u64 {
        1 + self.output.len() as u64
    }
}

/// The ordered list of blocks for a session, with a [`HeightIndex`] giving
/// O(log n) viewport queries for the virtualized timeline renderer (ticket T-2.4).
///
/// The index mirrors each block's [`Block::height_rows`]; it is kept in step by
/// [`Self::push`] (a new block) and [`Self::set_block_output`] (a block's output
/// snapshot lands, growing its height). Mutating a block's cwd/exit/finished flags
/// via [`Self::last_mut`] does not change its height, so the index stays consistent
/// without an explicit update there.
#[derive(Debug, Default)]
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

    /// Total height of all blocks in rows (the timeline's scroll extent).
    #[must_use]
    pub fn total_height_rows(&self) -> u64 {
        self.index.total()
    }

    /// The half-open range of block indices intersecting a viewport of `viewport_h`
    /// rows whose top edge is `scroll_top` rows from the top of the timeline -
    /// O(log n) via the height index (ticket T-2.4). Empty range when there is
    /// nothing to show / the scroll is past the end.
    #[must_use]
    pub fn blocks_in_viewport(&self, scroll_top: u64, viewport_h: u64) -> Range<usize> {
        self.index.blocks_in_viewport(scroll_top, viewport_h)
    }

    /// Replace block `i`'s output snapshot (the lifecycle driver, T-2.5, calls this
    /// on `D` with the captured grid rows) and update the height index to match.
    pub fn set_block_output(&mut self, i: usize, rows: Vec<RowSnapshot>) {
        if let Some(b) = self.blocks.get_mut(i) {
            b.output = rows;
            self.index.set(i, b.height_rows());
        }
    }

    fn last_mut(&mut self) -> Option<&mut Block> {
        self.blocks.last_mut()
    }

    fn push(&mut self, block: Block) {
        self.index.push(block.height_rows());
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
        }
    }

    /// Apply one mark at byte `offset` in the logical stream, mutating `list`.
    pub fn apply(&mut self, mark: &Mark, offset: usize, list: &mut BlockList) {
        match mark {
            Mark::CommandLine(_cmd) => {
                // The decoded command line (OSC 133 `C;cmdline=` / OSC 633 `E`).
                // Capturing it into `Block.command` is the lifecycle state
                // machine's job (ticket T-2.5); T-2.1 only detects + decodes it.
            }
            Mark::Cwd(path) => {
                self.pending_cwd = Some(path.clone());
                // If a block is open, update its cwd too.
                if let Some(b) = list.last_mut() {
                    if b.is_running() {
                        b.cwd = Some(path.clone());
                    }
                }
            }
            Mark::Prompt(PromptKind::PromptStart) => {
                self.phase = Phase::Prompt;
                self.command_start_offset = None;
            }
            Mark::Prompt(PromptKind::CommandStart) => {
                // `B`: command input begins.
                self.phase = Phase::Command;
                self.command_start_offset = Some(offset);
            }
            Mark::Prompt(PromptKind::OutputStart) => {
                // `C`: a new block opens; its output span starts here.
                self.phase = Phase::Output;
                list.push(Block {
                    command: String::new(),
                    output_span: OutputSpan::open(offset),
                    exit_code: None,
                    started_at: Instant::now(),
                    finished_at: None,
                    cwd: self.pending_cwd.clone(),
                    // The immutable output snapshot lands on `D` via
                    // BlockList::set_block_output (the lifecycle driver, T-2.5).
                    output: Vec::new(),
                });
            }
            Mark::Prompt(PromptKind::CommandDone { exit_code }) => {
                // `D`: close the current block.
                if let Some(b) = list.last_mut() {
                    if b.is_running() {
                        b.output_span.end = Some(offset);
                        b.exit_code = *exit_code;
                        b.finished_at = Some(Instant::now());
                    }
                }
                self.phase = Phase::Idle;
                self.command_start_offset = None;
            }
        }
    }

    /// Set the command text for the most recently opened (running) block. The app
    /// layer calls this once it has sliced the input echo between `B` and `C`.
    pub fn set_last_command(&self, list: &mut BlockList, command: impl Into<String>) {
        if let Some(b) = list.last_mut() {
            b.command = command.into();
        }
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
        assert_eq!(blk.output_span.start, 17);
        assert_eq!(blk.output_span.end, Some(42));
        assert_eq!(blk.exit_code, Some(0));
        assert_eq!(blk.succeeded(), Some(true));
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
        assert_eq!(blk.output_span.end, None);
        assert_eq!(blk.succeeded(), None);
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
        assert_eq!(list.iter().next().unwrap().exit_code, Some(0));
        assert_eq!(list.iter().nth(1).unwrap().exit_code, Some(1));
        assert_eq!(list.iter().nth(1).unwrap().succeeded(), Some(false));
    }

    #[test]
    fn cwd_mark_applied_to_open_block() {
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        seg.apply(&Mark::Cwd("/home/me".into()), 0, &mut list);
        seg.apply(&a(), 0, &mut list);
        seg.apply(&b(), 2, &mut list);
        seg.apply(&c(), 4, &mut list);
        assert_eq!(list.last().unwrap().cwd.as_deref(), Some("/home/me"));
    }

    #[test]
    fn set_last_command_fills_text() {
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        seg.apply(&a(), 0, &mut list);
        seg.apply(&b(), 2, &mut list);
        seg.apply(&c(), 4, &mut list);
        seg.set_last_command(&mut list, "ls -la");
        assert_eq!(list.last().unwrap().command, "ls -la");
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
        let snapshot = list.get(0).unwrap().output.clone();

        // "Reflow": mutate the source rows + push another block. The finished
        // block's stored output must be unchanged (it owns its copy).
        source[0].cells.clear();
        source.push(row(40));
        seg.apply(&a(), 100, &mut list);
        seg.apply(&c(), 104, &mut list);

        assert_eq!(
            list.get(0).unwrap().output,
            snapshot,
            "a finished block's snapshot must survive later reflow/activity unchanged"
        );
        assert_eq!(
            list.get(0).unwrap().output.len(),
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
        assert_eq!(b0.command, "cargo build");
        assert_eq!(b0.exit_code, Some(0));
        assert_eq!(b0.succeeded(), Some(true));
        assert_eq!(b0.cwd.as_deref(), Some("/home/me/project"));
        assert!(!b0.is_running());
        assert_eq!(b0.output.len(), 2, "immutable output snapshot captured");
        assert_eq!(b0.height_rows(), 3);
    }
}
