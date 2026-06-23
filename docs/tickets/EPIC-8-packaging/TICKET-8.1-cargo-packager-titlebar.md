---
id: T-8.1
epic: EPIC-8-packaging
title: cargo-packager .app + .dmg + hidden titlebar
status: ready-for-agent
labels: [packaging, macos, deferred]
depends_on: [T-4.3]
---

# Goal

Package `aterm` as a macOS `.app` + `.dmg` via cargo-packager, with the edge-to-edge hidden-titlebar window (winit + matching Info.plist) and the fonts bundled as resources. Deferred until distribution matters; ships unsigned/ad-hoc in v1.

# Context

- Research: [10-packaging-scaffold.md](../../research/10-packaging-scaffold.md) section (b) (cargo-packager 0.11.x; hidden titlebar two layers; minimum macOS 11.0; bundle identifier `ai.ameba.aterm`). Owner open-question #9 (bundle id, min macOS, signing milestone).

# Implementation notes

- Tool: `cargo-packager` 0.11.x (CrabNebula). Config via `[package.metadata.packager]` in `crates/app/Cargo.toml` or `Packager.toml`.
- Config: `product_name`, `identifier` (`ai.ameba.aterm` proposal - confirm), `version`, `icons`, `resources` (the `resources/fonts/` dir), `MacOsConfig.minimum_system_version = "11.0"`, `info_plist_path`.
- Hidden titlebar, two layers that must agree: (1) winit `WindowAttributesExtMacOS` `.with_titlebar_transparent(true)` + `.with_title_hidden(true)` + `.with_fullsize_content_view(true)` (keep traffic-light buttons); (2) Info.plist standard GUI metadata (`CFBundleName/Identifier/ShortVersionString`, `LSMinimumSystemVersion`, `NSHighResolutionCapable=true`). The titlebar transparency is a runtime NSWindow setting winit applies.
- v1 ships unsigned/ad-hoc (Gatekeeper right-click-open). Signing/notarization is T-8.4.

# Acceptance criteria

- `cargo packager` produces a launchable `.app` and a `.dmg`.
- The launched-from-Finder app shows the edge-to-edge hidden-titlebar window matching the dev experience.
- Bundled fonts load in the packaged app.
- `minimum_system_version` is 11.0; the app runs on Apple Silicon macOS.

# Out of scope

- OFL acknowledgements UI (T-8.2), signing/notarization (T-8.4).
