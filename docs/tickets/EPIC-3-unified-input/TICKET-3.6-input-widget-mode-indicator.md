---
id: T-3.6
epic: EPIC-3-unified-input
title: Input box widget + iA mode indicator (prompt glyph + chip)
status: done
labels: [ui, input, design]
depends_on: [T-3.1, T-4.2]
---

# Goal

Render the single shell-first input box as a persistent bottom footer with the iA-restrained mode indicator: a prompt glyph + a small SHELL/AGENT chip carry the routing target; the caret stays the one accent blue in both modes. No banner, no second box.

# Context

- Research: [05-unified-input-ux.md](../../research/05-unified-input-ux.md) section 3 (mode indication, ranked) + Recommendation 6; [07-ia-design-language.md](../../research/07-ia-design-language.md) section 5 (Prompt component, routing-target indicator) and design-system.md sections 5 + 7. Owner open-question #5 (recolor caret vs chip-only): **default is glyph + chip with the caret staying `accent.primary` in both modes**; recoloring the caret per mode is the owner-confirm alternative - note an amber agent-mode caret would collide with the `caution` risk color, so it is not the default.

# Implementation notes

- Crate: `aterm-ui`. Module `input_widget`.
- Full-width input, `iM Writing Mono NFM`, `fg.primary`, thin 2px caret. Hairline above separating from the timeline; persistent bottom zone with `space.4` padding.
- Mode indicator: the caret stays `accent.primary` (blue) in BOTH modes. The mode is carried by the prompt glyph - Shell = `❯`-class glyph, Agent = `✦`/spark glyph - plus a small right-aligned SHELL/AGENT chip (`type.label`, `font.ui`). Optional secondary for color-blind reinforcement: caret shape (block vs underline) and placeholder text ("Type a command" / "Ask the agent").
- Toggle cross-fade <= motion.fast (90ms) - a single cheap interpolation, never a layout reflow (60fps floor).
- Render the highlight overlay + ghost text from T-3.5; render preedit from T-3.2.
- Consume tokens from `aterm-tokens` (T-4.1/T-4.2): `accent.primary`, caret colors, font names.

# Acceptance criteria

- The input renders at the bottom, edge-to-edge, with the correct fonts and hairline.
- Toggling mode visibly changes the prompt glyph + SHELL/AGENT chip within 90ms, no reflow, text preserved; the caret stays accent-blue.
- Ghost text renders as a muted gray tail; preedit renders inline during composition.
- The indicator is legible in both light "paper" and dark themes.
- No per-frame allocation introduced (T-1.8 assertion still passes).

# Out of scope

- The routing logic (T-3.3) and history (T-3.7).
- Final component spec doc (T-4.6).

# Notes

**Done 2026-06-30.** Landed as `aterm-ui/src/input_widget.rs` (the fourth front-end over
the shared `GlyphAtlas`, after grid/prose/timeline), wired through `Frame.input`
(`renderer.rs`), `UiCallbacks::input` (`app.rs`), `Session::input` (`aterm-app`, exposing
the `InputModel` it already owns + drives), and `gpu.rs` (which reserves the bottom zone
and shrinks the timeline viewport).

- **Bottom footer + hairline + zone** (AC1): full-width, `space.4` padding, a top hairline.
  The host reserves the zone via the standalone `input_widget::zone_px`, so the timeline
  lays out above it and the two never overlap; the atlas viewport uniform stays the full
  surface size. The box is hidden in alt-screen (a full-screen app owns input). Because the
  timeline (not the raw grid) is the primary view, the box is the single on-screen home of
  the live command line - no double echo - so agent-mode typing, previously invisible, now
  shows.
- **Mode indicator** (AC2): a mode-carrying prompt glyph (`❯` Shell / the `nf-md-creation`
  "sparkles" Nerd-Font icon for Agent) + a right-aligned SHELL/AGENT chip (neutral / accent
  `Info`, via `components::PromptChip`). The caret stays one accent blue in both modes
  (an amber agent caret would collide with `caution`). The chip sits in a FIXED-WIDTH slot
  (the wider of the two labels) and the text origin is fixed, so a toggle swaps only the
  glyph + chip with the typed text preserved and NO reflow.
- **Ghost / preedit / highlight / placeholder / selection** (AC3): the fish ghost tail
  (muted), an inline preedit underline, the syntax overlay (a restrained token-only
  `SpanKind` mapping), an empty-buffer per-mode placeholder, and a selection background -
  all through the shared `cell_render::emit_cell` (Mono); the Quattro chip label shapes
  through `prose::ProseShaper` into the same glyph buffer. A per-line horizontal scroll
  keeps the caret visible on a long line; a vertical window over `MAX_INPUT_ROWS` keeps the
  caret line visible in a multi-line paste.
- **Both themes** (AC4) + **no per-frame alloc** (AC5): the front-end is one rect + one
  glyph draw, damage-gated by an FNV signature over everything drawn, so an idle present
  allocates nothing (proven by `unchanged_input_skips_rebuild_alloc_free`). 11 pure layout
  tests run on every platform; 3 GPU tests (macOS) cover prompt/text/caret ink in both
  themes AND both modes + the single-glyph-draw + the zero-alloc gate. A cross-platform
  `prompt_glyphs_exist_in_the_bundled_grid_font` guard asserts both prompt glyphs resolve
  to a non-`.notdef` gid (a review caught U+2726 `✦` being absent from the Mono Nerd Font).

**Residuals (not regressions - dependency-gated / owner-watched):**
- The `motion.fast` chip **cross-fade** (`components::Animation::CrossFade`) is the spec but
  is NOT yet time-driven: no frame clock is plumbed into a `Frame` (the timeline's running
  pulse / block-insert / focus-dim are all spec'd-not-live for the same reason). The swap is
  instant + reflow-free today (trivially within 90ms); a live alpha cross-fade is a small
  follow-up once a frame-time input lands - a shared "motion runtime" concern, not this
  widget's.
- The **mode-toggle hotkey + routing** (T-3.3), **history** (T-3.7), the **async
  highlight/ghost worker** (T-3.5), and the **IME preedit feed** (T-3.2) populate what this
  renders; until they land those overlays are simply empty (the box still shows the plain
  line). Today the host toggles mode on a Tab placeholder (T-3.3 owns the real chord).
- **AC: on-hardware iA visual review** on real input in both themes is the owner-watched
  acceptance step (this crate's "GPU/window code is owner-watched" convention); the render
  path is offscreen GPU-tested.

**Discovered (out of scope - flag for a follow-up, NOT T-3.6):** the already-landed timeline
gutter markers use BMP geometric glyphs (`●` U+25CF, `○` U+25CB, `◐` U+25D0, `▸` U+25B8) that
are NOT in the bundled Mono Nerd Font (only `✓` U+2713 is) - they resolve to `.notdef`, so
running / failed / unknown / approximate / interactive gutters likely render as identical
`.notdef` boxes on screen (the T-4.6 GPU test only asserts "any ink", which a `.notdef` box
satisfies). A dedicated fix should swap them for present (PUA) glyphs or sprite-render them,
and add the same non-`.notdef` gid guard this ticket introduced.
