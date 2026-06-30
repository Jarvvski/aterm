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

- **Keys now reach full-screen apps and running commands correctly.** When a full-screen
  program (vim, less, htop), a running foreground command, or a shell with no integration
  owns the terminal, keystrokes are passed through and encoded to the right PTY bytes -
  arrows, `Ctrl-C`/`Ctrl-Z`, Home/End/PageUp/PageDown, the function keys, and DECCKM
  application-cursor mode (arrows switch to `SS3`) - via the key encoder, instead of the
  previous stub that sent nothing for most of them. The terminal also now distinguishes "the
  shell is at its prompt" from "a foreground command is running" (by comparing the live
  foreground process group to the shell's), so while a command runs your typing goes to that
  program rather than the input box. (Ticket T-3.3.)
- **The mode-toggle hotkey and `Opt-Enter` work for real.** Pressing `Cmd-/` now flips the
  input between Shell and Agent routing with the typed text preserved (the prompt glyph +
  SHELL/AGENT chip change; the caret stays accent-blue) - it is the real chord now, not the
  old `Tab` stand-in, so `Tab` is freed and again requests shell completion. `Opt-Enter`
  sends the current line to the agent regardless of mode (the one-shot-to-agent). The toggle
  chord is rebindable without a rebuild via the `ATERM_TOGGLE_KEY` env var (e.g.
  `ATERM_TOGGLE_KEY=ctrl+t`); the full `config.toml` keybinding loader lands later. This is
  carried by a new modifier-aware key seam, so the app finally sees Cmd/Opt/Ctrl/Shift on a
  keystroke. (Ticket T-3.3.)
- **The unified input box is drawn (the live command line + iA mode indicator).** aterm's
  single shell-first input field now renders as a persistent bottom footer: a hairline
  separates it from the timeline, a mode-carrying prompt glyph (a `❯` chevron for Shell, a
  Nerd-Font "sparkles" icon for the agent) sits at the left edge, the typed command line
  draws in Mono with a thin 2px accent-blue caret (the caret stays the one accent in BOTH
  modes), and a small right-aligned SHELL/AGENT chip carries the routing target. When the
  buffer is empty a muted placeholder ("Type a command" / "Ask the agent") reinforces the
  mode; a selection paints with the selection color, the fish-style ghost-text tail draws
  muted after the line, an inline IME preedit underlines, and the async syntax-highlight
  overlay tints the line - all reading the input model the session already owns and drives,
  so agent-mode typing (previously invisible) now shows on screen. Toggling the mode swaps
  the prompt glyph + chip with the typed text preserved and no reflow. The box draws over a
  reserved bottom zone (the timeline viewport shrinks to sit above it; the box is hidden
  while a full-screen app owns the screen), through the shared glyph atlas as one rect plus
  one glyph draw, damage-gated so an idle present allocates nothing - in both themes.
  (Ticket T-3.6.) The mode-toggle hotkey + routing (T-3.3), history (T-3.7), and the
  async highlight/ghost (T-3.5) and IME preedit (T-3.2) feeds wire in under their own
  tickets; the `motion.fast` chip cross-fade and the on-hardware iA visual review are the
  remaining residuals (a frame clock for live motion, and the owner-watched visual pass).
- **The block timeline is now drawn on screen (iA component styling).** The renderer no
  longer falls back to the raw VT grid in normal use: it composes the Warp-style block
  timeline from the published block model - a left-gutter status marker (running =
  pulsing accent dot, exit-0 = success tick, exit≠0 = danger dot + code, heuristic =
  caution half-dot), the re-rendered command line, the captured output rows, hairline
  separators, and a "... +N lines" collapse affordance - all styled to the iA spec from
  `aterm-tokens` with no hardcoded colors, in both themes. A new token-driven component
  layer reifies the five component specs (command block, prompt routing chip, agent
  card, status chip, risk-gate badge) as pure, theme-aware style descriptors; the
  risk-gate badge always pairs a text label with its color across all three states
  (Allowed / Needs-approval / Blocked) for color-blind safety, and motion is capped to
  the three allowed animations (block insert, cross-fade, focus dim), each ≤ 220ms. The
  raw grid is now drawn only for full-screen (alt-screen) apps. The shared `GlyphAtlas`
  and its rect + glyph pipelines were hoisted up so the grid, prose, and timeline
  front-ends all draw through one atlas; the grid's 60fps invariants (single glyph draw
  call, rasterize-once, zero-alloc steady-state present) are preserved, and the timeline
  path is damage-gated so an idle present allocates nothing. (Ticket T-4.6.)
- **A running command streams its live output into its block.** The engine now captures
  the in-flight command's output incrementally and publishes it into the running block
  each tick, so the timeline shows a command's output as it streams (not only after it
  finishes); the active/running block shows its full output uncollapsed (tail-visible,
  like a live terminal), while finished blocks collapse long output to keep scrollback
  tidy. (Ticket T-4.6.) Composing the agent-card Duo prose body + Quattro chrome chips
  and the live unified-input prompt into the timeline follows their data/widget tickets
  (T-5.10, T-3.6); the on-hardware iA visual review is the owner-watched acceptance step.
