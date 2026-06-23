# ADR-0009: Text and glyph pipeline - cosmic-text/swash/glyphon, GPU atlas, grayscale AA, ASCII grid fast-path

## Status

Accepted

## Context

aterm renders two text registers in one scene: a constant-advance monospace terminal grid
and proportional prose/chrome for the agent transcript and UI. The custom-wgpu render-stack
decision ([ADR-0002](0002-render-stack.md)) means aterm owns the text pipeline; the dossier
([02-render-stack-eval.md](../research/02-render-stack-eval.md),
[09-performance-60fps.md](../research/09-performance-60fps.md)) established that the text path
is the same regardless of framework: a GPU glyph atlas + alpha-only mask + a single instanced
draw call, with CoreText rasterization on macOS, grayscale AA only, and one shaping engine
with two layout front-ends. aterm bundles the iM Writing Nerd Font, whose private-use-area
(PUA) glyphs are a known misalignment hazard ([12-licensing.md](../research/12-licensing.md),
fonts decision).

## Decision

- **Text stack: `cosmic-text 0.19` + `swash` + `glyphon`** on the wgpu surface (with `parley`
  available for richer proportional-prose layout/editor/IME/AccessKit where needed). One
  shaping engine, two layout front-ends: a constant-advance grid front-end for the terminal,
  a proportional front-end for prose/chrome.
- **GPU glyph atlas:** rasterize each glyph once into a GPU texture atlas, cache by
  `(glyph id, size, weight, subpixel-offset)`, store **alpha only**, and apply color in the
  shader via multiply. Draw the whole grid as one instanced draw call of textured quads.
- **Grayscale AA only** (no subpixel/LCD RGB AA) - matches the iA aesthetic and keeps the
  atlas alpha-only and theme-agnostic.
- **ASCII grid fast-path:** for plain ASCII runs in the terminal grid, **skip shaping
  entirely** and look glyphs up directly by codepoint at the constant advance. Shaping is
  reserved for non-ASCII, combining, wide, and proportional runs.
- **Fonts (locked):** bundle **iM Writing Nerd Font** (OFL 1.1, IBM-Plex-based). Use the
  **Mono Nerd Font Mono (NFM, constant advance)** variant for the terminal grid; the **Duo**
  and **Quattro** proportional variants for UI chrome and agent prose. Load OFL fonts via
  `ATSApplicationFontsPath`. Bundle the Duo/Quattro variants (only Mono is currently vendored).
- **Nerd Font PUA constraint table:** generate a per-codepoint constraint table (from the Nerd
  Fonts patcher data) so PUA glyphs (Powerline, box-drawing, braille, icons) align to the grid
  cell instead of mis-advancing. Pair with a built-in sprite face for box-drawing/Powerline/
  braille where exact cell alignment matters most.

## Consequences

- The single instanced draw call + alpha-only atlas keeps the GPU-side frame-build cheap, a
  direct contributor to the 60fps floor; per-frame glyph lookups are O(1) against the atlas
  with zero per-frame allocation.
- The ASCII fast-path removes shaping cost from the dominant terminal workload (plaintext
  scroll), which the perf research flags as the case to optimize for.
- Grayscale-only AA simplifies the atlas and matches the design language, at the cost of
  slightly less crisp text than LCD subpixel AA on some displays - an accepted, deliberate
  aesthetic trade.
- The Nerd Font PUA constraint table is real, easily under-scoped work; without it, PUA glyphs
  misalign the grid. It is a named v1 deliverable, generated rather than hand-authored.
- `cosmic-text` is pre-1.0; pin it. If proportional-prose layout, editing, or AccessKit text
  properties outgrow what `cosmic-text` gives the transcript, `parley` is the in-stack upgrade
  path without changing the atlas/renderer contract.
- CJK rendering and IME quality on this path are unverified and must be spiked early
  ([ADR-0004](0004-unified-input-model.md) covers the IME side).

## Alternatives considered

- **GPUI's built-in CoreText glyph path.** Moot under [ADR-0002](0002-render-stack.md) (GPUI
  not adopted); it would have been CoreText underneath anyway, and choosing custom keeps the
  terminal grid fast-path fully under our control.
- **Subpixel/LCD RGB anti-aliasing.** Rejected: it complicates the atlas (RGB coverage, not
  alpha), is background-color dependent, and conflicts with the near-monochrome iA aesthetic.
- **Shaping every run (no ASCII fast-path).** Rejected: shaping plain ASCII is wasted cost on
  the dominant terminal workload; the constant-advance grid makes a direct codepoint lookup
  correct and far cheaper.
- **A system/non-bundled font.** Rejected: bundling iM Writing Nerd Font (OFL 1.1) guarantees
  the exact three-register look across machines and is license-clean for a GPLv3 app; the Mono
  NFM constant advance is required for the grid.
