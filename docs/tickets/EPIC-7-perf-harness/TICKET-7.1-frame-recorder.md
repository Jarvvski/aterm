---
id: T-7.1
epic: EPIC-7-perf-harness
title: In-process frame recorder
status: done
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

# Resolution

**2026-06-25 (agent): Done.** Built greenfield (no prior scaffold existed). The
recorder instrument + analysis is a pure, headless-tested module; the present-loop
hook is opt-in (mirroring the T-1.5 `DisplayLink` precedent), with the
scenario-driven live run deferred to T-7.2 (out of scope).

- New `aterm-ui::recorder` (pure, clock-injected like `present`): `FrameTiming`
  (the per-frame inputs), `FrameSample` (the stored record + derived
  `frame_dropped`), `Refresh` (Hz60/Hz120 -> deadline), `FrameRecorder` (a
  fixed-capacity ring, pre-sized at construction so `record` is index-write only),
  `FrameStats::from_samples` (p50/p99/max + dropped count/pct, nearest-rank), and
  `to_json`. 11 unit tests.
- **AC1 (all fields, no per-frame alloc):** `FrameSample` carries cpu/gpu/present-
  interval/dirty-cells/allocations/frame_dropped; `record_is_allocation_free_on_the_hot_path`
  asserts 0 allocations via the crate alloc probe over a warmed ring.
- **AC2 (valid JSON + p50/p99/max/dropped analysis):** `to_json` +
  `FrameStats`; `json_dump_is_valid_and_round_trips` proves the dump re-parses to
  identical samples and analyzes identically.
- **AC3 (present_interval matches refresh):** the interval + drop derivation is
  tested headless with synthetic intervals (`frame_dropped_is_derived...`,
  `stats_compute_percentiles_max_and_dropped`). The opt-in present-loop hook
  (`app.rs::redraw`) feeds it live cpu time + vsync deltas; the live
  "matches-the-active-refresh-during-interaction" confirmation is on-hardware via
  the T-7.2 driver (consistent with how the windowed surface / `DisplayLink` are
  validated - app.rs has no headless tests).
- **AC4 (GPU frame time):** documented limitation - the wgpu device requests no
  `TIMESTAMP_QUERY` feature, so `gpu_frame_ms` is `Option` and `None` today; the
  fallback (Instruments / Metal System Trace) and the shape to carry it later are
  noted in the module docs.
- **Adversarial review (3 lenses, skeptic-verified) found 2 real defects, both
  fixed before landing:** (1) `to_json`'s non-finite "guard" was dead code -
  serde_json emits JSON `null` for NaN/Inf (not an error), breaking round-trip;
  `record` now sanitizes non-finite timings to 0.0 (`finite_or_zero`) + a
  regression test + honest doc. (2) `last_present_at` was not reset when the
  scheduler goes idle, so the first frame of a new warm burst counted the whole
  idle gap as one dropped frame (inflating the very metric the recorder produces);
  `RedrawRequested` now clears it on the idle branch. Also hardened: the present
  hook's instrumentation is gated behind `recorder.is_some()`, so the default
  (no-recorder) path is provably zero added work.

74 `aterm-ui` tests green; `mise run fmt && lint && build && test` clean at
`-D warnings`. Deps: `serde`/`serde_json` added to `aterm-ui` from the existing
workspace pins (already in the tree via `aterm-agent`; MIT OR Apache-2.0, no new
download, no new internal-crate edge) - cargo-deny is unaffected (not run locally;
CI runs it). No version bump / CHANGELOG entry: internal opt-in instrumentation, no
user-visible runtime change.
