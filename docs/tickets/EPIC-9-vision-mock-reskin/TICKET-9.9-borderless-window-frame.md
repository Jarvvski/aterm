---
id: T-9.9
epic: EPIC-9-vision-mock-reskin
title: Window frame - native transparent titlebar (real traffic lights, native rounding + shadow)
status: done
labels: [ui, chrome, macos]
depends_on: [T-9.2, T-9.8]
---

# Goal

Make the custom title bar (T-9.2) the ONLY title bar. Today the window is created
with `Window::default_attributes()`, so macOS draws its full native titlebar AND
aterm draws its own custom bar directly below it - two stacked bars, with the
custom bar's traffic-light dots reading as fake because they duplicate the real,
working native buttons an inch above them. This ticket hides the native chrome,
gives the window the mock's rounded corners + soft drop shadow (a transparent
surface), and makes the window controls real. It closes T-9.2's deferred first
acceptance criterion.

# Context

- North star: [ADR-0011](../../adr/0011-vision-mock-ui-north-star.md) sanctions the
  custom title bar and retires the "no title bar" clause. Visual source:
  [`AtermWindow.dc.html`](../../design/vision-mock/AtermWindow.dc.html) - the
  rounded `.aw` container with the soft drop shadow.
- This is the deferral recorded across the shipped chrome work:
  - T-9.2 AC1 is left UNCHECKED: *"the rounded corners + window border frame + drop
    shadow cannot be drawn into a native-decorated opaque surface - they land with
    the borderless packaging."* T-9.2 Notes: *"a shadow lives OUTSIDE the window;
    rounding needs a transparent surface."*
  - The custom bar's dots are DECORATIVE no-ops today (T-9.2: *"real
    close/minimize/zoom is packaging"*).
- Window creation is `window_attributes()` in `aterm-ui/src/window.rs:18`
  (`Window::default_attributes().with_title(..).with_inner_size(..)`) - no
  decoration/titlebar flags.
- Research: [`10-packaging-scaffold.md`](../../research/10-packaging-scaffold.md)
  §(b) - the winit `WindowAttributesExtMacOS` hidden-titlebar layers.
- **Boundary vs T-8.1**: T-8.1 keeps ONLY the cargo-packager `.app`/`.dmg` +
  Info.plist work. The window-chrome BEHAVIOR (hiding the native titlebar, the
  transparent surface, wiring real controls) moves HERE - it is a dev-run visual
  bug today, not a packaging concern, and this ticket must land for the doubled bar
  to disappear in `mise run run`, long before any `.dmg` exists. T-8.1's Info.plist
  just has to agree with the window attributes this ticket sets.

# Decision to resolve (the traffic-light dots)

The native titlebar and the mock's custom dots cannot both own the top-left. Pick
one and document it:

- **Option A (recommended - full mock parity)**: fully hide the native titlebar,
  keep the custom dots, and wire them to real controls. Clicking a dot (via the
  T-9.8 hit map) calls the winit control: close -> `event_loop.exit()` / window
  drop, minimize -> `Window::set_minimized(true)`, zoom -> `Window::set_maximized`
  toggle. Matches the mock's warm-hued dots exactly; costs the wiring + depends on
  T-9.8. **This is why T-9.8 is a dependency.**
- **Option B (cheaper, less parity)**: use the transparent-titlebar approach
  (`with_titlebar_transparent(true)` + `with_fullsize_content_view(true)`, keep the
  NATIVE floating buttons), and REMOVE the custom dots from `title_bar.rs`, leaving
  the native buttons where the mock's dots would sit. Real controls for free; drops
  the mock's custom dots and needs the title-bar layout to reserve the
  native-button inset. If chosen, drop the T-9.8 dependency and note it.

Go with **A** unless it proves fiddly on the installed winit/macOS version; either
way, record the choice in the ticket Notes so the next agent knows why.

# Implementation notes

- **Hide native chrome** (`aterm-ui/src/window.rs`): add the `WindowAttributesExtMacOS`
  flags. Option A: `with_titlebar_hidden(true)` (or decorations off) + transparent
  surface. Option B: `with_titlebar_transparent(true)` + `with_title_hidden(true)`
  + `with_fullsize_content_view(true)`. Keep the existing title + inner-size.
