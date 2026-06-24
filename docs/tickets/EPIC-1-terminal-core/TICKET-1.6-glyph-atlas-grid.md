---
id: T-1.6
epic: EPIC-1-terminal-core
title: Glyph atlas + monospace grid fast-path (cosmic-text/swash)
status: ready-for-human
labels: [ui, render, text]
depends_on: [T-1.5]
---

# Goal

Render the terminal grid: a GPU glyph atlas (alpha-only mask, color by multiply), a single instanced draw call per frame, and an ASCII fast-path that skips shaping for plain single-width runs. This is the text pipeline the whole UI sits on.

# Context

- Research: [08-text-glyph-rendering.md](../../research/08-text-glyph-rendering.md) sections 1-2, 5 and Recommendations 1-6, 10. Grayscale AA only (macOS disabled subpixel AA in 2018); CoreText rasterization on macOS with swash as the portable fallback.

# Implementation notes

- Crate: `aterm-ui`. Module `text`.
- Dependencies: `cosmic-text = "0.19"`, `swash = "0.2"`, `glyphon = "0.9"` (or build the atlas directly via glyphon's etagere). Pin exactly.
- Atlas: rasterize each unique (glyph, size, subpixel-offset) once; store 8-bit alpha; composite color by multiply so ANSI/theme colors don't multiply atlas storage. Reserve a separate BGRA atlas slot for color glyphs (defer color emoji population to a later ticket unless trivial).
- One shaping engine (HarfRust via cosmic-text), two front-ends: a constant-advance grid layout (this ticket) and a proportional `Buffer` path (used later by agent prose / T-2.7, T-3.6). Share atlas/shader/FontSystem.
- ASCII grid bypass: plain single-width runs map codepoint->glyph at constant advance, skipping the shaper. Cache shaped runs per line/block (`ShapeRunCache`) for the non-ASCII path.
- Load the bundled `iM Writing Mono NFM` Regular/Bold/Italic/BoldItalic from `resources/fonts/` (Duo/Quattro added in T-4.3). Honor cell metrics from the Mono face.
- Subpixel-position variant count: for a constant-advance grid most glyphs land on integer origins; measure the right variant count (far fewer than GPUI's 16 for proportional). Document the measured number.

# Acceptance criteria

- The grid renders a snapshot from `aterm-core` correctly: ASCII, bold/italic, fg/bg, wide (CJK) cells occupy two columns, basic ligature line (e.g. `=>`) shapes on the non-fast-path.
- A plain ASCII line provably takes the fast-path (no shaper call - assert via a counter/trace).
- The full visible grid draws in a single instanced draw call (assert draw-call count == 1 for glyph layer).
- Grayscale AA only; no LCD subpixel path.
- Atlas is built once and reused across frames for repeated glyphs (assert no re-rasterization of a cached glyph).

# Out of scope

- Nerd Font PUA constraint table (T-4.4) and sprite face (T-4.5) - plain PUA glyphs may look misaligned until then.
- Duo/Quattro proportional faces (T-4.3).
- Block/timeline virtualization (T-2.7).
- Damage-driven redraw decisions (T-1.8).

# Notes

2026-06-24 (agent): Landed the GPU-free HALF of the text pipeline; status ->
`ready-for-human` because the actual rendering needs a GPU/device and on-screen
verification (the same owner-watched render pass as T-1.5).

**What landed (new `aterm-ui::text` module, dep-free, 18 unit tests):**

- `resolve_color(CellColor, &Theme, is_fg)` - VT cell color -> concrete `Rgba`:
  ANSI-16 via the themed palette, the xterm 256-color cube (16..=231) and 24-step
  grayscale ramp (232..=255), true color, and the semantic Named slots. The Named
  discriminants were verified against `vte` 0.15's `NamedColor` (Foreground=256,
  Background=257, Cursor=258, DimBlack=259..DimWhite=266, BrightForeground=267,
  DimForeground=268) - dim ANSI maps onto dimmed ANSI 0..7, bright/dim-fg onto the
  theme's primary/muted text. (Note: the `CellColor::Named` doc comment in
  `aterm-core/terminal.rs` is off-by-one on this range; the code here matches the
  real enum.)
- `build_grid_cells(&Snapshot, &Theme, &mut Vec<GridCell>)` - flattens the grid into
  per-cell instances (the GPU's input): resolved fg/bg, inverse applied (swap),
  wide glyphs flagged, the trailing wide-spacer dropped, the true grid column
  preserved. Reuses the buffer (zero-alloc steady state, T-1.5 discipline).
- `is_ascii_fast` / `classify_run` - the ASCII fast-path DECISION (plain
  single-width printable ASCII -> constant advance, no shaper).
- `ShelfAllocator` (glyph-atlas rect packer) + `GlyphCache` keyed by
  `(glyph_id, face, px)` with a `rasterizations()` counter realizing AC5's
  once-only LOGIC. Subpixel-variant count is documented as **1** for a
  constant-advance grid (glyphs land on integer cell origins) - far fewer than the
  ~16 a proportional layout needs.

Adversarial review (2 lenses x skeptic verify, 13 findings) confirmed every formula
by hand (cube/grayscale = exact xterm; no overflow; Named map = exact vte) and found
**no defects**.

**Pending the owner / a GPU device (the `ready-for-human` items) - cannot be done or
verified headlessly:**

1. **The renderer is not yet wired to this module.** `gpu.rs` still draws text via
   `glyphon`; nothing consumes `GridCell`/`GlyphCache` yet. Replacing the glyphon
   path with the custom atlas + instanced pipeline is the GPU work below.
2. **ACs needing the GPU/shader/visual pass (a, c, d, and AC2's assertion):** swash
   /CoreText rasterization into the alpha atlas texture; the wgpu instanced pipeline
   + grayscale composite-by-multiply shader; the single-instanced-draw-call
   submission (AC c); grayscale-AA-only (AC d); correct on-screen rendering of
   ASCII/bold/italic/fg/bg/wide/ligatures (AC a). For AC2, the fast-path DECISION is
   realized + tested here, but "assert no shaper call via a counter" needs the real
   shaper integration to count against - deferred to that pass, not claimed met here.

`fmt`/`clippy`/full-workspace `build`/`test` green (aterm-ui 33 tests, +18); no new
dependencies (pure logic over `aterm-core` + `aterm-tokens`).
