# Changelog

User-visible (and contributor-visible) changes to aterm, newest first. Each entry
describes what someone would notice. Pure internal refactors don't appear here -
use `jj log` for the full history.

Semver: PATCH for fixes, MINOR for everything else. MAJOR (1.0.0+) is locked off
until the owner explicitly approves it - never auto-bump. The version of record is
`[workspace.package].version` in the root `Cargo.toml`. New entries go on top, under
the next version (or an `## Unreleased` heading until a version is cut).

## Unreleased

### Changed

- **Frame pacing (render loop).** The window now presents on a keep-warm schedule
  instead of a continuous redraw spin: after any activity (a keystroke, a resize, or
  newly published shell output) it presents every vsync - `Fifo`-locked to the panel
  refresh - for ~1s, then idles to **zero drawn frames** until the next activity.
  Idle CPU drops to ~0, and the pacing is driven by a pure, unit-tested keep-warm
  scheduler (ticket T-1.5). The precise self-bridged `CADisplayLink` vsync source the
  60fps floor targets is layered on behind a seam (opt-in, validated on ProMotion
  hardware).
- **Allocation-free steady-state present.** The renderer no longer deep-clones the
  grid every frame (it borrows the engine's published `Arc<Snapshot>`) and the grid
  text buffer is reshaped only when the content or window size changes - an unchanged
  warm frame now allocates nothing on the present path (ticket T-1.5 AC5). Per-cell
  color/attr drawing and the formal debug allocation assertion remain T-1.6 / T-1.8.

### Known limitations

- **Resize is not yet tear-synchronized.** T-1.5 AC4 (toggle `presentsWithTransaction`
  on the Metal layer for the duration of a live resize) is **deferred**: it requires
  reaching wgpu's `CAMetalLayer` via `Surface::as_hal` and a synchronous main-thread
  transactional present during resize, and tear-free resize cannot be verified without
  a display. The present-with-transaction protocol already lives in wgpu-hal, so this
  is a contained follow-up; it is flagged for the owner to validate alongside the
  on-hardware CADisplayLink pass (see ticket T-1.5 notes).

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