- **Transparent surface + rounded corners + shadow**: request a transparent window
  (`with_transparent(true)`) and configure the wgpu surface for alpha; draw the
  mock's `bg.canvas` fill into a rounded-rect with the 1px `hairline` border and the
  soft drop shadow (`0 24px 60px -34px` black, low alpha) - the rect-pipeline
  element T-9.2 specced but could not draw into an opaque native surface. The T-1.8
  no-per-frame-alloc invariant holds.
- **Real controls (Option A)**: on a completed click on a dot's `HitTarget`
  (from T-9.8), invoke the matching winit call. The dots keep their warm chrome
  tokens; hover behavior comes from T-9.8.
- The custom bar keeps its centered title/cwd and sidebar glyph unchanged.
- Non-goal for the layout: this does not add/move the sidebar (EPIC-10) or change
  the input box.

# Acceptance criteria

- [x] Running the app (`mise run run`) shows exactly ONE title bar - the native
  macOS titlebar is gone and only aterm's custom bar renders. *(REWORKED
  2026-07-02 to the native transparent titlebar - see Notes; verified on hardware
  by screenshot. The `.titled` window paints no titlebar background/text/separator,
  so the custom bar is the only visible bar.)*
- [x] The window has rounded corners and a soft drop shadow in both themes
  (completes T-9.2 AC1 - checkbox updated). *(Reworked: these are the NATIVE
  `.titled` window's corners + shadow now, not drawn through tokens - the 2026-07-02
  owner direction chose native chrome over the mock's self-drawn `.aw` frame. The
  1px hairline border is likewise the native window border.)*
- [x] Window controls work: close, minimize, and zoom/maximize are reachable by
  mouse AND close/minimize by keyboard (`Cmd-W`/`Cmd-M`). The chosen option is
  recorded in Notes. *(Reworked: the mouse controls are the REAL native
  traffic-light buttons - full native behavior incl. zoom, Option-click, hover
  states - not aterm-drawn dots.)*
- [x] Offscreen GPU render test asserts the title bar in both themes; no per-frame
  heap allocation introduced. *(Reworked: with no drawn frame the test now asserts
  the INVERSE - the native traffic-light inset stays EMPTY (nothing may ink under
  the real buttons) while the toggle glyph, centered title, and hairline ink -
  `title_bar_inks_centered_title_and_keeps_the_native_button_inset_empty`.)*

# Out of scope

- cargo-packager `.app`/`.dmg` bundling + Info.plist ([T-8.1](../EPIC-8-packaging/TICKET-8.1-cargo-packager-titlebar.md)).
- Signing/notarization ([T-8.4](../EPIC-8-packaging/TICKET-8.4-signing-notarization.md)).
- Drag-to-move the title bar / drag-to-resize edges on the borderless window - if
  the transparent/hidden-titlebar transition breaks native window dragging, capture
  it as a follow-up; do not expand this ticket to a full custom window manager.
- The sidebar panel and session binding ([EPIC-10](../EPIC-10-sessions-sidebar/)).

## Notes

Landed 2026-07-02 as **Option A**; **REWORKED the same day to Option B (native transparent
titlebar) on owner direction**: "It should use the native style transparent title bar that
other apps have - kitty, Slack, Linear" (with screenshots). Option A's fully-custom frame -
self-drawn rounded corners, drawn hairline, aterm-drawn traffic-light dots - read as "some
entirely custom thing" next to those apps' real native chrome. The rework:

- **The window is `.titled` with a transparent titlebar** (`window.rs`):
  `with_titlebar_transparent(true)` + `with_title_hidden(true)` +
  `with_fullsize_content_view(true)` - byte-for-byte alacritty's `decorations =
  "Transparent"`, and the same style wezterm's `INTEGRATED_BUTTONS` and kitty use. macOS
  paints NO titlebar background, title text, or separator (verified by an offscreen AppKit
  paint probe on macOS 15.7.7), so aterm's custom bar is the only visible bar - while the
  rounded corners, drop shadow, 1px window border, and traffic-light buttons are all REAL
  native chrome. Everything Option A hand-built (the `window_frame.rs` SDF frame renderer,
  the `frame_pipeline` + `FrameInstance` + `vs_frame`/`fs_frame` WGSL, the transparent
  surface/alpha-mode selection, the drawn dots + their `HitTarget::WindowControl` wiring)
  is REMOVED; the wgpu surface is opaque again.
- **Window controls are the native buttons.** Mouse close/minimize/zoom is AppKit's own
  (the button widgets hit-test before our content view - native hover states, Option-click
  zoom). `Cmd-W`/`Cmd-M` remain keyboard chords in `app.rs` (`apply_window_control`, now a
  private two-variant enum - zoom has no conventional chord and needs no arm) since aterm
  has no menu bar to provide them. Double-click-the-band-to-zoom is implemented in aterm
  (a second no-target band press within 500ms toggles `set_maximized`): an adversarial
  reviewer PROVED with an AppKit probe that `performWindowDragWithEvent` alone never fires
  the native double-click action (the transparent titlebar routes band presses to the
  content view, so AppKit's own handler never sees them) - this is the second half of the
  Zed pattern (theirs reads `AppleActionOnDoubleClick`; aterm assumes the Zoom default and
  a fixed 500ms interval - winit exposes neither the pref nor `clickCount`).
- **The title bar aligns to the native button geometry** (`title_bar.rs`): AppKit centers
  the buttons at y=14pt in the standard 28pt band and winit exposes no
  `trafficLightPosition` to move them, so `TITLE_BAR_LOGICAL` goes 44 -> **28** (the band
  IS the native band; glyph + title + buttons share one vertical center) and the sidebar
  glyph starts at `TRAFFIC_LIGHT_INSET_LOGICAL` = **71** logical px (the cluster ends at
  x=61pt on macOS 15; 71 is Zed's `TRAFFIC_LIGHT_PADDING` convention). The bar draws
  NOTHING left of the inset - the GPU test asserts that region stays ink-free in both
  themes so drawn chrome can never creep back under the real buttons.
- **Pointer behavior in the band, measured on macOS 15.7.7** (hitTest probe): with the
  transparent titlebar, events in the band away from the buttons reach the CONTENT view -
  so the sidebar glyph stays hover/clickable with zero extra work - and nothing drags
  automatically (`mouseDownCanMoveWindow` is NO for opaque views). Dragging stays the
  explicit `Window::drag_window()` on a no-target band press (the Zed pattern), unchanged
  from Option A's adversarial-review fix; `movable_by_window_background` stays off (it
  swallows the mouseUp of a drifted click).
- **The band is permanent chrome - reserved even in alt-screen** (an adversarial-review
  catch, major): the native buttons float over the surface's top-left and are never
  hidden, so hiding the bar in alt-screen (the pre-rework rule, written when the native
  titlebar sat ABOVE the content) would put a full-screen TUI's top-left cells under the
  immovable buttons - occluded, and a click aimed at them would land on the red button
  and close the whole app. Now: the bar draws whenever the host supplies it (including
  alt-screen), the PTY grid is sized from the surface MINUS the band (`app.rs` resize),
  and the grid fast-path lays out below a matching `top_inset`
  (`grid_render.rs::prepare`, folded into its damage key).
- **Verified on hardware by screenshot** (this session, `screencapture -l` of the running
  window): one title bar, real colored traffic lights centered in the 28px band with the
  glyph + title, native rounded corners + shadow. CI keeps proving the drawn bar (title +
  hairline ink, inset empty, both themes) + the pure hit/intent wiring; the native chrome
  itself is AppKit's.

History (Option A, superseded but kept for the record): borderless `NSWindowStyleMask::Borderless`
window + self-drawn SDF rounded frame + drawn dots wired through the T-9.8 hit map; a WGSL
`half` -> `half_size` Metal-reserved-word fix; a `movable_by_window_background` mouseUp-race
fix (the drag_window pattern above survives from it). Superseded because the owner wants the
native chrome, which is also strictly less code: the rework deletes ~500 lines (frame renderer,
frame pipeline, dot drawing/hover/wiring) and removes the transparency safety-valve complexity.

Deferred (per out-of-scope): the `.app` Info.plist must agree with these window attributes
([T-8.1](../EPIC-8-packaging/TICKET-8.1-cargo-packager-titlebar.md)); kitty-style fullscreen
button-visibility quirks are native-handled here (buttons are never hidden).
