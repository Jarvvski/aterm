# Bundled fonts — iM Writing Nerd Font

These faces are **iM Writing Nerd Font**, a Nerd-Fonts patch of iA Writer's
typefaces (themselves based on IBM Plex). They are licensed under the **SIL Open
Font License 1.1** (see `OFL-LICENSE.md`), which permits bundling and
redistribution inside an application - including a GPLv3 app like aterm - as long
as the OFL notice travels with the fonts. That is why they are vendored here
rather than relying on the user having them installed: the terminal grid font
must always be present.

## Grid vs. prose split

aterm uses two font registers (mirrored in `docs/design/tokens.toml` and
reified in `aterm-tokens::font`):

- **Grid (monospace, mandatory):** `iM Writing Mono Nerd Font Mono`. Constant
  advance width - the terminal grid, command echo, code, and diffs render in
  this. Faces vendored: Regular, Bold, Italic, BoldItalic
  (`iMWritingMonoNerdFontMono-*.ttf`).
- **Prose (proportional):** `iM Writing Duo` for agent prose / transcript body
  (`iMWritingDuoNerdFont-Regular/Bold.ttf`).
- **UI / chrome (proportional):** `iM Writing Quattro` for the dense status
  strip, chips, and command palette (`iMWritingQuatNerdFont-Regular/Bold.ttf`).

Only the faces aterm actually wires up are vendored; the full pack (Italic/
BoldItalic for Duo/Quattro, the non-Mono Mono variants, etc.) lives in the
upstream `iA-Writer.zip` and can be added when a role needs them. All three
registers are wired (T-4.3): the grid front-end (`crate::grid_render`) draws Mono,
and the prose front-end (`crate::prose`) shapes Duo/Quattro - both through one
shared glyph atlas (`crate::atlas`). Duo/Quattro ship Regular + Bold only, so a
synthetic Italic falls back to Regular and BoldItalic to Bold.

The faces are embedded into the binary at compile time via `include_bytes!` in
`crates/aterm-ui/src/fonts.rs` and rasterized / shaped directly with **swash**
(no `FontSystem` indirection); `crate::fonts::face_bytes` is the single
`(family, face) -> bytes` router both the rasterizer and the prose shaper use.

## Measured metrics (T-4.3)

Measured from the bundled `*-Regular.ttf` with swash (`units_per_em = 1000`).
**Vertical metrics are identical across all three registers** - `ascent 1025,
descent 275, leading 0, cap_height 698, x_height 516` - so prose and grid share
one baseline geometry. Advance widths, in em-fractions (`advance / units_per_em`):

| glyph        | Mono  | Duo   | Quattro |
|--------------|-------|-------|---------|
| space        | 0.667 | 0.600 | 0.450   |
| i, l         | 0.667 | 0.600 | 0.300   |
| r, f         | 0.667 | 0.600 | 0.450   |
| s, a, 0, n   | 0.667 | 0.600 | 0.600   |
| m, w, M, W   | 0.667 | 0.900 | 0.900   |
| average      | 0.667 | 0.874 | 0.873   |

So **Mono** is a constant 0.667em (the grid's column invariant), **Duo** is
duospace (0.6em, with m/w/M/W at 0.9em = 1.5x), and **Quattro** spans four widths
(0.3 / 0.45 / 0.6 / 0.9em). These values are asserted directly against the live
faces by `crate::prose`'s `prose_metrics_match_the_documented_table` test, so the
table cannot silently drift from the bundle.

**Prose measure.** Agent prose wraps at a measure of `MEASURE_CH` (72) characters,
where a character is the CSS `ch` unit - the advance of `'0'` (0.600em in Duo). The
terminal grid is never capped; it follows the PTY column count.

## Licensing (OFL 1.1)

The patched iM Writing set carries two upstream licenses - the Nerd Fonts patch and
iA's IBM Plex modification - both released under the **SIL Open Font License 1.1**,
reproduced in `OFL-LICENSE.md`. The OFL permits bundling/redistribution inside a
GPLv3 application as long as that notice travels with the fonts (it does, here). The
in-app acknowledgements surface is tracked separately (EPIC-8 / T-8.2).
