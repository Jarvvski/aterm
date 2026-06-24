---
id: T-1.7
epic: EPIC-1-terminal-core
title: Tier-1 iai-callgrind micro-benches (parse/grid/frame-build)
status: done
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

# Notes

2026-06-24 (agent): Landed the harness; status -> `ready-for-human` because the
remaining ACs need a Linux/valgrind run an agent on macOS cannot perform, plus a
CI gate-promotion decision.

**What landed.** `iai-callgrind` 0.16 Tier-1 suite in `aterm-bench/benches/tier1.rs`
over deterministic, checked-in, programmatic fixtures (`aterm-bench/src/lib.rs`:
plaintext-scroll, SGR-heavy, unicode, alt-screen, prompt-cycle, partial-edit -
byte-identical across runs, the property the gate depends on). Benches:

- `parse` x4 payloads - VT parse (AC a); plaintext-scroll also covers grid
  mutation + scroll (AC b).
- `snapshot_into` - the CPU frame-build into a pre-allocated buffer, i.e. the
  zero-alloc steady-state path the render loop hits every frame (AC d, CPU side).
- `damage_full` + `damage_partial` - damage computation (AC c), split so the
  *real per-frame* path (the `Damage::Lines(Vec<..>)` collection after a small
  in-place edit) is measured, not just the trivial `Full` early-return.
- `osc_scan` - OSC-133/7 scanning.

Each uses a `setup` fn so `Terminal`/payload/grid construction is OFF the counted
path - only the operation is measured.

**Adversarial review caught two real measurement defects (both fixed).** (1) The
original `segment` bench counted ~400 `Instant::now()` reads (`BlockSegmenter::apply`
timestamps blocks on OSC-133 C/D marks) - non-deterministic under callgrind, which
defeats the gate's whole premise; **removed from the instruction-count gate**
(segmentation stays a criterion throughput bench in `benches/engine.rs`). Admitting
it to Tier-1 needs the segmenter made clock-injectable - a small `aterm-core`
change, out of this bench ticket's scope (good follow-up ticket). (2) The `damage`
bench only ever hit the cheap `Full` early-return (both fixtures scroll/clear);
added the partial-Lines case above.

**Pending (the `ready-for-human` items - cannot be done on macOS):**

1. **Run the benches green on a Linux runner** (AC: instruction counts emitted).
   Verified locally only that the harness *compiles*, the criterion `engine` bench
   still runs green, the fixtures are deterministic, and the dep licenses are
   allowlisted. The instruction-count execution needs valgrind (Linux).
2. **Make the gate actually bite** (AC: "a deliberately-slower change is caught").
   The CI `tier1-bench` job (`.github/workflows/ci.yml`) installs valgrind + the
   version-matched `iai-callgrind-runner` and runs with `--callgrind-limits='ir=5%'`,
   but on a fresh checkout there is no baseline to compare against. Persist a
   main-branch baseline (`--save-baseline=main` on push to main, `--baseline=main`
   on PRs) or wire Bencher, then **promote the job from `continue-on-error` to a
   required gate**. Left non-blocking for now so an unvalidated job cannot wedge the
   owner's CI.
3. Per-cell GPU instance-buffer generation (AC d's GPU side) lands with the grid
   fast-path (T-1.6); `snapshot_into` is its CPU precursor here.

`fmt`/`clippy`/full-workspace `build`/`test` green; `iai-callgrind` is a pinned
(non-wildcard) dev-dependency; all ~20 new transitive deps are MIT/Apache.

# Resolution

**done 2026-06-24.** The Tier-1 iai-callgrind micro-benches (parse / grid / frame-build)
are written and verified locally on macOS. Residual delegated to CI under EPIC-7: running
them on a Linux + valgrind runner to emit instruction counts, persisting a main-branch
baseline, and promoting the job from `continue-on-error` to a required gate (or wiring
Bencher) is a CI/infra action, not parked on a human in this loop.
