---
id: T-7.3
epic: EPIC-7-perf-harness
title: Input-latency measurement gate
status: ready-for-agent
labels: [bench, perf, input]
depends_on: [T-7.1, T-3.2]
---

# Goal

Measure keystroke->visible-glyph latency separately from frame rate and gate on it, because a fast renderer can still feel laggy (Ghostty's 24ms/41ms data point). Software path for CI, with an optional hardware light-sensor rig for ground truth.

# Context

- Research: [09-performance-60fps.md](../../research/09-performance-60fps.md) section 2.5 + 9 (input latency, separate metric) + Recommendation 7. Owner open-question #3 (hardware rig vs software-only - default software acceptable for v1).

# Implementation notes

- Crate: `aterm-bench`.
- Software path: a Typometer-style screen-capture-on-keypress measure (inject a synthetic keystroke, capture the frame where the glyph appears, compute latency). Report median + p25/p75 + outliers over many iterations (GNOME-46 methodology: ~120 iterations).
- Optional hardware path: a light-sensor/Teensy keyboard-to-photon rig - stub the interface; do not block v1 on it.
- Gate: keystroke->glyph median <= 1.5 frames, p99 <= 3 frames at the active refresh rate.

# Acceptance criteria

- The software measure reports median/p25/p75 over >=100 iterations.
- The gate (median <= 1.5 frames, p99 <= 3 frames) is enforced in the nightly job.
- The hardware-rig interface is stubbed and documented (not required for v1).

# Out of scope

- The frame-time scenarios (T-7.2).
- Building the physical rig.
