---
id: T-11.2
epic: EPIC-11-editor-mode
title: Editor writing-surface widget (centered prose editing)
status: ready-for-agent
labels: [ui, editor, design]
depends_on: [T-11.1, T-9.1]
---

# Goal

Draw the editor view from the vision mock: a calm, centered writing surface (max ~620px wide) with a mode-colored caret and a quiet header, backed by the document model from T-11.1 and styled with the reconciled tokens from T-9.1. Text editing reuses the existing input-box editing infrastructure rather than reinventing a text editor.

# Context

- Visual source of record: [`docs/design/vision-mock/AtermWindow.dc.html`](../../design/vision-mock/AtermWindow.dc.html), `screen="editor"`: a header row with the filename + "edited" state on the left and "markdown · N words · ⌘S save · esc to shell" on the right, separated from the body by a `hairline` rule; below it a centered `textarea` (`max-width:620px`, prose type ~16px / line-height 1.9, transparent background, `caret-color` = the mode accent), the timeline and input bar hidden.
- Decision of record: [ADR-0011](../../adr/0011-vision-mock-ui-north-star.md) (editor as a first-class view; the warm palette + mode-tinted caret). Tokens: T-9.1 (reconciled `fg.primary`, `hairline`, `bg.canvas`, `accent`/`agent` mode accent, prose font).
- Research: [07-ia-design-language.md](../../research/07-ia-design-language.md) (centered measure, generous leading, distraction-free surface). [05-unified-input-ux.md](../../research/05-unified-input-ux.md) + the shipped input box (T-3.2 IME, T-3.6 input widget) - the editing infra to reuse.

# Implementation notes

- Crate: `aterm-ui`. Add an editor writing-surface widget that renders the T-11.1 document model. Consume `aterm-tokens` only - no hardcoded hex.
- **Reuse, do not reinvent, the editing core.** Cursor movement, selection, multi-line editing, and IME (winit `Ime` preedit, T-3.2) already exist for the input box. Factor the shared editing behavior so the writing surface and the input box use the same text-editing core rather than a second implementation; note the reuse point explicitly in the code. The difference is presentation (centered, wide measure, prose type) and that Enter inserts a newline here (no routing/submit).
- **Layout:** centered column, `max-width` ~620px, prose font at the editor type size (the mock uses ~16px / 1.9); `fg.primary` text on `bg.canvas`; caret = the current mode accent (`--mode`: shell accent, or agent accent if in Agent - the caret-tint rule from ADR-0011). The header row: filename (`fg.primary`) + " · edited" (dim when dirty) on the left; "markdown · N words · ⌘S save · esc to shell" in `fg.faint` on the right; a `hairline` rule under it.
- **Perf:** hold the 60fps floor and the zero-per-frame-allocation invariant (T-1.8). The editor is a low-churn surface; drive it through the same damage-gated present path as the timeline, no busy redraw. Cap prose at the measure column; do not introduce a per-keystroke full reshape (the T-1.8 cure must hold here too).
- The word count in the header is fed by the T-11.1 model; this widget only renders it.

# Acceptance criteria

- [ ] The editor view renders a centered (~620px max) writing surface with the header row to spec in both themes; no hardcoded colors (all via T-9.1 tokens).
- [ ] The caret tints to the active mode accent (shell vs agent), per ADR-0011.
- [ ] Typing, cursor movement, selection, and IME preedit work in the surface via the shared editing core (not a duplicate implementation); a test/reference documents the reuse.
- [ ] The header shows the filename, dirty/"edited" state, and live word count from the T-11.1 model.
- [ ] No frame-budget regression: the T-1.8 no-per-frame-allocation assertion holds while editing (no per-keystroke full reshape).

# Out of scope

- File open/save, dirty tracking, word-count computation, and the enter/exit transitions (T-11.1).
- Syntax highlighting, markdown preview/rendering, LSP, multi-buffer tabs.
- Sessions (EPIC-10), settings (EPIC-12), and the shared frame/token work (EPIC-9).
