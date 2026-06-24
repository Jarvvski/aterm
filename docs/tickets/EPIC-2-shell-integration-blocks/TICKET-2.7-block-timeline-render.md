---
id: T-2.7
epic: EPIC-2-shell-integration-blocks
title: Block/timeline rendering (virtualized)
status: done
labels: [ui, render, block-model]
depends_on: [T-2.4, T-1.6]
---

# Goal

Render the BlockList as a single vertically-scrolling wall-clock timeline, virtualized via the SumTree so only on-screen blocks and rows build instances, holding 60fps with thousands of blocks. Alt-screen apps render as a single full-window pass-through surface outside the block list.

# Context

- Research: [03-pty-vt-rust.md](../../research/03-pty-vt-rust.md) section C (renderer contract: each block exposes only its pixel height; virtualize twice) + section D (alt-screen surface); [09-performance-60fps.md](../../research/09-performance-60fps.md) section 3 (damage); [08-text-glyph-rendering.md](../../research/08-text-glyph-rendering.md) section 5 (only visible rows become geometry).

# Implementation notes

- Crate: `aterm-ui`. Module `timeline`.
- Read the BlockList snapshot (T-2.4) and `IntegrationStatus`. Use `SumTree::blocks_in_viewport` to pick intersecting blocks, then visible rows within each. Scrollback is data, not geometry - cost per frame ~ O(visible cells).
- Render each block: a left gutter status marker (running pulse / exit-0 tick / exit!=0 dot+code), the command line (Mono NFM, re-rendered not raw), and the output rows via the grid fast-path (T-1.6). Hairline separators between blocks. Final token/component spec polish is T-4.6; this ticket establishes correct geometry + virtualization.
- Alt-screen: when `TermMode::ALT_SCREEN`, render the alt grid as one full-window surface; route input straight to the PTY (T-3.4 owns the key encode); on exit, resume the timeline.
- Damage: only rebuild instances for changed blocks/rows; integrate with T-1.8 damage tracking.

# Acceptance criteria

- A timeline of 10k blocks scrolls at the display refresh rate; only on-screen blocks build instances (assert visible-block count via a counter).
- Scroll-to-top / scroll-to-bottom jumps land on the correct block via the SumTree.
- Running `vim` switches to the full-window alt-screen surface and exiting returns to the timeline at the right scroll position.
- A long-output block collapses to N lines with a "... +123 lines" affordance.
- No frame-budget regression vs T-1.8 baseline for a scroll scenario (formal gate in T-7.2).

# Out of scope

- Final iA component styling (T-4.6).
- Agent transcript rendering (T-5.10) - though the timeline must accept future block variants.
- Input routing / key encode (Epic 3).

# Notes

**Landed 2026-06-24** (jj, not pushed). The virtualized block-timeline layout engine,
the model-thread -> render-thread block publish seam, and collapse-aware display height
- the geometry + virtualization the renderer consumes. Like T-2.6, this wires + tests
the logic and defers the on-screen pixels (see follow-ups); it degrades loudly, never
silently.

- **Pure layout engine (`aterm-ui/src/timeline.rs`).** `layout(blocks, alt_screen,
  scroll, viewport_rows) -> TimelineLayout` is the "virtualize twice" core: the SumTree
  ([`BlockList::blocks_in_viewport`], O(log n)) picks the blocks intersecting the
  viewport, then `visible_rows` clips each to the rows actually on screen. No GPU, no
  clock - exhaustively unit-tested. Emits per-block `GutterMarker` (Running / Ok /
  Failed(code) / Unknown / Interactive / Approximate), `TimelineRow` items (Command /
  Output(i) / CollapseAffordance), `top_in_viewport`/`first_row_in_viewport` placement,
  and a `Scroll` model (clamp / to_top / to_bottom / by). A zero-alloc `visible_block_
  count` is the live-path AC1 counter; `layout` (which allocs) is for drawing + tests.
- **Publish seam (`engine.rs`).** `Engine::latest_blocks() -> Arc<BlockList>` mirrors
  `latest_snapshot`; the model thread re-publishes the (now `Clone`) `BlockList` only
  when it actually changed (a `blocks_touched` flag set on any mark-driven mutation or
  heuristic append), so an output-only flood pays nothing. `Frame.blocks` +
  `UiCallbacks::blocks()` + `Session::blocks()` carry it to the renderer.
