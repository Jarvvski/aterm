---
title: Competitive Landscape & Positioning
domain: competitive-landscape
status: research
---

# Competitive Landscape & Positioning

## TL;DR

- **Warp is the direct precedent and now a partial open-source one.** It is the only shipping product with aterm's exact shape: a controlled native UI over a hidden PTY, command-as-data "blocks", a full agentic loop (Agent Mode + Cloud Agents), and a hand-rolled Rust+Metal renderer hitting >144fps / ~1.9ms average redraw [1][2][3]. On 2026-04-28 Warp open-sourced its client under **AGPL-3.0** (with some UI crates MIT); server/AI/cloud stays proprietary [3][4]. This validates the technical bet but raises the bar: aterm cannot win on "blocks + AI in Rust" alone - Warp already owns that. aterm differentiates on **radical iA-Writer minimalism + GPLv3 + a single shell-first input** vs Warp's dense, busy, telemetry-heavy UI.
- **The renderer model to copy is Warp's / Zed's, not a terminal-grid renderer.** Warp built its own retained-mode Rust UI framework (primitives -> elements) on Metal because no Rust GUI toolkit supported Metal at the time; the same author (Nathan Sobo) later shipped **GPUI** (the framework behind Zed), which is the modern, open option for exactly this [1]. Pure terminal-grid engines (Ghostty, Kitty, Alacritty, WezTerm) are NOT the right reference architecture for a block UI - they render a character grid, not a document of UI elements.
- **For raw terminal-emulation correctness and speed, Ghostty is the gold standard to learn from but not to clone.** Zig + native Metal, multi-threaded (read/write/render threads per surface), SIMD VT parser, ~2-5ms input latency, MIT-licensed [5][6][7]. It has zero block/AI features - it is a fast classic emulator. aterm should treat Ghostty as the bar for "the hidden shell layer feels instant," not as a UI competitor.
- **AI in terminals has bifurcated into two camps, and aterm must pick a clear lane.** Camp A = *terminal emulators with AI bolted on* (Warp, Wave). Camp B = *AI coding agents that live inside any terminal* (Claude Code, Codex CLI, Gemini CLI, Aider, Goose, Amazon Q) [8][9]. aterm is unusual: it is Camp A in form but wants Camp B's agentic depth natively. The risk is being out-flanked by Camp B tools that run *inside* aterm. Mitigation: make aterm the best *host* for agents (MCP, transcript UI, the deterministic risk gate) while shipping a first-class native Claude agent.
- **License is a real differentiator.** Warp = AGPL-3.0 (network-copyleft, scary to some enterprises); Ghostty/Wave = permissive (MIT/Apache-2.0); Kitty/WezTerm = GPL-3.0; aterm = **GPLv3**. aterm sits with Kitty/WezTerm on licensing - more permissive in spirit than Warp's AGPL, fully open vs Warp's proprietary brain.
- **Recommendation:** Position aterm as *"Warp's controlled-UI + agentic power, with iA Writer's radical minimalism, in native Rust - and genuinely, fully open."* Build the renderer on a GPUI-class retained-mode element tree over wgpu/Metal (not a grid engine). Steal Warp's block model, OSC-133 integration, and approval-policy agent loop; deliberately differ on visual density, the single shell-first input box, telemetry/account-gating, and openness.

## Findings

### Reference architecture: controlled-UI terminals (the category aterm is in)

