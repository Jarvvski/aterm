---
title: aterm Implementation Ticket Backlog - Index
status: ready
---

# aterm Implementation Ticket Backlog

This is the Phase-2 implementation backlog. Each ticket is a focused unit of work sized for one agent (or one human) to complete in a single sustained session. Tickets are authored against the research dossier in [`docs/research/`](../research/) (start with [`00-overview.md`](../research/00-overview.md)) and the locked decisions recorded in [`docs/adr/`](../adr/). Where a ticket and an ADR or the dossier disagree, the agent must STOP and flag it, not silently override.

## How to read a ticket

Every ticket file is `EPIC-N-<epic-slug>/TICKET-<id>-<slug>.md` with YAML frontmatter and a fixed body:

```yaml
---
id: T-1.3                       # epic.sequence; stable, never reused
epic: EPIC-1-terminal-core
title: ...
status: ready-for-agent         # one of the five triage labels (see ../agents/triage-labels.md)
labels: [core, perf, ...]
depends_on: [T-1.1, T-1.2]      # ticket ids that must land first
---
```

Body sections, always in this order:

- **Goal** - one paragraph, the outcome.
- **Context** - links to the relevant research doc(s) and ADR(s). Read these first.
- **Implementation notes** - concrete crates, types, modules, files to touch. Decisive, not exploratory.
- **Acceptance criteria** - testable bullet list. "Done" means every box is green.
- **Out of scope** - explicit boundaries so the unit stays focused.

### Status

