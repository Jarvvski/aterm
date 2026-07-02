---
id: T-9.8
epic: EPIC-9-vision-mock-reskin
title: Mouse-pointer plumbing + hit-testing (hover/click affordances)
status: done
labels: [ui, input]
depends_on: [T-9.1, T-9.2, T-9.3, T-9.4, T-9.5]
---

# Goal

Give the app a mouse pointer. Today `aterm-ui` handles only keys + the scroll
wheel (`WindowEvent::MouseWheel`); there is no pointer position, no hit-testing,
and no click dispatch, so every hover/click affordance the mock calls for has
shipped as a keyboard-only stub with a documented deferral. This ticket adds the
one shared plumbing layer - pointer tracking, a pure hit-test map, hover state,
and click dispatch - and uses it to complete the affordances already drawn by
T-9.2/9.3/9.4/9.5. It is the cross-cutting prerequisite those tickets each
name in their Notes.

# Context

- North star: [ADR-0011](../../adr/0011-vision-mock-ui-north-star.md). Visual
  source: [`AtermWindow.dc.html`](../../design/vision-mock/AtermWindow.dc.html) -
  the hover states on the title-bar sidebar glyph, the block-meta reveal, the
  mode chip, and the completion popover rows.
- The deferral is recorded verbatim in the shipped tickets:
  - T-9.2 Notes: *"The toggle glyph's hover color change AND its pointer click
    need mouse hit-testing, which does not exist yet (the app handles only keys +
    wheel)."*
  - T-9.3: the block-meta reveal reuses the `Animation::FocusDim` slot but nothing
    drives the hover state (`components.rs` `BlockMetaStyle`; `timeline_render.rs`
    "hover-gating itself is a follow-up").
  - T-9.4: the mode-chip click.
  - T-9.5 Notes: the modes explainer's "tap the chip" affordance + the popover
    rows, "the same cross-cutting prerequisite as the T-9.4 chip click / T-9.2
    sidebar-glyph click."
- The affordances already exist visually and are keyboard-drivable (`Cmd-B`
  sidebar intent, `Cmd-/` mode toggle, `Cmd-?` modes, Tab/arrows in the popover).
  This ticket adds the *pointer* path into the SAME intents - it must not
  introduce a second, divergent action route.
- Crate: `aterm-ui` owns pointer state + the pure hit map + hover; `aterm-app`
  routes the winit events in and maps click targets to the existing `Session`
  intents. The 60fps invariants (T-1.8) hold: no per-frame allocation on a steady
  (unchanged-hover) frame.

# Implementation notes

- **Winit events** (`aterm-ui/src/app.rs`): add arms for `WindowEvent::CursorMoved`
  (store pointer position, converting physical px -> logical, accounting for the
  `top_inset` title-bar band), `CursorLeft` (clear hover), and `MouseInput`
  (track button press + release for click = press-then-release on the same
  target). `MouseWheel` stays as-is.
- **Pure hit map** (new, `aterm-ui`, e.g. `hit.rs`): a `HitTarget` enum
  (`SidebarToggle`, `ModeChip`, `BlockMeta(BlockId)`, `CompletionRow(usize)`, room
  to grow) and a `HitMap` that the front-ends populate with `(Rect, HitTarget)`
  regions as they compute geometry during `prepare`/draw (they already know these
  rects - title_bar, input box + chip, timeline blocks, completion_render). A pure
  `HitMap::hit(point) -> Option<HitTarget>` is the crown of the ticket and is
  unit-tested with NO window (last-inserted-wins / topmost for overlaps; document
  the rule).
- **Hover state**: the current hovered `HitTarget` lives in the UI state; on
  change, flag damage so the frame redraws. Drive: title-bar sidebar glyph
  `fg.muted` -> `fg.primary`; the block-meta `FocusDim` reveal for the hovered
  block; the mode-chip hover treatment; optionally the completion active row
  follows the pointer. A steady hover (no change) must allocate nothing and not
  force redraws (respect the unchanged-frame early-out).
