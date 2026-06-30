---
id: T-4.6
epic: EPIC-4-design-system
title: Component specs - block, prompt, agent card, chip, risk badge
status: done
labels: [ui, design]
depends_on: [T-4.1, T-2.7]
---

# Goal

Apply the iA component specs to the live UI: the command block, the unified prompt, the agent card, the status chip, and the risk-gate badge - flat rectangles, hairline separators, generous whitespace, one scarce accent, color+label always paired.

# Context

- Research: [07-ia-design-language.md](../../research/07-ia-design-language.md) section 5 (component guidance) + Recommendations 1-4, 9. Owner open-question #3 (how loud the risk gate is - quiet caution chip vs full-width banner; default: quiet chip).

# Implementation notes

- Crate: `aterm-ui`. Style the widgets built in T-2.7 (block/timeline) and T-3.6 (prompt) using `aterm-tokens` (T-4.1/T-4.2). No hardcoded hex.
- **Command block**: left gutter status marker (running pulse `accent.primary` dot / exit-0 thin `success` tick / exit!=0 `danger` dot + code in `type.caption`); command line Mono NFM `fg.primary`; output full-width `bg.canvas`; hairline top/bottom only; collapsed "... +N lines".
- **Prompt**: the SHELL/AGENT routing-target chip at the input's left edge (neutral fill for shell, `accent.primary.weak` for agent), cross-fades on toggle (motion.fast).
- **Agent card**: `bg.surface`, `radius.md`, 1px hairline, `space.4` padding, `space.6` vertical gap; header (Duo medium 500 `type.heading`) + status chip; prose body Duo `type.body` ~72ch; nested mini command blocks (Mono NFM) for tool calls with an inline risk-gate badge; reasoning text in muted `fg.secondary`.
- **Status chip**: `radius.sm`, Quattro `type.label`, variants neutral/info/success/caution/danger (weak tint + saturated text), hairline border only on neutral.
- **Risk-gate badge**: three states mapped to semantic colors - Allowed -> `success` (silent or "auto"); Needs approval -> `caution` "APPROVE?" + parsed reason in `type.caption`; Blocked -> `danger` "BLOCKED" + reason. Color is the fast signal but ALWAYS paired with a text label (color-blind safety). Sits in the gutter alignment so a scanning eye reads gutter color = safety state.
- Motion budget: only block insert (fade + 4px rise), gate state cross-fade, focus dim - all <= 220ms decelerate. No decorative spinners; running = one pulsing dot.

# Acceptance criteria

- All five components render to spec in both themes; no hardcoded colors (all via tokens).
- The risk-gate badge always shows a text label alongside color (verified for all three states).
- Toggling the prompt mode cross-fades the chip within motion.fast and preserves text.
- Motion is capped to the three allowed animations, each <= 220ms; no per-frame allocation introduced (T-1.8 assertion holds).
- A visual review on real tool output (ls/vim/git diff) confirms the iA look on both themes.

# Out of scope

- The agent-card *data* model (T-5.10) - this styles whatever the model provides.
- Focus-Mode dimming (T-8.5).

# Notes

**Inherited 2026-06-24 (from T-2.7, T-2.6).** This ticket now owns the on-screen DRAWING
of the block timeline. Its data + geometry are ready: T-2.7 landed the pure `timeline`
layout engine (`aterm-ui/src/timeline.rs`: virtualized blocks, gutter markers, output
rows, collapse affordance, scroll model) and finished-block output capture, and T-2.6
landed the integration-indicator presentation (`IntegrationIndicator`). Remaining for
this ticket: the component styling AND the live-active-region composition (drawing
finished blocks from captured output while the running command shows its live output,
without duplicating scrollback) - the renderer currently draws the raw grid as a
non-regressing stand-in. Scroll input is EPIC-3 (T-3.x).

**Done 2026-06-30.** Landed across five commits:
- `aterm-ui/src/components.rs` - the pure, token-driven, both-themes-tested component
  style layer (command block, prompt routing chip, agent card, status chip, 3-state
  risk-gate badge that always pairs a label with its color; the 3-animation motion
  budget). No hardcoded colors. (AC1 style, AC2, AC3-chip, AC4-motion.)
- The shared `GlyphAtlas` + rect/glyph pipelines were hoisted up to `GpuRenderer` (the
  T-4.3 forward-note) so grid/prose/timeline share one atlas; the grid's 60fps
  invariants are preserved.
- `aterm-ui/src/timeline_render.rs` - the GPU timeline compositor (gutter marker,
  command line, output rows via the shared `cell_render::emit_cell`, hairlines, collapse
  affordance), damage-gated, GPU-tested in both themes.
- The **live-active region** was solved at the data model rather than by snapshot-slicing
  (the published `Snapshot` exposes no prompt-row anchor): `aterm-core` now streams the
  running command's output into its block incrementally, and the running block shows its
  full uncollapsed tail. `GpuRenderer` draws the **block timeline as the primary view**
  (the raw grid only for alt-screen / no-engine), so the stand-in is replaced. (AC1 draw,
  AC4 no-per-frame-alloc via the timeline idle gate + the grid steady-state.)

**Consolidated forward (not regressions - dependency-gated):**
- The agent-card Duo prose body + the Quattro chrome chips are STYLED + tested but not
  yet DRAWN live (no agent-step data model) -> **T-5.10** (data) / **T-5.11** (approval
  UX) wire them through the same atlas.
- The unified-input prompt chip + the live pre-submit input echo are the input box's
  domain -> **T-3.6**. Until then a command's typed text appears in the timeline once
  submitted (its block's command line).
- AC5 (on-hardware iA visual review on real `ls`/`vim`/`git diff` output, both themes) is
  the **owner-watched** acceptance step, consistent with this crate's "GPU/window code is
  owner-watched, not unit-tested" convention; the render path itself is offscreen
  GPU-tested (gutter + output + hairline ink, both themes).
