# Changelog

User-visible (and contributor-visible) changes to aterm, newest first. Each entry
describes what someone would notice. Pure internal refactors don't appear here -
use `jj log` for the full history.

Semver: PATCH for fixes, MINOR for everything else. MAJOR (1.0.0+) is locked off
until the owner explicitly approves it - never auto-bump. The version of record is
`[workspace.package].version` in the root `Cargo.toml`. New entries go on top, under
the next version (or an `## Unreleased` heading until a version is cut).

## Unreleased

### Added

- **Virtualized block-timeline layout.** The block list is now published to the
  renderer and laid out as a single vertically-scrolling timeline, virtualized over the
  SumTree height index so a 10k-block history costs O(visible rows) per frame, not
  O(history): the index picks the blocks intersecting the viewport (O(log n)), then only
  the rows on screen within each become geometry. Each block carries a gutter status
  marker (running / exit-0 / failed-with-code / unknown / interactive / approximate),
  and long output collapses to a capped height with a "... +N lines" affordance - the
  collapse folded into the block's display height so scroll-to-block and the drawn
  layout share one coordinate space. A full-screen app (vim) switches the layout to a
  full-window alt-screen surface and leaves the scroll untouched, so exiting resumes the
  timeline in place. The engine publishes the live block list each time it changes; the
  renderer consumes it and reports a live visible-block count. Drawing the timeline
  cards on screen (replacing the raw grid) awaits finished-block output-row capture and
  EPIC-4 component styling; this lands the geometry, the virtualization, and the publish
  seam (ticket T-2.7).
- **Shell-integration status indicator.** aterm now surfaces a visible three-state
  integration status - Integrated / Heuristic / None - so it degrades loudly instead
  of silently (the prototype's worst sin). "Integrated" is shown only after a
  nonce-matched OSC-133 `A` confirms the shell's hooks are live; a supported shell
  whose hooks never fire falls back to clearly-labeled *approximate* command blocks
  (prompt-line heuristic); an unsupported shell honestly reports no integration. Each
  non-Integrated state carries a one-click "why?" (e.g. "shell-integration hooks did
  not load"), and the status transitions are observable as they happen. The engine
  publishes the live state; the renderer is handed it each frame with a glyph + tooltip
  presentation (the on-screen placement is EPIC-4 polish) (ticket T-2.6).
- **Command-block lifecycle.** The block segmenter now drives the full lifecycle from
  the shell's OSC-133 marks: a normal command cycle yields one finalized block (its
  command line taken from `cmdline=`), a Ctrl-C'd command auto-closes with an unknown
  exit when the next prompt arrives, an empty Enter creates no block, a no-output
  command collapses to a thin marker, and running a full-screen app (vim/htop)
  becomes a single compact "interactive" block instead of fragmenting into phantom
  blocks from the app's own marks. Marks now fire in lockstep with the grid - the
  engine interleaves VT parsing and mark-application by stream offset, so the
  alt-screen decision is made against the true emulator state (ticket T-2.5).
- **bash + fish shell integration.** Command-block marks now work in bash and fish
  too, not just zsh - same zero-dotfile-edit, nonce-stamped OSC-133 + OSC 7 contract.
  bash launches via a `--rcfile` bootstrap that reconstructs the normal startup
  sequence (preserving `/etc/profile`) then installs hooks last, version-branched:
  `PS0` + `PROMPT_COMMAND` on bash >= 5.3, a minimal `DEBUG`-trap preexec emulation on
  bash 3.2 - 5.2 (so macOS's system bash works, if less reliably). fish injects via a
  `vendor_conf.d` script on `XDG_DATA_DIRS` and hooks the `fish_prompt`/`fish_preexec`/
  `fish_postexec` events. The engine reports the detected shell (and whether
  integration is active) for the forthcoming status indicator; an unrecognised shell
  runs raw and reports as unknown (ticket T-2.3). All three shims percent-encode the
  command line byte-wise, so UTF-8 commands (accented paths, non-Latin text) round-trip
  exactly - which also corrects the zsh shim's command-line encoding.
- **zsh shell integration (command-block marks).** Launching aterm with zsh now
  installs a per-session `ZDOTDIR` shim - zero dotfile edits, restores the user's
  real config, removed on exit - that emits nonce-stamped OSC-133 A/B/C/D + OSC 7
  marks around the prompt and command. The engine arms its OSC filter with the
  shim's nonce, so command blocks segment reliably regardless of prompt theme and a
  foreign program's (un-nonced) marks are ignored (tickets T-2.2 + T-2.1).
- **Terminal query replies.** Programs that probe the terminal - Primary Device
  Attributes (`\x1b[c`), cursor-position / status (`\x1b[6n`) - now receive their
  answers: the VT engine writes the reply straight back to the PTY on the model
  thread instead of dropping it, so terminal-capability detection works (ticket
  T-1.9). The write is `poll(POLLOUT)`-guarded, so a program that floods queries
  while never reading its own input cannot deadlock the engine.
- **Foreground-process-group signalling (Unix).** `Engine::signal_foreground` and
  `Engine::foreground_pgid` let Ctrl-C / agent-cancel target the *running command's*
  process group (resolved via the terminal's foreground pgid), not the hidden shell.
  Guarded against signalling pgid <= 1 (which would hit our own group or init).

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
