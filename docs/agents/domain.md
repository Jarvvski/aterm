# Domain vocabulary

The shared language of aterm. Use these exact terms in ticket titles, type names, test names, comments, and proposals. When the dossier and this glossary agree, the dossier holds the long explanation; this file holds the canonical short definition. Sources are the research docs under `docs/research/`.

## Terminal & timeline

**Block** - the core display primitive. One block corresponds to one command's lifecycle: the prompt, the typed command, and that command's output, segmented as a single one-grid-per-command unit rather than a raw scrolling VT grid. Finished command blocks store immutable per-row snapshots so history is immune to reflow. (`01-warp-internals.md`, `03-pty-vt-rust.md`)

**Command block** - a `Block` whose content is a shell command and its output (the default case). Contrast with agent-step blocks; both are variants in the same `BlockList`.

**Timeline** - the single, wall-clock-ordered list of blocks shown to the user. Human command blocks and agent conversation/step blocks live in the SAME list, sorted by timestamp. This one-block-primitive, single-timeline design is the structural reason human and agent activity interleave with no special-casing. A `SumTree` height index gives O(log n) viewport queries. (`01-warp-internals.md`, `06-agent-architecture.md`)

**BlockList** - the model-thread-owned collection of blocks plus the height index; it publishes immutable snapshots + dirty regions to the renderer.

**Grid** - the VT character grid for the in-flight command, owned by `alacritty_terminal`'s `Term`. The live grid is mutable; once a command finishes (OSC-133 D), its rows are snapshotted into the block.

## Shell integration marks

**OSC-133 marks (A / B / C / D)** - the FinalTerm "semantic prompt" protocol aterm consumes as its canonical command-lifecycle signal. Command boundaries come from these hook-emitted marks, NOT from VT inference. (`04-shell-integration.md`)
- **A** = `FTCS_PROMPT`: sent just before the shell prints its prompt; opens a new prompt region.
- **B** = `FTCS_COMMAND_START`: just after the prompt, before user input; the boundary between prompt text and the typed command.
- **C** = `FTCS_COMMAND_EXECUTED`: just before output starts; the command was accepted and is running. May carry `cmdline=` (the percent-encoded command text).
- **D** = `FTCS_COMMAND_FINISHED [; <exit>]`: command finished; closes the block and attaches the optional exit code. A D right after C/B with no output signals an aborted/empty command.

**OSC-7** - the current-working-directory mark: `OSC 7 ; file://<host><abs-path> ST`. The host lets aterm distinguish a local cwd from a remote/SSH one. Sets the cwd for the upcoming (or, from `precmd`, the next) block.

**Nonce gating** - every mark aterm trusts must carry a per-session nonce (`tag=NONCE`); marks whose nonce does not match are dropped. This defeats nested-shell and prompt-framework spoofing (e.g. a stray starship/p10k mark). The same nonce guards the `cmdline=` field.

**Shell shim** - the integration script loaded into the user's shell via env-var indirection (zsh `ZDOTDIR`, bash `ENV`/`--rcfile` or the bundled `bash-preexec` shim, fish `XDG_DATA_DIRS` vendor_conf.d) - NEVER by editing the user's dotfiles. It installs the preexec/precmd-style hooks that emit the OSC-133/7 marks.

**Integration indicator** - the visible three-state status (`Integrated` / `Heuristic` / `None`) surfaced to the user; aterm degrades loudly, never silently. (`04-shell-integration.md`)

## Unified input

**InputModel** - the single pure-Rust reducer backing the one shell-first input box. It holds `text` + `selection` + a `mode: Shell | Agent` field. The mode-toggle hotkey mutates ONLY `mode`, so typed text is preserved across the toggle by construction. There is no second input and no sigil scheme. (`05-unified-input-ux.md`)

**Mode (Shell | Agent)** - which destination Enter routes to. Shown by caret tint + prompt glyph (no banner). Shell mode sends the line to the live shell; Agent mode sends it to the agent turn loop.

**Routing** - the decision, made by the routing brain in `aterm-app`, of where a submitted line goes, gated by `mode` and the IME `preedit-active` state (Enter confirms an IME candidate and never submits while a pre-edit is active).

## Agent & safety

**LlmProvider** - the trait seam abstracting the LLM backend. `AnthropicProvider` (default, `claude-opus-4-8`, Messages API) and `OpenAiProvider` (Responses API) both implement it, behind a provider-neutral event mapper and one shared turn loop. (`06-agent-architecture.md`)

**Turn loop / agentic loop** - the client-side manual loop: plan -> act (call a typed tool) -> observe (tool result) -> repeat, streaming over SSE and looping while `stop_reason == "tool_use"`. Runs on a tokio runtime off the render thread.

**Tool** - a typed custom tool the agent may call: `run_command`, `read_file`, `edit_file`, `list_dir`, `glob`, `grep`. Tools take argv, not a shell string. Every `run_command` call is gated and sandboxed.

**Risk gate** - the deterministic, code-side (never the prompt) classifier that parses a command's argv (zsh-aware) and returns a verdict before execution:
- **Safe** - provably benign; auto-runs by default (Auto-Safe ON) *only if* it carries no shell-active reason.
- **Caution** - always requires explicit confirmation.
- **Dangerous** - always requires explicit confirmation.
The gate over-approximates toward RequireConfirm: when in doubt, it does not return Safe. It is a classifier, not a security boundary - the Seatbelt sandbox is the real boundary. (`06-agent-architecture.md`)

**Sandbox** - the trait wrapping the mandatory OS-level execution boundary. v1 backend: macOS Seatbelt via `sandbox-exec`, plus `setrlimit` + timeout-kill. The trait lets a future native-API/VM backend swap in if Seatbelt is pulled.

**Secrets** - the single source of secret/sensitive-path knowledge. It feeds BOTH the risk gate (the sensitive-path deny-set) and the `OutputSanitizer`, so the two cannot drift.

**OutputSanitizer** - redacts secret values from command output *before* truncation, so secrets never reach the model context or the transcript. Also part of the layered prompt-injection defense (the agent reads untrusted command output).

**Sink** - an execution sink: the side that actually runs a gated, sandboxed command (or applies an edit) and returns the observed result to the turn loop.
