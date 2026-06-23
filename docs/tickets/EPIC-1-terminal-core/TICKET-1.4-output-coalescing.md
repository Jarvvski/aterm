---
id: T-1.4
epic: EPIC-1-terminal-core
title: Output coalescing + grid snapshot publication
status: ready-for-agent
labels: [core, perf]
depends_on: [T-1.3]
---

# Goal

Coalesce PTY byte bursts on a short tick so a megabyte flood becomes one parse pass + one publish, not thousands - decoupling byte-rate from frame-rate and protecting the 60fps floor.

# Context

- Research: [03-pty-vt-rust.md](../../research/03-pty-vt-rust.md) section E (the documented GPUI `cat`-flood freeze fixed by a 4ms batching interval) and Recommendation 3; [09-performance-60fps.md](../../research/09-performance-60fps.md) section 5 (PTY backpressure, sample grid at most once per vsync).

# Implementation notes

- Crate: `aterm-core`, model thread (T-1.3).
- On the model thread, merge everything available within a coalescing window (~4-8ms, comfortably under 16.6ms/8.3ms budgets) before publishing a snapshot. Parse continuously for correctness; publish at most once per tick.
- The coalesce interval is a tuned heuristic (4-8ms starting point per the dossier) - make it a named constant, document that T-7.2's `output_flood` scenario tunes it.
- Visible-rate guard under sustained flood: parse all bytes (grid stays correct) but throttle snapshot publication to the display rate so the renderer never sees more than one coherent state per vsync.
- Reuse snapshot buffers (no per-publish allocation; ties to T-1.3 pool).

# Acceptance criteria

- A test feeding a 5 MB fixture in one burst results in O(1)-ish publishes per tick (assert publish count << byte-chunk count), and the final grid state is correct.
- `cat` of a large file does not produce a publish storm: instrument publish count over a fixed wall-clock and assert it tracks ticks, not bytes.
- Steady-state typing (one byte at a time with idle gaps) still publishes promptly (latency within one tick), i.e. coalescing does not add perceptible lag for interactive input.

# Out of scope

- The render-side present pacing (T-1.5).
- Frame-time measurement (T-1.8, T-7.1).
