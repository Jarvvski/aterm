---
id: T-2.6
epic: EPIC-2-shell-integration-blocks
title: Three-state integration indicator + heuristic fallback
status: ready-for-agent
labels: [ui, shell-integration]
depends_on: [T-2.3, T-2.5]
---

# Goal

Surface a visible three-state integration status (Integrated / Heuristic / None) with a "why", confirmed only after a nonce-matched OSC 133;A is seen, plus a labeled heuristic block detector for unsupported/unintegrated shells. Never degrade silently (the prototype's #1 sin).

# Context

- Research: [04-shell-integration.md](../../research/04-shell-integration.md) section 4 + Recommendations 4, 7. Owner open-question #5/#3 (heuristic fallback vs honest "no blocks") - default to the labeled heuristic per the dossier lean.

# Implementation notes

- Crate: `aterm-core` owns the state (`IntegrationStatus { Integrated, Heuristic, None }` + a reason string); `aterm-ui` renders the indicator.
- Confirm "Integrated" only after the first nonce-matched `133;A` within a short window after spawn. If the shell is supported but no marks arrive -> "Heuristic" + enable the regex/heuristic block detector (newline + cursor-at-col-0 prompt detection). If unsupported shell -> "None".
- Indicator UI: a single glyph + tooltip in the block gutter or status strip (iA-restrained, uses tokens from Epic 4); a one-click "why?" explaining what is missing (e.g. "running fish 3.1 - upgrade to 3.2+ for native blocks", or "bash 3.2 - marks may be unreliable").
- Heuristic detector lives in `aterm-core` (block module) and produces clearly-labeled approximate blocks.

# Acceptance criteria

- A zsh/bash/fish session with working hooks shows "Integrated" only after a nonce-matched A.
- A supported shell with hooks disabled shows "Heuristic" and produces labeled approximate blocks.
- An unsupported shell (dash) shows "None".
- The "why?" reason string is populated for each non-Integrated case.
- Status transitions are observable and never silent.

# Out of scope

- The shims (T-2.2, T-2.3) and lifecycle (T-2.5).
- Final token/visual polish (Epic 4 supplies tokens; this wires the state).
