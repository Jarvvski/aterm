---
id: T-1.7
epic: EPIC-1-terminal-core
title: Tier-1 iai-callgrind micro-benches (parse/grid/frame-build)
status: ready-for-agent
labels: [bench, perf, ci]
depends_on: [T-1.4]
---

# Goal

Land the Tier-1 instruction-count benchmark suite in `aterm-bench` that gates every PR on noisy shared CI runners, covering VT parse, grid mutation, damage computation, and the GPU-free frame-build (instance buffer generation).

# Context

- Research: [09-performance-60fps.md](../../research/09-performance-60fps.md) section 9 (Tier 1: iai-callgrind, instruction counts negate runner noise; gate on >N% regression, start 5%). [10-packaging-scaffold.md](../../research/10-packaging-scaffold.md) section (c) CI.

# Implementation notes

- Crate: `aterm-bench` (depends on `aterm-core`, `aterm-ui` for the frame-build portion behind a `bench` feature). `harness = false`.
- Dependency: `iai-callgrind` (pin). Optionally wire Bencher for history.
- Benchmarks: (a) `vte` parse of fixed payloads - plaintext scroll, dense unique-style cells, unicode, SGR-heavy, alt-screen TUI redraw; (b) grid mutation + scroll; (c) damage computation; (d) frame-build CPU work (instance buffer generation for a fixed grid state, no GPU).
- Fixtures: deterministic byte payloads checked into `aterm-bench/fixtures/`.
- CI: a required job running these; fail the PR on >5% instruction-count regression (generous to start, tighten later). This runs on shared GitHub runners (noise-immune).

# Acceptance criteria

- `cargo bench -p aterm-bench` runs all four micro-benches and emits instruction counts.
- A deliberately-slower change (e.g. an extra clone in the hot path) is caught by the regression gate in a dry run.
- The benches run green on a Linux runner (no GPU/window needed for a-d; frame-build uses CPU-only instance generation).

# Out of scope

- Tier-2 frame-level recorder + scenarios (Epic 7).
- Input-latency measurement (T-7.3).
