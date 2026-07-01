---
id: T-10.3
epic: EPIC-10-sessions-sidebar
title: Session keybindings + switching/focus routing
status: ready-for-agent
labels: [app, sessions, input]
depends_on: [T-10.1, T-10.2]
---

# Goal

Wire the session intents to real behavior: `⌘T` opens a new session, clicking a row
switches to it, the `✕` (or a close keybinding) closes it, and input/focus follows
the active session so keystrokes and submitted lines always reach the session the
user is looking at. Closing the active session selects a sensible neighbor.

# Context

- North star: [ADR-0011](../../adr/0011-vision-mock-ui-north-star.md). The mock's
  sidebar footer advertises `⌘T new session` (and `⌘I` mode / `⌘L` theme, owned
  elsewhere): [`AtermWindow.dc.html`](../../design/vision-mock/AtermWindow.dc.html).
- Depends on the `SessionList` ([T-10.1](TICKET-10.1-session-model.md)) and the
  sidebar intents ([T-10.2](TICKET-10.2-sessions-sidebar-ui.md)). Routing lives in
  `aterm-app` alongside the unified-input routing brain
  ([T-3.3](../EPIC-3-unified-input/)); reuse that keymap seam, do not fork a second.
- Domain: `Routing` (`docs/agents/domain.md`) - where a submitted line goes; here it
  must also resolve *which session* it targets (the active one).

# Implementation notes

- Crate: `aterm-app`. Add the session keymap to the existing keymap/routing layer:
  `⌘T` -> `SessionList::new` + make it active; row-click -> `set_active`; `✕` and a
  close keybinding -> `close(id)`. Keep `⌘I` (mode toggle, T-3.3) and `⌘L` (theme)
  untouched; only add session bindings, and avoid collisions.
- **Focus routing**: the active session is the single input/keystroke target. On
  switch, the input box (its `InputModel` text + selection + mode, per ADR-0004) and
  the completion/history state rebind to the newly active session; typed-but-unsent
  text policy on switch (per-session draft vs shared) is a small decision - default
  to per-session draft and record it in `# Notes` if you deviate.
- **Close semantics**: closing the active session selects the nearest neighbor
  (prefer the previous, else the next); closing the last session's behavior
  (empty-state vs quit) is a product call - default to keeping one empty session and
  showing the launch state (T-9.5), and flag if that seems wrong.
- Guard against races with T-10.1's lifecycle (never leave active pointing at a
  closed session).

# Acceptance criteria

- [ ] `⌘T` creates and activates a new session; row-click switches; `✕`/close
  keybinding closes; all drive the T-10.1 `SessionList`.
- [ ] Keystrokes and submitted lines always reach the active session; after a
  switch, input/history/completion state reflects the newly active session.
- [ ] Closing the active session selects a valid neighbor and never leaves an
  invalid active id; closing the last session lands on the documented default.
- [ ] Session bindings do not collide with existing keymaps (`⌘I`, `⌘L`, routing);
  covered by tests. `mise run build && mise run test` pass.

# Out of scope

- The session engine ([T-10.1](TICKET-10.1-session-model.md)) and the sidebar/
  title-bar rendering ([T-10.2](TICKET-10.2-sessions-sidebar-ui.md)).
- The mode toggle (`⌘I`, T-3.3) and theme toggle (`⌘L`) themselves.