#### Warp (the primary competitor)
- **Language/render:** Rust client, GPU-rendered. They built a **custom retained-mode UI framework from scratch** (no Rust GUI toolkit supported Metal at the time; Druid had no GPU backend, Azul was OpenGL-only). Architecture: low-level primitives (rect, image, glyph) rendered in Metal, composed into higher-level elements (snackbar, context menu, block). They claim the GPU pipeline is `<250` lines of shader code, intended to be re-implemented per-API for portability [1].
- **Perf (Warp's own numbers):** ">144 FPS" even with heavy UI + output; "average time to redraw the screen ... was only 1.9 ms" over a measured week [1]. Marketing also cites "60fps on multi-megabyte build logs" vs iTerm2/Terminal.app/GNOME Terminal [2]. On Linux: Vulkan preferred, OpenGL 3.3+ fallback (`WARP_ENABLE_OPENGL=1`) [2]. These are vendor figures - treat as directional, not independently verified.
- **Block model:** Each command+output is a discrete block carrying command text, stdout, exit code, duration, timestamp, and a stable ID for linking. Supports copy-block-output, bookmarking, sharing via "Warp Drive" as structured artifacts, and block-to-block navigation instead of scrolling [2].
- **AI/agent (three layers):** (1) *Active AI* - contextual suggestions watching history, exit codes, branch, recent I/O. (2) *Agent Mode* - multi-step NL task execution, invoked with `#` prefix or `Ctrl+\``, with step-by-step approval. (3) *Cloud Agents ("Oz")* - autonomous agents in containerized cloud envs, triggered by webhook/cron/manual, billed per-execution (20+ credits) [2].
- **MCP:** Implements Model Context Protocol; auto-discovers existing MCP configs from `.claude.json`, `.mcp.json`, `.codex/config.toml`, sharing servers across Claude Code/Codex/Warp [2].
- **Shell integration:** Auto-detects bash/zsh/fish/PowerShell/nushell; full integration over SSH if installed remotely; `WARP_IS_LOCAL_SHELL_SESSION` env marker [2].
- **License/openness:** Client open-sourced 2026-04-28 under **AGPL-3.0**; some UI crates (`warpui`, `warpui_core`) MIT; server, AI orchestration, and cloud remain **proprietary**; "agent workflows powered by OpenAI models"; OpenAI is a founding sponsor [3][4].
- **What aterm learns:** the block-as-data-structure model; the primitives->elements render layering; OSC-133-driven block boundaries; the approval-policy agent loop; MCP config auto-discovery for interop with Claude Code/Codex.
- **Where aterm deliberately differs:** Warp's UI is dense, chrome-heavy, account-gated, and telemetry-instrumented. aterm goes radically minimal (iA Writer), local-first/no-account, one shell-first input box with a hotkey routing toggle (Warp uses a `#`/keybinding agent mode and a separate AI surface), and fully open (no proprietary brain).

#### Wave Terminal
- **Stack:** Hybrid - **Go backend (~48%)** + **TypeScript/Electron frontend (~43%)**; Electron app with a Go service [10][11]. This is the Electron tax aterm exists to avoid - good feature laboratory, wrong performance class.
- **Model:** "blocks" are heterogeneous widgets - terminal, file preview, web browser, AI chat - in a drag/drop/resizable workspace. Durable SSH sessions with auto-reconnect; `wsh` workspace command system [10][11].
- **AI:** "Wave AI" reads terminal output/scrollback, analyzes widgets, reads/writes/edits files with backups; BYO API key for OpenAI/Claude/Azure/Perplexity/Gemini + local via Ollama/LM Studio [11].
- **License:** **Apache-2.0** [10].
- **What aterm learns:** heterogeneous block content (a block need not be just text - could be a diff, a file preview, an agent step) and durable-SSH UX. **Differ:** no Electron; aterm's blocks stay terminal/agent-focused, not a kitchen-sink widget canvas, to preserve minimalism.

### Reference architecture: classic GPU terminal emulators (the "hidden shell" bar)

These render a **character grid**, not a UI document. aterm wraps a hidden shell, so their *VT-parsing/PTY/grid* layer is the reference for the part of aterm users never see; their *UI* is not.

#### Ghostty
- **Stack:** Zig (~79%) + Swift (~11%, macOS native UI) + C/C++. **Native Metal on macOS, OpenGL on Linux.** Multi-threaded: dedicated read, write, and render threads per terminal surface. `libghostty` is an embeddable C/Zig lib; `libghostty-vt` is the VT parser/state with CPU-specific SIMD optimizations [5][6][7].
- **Perf:** community benchmarks put input latency ~2-5ms (Typometer-class); holds 60fps scrolling large logs where iTerm2 drops into the high 20s; ~3x throughput and notable memory advantage vs iTerm2 in 2026 community tests [6][12]. Note: a widely-cited macOS "29.2ms" kitty figure is dominated by display refresh/VSync and methodology, not the emulator - treat cross-OS latency numbers cautiously [12].
- **AI/blocks:** none. Pure, fast, correct emulator. License: **MIT** [5].
- **What aterm learns:** the threading model (separate PTY-read / render threads), SIMD VT parsing, native-Metal-not-abstraction-layer for the hot path, and that "feels instant" = low input latency, not just high fps. **Differ:** aterm needs a block/document UI on top, which Ghostty explicitly does not do.

#### Kitty
- **Stack:** **C (hot path) + Python (config, "kittens" plugins)**, GPU via **OpenGL**. Created by Kovid Goyal. Linux + macOS. Originated the kitty graphics protocol and a remote-control IPC. License: **GPL-3.0** [13][14].
- **Relevance:** reference for the kitty graphics protocol (inline images) and ligature handling; same license family as aterm. Not a block/AI competitor.

#### WezTerm
- **Stack:** **Rust**, GPU via **`wgpu` (WebGPU)** -> Metal/Vulkan/DX/GL; cross-platform; built-in multiplexer; Lua config. License: **MIT** [15][16].
- **Relevance:** the closest existing proof that *Rust + wgpu* is a viable terminal render stack, and a strong reference for the wgpu path if aterm chooses wgpu over raw Metal. Multiplexer design is worth studying for session persistence. No blocks/AI.

#### iTerm2
- **Stack:** Objective-C/Swift, macOS-only, has had a GPU (Metal) renderer for years; mature, feature-dense. License: GPL-2.0. The incumbent aterm/Warp/Ghostty are all displacing. Recently added basic AI command-suggestion features but is not agentic [9][12].

#### Tabby / Hyper (Electron tier - cautionary)
- **Hyper:** Electron; high input latency, slow large-output rendering, high memory; development appears stalled (an Oct-2024 "is Hyper still developed?" issue went effectively unanswered). MIT [9].
- **Tabby:** Electron but better resource management than Hyper; actively developed through 2025/26; still heavier than native. MIT [9].
- **Lesson:** these are exactly the performance class aterm's native-Rust mandate exists to escape. Useful only as anti-examples.

### The AI-agent landscape (the lane that could out-flank aterm)

Two camps [8][9]:

- **Camp A - terminal emulators with AI:** Warp, Wave (covered above), plus iTerm2's light suggestions. These own the *surface*.
- **Camp B - AI coding agents that run inside any terminal:** Claude Code (Anthropic), Codex CLI (OpenAI), Gemini CLI (Google), Aider, OpenCode, Goose, Amazon Q Developer CLI. These own the *agent loop* and are terminal-agnostic.

Concrete state of Camp B (as surfaced in current comparisons - treat leaderboard numbers as third-party claims):
- **Claude Code** - agentic tool that reads the codebase, edits files, runs commands, integrates with dev tools; available in terminal/IDE/desktop/browser; strong on multi-file refactors and git workflows [8][17]. This is aterm's chosen default-provider stablemate - aterm's native agent is effectively a "Claude Code-class loop with a GPU-native transcript UI."
- **Codex CLI (OpenAI)** and **Claude Code** are described as the two leading terminal agents in 2026; a third-party "Terminal-Bench 2.1" leaderboard cites Codex CLI (GPT-5.5) #1 and Claude Code (Opus) #2 [8] - unverified by us; cite cautiously.
- **Amazon Q Developer CLI** - the former **Fig**; open-source autocomplete engine (`withfig/autocomplete`, now `aws/amazon-q-developer-cli-autocomplete`); AI autocomplete for hundreds of CLIs + agentic execution (generate code, edit files, git workflows) with permission gating; free [18][19].
- **Aider, Gemini CLI, OpenCode, Goose** - model-agnostic, mostly free/open; Aider is git-native but its repo activity has slowed [8].

**Strategic implication for aterm:** Camp B agents will run *inside* aterm regardless of what aterm ships. So aterm's agent must justify itself by being **deeply integrated with the block timeline, the deterministic risk gate, the secrets sanitizer, and the GPU-native transcript** - things an in-terminal CLI agent cannot do because it only sees a dumb grid. aterm should also be the best *host* for Camp B tools (MCP, OSC-133 so it understands their command boundaries), turning a threat into a moat. The Fig/Amazon-Q autocomplete engine is open-source and worth studying for the ghost-text completion UX.

### License comparison (one-glance)

| Product | Render/stack | License | Blocks | Native agent |
|---|---|---|---|---|
| **aterm** (target) | Rust, Metal/wgpu | **GPLv3** | yes | yes (Claude) |
| Warp | Rust, custom Metal framework | AGPL-3.0 (client); proprietary brain | yes | yes (OpenAI) |
| Wave | Go + Electron/TS | Apache-2.0 | yes (widgets) | yes (BYO key) |
| Ghostty | Zig + Metal/GL | MIT | no | no |
| Kitty | C + Python, OpenGL | GPL-3.0 | no | no |
| WezTerm | Rust, wgpu | MIT | no | no |
| iTerm2 | ObjC/Swift, Metal | GPL-2.0 | no | light AI |
| Tabby | Electron | MIT | no | no |
| Hyper | Electron | MIT | no | no |

(Stacks/licenses sourced inline above; verify each at release time - licenses change, as Warp's 2026 move proves.)

## Recommendations for aterm

1. **Build the UI on a retained-mode element tree over wgpu/Metal (GPUI-class), NOT a terminal-grid renderer.** *Rationale:* aterm is a document-of-blocks app like Warp/Zed, not a grid emulator; Warp had to invent this and Nathan Sobo's GPUI now exists as the open template. **Confidence: High.** (Final wgpu-vs-raw-Metal call belongs to the rendering-stack researcher - cross-reference.)
2. **Steal Warp's block-as-data-structure model wholesale** (command, stdout, exit code, duration, timestamp, stable ID), and drive block boundaries from OSC-133 shell integration (already a "keep" from the prototype). *Rationale:* it is the proven primitive for everything downstream (selection, sharing, agent context). **Confidence: High.**
3. **Differentiate hard on visual density and the single shell-first input box.** Warp is busy and chrome-heavy; aterm's iA-Writer minimalism + one input with a routing-toggle hotkey is the clearest felt difference. *Rationale:* "blocks + AI in Rust" is no longer novel post-Warp-OSS; the *feel* is the wedge. **Confidence: High.**
4. **Make aterm a first-class HOST for Camp B agents (Claude Code/Codex/Q), not just a competitor.** Auto-discover MCP configs (`.mcp.json`, `.claude.json`, `.codex/config.toml`) like Warp; emit/consume OSC-133 so external agents' command boundaries render as proper blocks. *Rationale:* those agents will run inside aterm anyway; hosting them well is a moat, not a concession. **Confidence: High.**
5. **Position the native agent as "Claude Code-class loop, but wired into the risk gate + sanitizer + GPU transcript."** Lead with the deterministic code-side risk gate and secrets sanitizer (prototype "keep" ideas) as differentiators no in-terminal CLI agent can match. *Rationale:* depth-of-integration is the only durable edge over terminal-agnostic agents. **Confidence: Med.**
6. **Treat Ghostty as the latency/throughput bar for the hidden shell layer.** Target ~2-5ms input latency and 60fps-floor large-log scroll; copy its read/write/render thread separation and SIMD-class VT parsing. *Rationale:* "feels instant" is input latency, not just fps. **Confidence: High.**
7. **Keep GPLv3 and lead marketing with "genuinely, fully open" vs Warp's proprietary brain + AGPL.** *Rationale:* a concrete trust/values wedge against the only direct competitor; AGPL also spooks some enterprises. **Confidence: Med.**
8. **Borrow the Fig/Amazon-Q open autocomplete engine's ghost-text UX** for inline command completion in the single input box. *Rationale:* it is open-source, battle-tested, and complements (does not conflict with) the shell-first input. **Confidence: Med.**

## Risks & unknowns

- **Warp's open-sourcing changes the game mid-flight.** With an AGPL Rust client now public, Warp's block engine and renderer are studyable - and forkable by others. aterm's "native Rust blocks" novelty is gone; the wedge must be feel + openness + agent integration. License contamination risk: aterm (GPLv3) must NOT copy code from Warp's AGPL-3.0 client (incompatible direction for redistribution) - learn from it, don't lift it.
- **Camp B agents could make aterm's native agent redundant.** If Claude Code/Codex keep improving inside any terminal, aterm's native agent must justify itself purely on integration depth. Unverified whether users prefer a native agent over their existing CLI agent habit.
- **Vendor perf numbers are unverified.** Warp's ">144fps"/"1.9ms redraw" and the "Terminal-Bench 2.1" agent rankings are self-/third-party-reported. I did not independently benchmark anything. Cross-OS latency figures (e.g. macOS "29ms") are VSync/methodology-dominated and unreliable for emulator comparison.
- **GPUI maturity/portability for aterm's needs is unconfirmed here.** I established that GPUI exists and shares Warp's lineage, but did not assess its API stability, text/ligature quality, or accessibility - that is the rendering-stack researcher's call.
- **Could not verify exact current versions** of most competitors (Ghostty had no version on its repo page; Warp/Wave version strings not pinned). Cited license/stack facts reflect mid-2026 sources and should be re-checked at implementation time.

## Open questions for the product owner

1. **Native agent vs host-for-agents emphasis:** is aterm's flagship the built-in Claude agent, or being the best terminal to run *any* agent (Claude Code/Codex) in? This shifts the whole product narrative and roadmap priority.
2. **How explicitly do we attack Warp?** "The open, minimal Warp" is a sharp wedge but invites direct comparison on a feature surface where Warp is years ahead. Comfortable being "Warp but minimal + open," or do we under-play the comparison?
3. **MCP/interop scope at v1:** do we ship MCP config auto-discovery and OSC-133 hosting of external agents from day one (interop-first), or land the native experience first?
4. **AGPL contamination policy:** confirm a hard rule that no Warp (AGPL) source is read-for-copying by humans or agents, to keep aterm's GPLv3 distribution clean.
5. **Telemetry/account stance:** Warp gates features behind accounts + telemetry. Is aterm strictly local-first/no-account as a stated value? (Strongly implied by the brief but worth confirming as marketing copy.)

## Sources

1. [How Warp Works - Warp blog](https://www.warp.dev/blog/how-warp-works) (custom Rust UI framework, primitives->elements on Metal, >144fps / 1.9ms redraw, Nathan Sobo / Flutter-inspired framework)
2. [Warp Guide 2026 - DeployHQ](https://www.deployhq.com/guides/warp) (block fields, Active AI / Agent Mode / Cloud Agents "Oz", MCP auto-discovery, shell integration, Vulkan/OpenGL fallback, 60fps claim, dual MIT/AGPL detail)
3. [Warp - The Agentic Development Environment](https://www.warp.dev/) and open-source announcement
4. [Warp is now open source - Warp blog](https://www.warp.dev/blog/warp-is-now-open-source) (2026-04-28, AGPL-3.0 client, proprietary server/AI, OpenAI-powered workflows)
5. [ghostty-org/ghostty - GitHub](https://github.com/ghostty-org/ghostty) (MIT, Zig/Swift/C, Metal on macOS / OpenGL on Linux, libghostty / libghostty-vt, multi-threaded, SIMD parser)
6. [Rendering System - Ghostty DeepWiki](https://deepwiki.com/ghostty-org/ghostty/5-rendering-system) (per-surface read/write/render threads, up to 120fps design)
7. [ghostty/src/terminal/render.zig - GitHub](https://github.com/ghostty-org/ghostty/blob/main/src/terminal/render.zig)
8. [Best Terminal AI Coding Agents in 2026 - amux](https://amux.io/blog/best-terminal-ai-coding-agents-2026/) (Claude Code, Gemini CLI, Codex, OpenCode, Aider, Warp, Goose, Q Developer; Terminal-Bench 2.1 leaderboard claim)
9. [Best Terminal Emulators Compared - DEV](https://dev.to/_d7eb1c1703182e3ce1782/best-terminal-emulators-compared-iterm2-warp-alacritty-windows-terminal-and-more-3f6) (iTerm2/Hyper/Tabby positioning, Electron latency caveats)
10. [wavetermdev/waveterm - GitHub](https://github.com/wavetermdev/waveterm) (Go backend + Electron/TS, Apache-2.0, drag/drop blocks, durable SSH, wsh)
11. [Wave Terminal](https://www.waveterm.dev/) and [Wave AI docs](https://legacydocs.waveterm.dev/features/waveAI) (BYO key OpenAI/Claude/Gemini/Ollama)
12. [Ghostty vs iTerm2 2026 - tech-insider](https://tech-insider.org/ghostty-vs-iterm2-2026/) and [Terminal Latency - beuke.org](https://beuke.org/terminal-latency/) (2-5ms latency, throughput/memory vs iTerm2, VSync caveats)
13. [kovidgoyal/kitty - GitHub](https://github.com/kovidgoyal/kitty) (C + Python, OpenGL, GPL-3.0, graphics protocol, kittens)
14. [Kitty terminal guide 2026 - Petronella](https://petronellatech.com/blog/kitty-terminal-setup-guide-2026) (architecture/license confirmation)
15. [wezterm/wezterm - GitHub](https://github.com/wezterm/wezterm) (Rust, wgpu/WebGPU, multiplexer, MIT)
16. [WezTerm - Terminal Guide](https://www.terminal.guide/tools/terminal-emulator/wezterm/) (wgpu rendering, cross-platform)
17. [Claude Code - Anthropic](https://claude.com/product/claude-code) and [docs](https://code.claude.com/docs/en/overview)
18. [aws/amazon-q-developer-cli-autocomplete - GitHub](https://github.com/aws/amazon-q-developer-cli-autocomplete) and [withfig/autocomplete](https://github.com/withfig/autocomplete) (Fig lineage, open autocomplete engine)
19. [Enhanced Amazon Q Developer CLI - AWS blog](https://aws.amazon.com/blogs/devops/introducing-the-enhanced-command-line-interface-in-amazon-q-developer/) (agentic execution with permission gating, free)
