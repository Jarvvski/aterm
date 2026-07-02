---
id: T-9.2
epic: EPIC-9-vision-mock-reskin
title: Window frame + custom title bar shell (traffic lights, centered title, sidebar toggle)
status: done
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
  *(Resolved by [T-9.9](TICKET-9.9-borderless-window-frame.md), reworked 2026-07-02:
  a native TRANSPARENT titlebar - the bar draws under the real traffic lights.)*
- Crate: `aterm-ui` (frame/chrome), wired by `aterm-app`.

# Implementation notes

- **Frame**: `bg.canvas` fill, 1px `hairline` border, ~12px corner radius, the
  mock's soft drop shadow (`0 24px 60px -34px` black at low alpha). Draw as a
  rect-pipeline element; no per-frame allocation (T-1.8 invariant holds).
- **Title bar**: 44px tall, `hairline` bottom rule. Left: three 12px traffic-light
  dots at the mock's warm hues (red `#e0655a`, amber `#dfa63f`, green `#7cae5b` -
  these are the standard macOS-control colors, acceptable as literal chrome
  constants or added as `chrome.*` tokens; prefer tokens). Then the `◧`
  sidebar-toggle glyph in `fg.muted`, hover -> `fg.primary`, emitting a
  toggle-sidebar intent (consumed in EPIC-10). Center (absolutely positioned,
  pointer-events none): the active title in `fg.primary` + `  -  <cwd>` in
  `fg.muted`, `type.label`-ish size. (`fg.muted` is the mock's `--ink-faint`; the
  `fg.faint` token is a further-derived, disabled step.)
- With no sidebar yet, the centered title shows a placeholder title + the current
  cwd (from OSC-7 if available, else the process cwd). EPIC-10 replaces the title
  with the active session name and makes `◧` actually open/close the panel.
- Traffic-light dots are decorative in v1 (real close/min/zoom wiring is a
  packaging concern); render them, do not hook window controls here unless
  trivially available from winit.

# Acceptance criteria

- [x] The window renders rounded with a hairline border + the soft shadow in both
  themes, resolving all colors through T-9.1 tokens (chrome dot colors via tokens
  or a documented chrome-constant set, no scattered literals). *(COMPLETED by
  [T-9.9](TICKET-9.9-borderless-window-frame.md), REWORKED 2026-07-02 on owner
  direction to the NATIVE transparent-titlebar chrome: the rounded corners, window
  border, and drop shadow are the real `.titled` window's own - not token-drawn -
  and the traffic-light dots are the real native buttons, superseding the drawn
  token-colored dots. See the T-9.9 Notes.)*
- [x] The 44px title bar shows the three traffic-light dots, the `◧` toggle glyph
  (with the hover color change), and a centered title + cwd. *(Dots + toggle glyph
  substitute + centered title/cwd all render; the hover color change needs mouse
  hit-testing - see Notes. REWORKED by T-9.9: the bar is now 28px - the native
  titlebar band height - and the dots are the REAL native buttons floating in a
  reserved 71px inset, no longer drawn by this front-end.)*
- [x] The `◧` glyph emits a toggle-sidebar intent (a no-op stub until EPIC-10 is
  fine, but the intent path exists and is tested). *(The intent path is `Cmd-B` ->
  `Session::toggle_sidebar`, tested via the routing chord; the glyph's pointer
  click awaits mouse hit-testing - see Notes.)*
- [x] Offscreen GPU render test asserts title-bar ink in both themes; no per-frame
  heap allocation introduced.

## Notes

Landed 2026-07-02. The title bar is a new `aterm-ui` front-end (`title_bar.rs`) over the
shared glyph atlas, mirroring the input box: three traffic-light dots (`nf-fa-circle` in the
new `chrome.close`/`chrome.minimize`/`chrome.zoom` tokens - identical in both themes, so no
hex is scattered), a sidebar-toggle glyph, and an absolutely-centered active title
(`fg.primary`) + `  -  <cwd>` (`fg.muted`, home abbreviated to `~`), over a bottom `hairline`
rule. The host reserves the top band (`title_bar_px`; 44px as landed, 28px since the T-9.9
rework aligned it to the native titlebar) so the timeline lays out below it
(a `top_inset` threaded into `TimelineRenderer::prepare`). Damage-gated + one rect + one
glyph draw, like every other front-end; a GPU test asserts it inks in both themes and the
unchanged-frame early-out allocates nothing.

Font substitutions (guarded by `title_bar_glyphs_exist_in_the_bundled_grid_font`): the mock's
`◧` (U+25E7) is `.notdef` in the bundled Mono Nerd Font, so `nf-fa-columns` (U+F0DB, a
two-panel icon) stands in.

Deferred (documented, not silently dropped):
- **Rounded corners + border frame + soft drop shadow** were deferred as a
  titlebar-less-window property. *(COMPLETED by [T-9.9](TICKET-9.9-borderless-window-frame.md),
  reworked 2026-07-02: they are the native `.titled` window's own chrome under a transparent
  titlebar - nothing is drawn.)* The in-surface chrome (toggle glyph, centered title/cwd,
  bottom hairline) was delivered here; the drawn dots were later superseded by the real
  native buttons (T-9.9).
- **The toggle glyph's hover color change AND its pointer click** need mouse hit-testing,
  which does not exist yet (the app handles only keys + wheel) - the same cross-cutting
  prerequisite as the T-9.4 mode-chip click and the T-9.3 block-meta hover reveal. Until it
  lands, the toggle-sidebar intent is driven by the `Cmd-B` hotkey (`Session::toggle_sidebar`,
  flipping `sidebar_open`), which EPIC-10's sidebar panel will consume. The intent trigger is
  tested (`sidebar_toggle_chord_is_cmd_b_and_distinct_from_the_other_hotkeys`).
- The title bar was drawn in normal (non-alt-screen) mode only. *(Changed by the T-9.9
  rework: the native buttons are permanent chrome, so the band is reserved and the bar
  drawn in alt-screen too - a full-screen TUI lays out below it.)* The cwd was the
  process cwd at spawn. *(Made LIVE on owner request, 2026-07-02: the engine publishes
  each OSC-7 report through `Engine::current_cwd` and the session re-abbreviates on
  change, so `cd` reaches the bar at the next prompt.)* The active session name replacing
  the placeholder "aterm" title remains EPIC-10 (which owns the title-bar/session binding).

# Out of scope

- The sidebar panel itself, session names, and making `◧` actually toggle a
  panel - [EPIC-10](../EPIC-10-sessions-sidebar/).
- Real window-control (close/minimize/zoom) behavior and the window chrome -
  landed with [T-9.9](TICKET-9.9-borderless-window-frame.md) (native transparent
  titlebar); packaging remains [T-8.1](../EPIC-8-packaging/TICKET-8.1-cargo-packager-titlebar.md).
