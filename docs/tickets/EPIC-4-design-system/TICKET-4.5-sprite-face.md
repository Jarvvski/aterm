---
id: T-4.5
epic: EPIC-4-design-system
title: Sprite face for box-drawing/Powerline/braille
status: ready-for-agent
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
