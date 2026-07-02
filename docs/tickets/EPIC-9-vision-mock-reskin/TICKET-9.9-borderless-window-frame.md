---
id: T-9.9
epic: EPIC-9-vision-mock-reskin
title: Borderless window frame - hide native titlebar, rounded corners + shadow, real window controls
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
  macOS titlebar is gone and only aterm's custom bar renders. *(Borderless +
  transparent window via `with_titlebar_hidden`; verified on hardware - CI is
  headless. See Notes.)*
- [x] The window has the mock's rounded corners, 1px hairline border, and soft drop
  shadow, in both themes, resolving colors through T-9.1 tokens (completes T-9.2 AC1
  - checkbox updated). *(Self-drawn rounded `bg.canvas` frame + hairline via the SDF
  frame pipeline; the drop shadow is the OS window shadow hugging the drawn opaque
  region.)*
- [x] Window controls work: close, minimize, and zoom/maximize are reachable by
  mouse (**Option A**: the custom dots, wired via the T-9.8 hit map) AND by keyboard
  (`Cmd-W`/`Cmd-M`). The chosen option is recorded in Notes.
- [x] Offscreen GPU render test asserts the frame (rounded fill + hairline) inks in
  both themes with the corners rounded away; no per-frame heap allocation introduced.

# Out of scope

- cargo-packager `.app`/`.dmg` bundling + Info.plist ([T-8.1](../EPIC-8-packaging/TICKET-8.1-cargo-packager-titlebar.md)).
- Signing/notarization ([T-8.4](../EPIC-8-packaging/TICKET-8.4-signing-notarization.md)).
- Drag-to-move the title bar / drag-to-resize edges on the borderless window - if
  the transparent/hidden-titlebar transition breaks native window dragging, capture
  it as a follow-up; do not expand this ticket to a full custom window manager.
- The sidebar panel and session binding ([EPIC-10](../EPIC-10-sessions-sidebar/)).

## Notes

Landed 2026-07-02. **Option A** (full mock parity: the custom warm dots wired to real
controls). The chosen mechanism differs slightly from the ticket's sketch, for a reason:

- **Borderless + transparent, not "titlebar-transparent".** winit's `with_titlebar_hidden(true)`
  makes the window `NSWindowStyleMask::Borderless`, which - unlike `with_titlebar_transparent`
  (which keeps a `.titled` window with a titlebar view) - lets the CONTENT view receive the clicks
  on our custom dots (a transparent titlebar view would intercept them). The cost is that a
  borderless window has no native rounding, so we draw it ourselves (which the AC's offscreen test
  wants anyway). `with_fullsize_content_view(true)` + `with_transparent(true)` + `with_has_shadow(true)`
  round it out (`window.rs`).
- **Self-drawn rounded frame** (`window_frame.rs` + a dedicated `frame_pipeline` in `atlas.rs`): a
  `FrameInstance` (rect + fill + border + `[radius, border_px]`) drawn through a rounded-rect SDF
  fragment shader (`vs_frame`/`fs_frame`) so the corners fall to alpha 0. One instance covering the
  window: `bg.canvas` fill + a 1px `hairline` ring, radius 12px (the mock's `.aw`). Damage-gated
  alloc-free. Drawn FIRST, beneath everything - its canvas fill is the base every layer composits
  onto (the transparent clear replaces the old opaque canvas clear).
- **The soft drop shadow is the OS window shadow** (`with_has_shadow`), which hugs the drawn opaque
  rounded region - higher quality than compositing a blurred rect ourselves, and it lives OUTSIDE
  the surface where we cannot draw. (Divergence from the ticket's "draw the shadow" note.)
- **Real controls** (`app.rs` `apply_window_control`): a click on a dot's `HitTarget::WindowControl`
  (via the T-9.8 hit map) calls close -> `event_loop.exit()`, minimize -> `Window::set_minimized`,
  zoom -> `Window::set_maximized` toggle; `Cmd-W`/`Cmd-M` do the same from the keyboard (intercepted
  before host routing - the host binds neither). Dots brighten on hover (`title_bar.rs`). Window
  controls stay live EVEN under the risk-gate approval modal (close/min/zoom can't bypass a safety
  decision, and a window you can't close while a gate is up would feel stuck).
- **Safety valve:** the transparent surface + rounded frame apply only when the adapter grants a
  transparent composite alpha mode (`PostMultiplied` preferred, then `PreMultiplied`); otherwise the
  surface stays `Opaque`, the frame clears to the canvas, and the rounded frame is skipped - a square
  window identical to pre-T-9.9, zero regression.

Adversarial-review fix (a find->verify workflow: 4 raw -> 1 confirmed, 3 refuted): `movable_by_window_background`
made macOS start a background-drag loop on any press-with-drift, swallowing the terminating `mouseUp` -
so a dot click that drifted a few px was silently lost (the dots ARE the drag region). Replaced with an
EXPLICIT `Window::drag_window()` started only on a title-bar-band press that carries NO hit target - the
dots (which carry a target) stay clickable, and the chrome is still draggable, with no mouseUp race. A
separate WGSL fix landed during implementation: the frame shader's `half` field collided with Metal's
`half` type name (whole shader module failed to compile) -> renamed `half_size`.

**On-hardware residuals (CI is headless - `GpuRenderer::new`/the surface are never built in tests):**
the actual transparency, the OS drop shadow, the single-bar / no-native-titlebar result, and live
window dragging are verified on hardware (`mise run run`); CI proves the drawn rounded frame (fill +
hairline + rounded-away corners, both themes) + the pure hit/intent wiring. Deferred (per out-of-scope):
edge-resize affordances beyond the native Borderless resize; a richer drag region; the `.app` Info.plist
must agree with these window attributes ([T-8.1](../EPIC-8-packaging/TICKET-8.1-cargo-packager-titlebar.md)).
