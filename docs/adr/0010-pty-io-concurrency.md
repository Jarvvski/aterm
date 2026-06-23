---
title: ADR-0010 - PTY I/O concurrency model
status: Accepted
---

# ADR-0010: PTY I/O concurrency model - blocking threads, not async

## Status

Accepted. Ratifies dossier open question #3 ("threads vs tokio for PTY I/O"), which expected this resolved as threads. Owner-confirmable, but effectively forced by ADR-0003's three-thread model and ADR-0007's choice of `portable-pty`.

## Context

The dossier left "threads vs tokio for PTY I/O" as open question #3, noting it should resolve to threads and that it interacts with the runtime/render choice. ADR-0007 locked `portable-pty`, whose reader/writer are blocking `Read`/`Write` handles. The architecture (ADR-0003, `architecture.md` section 2) locked a three-thread model: a PTY reader thread, a model/VT/block thread, and a vsync render thread, communicating over bounded channels. Separately, ADR-0005 runs the agent subsystem (Anthropic Messages-API client, SSE streaming) on a tokio runtime.

## Decision

- **The PTY side uses blocking OS threads, not async.** The PTY reader is a dedicated thread doing a blocking `read()` into a reusable buffer and sending chunks over a **bounded** channel; the bound IS the backpressure (under a `cat`/`yes` flood the reader blocks on send -> blocks read -> the kernel PTY buffer applies flow control). No tokio on the PTY/model/render path; no per-read heap allocation in the hot loop.
- **The agent subsystem keeps its own tokio runtime** (ADR-0005) for HTTP/SSE. The two subsystems are **intentionally not unified** - they have different concurrency shapes (blocking byte streams vs. async network I/O) and live in different crates (`aterm-core` vs `aterm-agent`). Do not "simplify" by forcing the PTY path onto tokio or the agent path onto threads.
- This also keeps the Windows door open: `portable-pty` is cross-platform and the blocking-thread model is the portable one.

## Consequences

- A deterministic, allocation-free hot path on the 60fps-critical side; backpressure is structural, not bolted on.
- Two concurrency models coexist in one binary. Acceptable and documented; the boundary is the `aterm-core` <-> `aterm-agent` crate edge.
- T-1.1's earlier "flag if no ADR exists" note is resolved by this ADR.

## Alternatives considered

- **tokio for everything** (PTY reads via `tokio::io` on the master fd): unifies the runtime but adds async overhead on the latency-critical PTY path, complicates the render-thread handoff, and `portable-pty`'s blocking handles do not fit cleanly. Rejected.
- **`pty-process`** (async, Unix-only): would couple PTY I/O to tokio and close the Windows door. Rejected in ADR-0007.
- **A single thread polling PTY + rendering**: cannot hold the 60fps floor under output floods. Rejected (the three-thread split is ADR-0003).
