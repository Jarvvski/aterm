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
upstream `iA-Writer.zip` and can be added when a role needs them.

The faces are embedded into the binary at compile time via `include_bytes!` in
`crates/aterm-ui/src/fonts.rs`, then loaded into the cosmic-text `FontSystem`.
