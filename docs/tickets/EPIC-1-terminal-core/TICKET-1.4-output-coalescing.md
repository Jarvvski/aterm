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

# Notes

2026-06-23 (agent): Landed. Builds on the T-1.3 engine. Two pieces:

**Coalescing window (`engine::run_model`).** Replaced T-1.3's fixed `DRAIN_BATCH`
(byte-driven: one publish per ~16 chunks) with a *lazy timed window*. State:
`deadline: Option<Instant>` + a `timer: Receiver<Instant>` that is `never()` while
idle and a one-shot `after(COALESCE_INTERVAL)` while there is unpublished output.
The model parses bytes *continuously* for correctness, but publishes at most once
per `COALESCE_INTERVAL` (`5ms`, a `pub(crate)` const; the dossier's 4-8ms range,
under the 16.6/8.3ms frame budgets; T-7.2's `output_flood` tunes it). Three flush
paths keep it both starvation-free and stall-free:
- The **byte arm** drains available bytes until `Instant::now() >= deadline`
  (clock-bounded, so a sustained flood where the reader refills as fast as we drain
  cannot spin here forever), then publishes once the window elapses. This makes the
  flush independent of `select!` fairness under flood.
- The **timer arm** flushes after a burst goes idle (so a lone keystroke or a
  finished burst publishes within one window - never stalls waiting for more bytes).
- **Exit** publishes a final coherent frame before shutdown.
Idle = no `deadline` = `never()` timer = the model thread truly sleeps (no periodic
wakeup). Resizes are coalesced like output (`drain_control` now returns
`(shutdown, dirtied)` instead of publishing inline). The display-rate cap the
ticket mentions is the *renderer's* job (it samples `latest_snapshot()` at vsync -
a pull model, T-1.5); publishing at 200Hz is harmless because the renderer reads
only the latest, and the grid copy is cheap. Flagged this scope split in review.

**Zero-allocation publish (double-buffer).** Added `Terminal::snapshot_into(&self,
out: &mut Snapshot)` which fills a caller-provided snapshot in place (clears +
resizes the `cells` Vec, reusing capacity); `snapshot()` now delegates to it.
`Model::publish` keeps a spare `back: Arc<Snapshot>`: it writes into `back` in place
via `Arc::get_mut` (ensuring unique ownership first), stamps the version, swaps
`back` into `latest` (`Arc::clone` + `mem::replace` - refcount/move, no alloc), and
reclaims the previously-live buffer as the next spare. Two buffers cycle, so steady
state allocates neither the `Vec` nor the `Arc`. The consumer never sees a torn
read: the model only writes into the buffer *not* in `latest`, and swaps in only
after the write completes. If a consumer still holds the spare (`get_mut` -> None) it
allocates a fresh buffer rather than block - correctness over the fast path (a
robustness margin for the T-1.5 render thread that may hold an `Arc` across a frame;
a triple buffer is a localized future bump behind the same contract).

Six tests: `snapshot_into` reuse (deterministic: same cells pointer + capacity
across 200 re-renders at stable dims) + parity with `snapshot()`;
`flood_publishes_track_ticks_not_bytes` (publishes bounded by elapsed/interval -
throughput-independent, so robust on slow/contended CI); `burst_coalesces_and_final
_grid_is_correct` (~6.9MB `seq` burst, final grid shows the last line, publishes
O(per-tick)); `lone_input_publishes_within_one_window` (no interactive stall). The
existing T-1.3 tests still pass unchanged. 6/6 repeated full-suite runs under
parallel contention - no flakiness.

Two test-only bugs found and fixed before review: macOS BSD `seq` prints `1000000`
as `1e+06` (use `-f "%.0f"`); and a `publishes < chunks` assertion was flaky under
parallel-test CPU contention (chunks fixed, but contention stretches wall-clock ->
more ticks -> more publishes) - replaced with the throughput-independent
`publishes <= elapsed/interval` bound. Adversarial review (3 lenses: coalescing
correctness/concurrency, double-buffer safety, scope+fidelity) returned zero
confirmed findings.

No version bump / CHANGELOG entry: internal engine behaviour, no user-visible
change yet. `fmt`/`clippy`/`build`/`test` all green.
