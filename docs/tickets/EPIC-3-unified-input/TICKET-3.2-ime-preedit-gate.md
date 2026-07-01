---
id: T-3.2
epic: EPIC-3-unified-input
title: IME via winit Ime events + preedit-active gate
status: done
labels: [ui, input, ime, macos]
depends_on: [T-1.5]
---

# Goal

Wire macOS IME into the self-drawn input box via winit 0.30 `Ime` events, with a hand-rolled `NSTextInputClient` escape hatch identified, and make `preedit-active` the highest-priority routing gate so Enter confirms an IME candidate and never submits.

# Context

- Research: [05-unified-input-ux.md](../../research/05-unified-input-ux.md) section 2 (macOS IME) + Recommendations 4-5. Known trap: Zed terminal #23003 - Enter during composition inserts a newline instead of confirming. Known winit gaps: `_selected_range`/`_replacement_range` ignored (#3617), historical Pinyin `set_marked_text` OOB crash.

# Implementation notes

- Crate: `aterm-ui`. Module `ime`. Feeds the `InputModel.preedit` field (T-3.1).
- winit: `Window::set_ime_allowed(true)`; handle `WindowEvent::Ime(Ime)` -> `Enabled`/`Preedit(String, Option<(usize,usize)>)` (byte-indexed cursor range)/`Commit(String)`/`Disabled`. Call `Window::set_ime_cursor_area(...)` so the candidate window sits under the caret.
- Map `Preedit` -> `InputModel.preedit = Some(Preedit { text, cursor })`; `Commit` -> insert committed text and clear preedit.
- Escape hatch: design the IME seam so a hand-rolled `NSTextInputClient` on a single raw `NSView` (Zed/GPUI model, via `objc2`) can replace winit's IME if #3617 / Pinyin gaps bite CJK users. Stub the trait now; implement only if needed (document the decision).
- The `preedit-active` gate itself is enforced in the routing brain (T-3.3) which reads `preedit.is_some()` first; this ticket guarantees `preedit` is populated/cleared correctly.

# Acceptance criteria

- Composing Japanese/Pinyin shows preedit text inline with the candidate window positioned at the caret.
- Pressing Enter while composing confirms the candidate (commits) and does NOT submit/route (verified end-to-end with T-3.3).
- `Commit` inserts the final text as inert characters (T-3.1 semantics).
- `Disabled`/blur clears any dangling preedit.
- Document whether winit's IME is sufficient or the NSTextInputClient hatch is needed for the target IMEs.

# Out of scope

- The full routing brain (T-3.3) - this ticket only populates `preedit` and verifies the gate behavior jointly.
- Key encoding for raw passthrough (T-3.4).

# Notes (landed 2026-07-01)

All ACs met. The IME feed is wired end to end; the pure pieces are unit-tested on every
platform, the `set_ime_cursor_area` positioning is the one macOS-only side effect.

- **Core mutators (`aterm-core::InputModel`).** New `set_preedit(Option<Preedit>)` (a
  transient overlay - never touches text/selection/undo) and `commit_ime(&str)` (clears
  the preedit, then inserts through the ordinary `insert` path: ONE undo unit, replaces a
  selection, inert/literal). `take()` already dropped a dangling preedit; a test now guards
  it. Covered by six `input::tests` cases (transient overlay, one-undo-unit commit,
  selection-replace, empty-commit-only-clears, inert control chars, take-clears).
- **The neutral seam (`aterm-ui::ime`).** A renderer-neutral `ImeEvent`
  (`Enabled`/`Preedit{text,cursor}`/`Commit`/`Disabled`) with `from_winit` mapping
  `winit::event::Ime` 1:1 (mirrors how `KeyPress` abstracts winit keys), so `aterm-app`
  drives composition without naming winit. `UiCallbacks::on_ime` delivers it. `app.rs` calls
  `Window::set_ime_allowed(true)` on window creation, maps `WindowEvent::Ime` to `on_ime`
  (composition is keep-warm activity), and positions the candidate window under the caret via
  `set_ime_cursor_area` using the caret rect the input front-end now records each build
  (`InputWidgetRenderer::caret_area_px` -> `GpuRenderer::ime_cursor_area`), issued only when
  the caret moves.
- **Blur clears the preedit (AC4, review finding).** winit on macOS does NOT emit
  `Ime::Disabled` on `windowDidResignKey` - only `Focused(false)` - so a composition marked at
  blur would otherwise leave `preedit` set forever, and the routing brain (gating on
  `preedit_active` first) would swallow every subsequent key, wedging the input box. `app.rs`
  handles `WindowEvent::Focused(false)` by synthesizing an `on_ime(ImeEvent::Disabled)`, which
  the host's `apply_ime` turns into `set_preedit(None)` (already covered by
  `disabled_clears_a_dangling_preedit`). This closes the "Disabled/blur clears any dangling
  preedit" AC, which the initial commit met for Disabled but not blur.
- **The gate (AC2, the Zed #23003 trap).** `Session::routing_context` now sources
  `preedit_active = input.preedit().is_some()` (was hardcoded `false`), so the routing brain's
  existing highest-priority gate makes Enter/Tab/Esc confirm the candidate and never
  submit/route mid-composition. The `decide` behavior was already tested
  (`routing::ime_composition_owns_enter_and_never_submits`); the Session glue is a pure
  `apply_ime` helper with three `session::tests` cases (populate-then-empty-clears,
  commit-inserts-and-clears, disable-clears).
- **AC5 decision: winit's IME is sufficient for v1; the `NSTextInputClient` hatch is NOT
  implemented.** winit 0.30's `Ime` events cover inline preedit + byte-indexed cursor +
  commit + `set_ime_cursor_area` for the target IMEs (Japanese/Pinyin). The known winit gaps
  (#3617 selected/replacement range ignored; the historical Pinyin `set_marked_text` OOB) do
  not affect inline compose+commit, so we ship on winit. The escape hatch is designed as a
  seam - the documented `NativeTextInput` marker trait in `aterm-ui::ime` - so a hand-rolled
  `NSTextInputClient` on a raw `NSView` (via `objc2`) can be slotted behind it if a real CJK
  gap is reported, with the host still only speaking `ImeEvent`.
- **Residual:** the on-hardware CJK candidate-window placement is only exercisable on a real
  macOS IME (no unit test for the winit side effect); the pure mapping + gate + mutators are
  fully covered. `mise run fmt && lint && build && test` green at `-D warnings` (the sole red
  is the pre-existing `flood_publishes_track_ticks_not_bytes` load-flake, which passes in
  isolation and is untouched here). No version bump (accumulating under Unreleased).