`status` is one of the six canonical triage labels in [`../agents/triage-labels.md`](../agents/triage-labels.md) - `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `done`, `wontfix` - describing *who acts next*. Most open tickets are `ready-for-agent`; T-8.4 is `needs-info`; the landed EPIC-1/EPIC-2 foundation (all of EPIC-1 - T-1.1-T-1.9 - plus T-2.2-T-2.7), the full EPIC-4 design-system pass (T-4.1-T-4.6: tokens, themes, the three-register fonts, the Nerd-Font + sprite faces, and the on-screen block-timeline component drawing), plus the EPIC-3 pure-core units T-3.1 (the unified-input reducer) and T-3.7 (the shared history ring + per-mode lenses), and the EPIC-5 safety unit T-5.6 (the single Secrets source + OutputSanitizer), is `done` (acceptance criteria met; any on-hardware/visual residual is consolidated into its proper future ticket - EPIC-3/T-3.6 (the input box + live prompt echo), EPIC-5/T-5.10 (agent-card data), EPIC-7, T-8.1 - never parked on a human). With T-1.8 the renderer draws via the wgpu instanced glyph-atlas pipeline (the typing-lag cure), not the interim glyphon path. **Ordering/blocking is expressed by `depends_on`, not a status** - a ticket whose upstream tickets have not landed still reads `ready-for-agent`; an agent simply works the dependency-satisfied tickets first.

## Conventions all tickets share

- **Workspace layout is locked** (see ADR-0003 / dossier "Canonical Cargo workspace layout"): `aterm-core`, `aterm-tokens` (leaves), `aterm-agent` (-> core), `aterm-ui` (-> core, tokens), `aterm-app` (-> ui, agent), `aterm-bench` (-> core, ui). No dependency cycles. A ticket that needs a new cross-crate edge that violates this direction is wrong - re-scope it.
- **The scaffold/workspace itself is built separately.** No ticket here creates the workspace, `mise.toml`, CI skeleton, or the `aterm-app` binary shell; they assume those exist.
- **The 60fps floor is an architectural property, not a later optimization.** Every `aterm-ui`/`aterm-core` ticket must respect: vsync-driven render, damage tracking, PTY/model/render thread decoupling, zero per-frame heap allocation in the hot path. Perf-validation tickets (T-1.7, T-1.8) and Epic 7 are the standing proof.
- **Locked product decisions** (do not relitigate): custom `wgpu` + `cosmic-text`/`swash` renderer behind an `aterm-ui` seam (no GPUI, no render-spike gate - the spike work is folded into Epic 1 as early perf-validation); one shell-first input box with a hotkey that flips only a `mode` field (text preserved by construction); full agentic loop calling the Anthropic Messages API directly over HTTP; **multi-provider seam in v1** (Anthropic + OpenAI behind one `LlmProvider` trait); **auto-safe autonomy default ON** with a mandatory Seatbelt sandbox; bundled iM Writing Nerd Font; GPLv3.
- **jj, not git.** Landing a change uses the Jujutsu workflow in `CLAUDE.md`. Never invoke `git`.

## Build order

The ordering front-loads the two existential risks (the perf floor and the engine) and defers everything orthogonal. Epic 0 from the dossier (scaffold + a blocking render-spike gate) is intentionally absent: the scaffold is built separately, and because the owner committed to building directly on `wgpu`, the spike work is folded into Epic 1 as non-blocking early perf-validation tickets (T-1.7, T-1.8).

| Epic | Theme | Depends on |
|---|---|---|
| [EPIC-1](EPIC-1-terminal-core/) | Terminal core + GPU grid (the foundation; holds 60fps under flood) | - |
| [EPIC-2](EPIC-2-shell-integration-blocks/) | Shell integration + block model (command/output blocks in one timeline) | EPIC-1 |
| [EPIC-3](EPIC-3-unified-input/) | Unified shell-first input box + routing | EPIC-1 (EPIC-2 for integration-state) |
| [EPIC-4](EPIC-4-design-system/) | Design system pass (tokens, fonts, component specs) | EPIC-1 |
| [EPIC-5](EPIC-5-agent-loop-safety/) | Agent loop + safety (multi-provider, gated, sandboxed) | EPIC-2, EPIC-3 |
| [EPIC-6](EPIC-6-mcp-interop/) | MCP + interop (best host for external agents) | EPIC-5 |
| [EPIC-7](EPIC-7-perf-harness/) | Tier-2 perf harness + hardening | EPIC-1..4 |
| [EPIC-8](EPIC-8-packaging/) | Packaging, signing, polish (deferred) | EPIC-1..6 |

## Ticket roster

### EPIC-1 - Terminal core + GPU grid
| id | title | status | depends_on |
|---|---|---|---|
| T-1.1 | PTY spawn/resize/signals over portable-pty | done | - |
| T-1.2 | alacritty_terminal Term wiring + VT parse loop | done | T-1.1 |
| T-1.3 | Three-thread reader/model/render split + bounded backpressure | done | T-1.1, T-1.2 |
| T-1.4 | Output coalescing + grid snapshot publication | done | T-1.3 |
| T-1.5 | wgpu device/surface + CADisplayLink present loop + keep-warm | done | T-1.3 |
| T-1.6 | Glyph atlas + monospace grid fast-path (cosmic-text/swash) | done | T-1.5 |
| T-1.7 | Tier-1 iai-callgrind micro-benches (parse/grid/frame-build) | done | T-1.4 |
| T-1.8 | Render-path perf validation (folded-in spike) + damage tracking | done | T-1.5, T-1.6 |
| T-1.9 | Event::PtyWrite reply channel + foreground pgid tracking | done | T-1.1, T-1.2 |

### EPIC-2 - Shell integration + block model
| id | title | status | depends_on |
|---|---|---|---|
| T-2.1 | OSC-133/OSC-7 pre-parser filter with nonce gating | done | T-1.2 |
| T-2.2 | Shell-integration shim extraction + ZDOTDIR/ENV/XDG injection | done | T-1.1 |
| T-2.3 | bash + fish hooks (version-branched) | done | T-2.2 |
| T-2.4 | BlockList + SumTree height index + immutable snapshots | done | T-2.1 |
| T-2.5 | Block lifecycle state machine + alt-screen suppression | done | T-2.1, T-2.4 |
| T-2.6 | Three-state integration indicator + heuristic fallback | done | T-2.3, T-2.5 |
| T-2.7 | Block/timeline rendering (virtualized) | done | T-2.4, T-1.6 |

### EPIC-3 - Unified input box
| id | title | status | depends_on |
|---|---|---|---|
| T-3.1 | Pure InputModel reducer (text + selection + mode) | done | - |
| T-3.2 | IME via winit Ime events + preedit-active gate | ready-for-agent | T-1.5 |
| T-3.3 | Routing brain (disposition gates) + hotkey toggle | ready-for-agent | T-3.1, T-2.1 |
| T-3.4 | Key encoder (Kitty protocol + DECCKM) for raw passthrough | done | T-3.3 |
| T-3.5 | Async/debounced highlight + ghost text overlay | ready-for-agent | T-3.1 |
| T-3.6 | Input box widget + iA mode indicator (prompt glyph + chip) | ready-for-agent | T-3.1, T-4.2 |
| T-3.7 | Shared history ring + per-mode query lens | done | T-3.1 |

### EPIC-4 - Design system pass
| id | title | status | depends_on |
|---|---|---|---|
| T-4.1 | aterm-tokens: semantic tokens + spacing/type scale | done | - |
| T-4.2 | aterm-tokens: two ANSI-16 palettes + theme switching | done | T-4.1 |
| T-4.3 | Bundle Duo/Quattro + three-register font wiring | done | T-1.6 |
| T-4.4 | Nerd Font per-codepoint constraint table | done | T-1.6 |
| T-4.5 | Sprite face for box-drawing/Powerline/braille | done | T-1.6 |
| T-4.6 | Component specs: block, prompt, agent card, chip, risk badge | done | T-4.1, T-2.7 |

### EPIC-5 - Agent loop + safety
| id | title | status | depends_on |
|---|---|---|---|
| T-5.1 | LlmProvider trait + provider-neutral event model | done | - |
| T-5.2 | AnthropicProvider (Messages API, SSE, adaptive thinking) | done | T-5.1 |
| T-5.3 | OpenAiProvider (Responses API) | done | T-5.1 |
| T-5.4 | Typed tool definitions (run_command/read_file/edit_file/...) | done | T-5.1 |
| T-5.5 | Deterministic risk gate (zsh-aware argv parse) | done | - |
| T-5.6 | Single Secrets source + OutputSanitizer | done | - |
| T-5.7 | Seatbelt sandbox (sandbox-exec) + setrlimit + timeout-kill | ready-for-agent | T-5.4 |
| T-5.8 | Agentic turn loop (shared, provider-neutral) | done | T-5.2, T-5.4, T-5.5 |
| T-5.9 | Command-execution sinks (no-shell runner + gated PTY inject) | ready-for-agent | T-5.5, T-5.7, T-1.1 |
| T-5.10 | Timeline transcript model (AgentTurn/AgentStep, tool_use_id join) | ready-for-agent | T-5.8, T-2.4 |
| T-5.11 | Approval UX + autonomy controls (auto-safe default) | ready-for-agent | T-5.5, T-5.10, T-4.6 |

### EPIC-6 - MCP + interop
| id | title | status | depends_on |
|---|---|---|---|
| T-6.1 | Messages-API MCP connector (remote HTTP servers) | ready-for-agent | T-5.2, T-5.5 |
| T-6.2 | Local stdio MCP client | ready-for-agent | T-5.4, T-5.5 |
| T-6.3 | MCP config auto-discovery | ready-for-agent | T-6.2 |

### EPIC-7 - Tier-2 perf harness + hardening
| id | title | status | depends_on |
|---|---|---|---|
| T-7.1 | In-process frame recorder | done | T-1.5, T-1.8 |
| T-7.2 | Seven scripted stress scenarios + driver | ready-for-agent | T-7.1 |
| T-7.3 | Input-latency measurement gate | ready-for-agent | T-7.1, T-3.2 |
| T-7.4 | Resize/reflow perf check + shell-matrix hardening | ready-for-agent | T-7.2, T-2.3 |

### EPIC-8 - Packaging, signing, polish (deferred)
| id | title | status | depends_on |
|---|---|---|---|
| T-8.1 | cargo-packager .app + .dmg + hidden titlebar | ready-for-agent | T-4.3 |
| T-8.2 | OFL font bundle + acknowledgements UI | ready-for-agent | T-8.1 |
| T-8.3 | Config load + API-key Keychain custody | ready-for-agent | T-5.6 |
| T-8.4 | Signing/notarization (when distribution matters) | needs-info | T-8.1 |
| T-8.5 | Focus-Mode analog + completions menu | ready-for-agent | T-2.7, T-3.5 |
