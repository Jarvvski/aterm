---
id: T-8.2
epic: EPIC-8-packaging
title: OFL font bundle + acknowledgements UI
status: ready-for-agent
labels: [packaging, licensing, deferred]
depends_on: [T-8.1]
---

# Goal

Satisfy the OFL 1.1 obligations for the bundled iM Writing Nerd Font and surface a third-party acknowledgements UI - shipping the OFL text + iA/IBM copyright, registering fonts per-app via `ATSApplicationFontsPath`, and keeping the renamed "iMWriting" family.

# Context

- Research: [10-packaging-scaffold.md](../../research/10-packaging-scaffold.md) section (b) (OFL obligations; `ATSApplicationFontsPath`); [12-licensing.md](../../research/12-licensing.md) (GPLv3 GO; iM Writing Nerd Font bundleable under OFL 1.1; ship OFL text + copyright, keep the iMWriting name; `cargo deny check licenses` with GPL/AGPL denylist).

# Implementation notes

- Place `OFL.txt` + the copyright header in `resources/fonts/` beside the `.ttf`s; reference from `THIRD-PARTY-NOTICES.md` (port the prototype's correct text).
- Register fonts per-app via the Info.plist `ATSApplicationFontsPath` key set to the fonts path relative to `Contents/Resources/` (e.g. `"fonts/"`) - no system install. (Or load bytes directly via the text stack; decide consistently with T-4.3.)
- OFL obligations: never sell the font alone (n/a), include copyright + full OFL text, never use reserved names "iA Writer"/"Plex" (the family is renamed iMWriting), OFL does not infect the surrounding GPLv3.
- Acknowledgements UI: a simple in-app view listing third-party licenses (font OFL + crate licenses). Wire `cargo deny check licenses` with a GPL/AGPL denylist into CI (study-only treatment of Warp's AGPL client - never copy source).

# Acceptance criteria

- The packaged app includes `OFL.txt` + copyright beside the fonts.
- Fonts activate per-app (no system font DB install).
- An acknowledgements UI lists the font OFL + key crate licenses.
- `cargo deny check licenses` passes with the GPL/AGPL denylist and flags any incompatible dep.

# Out of scope

- Signing/notarization (T-8.4); the renderer font wiring (T-4.3).
