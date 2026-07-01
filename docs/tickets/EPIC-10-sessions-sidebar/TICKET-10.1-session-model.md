---
id: T-10.1
epic: EPIC-10-sessions-sidebar
title: Multi-session model - concurrent PTY-backed sessions + SessionList
status: ready-for-agent
labels: [core, sessions]
depends_on: []
---

# Goal

Support multiple concurrent terminal sessions, each owning its own PTY, VT engine,
and block timeline, behind a `SessionList` that tracks the active session and its
lifecycle (create / close / switch). The render thread reads the active session's
snapshot; background sessions keep running. This is the data foundation the
sidebar (T-10.2) presents; it renders nothing itself.

# Context

- North star: [ADR-0011](../../adr/0011-vision-mock-ui-north-star.md) sanctions the
  sessions surface (the "no sidebar by default" clause of `design-system.md` §1 is
  retired). Visual source of the affordance:
  [`AtermWindow.dc.html`](../../design/vision-mock/AtermWindow.dc.html) `<!-- sidebar -->`
  (the mock carries a `sessions` list with a name + running state + active flag).
- The single-session engine this generalizes: PTY spawn/resize/signals
  ([T-1.1](../EPIC-1-terminal-core/)), the `Term`/VT loop (T-1.2), the three-thread
  reader/model/render split + bounded backpressure (T-1.3, see ADR-0010), and the
  `BlockList` + immutable snapshots (T-2.4). Domain: `Grid`, `BlockList`,
  `Timeline` (`docs/agents/domain.md`).

# Implementation notes

- Crate: `aterm-core` (the session engine) wired by `aterm-app` (ownership +
  active-session selection). Do NOT introduce any cross-crate edge that violates
  the one-way arrow (ADR-0003); a `Session` is a `core` construct.
- Model a `Session` as the unit that today is implicitly "the terminal": its PTY
  handle + reader thread, its `Term`/grid, its model thread, its `BlockList`, its
  cwd/integration state, and a `name`. A `SessionList` holds an ordered set of
  sessions + the active id, with `new()`, `close(id)`, `set_active(id)`, and a
  stable id allocator (never reuse an id).
- **Threading**: each session keeps the locked three-thread shape (reader / model
  / render-facing snapshot). Only the ACTIVE session's snapshot is drawn; inactive
  sessions keep draining their PTY into their own `BlockList` (bounded channels
  give the same backpressure per session). The single render thread swaps which
  session snapshot it reads on `set_active`; it never blocks on a background
  session. Be explicit about the resource ceiling (N reader threads) and note it.
- `close(id)` tears down that session's PTY + threads cleanly; if it was active,
  selection moves to a neighbor (T-10.3 owns the keybinding, this owns the
  invariant that active is always valid while >=1 session exists).
- Naming: derive a default name (e.g. the running command or the shell), leaving
  richer naming to a follow-up; the mock shows names like "main" / "dev server".

# Acceptance criteria

- [ ] `SessionList` supports create / close / switch with a stable, never-reused id
  and an always-valid active id while >=1 session exists; unit-tested.
- [ ] Each session owns an independent PTY + `Term` + `BlockList`; a command in a
  background session advances that session's timeline without being drawn, and
  switching to it shows its accumulated blocks.
- [ ] Per-session bounded backpressure holds (a flood in a background session
  blocks only that session's reader, not the app); covered by a test.
- [ ] No crate-boundary violation (ADR-0003); `mise run build && mise run test`
  pass.

# Out of scope

- The sidebar UI, the title-bar binding, and the `◧` toggle - [T-10.2](TICKET-10.2-sessions-sidebar-ui.md).
- Keybindings (`⌘T`, switch, close) and focus routing - [T-10.3](TICKET-10.3-session-keybindings.md).
- The window-frame/title-bar chrome shell itself - [T-9.2](../EPIC-9-vision-mock-reskin/TICKET-9.2-window-frame-titlebar.md).
