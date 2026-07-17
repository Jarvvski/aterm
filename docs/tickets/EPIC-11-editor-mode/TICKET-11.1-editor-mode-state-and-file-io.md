---
id: T-11.1
epic: EPIC-11-editor-mode
title: Editor mode state + file open/save + mode transitions
status: done
labels: [ui, app, editor]
depends_on: []
---

# Goal

Add an editor view to aterm: opening a file folds the block timeline away and shows a writing surface; `esc` returns to the shell; `⌘S` saves. This ticket owns the state and file I/O half - the app-level view state, the open/save/dirty/word-count model, and the transitions in and out - so T-11.2 can attach the on-screen writing surface to a working model.

# Context

- Visual source of record: [`docs/design/vision-mock/AtermWindow.dc.html`](../../design/vision-mock/AtermWindow.dc.html), the `screen="editor"` state - a file opened into a calm, centered surface with a header ("`NOTES.md` · edited" left; "markdown · N words · ⌘S save · esc to shell" right), the timeline hidden and the input bar suppressed (`showInput` is false for `editor`).
- Decision of record: [ADR-0011](../../adr/0011-vision-mock-ui-north-star.md) - adopts the vision mock as the UI north star and adds editor mode as a first-class app view. Note it explicitly rejects modeling editor as a third `InputModel` mode.
- Research: [07-ia-design-language.md](../../research/07-ia-design-language.md) (the iA writing-surface ethos - a calm, centered, distraction-free editing surface). [05-unified-input-ux.md](../../research/05-unified-input-ux.md) for the input/editing infrastructure the surface reuses (T-11.2).

# Implementation notes

- Crate: `aterm-app` owns the view state and orchestration; `aterm-core` owns any pure file/document model worth unit-testing (path, buffer, dirty flag, word count) with no window or I/O in the test. Keep the crate boundary: file read/write is transport, not domain - do not leak it into the timeline model.
- **This is an app-level view state, NOT a third `InputModel` mode.** The locked `InputModel` (domain.md, ADR-0004) holds `text` + `selection` + `mode: Shell | Agent`, and the mode-toggle hotkey mutates ONLY `mode`. Editor mode is a separate `AppView { Terminal, Editor }`-style enum at the app layer; entering it does not touch `InputModel.mode`, so the Shell/Agent routing target is preserved and restored on exit. If the implementation finds itself wanting a third `InputModel` variant, STOP and flag it (see `# Notes`) - that would contradict a locked decision.
- **Enter editor:** an explicit action opens a file into the editor view (the trigger surface - a command, an agent action, or a keybinding - is the app's to wire; the mock does not prescribe it). Read the file into an editable buffer; track `path`, `dirty` (edited-since-save), and a live word count.
- **Exit editor:** `esc` returns to the shell view (the timeline + input bar reappear, unchanged). Define the dirty-on-exit behavior (save prompt vs discard vs keep-open) and record the chosen default in `# Notes` for owner review rather than guessing silently.
- **Save:** `⌘S` writes the buffer back to `path`, clears `dirty`. Surface save failures (permission/IO) without crashing the view.
- Fold behavior: entering editor suppresses the timeline render and the input bar; leaving restores them. Do not destroy timeline/session state - it must be intact on return.

# Acceptance criteria

- [x] An `AppView`-level editor state exists that is distinct from `InputModel.mode`; entering/leaving editor does not mutate `InputModel.mode` (unit-tested).
- [x] Opening a file populates an editable buffer with `path`, `dirty=false`, and a correct word count; editing sets `dirty=true` and updates the count.
- [x] `⌘S` saves to `path` and clears `dirty`; an IO/permission failure is reported, not panicked.
- [x] `esc` returns to the shell view with the timeline and input bar restored intact; the pre-editor Shell/Agent routing target is preserved.
- [x] Pure document model (buffer/dirty/word-count) is unit-tested in `aterm-core` with no window and no filesystem.

# Out of scope

- The on-screen writing-surface widget, its centered layout, header, and caret (T-11.2).
- The shared frame/token re-skin (EPIC-9), sessions (EPIC-10), settings (EPIC-12).
- Syntax highlighting, LSP, multiple open buffers/tabs, or any IDE feature beyond a single calm markdown-ish writing surface.

# Notes

2026-07-01 (agent): The dirty-on-`esc` default (save / discard / keep-open) and the concrete open-a-file trigger surface are left for the implementing agent to choose a sensible default and record here for owner review; neither is fixed by the mock. Flag here (and set `status: ready-for-human`) if any implementation path would require a third `InputModel` mode, which ADR-0004 / ADR-0011 forbid.

2026-07-17 (agent): The owner approved `aterm <path>` as the explicit open trigger and
retaining unsaved document contents in memory when `Esc` returns to the terminal. The pure
`Document` module owns text, path, dirty state, and cached word count behind a small interface;
the app's `EditorSession` owns filesystem transport and `AppView` transitions without adding a
hypothetical storage seam. Entering editor makes the existing render callbacks omit the timeline,
raw grid, and unified input; `Esc` restores those same live sources without changing the
`InputModel` text or Shell/Agent mode. `Cmd-S` reports write failures and leaves the document
dirty. `mise run fmt`, `mise run lint`, `mise run build`, and `mise run test` pass.
