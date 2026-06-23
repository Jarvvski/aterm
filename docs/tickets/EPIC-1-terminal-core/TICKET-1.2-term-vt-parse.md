---
id: T-1.2
epic: EPIC-1-terminal-core
title: alacritty_terminal Term wiring + VT parse loop
status: ready-for-agent
labels: [core, vt]
depends_on: [T-1.1]
---

# Goal

Wire `alacritty_terminal`'s `Term` + `vte::ansi::Processor` into `aterm-core` so bytes from the PTY produce a correct, reflowing grid with alt-screen, damage, and a cheap per-frame `RenderableContent` read path.

# Context

- Research: [03-pty-vt-rust.md](../../research/03-pty-vt-rust.md) section B and Recommendation 1. Note Risk: alacritty_terminal is pre-1.0; pin exactly and re-verify `Handler`/`Event`/`RenderableContent` signatures against the pinned version before coding.
- ADR: terminal-engine choice (alacritty_terminal 0.26 published crate, NOT Zed's fork).

# Implementation notes

- Crate: `aterm-core`. New module `term`.
- Dependency: `alacritty_terminal = "0.26"` (pin exactly; Apache-2.0). Use only its `Term`/`Grid`/`vte` layers; do NOT use its bundled `tty` module (T-1.1 owns the PTY).
- Construct `Term::new(config, &TermSize, event_proxy)`. Implement a small `EventListener` that forwards `Event` variants over a channel to the app (Title/ResetTitle/Bell/ClipboardStore/ClipboardLoad/PtyWrite/Exit/CursorBlinkingChange). PtyWrite handling is wired in T-1.9; here just surface the events.
- Drive parsing with `vte::ansi::Processor::advance(&mut term, &bytes)`. Provide a `feed(&[u8])` entry point on the model side that the coalescer (T-1.4) will call. NOTE: the OSC-133/7 filter (T-2.1) sits *in front of* this `feed`; design `feed` to accept already-filtered bytes plus a side-channel of detected marks - but do not implement the filter here.
- Expose `renderable_content() -> RenderableContent` (display_iter, cursor, display_offset, selection, colors, mode) for the renderer snapshot. Expose `mode()` so alt-screen (`TermMode::ALT_SCREEN`) is queryable.
- Expose `resize(TermSize)` (reflows live grid + scrollback). Default scrollback 10_000; make it config-surfaced.
- Surface `TermDamage` (Full / Partial line bounds) so T-1.8 damage tracking can read it.

# Acceptance criteria

- Feeding a captured byte stream (e.g. `ls --color`, an SGR-heavy fixture, a unicode/CJK fixture, an alt-screen vim redraw) produces the expected cells (assert a few cell chars + fg/bg + flags).
- `mode()` reports `ALT_SCREEN` after `\x1b[?1049h` and clears after `\x1b[?1049l`.
- Resizing the term reflows without panic; a maximized-window (e.g. 200x60) resize completes (perf measured later in T-7.4).
- A test asserts `Event::Title` fires for an OSC 0/2 title sequence.
- Pinned `alacritty_terminal` version's `Handler`/`RenderableContent` signatures are confirmed in a doc comment.

# Out of scope

- OSC-133/7 interception (T-2.1) and the block model (Epic 2).
- Threading/coalescing (T-1.3, T-1.4).
- PtyWrite reply + pgid (T-1.9).