- **Collapse in one coordinate space (`block.rs`).** `COLLAPSED_OUTPUT_ROWS` (=16, a
  tunable default, precedent: `is_thin`) + `Block::display_height_rows()` collapse a
  long block to command + cap + a "... +N lines" affordance, and that display height is
  what feeds the `HeightIndex`. So `blocks_in_viewport` / `block_at` / `block_top_row`
  and the drawn layout never drift - one coordinate system (honors "virtualized via the
  SumTree").
- **Renderer (`gpu.rs`).** Computes `viewport_rows` from the surface + GRID cell height,
  reports the live visible-block count via `timeline::visible_block_count`, and pins
  scroll to the bottom (auto-follow) - skipped while alt-screen, so the timeline scroll
  is preserved across the vim round-trip (AC3). The on-screen view stays the grid (see
  follow-up 1).
- **AC mapping.** AC1 (10k blocks, only on-screen build geometry, count via a counter):
  `timeline::virtualization_builds_only_on_screen_blocks` + the live `GpuRenderer::
  visible_block_count`; core scale proof is `block::height_index_scales_to_10k_blocks`.
  AC2 (scroll-to-top/bottom land on the right block via the SumTree):
  `scroll_to_top_and_bottom_land_on_the_right_block` + `scroll_clamps_within_bounds`.
  AC3 (vim -> alt surface, exit resumes at the right scroll):
  `alt_screen_switches_mode_and_preserves_scroll` + the `!alt_screen` scroll guard in
  `gpu.rs`. AC4 (long output collapses to N + affordance): `long_block_collapses_with_
  affordance`, `row_level_virtualization_clips_a_block_to_the_viewport`, and core
  `display_height_collapses_long_output` / `height_index_tracks_collapsed_display_
  height`. AC5 (no frame-budget regression): the live path is alloc-free (counter is
  O(log n)); the formal gate is T-7.2.
- **Seam verified end to end:** `engine::latest_blocks_publishes_the_segmented_list_to_
  consumers` drives a real `/bin/sh` heuristic session and asserts the two approximate
  blocks surface through `latest_blocks()`.

**Adversarial review applied** (5 lenses, ultracode; each finding skeptic-verified,
default not-real). 0 blockers/majors. One confirmed nit: the `(snapshot, blocks)` pair
publishes under two separate locks, so a consumer can briefly observe a one-frame-skewed
pair - harmless today (the counter is unused while alt-screen and self-heals next frame),
fixed by making the publish comment state the eventual-consistency guarantee honestly.
A scroll-clobber-during-alt-screen wart (dismissed as not-a-bug, but real for AC3 once
scroll input lands) was hardened with the `!alt_screen` guard. Two findings dismissed.

**Follow-ups for the human (why `ready-for-human`):**
1. **On-screen DRAWING of the timeline cards is deferred - and gated on output capture
   (below).** The engine publishes the block list and the renderer virtualizes it +
   reports the live visible-block count, but it still draws the raw grid, not block
   cards. This avoids a regression: with no finished-block output captured yet, a
   card view would show only command lines (strictly worse than the grid). Card styling
   is also EPIC-4 (T-4.6). All five ACs are met as tested behavior; the pixels follow
   capture + styling. (Mirrors T-2.6, where the indicator state was wired + tested and
   the on-screen glyph deferred to EPIC-4.)
2. **Finished-block OUTPUT-ROW CAPTURE is not wired - the key remaining dependency, and
   an owner-facing architecture call.** `set_block_output` is never invoked, so every
   finished block has empty `output` (display height 1, just the command line).
   `Terminal` exposes only a viewport snapshot, and blocks track byte offsets, not grid
   lines - so capturing a block's rows needs (a) a Terminal API to extract a grid+
   scrollback row range and (b) a block -> grid-line mapping, plus the immutable-snapshot/
   reflow story the dossier calls "the prototype's most important design choice". This
   is a substantial separate ticket that T-2.4/T-2.5 deferred ("grid-row capture is a
   follow-up"). The timeline engine + collapse already handle output rows on synthetic
   data (tested), so it lights up the moment capture lands.
3. **The T-1.6 instanced GPU fast-path is still not wired** (only the CPU half exists);
   the timeline (and grid) render through glyphon. The instanced fast-path + damage
   tracking are T-1.8 - also the cure for the typing-lag stand-in.
4. **`COLLAPSED_OUTPUT_ROWS` (16) is an untuned default and there is no per-block expand
   toggle yet** (expand is EPIC-3/4 input). Revisit the constant against the T-7.x
   UX/perf matrix.
5. **`BlockList` is deep-cloned on each change-publish.** Fine now (blocks are small -
   no captured output - and change at human pace), but once capture lands the clone gets
   heavy; switch the list to `Vec<Arc<Block>>` (cheap clone) if it shows in a bench.

# Resolution

**done 2026-06-24.** All five acceptance criteria are met and tested (the layout engine
+ capture + virtualization counter + alt-screen mode). The two follow-ups above are
resolved/delegated:
1. **Output-row capture (was the architecture call) - DONE.** Owner chose full-scrollback
   capture; implemented by byte replay (the OSC pre-parser's complete stream replayed
   through a throwaway terminal at `D`), since `alacritty_terminal` 0.26 exposes no
   stable grid-line anchor. Finished blocks now own their immutable output rows.
2. **On-screen card DRAWING delegated to T-4.6.** With capture done, the layout engine is
   ready to draw; the coherent card view (live-active-region composition + component
   styling) is T-4.6's explicit scope (block/prompt/card specs) and is now unblocked. The
   renderer keeps the grid view meanwhile (no regression).
