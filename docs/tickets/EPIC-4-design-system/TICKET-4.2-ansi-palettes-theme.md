---
id: T-4.2
epic: EPIC-4-design-system
title: aterm-tokens - two ANSI-16 palettes + theme switching
status: ready-for-agent
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
