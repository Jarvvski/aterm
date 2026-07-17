# Changelog

User-visible (and contributor-visible) changes to aterm, newest first. Each entry
describes what someone would notice. Pure internal refactors don't appear here -
use `jj log` for the full history.

Semver: PATCH for fixes, MINOR for everything else. MAJOR (1.0.0+) is locked off
until the owner explicitly approves it - never auto-bump. The version of record is
`[workspace.package].version` in the root `Cargo.toml`. New entries go on top, under
the next version (or an `## Unreleased` heading until a version is cut).

## 0.6.0 - 2026-07-17

### Added

- **Preferences now has its calm typographic surface.** Press `Cmd-,` to open four
  token-driven rows for theme, font size, default provider, and autonomy, with reusable
  segmented controls and a font-size stepper. `Esc` restores the previous terminal or
  editor view without disturbing it. The footer follows the package version automatically,
  and unchanged frames remain allocation-free. Control persistence follows in T-12.2.
  (Ticket T-12.1.)

## 0.5.0 - 2026-07-17

### Added

- **Editor mode now has its calm writing surface.** Opening a file renders a centered
  620px prose column with a quiet filename, dirty-state, word-count header, a mode-colored
  caret, selection, multiline editing, undo/redo, and native IME preedit. The document and
  unified input now share one editing implementation, while retained per-line layouts avoid
  full-document reshaping and unchanged frames allocate nothing. (Ticket T-11.2.)

## 0.4.0 - 2026-07-17

### Added

- **File-backed editor mode now has a complete lifecycle.** Launching `aterm <path>` loads
  that file into a pure document model with live dirty state and word count while folding
  the terminal timeline and input away without destroying them. `Cmd-S` saves through the
  app filesystem adapter, and `Esc` restores the terminal with its draft text and Shell or
  Agent routing target unchanged. Unsaved editor contents stay in memory on exit; the
  centered on-screen writing surface follows in T-11.2. (Ticket T-11.1.)

## 0.3.0 - 2026-07-17

### Added

- **Sessions now behave independently end to end.** `Cmd-T` or the sidebar `+` opens and
  activates a new PTY-backed session; clicking a row switches to it; the row close control
  or `Cmd-Shift-W` closes it and selects the previous neighbor. `Cmd-W` remains the native
  window-close shortcut. Draft text, selection, mode, history, completion state, and autonomy
  follow their owning session, and closing the last session opens a fresh launch session
  instead of leaving an invalid empty host. Closing a flooded background session also reaps
  its whole PTY process group without wedging later session cleanup. (Ticket T-10.3.)

## 0.2.0 - 2026-07-17

### Added

- **The sessions sidebar is live.** `Cmd-B` or the title-bar toggle opens a 210px panel
  listing every terminal session, with running/idle status, active-session highlighting,
  ellipsized names, hover close controls, and add/select/close intents ready for the
  keybinding follow-up. Opening it reflows the terminal into the remaining width without
  per-frame allocation. The title bar now follows the active session's name and working
  directory instead of displaying a placeholder. (Ticket T-10.2.)
- **The title bar's directory is live.** The path beside the title now tracks the shell's
  actual working directory (reported via OSC-7 at every prompt by the shell integration),
  so a `cd` shows up in the title bar at the next prompt - previously it was frozen at
  whatever directory aterm was launched from. Home still abbreviates to `~`; without shell
  integration it falls back to the launch directory as before.
- **One title bar with the native macOS transparent-titlebar chrome.** The doubled title bar
  is gone: the window now uses the same treatment as kitty/Slack/Linear - a native titled
  window whose titlebar is transparent, so aterm's slim custom bar (sidebar glyph, centered
  title + cwd, hairline) is the only visible bar while the REAL traffic-light buttons float
  over its left edge with native rounded corners and the native drop shadow. Close, minimize,
  and zoom are the genuine native buttons (hover states, Option-click and all); `Cmd-W` /
  `Cmd-M` also work from the keyboard, dragging the bar moves the window, and double-clicking
  it zooms. The bar matches the native titlebar height so the buttons, glyph, and title share
  one center line, and it stays visible even under a full-screen app (vim, htop): the terminal
  grid now sits below the bar, so no content ever hides under the buttons - where a stray
  click would have hit the close button. (Ticket T-9.9; reworked the same day from an initial
  fully-custom borderless frame with drawn dots, on owner direction.)
