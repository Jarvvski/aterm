# CONTEXT.md - aterm orientation

A short map of the project. For the working contract (architecture, locked decisions, toolchain, jj workflow) read `CLAUDE.md`. For vocabulary read `docs/agents/domain.md`. For full reasoning read `docs/research/00-overview.md`.

## Product vision

aterm is a native, GPU-rendered macOS terminal in Rust with the feel of iA Writer. It runs a real login shell over a PTY, parses the byte stream itself, and segments output into one-grid-per-command **blocks** driven by shell-integration hooks (OSC-133/OSC-7). A single shell-first input box drives either the live shell or a full client-side AI agent loop, and human and agent activity interleave in one wall-clock **timeline** with no special-casing. The non-negotiable bar is a **60fps floor** (120fps on ProMotion) for typing, scrolling, and streaming output.

## Differentiator vs. Warp

Warp open-sourced its client under AGPL-3.0 (April 2026), so the wedge is no longer "blocks + AI in Rust." aterm's distinct value is:

- **iA-Writer minimalism** - a radically quiet, paper-like visual language, three-register typography, one scarce blue accent, motion capped to protect the frame floor. Warp's chrome is busy; aterm's is silent.
- **Unified shell/agent input** - ONE input box. A hotkey flips whether Enter goes to the shell or the agent, and the text you already typed is preserved across the flip (a pure `InputModel` reducer mutates only a `mode` field). No second prompt, no sigil scheme, no modal switch that eats your input.
- **Full GPLv3 openness** - a clean permissive render/PTY/text stack and no AGPL/other-GPL linkage (the reason GPUI was rejected).
- **Best host for external agents** - deep first-party agent integration plus being an excellent host for Camp B agents (Claude Code/Codex) via MCP auto-discovery and OSC-133.

## Current status: Phase 2 (scaffold)

The 12-domain research dossier is complete and its decisions are locked (see `CLAUDE.md`). We are writing the Phase-2 design docs and standing up the 6-crate Cargo workspace, mise tasks, CI, and the agent conventions. No application code has been built yet. The scaffold (`Cargo.toml`, `mise.toml`, `.gitignore`, crate skeletons, packaging config) is owned by a separate build in progress; agents should not hand-author those files.

Recommended build order (see `docs/tickets/INDEX.md`): Epic 1 terminal core + GPU grid -> Epic 2 shell integration + block model -> Epic 3 unified input -> Epic 4 design system -> Epic 5 agent loop + safety -> Epic 6 MCP -> Epic 7 perf harness -> Epic 8 packaging. (The dossier's "Epic 0" - scaffold + render spike - is intentionally absent: the scaffold is built separately and, with wgpu committed, the spike work folds into Epic 1's perf-validation tickets.)

## Docs tree

```
CLAUDE.md                  # project memory: architecture, locked decisions, toolchain, jj workflow (read this first)
CONTEXT.md                 # this file: vision, differentiator, status, docs map
docs/
  research/                # the 12-domain dossier (Phase 1)
    00-overview.md         #   exec summary + cross-cutting architecture + risk register
    01-warp-internals.md   #   Warp's actual model (blocks, single timeline)
    02-render-stack-eval.md
    03-pty-vt-rust.md      #   PTY/VT engine + the 3-thread model
    04-shell-integration.md
    05-unified-input-ux.md #   the InputModel + hotkey routing
    06-agent-architecture.md
    07-ia-design-language.md
    08-text-glyph-rendering.md
    09-performance-60fps.md
    10-packaging-scaffold.md  # workspace layout, packaging, agent conventions
    11-competitive-landscape.md
    12-licensing.md
    _gaps.md               #   adversarial completeness critique
  adr/                     # architecture decision records, NNNN-title.md, one decision per file
  tickets/                 # the backlog: INDEX.md + EPIC-N-<slug>/TICKET-<id>-<slug>.md
  agents/                  # how agents consume the above
    issue-tracker.md       #   the markdown ticket convention
    triage-labels.md       #   the five canonical triage labels
    domain.md              #   key domain vocabulary (use it exactly)
  design/                  # design source feeding the aterm-tokens crate
```
