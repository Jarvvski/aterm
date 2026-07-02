---
id: T-9.9
epic: EPIC-9-vision-mock-reskin
title: Borderless window frame - hide native titlebar, rounded corners + shadow, real window controls
status: ready-for-agent
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

- [ ] Running the app (`mise run run`) shows exactly ONE title bar - the native
  macOS titlebar is gone and only aterm's custom bar renders.
- [ ] The window has the mock's rounded corners, 1px hairline border, and soft drop
  shadow, in both themes, resolving colors through T-9.1 tokens (completes T-9.2 AC1
  - update that checkbox).
- [ ] Window controls work: close, minimize, and zoom/maximize are reachable by
  mouse (Option A: the custom dots; Option B: the retained native buttons) AND by
  keyboard (`Cmd-W`/`Cmd-M`). The chosen option is recorded in Notes.
- [ ] Offscreen GPU render test asserts a single title bar + the frame (rounded fill
  + hairline) inks in both themes; no per-frame heap allocation introduced.

# Out of scope

- cargo-packager `.app`/`.dmg` bundling + Info.plist ([T-8.1](../EPIC-8-packaging/TICKET-8.1-cargo-packager-titlebar.md)).
- Signing/notarization ([T-8.4](../EPIC-8-packaging/TICKET-8.4-signing-notarization.md)).
- Drag-to-move the title bar / drag-to-resize edges on the borderless window - if
  the transparent/hidden-titlebar transition breaks native window dragging, capture
  it as a follow-up; do not expand this ticket to a full custom window manager.
- The sidebar panel and session binding ([EPIC-10](../EPIC-10-sessions-sidebar/)).
