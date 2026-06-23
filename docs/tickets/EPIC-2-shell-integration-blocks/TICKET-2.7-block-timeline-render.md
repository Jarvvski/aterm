---
id: T-2.7
epic: EPIC-2-shell-integration-blocks
title: Block/timeline rendering (virtualized)
status: ready-for-agent
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
