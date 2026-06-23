---
id: T-4.1
epic: EPIC-4-design-system
title: aterm-tokens - semantic tokens + spacing/type scale
status: ready-for-agent
labels: [tokens, design]
depends_on: []
---

# Goal

Populate `aterm-tokens` with the typed semantic color tokens (light "paper" + dark), spacing scale, type scale, radii, motion, and caret rules, as Rust consts/structs - the single typed source for the iA look.

# Context

- Research: [07-ia-design-language.md](../../research/07-ia-design-language.md) section 3 (derived semantic tokens table), section 4 (spacing, motion, caret). Risk: the accent blue hex is derived, not source-verified - owner open-question #6 must confirm/sample `#1A93E8`/`#4DA6F0` before locking; use `#1577C2` for accent-bearing small text on light bg (AA).
- ADR: design-tokens decision (if a machine-readable token file is the source of truth, generate `aterm-tokens` from it).

# Implementation notes

- Crate: `aterm-tokens` (leaf, no internal deps).
- Encode the semantic token set from the dossier table: `bg.canvas/surface/surface.alt`, `fg.primary/secondary/muted/faint`, `accent.primary` (+`.weak`), `hairline`(+`.strong`), `selection.bg`, `success/caution/danger/info`, each with light + dark values. `success/caution/danger` use the iA syntax-highlight hue family (green/yellow/magenta-red), not generic alert colors.
- Spacing scale (4px base: space.0..space.12), radii (sm 4px, md 6px), type scale (`type.grid` 13/1.30, `type.body` 14/1.50, `type.label` 11/1.30, `type.heading` 16/1.35, `type.caption` 10.5/1.30), font-family names (Mono NFM / Duo / Quattro), motion durations (fast 90 / base 140 / slow 220ms) + decelerate easing, caret rules (2px bar, soft opacity blink, suppress while typing).
- Provide a `Theme` enum {Paper, Dark} and a typed accessor so `aterm-ui` reads tokens without hardcoded hex.
- Mark the accent hex with a TODO/flag that it is pending owner confirmation (do not block, but make it a single point of change).

# Acceptance criteria

- `aterm-tokens` exposes every semantic token for both themes via a typed API.
- A unit test asserts WCAG contrast for key pairs using a real contrast computation (e.g. `fg.primary` on `bg.canvas` >= 7:1; `accent.primary` large/UI >= 3:1) - the dossier's ratios were estimates, so compute them here for real.
- No internal-crate dependency (leaf preserved).
- Switching `Theme` returns the correct value set.

# Out of scope

- ANSI-16 palettes + runtime theme switching plumbing (T-4.2).
- Component specs (T-4.6).
