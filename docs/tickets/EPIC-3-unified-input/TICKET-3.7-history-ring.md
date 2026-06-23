---
id: T-3.7
epic: EPIC-3-unified-input
title: Shared history ring + per-mode query lens
status: ready-for-agent
labels: [core, input]
depends_on: [T-3.1]
---

# Goal

One shared, wall-clock-ordered history ring storing both shell commands and agent prompts with a `mode` tag, exposed through two query lenses so Up-arrow / Ctrl-R in Shell mode searches shell entries and in Agent mode searches agent prompts.

# Context

- Research: [05-unified-input-ux.md](../../research/05-unified-input-ux.md) section 4 (shared vs separate history) + Recommendation 8. Owner open-question #3 (history scope default; do agent prompts leak into the real shell history file - default: no).

# Implementation notes

- Crate: `aterm-core` (pure data) consumed by `aterm-ui`/`aterm-app`.
- A single ring: entries `{ text, mode: Shell|Agent, timestamp }`. Two query lenses (Shell-only, Agent-only) with a user setting to widen either to "all".
- Up-arrow / Ctrl-R use the lens matching the current `InputModel.mode`. Shell-mode ghost text (T-3.5) draws from the Shell lens.
- Do NOT write agent prompts into the user's real shell history file; aterm's history is separate (persist to aterm's own config/data dir).

# Acceptance criteria

- Submitting a shell command and an agent prompt stores both with correct mode tags + timestamps.
- Up-arrow in Shell mode cycles shell entries only; in Agent mode, agent entries only.
- Ctrl-R fuzzy/prefix search respects the lens.
- The "widen to all" setting surfaces both in either mode.
- Agent prompts are absent from the user's shell history file (assert it is untouched).

# Out of scope

- Persistence format/migration polish (config work in T-8.3).
- Completion menus (later).
