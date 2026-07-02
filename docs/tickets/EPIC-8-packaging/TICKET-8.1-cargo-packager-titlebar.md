---
id: T-8.1
epic: EPIC-8-packaging
title: cargo-packager .app + .dmg (Info.plist matches the borderless window)
status: ready-for-agent
labels: [packaging, macos, deferred]
depends_on: [T-4.3, T-9.9]
---

# Goal

Package `aterm` as a macOS `.app` + `.dmg` via cargo-packager, with the fonts bundled as resources and an Info.plist whose GUI metadata agrees with the borderless window attributes set in [T-9.9](../EPIC-9-vision-mock-reskin/TICKET-9.9-borderless-window-frame.md). Deferred until distribution matters; ships unsigned/ad-hoc in v1. (The window-chrome BEHAVIOR - hiding the native titlebar, the transparent surface, real window controls - is T-9.9, not this ticket; a dev-run build must already show the single custom title bar before this packages anything.)

# Context

- Research: [10-packaging-scaffold.md](../../research/10-packaging-scaffold.md) section (b) (cargo-packager 0.11.x; hidden titlebar two layers; minimum macOS 11.0; bundle identifier `ai.ameba.aterm`). Owner open-question #9 (bundle id, min macOS, signing milestone).

# Implementation notes

- Tool: `cargo-packager` 0.11.x (CrabNebula). Config via `[package.metadata.packager]` in `crates/app/Cargo.toml` or `Packager.toml`.
- Config: `product_name`, `identifier` (`ai.ameba.aterm` proposal - confirm), `version`, `icons`, `resources` (the `resources/fonts/` dir), `MacOsConfig.minimum_system_version = "11.0"`, `info_plist_path`.
- Info.plist standard GUI metadata (`CFBundleName/Identifier/ShortVersionString`, `LSMinimumSystemVersion`, `NSHighResolutionCapable=true`). The hidden-titlebar winit `WindowAttributesExtMacOS` layer is owned by T-9.9; this ticket's plist must simply not contradict it.
- v1 ships unsigned/ad-hoc (Gatekeeper right-click-open). Signing/notarization is T-8.4.

# Acceptance criteria

- `cargo packager` produces a launchable `.app` and a `.dmg`.
- The launched-from-Finder app shows the edge-to-edge hidden-titlebar window matching the dev experience.
- Bundled fonts load in the packaged app.
- `minimum_system_version` is 11.0; the app runs on Apple Silicon macOS.

# Out of scope

- OFL acknowledgements UI (T-8.2), signing/notarization (T-8.4).
