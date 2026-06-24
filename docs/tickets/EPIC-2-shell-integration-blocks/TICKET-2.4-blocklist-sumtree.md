---
id: T-2.4
epic: EPIC-2-shell-integration-blocks
title: BlockList + SumTree height index + immutable snapshots
status: ready-for-human
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

# Notes

2026-06-24 (agent): Landed the data structure; all four ACs verified headlessly.
Status -> `ready-for-human` for ONE design decision the ticket itself flags (the
Block enum-variant API, see below) - the rest is done.

**What landed (`aterm-core::block`, pure, dep-free):**

- **`HeightIndex`** - a Fenwick (binary-indexed) tree over per-block heights, the
  "SumTree" the timeline virtualizes over. `push`/`set`/`prefix`/`total`/`block_at`/
  `blocks_in_viewport` are all O(log n); `remove` (eviction) rebuilds O(n). Chosen
  over a flat prefix-sum Vec because the running tail block's height changes every
  frame and a flat array is O(n) per change. Verified against a naive reference over
  every row, at 10k-block scale, with zero-height blocks and all viewport edges.
- **`RowSnapshot` + `Block.output: Vec<RowSnapshot>`** - finished blocks own an
  immutable copy of their output rows, so history survives grid reflow/eviction
  (the prototype's key design choice). `BlockList` keeps the `HeightIndex` in step
  (`push`, `set_block_output`); `last_mut` is private so external callers cannot
  desync the index.

**ACs - all verified by tests:** AC1 (an A/B/C/D cycle yields a block with command
text, exit code, cwd, and an immutable output snapshot), AC2 (a finished block's
stored rows are unchanged by later activity/reflow - owned copies), AC3 (O(log n)
viewport over 10k blocks - correctness at scale + a generous timing sanity), AC4
(set/remove keep the index consistent with a naive model). 84 `aterm-core` tests
pass; `fmt`/`clippy`/full-workspace `build`/`test` green.

Adversarial review (2 lenses x skeptic, 14 findings) found **no correctness
defects** - the Fenwick build/query/update/remove were each hand-traced + fuzzed
clean. It surfaced one perf nit (a `usize::BITS` vs `n as u32` width mismatch in the
`block_at` step seed that wasted ~32 loop iterations per query on the 60fps hot
path); fixed.

**Owner-confirm item (the `ready-for-human` reason):** the ticket lists the Block
**enum-variant API** (`RunningBlock` / `CommandBlock { .. }` / `Interactive { .. }`,
plus room for Epic-5 agent-transcript variants) under *"confirm exact API before
coding per the interface-design rule"*. I deliberately did NOT unilaterally redesign
the working `Block` struct into that enum: it is a public data-contract decision
(per CLAUDE.md, confirm before writing), and converting it would ripple through the
T-2.1/T-2.2 segmenter and intersects T-2.5 (the `Interactive`/alt-screen variant) and
Epic-5 (agent variants). All four ACs are met on the existing struct (which already
models a command block: command/exit/cwd/finished + the new immutable snapshot). The
enum reshaping wants an owner call on the variant set + field names, then an agent
can implement it - ideally coordinated with T-2.5 and Epic-5 so the variant set is
designed once. No CHANGELOG entry: internal data structure, no user-visible change.