- **The mouse works now - hover and click affordances.** aterm tracked only keys and the
  scroll wheel; it now has a pointer. Hovering the title-bar sidebar glyph brightens it, the
  mode pill lifts its tint, a completion row tints under the cursor, and a command block reveals
  its status/duration meta on hover (it now stays hidden until you point at it, matching the
  mock). Clicking drives the exact same intent as the keyboard: the sidebar glyph == `Cmd-B`,
  the mode pill == `Cmd-/`, a completion row == selecting it and pressing `Enter`. The pointer
  turns into a hand over anything clickable. While a risk-gate approval is pending, the card is
  modal to the mouse too - no click can slip through to a control behind it. (Ticket T-9.8.)
- **A risk-gate approval card (the vision-mock `gate` state).** When the agent proposes a
  command the safety gate can't auto-run, a caution-bordered card now floats over the input:
  the proposed command (the argument always run through the secret redactor first), a `△` with
  a plain-language title and reason, and a split **Approve** button with a dropdown, a **Reject**
  button, and a keyboard hint. `Enter` approves once, `Esc` rejects; `↓`/`Tab` opens the
  dropdown, where `↑`/`↓` choose and `Enter` selects "Approve once" or "Always approve".
  "Always approve" widens this session's autonomy so future Caution commands stop asking -
  destructive and shell commands still always ask, and everything still runs inside the
  mandatory sandbox. The decision is recorded in the timeline as a `✓ Approved` / `✕ Rejected`
  line. (Ticket T-9.7.)
- **The agent turn reads like the vision mock now.** An agent turn in the timeline is styled
  as one grouped card: an agent-accent `◊` header carrying your request and an "agent - N steps"
  meta, an uppercase PLAN eyebrow over the opening plan, tool-call rows showing the tool name in
  the accent color beside its argument (a path, or the command - always run through the secret
  redactor first) with a right-aligned "+N -M" on an edit, tool output in a hairline-bordered
  block with `+`/`-` diff lines and FAILED/ok test results colored, and a closing summary set
  off by a hairline. A gated call still shows its `APPROVE?`/`BLOCKED` verdict inline; auto-run
  calls stay clean. (Ticket T-9.6.)
- **Launch + modes screens and a tab-completion popover (the vision-mock quiet states).** A
  fresh window now shows a centered "aterm" splash with a tagline instead of a blank timeline.
  Press `Cmd-?` for a one-input-two-destinations explainer (the shell/agent split). And `Tab`
  opens a fuzzy-finder popover that hugs the prompt: type to narrow it (matched letters glow in
  the accent color), `up`/`down` to move, `Enter`/`Tab` to accept (it fills the input), `Esc`
  to dismiss. Completions are drawn from the commands you've run this session for now; richer
  sources ($PATH, argument specs, persisted history) come later. `Tab` still reaches the shell's
  own completer when the finder has nothing to offer. (Ticket T-9.5.)
- **A custom window title bar (the vision-mock chrome).** The window now draws a slim title
  bar with a sidebar-toggle glyph and a centered active title plus the current directory
  (home shown as `~`), over a hairline rule; the timeline lays out below it. The sidebar
  panel arrives with multi-session support - `Cmd-B` flips the (stubbed) toggle-sidebar
  intent today. (Ticket T-9.2; the bar's height, window chrome, and traffic lights were
  finalized by the T-9.9 rework above - native buttons, 28px band.)

### Changed

