---
id: T-4.2
epic: EPIC-4-design-system
title: aterm-tokens - two ANSI-16 palettes + theme switching
status: done
labels: [tokens, design, render]
depends_on: [T-4.1]
---

# Goal

Add the two theme-tuned ANSI-16 color palettes (light "paper" + dark) to `aterm-tokens`, and wire runtime theme switching into the grid renderer so terminal output and aterm's own UI share one hue family.

# Context

- Research: [07-ia-design-language.md](../../research/07-ia-design-language.md) section 3 (ANSI 16-color tables per theme). Risk: ANSI tuning is hand-derived taste - must be eyeballed against real `ls --color`/vim/htop/git diff on both themes before locking; light "paper" + bright ANSI is the riskiest combo (bright cyan/yellow near-invisible on light bg). Owner open-question #4 (honor terminal-app OSC palette overrides vs enforce theme).

# Implementation notes

- Crate: `aterm-tokens` (the palettes) + `aterm-ui` (apply to the grid via `Colors`).
- Encode both ANSI-16 tables verbatim from the dossier (indices 0-15, normal + bright), per theme. On a light bg, ANSI "white" maps to dark text and "black" to darkest (standard light-terminal convention).
- Feed the palette into `alacritty_terminal`'s `Colors` so the grid renderer resolves ANSI indices through the theme palette.
- Theme switching: a runtime toggle (and a first-launch default - owner open-question #1: follow macOS appearance vs default "paper"; default to following system appearance, flag if unconfirmed). Switching re-resolves colors without reallocating the grid.
- Decide OSC palette override policy: default to honoring DECSCUSR/OSC palette requests but provide an "enforce aterm theme" setting (owner open-question #4).

# Acceptance criteria

- Both ANSI palettes are exposed and selected by `Theme`.
- Rendering `ls --color`, a `git diff`, and an htop-like fixture looks correct on both themes (manual eyeball noted in the PR; the dossier flags this as required-before-lock).
- Switching theme at runtime updates grid colors live with no realloc and within the frame budget.
- A bright-cyan/bright-yellow run is legible on the light "paper" bg (the riskiest combo) - if not, document the needed saturation boost/remap.

# Out of scope

- The semantic UI tokens (T-4.1).
- Fonts (T-4.3).

# Notes

**Landed 2026-06-29.** Much of the plumbing already existed in the scaffold (the
`LIGHT`/`DARK` themes incl. full ANSI-16 tables + `Theme::for_kind` landed with T-4.1;
`aterm-ui` already resolved `CellColor` against the active theme in `text.rs` and
already threaded `theme: Theme` per-frame with a `theme_signature` rebuild gate). This
ticket closed the genuine gaps: a centralized 256-color resolver, a runtime theme
switch, and the light-"paper" bright-color legibility remap.

What changed (three files; tokens CONST values untouched):

- `aterm-tokens` (pure, leaf): `AnsiPalette::indexed(idx)` resolves the full xterm
  256-color space in one home (themed 0-15 via `by_index`, the standard 6×6×6 cube
  16-231, the 24-step grayscale ramp 232-255); `legible_against(fg,bg,min_ratio)` +
  `with_fg_legibility(bg,min_ratio)` are the pure legibility-remap primitives;
  `ThemeKind::toggle()` for the runtime flip.
- `aterm-ui/text.rs`: `resolve_indexed` now delegates to `AnsiPalette::indexed` (DRY;
  the cube/grayscale arithmetic moved to the leaf crate where it is unit-tested).
- `aterm-ui/app.rs`: `effective_theme(kind)` is the theme the renderer actually draws -
  it applies the legibility remap; `set_theme`/`toggle_theme` switch at runtime;
  `WindowEvent::ThemeChanged` + `Window::theme()` follow the OS appearance (opt-in via
  `with_follow_system`).

AC coverage:

1. **Both palettes exposed + selected by `Theme`** - met (resolution goes through
   `theme.ansi`; both themes resolve distinctly, asserted in
   `ansi_output_resolves_through_the_active_theme_palette`).
2. **Looks correct on both themes** - the resolution is proven headlessly (git-diff
   red/green, ls dir-blue/symlink-cyan, indexed bright-yellow resolve to the right
   per-theme palette colors). The literal "eyeball `ls --color`/`vim`/`htop`/`git
   diff` on real hardware" is the on-hardware visual residual (consolidated into the
   EPIC-7 / on-hardware pass per the INDEX convention, not parked on a human).
3. **Runtime switch, live, no realloc** - `set_theme`/`toggle_theme` +
   `ThemeChanged`; the snapshot is unchanged, so the renderer's `theme_signature`
   rebuild gate re-resolves each cell into its EXISTING instance buffers
   (`theme_switch_reuses_buffer_and_re_resolves_colors` proves no realloc at the pure
   layer; `theme_signature_pins_the_effective_palette_the_renderer_draws` proves the
   gate invalidates on the effective palette the renderer draws).
4. **Bright cyan/yellow legible on light "paper"** - the verbatim dossier values fail
   a 3:1 floor (bright_cyan ~2.8:1, bright_yellow ~2.5:1). Per `design-system.md` §3
   the fix is an **output-time renderer remap, not a token edit**, so `effective_theme`
   pulls every light ANSI fg up to >=3:1 against the canvas (the WCAG large-text/UI
   bar - the right one for decorative monospace output; a full 4.5:1 body bar would
   over-darken and invert the bright>normal ordering). Gated on a light background
   (`bg_canvas` luminance > 0.5) so the dark theme's intentionally-dim slots are never
   lifted. The remap is documented as the dossier's anticipated contingency.

Decisions flagged (not relitigated):

- **OQ#1 (first-launch default: follow-system vs paper)** is unconfirmed and the
  scaffold's `aterm-app` config already defaults to Light "paper". The follow-OS
  machinery is implemented and ready (`with_follow_system`), but the shipped default
  stays the configured theme (follow-system OFF) to avoid relitigating an unconfirmed
  owner question. Enabling it (or a config knob) is the path to the ticket's stated
  "follow system appearance" default - revisit when OQ#1 is answered (T-8.3 config).
- **OQ#4 (honor OSC palette overrides vs enforce the theme)** is unaddressed by this
  ticket; the engine does not yet surface OSC-4/OSC-104 palette overrides to the
  renderer, so there is nothing to honor or enforce yet. Revisit when palette-override
  parsing lands.
- The 256-color CUBE (16-231) and truecolor are NOT remapped on light bg - only the
  16 ANSI slots. An app emitting 256-color cube cyan rather than ANSI bright-cyan is
  an explicit color choice and out of the "ANSI bright" risk the dossier names;
  remapping explicit choices was judged wrong. Documented, not a gap.

Adversarial review (4 lenses: color-math, switch-flow, design-fidelity, test-quality;
find -> default-refute verify; 9 findings, 3 confirmed - all LOW, all test-quality,
no correctness defects): (1) the rebuild-gate guard test pinned the RAW light theme,
not the effective (remapped) palette the renderer draws - added
`theme_signature_pins_the_effective_palette_the_renderer_draws`; (2) the low-16
`indexed` tests compared `indexed` to `by_index` (tautological) - re-anchored to
ground-truth dossier hex; (3) the legibility "direction" assertion was redundant with
the contrast check - added per-channel endpoint assertions that pin endpoint selection
independently. The 6 refuted findings were correctly refuted (the binary-search
correctness was confirmed sound - a clarifying comment on the single-threshold
predicate was added; the cube-bypass and follow-system default are documented scope
decisions; the em-dashes flagged were pre-existing, not introduced here).

No version bump: changes accrue under `## Unreleased` in `CHANGELOG.md` (version of
record stays `0.1.0`). Tests: aterm-tokens 14, aterm-ui 81 (incl. 7 new T-4.2 tests);
full workspace green; clippy `-D warnings` clean.
