---
title: Rust Workspace Scaffold, Packaging & Agent Conventions
domain: packaging-scaffold
status: research
---

# Rust Workspace Scaffold, Packaging & Agent Conventions

## TL;DR

- **Workspace:** a Cargo virtual workspace with five member crates - `aterm-core` (PTY + VT + block model, no UI), `aterm-agent` (LLM seam + agentic loop + risk gate + secrets), `aterm-app` (the binary: renderer + window), plus `aterm-tokens` (design tokens, leaf dependency) and `aterm-bench` (Criterion harnesses). Strict dependency direction `app -> agent -> core`, with `tokens` a shared leaf used by `app` (and any crate needing theme constants). Enforce the boundary in CI with `cargo-deny` and the dependency graph, not just convention. [3][12]
- **Packaging:** use **cargo-packager** (CrabNebula, current `v0.11.8`, Nov 2025) for the `.app` + `.dmg`. It is the only actively maintained Rust-native tool that produces both a macOS App Bundle and a DMG, supports a custom `Info.plist` (via `info_plist_path`), bundles arbitrary `resources` (the fonts), and has first-class signing/notarization fields for later. `cargo-bundle` is the simpler but lower-velocity fallback; `dist` (formerly `cargo-dist`) is **not** a fit - GUI `.app`/`.dmg` bundling has been out of scope since issue #24 (2022). [1][4][5][7][9]
- **Hidden title bar:** do it at the window layer (winit `WindowAttributesExtMacOS`: `with_titlebar_transparent(true)` + `with_title_hidden(true)` + `with_fullsize_content_view(true)`), and back it in the bundle with the matching `Info.plist` keys. Edge-to-edge content with the traffic-light buttons floating over it - the prior prototype's look. [8][10]
- **Fonts + OFL:** drop the patched `iMWriting*` `.ttf` files into `resources/fonts/`, ship the OFL 1.1 text + copyright beside them, and register them per-app via the `ATSApplicationFontsPath` Info.plist key (no system install). OFL conditions: never sell the font alone, always include the license + copyright, never reuse the reserved names "iA Writer"/"Plex" - all already satisfied by the prototype's renamed family. [13][14][16]
- **Signing/notarization: explicitly v-later.** v1 ships an unsigned/ad-hoc `.app` (Gatekeeper right-click-open). When it matters, use Apple-credentialed `cargo-packager` notarization, or the fully open-source `rcodesign` (apple-codesign `0.29.x`) so notarization can run from CI on Linux without a Mac. [11][15]
- **Toolchain + agent conventions:** `mise.toml` pins the Rust toolchain and wires `run/build/test/fmt/clippy/bench`; lints live in `[workspace.lints]` so every crate inherits one policy; GitHub Actions gates fmt + clippy `-D warnings` + test, with the 60fps Criterion bench as a separate informational/regression job. Port the prototype's agent scaffolding nearly verbatim: `CLAUDE.md`, `docs/tickets/<slug>/` markdown issues with a `Status:` triage line, `docs/adr/`, a `CONTEXT.md`, and a jj-based "landing a change" workflow. [12][17]

## Findings

### (a) Cargo workspace layout

The prototype's three-module split (`:core -> :agent -> :ui-desktop`, dependency direction up) is sound and maps cleanly onto a Cargo virtual workspace. A virtual workspace = a root `Cargo.toml` with `[workspace]` and no `[package]`; members live in `crates/`. Shared dependency versions go in `[workspace.dependencies]` and members reference them with `dep = { workspace = true }`, which keeps versions unified across the tree (the Cargo analogue of the prototype's central `settings.gradle.kts` repo/version block).

Proposed members:

