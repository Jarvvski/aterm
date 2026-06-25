---
id: T-3.5
epic: EPIC-3-unified-input
title: Async/debounced highlight + ghost text overlay
status: ready-for-agent
labels: [ui, input, perf]
depends_on: [T-3.1]
---

# Goal

Compute syntax highlight, error underlining, and ghost-text suggestions off the main thread, debounced, applied as non-inheritable style overlays - so they never stall the 60fps render loop. The render reads the last-good overlay and never blocks.

# Context

- Research: [05-unified-input-ux.md](../../research/05-unified-input-ux.md) sections 1 (Warp: long debounce + short-circuit on space/paste/selection; inheritable vs non-inheritable styles), 4 (fish-style ghost text) and Recommendations 7-8. No exact Warp debounce ms published - start ~80-150ms idle, tune against the frame budget.

# Implementation notes

- Crate: `aterm-ui` (overlay application) + a worker (off the render thread). The overlay populates `InputModel.overlay`/`ghost` (T-3.1) via channel; the render path reads the last-good snapshot.
- Highlight: a command-line parser (shell syntax) producing non-inheritable style spans (error underline, command/arg/flag tinting). Recompute on `mode` toggle (shell highlight vs agent prose ~none); the recompute is async, text never flickers.
- Debounce idle ~80-150ms; short-circuit on space/paste/selection for instant feedback. All work async; never on the keyDown path.
- Ghost text (Shell mode): fish-style suggestion from history (most-recent prefix match), muted gray tail, accepted with `Right`/`End` at end-of-line (zsh-autosuggestions semantics). Agent mode ghost from prior prompts or off by default (owner open-question #4 - default off).

# Acceptance criteria

- Typing a long command shows error underlining only after the debounce, and instantly on space/paste.
- Ghost text appears from history and is accepted by `Right`/`End` at line end.
- A stress test injecting rapid keystrokes shows the render loop never blocks on highlight/ghost (frame budget held; verify with T-1.8 instrumentation).
- Toggling mode recomputes the overlay without text flicker.
- Highlight spans are non-inheritable (typing after a styled run does not inherit the style).

# Out of scope

- Spec-driven completion menus (deferred; T-8.5 / later).
- The widget rendering (T-3.6).

# Notes

**2026-06-25 (agent): pure compute half landed; ticket stays `ready-for-agent`
(the async worker + overlay render remain).** The headless-testable core - the
highlighter and the ghost logic - is implemented and tested in `aterm-core`; the
aterm-ui async/debounced worker and the overlay render are NOT done, so the two
render-dependent ACs are NOT met yet. Honest AC status:

- **AC5 (non-inheritable spans) - MET.** New `aterm-core::highlight::highlight_command_line`
  tokenizes the line into non-overlapping `StyleSpan`s (`SpanKind` =
  Command/Argument/Flag/QuotedString/Operator/ErrorUnderline) over CHAR offsets:
  first word / post-separator word = Command, `-x`/`--long` = Flag, quoted runs =
  QuotedString (an unterminated quote = ErrorUnderline to end of line), `| & ; < >
  && ||` = Operator (a separator resets the next word to a command; a redirect does
  not). Non-inheritability is structural (recomputed from the whole text) and
  tested. `Highlight` (was an empty placeholder) now carries `Vec<StyleSpan>`.
- **AC2 (ghost from history + accept) - logic MET; key binding deferred.**
  `highlight::ghost_for` returns the most-recent prefix match (Shell only; agent
  ghost defaulted off per owner Q#4) as the FULL suggested line.
  `InputModel::ghost_tail` derives the visible tail live (`suggestion.strip_prefix(text)`);
  `accept_ghost` inserts that live tail at end-of-line as one undo unit. The
  `Right`/`End` binding itself is the T-3.6 widget's job.
- **AC4 (recompute on mode toggle) - logic MET; "without flicker" deferred.**
  `highlight_for(text, mode)` is mode-aware (Agent prose -> empty); the no-flicker
  guarantee is a render property of the worker/overlay (below).
- **AC1 (underline shown after debounce, instant on space/paste) and AC3 (render
  loop never blocks; verify with T-1.8 instrumentation) - NOT met.** These require
  the aterm-ui async worker (debounced ~80-150ms, off the render thread, applying
  via `set_highlight`/`set_ghost` over a channel) and the overlay render reading
  the last-good snapshot. That is the remaining work to close this ticket.

A 3-lens adversarial review found and this fixes a real defect: a debounced worker
can let the buffer advance past a suggestion, so the original tail-only `GhostText`
could append a STALE tail on accept (`git st`+ghost`atus`, type `ash` -> accepting
gave `git stashatus`). Fixed by the full-suggestion model: `GhostText` stores the
whole suggested line and the tail is re-derived live, so a diverged buffer neither
shows nor accepts a stale ghost; `take()` also clears the ghost on submit.
Regression tests cover both. `mise run fmt && lint && build && test` green at
`-D warnings`; `aterm-core` 171 tests. No version bump (internal, no user-visible
surface yet).

**Remaining to reach `done`:** the aterm-ui debounced worker + overlay render
(AC1, AC3) and the `Right`/`End` accept binding in the T-3.6 widget.
