//! The virtualized block-timeline layout (ticket T-2.7).
//!
//! Pure geometry: given the published [`BlockList`] (the model thread's immutable
//! snapshot, ticket T-2.4/T-2.7 seam), the alt-screen flag, a [`Scroll`] position,
//! and the viewport height in rows, [`layout`] produces the on-screen placement of
//! the blocks that intersect the viewport. No GPU, no clock, no allocation of the
//! grid - so it is exhaustively unit-testable on any host (the crate's "pure logic is
//! heavily unit-tested with no window" rule).
//!
//! **Virtualize twice** ([`03-pty-vt-rust.md`] section C): the SumTree height index
//! ([`BlockList::blocks_in_viewport`], O(log n)) picks the blocks intersecting the
//! viewport, then within each only the rows on screen become [`TimelineRow`] geometry.
//! Scrollback is data, not geometry ([`08-text-glyph-rendering.md`] section 5): a
//! 10k-block history costs O(visible rows) per frame, not O(history).
//!
//! **One coordinate space.** All row math is in *display* rows - [`Block::
//! display_height_rows`], which collapses long output (ticket T-2.7, AC4). Because the
//! height index is built from the same display heights, `blocks_in_viewport`,
//! scroll-to-block, and the drawn layout always agree - there is no second coordinate
//! system to drift.
//!
//! **Alt screen** ([`03-pty-vt-rust.md`] section D, ADR-0007): when a full-screen app
//! owns the screen the layout returns [`TimelineMode::AltScreen`] - the caller draws
//! the alt grid full-window instead of the block list, and the block list + scroll are
//! left untouched so exiting resumes the timeline exactly where it was (AC3).
//!
//! Drawing the laid-out blocks (gutter glyphs, command line, output rows, hairline
//! separators) is the renderer's job; final token/component styling is EPIC-4 (T-4.6).
//! This module establishes the correct geometry + virtualization the renderer consumes.

use aterm_core::{Block, BlockList};

/// Vertical scroll position of the timeline, in *display* rows from the very top
/// (`0` = top of the oldest block). The renderer owns one of these; input bindings
/// (wheel / keys) that mutate it are EPIC-3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Scroll {
    pub offset_rows: u64,
}

impl Scroll {
    /// The maximum scroll offset that still keeps content on screen: the last full
    /// viewport. `0` when the whole timeline fits (it then pins to the top).
    #[must_use]
    fn max_offset(total_rows: u64, viewport_rows: u64) -> u64 {
        total_rows.saturating_sub(viewport_rows)
    }

    /// This offset clamped into `[0, max_offset]` for the given content/viewport.
    #[must_use]
    pub fn clamped(self, total_rows: u64, viewport_rows: u64) -> Self {
        Self {
            offset_rows: self
                .offset_rows
                .min(Self::max_offset(total_rows, viewport_rows)),
        }
    }

    /// Jump to the top (the oldest block).
    pub fn to_top(&mut self) {
        self.offset_rows = 0;
    }

    /// Jump to the bottom (pin to the latest block - the live terminal default).
    pub fn to_bottom(&mut self, total_rows: u64, viewport_rows: u64) {
        self.offset_rows = Self::max_offset(total_rows, viewport_rows);
    }

    /// Scroll by a signed row delta (positive = toward the bottom), clamped into
    /// range so a wheel fling can never scroll past either end.
    pub fn by(&mut self, delta: i64, total_rows: u64, viewport_rows: u64) {
        let max = Self::max_offset(total_rows, viewport_rows) as i64;
        self.offset_rows = (self.offset_rows as i64 + delta).clamp(0, max) as u64;
    }
}

