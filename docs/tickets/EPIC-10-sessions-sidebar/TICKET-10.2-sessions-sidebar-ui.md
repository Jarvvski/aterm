---
id: T-10.2
epic: EPIC-10-sessions-sidebar
title: Sessions sidebar + title-bar session binding
status: done
labels: [ui, sessions, chrome]
depends_on: [T-10.1, T-9.1, T-9.2]
---

# Goal

Render the mock's sessions sidebar and bind it to the live `SessionList`: a
toggleable 210px left panel listing sessions (a running/idle status dot, the name,
a hover-revealed close control), an uppercase "SESSIONS" label + a `+` new-session
affordance, the active row marked by an accent-tinted fill and a 2px inset accent
bar, and a footer of shortcut hints. Also bind the custom title bar to the active
session name + cwd, and make the `◧` glyph actually toggle the panel.

# Context

- North star: [ADR-0011](../../adr/0011-vision-mock-ui-north-star.md). Visual
  source: [`AtermWindow.dc.html`](../../design/vision-mock/AtermWindow.dc.html) -
  the `<!-- sidebar -->` block and the `<!-- title bar -->` `activeSessionName`.
- Data: the `SessionList` from [T-10.1](TICKET-10.1-session-model.md). Chrome shell
  + the `◧` toggle-intent stub + the centered title/cwd from
  [T-9.2](../EPIC-9-vision-mock-reskin/TICKET-9.2-window-frame-titlebar.md). Tokens:
  [T-9.1](../EPIC-9-vision-mock-reskin/TICKET-9.1-token-reconciliation.md).
- Crate: `aterm-ui` (the panel widget), wired by `aterm-app`.

# Implementation notes

- **Panel**: 210px wide, a `hairline` right border, `space`-scaled padding, on
  `bg.canvas`. Header row: "SESSIONS" in `fg.faint` uppercase (`type.label`, wide
  letter-spacing) + a `+` affordance (`fg.faint`, hover `fg.primary`) that emits a
  new-session intent (handled in T-10.3).
- **Rows** (`sc-for` over sessions): a 6px status dot - `accent.primary` when the
  session is running, `fg.faint` when idle; the name, ellipsized on overflow; a
  `✕` close control that is invisible until row hover (`fg.faint`, hover
  `fg.primary`). The **active** row: an `accent.primary` weak-tint fill + a 2px
  inset accent bar on the left edge (the mock's `box-shadow: inset 2px 0 0 accent`).
  Click selects; the `✕` closes (both emit intents, T-10.3).
- **Footer** (pinned to the panel bottom): `fg.faint` hint lines - "⌘T new
  session", "⌘I switch mode", "⌘L switch theme".
- **Title-bar binding**: replace T-9.2's placeholder title with the active
  session's name in `fg.primary` + "  -  <cwd>" in `fg.faint` (cwd from OSC-7 when
  integrated, else the process cwd). Wire the `◧` glyph so it opens/closes this
  panel (toggle the sidebar-open state T-9.2 exposed).
- No hardcoded hex; all colors via T-9.1. Motion: a panel open/close may reuse an
  existing slot or be instantaneous - do NOT add a fourth animation kind; keep the
  <=3 / <=220ms budget and the T-1.8 no-per-frame-alloc invariant.

# Acceptance criteria

- [x] The sidebar renders to the mock in both themes: SESSIONS header + `+`, one
  row per live session with the correct running/idle dot color, ellipsized name,
  hover-revealed `✕`, and the active row's tint + 2px inset accent bar.
- [x] The `◧` glyph toggles the panel; the title bar shows the active session name
  + cwd and updates on switch.
- [x] Selecting / closing / adding a session drives the T-10.1 `SessionList` via
  intents (wired fully in T-10.3; the intents exist and are tested here).
- [x] Motion budget + T-1.8 no-per-frame-alloc hold; a widget/GPU test covers the
  panel (>=2 sessions, one active, one idle) in both themes.

# Out of scope

- The session engine itself - [T-10.1](TICKET-10.1-session-model.md).
- Keybinding wiring and focus routing across sessions - [T-10.3](TICKET-10.3-session-keybindings.md).

# Notes

2026-07-17 (agent): The UI owns a retained `SidebarItem` projection and lends it to
the deep `SidebarRenderer` through `SidebarView`; no `aterm-core` public interface was
widened. Running means the session's foreground process group is not the shell. The
sidebar emits index-based add/select/close intents, while T-10.3 remains responsible
for applying them and focus/keybinding routing. The bundled UI font lacks the literal
`✕` and `⌘` glyphs, so the renderer uses its supported `×` close mark and the
Nerd Font Command-key icon from the bundled grid face.
