---
id: T-4.3
epic: EPIC-4-design-system
title: Bundle Duo/Quattro + three-register font wiring
status: ready-for-agent
labels: [design, text, fonts]
depends_on: [T-1.6]
---

# Goal

Add the iM Writing Duo and Quattro proportional variants to the font bundle (only Mono is currently vendored) and wire the three-register split: Mono NFM for the grid, Duo for agent prose, Quattro for dense UI chrome.

# Context

- Research: [07-ia-design-language.md](../../research/07-ia-design-language.md) section 2 (Mono/Duo/Quattro mapping); [08-text-glyph-rendering.md](../../research/08-text-glyph-rendering.md) Recommendation 11 (Duo/Quattro not yet vendored; the proportional path has no font shipped). Risk: Duo/Quattro precise metrics are qualitative in iA essays - measure advance/x-height/cap-height from the actual TTFs before any Duo-advance-dependent layout. Licensing: confirm the patched iMWriting set is OFL-clean (Nerd Fonts patch + iA's Plex modification = two upstream licenses).

# Implementation notes

- Place the Duo/Quattro `.ttf`s under `resources/fonts/` beside Mono NFM. If a license-clean patched Duo/Quattro is unavailable, define a system proportional fallback and flag it.
- Wire the proportional `Buffer` layout front-end (the second front-end from T-1.6) to use Duo for `type.body` agent prose (cap measure ~72ch) and Quattro for `type.label` chrome.
- Register fonts per-app (loaded directly by the text stack from the bundle, or via `ATSApplicationFontsPath` - decide with packaging T-8.1; for dev, load bytes directly).
- Measure and record the real Duo/Quattro metrics; feed them into `aterm-tokens` type scale if needed.

# Acceptance criteria

- Duo and Quattro load and render agent prose / chrome labels respectively.
- Agent prose wraps at ~72ch in Duo; the terminal grid remains Mono NFM uncapped.
- Measured Duo/Quattro metrics are documented.
- The bundled set's OFL/Nerd-Font licensing is confirmed clean (or a fallback is documented).

# Out of scope

- Nerd Font constraint table (T-4.4) and sprite face (T-4.5).
- The packaging-time font registration (T-8.1/T-8.2).