/// The left-gutter status marker for a block (ticket T-2.7). This is the *semantic*
/// state; the glyph + token color the renderer maps it to are EPIC-4 (T-4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GutterMarker {
    /// The command is still running (no `D` / exit yet) - a running pulse.
    Running,
    /// Finished with exit 0 - a success tick.
    Ok,
    /// Finished with a non-zero exit - a failure dot carrying the code.
    Failed(i32),
    /// Finished but the exit code is unknown (Ctrl-C / a missing `D`).
    Unknown,
    /// A full-screen (alt-screen) app block - "ran vim".
    Interactive,
    /// A heuristic (approximate) block - boundaries are a best-effort guess, not
    /// integration-confirmed (ticket T-2.6); the renderer labels it so.
    Approximate,
}

impl GutterMarker {
    /// The marker for `block`. Approximate and interactive take priority over the
    /// exit-based markers because they describe how the block was *segmented* (and
    /// they are mutually exclusive: heuristic blocks are never interactive).
    #[must_use]
    pub fn for_block(block: &Block) -> Self {
        if block.approximate {
            Self::Approximate
        } else if block.interactive {
            Self::Interactive
        } else if block.is_running() {
            Self::Running
        } else {
            match block.exit_code {
                Some(0) => Self::Ok,
                Some(code) => Self::Failed(code),
                None => Self::Unknown,
            }
        }
    }
}

/// One rendered row within a visible block, in top-to-bottom timeline order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineRow {
    /// The command / prompt line (block-relative display row 0).
    Command,
    /// Output row `index` into the block's `output` snapshot (0-based, always less
    /// than the collapse cap [`aterm_core::COLLAPSED_OUTPUT_ROWS`]).
    Output(usize),
    /// The "... +`hidden` lines" affordance shown at the foot of a collapsed block
    /// (ticket T-2.7, AC4).
    CollapseAffordance { hidden: u64 },
}

/// A block intersecting the viewport, with its on-screen placement and the subset of
/// its rows that are actually visible. "Virtualize twice": this block was picked by
/// the SumTree, and only `rows` of it become geometry.
#[derive(Debug, Clone)]
pub struct VisibleBlock<'a> {
    /// Index of this block in the [`BlockList`].
    pub index: usize,
    /// The block itself (command text, cwd, exit, approximate/interactive flags).
    pub block: &'a Block,
    /// The left-gutter status marker.
    pub gutter: GutterMarker,
    /// This block's full display height in rows ([`Block::display_height_rows`]).
    pub display_height: u64,
    /// The display-row of this block's TOP edge, relative to the viewport top.
    /// Negative when the block begins above the viewport (partially scrolled off the
    /// top); the renderer draws the top hairline separator only when this is `>= 0`.
    pub top_in_viewport: i64,
    /// The display-row (relative to the viewport top) where the FIRST entry of `rows`
    /// is drawn. `0` when the block is scrolled so its top is above the viewport.
    pub first_row_in_viewport: i64,
    /// The visible rows of this block, top-to-bottom. Only rows intersecting the
    /// viewport are present (row-level virtualization); empty is impossible for a
    /// block the SumTree returned.
    pub rows: Vec<TimelineRow>,
}

/// What the renderer should draw this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineMode {
    /// The virtualized block timeline.
    Timeline,
    /// A full-screen app owns the screen (ticket T-2.7 AC3 / ADR-0007): draw the alt
    /// grid as one full-window surface, NOT the block list. The block list + scroll
    /// are preserved and resumed when the app exits.
    AltScreen,
}

/// The laid-out timeline for one frame.
#[derive(Debug, Clone)]
pub struct TimelineLayout<'a> {
    /// Whether to draw the block timeline or the full-window alt-screen surface.
    pub mode: TimelineMode,
    /// Total scroll extent of the whole timeline, in display rows.
    pub total_rows: u64,
    /// The clamped scroll offset actually used for this layout.
    pub scroll: Scroll,
    /// Blocks intersecting the viewport, top-to-bottom. Empty in `AltScreen` mode.
    pub visible: Vec<VisibleBlock<'a>>,
}

