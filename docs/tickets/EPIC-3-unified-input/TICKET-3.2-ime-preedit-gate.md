---
id: T-3.2
epic: EPIC-3-unified-input
title: IME via winit Ime events + preedit-active gate
status: ready-for-agent
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