- **Click dispatch** (`aterm-app`): on a completed click, look up the target and
  emit the EXISTING intent - `SidebarToggle` -> `Session::toggle_sidebar` (the
  `Cmd-B` path), `ModeChip` -> the `Cmd-/` mode toggle, `CompletionRow(i)` ->
  activate + accept that row (the popover's Enter path), `BlockMeta` -> whatever
  the block-meta affordance is (or no-op if none yet). No new action semantics.
- **Cursor icon** (nice-to-have, keep cheap): `Window::set_cursor` to a pointer
  over a clickable target, default arrow/text elsewhere. If it adds churn, defer
  it in the Notes rather than half-doing it.

# Acceptance criteria

- [x] `CursorMoved` / `CursorLeft` / `MouseInput` are handled; the pointer
  position is tracked in physical px (surface-relative), which is scale-correct
  against the front-ends' physical-px rects (see Notes on the coordinate-space choice).
- [x] A pure `HitMap::hit(point)` is unit-tested with no window (hits, misses,
  overlap/topmost rule, and empty-map), on every platform.
- [x] Hover drives the four affordances the shipped tickets deferred: the title-bar
  sidebar glyph color change (T-9.2), the block-meta `FocusDim` reveal (T-9.3), the
  mode-chip hover (T-9.4), and the completion popover rows (T-9.5) - each damage-flagged,
  with no per-frame allocation on a steady frame (T-1.8 invariant holds).
- [x] Clicking a target emits the SAME intent as its keyboard equivalent: the
  sidebar glyph == `Cmd-B`, the mode chip == `Cmd-/`, a completion row == Enter.
  Verified (the intent trigger is tested, mirroring the chord tests).
- [x] A GPU/widget test covers a hover-driven redraw in both themes.

# Out of scope

- **Text selection by mouse drag** (select terminal/block output, drag-to-extend,
  copy-on-select). That is a distinct feature with its own model and is NOT part of
  this plumbing ticket - it gets its own future ticket.
- The T-9.7 approval card's split "Approve + menu" button and the T-9.6 tool-call
  rows are their OWN re-skin tickets; they may ADOPT this hit-testing once it lands
  but are not hard-blocked by it (both ship keyboard-first).
- Real window-control (close/min/zoom) behavior on the traffic-light dots - a
  packaging concern ([T-8.1](../EPIC-8-packaging/TICKET-8.1-cargo-packager-titlebar.md)).
- Drag-to-resize / drag-the-title-bar-to-move (native titlebar handles these until
  the borderless window lands in T-8.1).

## Notes

Landed 2026-07-02. One shared plumbing layer, no second action route:

- **Pure map** (`aterm-ui/src/hit.rs`): `HitTarget { SidebarToggle, ModeChip, BlockMeta(usize),
  CompletionRow(usize) }` + `HitMap` (a `Vec<(rect, target)>`). `HitMap::hit` scans newest-first
  so the LAST-inserted (topmost, drawn-over) region wins on overlap; rects are half-open on the
  right/bottom edge so abutting regions never both match. No winit/GPU/clock - six unit tests run
  on every platform (hits, misses, overlap/topmost, empty, zero-size, capacity-retained clear).
- **Front-ends** cache their clickable rects on the rebuild path (so an idle early-out still hit-
  tests) and each folds a hover discriminant into its own damage signature so a hover change forces
  exactly one rebuild: the title-bar sidebar glyph brightens `fg.muted -> fg.primary`; the mode pill
  lifts to a stronger accent tint (text re-nudged for legibility); a completion row draws the mock's
  neutral `.cmpl:hover` tint; and the command block-meta is now REVEALED only when its block is
  hovered - the mock's `.block-meta { opacity:0 }` / `.block:hover { opacity:1 }`, the reveal in the
  `BlockMetaStyle::reveal_animation` (`FocusDim`) slot.
- **Renderer** (`gpu.rs`) owns the `HitMap` + the hovered target; `render_inner` passes the hover to
  each front-end, folds the hovered block into `timeline_sig`, and rebuilds the map alloc-free
  (`clear` + push on the warm `Vec`, from each drawn front-end's accessor) in DRAW order so topmost
  wins. `hit_test`/`set_hover`/`hovered` are the host seam.
- **Pointer path** (`app.rs`): `CursorMoved` tracks the position and updates hover, repainting ONLY
  on a hover CHANGE (a steady hover allocates nothing and forces no redraw - the T-1.8 floor) and
  swapping the cursor to a hand over a clickable target; `CursorLeft` clears it; `MouseInput`
  fires a click on a left press-then-release on the SAME target. The new `UiCallbacks::on_click`
  hands the resolved target to the host.
- **Intents** (`session.rs`): `on_click` routes each target to the EXISTING intent -
  `SidebarToggle -> toggle_sidebar` (== `Cmd-B`), `ModeChip -> toggle_mode` (== `Cmd-/`, an
  extracted helper now shared with the `ToggleMode` disposition so the two paths are identical),
  `CompletionRow(i) -> Completion::set_index(i) + accept_completion` (== arrow-to-row + `Enter`),
  `BlockMeta -> no-op` (hover-reveal only). Three macOS-gated tests spawn a real `Session` and assert
  click == chord (skipping gracefully if no login shell spawns).

**Modal safety.** While a risk-gate approval card is parked, the hit map is left EMPTY, so no click
can slip through to a control behind the pending decision; the hover treatments are also cleared
(effective hover forced to `None`) so nothing behind the modal reads as clickable (an
adversarial-review fix - hover must imply clickable).

**Decisions / divergences (flagged, not silently overridden):**
- **Coordinate space is physical px, not the AC's literal "logical".** The pointer (winit reports
  `CursorMoved` in physical px, relative to the content-area origin where our surface `(0,0)` sits)
  and every front-end's rects are both physical, so physical-vs-physical is inherently scale-correct
  and needs no title-bar `top_inset` offset and no logical-px rounding round-trip. This satisfies the
  AC's intent (scale-correct, inset-aware) with less error surface.
- **`HitTarget::BlockMeta` carries the block INDEX (`usize`), not a `BlockId`.** `aterm-core` has no
  stable block id; the index (matching `VisibleBlock::index`) is the identity, re-resolved every
  frame from the live block list, so a transient off-by-one on eviction only flickers a hover.
- **The block-meta is now hidden until hover** (a behavior change from T-9.3's always-on meta), to
  match the mock exactly. The dot color/shape + the "exit N" label still carry the state without
  color when revealed (color-blind safety).

Deferred (documented): the traffic-light dots' hover + real window controls ride in
[T-9.9](TICKET-9.9-borderless-window-frame.md) (which extends `HitTarget` with the window controls);
text selection by mouse drag is its own future ticket (out of scope above).