impl TimelineLayout<'_> {
    /// Number of blocks that built geometry this frame - the AC1 virtualization
    /// counter. Stays bounded by the viewport even for a 10k-block list.
    #[must_use]
    pub fn visible_block_count(&self) -> usize {
        self.visible.len()
    }

    /// Total rows of geometry built this frame across all visible blocks (O(visible
    /// cells) is the per-frame cost target; this is its row half).
    #[must_use]
    pub fn visible_row_count(&self) -> usize {
        self.visible.iter().map(|b| b.rows.len()).sum()
    }
}

/// Lay out the timeline for one frame (ticket T-2.7). Pure - no GPU, no clock.
///
/// See the module docs for the "virtualize twice" + one-coordinate-space + alt-screen
/// contract. `alt_screen` comes from the published `Snapshot`; `viewport_rows` is the
/// window height in grid rows; `scroll` is clamped here, so the caller can pass an
/// over-scrolled value and read the corrected one back from [`TimelineLayout::scroll`].
#[must_use]
pub fn layout(
    blocks: &BlockList,
    alt_screen: bool,
    scroll: Scroll,
    viewport_rows: u64,
) -> TimelineLayout<'_> {
    let total_rows = blocks.total_height_rows();
    let scroll = scroll.clamped(total_rows, viewport_rows);

    if alt_screen {
        // A full-screen app owns the screen: no block geometry, scroll untouched.
        return TimelineLayout {
            mode: TimelineMode::AltScreen,
            total_rows,
            scroll,
            visible: Vec::new(),
        };
    }

    let mut visible = Vec::new();
    if viewport_rows > 0 {
        let vp_top = scroll.offset_rows;
        let vp_bottom = vp_top.saturating_add(viewport_rows); // exclusive
        for index in blocks.blocks_in_viewport(vp_top, viewport_rows) {
            let Some(block) = blocks.get(index) else {
                continue;
            };
            let block_top = blocks.block_top_row(index);
            let (rows, first_abs_row) = visible_rows(block, block_top, vp_top, vp_bottom);
            visible.push(VisibleBlock {
                index,
                block,
                gutter: GutterMarker::for_block(block),
                display_height: block.display_height_rows(),
                top_in_viewport: block_top as i64 - vp_top as i64,
                first_row_in_viewport: first_abs_row as i64 - vp_top as i64,
                rows,
            });
        }
    }

    TimelineLayout {
        mode: TimelineMode::Timeline,
        total_rows,
        scroll,
        visible,
    }
}

/// The display rows of `block` (anchored at absolute display-row `block_top`) that
/// fall within `[vp_top, vp_bottom)`, in order, plus the absolute display-row of the
/// first visible one.
///
/// A block's display rows are exactly [`Block::display_height_rows`] of them, in
/// order: row 0 is the command line; rows `1..=shown` are output rows `0..shown`
/// (capped at the collapse limit); and when collapsed a final "... +N lines"
/// affordance row. Building the row list and then clipping it to the viewport is the
/// row-level half of "virtualize twice".
fn visible_rows(
    block: &Block,
    block_top: u64,
    vp_top: u64,
    vp_bottom: u64,
) -> (Vec<TimelineRow>, u64) {
    let shown = block.shown_output_rows();
    let hidden = block.collapsed_hidden_rows();

    // The block's full display-row list (length == display_height_rows()).
    let mut all = Vec::with_capacity(1 + shown as usize + usize::from(hidden.is_some()));
    all.push(TimelineRow::Command);
    for i in 0..shown as usize {
        all.push(TimelineRow::Output(i));
    }
    if let Some(hidden) = hidden {
        all.push(TimelineRow::CollapseAffordance { hidden });
    }

    // Keep only the rows whose absolute display-row is inside the viewport.
    let mut rows = Vec::new();
    let mut first_abs = block_top;
    for (k, row) in all.into_iter().enumerate() {
        let abs = block_top + k as u64;
        if abs >= vp_top && abs < vp_bottom {
            if rows.is_empty() {
                first_abs = abs;
            }
            rows.push(row);
        }
    }
    (rows, first_abs)
}

