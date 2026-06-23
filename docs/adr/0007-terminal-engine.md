# ADR-0007: Terminal engine - alacritty_terminal 0.26 + portable-pty + immutable per-block snapshots

## Status

Accepted

## Context

aterm renders its own block-based timeline but must still faithfully emulate VT/ANSI inside
each block (vim, ssh, `git log`, npm progress bars all emit real escapes). The dossier
([03-pty-vt-rust.md](../research/03-pty-vt-rust.md)) evaluated building a grid from scratch
on bare `vte` versus reusing a published emulator core, and evaluated PTY-driving options. It
also documented `alacritty_terminal`'s known reflow sharp edges and the Warp-style block
model that sits *on top of* the grid.

## Decision

- **VT/grid engine: `alacritty_terminal = "0.26"`** (Apache-2.0) - the published crate, NOT
  Zed's git fork. It is the most battle-tested reusable terminal core in Rust and bundles the
  hard parts: the Williams-state-machine parser (`vte 0.15`), a reflowing scrollback
  `Grid<Cell>`, alternate-screen handling, line-level damage tracking, and a
  `RenderableContent` cell iterator the renderer walks each frame. We use only its
  `Term`/`Grid`/parser layer.
- **PTY: `portable-pty = "0.9"`** (MIT) instead of alacritty's bundled `tty` module - so we
  own spawn/resize/signal/process-group semantics and keep the Linux/Windows ConPTY door open
  ([ADR-0001](0001-language-and-platform.md)). We feed its reader's bytes into alacritty's
  parser ourselves. The login shell is launched with our ZDOTDIR/ENV/XDG shim env vars
  ([ADR-0008](0008-shell-integration.md)).
- **Block model: a Warp-style `BlockList` layered on top of the grid, not inside it.** OSC-133
  A->B->C->D drives the block lifecycle; OSC 7 reports cwd. We intercept these marks ourselves
  (alacritty does not parse OSC 133 - its issue #5850 is open; see
  [ADR-0008](0008-shell-integration.md)).
- **Finished blocks store immutable per-block row snapshots.** On command finish, output rows
  are snapshotted into an immutable `CommandBlock { output: Vec<RowRun>, exit_code, cwd,
  cmdline, ts }`, keyed to block-relative y. History is therefore immune to later grid
  reflow/eviction - this dodges alacritty's known reflow sharp edges (content jumbling,
  scrollback-line loss, slow reflow on large windows). Only the *live* grid goes through
  alacritty reflow on resize; history is re-wrapped from our own stored snapshots. A `SumTree`
  height index gives O(log n) viewport queries.
- **Alt-screen passthrough.** Full-screen apps live outside the block list. On `?1049h`
  (detected via `TermMode::ALT_SCREEN`) the UI renders the alt grid as one full-window surface
  with keyboard/mouse/scroll passed straight through to the PTY; OSC-133 marks emitted while
  alt-screen is active are suppressed (decided at fire time). On exit the command becomes a
  compact `Interactive` block ("ran vim - 12s") with no captured output.
- Wire `Event::PtyWrite` (DA/DSR/cursor-position query replies) back to the PTY master writer.
- Track the foreground process-group id (via the master fd / procinfo) so Ctrl-C and
  agent-cancel hit the right process.

## Consequences

- We inherit a correct, years-hardened VT grid (the same core Zed's terminal uses) and own
  everything above it (the block list, the chrome, the timeline) - exactly the
  controlled-UI-not-a-real-terminal shape aterm wants.
- Immutable per-block snapshots make history correctness independent of alacritty's reflow
  bugs; the live grid still uses alacritty reflow on resize, which needs an early perf check on
  a maximized 4K window.
- `alacritty_terminal` is pre-1.0 (0.26) with breaking minors and known live-grid reflow
  issues. Mitigation: pin exactly; re-verify the `Handler`/`Event` signatures and the
  `vte::ansi::Processor` re-export path against the pinned version before coding.
- `portable-pty` keeps the cross-platform door open at the cost of not using alacritty's own
  tty module; its fd exposure for pgid/signals must be re-verified in code (likely
  `as_raw_fd` + `killpg`).
- The OSC-133 mark filter is load-bearing (alacritty won't parse 133) and must be exactly
  correct against real shells, including split sequences and BEL-vs-ST terminators.

## Alternatives considered

- **`wezterm-term`.** The closest semantic match (richest escape coverage), but deliberately
  unpublished with no API stability guarantee - depending on it means vendoring a moving fork.
  Rejected as a v1 dependency; remains the fallback if alacritty's grid proves too limiting.
- **`termwiz`.** Published, but positioned as a toolkit for *building* emulators, not a
  ready `Term` you feed bytes and read a grid from - we'd reassemble the grid/scrollback/alt-
  screen glue ourselves. Rejected.
- **A custom grid on bare `vte`.** The purist answer for a controlled UI, but it means
  re-debugging ~9k LOC of escape handling under a 60fps-first, ship-it mandate. Rejected for
  v1; revisit only if alacritty's grid blocks a specific UI requirement.
- **Zed's `alacritty_terminal` git fork.** Rejected: depend on the published crate directly to
  avoid tracking someone else's fork.
- **`pty-process` (async, Unix-only).** Rejected for v1: it forecloses the Windows door the
  owner requires kept open ([ADR-0001](0001-language-and-platform.md)); the blocking
  `portable-pty` reader also gives the simplest backpressure story.
