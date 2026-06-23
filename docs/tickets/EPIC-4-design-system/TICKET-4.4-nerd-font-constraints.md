---
id: T-4.4
epic: EPIC-4-design-system
title: Nerd Font per-codepoint constraint table
status: ready-for-agent
labels: [text, fonts, render]
depends_on: [T-1.6]
---

# Goal

Generate (or vendor) a per-codepoint constraint table for Nerd Font PUA glyphs so Powerline/icon glyphs scale/center/stretch to align in the monospace grid instead of looking small, squished, or off-cell.

# Context

- Research: [08-text-glyph-rendering.md](../../research/08-text-glyph-rendering.md) section 3 (Nerd Font specifics, Ghostty's `nerd_font_codegen.py` -> `getConstraint(cp)`) + Recommendation 8 (do not under-scope). Risk: real work; if vendoring Ghostty's generated data, license-check (Ghostty MIT, aterm GPLv3 - compatible to consume, verify).

# Implementation notes

- Crate: `aterm-ui` (text module) consumes the table; the table itself can be a generated Rust source or a data file under `aterm-ui` (or a small build step).
- Codepoint ranges: Powerline `E0A0-E0A2,E0B0-E0B3`; Powerline Extra `E0A3,E0B4-E0C8,E0CA,E0CC-E0D7,2630`; broad icon sets across BMP PUA `E000-F8FF` and Material Design Icons in SMP PUA `U+F0000+` (handle beyond-BMP codepoints).
- `getConstraint(cp)` returns scaling/positioning directives (e.g. `center1` = center within the first cell of a multi-cell glyph; span-both-cells; stretch). Drive double-width cell behavior with East Asian Width + Nerd Font width rules.
- Regenerate from the official Nerd Fonts patcher (à la Ghostty's codegen) OR vendor Ghostty's mapping under a verified-compatible license; document the source + license.

# Acceptance criteria

- Common Powerline separators (`E0B0`-`E0B3`) render full-cell-height and seamless.
- A sampling of icon glyphs across BMP and SMP PUA align centered/full-width per their constraint.
- Beyond-BMP codepoints (`U+F0000+`) resolve without panic.
- The table's provenance and license are documented.

# Out of scope

- Procedurally-drawn box/Powerline/braille (T-4.5) - those bypass the font entirely.
- Color emoji (deferred).
