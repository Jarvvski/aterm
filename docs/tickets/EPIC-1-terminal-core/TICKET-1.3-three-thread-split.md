---
id: T-1.3
epic: EPIC-1-terminal-core
title: Three-thread reader/model/render split + bounded backpressure
status: ready-for-agent
labels: [core, perf, threading]
depends_on: [T-1.1, T-1.2]
---

# Goal

Stand up the canonical three-thread topology - PTY reader, model (owns `Term`), render - communicating over bounded mailboxes, so a flooding subprocess applies natural backpressure and never stalls the UI.

# Context

- Research: [03-pty-vt-rust.md](../../research/03-pty-vt-rust.md) section E; [09-performance-60fps.md](../../research/09-performance-60fps.md) section 2.4 (thread architecture) and Recommendation 2-3.
- ADR: threads + bounded channels (not async) for PTY I/O.

# Implementation notes

- Crate: `aterm-core` (reader + model threads). The render thread is owned by `aterm-ui` (T-1.5); this ticket defines the snapshot/mailbox contract `aterm-ui` consumes.
- **Reader thread**: owns `master.try_clone_reader()`, loops blocking `read()` into a reusable ~64 KiB buffer (Zellij uses 65536), sends `Bytes` over a *bounded* channel (e.g. `crossbeam-channel`/`std::sync::mpsc::sync_channel` with a small bound). Bounded send = backpressure: reader blocks -> read blocks -> kernel PTY buffer applies flow control. Reader never touches grid or GPU.
- **Model thread**: owns the `Term` (T-1.2). Drains the byte channel, will feed through the OSC filter (T-2.1) then `Processor::advance`. Publishes an immutable snapshot to the renderer via triple-buffer / `arc-swap` / `parking_lot::Mutex<Snapshot>`. Coalescing logic lands in T-1.4; this ticket just establishes the thread + the publish handle.
- Mailboxes: main->model (`resize`, `focus`, `config-change`, `write-input`), model->render (snapshot version bump / dirty signal). Keep messages small and bounded.
- Define the `Snapshot` type (grid rows or a `RenderableContent` capture + damage set + cursor + mode + scrollback offset). Zero per-frame allocation target: reuse snapshot buffers (double/triple-buffer pool), do not allocate a fresh `Vec` per publish.

# Acceptance criteria

- An integration test runs `yes` (or `cat` of a large fixture) through the pipeline for N seconds and asserts: process memory stays bounded (no unbounded queue growth), and the model thread keeps draining.
- Killing the reader/closing the PTY cleanly shuts down all threads (no panic, no hang).
- The snapshot publish is observable from a consumer stub; a test reads two successive snapshots and sees the version monotonically increase.
- No data race under `cargo test` with `--features` for a stress loop; `RUSTFLAGS=-Zsanitizer=thread` optional but document the result if run.

# Out of scope

- Coalescing tick + visible-rate cap (T-1.4).
- The actual GPU render thread + CADisplayLink (T-1.5).
- OSC filtering (T-2.1).

# Notes

2026-06-23 (agent): Landed. New `aterm-core::engine` module stands up the
three-thread topology. **Reader thread**: owns `master.try_clone_reader()`, blocks
`read()` into a reused 64 KiB buffer (`READ_BUF_BYTES`, Zellij's size), and sends
`PtyEvent` chunks over a **bounded** `crossbeam` channel (`READER_QUEUE_DEPTH = 16`
=> <=1 MiB in flight). The bound IS the backpressure: a full channel blocks the
reader's `send`, which blocks `read`, which lets the kernel PTY buffer throttle the
child (ADR-0010). **Model thread**: owns the `Pty` (for resize + child reaping on
drop), the writer, the `Term`, and the `OscScanner`/`BlockSegmenter`/`BlockList`
(relocated from the app's stopgap - the model thread is the rightful owner of the
block model per `domain.md`). It drains bytes in bounded batches (`DRAIN_BATCH`),
scans+segments+feeds, and publishes. The **render thread is T-1.5**; this ticket
ships the contract it consumes.

**Publish contract / mailbox.** Snapshots publish via `Arc<Mutex<Arc<Snapshot>>>`
(the ticket's named "`parking_lot::Mutex<Snapshot>`" option, realized over std - no
new dependency; `Engine::latest_snapshot()` is a cheap `Arc` clone). `Snapshot`
gained a monotonic `version: u64` the model stamps on each publish (the AC's
"successive snapshots see version increase"), plus `Snapshot::empty()` to seed the
handle. The main->model `ToModel` mailbox carries `Resize`/`Input`; it is
*unbounded* deliberately - the render/main thread must never block, and these are
human-rate, not a flood vector (the byte channel is the bounded backpressure path).
Shutdown is a clean cascade: dropping `Engine` drops the mailbox sender ->
model `select!` sees the disconnect and breaks -> drops its `Pty` (kill+reap +
master close) -> reader's blocking `read()` returns EOF -> both threads join in
`Drop`. The reverse (child exits first) flows the same way via `PtyEvent::Exited`.
A `drain_control` pass + `select!` ordering makes shutdown detection and
resize/input servicing starvation-free even under a sustained flood.

Six `#[cfg(all(test, unix))]` integration tests (run on both macOS and Linux CI):
monotonic version (via `cat` echo), flood-keeps-draining-with-bounded-queue (`yes`,
asserts `max_queue_depth <= READER_QUEUE_DEPTH`), both shutdown directions
(child-exit + drop-while-running, the latter watchdogged so a teardown hang fails
rather than hangs CI), and a concurrent-read stress loop (versions non-decreasing,
grid internally consistent). Verified 8/8 repeated runs (no flakiness).

**Adversarial review (one confirmed finding, fixed before landing).** The bounded
*byte* channel capped raw bytes, but the **VT window-event channel**
(`terminal.rs`) was `unbounded()` and the model thread produces into it
synchronously during `feed()` (Bell/DSR/DECRQM/Title) with no drain inside the bare
`Engine` - so a child spamming `\x1b[6n`/`\a` grew it without bound, and the
`yes`/`cat` tests missed it (neither emits control sequences). Fixed: the event
channel is now `bounded(EVENT_CHANNEL_CAP = 1024)` with **drop-on-full** via
`try_send` in `ChannelListener` - a *blocking* send would deadlock the model thread
against itself (it is the synchronous producer AND the only guaranteed drainer);
the forwarded events are latest-wins/coalescable and `PtyWrite` replies degrade
gracefully (T-1.9 owns the reply path). Added a `control_sequence_flood_*` test
(`yes $'\x1b[6n'`) that asserts the event backlog stays in `[1, cap]` with no
drain, closing the AC1 gap. Two findings were refuted and recorded as forward
notes: the reader's `buf[..n].to_vec()` per-chunk allocation is off the
60fps-critical path and a documented **T-1.4** buffer-pool deferral (no AC covers
it); the `max_queue_depth` one-off undercount is a cosmetic observability quirk -
boundedness is guaranteed by the channel *type*, and the regression that matters
(reverting to unbounded) still fails the test loudly.

This replaces the stopgap reader + on-render-thread VT parse that lived in
`aterm-app/src/session.rs`; `Session` is now a thin bridge holding an `Engine`
(snapshot read, input/resize routing, terminal-event draining, `block_count` via
metrics). No version bump / CHANGELOG entry: internal engine wiring, no
user-visible behaviour change. `fmt`/`clippy`/`build`/`test` all green.
