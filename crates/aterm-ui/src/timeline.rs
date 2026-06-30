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

/// The inter-block vertical rhythm, in whole *display* rows (ticket T-4.7): one blank
/// line-box of whitespace between adjacent blocks, so the timeline reads like iA Writer
/// rather than a dense terminal. This is the row-coordinate realization of the design's
/// `aterm_tokens::space::S6` (~24px) "between blocks" token - the row coordinate's
/// quantum is the grid line box (~17px logical), so the gap is one row; an exact-px
/// vertical gap would require a pixel-precise scroll coordinate (the row-based SumTree /
/// [`Scroll`] coordinate is whole-row), a deliberately deferred refactor.
///
/// The gap is baked into the gapped coordinate ([`total_display_rows`], [`gapped_top`])
/// the layout reports, so the scroll extent, scroll-clamp, hit-testing, and on-screen
/// placement ALL account for it - it is never added "only at paint time" (T-4.7 AC4).
/// The renderer (`timeline_render`) draws these gap rows as empty whitespace with one
/// muted hairline centered in each boundary gap.
pub const GAP_ROWS: u64 = 1;

/// The timeline's total scroll extent in *display* rows, INCLUDING the inter-block gaps
/// ([`GAP_ROWS`] between each adjacent pair) - the gapped-coordinate analogue of
/// [`BlockList::total_height_rows`] (which is the gap-less content total). A list of `n`
/// blocks has `n - 1` interior boundaries, so `n.saturating_sub(1)` gaps. This is the
/// extent the scroll position is clamped against (T-4.7 AC4); both [`layout`] and the
/// live caller's scroll-to-bottom use it so they share one coordinate.
#[must_use]
pub fn total_display_rows(blocks: &BlockList) -> u64 {
    let n = blocks.len() as u64;
    blocks.total_height_rows() + n.saturating_sub(1) * GAP_ROWS
}

/// The gapped-coordinate display-row of block `i`'s top content edge: its gap-less
/// content top ([`BlockList::block_top_row`], O(log n) via the SumTree) plus the `i`
/// inter-block gaps that precede it. Strictly increasing in `i` (each block is >= 1
/// content row plus a gap), so the viewport binary searches below are well-defined.
#[must_use]
fn gapped_top(blocks: &BlockList, i: usize) -> u64 {
    blocks.block_top_row(i) + i as u64 * GAP_ROWS
}

