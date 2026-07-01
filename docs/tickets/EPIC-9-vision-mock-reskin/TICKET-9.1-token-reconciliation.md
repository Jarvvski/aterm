---
id: T-9.1
epic: EPIC-9-vision-mock-reskin
title: Reconcile tokens to the vision mock - warm palette + agent second-accent + elevated surface
status: done
labels: [tokens, design, ui]
depends_on: []
---

# Goal

Rewrite `aterm-tokens` (and its two mirror docs) to the imported vision mock's
warm two-theme palette, add the Agent-mode second accent and the elevated-surface
color the mock relies on, and re-validate contrast. This is the keystone of the
re-skin: every other EPIC-9/10/11/12 ticket resolves colors through these tokens,
never hardcoded hex.

# Context

- North star: [ADR-0011](../../adr/0011-vision-mock-ui-north-star.md) - adopt the
  imported mock as authoritative; the mock wins where it disagrees with the old
  `design-system.md`. Visual source of record:
  [`docs/design/vision-mock/AtermWindow.dc.html`](../../design/vision-mock/AtermWindow.dc.html)
  (the `.aw[data-theme="dark"|"light"]` custom-property blocks).
- Docs to keep in lockstep: [`docs/design/tokens.toml`](../../design/tokens.toml)
  (values, source of truth) and [`docs/design/design-system.md`](../../design/design-system.md)
  (intent). ADR-0011 resolves the "OWNER CONFIRMATION REQUIRED" accent note.
- Crate: `aterm-tokens` (leaf; T-4.1/T-4.2 defined the current set).

# Implementation notes

- **Semantic colors** - replace the current `[color.dark]` / `[color.light]`
  values with the mock's, keeping the semantic token names:
  - dark: canvas `#1b1915`, elevated `#221f19`, ink `#ece6d8`, ink-dim `#9a9382`,
    ink-faint `#5e584b`, hairline `rgba(236,230,216,0.085)`, accent `#3d88cc`,
    agent `#9d86d6`, warn `#d59a4a` / warn-bg `rgba(213,154,74,0.09)`, err
    `#d47257`, ok `#82ac79`.
  - light: canvas `#faf7ef`, elevated `#f2ede1`, ink `#26231b`, ink-dim `#6c6555`,
    ink-faint `#a89f8c`, hairline `rgba(38,35,27,0.10)`, accent `#2f7dc2`, agent
    `#7458bd`, warn `#b57d2c` / warn-bg `rgba(181,125,44,0.08)`, err `#bf5a40`,
    ok `#5c8a56`.
  - Map onto the existing token vocabulary: `bg.canvas`, `bg.surface`/`bg.elev`
    (add the elevated-surface token - popovers, the gate dropdown, the completion
    menu use it), `fg.primary`/`fg.secondary`/`fg.faint` (the mock's ink /
    ink-dim / ink-faint three-step), `hairline`, `success`(=ok), `caution`(=warn)
    + `caution_weak`(=warn-bg), `danger`(=err).
- **Agent second accent** - add `accent.agent` (purple) alongside `accent.primary`
  (blue). Introduce a resolved `mode` accent concept (shell -> `accent.primary`,
  agent -> `accent.agent`) so widgets can ask for "the current mode color" (the
  mock's `--mode` custom property). This relaxes the old one-accent rule per
  ADR-0011.
- **ANSI-16 palettes** - re-tune `[ansi.dark]` / `[ansi.light]` so raw terminal
  output belongs to the warmer family (T-4.2's structure stays; only hues move).
  Eyeball against real output is deferred to EPIC-7's shell-matrix, but pick
  warm-consistent hues now.
- **Contrast** - re-validate ink-on-canvas and accent/agent-on-canvas with the
  real WCAG library already used in T-4.2; note any pair that drops below AA for
  its use (small text vs UI/large) and record it, do not silently ship a fail.
- Update `tokens.toml` and `design-system.md` together in the same commit; the
  Rust constants regenerate/rebuild from `tokens.toml`.

# Acceptance criteria

- [x] `tokens.toml` carries the mock's dark + light values under the existing
  semantic names, plus the new `bg.elev` and `accent.agent` tokens; the
  `aterm-tokens` crate exposes them as typed constants and builds clean.
- [x] `design-system.md` intent text matches (warm palette, two mode accents,
  elevated surface); the stale "one accent" / "no title bar / no sidebar" clauses
  are updated to cite ADR-0011.
- [x] A "current mode accent" resolver exists (shell->primary, agent->agent) and
  is unit-tested for both modes and both themes.
- [x] Contrast re-validation runs in a test; every quoted ratio is recomputed
  against the new hexes, and any sub-AA pair is annotated with its permitted use.
- [x] No consumer regresses: `mise run build && mise run test` pass across the
  workspace.

## Notes

Landed 2026-07-01. The mock's alpha tints (`hairline`, `selection_bg`, the
`*_weak` fills) are stored pre-composited to opaque (they only ever sit on
canvas). `fg_muted` is the one intentional sub-AA tone (2.45/2.49:1 - the mock's
faint meta), annotated and guarded by `fg_muted_is_intentionally_sub_aa`. Wiring
the mode accent into the caret/prompt-glyph/mode-chip pixels is deferred to T-9.4
(this ticket adds only the `mode_accent` resolver + `accent_agent` token).

# Out of scope

- Applying the tokens to widgets - each downstream ticket (T-9.2..T-9.7) does its
  own surface.
- Bundling any new font (fonts are EPIC-4, already done).
