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
