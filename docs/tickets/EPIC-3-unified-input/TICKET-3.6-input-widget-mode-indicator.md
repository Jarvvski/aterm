---
id: T-3.6
epic: EPIC-3-unified-input
title: Input box widget + iA mode indicator (prompt glyph + chip)
status: ready-for-agent
labels: [ui, input, design]
depends_on: [T-3.1, T-4.2]
---

# Goal

Render the single shell-first input box as a persistent bottom footer with the iA-restrained mode indicator: a prompt glyph + a small SHELL/AGENT chip carry the routing target; the caret stays the one accent blue in both modes. No banner, no second box.

# Context

- Research: [05-unified-input-ux.md](../../research/05-unified-input-ux.md) section 3 (mode indication, ranked) + Recommendation 6; [07-ia-design-language.md](../../research/07-ia-design-language.md) section 5 (Prompt component, routing-target indicator) and design-system.md sections 5 + 7. Owner open-question #5 (recolor caret vs chip-only): **default is glyph + chip with the caret staying `accent.primary` in both modes**; recoloring the caret per mode is the owner-confirm alternative - note an amber agent-mode caret would collide with the `caution` risk color, so it is not the default.

# Implementation notes

- Crate: `aterm-ui`. Module `input_widget`.
- Full-width input, `iM Writing Mono NFM`, `fg.primary`, thin 2px caret. Hairline above separating from the timeline; persistent bottom zone with `space.4` padding.
- Mode indicator: the caret stays `accent.primary` (blue) in BOTH modes. The mode is carried by the prompt glyph - Shell = `❯`-class glyph, Agent = `✦`/spark glyph - plus a small right-aligned SHELL/AGENT chip (`type.label`, `font.ui`). Optional secondary for color-blind reinforcement: caret shape (block vs underline) and placeholder text ("Type a command" / "Ask the agent").
- Toggle cross-fade <= motion.fast (90ms) - a single cheap interpolation, never a layout reflow (60fps floor).
- Render the highlight overlay + ghost text from T-3.5; render preedit from T-3.2.
- Consume tokens from `aterm-tokens` (T-4.1/T-4.2): `accent.primary`, caret colors, font names.

# Acceptance criteria

- The input renders at the bottom, edge-to-edge, with the correct fonts and hairline.
- Toggling mode visibly changes the prompt glyph + SHELL/AGENT chip within 90ms, no reflow, text preserved; the caret stays accent-blue.
- Ghost text renders as a muted gray tail; preedit renders inline during composition.
- The indicator is legible in both light "paper" and dark themes.
- No per-frame allocation introduced (T-1.8 assertion still passes).

# Out of scope

- The routing logic (T-3.3) and history (T-3.7).
- Final component spec doc (T-4.6).
