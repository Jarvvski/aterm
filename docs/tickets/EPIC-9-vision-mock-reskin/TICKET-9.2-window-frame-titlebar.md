---
id: T-9.2
epic: EPIC-9-vision-mock-reskin
title: Window frame + custom title bar shell (traffic lights, centered title, sidebar toggle)
status: ready-for-agent
labels: [ui, chrome]
depends_on: [T-9.1]
---

# Goal

Draw the mock's window frame: a rounded, hairline-bordered window with a soft
drop shadow, and a 44px custom title bar carrying the traffic-light dots, a
centered active-title + cwd path, and the `◧` sidebar-toggle glyph. This is the
chrome shell that every screen sits inside; it renders correctly with a single
session (session *data* and the sidebar panel come from EPIC-10).

# Context

- North star: [ADR-0011](../../adr/0011-vision-mock-ui-north-star.md) sanctions a
  custom title bar (the "no title bar" clause of `design-system.md` §1 is
  retired). Visual source:
  [`AtermWindow.dc.html`](../../design/vision-mock/AtermWindow.dc.html) - the
  root `.aw` container and the `<!-- title bar -->` block.
- The window keeps a hidden *native* titlebar; the custom bar is drawn inside it.
  Packaging of the titlebar-less window is [T-8.1](../EPIC-8-packaging/TICKET-8.1-cargo-packager-titlebar.md).
- Crate: `aterm-ui` (frame/chrome), wired by `aterm-app`.

# Implementation notes

- **Frame**: `bg.canvas` fill, 1px `hairline` border, ~12px corner radius, the
  mock's soft drop shadow (`0 24px 60px -34px` black at low alpha). Draw as a
  rect-pipeline element; no per-frame allocation (T-1.8 invariant holds).
- **Title bar**: 44px tall, `hairline` bottom rule. Left: three 12px traffic-light
  dots at the mock's warm hues (red `#e0655a`, amber `#dfa63f`, green `#7cae5b` -
  these are the standstill macOS-control colors, acceptable as literal chrome
  constants or added as `chrome.*` tokens; prefer tokens). Then the `◧`
  sidebar-toggle glyph in `fg.faint`, hover -> `fg.primary`, emitting a
  toggle-sidebar intent (consumed in EPIC-10). Center (absolutely positioned,
  pointer-events none): the active title in `fg.primary` + `  -  <cwd>` in
  `fg.faint`, `type.label`-ish size.
- With no sidebar yet, the centered title shows a placeholder title + the current
  cwd (from OSC-7 if available, else the process cwd). EPIC-10 replaces the title
  with the active session name and makes `◧` actually open/close the panel.
- Traffic-light dots are decorative in v1 (real close/min/zoom wiring is a
  packaging concern); render them, do not hook window controls here unless
  trivially available from winit.

# Acceptance criteria

- [ ] The window renders rounded with a hairline border + the soft shadow in both
  themes, resolving all colors through T-9.1 tokens (chrome dot colors via tokens
  or a documented chrome-constant set, no scattered literals).
- [ ] The 44px title bar shows the three traffic-light dots, the `◧` toggle glyph
  (with the hover color change), and a centered title + cwd.
- [ ] The `◧` glyph emits a toggle-sidebar intent (a no-op stub until EPIC-10 is
  fine, but the intent path exists and is tested).
- [ ] Offscreen GPU render test asserts title-bar ink in both themes; no per-frame
  heap allocation introduced.

# Out of scope

- The sidebar panel itself, session names, and making `◧` actually toggle a
  panel - [EPIC-10](../EPIC-10-sessions-sidebar/).
- Real window-control (close/minimize/zoom) behavior and the titlebar-less
  packaging - [T-8.1](../EPIC-8-packaging/TICKET-8.1-cargo-packager-titlebar.md).
