---
id: T-8.4
epic: EPIC-8-packaging
title: Signing/notarization (when distribution matters)
status: needs-info
labels: [packaging, macos, deferred]
depends_on: [T-8.1]
---

# Goal

Code-sign and notarize the `.app`/`.dmg` so Gatekeeper accepts it without a right-click-open - deferred until the milestone where distribution matters. Status `needs-info`: the owner must confirm the milestone and the signing path before this starts.

# Context

- Research: [10-packaging-scaffold.md](../../research/10-packaging-scaffold.md) section (b) (signing v-later; Apple-native via cargo-packager, OR open-source `rcodesign`/apple-codesign 0.29.x which runs from Linux CI). Owner open-question #9 (the milestone at which signing/notarization becomes a requirement) - unresolved, hence `needs-info`.

# Implementation notes

- Two paths (pick when the decision lands):
  - Apple-native: Developer ID Application cert + cargo-packager `signing_identity` / `notarization_credentials` (Apple ID + app-specific password + team id, or App Store Connect API key). Runs on a Mac.
  - Open-source: `rcodesign` (apple-codesign 0.29.x) signs + `notary-submit` + staples, runnable from Linux/Windows CI with no Apple tooling. Needs Hardened Runtime + an `entitlements.plist`; a GPU/terminal app needs minimal entitlements (be careful: the Seatbelt sandbox in T-5.7 is `sandbox-exec`, separate from App Sandbox entitlements).
- Capture the decision as an ADR (e.g. `0004-code-signing.md`) when made; do not block earlier epics on it.

# Acceptance criteria

- (Once unblocked) The `.app` is signed + notarized + stapled; Gatekeeper opens it without a warning on a clean machine.
- The chosen path (Apple-native vs rcodesign) is recorded in an ADR.
- CI can produce a signed artifact (Mac or Linux per the chosen path).

# Out of scope

- Everything until the owner confirms the milestone + path (this ticket is `needs-info`).