- **The command timeline and input box now wear the vision-mock look.** Command blocks lead
  with an accent `❯` prompt glyph and carry a right-aligned meta - a status dot plus the run
  duration (a slower success turns the dot `success`-green; a failure shows "exit N · Ns" in
  `danger`). Plain command output is dimmed one step to `fg.secondary` so a block reads
  command-then-output, while any color a program prints is preserved. The input box gains the
  two-accent mode model: the prompt glyph (`❯` shell / `◊` agent), the caret, and the mode
  chip all tint to the current mode - shell blue or agent purple - and the chip is now a pill
  in the mode color showing the glyph, label, and the `⌘I` toggle hint. State stays legible
  without color (the "exit N" / "running" labels and distinct dot shapes). The meta renders
  always-on for now - its hover-reveal fade reserves the existing focus-dim slot (so the
  motion budget is unchanged), but the hover gating itself lands with the frame clock.
  (Tickets T-9.3, T-9.4.)
- **New warm color palette - aterm now matches its vision-mock look.** Both themes are
  reskinned to the warm two-theme palette adopted as the UI north star in ADR-0011: a warm
  "paper" light (`#FAF7EF`) and a warm near-black dark (`#1B1915`), with warmer semantic
  colors (success/caution/danger) and a warmer ANSI-16 set - visible today anywhere the
  grid, timeline, and input already draw. Agent mode gains its own second accent - a purple
  (`#7458BD` light / `#9D86D6` dark) alongside the shell blue - and a `mode_accent` resolver
  so widgets can ask for "the current mode color"; the always-blue-caret rule is superseded
  by ADR-0011. A new elevated-surface tone (`bg.elev`) backs popovers, the gate approve-menu,
  and the completion menu. Contrast was re-validated against a real WCAG computation; the one
  intentionally faint tone (meta/placeholder text) is documented with its permitted use. This
  is the token layer only: wiring the caret, prompt glyph, and mode chip to the mode accent -
  and the other surfaces to the new tokens - lands in the follow-up tickets (T-9.2+). (Ticket
  T-9.1.)

### Added

- **aterm now auto-discovers your MCP servers.** On startup it reads the standard `mcpServers`
  JSON config - a project `.mcp.json` (searched upward from the working directory) and a
  user-level file (the first that exists among `$HOME/mcp.json`, the `$XDG` locations, and
  `~/.claude.json`) - then automatically connects the local (stdio) servers and wires the remote
  ones through the Anthropic MCP connector, so the servers you already set up for Claude Code work
  in aterm with no extra config. Every discovered tool passes through the same risk gate as a
  built-in tool: local tools require confirmation, and remote servers are deny-by-default (no tool
  enabled until you allow-list it), so a discovered destructive tool never runs unattended. A
  project entry overrides a user one of the same name; `${VAR}` / `${VAR:-default}` are expanded.
  Disable a server with `"disabled": true` in the config or `ATERM_MCP_DISABLE=name1,name2`;
  discovered servers and their status are logged at startup. (Codex's `~/.codex/config.toml` is a
  deferred follow-up.) (Ticket T-6.3.)
