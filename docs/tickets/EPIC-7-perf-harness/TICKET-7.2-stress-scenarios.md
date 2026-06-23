---
id: T-7.2
epic: EPIC-7-perf-harness
title: Seven scripted stress scenarios + driver
status: ready-for-agent
labels: [bench, perf, ci]
depends_on: [T-7.1]
---

# Goal

Implement the seven named, scripted stress scenarios and a driver that feeds deterministic PTY input + synthetic input events, with the p50/p99/max/dropped-frame pass/fail gates - the standing "60fps always" proof. Run nightly on a self-hosted Apple Silicon ProMotion runner.

# Context

- Research: [09-performance-60fps.md](../../research/09-performance-60fps.md) section 9 (Tier 2 scenarios table + pass/fail criteria) + Recommendation 7, 9. Owner open-question #2 (self-hosted runner), #5 (adversarial policy: gate real-world, track adversarial non-blocking).

# Implementation notes

- Crate: `aterm-bench`. Driver feeds deterministic input and dumps the recorder (T-7.1) to JSON.
- Scenarios: `fast_scroll` (page-down through 100k lines), `output_flood` (`cat` 200MB / `yes` 10s - also tunes the T-1.4 coalesce interval), `large_scrollback` (1M-line ring, jump to top), `agent_stream_while_typing` (inject streamed tokens while replaying keystrokes), `window_resize` (animate 800->1600px over 1s, the presentsWithTransaction path), `fullscreen_tui_redraw` (full repaints), `idle` (5s idle, assert ~0 frames drawn).
- Gates (steady-state, per scenario, Apple Silicon ProMotion): p50 <= 8.33/16.67ms; p99 <= 8.0ms (120Hz target) / hard floor <= 16.0ms (60Hz never exceed); max <= 16.0ms non-resize (resize allowed a one-frame transaction spike); dropped frames 0 in steady state (flood/resize a bounded declared budget e.g. <=2 during resize); idle CPU ~0 / frames ~0 after keep-warm.
- Gate hard on the 60fps floor; track 120fps as non-blocking. Gate on real-world; track adversarial (every-cell-unique-style) non-blocking.

# Acceptance criteria

- All seven scenarios run via the driver and emit JSON + a pass/fail verdict.
- The gates are enforced: a scenario breaching p99 <= 16.0ms fails.
- `idle` asserts ~0 frames drawn after the keep-warm window.
- `output_flood` shows render decoupled from byte-rate (frames track vsync, not bytes).
- A nightly CI job runs these on the self-hosted runner and blocks release on a breach.

# Out of scope

- Input-latency gate (T-7.3).
- Resize-reflow correctness + shell matrix (T-7.4).
- Provisioning the runner (infra/owner).