/// The number of blocks that WOULD build geometry for this viewport, computed without
/// allocating the full layout - O(log n) via the SumTree (ticket T-2.7, the AC1
/// counter for the live render path; [`layout`] is for drawing + tests). Zero in
/// alt-screen mode (the block list is not drawn).
#[must_use]
pub fn visible_block_count(
    blocks: &BlockList,
    alt_screen: bool,
    scroll: Scroll,
    viewport_rows: u64,
) -> usize {
    if alt_screen || viewport_rows == 0 {
        return 0;
    }
    let total = blocks.total_height_rows();
    let scroll = scroll.clamped(total, viewport_rows);
    blocks
        .blocks_in_viewport(scroll.offset_rows, viewport_rows)
        .len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_core::{BlockSegmenter, Mark, PromptKind, RowSnapshot, COLLAPSED_OUTPUT_ROWS};

    /// Build a `BlockList` of `n` finished, output-less command blocks (each 1
    /// display row) via the real segmenter - the public way to populate a list.
    fn build_blocks(n: usize) -> BlockList {
        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        for i in 0..n {
            let base = i * 4;
            seg.apply(&Mark::Prompt(PromptKind::PromptStart), base, &mut list);
            seg.apply(&Mark::Prompt(PromptKind::OutputStart), base + 1, &mut list);
            seg.apply(
                &Mark::Prompt(PromptKind::CommandDone { exit_code: Some(0) }),
                base + 3,
                &mut list,
            );
        }
        list
    }

    fn blank_rows(n: usize) -> Vec<RowSnapshot> {
        vec![RowSnapshot::new(Vec::new()); n]
    }

    #[test]
    fn empty_timeline_has_no_visible_blocks() {
        let blocks = BlockList::new();
        let l = layout(&blocks, false, Scroll::default(), 30);
        assert_eq!(l.total_rows, 0);
        assert_eq!(l.mode, TimelineMode::Timeline);
        assert!(l.visible.is_empty());
        assert_eq!(l.visible_block_count(), 0);
    }

    #[test]
    fn virtualization_builds_only_on_screen_blocks() {
        // AC1: a 10k-block timeline builds geometry for only the ~viewport blocks on
        // screen, never the whole list. The SumTree culls in O(log n).
        let blocks = build_blocks(10_000);
        assert_eq!(blocks.total_height_rows(), 10_000);
        let viewport = 40u64;
        let l = layout(&blocks, false, Scroll { offset_rows: 5_000 }, viewport);
        assert_eq!(l.mode, TimelineMode::Timeline);
        assert!(
            l.visible_block_count() <= viewport as usize + 1,
            "only on-screen blocks build geometry, got {}",
            l.visible_block_count()
        );
        assert!(l.visible_block_count() >= viewport as usize - 1);
        // The first visible block contains the top scroll row (each block is 1 row).
        assert_eq!(l.visible.first().unwrap().index, 5_000);
        // The cheap counter agrees with the full layout.
        assert_eq!(
            visible_block_count(&blocks, false, Scroll { offset_rows: 5_000 }, viewport),
            l.visible_block_count()
        );
    }

    #[test]
    fn scroll_to_top_and_bottom_land_on_the_right_block() {
        // AC2: scroll-to-top / scroll-to-bottom jumps land on the correct block via
        // the SumTree.
        let blocks = build_blocks(1_000);
        let viewport = 30u64;
        let total = blocks.total_height_rows();

        let mut scroll = Scroll::default();
        scroll.to_bottom(total, viewport);
        let bottom = layout(&blocks, false, scroll, viewport);
        assert_eq!(
            bottom.visible.last().unwrap().index,
            999,
            "scroll-to-bottom shows the last block"
        );

        scroll.to_top();
        let top = layout(&blocks, false, scroll, viewport);
        assert_eq!(top.visible.first().unwrap().index, 0);
        assert_eq!(
            top.visible.first().unwrap().top_in_viewport,
            0,
            "block 0 sits at the viewport top"
        );
    }

    #[test]
    fn scroll_clamps_within_bounds() {
        let blocks = build_blocks(10); // total 10 display rows
        let viewport = 4u64;
        // Over-scroll clamps to max_offset = total - viewport = 6.
        let l = layout(&blocks, false, Scroll { offset_rows: 9_999 }, viewport);
        assert_eq!(l.scroll.offset_rows, 6);
        assert_eq!(
            l.visible.last().unwrap().index,
            9,
            "clamped scroll still shows the last block"
        );

        // by()/to_bottom respect the same bounds.
        let mut s = Scroll::default();
        s.by(100, 10, viewport);
        assert_eq!(s.offset_rows, 6, "by() clamps at the bottom");
        s.by(-100, 10, viewport);
        assert_eq!(s.offset_rows, 0, "by() clamps at the top");

        // Content shorter than the viewport pins to the top (max_offset == 0).
        let mut s2 = Scroll { offset_rows: 5 };
        s2.to_bottom(3, 30);
        assert_eq!(s2.offset_rows, 0);
    }

    #[test]
    fn long_block_collapses_with_affordance() {
        // AC4: a long-output block collapses to the cap + a "... +N lines" affordance.
        let mut blocks = build_blocks(1);
        let cap = COLLAPSED_OUTPUT_ROWS as usize;
        blocks.set_block_output(0, blank_rows(cap + 123));
        // A viewport taller than the collapsed block, so all its display rows show.
        let l = layout(&blocks, false, Scroll::default(), 1_000);
        let vb = &l.visible[0];
        assert_eq!(
            vb.rows.len(),
            1 + cap + 1,
            "command + capped output rows + one affordance row"
        );
        assert_eq!(vb.rows[0], TimelineRow::Command);
        assert!(matches!(vb.rows[cap], TimelineRow::Output(_)));
        assert_eq!(
            vb.rows[1 + cap],
            TimelineRow::CollapseAffordance { hidden: 123 }
        );
        assert_eq!(
            vb.display_height,
            1 + COLLAPSED_OUTPUT_ROWS + 1,
            "the collapsed block reserves command + cap + affordance, not 1 + long"
        );
    }

    #[test]
    fn row_level_virtualization_clips_a_block_to_the_viewport() {
        // The row half of "virtualize twice": a block taller than the viewport emits
        // only the rows on screen. One block of 1 + 10 = 11 display rows, a 4-row
        // viewport scrolled 3 rows down.
        let mut blocks = build_blocks(1);
        blocks.set_block_output(0, blank_rows(10));
        let l = layout(&blocks, false, Scroll { offset_rows: 3 }, 4);
        let vb = &l.visible[0];
        // Display rows: Command@0, Output(0)@1, ..., Output(9)@10. Viewport [3,7) ->
        // abs 3,4,5,6 -> Output(2),Output(3),Output(4),Output(5).
        assert_eq!(vb.rows.len(), 4);
        assert_eq!(vb.rows[0], TimelineRow::Output(2));
        assert_eq!(vb.rows[3], TimelineRow::Output(5));
        assert_eq!(
            vb.first_row_in_viewport, 0,
            "the first visible row sits at the viewport top"
        );
        assert_eq!(
            vb.top_in_viewport, -3,
            "the block's top edge is 3 rows above the viewport"
        );
    }

    #[test]
    fn alt_screen_switches_mode_and_preserves_scroll() {
        // AC3: a full-screen app switches to the alt-screen surface (no block
        // geometry) and the scroll is untouched, so exiting resumes the timeline.
        let blocks = build_blocks(100);
        let viewport = 20u64;
        let scroll = Scroll { offset_rows: 50 }.clamped(blocks.total_height_rows(), viewport);

        let alt = layout(&blocks, true, scroll, viewport);
        assert_eq!(alt.mode, TimelineMode::AltScreen);
        assert!(
            alt.visible.is_empty(),
            "no block geometry while a full-screen app owns the screen"
        );
        assert_eq!(alt.scroll, scroll, "alt-screen leaves the scroll untouched");
        assert_eq!(
            visible_block_count(&blocks, true, scroll, viewport),
            0,
            "the counter reports zero block geometry in alt-screen mode"
        );

        // Exiting (alt_screen=false) resumes the timeline at the same scroll.
        let resumed = layout(&blocks, false, alt.scroll, viewport);
        assert_eq!(resumed.mode, TimelineMode::Timeline);
        assert_eq!(resumed.scroll, scroll);
        assert!(!resumed.visible.is_empty());
    }

    #[test]
    fn gutter_marker_reflects_block_state() {
        // ok / failed / running / unknown via the real segmenter.
        let a = || Mark::Prompt(PromptKind::PromptStart);
        let c = || Mark::Prompt(PromptKind::OutputStart);
        let d = |code| {
            Mark::Prompt(PromptKind::CommandDone {
                exit_code: Some(code),
            })
        };

        let mut list = BlockList::new();
        let mut seg = BlockSegmenter::new();
        // 0: ok.
        seg.apply(&a(), 0, &mut list);
        seg.apply(&c(), 1, &mut list);
        seg.apply(&d(0), 2, &mut list);
        // 1: failed(2).
        seg.apply(&a(), 3, &mut list);
        seg.apply(&c(), 4, &mut list);
        seg.apply(&d(2), 5, &mut list);
        // 2: running (no D yet).
        seg.apply(&a(), 6, &mut list);
        seg.apply(&c(), 7, &mut list);

        assert_eq!(
            GutterMarker::for_block(list.get(0).unwrap()),
            GutterMarker::Ok
        );
        assert_eq!(
            GutterMarker::for_block(list.get(1).unwrap()),
            GutterMarker::Failed(2)
        );
        assert_eq!(
            GutterMarker::for_block(list.get(2).unwrap()),
            GutterMarker::Running
        );

        // unknown: a running block orphaned by a missing D (next prompt closes it
        // with exit None).
        seg.apply(&a(), 8, &mut list); // closes block 2 as unknown
        assert_eq!(
            GutterMarker::for_block(list.get(2).unwrap()),
            GutterMarker::Unknown
        );

        // interactive: a TUI block (set via the alt-screen transition).
        let mut tui = BlockList::new();
        let mut tseg = BlockSegmenter::new();
        tseg.apply(&a(), 0, &mut tui);
        tseg.apply(&c(), 1, &mut tui);
        tseg.set_alt_screen(true, &mut tui);
        assert_eq!(
            GutterMarker::for_block(tui.get(0).unwrap()),
            GutterMarker::Interactive
        );
    }

    #[test]
    fn approximate_blocks_carry_the_approximate_marker() {
        use aterm_core::HeuristicSegmenter;
        let mut list = BlockList::new();
        let mut h = HeuristicSegmenter::new();
        h.note_prompt_if_idle(2, 0, &mut list); // anchor the first prompt
        h.observe_output(b"ls\r\nout\n");
        h.note_prompt_if_idle(2, 30, &mut list); // one approximate block
        assert_eq!(list.len(), 1);
        let l = layout(&list, false, Scroll::default(), 30);
        assert_eq!(l.visible[0].gutter, GutterMarker::Approximate);
        assert!(l.visible[0].block.approximate);
    }

    #[test]
    fn zero_height_viewport_builds_nothing() {
        let blocks = build_blocks(50);
        let l = layout(&blocks, false, Scroll::default(), 0);
        assert!(l.visible.is_empty());
        assert_eq!(visible_block_count(&blocks, false, Scroll::default(), 0), 0);
    }
}
