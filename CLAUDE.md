# CLAUDE.md - aterm project memory

This file is loaded into every agent's context. Keep it tight, high-signal, and current. It is the working contract; the full reasoning lives in `docs/research/` (start at `00-overview.md`).

## What aterm is

aterm is a native, GPU-rendered macOS terminal in Rust. It clones the *behavior* of Warp - a controlled native UI wrapping a hidden background shell over a PTY, rendering its own block-based timeline instead of a raw VT grid - but with the radically minimal visual language of iA Writer. One shell-first input box (a hotkey flips where Enter routes - live shell vs. the AI agent - while preserving typed text) drives either the shell or a full client-side agentic loop backed by an LLM, with a deterministic risk gate and OS sandboxing. The headline non-functional requirement, against which everything is judged, is a guaranteed **60fps floor** for normal use (typing, scrolling, streaming output), ideally 120fps on ProMotion. macOS-first (Apple Silicon, Metal), GPLv3.

## Architecture spine (6 crates, one-way dependency arrow)

```
aterm-app  ->  { aterm-ui, aterm-agent }
aterm-ui   ->  { aterm-core, aterm-tokens }
aterm-agent -> aterm-core
aterm-bench -> { aterm-core, aterm-ui }
aterm-tokens, aterm-core : leaves (no internal deps)
```

No dependency cycles. The boundary is enforced in CI by `cargo deny` (graph/bans), not by convention alone - e.g. `aterm-core` must never pull in `aterm-agent` or any LLM SDK.

