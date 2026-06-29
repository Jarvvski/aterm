---
id: T-4.5
epic: EPIC-4-design-system
title: Sprite face for box-drawing/Powerline/braille
status: done
labels: [text, fonts, render]
depends_on: [T-1.6]
---

# Goal

Draw box-drawing, block elements, Powerline separators, braille, and "Symbols for Legacy Computing" procedurally into the atlas (a built-in sprite face), so they are pixel-perfect and seamless regardless of font - removing a whole class of misalignment bugs.

# Context

- Research: [08-text-glyph-rendering.md](../../research/08-text-glyph-rendering.md) section 3 (Ghostty draws these in `src/font/sprite/Face.zig` via a canvas: box/line/fill/trim) + Recommendation 7. Cheap (drawn once into the atlas).

# Implementation notes

- Crate: `aterm-ui`. Module `text::sprite`.
- For the relevant Unicode ranges (box-drawing `U+2500-257F`, block elements `U+2580-259F`, braille `U+2800-28FF`, Powerline `E0Bx`, legacy-computing symbols), generate the glyph procedurally (draw lines/fills/arcs on a per-cell canvas sized to the grid cell, then rasterize into the alpha atlas) instead of using the font outline.
- A lookup decides sprite-face vs font-glyph per codepoint; sprite glyphs are cached in the atlas like any other.
- Seamlessness: box-drawing must tile with no gaps/overlaps at the cell boundary across the grid.

# Acceptance criteria

- A box-drawing TUI (e.g. a table, `tmux`-style borders) renders seamless with no inter-cell gaps on both themes.
- Powerline separators from the sprite face are pixel-identical regardless of which font is active.
- Braille patterns (e.g. a `btop`/spinner fixture) render correctly.
- Sprite glyphs are atlas-cached (no re-rasterization per frame).

# Out of scope

- Nerd Font PUA icon constraints (T-4.4) - those use the font, scaled.
- Color glyphs.

# Notes

**Landed 2026-06-29.** New pure module `aterm-ui::sprite` draws the sprite ranges into
an 8-bit alpha coverage bitmap sized to the cell, returning the SAME
`crate::glyph::RasterGlyph` swash produces, so the existing atlas + `GlyphCache` +
instanced pipeline consume a sprite exactly like a font glyph (drawn once, cached,
never re-rasterized - AC4). `is_sprite(ch)` / `render(ch, w, h)` are the surface.

Coverage (drawn):

- **Box-drawing** via a uniform 4-arm model (`box_arms(ch) -> [up,down,left,right]`
  weights; `draw_box` draws each arm as a band from edge to centre, opposite arms
  union at the centre). Straight lines, all corners (light/heavy/mixed-corner), the
  pure-weight T/cross junctions, and the half-lines (light + heavy + the mixed
  collinear half-lines). A horizontal/vertical line inks BOTH edge rows/cols, so
  adjacent cells abut with no gap (AC1, the seamless property, asserted headlessly).
- **Block elements** `U+2580..259F`: half/eighth blocks, quadrants, and the three
  shades. All vertical/horizontal divisions snap to ONE shared rounded-1/8 gridline
  set, so complementary glyphs (▀/▄, the quadrants) tile with no seam even at odd
  cell heights (the default 1x height is 17, odd - this was a review fix).
- **Braille** `U+2800..28FF`: the full 256-pattern 2x4 dot matrix (algorithmic from
  the dot-mask) - AC3 (btop/spinner). Blank braille `U+2800` returns an empty glyph
  the pipeline skips (review fix).
- **Powerline** `U+E0B0..E0B3`: the filled/outline left/right triangles, procedural
  and therefore font-independent and pixel-identical (AC2).

Deliberate scope cut (deferred to the FONT path, `classify` returns `None`, no
regression): the mixed light/heavy box junctions, the double-line set
(`U+2550..256C`), arcs (`U+256D..2570`), diagonals (`U+2571..2573`), and dashes.
These render via the normal glyph path exactly as before. The straight-line set
covers tables / tmux borders (AC1); the deferred set is rarer and its per-arm weights
/ corner-radius math were judged not worth the table-error risk for this unit.

Integration: a sprite is keyed in the atlas by its CODEPOINT in `GlyphKey.glyph_id`
plus a NEW `sprite: bool` discriminant (so it can't collide with a font glyph-id of
the same numeric value); `place_glyph` (refactored out of `rasterize_into_atlas`) is
shared by the font and sprite paths. A sprite is placed at the cell-box origin
(`round(cell_x), round(cell_y)`), not baseline-relative.

AC coverage: (1) seamless box tiling - the per-sprite edge-coverage property is proven
headlessly; the sub-pixel cumulative-tiling drift across a long row at fractional cell
widths is the on-hardware residual (consolidated to EPIC-7, per the INDEX convention).
(2) Powerline font-independent + (3) braille correct + (4) atlas-cached - all proven by
unit tests PLUS a GPU render-to-texture test (`sprite_glyphs_render_through_the_atlas_
pipeline`) that composites a full-block and a line on real Metal.

Adversarial review (4 lenses: box-table, block/braille/Powerline, integration,
geometry/AC; find -> default-refute verify; 7 findings, 2 confirmed, both LOW, both
fixed): (1) blank braille emitted a cached transparent quad instead of being skipped -
`render` now returns a truly-empty glyph when nothing inks; (2) the half-block midline
disagreed with the quadrant/eighth midline by 1px at odd heights - all block divisions
now share one rounded-1/8 gridline set (regression test
`half_blocks_and_quadrants_share_one_midline_at_odd_height`). The box-arm weight table
and the braille bit-to-dot mapping were both verified correct against the Unicode
names/numbering (5 findings refuted).

aterm-ui: 99 tests (17 sprite + 1 atlas-collision + 1 GPU); full workspace green;
clippy `-D warnings` clean. CHANGELOG entry under `## Unreleased`; no version bump.
