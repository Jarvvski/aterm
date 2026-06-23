---
id: T-7.1
epic: EPIC-7-perf-harness
title: In-process frame recorder
status: ready-for-agent
labels: [bench, perf, ci]
depends_on: [T-1.5, T-1.8]
---

# Goal

Build the Tier-2 in-process frame recorder: a ring buffer capturing per-frame CPU frame time, GPU frame time, present interval, dirty-cell count, allocations (debug), and a frame-dropped flag - the instrumentation the "60fps always" proof reads.

# Context

- Research: [09-performance-60fps.md](../../research/09-performance-60fps.md) section 9 (Tier 2: in-process frame recorder) + Recommendation 7. The hard gate is the 60fps floor (16ms); 120fps is tracked, not blocking (owner open-question #4).

# Implementation notes

- Crate: `aterm-ui` (recorder hooks) + `aterm-bench` (analysis/driver).
- Per frame, record: `cpu_frame_ms` (snapshot->encode), `gpu_frame_ms` (Metal GPU timestamps / MTLCommandBuffer start-end - may require the `metal` crate or Instruments if wgpu timestamps are insufficient; document), `present_interval_ms` (delta between CADisplayLink timestamps), `dirty_cells`, `allocations` (debug build), `frame_dropped` (present_interval exceeded deadline + tolerance).
- Ring buffer, zero-allocation recording on the hot path. Dump to JSON for offline percentile/histogram analysis.
- Confirm wgpu Metal GPU-timestamp support; if absent, fall back to the `metal` crate path or Instruments and note it.

# Acceptance criteria

- The recorder captures all fields per frame with no per-frame allocation in the recording path.
- A run dumps valid JSON consumable by an analysis script computing p50/p99/max/dropped.
- `present_interval_ms` matches the active refresh (8.33ms @120Hz / 16.67ms @60Hz) during interaction.
- GPU frame time is captured (or the limitation is documented with the fallback).

# Out of scope

- The scenarios + driver (T-7.2), input latency (T-7.3).
- The self-hosted runner infra (owner decision; flagged).