- **Three-register fonts: Duo prose + Quattro chrome over one shared glyph atlas.** The
  agent-prose register (iM Writing **Duo**, duospace) and the dense-chrome register (iM
  Writing **Quattro**, four widths) now load and render through a real proportional text
  path - full swash shaping (clusters, kerning, ligatures where the face carries them)
  plus greedy word-wrap at the prose measure (`MEASURE_CH` = 72 `ch`, the advance of
  '0'). The terminal grid stays iM Writing **Mono** and uncapped. The glyph atlas, glyph
  cache, rasterizer, and glyph GPU pipeline were extracted into a shared `GlyphAtlas` so
  the grid and prose front-ends share one atlas + one instanced draw pipeline (one
  shaping engine, two layout front-ends) - the grid's 60fps fast path (zero-alloc
  steady-state present, single glyph draw call, rasterize-once) is preserved unchanged.
  Measured Duo/Quattro metrics are documented and the bundled set is confirmed
  OFL-1.1-clean. Composing prose into the live timeline / agent cards is a follow-up
  (T-4.6). (Ticket T-4.3.)
- **Nerd Font icons align in the grid cell (per-codepoint constraint table).** PUA
  icon glyphs - Powerline symbols, Devicons, Font Awesome, Seti, Weather, Octicons,
  Codicons, Pomicons, and Material Design Icons (including the beyond-BMP `U+F0000+`
  range) - are now scaled and centered to fit the terminal cell instead of rendering
  small, squished, or off-cell at the font's native size, and the Powerline-extra
  separators stretch edge-to-edge so they tile seamlessly. Each codepoint is matched
  to a fit-and-center or stretch-to-fill directive grounded in the bundled face's
  actual charset; box-drawing, blocks, braille, and the Powerline triangles stay with
  the procedural sprite face. Ordinary text placement is unchanged and the
  steady-state frame build stays allocation-free. (Ticket T-4.4.)
- **Procedural sprite face for box-drawing, blocks, braille, and Powerline.** Box
  lines/corners/junctions (`U+2500..` light + heavy), block elements + quadrants +
  shades (`U+2580..259F`), the full braille dot matrix (`U+2800..28FF`), and the
  Powerline separators (`U+E0B0..E0B3`) are now DRAWN directly into the glyph atlas
  rather than taken from the font outline. They are pixel-perfect and seamless
  regardless of which font is active: box lines tile edge-to-edge with no inter-cell
  gaps, Powerline triangles are font-independent, and braille/block art (e.g. btop
  graphs) renders crisply. Each sprite is rasterized once and cached like any glyph.
  The rarer variants (mixed light/heavy junctions, double-line, arcs, diagonals,
  dashes) still come from the font, unchanged. (Ticket T-4.5.)
- **Theme-tuned ANSI palettes + runtime theme switching.** Terminal output now
  resolves its ANSI colors through the active theme's 16-color palette (the warm
  "paper" light set and the dark set), with the full xterm 256-color space (the
  6×6×6 cube and 24-step grayscale ramp) resolved in one place. The theme can switch
  at runtime - via an explicit toggle or by following the macOS appearance
  (`WindowEvent::ThemeChanged`, opt-in) - and grid colors re-resolve live against the
  new palette with no grid reallocation. On the light "paper" background the
  saturated bright ANSI colors (bright cyan/yellow especially) are remapped at render
  time to stay legible (a minimum 3:1 contrast against the canvas); this is a
  renderer adjustment only - the design-token palette values are unchanged. The dark
  theme is left as-is (its dim colors are intentional). (Ticket T-4.2.)
- **GPU instanced grid renderer (the typing-lag cure).** The terminal grid now draws
  through a custom wgpu instanced atlas pipeline instead of the interim glyphon
  whole-buffer reshape. Each unique glyph is rasterized once (swash) into a shared
  8-bit alpha atlas; the whole visible grid then renders as one background pass plus a
  single instanced glyph draw call, with per-cell foreground/background color,
  bold/italic faces, underline, inverse, and wide (two-column) cells. This removes the
  per-keystroke full-grid reshape that made typing feel sluggish (seconds per keystroke
  with Nerd Font icon glyphs on screen). The instance build is gated on a cheap
  `(snapshot version, viewport, theme)` signature, so an unchanged frame reuses the
  GPU buffers with zero work and zero allocation - the steady-state present allocates
  nothing. Grayscale AA only (no LCD subpixel). Verified by offscreen
  render-to-texture pixel tests on Metal (ticket T-1.8, completing the T-1.6 GPU half).
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

### Fixed

- **Timeline gutter status markers now render (they were invisible/indistinct boxes).**
  The command-block gutter markers (running / ok / failed / unknown / interactive /
  heuristic) used BMP geometric glyphs (`●` `✓`-aside `○` `◐` `▸`) that are NOT in the
  bundled iM Writing Mono Nerd Font - five of the six resolved to `.notdef` and drew as
  identical boxes, collapsing the at-a-glance status distinction (only the success tick
  was correct). They now use present Nerd-Font icons (`nf-fa-circle` / `-check` / `-circle-o`
  / `-caret-right` / `-circle-half-stroke`), auto-centered into the cell by the per-codepoint
  constraint table, with a cross-platform guard test asserting every gutter glyph resolves
  to a real (non-`.notdef`) glyph in the bundled face. (Ticket T-4.6.)

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
