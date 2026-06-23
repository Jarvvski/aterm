---
id: T-4.6
epic: EPIC-4-design-system
title: Component specs - block, prompt, agent card, chip, risk badge
status: ready-for-agent
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
