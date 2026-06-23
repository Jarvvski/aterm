---
id: T-2.4
epic: EPIC-2-shell-integration-blocks
title: BlockList + SumTree height index + immutable snapshots
status: ready-for-agent
labels: [core, block-model]
depends_on: [T-2.1]
---

# Goal

Implement the Warp-style `BlockList` layered on top of the VT grid: typed blocks keyed to commands, each finished block storing an immutable row snapshot, with a `SumTree` height index giving O(log n) viewport queries. This is the data structure the unified timeline and renderer virtualization rely on.

# Context

- Research: [03-pty-vt-rust.md](../../research/03-pty-vt-rust.md) section C and Recommendation 4. Immutable per-block snapshots make history immune to alacritty reflow sharp edges (the prototype's most important design choice).
- ADR / glossary: `CONTEXT.md` block / command block / timeline vocabulary.

# Implementation notes

- Crate: `aterm-core`. Module `block`.
- Types (confirm exact API before coding per the interface-design rule): `Block` enum/variants `RunningBlock` (body = live grid region), `CommandBlock { command, output: Vec<RowRun>, exit_code, cwd, started_at, finished_at }`, `Interactive { label, duration }` (alt-screen apps), and reserve room for agent-transcript block variants (Epic 5 adds them - they are block variants in the same list, wall-clock ordered).
- On `OSC 133;C` open a `RunningBlock`; on `OSC 133;D` snapshot the output rows into an immutable `CommandBlock`. Finished blocks store their own row snapshot keyed to block-relative y so they survive grid reflow/eviction.
- Storage: a `Vec<RowRun>` per finished block for v1 (Warp's FlatStorage packed-bytes optimization is deferred unless huge-log memory bites).
- `SumTree`: balanced tree with per-block pixel-height sums at interior nodes; `blocks_in_viewport(scroll_top, viewport_h) -> range` in O(log n). Virtualize twice: blocks intersecting the viewport, then visible rows within each.
- The block list is a leaf data structure here; the lifecycle state machine that *drives* transitions is T-2.5; rendering is T-2.7.

# Acceptance criteria

- A scripted A/B/C/D cycle produces one `CommandBlock` with correct command text (from `cmdline=`), exit code, cwd, and an immutable output snapshot.
- Reflowing the live grid (resize) does not mutate a finished block's stored rows.
- `SumTree` returns the correct block range for a given scroll offset against a list of 10k blocks in O(log n) (assert via a benchmark/timing sanity check, not a hard gate).
- Inserting/removing blocks keeps the height index consistent.

# Out of scope

- Mark interception (T-2.1) and the lifecycle transitions/alt-screen suppression (T-2.5).
- Rendering/virtualization on the GPU (T-2.7).
- Agent transcript block variants (T-5.10).
