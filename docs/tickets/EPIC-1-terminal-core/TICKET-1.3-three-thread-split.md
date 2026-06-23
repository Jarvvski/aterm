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