- **Contributor: resize/reflow and the shell-integration matrix are now hardened with tests.**
  A finished command block's captured output is proven byte-identical after a window resize
  (only the live grid reflows - history is immune to alacritty's reflow bugs), and a new
  maximized-4K `maximized_reflow` bench scenario feeds the resize frame-budget gate. The
  shell matrix now runs real zsh / bash 5.3 / bash 3.2 to a first prompt and asserts each
  integrates (bash 3.2 integrates or honestly downgrades to a labeled "Heuristic"; fish
  skips when absent); starship/p10k/oh-my-posh-style un-nonced marks are proven to create no
  phantom or double blocks; `exec zsh` is shown to preserve integration; and `su`/`sudo -i`
  + tmux passthrough behavior (honest degradation in v1) is documented. (Ticket T-7.4.)
- **Contributor: input latency is now measured and gated separately from frame rate.** A new
  `latency_driver` (in `aterm-bench`) injects synthetic keystrokes into the real app loop and
  times keystroke->visible-glyph over ~120 iterations, reporting median + p25/p75 + outliers as
  JSON and gating on median <= 1.5 frames / p99 <= 3 frames at the active refresh - because a
  renderer can hold 60fps and still feel laggy. A nightly CI job runs it and fails on a breach.
  The measure + gate are a pure, headless-unit-tested module; the live software measure captures
  the present-scheduling pipeline (a lower bound), and the keyboard-to-photon hardware
  light-sensor rig is stubbed and documented for future ground truth. (Ticket T-7.3.)
- **You can now scroll back through the block timeline.** The mouse wheel / trackpad and the
  PageUp / PageDown keys scroll up into history; the view stays locked to the newest output (so a
  running command's tail is always visible) until you scroll away, and re-locks the moment you
  scroll back to the bottom - the standard terminal scroll-follow behavior. (Ticket T-7.2.)
- **Contributor: the "60fps always" proof now runs seven scripted stress scenarios.** A new
  `scenario_driver` (in `aterm-bench`) replays fast-scroll, output-flood, large-scrollback,
  agent-stream-while-typing, window-resize, fullscreen-TUI-redraw, and idle against the real app
  loop with the frame recorder installed, then emits a JSON report + a pass/fail verdict gated on
  the 60fps floor (16ms; 120fps tracked, non-blocking). A nightly CI job runs it and fails on a
  breach. The scenario set + gate are a pure, headless-unit-tested module; the live driver needs
  real hardware. (Ticket T-7.2.)
- **The input box now highlights your command as you type and suggests completions from your
  history.** In shell mode the command line gets restrained, near-monochrome syntax tinting
  (command vs argument vs flag) plus an error underline for mistakes like an unterminated quote -
  computed off the render thread and applied after a short (~90ms) debounce, and instantly on
  space or paste, so a burst of typing never stutters the frame. A fish-style ghost-text
  suggestion drawn from what you have run this session appears in muted gray after the caret and
  is accepted with Right or End at the end of the line. Switching between shell and agent mode
  re-styles the line at once (agent prose gets no shell highlight) without disturbing the text.
  (Tickets T-3.5, T-3.7.)
- **The input box now handles IME composition (Japanese, Pinyin, and other input methods).**
  Composing text shows the in-progress candidate inline under the caret, with the OS candidate
  window positioned there, and pressing Enter to confirm a candidate no longer submits the line
  or fires the agent - it just accepts the candidate (the classic terminal trap where Enter
  during composition inserted a newline). Committed text is inserted as ordinary characters, and
  losing focus mid-composition cleanly drops the half-composed text. (Ticket T-3.2.)
- **The agent can now use local MCP servers, fully on-device.** Point aterm at a local stdio
  MCP server (a filesystem server, a git server, your own project server) and its tools
  become tools the agent can call, no matter which model backend you run. aterm speaks the
  protocol itself - a small hand-rolled JSON-RPC-over-stdio client (no `rmcp` dependency) that
  spawns the server, lists its tools, and calls them - so nothing leaves your machine. Each
  MCP tool is registered as a first-class tool and goes through the exact same safety path as
  a native one: because its arguments are opaque, the gate treats every MCP call as
  needs-approval (it can never silently auto-run), its output is sanitized against your
  `Secrets` before it re-enters the conversation, and it shows up in the timeline like any
  other tool call. A native tool always wins a name collision, so an MCP server can never
  shadow or hijack a gated built-in. If a server crashes or hangs, the call comes back as a
  clean error (a closed pipe or a bounded per-call timeout) instead of wedging the turn.
  (Auto-discovering which servers to launch is a follow-up, T-6.3.) (Ticket T-6.2.)
- **The agent can now use remote MCP servers.** Point the Anthropic provider at a public
  HTTPS MCP server and its tools become available to the agent through the Messages-API MCP
  connector - Anthropic brokers the connection and runs the tools server-side (beta
  `mcp-client-2025-11-20`), so there is no local client to babysit. Each server is scoped by a
  **deny-by-default** per-tool policy: nothing is callable until you allow it by name, and a
  denylisted or unlisted tool is disabled in the request itself, so a destructive tool is
  gated off - never silently run. The connector's `mcp_tool_use`/`mcp_tool_result` blocks land
  in the same timeline as native tool calls (the result sanitized against your `Secrets`
  before it is shown, since a remote result is untrusted), and a malformed request (a server
  without its matching toolset) is caught locally instead of round-tripping to a 400. Note
  this path routes data through Anthropic and is NOT ZDR-eligible; privacy-sensitive users
  should prefer a local stdio server (T-6.2). (Ticket T-6.1.)
- **The agent actually runs now: ask it something and watch it work in the timeline.**
  Submitting a prompt to the agent (Enter in Agent mode, or Opt-Enter from anywhere) now
  starts a real client-side agentic turn on a background runtime - off the render thread, so
  the 60fps floor holds while it streams. Its steps land live in the same scrollback as your
  shell commands: your prompt, the model's thinking and prose (streamed in place, extending
  one block rather than relaying out the timeline), each proposed tool call with its risk
  badge, and each tool's sanitized result, interleaved by wall-clock. A proven-safe,
  non-shell-active tool auto-runs and shows an `auto` badge; a `Caution`/`Dangerous` or
  shell-active one parks the turn on the keyboard - the badge reads `APPROVE?`/`BLOCKED` with
  the parsed reason inline, and you answer with Enter/`y` (approve), `n` (deny, the turn
  continues), or Esc (cancel the whole turn). The approval seam is fail-closed: if you cancel
  or the turn dies, a parked call is denied, never run. The same `gate_tool` decides both the
  badge you see and whether the loop actually runs the call, so they can never disagree. With
  no API key set, a keyless mock turn drives the whole flow as a demo; with `ANTHROPIC_API_KEY`
  (or `OPENAI_API_KEY`) it runs a real Claude (or OpenAI) turn, sandboxed and gated. Key
  custody is still a follow-up (T-8.3). (Ticket T-5.11.)
- **Every gated command shows its risk verdict, and you control how much the agent runs on
  its own.** A proposed tool call now carries the deterministic risk gate's verdict as a
  badge in the timeline, paired ALWAYS with a text label (never color alone): a proven-safe
  command reads `auto`, an escalated one reads `APPROVE?`, and a destructive one reads
  `BLOCKED`, with the parsed reason ("deletes or overwrites files", ...) shown inline. An
  escalated command blocks the turn on an explicit Approve/Deny - the loop parks on a
  fail-closed approval channel (if the approval is dropped or the UI is gone, the command is
  DENIED, never run) until you answer. Autonomy is graduated and always visible as a chip
  next to the SHELL/AGENT routing chip: `ask-always` (confirm everything), `auto-safe` (the
  shipped default - a proven-safe, non-shell-active command auto-runs), and a session-scoped
  `auto-run` widening; `Cmd-Shift-A` cycles the tier and it takes effect on the next command.
  Two safety invariants hold in EVERY tier and can never be widened: a command with a
  shell-active reason (a pipe, redirect, `$(...)`, `&&`) never auto-runs, and a `Dangerous`
  command never auto-runs. A widening is session-scoped: a new session reverts to the
  AUTO-SAFE baseline, so a loosened posture never silently persists. The badge data rides
  into the timeline as an agent-domain-free projection, so the engine and renderer crates
  still name no agent type. (Ticket T-5.11.)
- **An agent turn now lives in the same timeline as your commands.** The agent's work is
  modelled as a transcript of timestamped steps - your prompt, the model's thinking and
  prose, each tool call (with the deterministic gate's decision), its approval, and its
  sanitized result - and those steps render as blocks interleaved by wall-clock with your
  shell command blocks in one scrollback, so a long-running tool call sits in order next to
  whatever you typed meanwhile. Streaming is incremental: a new chunk of assistant text
  extends the current entry in place and redraws only that entry, never relaying out the
  whole timeline (the 60fps floor holds while the model streams). The transcript keeps two
  separate views that never bleed into each other - the rendered timeline (glossed risk,
  approval state, sanitized output) and the API conversation history sent back to the model
  (raw assistant + `tool_result` blocks); the derived history is a valid provider
  conversation that round-trips, and per-turn token usage is attributed to the turn.
  Internally this turned the timeline's block model into a proper variant type (a command
  block or an agent step) while keeping the agent-domain types out of the engine crate. The
  on-screen card styling + approval controls ride EPIC-4 / T-5.11. (Ticket T-5.10.)
- **The agent can now actually run its tools - safely.** The execution sinks the turn loop
  dispatches to are implemented: `run_command` runs the argv as a subprocess with NO shell
  (so a `|`, `>`, `$(...)`, or `~` in an argument is an inert literal, never interpreted) and
  is wrapped by the mandatory OS sandbox; the filesystem tools (`read_file`, `edit_file`,
  `write_file`, `list_dir`, `glob`, `grep`) run in-process and apply the gate's path checks
  themselves - they refuse to touch any credential path in the `Secrets` deny-set (so a
  secret file's contents never enter the result at all) and confine every write to the
  workspace root (a write escaping via an absolute path, `..`, or a symlinked parent is
  denied). `edit_file` makes an exactly-one-match replacement and rejects a stale edit (a
  file changed on disk since the agent last read it), and writes are atomic (temp file +
  rename). A separate, harder-gated path injects a command into the live interactive shell:
  because a real shell interprets it, any shell-active command must be confirmed even when it
  would otherwise rate safe. Raw output is captured and returned; the turn loop sanitizes it
  against the same `Secrets` source before it re-enters the model's context. (Ticket T-5.9.)
- **The block timeline now breathes like iA Writer instead of a dense terminal.** The
  command/output blocks get a generous horizontal gutter and top/bottom canvas breathing
  room (no longer flush to the window edge), a full blank line of whitespace between
  adjacent blocks, and the command line + output are padded in from the gutter. Block
  boundaries are marked by exactly one faint hairline, centered in the inter-block
  whitespace - the previous doubled/edge lines are gone, and whitespace (not a heavy rule)
  is the primary separation. The inter-block gap is part of the timeline's scroll geometry
  (scroll extent, scroll position, and hit-testing all account for it), not a paint-time
  cosmetic, so scrolling stays exact. The raw-VT grid fast-path keeps its own tight inset
  and is unchanged. (Ticket T-4.7.)
- **Agent-run commands are now confined by a mandatory OS sandbox.** Before a command the
  agent proposes can run, it is wrapped in a macOS Seatbelt profile (`sandbox-exec`) generated
  on the spot: it may write only inside the project/cwd (a write to `$HOME` or `/tmp` is
  denied), it cannot read OR overwrite any credential path from the single `Secrets` deny-set
  (`~/.ssh`, `~/.aws`, `.env`, `.git-credentials`, ... - even one living inside the project),
  and outbound network is denied by default (only local IPC kept; an explicit allowlist can
  punch holes). On top of that, every confined command runs under `setrlimit` caps (CPU time,
  address space, open files) and a wall-clock timeout that kills the whole process group, so a
  runaway cannot hang or fork-bomb the machine. This is the OS boundary beneath the risk gate -
  the gate is a classifier, this is the enforcement - and it is mandatory because the autonomy
  default is auto-safe. It sits behind a `Sandbox` trait so a future backend can replace the
  (deprecated-but-only-documented) `sandbox-exec`. Not yet wired to the agent's command tool
  (that is the execution sinks, T-5.9); this lands the boundary itself. (Ticket T-5.7.)
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
