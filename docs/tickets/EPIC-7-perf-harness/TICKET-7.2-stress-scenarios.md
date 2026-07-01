---
id: T-7.2
epic: EPIC-7-perf-harness
title: Seven scripted stress scenarios + driver
status: done
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

# Resolution

**2026-07-01 (agent): Done.** Split the way T-7.1 was - a PURE, headless-tested
scenario+gate+JSON model, plus an on-hardware live driver, plus the nightly job.
Landed in four commits (plus an adversarial-review fix commit).

- **Pure core** (`aterm-bench::scenario`): `ScenarioKind` (the seven exact
  `domain.md` names), `Scenario` (each a deterministic input program - an optional
  output `Generator` [`seq`/`yes`/an alt-screen repaint loop] + a timed
  synthetic-input `DriverAction` script + warmup/measure windows + engine
  scrollback), the `Gate` (the blocking 60fps floor: `present` p50<=16.67 /
  p99<=16.0 / max<=16.0 / 0 dropped, with per-scenario builders for the flood/resize
  drop budget, the resize one-frame spike allowance, the idle frames budget, and the
  flood render/byte decoupling), `Gate::evaluate -> Verdict {Pass, Fail{breaches},
  Inconclusive}`, the informational non-blocking `Target120`, and
  `ScenarioReport`/`RunReport` -> JSON. **16 unit tests**, no window/GPU (runs on the
  Linux CI too).
- **AC1 (all seven run + JSON + verdict):** the `scenario_driver` bin runs the real
  `aterm-ui` app loop (`run_with_recorder`) with the T-7.1 recorder installed,
  advances a per-scenario state machine from `tick()`, spawns the generators, replays
  synthetic input (typing / scroll / resize via the real window / streamed agent
  tokens), buckets recorded frames, evaluates the gate, and dumps the JSON `RunReport`
  + a pass/fail exit code. Smoke-verified end-to-end locally.
- **AC2 (gates enforced; p99>16ms fails):** the pure gate, exhaustively tested
  (`p99_over_the_floor_fails`, max/dropped/idle cases).
- **AC3 (idle ~0 frames after keep-warm):** the `idle` gate + the driver's idle phase
  (warmup elapses keep-warm, then `wants_redraw=false` lets the app idle so `on_frame`
  collects ~0).
- **AC4 (output_flood render decoupled from byte-rate):** decoupling now has two
  FALSIFIABLE halves - a sustained byte firehose (`bytes_fed >= MIN_FLOOD_BYTES`; a
  render coupled into the drain path collapses it) AND vsync-paced frames (the frame
  ceiling; catches a regression that drops `Fifo`). (The first cut was a frame-count
  ceiling alone, which is a tautology under `Fifo` - fixed after the review.)
- **AC5 (nightly CI, blocks on breach):** `.github/workflows/nightly-perf.yml` -
  `schedule` cron + `workflow_dispatch`, `--gate` (exits non-zero on a floor breach),
  uploads the JSON artifact always.
- **Real scroll input** (owner chose "full fidelity"): the `fast_scroll` /
  `large_scrollback` scenarios drive a genuine user scroll path - a new pure
  `ScrollState` follow-bottom scroll-lock in `aterm-ui`, wired to `MouseWheel` +
  `PageUp`/`PageDown` (and a driver-facing `ScrollCommand` seam). This wired the
  wheel/key bindings `timeline.rs` had deferred to EPIC-3.
- **New `UiCallbacks` seams** (all inert by default, zero-cost for a normal host):
  `poll_scroll`, `wants_redraw`, `on_frame` (only under a recorder - the T-7.1
  zero-overhead path is untouched), `should_exit`; plus `run_with_recorder`.
- **Adversarial review (4 lenses, skeptic-verified) found 5 real defects, all fixed
  before finalizing:** the unfalsifiable flood decoupling gate (AC4); the
  window_resize p99==max collapse at <100 frames (the allowed spike tripped the tight
  p99 - fixed by sizing the window to >=100 frames); the count-bounded (not
  duration-bounded) TUI-redraw source; and a measure-boundary action drop.

**HONEST CAVEAT (owner resolved open-question #2 as "GitHub runners, not
self-hosted"):** a GitHub-hosted `macos-14` runner is Apple Silicon but is NOT
ProMotion and is timing-noisy (§9 warns shared macOS runners cannot gate frame time
precisely). So the nightly gates the **60fps floor** as a smoke/regression signal;
120fps is tracked (`target_120hz`), not blocked; and a genuine 120Hz confirmation is
a manual on-hardware `--display-link` run. Where a runner cannot present at all
(headless), the driver reports every scenario `Inconclusive` and exits 0 - never a
false pass or a false failure.

The live present cadence / 120Hz hold on real ProMotion hardware remains the
on-hardware confirmation this driver enables but does not itself prove in CI
(consistent with T-1.5 / T-7.1). `window_resize` deep reflow correctness feeds T-7.4;
input-latency is T-7.3.
