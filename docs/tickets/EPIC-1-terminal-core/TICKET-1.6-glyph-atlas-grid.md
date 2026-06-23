---
id: T-1.6
epic: EPIC-1-terminal-core
title: Glyph atlas + monospace grid fast-path (cosmic-text/swash)
status: ready-for-agent
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