/// Smallest block index whose gapped BOTTOM edge is strictly below `row` (i.e. the first
/// block that still has a content row at or beyond `row`) - the first block intersecting
/// a viewport whose top is `row`. O(log n) over the (monotonic) gapped bottoms; each
/// probe is an O(log n) SumTree prefix, so O(log^2 n) - the same backbone as
/// [`BlockList::blocks_in_viewport`], extended with the uniform gap term.
#[must_use]
fn first_block_intersecting(blocks: &BlockList, row: u64) -> usize {
    let mut lo = 0usize;
    let mut hi = blocks.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let g_bottom =
            gapped_top(blocks, mid) + blocks.get(mid).map_or(0, Block::display_height_rows);
        if g_bottom > row {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

/// Smallest block index whose gapped TOP edge is at or past `row` - the first block
/// entirely below a viewport whose bottom (exclusive) is `row`. O(log^2 n) like
/// [`first_block_intersecting`]; the half-open `[first_block_intersecting(top),
/// first_block_at_or_below(bottom))` is exactly the set of blocks the layout draws.
#[must_use]
fn first_block_at_or_below(blocks: &BlockList, row: u64) -> usize {
    let mut lo = 0usize;
    let mut hi = blocks.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if gapped_top(blocks, mid) >= row {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

/// Vertical scroll position of the timeline, in *display* rows from the very top
/// (`0` = top of the oldest block). Display rows include the inter-block gaps
/// ([`GAP_ROWS`]); clamp against [`total_display_rows`], not the gap-less content total.
/// The renderer owns one of these; input bindings (wheel / keys) that mutate it are
/// EPIC-3.
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
    /// An agent transcript step (ticket T-5.10) - not a command block. Its styling
    /// is EPIC-4 (T-4.6); for now it reads as a neutral, labelled marker.
    Agent,
}

impl GutterMarker {
    /// The marker for `block`. For a command block, Approximate and interactive take
    /// priority over the exit-based markers because they describe how the block was
    /// *segmented* (and they are mutually exclusive: heuristic blocks are never
    /// interactive). An agent step is always [`Self::Agent`].
    #[must_use]
    pub fn for_block(block: &Block) -> Self {
        match block {
            Block::Agent(_) => Self::Agent,
            Block::Command(c) => {
                if c.approximate {
                    Self::Approximate
                } else if c.interactive {
                    Self::Interactive
                } else if c.is_running() {
                    Self::Running
                } else {
                    match c.exit_code {
                        Some(0) => Self::Ok,
                        Some(code) => Self::Failed(code),
                        None => Self::Unknown,
                    }
                }
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
    /// Line `index` (0-based) of an agent step's wrapped text (ticket T-5.10). Only
    /// appears for a [`Block::Agent`] entry.
    Agent(usize),
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
    // The scroll extent and every placement below are in the GAPPED display-row
    // coordinate (content rows + the inter-block [`GAP_ROWS`]), so scroll, hit-testing,
    // and on-screen placement share one coordinate that already accounts for the
    // whitespace rhythm (T-4.7 AC4).
    let total_rows = total_display_rows(blocks);
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
                                                              // "Virtualize twice" in the gapped coordinate: the SumTree (via the gapped
                                                              // binary searches) picks the blocks intersecting [vp_top, vp_bottom), then
                                                              // `visible_rows` keeps only the on-screen rows of each.
        let start = first_block_intersecting(blocks, vp_top);
        let end = first_block_at_or_below(blocks, vp_bottom);
        for index in start..end {
            let Some(block) = blocks.get(index) else {
                break;
            };
            let g_top = gapped_top(blocks, index);
            let (rows, first_abs_row) = visible_rows(block, g_top, vp_top, vp_bottom);
            visible.push(VisibleBlock {
                index,
                block,
                gutter: GutterMarker::for_block(block),
                display_height: block.display_height_rows(),
                top_in_viewport: g_top as i64 - vp_top as i64,
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
    // The block's full display-row list (length == display_height_rows()).
    let all: Vec<TimelineRow> = match block {
        Block::Command(c) => {
            let shown = c.shown_output_rows();
            let hidden = c.collapsed_hidden_rows();
            let mut all = Vec::with_capacity(1 + shown as usize + usize::from(hidden.is_some()));
            // row 0 is the command line; rows 1..=shown are output rows.
            all.push(TimelineRow::Command);
            for i in 0..shown as usize {
                all.push(TimelineRow::Output(i));
            }
            if let Some(hidden) = hidden {
                all.push(TimelineRow::CollapseAffordance { hidden });
            }
            all
        }
        // An agent step is its text lines, one TimelineRow::Agent per line.
        Block::Agent(a) => (0..a.line_count() as usize)
            .map(TimelineRow::Agent)
            .collect(),
    };

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
    let total = total_display_rows(blocks);
    let scroll = scroll.clamped(total, viewport_rows);
    let vp_top = scroll.offset_rows;
    let vp_bottom = vp_top.saturating_add(viewport_rows);
    // Same gapped [start, end) span the full `layout` builds, computed without
    // allocating the block list - O(log n) via the SumTree-backed binary searches.
    first_block_at_or_below(blocks, vp_bottom)
        .saturating_sub(first_block_intersecting(blocks, vp_top))
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
        // screen, never the whole list. The SumTree culls in O(log n) - now in the
        // GAPPED coordinate (each 1-row block is followed by a 1-row gap, so a block
        // occupies 2 gapped rows and ~viewport/2 blocks fit). T-4.7.
        let blocks = build_blocks(10_000);
        assert_eq!(blocks.total_height_rows(), 10_000, "gap-less content total");
        assert_eq!(
            total_display_rows(&blocks),
            10_000 + 9_999 * GAP_ROWS,
            "the scroll extent includes the 9,999 interior gaps"
        );
        let viewport = 40u64;
        let l = layout(&blocks, false, Scroll { offset_rows: 5_000 }, viewport);
        assert_eq!(l.mode, TimelineMode::Timeline);
        assert!(
            l.visible_block_count() <= viewport as usize + 1,
            "only on-screen blocks build geometry, got {}",
            l.visible_block_count()
        );
        // gapped_top(i) = 2i; viewport [5000, 5040) -> blocks 2500..2520 (20 of them).
        assert_eq!(l.visible_block_count(), 20);
        assert_eq!(l.visible.first().unwrap().index, 2_500);
        assert_eq!(l.visible.last().unwrap().index, 2_519);
        // The cheap O(log n) counter agrees with the full layout.
        assert_eq!(
            visible_block_count(&blocks, false, Scroll { offset_rows: 5_000 }, viewport),
            l.visible_block_count()
        );
    }

    /// Brute-force reference: the blocks whose GAPPED span intersects `[vp_top,
    /// vp_bottom)`, computed in O(n) directly from the height index + gap. The layout's
    /// O(log n) binary searches must agree with this for any heights/scroll (the
    /// gapped analogue of the core `height_index_matches_naive` test).
    fn naive_visible(blocks: &BlockList, vp_top: u64, vp_bottom: u64) -> Vec<usize> {
        (0..blocks.len())
            .filter(|&i| {
                let g_top = blocks.block_top_row(i) + i as u64 * GAP_ROWS;
                let g_bottom = g_top + blocks.get(i).unwrap().display_height_rows();
                g_top < vp_bottom && g_bottom > vp_top
            })
            .collect()
    }

    #[test]
    fn gapped_virtualization_matches_the_naive_reference() {
        // Varied heights so the gap term is non-trivial against the content tops.
        let mut blocks = build_blocks(30);
        for i in (0..30).step_by(3) {
            blocks.set_block_output(i, blank_rows(i % 7)); // 0..6 output rows
        }
        let total = total_display_rows(&blocks);
        for viewport in [1u64, 3, 8, total + 5] {
            for &scroll in &[0u64, 2, 7, total / 2, total.saturating_sub(1), total + 99] {
                let l = layout(
                    &blocks,
                    false,
                    Scroll {
                        offset_rows: scroll,
                    },
                    viewport,
                );
                let vp_top = l.scroll.offset_rows; // clamped
                let got: Vec<usize> = l.visible.iter().map(|b| b.index).collect();
                let want = naive_visible(&blocks, vp_top, vp_top + viewport);
                assert_eq!(got, want, "viewport={viewport} scroll={scroll}");
                // The cheap counter tracks the full layout for every case.
                assert_eq!(
                    visible_block_count(
                        &blocks,
                        false,
                        Scroll {
                            offset_rows: scroll
                        },
                        viewport
                    ),
                    got.len()
                );
            }
        }
    }

    #[test]
    fn scroll_to_top_and_bottom_land_on_the_right_block() {
        // AC2: scroll-to-top / scroll-to-bottom jumps land on the correct block via
        // the SumTree.
        let blocks = build_blocks(1_000);
        let viewport = 30u64;
        // Scroll-to-bottom uses the GAPPED extent (the live caller does the same), so
        // the bottom pins past the interior gaps to the genuine last block.
        let total = total_display_rows(&blocks);

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
        let blocks = build_blocks(10); // 10 content rows + 9 interior gaps = 19 gapped
        let viewport = 4u64;
        // Over-scroll clamps to max_offset = gapped_total - viewport = 19 - 4 = 15.
        assert_eq!(total_display_rows(&blocks), 19);
        let l = layout(&blocks, false, Scroll { offset_rows: 9_999 }, viewport);
        assert_eq!(l.scroll.offset_rows, 15);
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
    fn inter_block_gap_offsets_placement_not_just_paint() {
        // AC4: the inter-block gap is in the layout coordinate, so a following block's
        // placement (top_in_viewport) is shifted DOWN by GAP_ROWS - it is not added only
        // at paint time. Two 1-content-row blocks, both on screen from the top.
        let blocks = build_blocks(2);
        let l = layout(&blocks, false, Scroll::default(), 10);
        assert_eq!(l.visible.len(), 2);
        let (a, b) = (&l.visible[0], &l.visible[1]);
        assert_eq!(a.top_in_viewport, 0, "block 0 sits at the viewport top");
        assert_eq!(
            b.top_in_viewport,
            a.top_in_viewport + a.display_height as i64 + GAP_ROWS as i64,
            "block 1 is pushed down by block 0's height PLUS one gap row"
        );
        // The gap row (between the two) carries no block geometry: block 0 owns row 0,
        // block 1 owns row 2, and row 1 is the empty whitespace boundary.
        assert_eq!(b.top_in_viewport, 2);
    }

    #[test]
    fn scroll_extent_accounts_for_every_interior_gap() {
        // AC4: total_display_rows == content rows + (n-1) gaps, for collapsed blocks too.
        let mut blocks = build_blocks(4);
        blocks.set_block_output(1, blank_rows(5)); // block 1 -> 1 + 5 = 6 display rows
        let content = blocks.total_height_rows();
        assert_eq!(content, 1 + 6 + 1 + 1, "1 + (1+5) + 1 + 1 content rows");
        assert_eq!(
            total_display_rows(&blocks),
            content + 3 * GAP_ROWS,
            "4 blocks -> 3 interior gaps added to the scroll extent"
        );
        // A single block has no interior boundary -> no gap.
        assert_eq!(total_display_rows(&build_blocks(1)), 1);
        assert_eq!(total_display_rows(&BlockList::new()), 0);
    }

    #[test]
    fn alt_screen_switches_mode_and_preserves_scroll() {
        // AC3: a full-screen app switches to the alt-screen surface (no block
        // geometry) and the scroll is untouched, so exiting resumes the timeline.
        let blocks = build_blocks(100);
        let viewport = 20u64;
        // Seed against the GAPPED extent - the same coordinate every live path and
        // `layout` clamp against - so `alt.scroll == scroll` compares like with like.
        let scroll = Scroll { offset_rows: 50 }.clamped(total_display_rows(&blocks), viewport);

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
        assert!(l.visible[0].block.as_command().unwrap().approximate);
    }

    #[test]
    fn zero_height_viewport_builds_nothing() {
        let blocks = build_blocks(50);
        let l = layout(&blocks, false, Scroll::default(), 0);
        assert!(l.visible.is_empty());
        assert_eq!(visible_block_count(&blocks, false, Scroll::default(), 0), 0);
    }

    #[test]
    fn agent_steps_render_interleaved_with_command_blocks_in_order() {
        // T-5.10 AC1: an agent turn renders as ordered steps interleaved by wall-clock
        // with human command blocks in ONE timeline. Append order is wall-clock order;
        // layout visits the entries in that order, emitting command/output rows for a
        // command block and Agent(line) rows for an agent step.
        use aterm_core::{AgentBlock, AgentBlockKind};
        use std::time::Instant;

        let mut list = build_blocks(1); // one finished command (exit 0)
        list.push_agent(AgentBlock::new(
            AgentBlockKind::UserPrompt,
            "do it",
            Instant::now(),
        ));
        list.push_agent(AgentBlock::new(
            AgentBlockKind::AssistantText,
            "line one\nline two",
            Instant::now(),
        ));

        let l = layout(&list, false, Scroll::default(), 30);
        assert_eq!(l.visible.len(), 3, "command + two agent steps, in order");

        // Entry 0: the command block.
        assert_eq!(l.visible[0].gutter, GutterMarker::Ok);
        assert_eq!(l.visible[0].rows[0], TimelineRow::Command);

        // Entry 1: a single-line agent step.
        assert_eq!(l.visible[1].gutter, GutterMarker::Agent);
        assert_eq!(l.visible[1].rows, vec![TimelineRow::Agent(0)]);

        // Entry 2: a two-line agent step -> two Agent rows.
        assert_eq!(l.visible[2].gutter, GutterMarker::Agent);
        assert_eq!(
            l.visible[2].rows,
            vec![TimelineRow::Agent(0), TimelineRow::Agent(1)]
        );
        assert_eq!(l.visible[2].display_height, 2);
    }
}
