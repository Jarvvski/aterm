# Changelog

User-visible (and contributor-visible) changes to aterm, newest first. Each entry
describes what someone would notice. Pure internal refactors don't appear here -
use `jj log` for the full history.

Semver: PATCH for fixes, MINOR for everything else. MAJOR (1.0.0+) is locked off
until the owner explicitly approves it - never auto-bump. The version of record is
`[workspace.package].version` in the root `Cargo.toml`. New entries go on top, under
the next version (or an `## Unreleased` heading until a version is cut).

## 0.1.0 - 2026-06-23

### Added

- **Project bootstrap.** A native-Rust, macOS-first GPU terminal scaffold: a six-crate
  Cargo workspace (`aterm-core` / `-tokens` / `-agent` / `-ui` / `-app` / `-bench`) that
  compiles, runs, and is green. `mise run run` (or `cargo run -p aterm-app`) opens a
  `winit` + `wgpu` (Metal) window that renders a live login-shell PTY through a `glyphon`
  text path in the bundled iM Writing Nerd Font.
- **Engine (`aterm-core`).** PTY spawn/resize over `portable-pty`, VT/grid parsing via
  `alacritty_terminal`, an OSC-133/OSC-7 mark scanner (nonce-gated), the command-block
  model, and the pure unified-input `InputModel` reducer - the mode toggle preserves
  typed text by construction.
- **Agent safety spine (`aterm-agent`).** A deterministic, over-approximating risk
  classifier; a single `Secrets` source feeding both the gate and a redact-before-truncate
  `OutputSanitizer`; an auto-safe `ApprovalPolicy`; and the `LlmProvider` / `Sandbox` trait
  seams. The LLM provider clients and the agentic turn loop are compiling stubs (EPIC-5).
- **Design system.** `aterm-tokens` reifies the iA-derived light "paper" + dark themes,
  tuned ANSI-16 palettes, and type/spacing scales from `docs/design/tokens.toml`.
- **Foundations.** The 12-domain research dossier (`docs/research/`), 10 ADRs, the
  52-ticket backlog (`docs/tickets/`), `mise` tasks, GitHub CI, and a `cargo-deny` license
  policy that rejects AGPL / GPL-incompatible dependencies.
