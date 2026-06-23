---
id: T-3.1
epic: EPIC-3-unified-input
title: Pure InputModel reducer (text + selection + mode)
status: ready-for-agent
labels: [core, input]
depends_on: []
---

# Goal

Build the pure, offline-testable `InputModel` reducer that owns the in-progress command line - text, selection/caret, and a `mode: Shell|Agent` field - where the hotkey mutates ONLY `mode`, so text is preserved across the toggle by construction. This is the structural fix for the prototype's worst sin (the toggle that cleared context).

# Context

- Research: [05-unified-input-ux.md](../../research/05-unified-input-ux.md) sections 1, 2 (editing model) and Recommendations 1-3. The buffer is small (one occasionally-multiline command line); the data structure is not the bottleneck.
- ADR: unified-input model (one shell-first box; mode is a field, not a sigil; text preserved).

# Implementation notes

- Crate: `aterm-core` (or a dedicated `aterm-input` module within core - keep it pure, no UI/LLM). Confirm placement against the locked crate layout; it must be reusable by `aterm-ui` and `aterm-app`.
- Storage: `ropey = "0.6"` (char-indexed column math) or a plain `String` for v1. Do not over-engineer.
- Properties to port verbatim from the prototype's `CommandBuffer`:
  - Buffer stores characters only, never interprets them. A paste is ONE `insert` of the whole string as one undo unit; embedded newlines/control chars are literal and inert (structurally prevents paste-auto-execute).
  - Caret as an offset; motions pure (left/right/word/home/end/up/down with column memory); edits push undo units.
  - Submit is the caller reading `text` then resetting - the buffer does not decide whether Enter submits.
- `mode: InputMode { Shell, Agent }` lives IN the model; a `toggle_mode()` mutates only `mode`, provably leaving `rope`/`selection`/`undo` untouched.
- Reserve fields for `preedit: Option<Preedit>` (T-3.2), `overlay: Highlight` and `ghost: Option<GhostText>` (T-3.5) - but those are populated by other tickets.

# Acceptance criteria

- Property test: after any sequence of edits followed by `toggle_mode()`, `text` and `selection` are byte-for-byte identical to before the toggle.
- A paste containing `\n; rm -rf /` inserts as inert literal text (no execution, one undo unit); undo removes it whole.
- Word/home/end/up/down motions with column memory pass a table of cases.
- `submit()`/reset semantics leave the model empty and return the prior text.
- 100% pure: no I/O, runs on any platform; `cargo test` green with no window.

# Out of scope

- IME preedit (T-3.2), routing/hotkey wiring (T-3.3), highlight/ghost (T-3.5), the widget (T-3.6), history (T-3.7).