| Crate | Role | Depends on | Notes |
|---|---|---|---|
| `aterm-core` | Engine: PTY spawn, VT parse, grid + block model, OSC-133/OSC-7 shell-integration markers, shell-shim extraction. No UI, no LLM. | `aterm-tokens` (only if theme enums live there; otherwise nothing in-workspace) | A `lib`. This is the crate the 60fps story lives or dies in; keep it `no_std`-friendly where cheap, allocation-aware. |
| `aterm-agent` | LLM provider seam, the agentic turn loop, streaming-event mapper, **deterministic risk gate** (parsed command), the single `Secrets` source, output sanitizer, execution sinks, config (TOML). | `aterm-core` | A `lib`. The risk gate + secrets are the crown-jewel "keep" items from the prototype; they are pure logic and should be heavily unit-tested with no network. |
| `aterm-app` | The binary. Window (winit), GPU renderer, the unified wall-clock timeline, input box + hotkey routing, approval UI. | `aterm-agent`, `aterm-tokens` | `[[bin]] name = "aterm"`. The only crate that touches the OS window/GPU and the only one packaged. |
| `aterm-tokens` | Design tokens (colors, spacing, type scale, font family names) as Rust consts/structs, ideally generated from the machine-readable token file the design domain owns. | nothing (leaf) | Keeps the iA palette in one typed place; `app` and any future theming consume it. Leaf so it never creates a cycle. |
| `aterm-bench` | Criterion benchmark harnesses for the 60fps floor (VT-parse throughput, grid diff, frame-build time). | `aterm-core` (+ maybe `aterm-app` render internals exposed behind a `bench` feature) | A crate with `[[bench]]` targets and `harness = false`. Kept separate so Criterion + heavy fixtures don't bloat the shipping crates' dependency graph. |

Dependency direction is acyclic and one-way: `aterm-app -> aterm-agent -> aterm-core`; `tokens` is a leaf both `app` and `core` may read; `bench` sits off to the side depending only on `core`/internals. Enforce it: Cargo will reject cycles, and `cargo-deny`'s `bans`/graph checks can assert that, e.g., `aterm-core` never pulls in `aterm-agent` or any LLM SDK. [12]

Crate-naming note: Cargo crate names are global on crates.io, but since this is a GPLv3 app (not a published library) the names only need to be locally unique; `aterm-*` is fine and self-documenting. Use `[[bin]] name = "aterm"` so the produced binary is `aterm`, decoupled from the crate name `aterm-app`.

