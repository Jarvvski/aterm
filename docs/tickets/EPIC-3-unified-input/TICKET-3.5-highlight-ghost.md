---
id: T-3.5
epic: EPIC-3-unified-input
title: Async/debounced highlight + ghost text overlay
status: done
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

# Notes (landed 2026-07-01)

The remaining half landed: the off-thread debounced worker, the host wiring, and the
`Right`/`End` accept binding. All ACs met. (The render itself was already there - the T-3.6
input widget renders whatever `highlight`/`ghost_tail`/`preedit` the model carries - so this
was about computing the overlay off-thread and feeding it in, plus wiring the accept keys and
history.)

- **The async worker (`aterm-ui::overlay`).** A dedicated `OverlayWorker` thread with an
  unbounded request channel + a result channel. `request()` (the only keystroke-path call) is
  a single non-blocking send; `poll()` drains the latest result. The worker debounces with
  `recv_timeout(DEFAULT_DEBOUNCE=90ms)`, coalescing a burst to the newest request; an
  `immediate` request (space / paste / mode toggle / IME commit / ghost accept) short-circuits
  the wait (AC1). It computes via the pure `highlight_for` / `ghost_for` and posts the result;
  `Drop` disconnects the sender then joins (no deadlock, no hang). Seven `overlay::tests`
  including a 10k-burst non-blocking assertion (AC3) and the immediate-mid-debounce branch.
- **Host wiring (`aterm-app::Session`).** New `UiCallbacks::tick` (called at the top of every
  wake) drains the worker and applies the freshest result to the `InputModel`
  (`set_highlight`/`set_ghost`), so the render path only ever reads the last-good overlay and
  never blocks (AC3). An edit fires `request_overlay` ONLY when the buffer text actually
  changed (a pure caret motion does not, so a held arrow can't reset the debounce), with
  `immediate` chosen by `edit_is_immediate`. A mode toggle fires an immediate recompute (AC4);
  the worker's `highlight_for` drops shell spans + ghost in Agent mode, and `take()` clears the
  overlay on submit, so the switch re-styles with no stale flash and no text flicker.
- **Ghost + history (AC2).** Submitted lines are pushed into the shared `HistoryRing` (T-3.7),
  tagged with the submit mode (Opt-Enter-from-Shell is tagged Agent), via `Arc::make_mut`
  copy-on-write; the worker draws suggestions from an `Arc` snapshot under the mode's
  `HistoryScope` lens. `Right`/`End` at end-of-line accept the ghost (`accept_ghost`), else
  they are plain motions. `ghost_tail` is the single source of truth for "is a suggestion
  live" - it now gates on collapsed-selection + caret-at-end + strict-prefix, so a suggestion
  is only ever SHOWN where it can be ACCEPTED (display and acceptance can't disagree).
- **AC5 (non-inheritable spans)** was already met by the pure `highlight_command_line` (T-3.5
  first half) and is unchanged.
- **Adversarial review fixes folded in (this commit).** A multi-lens review flagged and this
  fixes: (1) held caret-motion keys resetting the debounce and starving a pending recompute -
  fixed by gating `request_overlay` on an actual text change (`EditOutcome.text_changed`);
  (2) a ghost shown mid-line where it could not be accepted - fixed by tightening
  `ghost_tail`'s visibility to match `accept_ghost`; (3) IME compose/commit slipping past the
  approval-park keyboard lock - `on_ime` now ignores buffer-mutating IME events while a turn is
  parked on an approval (a `Disabled` still passes to clear a dangling preedit); plus the two
  missing worker tests above. (The companion review fix to T-3.2 - clearing the preedit on
  window blur, since macOS emits no `Ime::Disabled` on focus loss - landed in the T-3.2 commit.)
- **Residual:** the toggle-recompute wiring (the trivial `request_overlay(true)` in the toggle
  arm) is not covered end-to-end because `Session` needs a live `Engine`/PTY to construct (no
  headless harness); its component behaviors ARE tested (`highlight_for_is_mode_aware`,
  `agent_mode_has_no_highlight_or_ghost`, `immediate_request_short_circuits_a_long_debounce`).
  `mise run fmt && lint && build && test` green at `-D warnings` (the sole red is the
  pre-existing `flood_publishes_track_ticks_not_bytes` load-flake, which passes in isolation and
  is untouched here). No version bump (accumulating under Unreleased).