| Crate | Role | Depends on |
|---|---|---|
| `aterm-core` | Engine. PTY spawn/resize/signals (`portable-pty`), VT/ANSI parse + grid (`alacritty_terminal` 0.26, the published crate - NOT Zed's fork), the block model, OSC-133/OSC-7 mark interception + nonce gating, the shell-integration shim extraction, the pure unified-input `InputModel` reducer. No UI, no LLM. | none |
| `aterm-tokens` | Design tokens (colors, spacing, type scale, font names) as typed Rust. | none |
| `aterm-agent` | `LlmProvider` trait + `AnthropicProvider` + `OpenAiProvider`, the provider-neutral event mapper, the agentic turn loop, the deterministic risk gate (zsh-aware argv parse), the single `Secrets` source, the `OutputSanitizer`, command-execution sinks, the `Sandbox` trait. | `aterm-core` |
| `aterm-ui` | The renderer SEAM. winit windowing, the wgpu device/surface, the cosmic-text/swash glyph atlas + grid fast-path, layout/hit-testing/focus/IME, the timeline/block/input widgets, damage tracking, the CADisplayLink-driven present loop. | `aterm-core`, `aterm-tokens` |
| `aterm-app` | The binary `aterm`. Wires ui+agent+core, owns the window + the 3-thread model, config load, the unified-input routing. | `aterm-ui`, `aterm-agent` |
| `aterm-bench` | criterion + iai-callgrind harnesses; the scripted 60fps stress scenarios. | `aterm-core`, `aterm-ui` |

## The 3-thread model (one paragraph)

Three threads over bounded mailboxes (convergent across every fast native terminal; see `03-pty-vt-rust.md`, `09-performance-60fps.md`). The **PTY reader thread** does a blocking `read()` into a reusable ~64 KiB buffer and sends bytes over a *bounded* channel - bounded gives implicit backpressure, so a `cat`/`yes` flood blocks the reader, which lets the kernel PTY buffer apply flow control; it never touches the GPU. The **model thread** owns the `Term`, grid, and `BlockList`; it drains the channel through the OSC-133/7 pre-parser filter, then the VT parser, mutates blocks, and publishes an immutable snapshot + dirty regions, **coalescing** bursts on a ~4-8ms tick so a megabyte burst becomes one parse pass and one frame. The **render thread** is driven by the display vsync callback (self-bridged CADisplayLink), reads the latest snapshot, draws at 60/120fps, and never blocks on the model or PTY. The agent runs on a tokio runtime *off* the render thread; SSE deltas land by channel and mutate the current timeline entry incrementally.

## Locked decisions (authoritative - do NOT relitigate)

| Area | Decision |
|---|---|
| Render stack | Custom **wgpu + parley/cosmic-text + swash** (the "Warp path"), behind the thin internal `aterm-ui` seam so the renderer stays swappable. No spike gate; build directly on wgpu. GPUI is a theoretical fallback only, not used. The 60fps floor is an architectural property we own (vsync render loop on self-bridged CADisplayLink, damage tracking, PTY/model/render decoupling, zero per-frame allocation, present-early + ~1s keep-warm). `aterm-bench` is the standing proof. |
| Input | **One shell-first input box.** A hotkey toggles where Enter routes (live shell vs. agent); typed text is PRESERVED across the toggle. A pure `InputModel` reducer holds text + selection + a `mode: Shell\|Agent` field; the hotkey mutates only `mode`. Visible mode indicator (prompt glyph + mode chip; the caret stays one accent blue - recoloring it per mode is an owner-confirm alternative), no banner. NOT a sigil scheme. |
| Agent | **Full agentic from day one** - a client-side manual loop (plan -> act[run_command/read_file/edit_file/list_dir/glob/grep] -> observe -> repeat) calling the LLM Messages API directly over HTTP (reqwest + tokio + SSE + serde). Default `claude-opus-4-8` with adaptive thinking + the `effort` param (NOT `budget_tokens`); stream over SSE; loop on `stop_reason: "tool_use"`. Reject the Agent SDK (GPLv3 conflict). Managed Agents out of scope. |
| Providers | **Multi-provider seam in v1** - `AnthropicProvider` AND `OpenAiProvider` behind one `LlmProvider` trait, with a provider-neutral event mapper and one shared turn loop. Default provider: Anthropic Claude (`claude-opus-4-8`); OpenAI uses the Responses API. |
| Autonomy | **AUTO-SAFE ON by default.** Commands the deterministic risk gate proves `Safe` (and that carry no shell-active reason) auto-run; `Caution`/`Dangerous` always require explicit confirmation. Because the default trust surface is larger, the gate over-approximates toward RequireConfirm, and a macOS Seatbelt (`sandbox-exec`) sandbox is MANDATORY (behind a `Sandbox` trait), plus `setrlimit` + timeout-kill. A single `Secrets` source feeds BOTH the gate (sensitive-path deny-set) and the `OutputSanitizer`. |
| Platform | **macOS-first** (Apple Silicon, Metal). Linux/Windows NOT precluded (`portable-pty` + the renderer trait keep the door open) but NO v1 work on them. |
| License | **GPLv3.** Bundle iM Writing Nerd Font under SIL OFL 1.1 (keep the renamed family, ship OFL text + copyright). |

See `00-overview.md` for the full rationale and the corrected-fact register. If your output would contradict a locked decision or an ADR, STOP and flag it - never silently override.

## Toolchain

- **mise** owns tasks and tool pins. Use these (do not hand-roll cargo invocations):
  - `mise run run` - run the app (`cargo run -p aterm-app`)
  - `mise run build` - `cargo build --workspace`
  - `mise run test` - `cargo test --workspace`
  - `mise run fmt` - `cargo fmt --all`
  - `mise run lint` - clippy across all targets (`cargo clippy --workspace --all-targets`)
  - `mise run bench` - the 60fps harness (`cargo bench -p aterm-bench`)
- **rustfmt** (`cargo fmt --all`) + **clippy** with warnings as errors. Lints live in `[workspace.lints]`; each crate opts in via `[lints] workspace = true`. Cherry-pick from `pedantic`, do not enable the whole group.
- **cargo deny** enforces the license denylist (deny GPL/AGPL on dependencies - aterm itself is GPLv3 but must not *link* AGPL/other-GPL object code; this is the rule that ruled GPUI out) and the crate-boundary graph.
- Rust edition 2021. `Cargo.lock` is committed (this is an app, not a library).

## Landing a change (jj, NOT git)

The owner uses **Jujutsu exclusively** in a colocated repo. **Never invoke `git`** - it can corrupt jj's operation log. jj has no staging area; the working copy is always a commit.

1. Make **one focused change** per commit.
2. Run `mise run fmt && mise run lint && mise run build && mise run test`; land only when all pass. (jj fires no git hooks, so fmt is a task, not a pre-commit hook.)
3. For user-visible changes, bump the version + add a dated `CHANGELOG.md` entry in the *same* commit.
4. `jj describe -m "<imperative one-liner>"` then `jj bookmark set main --to @` then `jj new`.
5. Invariant: every time `main` moves, the next command is `jj new`, so `@` is always an empty commit one above main.

**Versioning & changelog.** The version of record is `[workspace.package].version` in the root `Cargo.toml`. Semver: PATCH for fixes, MINOR otherwise; **never bump to 1.0.0 (or any MAJOR) without the owner's explicit approval - do not auto-bump.** A user-visible change adds a dated `CHANGELOG.md` entry (newest first) in the same commit; skip pure internal refactors. Push with `jj git push --bookmark main` (`--allow-new` the first time a bookmark is pushed).

Remote: `origin` = `Jarvvski/aterm` (SSH: `git@github.com:Jarvvski/aterm.git` - SSH avoids the OAuth `workflow`-scope gate on pushing `.github/workflows/`). `gh` auto-detection fails in jj workspaces - always pass `-R Jarvvski/aterm`.

## Testing conventions

- Pure logic (`aterm-core`, `aterm-agent` - the risk gate, secrets, sanitizer, event mapper) is heavily unit-tested with **no network and no window**; these tests run on Linux runners too.
- Anything touching the PTY or the window/GPU is macOS-only (CI runs on `macos-14` Apple Silicon).
- The risk gate, the single `Secrets` source, and the `OutputSanitizer` are crown-jewel safety logic - changes there require tests covering the new case before landing.
- Perf: `aterm-bench` Tier-1 iai-callgrind instruction-count micro-benches gate every PR (noise-immune on shared runners); the Tier-2 in-process frame recorder runs the scripted stress scenarios nightly on real ProMotion hardware. Track the 60fps floor as the hard gate, 120fps as informational.
- Agents use the exact domain vocabulary from `docs/agents/domain.md` in test names, ticket titles, and proposals.

## Where things live

- **Design tokens**: `aterm-tokens` crate (typed Rust); design source in `docs/design/`.
- **ADRs**: `docs/adr/NNNN-title.md` - one decision per file. Flag contradictions; never silently override.
- **Research dossier**: `docs/research/` (`00-overview.md` is the exec summary; `01..12` are the domain docs; `_gaps.md` is the completeness critique). Read the relevant doc before writing; cite it (e.g. "see `03-pty-vt-rust.md`") rather than restating.
- **Ticket backlog**: `docs/tickets/` - roster in `INDEX.md`; one file per ticket at `EPIC-N-<slug>/TICKET-<id>-<slug>.md`. Convention in `docs/agents/issue-tracker.md`; `status` uses the triage labels in `docs/agents/triage-labels.md`; ordering/blocking via each ticket's `depends_on`.
- **Orientation**: `CONTEXT.md` (vision + docs map). **Glossary/vocabulary**: `docs/agents/domain.md`.
- **Changelog / contributing / security**: `CHANGELOG.md` (newest first; version of record is `Cargo.toml` `[workspace.package].version`), `CONTRIBUTING.md` (jj workflow + crate boundaries), `SECURITY.md` (the agent threat model).