VT/PTY building blocks (cross-referenced from the engine domain, listed here only because they shape `core`'s dependency surface): the Rust ecosystem standard is `portable-pty` (from wezterm) for the PTY and `vte`/`vt100`/`alacritty_terminal` for parsing. Whichever the engine domain picks lands in `aterm-core`'s `Cargo.toml`; nothing about the workspace layout depends on that choice.

### (b) macOS app packaging

**Tool comparison (current as of 2026-06):**

- **cargo-packager** (CrabNebula): actively maintained, latest `@crabnebula/packager v0.11.8` (2025-11-27). Produces both macOS **DMG** and **App Bundle**. Configurable via `Packager.toml`, `packager.json`, or a `[package.metadata.packager]` table in `Cargo.toml`. Ships a `cargo-packager-updater` companion for self-update later. This is the recommendation. [1][4][5][7]
- **cargo-bundle** (burtonageo): the older, simpler tool - `cargo bundle` reads `[package.metadata.bundle]` and emits a `.app` (and `.deb`/`.msi`). Lower release velocity; DMG support is weaker. Reasonable fallback if cargo-packager's config surface proves fiddly. [6]
- **dist** / cargo-dist (axodotdev): a release *orchestrator* (build matrix, hosting, shell/PowerShell installers, GitHub Releases). It explicitly does **not** target GUI `.app`/`.dmg` bundling - issue #24 marked that out of scope back in 2022 and it still focuses on shipping binaries + installers. Not a fit for a windowed `.app`. Could be layered on later purely for release automation if desired, but it does not replace cargo-packager here. [9]

**cargo-packager macOS config surface** (top-level `Config` + nested `MacOsConfig`): [4][5]

- Top-level: `product_name`, `identifier` (reverse-DNS, e.g. `ai.ameba.aterm` or `dev.aterm`), `version`, `icons` (glob), `resources` (list of paths/globs or `{ src, target }` objects, copied into the bundle's Resources dir - this is the font-bundling hook), `external_binaries`, `before_packaging_command`.
- `MacOsConfig`: `minimum_system_version` (string, e.g. `"11.0"` for Apple-Silicon-era), `info_plist_path` (path to a custom `Info.plist` merged into the generated one), `frameworks`, `entitlements` (path to `entitlements.plist`), `signing_identity`, `signing_certificate` + `signing_certificate_password` (base64 p12 - for CI), `provider_short_name`, `notarization_credentials`, `embedded_provisionprofile_path`, `background_app` (sets `LSUIElement`), `exception_domain`.

**Hidden title bar / edge-to-edge window.** Two layers must agree:

1. *Window layer* (winit, `platform::macos::WindowAttributesExtMacOS`): build the window with `.with_titlebar_transparent(true)`, `.with_title_hidden(true)`, and `.with_fullsize_content_view(true)`; optionally `.with_titlebar_buttons_hidden(false)` to keep the traffic-light controls. This makes content draw under the title bar region, giving the iA-style edge-to-edge surface while keeping the standard close/minimize/zoom controls floating top-left. [8][10]
2. *Bundle layer* (`Info.plist`, supplied via `info_plist_path`): set the equivalent NSWindow style keys so the launched-from-Finder app matches the dev experience. The practical set is `NSFullSizeContentViewWindowMask` style + `NSWindowTitleHidden`-equivalent behavior; in plist terms you mainly need the app to be a normal foreground GUI app (no special key beyond the defaults), and the *titlebar transparency* itself is a runtime NSWindow setting that winit applies - so the plist's job is mostly font registration and standard GUI metadata, not the titlebar (which winit owns at runtime). Keep `CFBundleName`, `CFBundleIdentifier`, `CFBundleShortVersionString`, `LSMinimumSystemVersion`, `NSHighResolutionCapable = true`, and `NSSupportsAutomaticGraphicsSwitching` if relevant.

**Bundling the OFL fonts.** Place the patched `.ttf`s under `resources/fonts/` (e.g. `iMWritingMonoNerdFontMono-*.ttf` for the grid, the Duo/Quattro proportional variants for chrome/prose), list that directory in cargo-packager's `resources`, and register them with the per-app Info.plist key **`ATSApplicationFontsPath`** set to the fonts' path *relative to* `Contents/Resources/` (e.g. `"fonts/"`). macOS then activates those fonts for this app only, no system install. [14][16] Alternatively the renderer can load the `.ttf` bytes directly via the text stack (e.g. embedding with `include_bytes!` or loading from the bundle's Resources at runtime) and never touch the system font DB at all - decide with the render domain. Ship `OFL.txt` + the copyright header beside the fonts and reference them from `THIRD-PARTY-NOTICES.md` (the prototype already has the correct text to port). [13]

**OFL 1.1 obligations (all already satisfied by the renamed `iMWriting` family):** [13]
1. The font (or any component) must **never be sold by itself** - irrelevant for a bundled, GPL app.
2. Every copy must include the **copyright notice + the full OFL license text** (as standalone files, headers, or machine-readable metadata). Ship `resources/fonts/OFL.txt`.
3. **Reserved Font Names** ("iA Writer", "Plex") must not be used as the font's name in a modified version - which is exactly why the family is renamed `iMWriting`.
4. The original copyright holders' names can't be used to promote derived versions.
5. The font must stay under OFL; it does **not** infect the surrounding GPLv3 software. The prototype's `THIRD-PARTY-NOTICES.md` already states this correctly.

**Code signing + notarization - v-later, not v1.** For v1, ship an unsigned (or ad-hoc-signed) `.app`; users right-click -> Open the first time (Gatekeeper). When distribution matters:
- *Apple-native path:* an Apple Developer ID Application cert + cargo-packager's `signing_identity` / `notarization_credentials` (Apple ID + app-specific password + team id, or an App Store Connect API key). Runs on a Mac.
- *Open-source path:* `rcodesign` (the `apple-codesign` crate, current `0.29.x`, by indygreg/Gregory Szorc) signs, notarizes (`rcodesign notary-submit` speaks Apple's Notary API), and staples - and crucially runs from **Linux/Windows CI** with no proprietary Apple tooling, useful for a no-Mac CI runner. Hardened Runtime + an `entitlements.plist` is required for notarization; a GPU/terminal app typically needs minimal entitlements (no special sandbox in v1). [11][15] Capture this as an ADR when the decision is actually made; do not block v1 on it.

### (c) Toolchain

**mise.** Pin the Rust toolchain in `mise.toml` (mise has a first-class `rust` backend) and define the same task names the prototype used so muscle memory and CLAUDE.md carry over: `run`, `build`, `test`, `fmt`, plus `clippy` and `bench`. [17] mise pins the channel (e.g. `stable` or a dated `1.8x.0`) and components (`rustfmt`, `clippy`). A draft is in the Recommendations section.

**rustfmt + clippy.** Keep config minimal and explicit:
- `rustfmt.toml` at the root: a few opinionated keys (`edition = "2021"` or `2024`, `imports_granularity`, `group_imports`) - some of these are nightly-only in rustfmt, so prefer stable keys and note nightly ones as optional. Run via `cargo fmt --all`.
- Lints in `[workspace.lints]` in the root `Cargo.toml`, inherited by every member via `[lints] workspace = true`. This is the modern, single-source-of-truth approach (replaces scattering `#![deny(...)]` across crates). Opt into `clippy::all` + a cherry-picked slice of `clippy::pedantic`/`clippy::nursery` rather than the whole pedantic group (the community consensus is to cherry-pick, since pedantic is noisy and lints can conflict and need `priority`). [12] A `clippy.toml` at the root holds configurable-lint thresholds if needed (e.g. `cognitive-complexity-threshold`).
- CI runs `cargo clippy --workspace --all-targets -- -D warnings` so warnings fail the build.

**GitHub Actions CI.** Gate on three required checks plus one informational:
1. **fmt** - `cargo fmt --all --check`.
2. **clippy** - `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
3. **test** - `cargo test --workspace` (on `macos-14` / Apple Silicon runners, since the engine + window are macOS-specific; `aterm-core`/`aterm-agent` pure-logic tests can also run on cheaper Linux runners, but anything touching PTY/window needs macOS).
4. **bench (60fps floor)** - run `aterm-bench` Criterion targets and assert frame-build / VT-parse stays under budget. Criterion is great for *local* regression but its CI thresholds are noisy on shared runners; recommend running it as a **non-blocking** job that posts numbers, and gate hard only on a small set of microbenchmarks with generous margins (or use a dedicated self-hosted Mac runner for stable timing). The 16.6ms (60fps) / 8.3ms (120fps) frame budgets are the targets to track over time, not flaky per-PR gates. Use `Swatinem/rust-cache` for build caching and `dtolnay/rust-toolchain` (or mise in CI) to install the pinned toolchain.

### (d) Agent-facing conventions to scaffold

The prototype already evolved a clean, file-based agent workflow. Port it almost verbatim, adjusting only the module names and the issue-tracker location (the task asks for `docs/tickets/`, the prototype used `.scratch/<slug>/`; `docs/tickets/` is the better long-lived home and is the recommendation).

- **`CLAUDE.md`** at the repo root - the project memory. Mirror the prototype's structure: an Architecture table (the five crates + dependency direction), per-crate responsibility notes, the agent/risk-gate invariants ("the safety gate is deterministic code, never the prompt"; "one Secrets source feeds gate + sanitizer"; "never auto-approve on shell metacharacters/`~`/redirects"), shell-integration notes (zsh shim via ZDOTDIR, OSC-133/7), the "landing a change" jj workflow, versioning/changelog policy, the mise task list, and testing gotchas. The prototype's `CLAUDE.md` is an excellent template - reuse its prose discipline.
- **Issue tracker - `docs/tickets/`.** One feature per directory `docs/tickets/<feature-slug>/`; a `PRD.md` per feature; implementation issues at `docs/tickets/<slug>/issues/NN-<slug>.md` numbered from `01`; triage state on a `Status:` line near the top; conversation appended under a `## Comments` heading. (This is the prototype's `.scratch/` convention relocated to `docs/tickets/`.)
- **Triage labels** - the five canonical roles as literal strings on the `Status:` line: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. Document the mapping in `docs/agents/triage-labels.md`.
- **ADRs - `docs/adr/`.** Numbered `NNNN-title.md` (e.g. `0001-rust-rewrite.md`, `0002-render-stack.md`, `0003-packaging-tool.md`, `0004-code-signing.md`). One decision per file; agents must flag when their output contradicts an existing ADR rather than silently overriding (the prototype's `docs/agents/domain.md` already specifies this behavior).
- **`CONTEXT.md`** at the root - the single glossary/domain doc (block, command block, transcript, risk gate, sink, secrets, timeline, shell shim, input target/route). Agents use the glossary's exact vocabulary in titles, tests, and proposals.
- **`docs/agents/`** - the meta-docs explaining how agents consume the above: `issue-tracker.md`, `triage-labels.md`, `domain.md`. Port directly from the prototype.
- **"Landing a change" workflow (jj, not git).** The owner uses Jujutsu in a colocated repo and git must never be invoked. The flow (from the prototype's CLAUDE.md): make one focused change; run `mise run fmt && mise run clippy && mise run build && mise run test` and only land when all pass; bump the version + add a dated `CHANGELOG.md` entry for user-visible changes in the *same* commit; then `jj describe -m "<imperative one-liner>"`, `jj bookmark set main --to @`, `jj new`. Invariant: every time `main` moves, the next command is `jj new` so `@` is always an empty commit one above main. Note that jj does not fire git hooks, so formatting is a task, not a pre-commit hook.

### Concrete artifacts

**File / directory tree:**

```
atermr/
  Cargo.toml                  # [workspace] virtual manifest: members, workspace.dependencies, workspace.lints
  Cargo.lock                  # committed (this is an app, not a lib)
  rust-toolchain.toml         # optional: pins channel/components; complements mise
  rustfmt.toml
  clippy.toml                 # only if configurable-lint thresholds are needed
  deny.toml                   # cargo-deny: license + bans + crate-boundary graph checks
  mise.toml                   # tasks + tool pins
  Packager.toml               # OR [package.metadata.packager] inside crates/app/Cargo.toml
  CLAUDE.md                   # project memory (ported/adapted from prototype)
  CONTEXT.md                  # single glossary / domain doc
  CHANGELOG.md                # user-visible changes, newest first
  LICENSE                     # GPL-3.0-or-later
  THIRD-PARTY-NOTICES.md      # deps + bundled-font (OFL) notices (port from prototype)
  README.md
  .github/
    workflows/
      ci.yml                  # fmt + clippy + test (required) + bench (informational)
  crates/
    core/
      Cargo.toml              # aterm-core
      src/lib.rs
    agent/
      Cargo.toml              # aterm-agent  (depends: aterm-core)
      src/lib.rs
    app/
      Cargo.toml              # aterm-app  -> [[bin]] name = "aterm"  (depends: aterm-agent, aterm-tokens)
      src/main.rs
    tokens/
      Cargo.toml              # aterm-tokens (leaf)
      src/lib.rs
    bench/
      Cargo.toml              # aterm-bench (depends: aterm-core)
      benches/
        vt_parse.rs           # harness = false, Criterion
        frame_build.rs
  resources/
    fonts/
      iMWritingMonoNerdFontMono-Regular.ttf
      iMWritingMonoNerdFontMono-Bold.ttf
      ...                     # Duo / Quattro proportional variants
      OFL.txt                 # SIL OFL 1.1 license text (required by OFL cond. 2)
      LICENSE-iAWriterNerdFont.md
    Info.plist                # custom plist: ATSApplicationFontsPath=fonts/, LSMinimumSystemVersion, etc.
    entitlements.plist        # later, for hardened-runtime notarization
    icon.icns                 # app icon
  docs/
    adr/
      0001-rust-rewrite.md
      0002-render-stack.md
      0003-packaging-tool.md
    tickets/
      <feature-slug>/
        PRD.md
        issues/
          01-<slug>.md        # has a `Status:` triage line
    agents/
      issue-tracker.md
      triage-labels.md
      domain.md
    research/
      10-packaging-scaffold.md   # this file
```

**Draft `mise.toml`:**

```toml
[tools]
rust = "1.89.0"               # pin a concrete recent stable; VERIFY current version at pin time

[env]
RUST_BACKTRACE = "1"

[tasks.run]
description = "Run the aterm terminal app"
run = "cargo run -p aterm-app"

[tasks.build]
description = "Build the workspace (no tests)"
run = "cargo build --workspace"

[tasks.test]
description = "Run all workspace tests"
run = "cargo test --workspace"

[tasks.fmt]
description = "Format all crates"
run = "cargo fmt --all"

[tasks.clippy]
description = "Lint with warnings as errors"
run = "cargo clippy --workspace --all-targets --all-features -- -D warnings"

[tasks.bench]
description = "Run the 60fps benchmark harness"
run = "cargo bench -p aterm-bench"

[tasks.package]
description = "Build the .app + .dmg via cargo-packager"
run = "cargo packager --release"
```

(The `rust` version above is illustrative - confirm the current stable before pinning; do not treat `1.89.0` as a verified release number.)

**Root `Cargo.toml` sketch (workspace + shared lints):**

```toml
[workspace]
resolver = "2"
members = ["crates/core", "crates/agent", "crates/app", "crates/tokens", "crates/bench"]

[workspace.package]
edition = "2021"
license = "GPL-3.0-or-later"
rust-version = "1.89"        # MSRV; keep in sync with mise

[workspace.dependencies]
# shared, version-unified deps referenced by members as `dep = { workspace = true }`

[workspace.lints.rust]
unsafe_op_in_unsafe_fn = "warn"

[workspace.lints.clippy]
all = "warn"
# cherry-pick from pedantic rather than enabling the whole group:
# (each member opts in with `[lints] workspace = true`)
```

**CI sketch (`.github/workflows/ci.yml`):**

```yaml
name: ci
on:
  pull_request:
  push:
    branches: [main]

jobs:
  fmt:
    runs-on: macos-14
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { components: rustfmt }
      - run: cargo fmt --all --check

  clippy:
    runs-on: macos-14
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { components: clippy }
      - uses: Swatinem/rust-cache@v2
      - run: cargo clippy --workspace --all-targets --all-features -- -D warnings

  test:
    runs-on: macos-14            # window/PTY are macOS-specific
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo test --workspace

  bench:                          # informational / regression, NOT a hard gate
    runs-on: macos-14
    continue-on-error: true
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo bench -p aterm-bench -- --output-format bencher | tee bench.txt
```

(Action versions - `actions/checkout@v4`, `Swatinem/rust-cache@v2`, `dtolnay/rust-toolchain` - are the current widely-used majors; pin to the latest tags at scaffold time. `macos-14` is the GitHub-hosted Apple-Silicon runner label; confirm the current label, as GitHub rotates these.)

**Agent-conventions checklist (what to scaffold):**

- [ ] `CLAUDE.md` - architecture table (5 crates + dependency arrow), per-crate notes, risk-gate/secrets invariants, shell-integration notes, the jj "landing a change" flow, versioning/changelog policy, mise tasks, testing gotchas.
- [ ] `CONTEXT.md` - one glossary covering block / command block / transcript / risk gate / sink / secrets / timeline / shell shim / input route.
- [ ] `docs/adr/NNNN-*.md` - one decision per file; agents flag contradictions, never silently override.
- [ ] `docs/tickets/<slug>/PRD.md` + `issues/NN-<slug>.md` with a `Status:` triage line; comments appended under `## Comments`.
- [ ] `docs/agents/issue-tracker.md`, `triage-labels.md`, `domain.md` - the meta-docs (ported from the prototype).
- [ ] Triage label strings: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`.
- [ ] "Landing a change": `mise run fmt && clippy && build && test` clean -> version bump + dated CHANGELOG entry (same commit) -> `jj describe -m` -> `jj bookmark set main --to @` -> `jj new`. Never `git`. jj fires no git hooks, so fmt is a task not a hook.

## Recommendations for aterm

1. **Five-crate virtual workspace** `aterm-core`/`aterm-agent`/`aterm-app`/`aterm-tokens`/`aterm-bench`, members under `crates/`, shared versions in `[workspace.dependencies]`, lints in `[workspace.lints]`. Binary named `aterm`. - *Mirrors the proven 3-module split, adds the two crates the task calls for, and keeps the dependency arrow one-way.* **Confidence: High.**
2. **cargo-packager for `.app` + `.dmg`.** Configure via `[package.metadata.packager]` in `aterm-app/Cargo.toml` (or a root `Packager.toml`). - *Only actively maintained Rust-native tool doing both formats with Info.plist + resources + a signing path; dist is wrong-shaped, cargo-bundle is the fallback.* **Confidence: High.**
3. **Hidden title bar at the winit layer** (`with_titlebar_transparent` + `with_title_hidden` + `with_fullsize_content_view`), edge-to-edge content, traffic lights kept. - *Reproduces the prototype's iA look with the standard, documented winit macOS extension.* **Confidence: High.**
4. **Bundle fonts via `resources/fonts/` + `ATSApplicationFontsPath`**, ship `OFL.txt` beside them, keep the renamed `iMWriting` family. - *Satisfies OFL 1.1 (no standalone sale, license shipped, no reserved names) and activates fonts per-app with no system install.* **Confidence: High.**
5. **Defer signing/notarization explicitly to a later milestone**; record the choice between Apple-native and `rcodesign` as an ADR when it's actually made; ship v1 unsigned. - *Signing is operationally heavy and unrelated to the 60fps headline; not worth blocking v1.* **Confidence: High.**
6. **mise tasks `run/build/test/fmt/clippy/bench`**, Rust toolchain pinned in `mise.toml`. - *Carries the prototype's muscle memory and CLAUDE.md references forward unchanged.* **Confidence: High.**
7. **CI gates fmt + clippy `-D warnings` + test as required; the 60fps Criterion bench as a non-blocking/regression job** on a Mac runner, hard-gating only a few microbenchmarks with generous margins. - *Criterion timing on shared CI is noisy; a flaky perf gate trains people to ignore it.* **Confidence: Med.**
8. **Relocate the issue tracker to `docs/tickets/<slug>/`** (vs the prototype's `.scratch/`), keep everything else about the convention identical, port `docs/agents/*` verbatim. - *`docs/tickets/` is a durable, discoverable home that matches the task's instruction; `.scratch/` reads as throwaway.* **Confidence: High.**
9. **Enforce the crate boundary with `cargo-deny`** (bans/graph) so `aterm-core` can never accidentally depend on the agent or an LLM SDK. - *Turns the architectural rule into a CI check instead of a code-review hope.* **Confidence: Med.**
10. **Edition + toolchain: pin a recent stable Rust and use edition 2021** (move to 2024 only after confirming all deps + rustfmt keys are stable on it). - *2024 edition is fine but some rustfmt niceties are still nightly; don't trade determinism for polish on a perf-critical project.* **Confidence: Med.**

## Risks & unknowns

- **cargo-packager macOS config exactness.** The field list ([4][5]) is from the docs.rs API surface; the precise TOML key spellings under `[package.metadata.packager.macos]` and how a custom `info_plist_path` *merges* vs *overrides* the generated plist need verification against the live config docs (`docs.crabnebula.dev/packager/configuration/`) before scaffolding - the fetch of that page returned a stub. Treat the keys here as accurate-in-name, verify-in-spelling.
- **Hidden-titlebar plist vs runtime split.** I'm confident winit applies the transparent/fullsize-content title bar at runtime via NSWindow; I am *not* certain any `Info.plist` key is strictly required for it (as opposed to font registration, which definitely needs `ATSApplicationFontsPath`). Worst case the title bar setup is purely runtime and the plist only carries fonts + standard metadata. Verify by building a throwaway bundle.
- **Criterion as a CI gate.** Microbenchmark timing on GitHub-hosted runners is noisy enough that a hard 16.6ms gate will flake. The real 60fps verification is end-to-end frame timing in the running app on real Apple Silicon, which Criterion does not measure. The bench crate guards *components* (VT parse, grid diff), not the rendered-frame budget - that needs an in-app instrument the render domain should own.
- **edition 2024 + rustfmt.** Some `imports_granularity`/`group_imports` rustfmt options remain unstable (nightly) on current toolchains; if the project wants them on stable, that may not be possible yet. Unverified against the exact pinned toolchain.
- **`rcodesign` notarization reliability** for a *windowed* `.app` with bundled fonts/frameworks is plausible but I haven't verified an end-to-end run; Apple's Notary API behavior shifts. Validate before relying on a no-Mac CI signing path.
- **dist (cargo-dist) status** is from a 2022 issue + changelog skim; if the project later wants release *automation* (not bundling), re-check whether dist now wraps an external bundler. Low impact on v1.

## Open questions for the product owner

1. **Bundle identifier:** `ai.ameba.aterm`, `dev.aterm`, `com.github.jarvvski.aterm` (the prototype's namespace), or something else? This is baked into the plist, signing, and any future updater feed.
2. **Issue-tracker location:** confirm `docs/tickets/` (recommended) vs keeping the prototype's `.scratch/` convention.
3. **Minimum macOS version:** `11.0` (Big Sur, first Apple Silicon) is the natural floor given the Apple-Silicon-first target - confirm, or set higher (e.g. `13.0`/`14.0`) to use newer APIs.
4. **Rust edition:** 2021 (safe, recommended) vs 2024 (newer, minor rustfmt caveats)?
5. **Release automation appetite for v1:** just `mise run` + manual cargo-packager locally, or wire a GitHub Actions release job (and if so, do we want `dist` purely for the release/installer orchestration around the cargo-packager-produced artifacts)?
6. **Signing timeline:** when (which milestone) does notarization become a requirement, and is there a Mac CI runner available, or do we need the `rcodesign`-from-Linux path?

## Sources

1. cargo-packager (CrabNebula) - GitHub repo (version, formats, config methods): https://github.com/crabnebula-dev/cargo-packager
2. cargo-packager - crates.io: https://crates.io/crates/cargo-packager
3. The Cargo Book - Workspaces (`[workspace.dependencies]`, virtual manifest): https://doc.rust-lang.org/cargo/reference/workspaces.html
4. cargo-packager `MacOsConfig` API (signing, entitlements, info_plist_path, minimum_system_version, notarization_credentials): https://docs.rs/cargo-packager/latest/cargo_packager/config/struct.MacOsConfig.html
5. cargo-packager `Config` API (resources, icons, identifier, product_name, before_packaging_command): https://docs.rs/cargo-packager/latest/cargo_packager/config/struct.Config.html
6. cargo-bundle - GitHub repo (`[package.metadata.bundle]`, `.app`): https://github.com/burtonageo/cargo-bundle
7. cargo-packager docs (Get Started; configuration via Packager.toml/Cargo.toml): https://docs.crabnebula.dev/packager/
8. winit `WindowAttributesExtMacOS` (with_titlebar_transparent / with_title_hidden / with_fullsize_content_view / with_titlebar_buttons_hidden): https://docs.rs/winit/latest/winit/platform/macos/trait.WindowAttributesExtMacOS.html
9. dist / cargo-dist issue #24 - macOS .dmg/.app GUI bundling out of scope: https://github.com/axodotdev/cargo-dist/issues/24
10. Slint discussion - hiding the macOS title bar (fullsize content + title hidden + transparent titlebar): https://github.com/slint-ui/slint/discussions/4284
11. apple-codesign / rcodesign docs (open-source signing + notarization, Notary API, cross-platform from Linux): https://gregoryszorc.com/docs/apple-codesign/stable/
12. Clippy workspace lints / `[workspace.lints]` and pedantic cherry-picking: https://coreyja.com/notes/clippy-pedantic-workspace and https://doc.rust-lang.org/clippy/configuration.html
13. SIL Open Font License 1.1 official text (conditions 1-5: no standalone sale, include license + copyright, reserved names, no promotion, stays under OFL): https://openfontlicense.org/open-font-license-official-text/
14. Apple Info.plist key reference - `ATSApplicationFontsPath` (per-app font activation from Resources): https://developer.apple.com/library/archive/documentation/General/Reference/InfoPlistKeyReference/Articles/GeneralPurposeKeys.html
15. apple-platform-rs / rcodesign repo (install, Notary API client, supported OSes): https://github.com/indygreg/apple-platform-rs
16. Embedding a custom font into a macOS app bundle (ATSApplicationFontsPath usage): https://nilcoalescing.com/blog/EmbeddingACustomFontIntoAMacOSAppBundle/
17. mise - Rust backend / tasks: https://mise.jdx.dev/lang/rust.html
