---
title: How Warp Actually Works
domain: warp-internals
status: research
---

# How Warp Actually Works

## TL;DR

- Warp is **not a real terminal** in the sense that it does not draw a single VT100 grid to a window. It spawns a hidden shell over a PTY, **parses the byte stream itself**, and renders its own block-based UI. The terminal grid logic is **forked from Alacritty's grid code** but split into **one grid per command** instead of one global grid [1][3].
- Command boundaries come from **shell integration hooks** (`preexec`/`precmd` in zsh, `bash-preexec` for bash) that emit **Device Control Strings (DCS) carrying JSON** events (e.g. `{"hook":"Preexec","value":{"command":"..."}}`). This - not raw VT parsing - is what creates "blocks." OSC 133 is supported as an alternative/interop path but the native path is Warp's own DCS protocol [3][5][6].
- Rendering is a **custom Rust UI framework** (informally "warpui"), built with **Nathan Sobo** (Atom/Zed co-founder), loosely **Flutter-inspired**, **retained element tree on top of ~200 lines of Metal shaders** over three primitives: **rect, image, glyph (texture-atlas)**. It is a **sibling of, but separate from, Zed's GPUI** - shared lineage, different codebase [1][2][7].
- Public performance numbers: **>144 FPS** under heavy UI + output, **~1.9 ms average frame redraw** over a week of telemetry, target floor **60fps on 4K/8K**. They cite that a well-managed GPU pipeline (one glyph rasterization, minimal state changes, few draw calls) can hit **400+ fps** [1].
- The **input editor is a full editor, not a readline**: text stored in a **SumTree** (Zed-lineage rope variant) indexable by byte/char/line in O(log N), designed as an **operation-based CRDT** from day one; syntax highlighting via a **custom Nushell-inspired command parser** run async/debounced, drawn with custom Metal primitives (no ANSI styling) [1][4][8].
- The **agent reuses the exact same block + PTY machinery**: agent conversations are "rich content blocks" inserted into the one `BlockList`; commands the agent runs are ordinary terminal blocks "indistinguishable from what you'd type." Approval/autonomy is a **code-side permission layer** (allowlist/denylist, per-process autonomy levels), not a prompt convention [9][10][11].
- **Recommendation for aterm:** copy the *model* (PTY + self-parsed grid forked from a vte/alacritty-grade parser, hook-driven block segmentation, one timeline for human+agent blocks, code-side permission gate) but **do not** try to clone warpui. Build on a maintained Rust GPU UI stack (covered in the render-stack research). The block model and shell-integration bootstrap are the reproducible crown jewels; the bespoke framework is not worth reinventing.

## Findings

### (a) The "wrap a hidden shell" model - why it is "not a real terminal"

A conventional terminal emulator (xterm, iTerm2, Alacritty) is a thin device: it owns a PTY, the shell writes a byte stream of characters + VT100/ANSI escape sequences into it, and the emulator's only job is to interpret those bytes into a single 2D character grid and paint it. The emulator has **no concept of "a command" or "its output"** - it only sees cells changing [3].

Warp keeps the PTY plumbing (it still spawns a real shell - zsh/bash/fish - reads/writes bytes, and interprets VT100 sequences) but **inverts the role of the UI**: instead of being the passive renderer of one grid, Warp's UI is the authority and the shell is a controlled subprocess feeding it [1][3]. Concretely:

- Warp **forked Alacritty's grid code** (Rust, performance-tuned) as the VT-interpretation core rather than writing a parser from scratch [3].
- It does **not** keep one global grid. The VT100 spec assumes a single grid where later output can overwrite earlier cells; that makes "where did command A's output end and B's begin" unrecoverable. Warp instead **creates a separate grid per command/output pair**, keyed off the shell hooks (below). This is the structural reason blocks can be independently selected, copied, searched, re-run, and shared [3].
- This is why Warp markets itself as rebuilt "from the ground up" rather than a terminal *emulator*: the screen the user sees is **never a single VT grid**; it is a virtualized list of typed UI blocks, some of which happen to wrap a per-command grid [9].

VERIFIED: PTY + VT100 parsing + Alacritty grid fork + per-command grids [1][3]. INFERENCE: that the per-command grid still runs a full vte-style state machine per block (likely, but the exact crate boundary - whether they vendored `alacritty_terminal` or only the `Grid<T>`/`Storage<T>` types - is not stated publicly).

### (b) The block model and the Warpify / shell-integration bootstrap

**Block model.** The UI is built around a **`BlockList`**: "an ordered list of blocks - typed, self-contained units of content that stack vertically and scroll together" [9]. Block types include:
- **Terminal blocks** - wrap a command's input grid + output grid + storage.
- **Rich content blocks** - "arbitrary UI views, plugged into the `BlockList` at a specific position, wrapping a handle to a view the UI layer knows how to render." Agent conversations, diffs, error explanations are rich blocks [9].

**Block storage is hybrid** [9]:
- **`GridStorage`** for the mutable/active region (where the cursor is, live output).
- **`FlatStorage`** for immutable scrollback - "a packed byte buffer plus a few sparse tables" (compressed, cheap to hold thousands of blocks).
- Block heights are indexed in a **SumTree** so the virtualized renderer can answer "what blocks are in the viewport / at scroll offset Y" in **O(log n)** across thousands of blocks. Crucially, **human and agent blocks share this one structure** - "none of that requires special cases in the height/indexing machinery or the virtualized renderer" [9].

**Shell integration / how blocks get created.** Command boundaries are not inferred from VT bytes; they are signalled explicitly by the shell:
- Warp installs **`preexec`/`precmd` hooks** - native in zsh and fish; for bash it relies on the **`bash-preexec`** approach [3].
- These hooks print **Device Control Strings (DCS) wrapping JSON** to the PTY. Example payload shown publicly: `{"hook":"Preexec","value":{"command":"$warp_escaped_command"}}` [9]. Warp's parser intercepts these DCS events (they are invisible "in-band" control data, not displayed) and uses them to open/close blocks, attach the command text, exit code, cwd, timing, etc. [3][9].
- **OSC 133** (semantic prompt: prompt-start `A`, command-start `B`/`C`, command-end `D`) is supported as an interop/standard path - e.g. powerlevel10k with `POWERLEVEL9K_TERM_SHELL_INTEGRATION=true` - but it is a known fragile area: setting bash `PROMPT_COMMAND` to emit OSC-133 `A` can make Warp stop showing program output, and there are open issues about Warp getting "confused" by OSC-133 `A` data [5][6]. The robust path is Warp's own DCS+JSON protocol.

**Warpify / subshells bootstrap.** "Warpify" is how Warp re-establishes block tracking inside a context where its hooks were lost - SSH sessions, `docker exec`, `sudo -s`, nested shells, dev containers. The mechanism: a **bootstrap snippet is run in the subshell that prints a DCS to be read by Warp, signalling that a subshell session has started and is ready to be "Warpified."** Warp then reinstalls its hooks/shell integration in that subshell so blocks, autosuggestions, and completions work there too [12]. To support all this, Warp has "built custom support for a **subset of shell functionality** (decoupling functionality from the shell and moving it to the terminal)" [12].

VERIFIED: BlockList, rich vs terminal blocks, GridStorage/FlatStorage, SumTree height index, DCS+JSON hook protocol, the literal `Preexec` JSON example, OSC-133 fragility, Warpify-via-DCS [3][5][6][9][12]. UNKNOWN: the exact ZDOTDIR/rc-injection mechanism Warp uses to install hooks at startup is not spelled out in the public posts (the prior aterm prototype used a ZDOTDIR shim, which is the clean approach; see Recommendations).

### (c) The custom Rust UI framework and Metal GPU rendering

**Why they built their own.** When Warp started (~2020-2021), there was no stable Rust GUI framework with a Metal backend. Their stated survey: **Druid** had no GPU backend yet; **Azul** supported only OpenGL. So they built their own, **partnering with Nathan Sobo** (co-founder of Atom, later co-founder of Zed/Zed Industries), who had already begun a Rust UI framework **"loosely inspired by Flutter"** [1][2][7].

**Relationship to Zed's GPUI.** This is a frequent point of confusion. Warp's framework (community-named "warpui") and Zed's **GPUI** are **separate codebases that share intellectual lineage through Nathan Sobo**, not the same library [1][7]. GPUI is the framework that ships in Zed - "a hybrid immediate and retained mode, GPU accelerated UI framework for Rust" where "views build a tree of elements, lay them out and style them with a tailwind-style API" [7]. Warp's is its own internal framework with the same Flutter-ish philosophy. Treat anything you read about GPUI as *illustrative of the approach*, not as Warp's actual code.

**Rendering architecture** [1]:
- **Three GPU primitives**, implemented in **~200 lines of Metal shader code**: **rectangle**, **image**, **glyph** (glyphs via a **texture atlas**, rasterized once and reused).
- Higher-level **elements** (snackbar, context menu, block, etc.) are **composed from primitives**. The element layer is **GPU-API-agnostic**; only the ~200-line primitive layer is Metal-specific, so porting to OpenGL/WebGL/another backend means reimplementing **<250 lines of shaders** and leaving the element tree untouched [1].
- They explicitly contrast with CPU 2D rendering (caps out at "standard 2D performance") vs a disciplined GPU renderer that "minimiz[es] state changes between two frames, rasteriz[es] glyphs only once, and minimiz[es] the number of draw calls" to "push rendering on the GPU to 400+ fps" [1].

**Architecture internals (from the "Why is building a UI in Rust so hard?" post)** [2]:
- An **ECS-flavored / entity-id model** rather than OOP inheritance. A `Window` holds a `HashMap<EntityId, View>`, a `root_view`, and a `focused_view`. A `Presenter` holds `rendered_views: EntityId -> Element` and `parents: childEntityId -> parentEntityId` for tree traversal.
- Honest pain points they document: **no easy bidirectional traversal** of the painted element tree, which "makes event handling more difficult"; and use of **`RefCell` for shared mutability** during platform events that caused **"a few crashes in the wild due to concurrent `borrow_mut`."** This is a direct, real-world warning about the cost of hand-rolling a retained GUI in Rust [2].

**Performance numbers (public)** [1]:
- **>144 FPS** sustained with many UI elements + heavy terminal output.
- **~1.9 ms** average time to redraw the screen, averaged over a week of (presumably telemetry) data.
- Stated **target: 60fps minimum on 4K/8K displays**.
- Benchmarked scrolling against **`vtebench`** (single-line scroll).

VERIFIED: primitives, ~200-line Metal shaders, Flutter-inspired, Sobo collaboration, the FPS/1.9ms/400fps figures, ECS/EntityId model, RefCell crash anecdote, vtebench [1][2][7]. INFERENCE: "warpui" is a community name; Warp's own posts don't consistently name the framework. The separateness from GPUI is well-supported but the degree of code sharing is not public.

### (d) The input editor

Warp's input line is a **full text editor**, deliberately not a shell readline [1][4][8]:
- **Backing store: SumTree** - a B-tree-like structure (Zed lineage) "similar to a Rope," holding generic summarized items, **indexable in multiple dimensions (bytes, chars, lines) in O(log N)**, self-rebalancing [1][4][8].
- The same SumTree holds **display-only transformations that don't mutate the buffer** (code folding, non-selectable annotations), so Warp can query "buffer text" vs "display text" at any point cheaply [8].
- It was designed as an **operation-based CRDT from the start** (edits modeled as operations stored in a SumTree) to enable real-time collaborative editing later [1][8].
- Editor UX: **multiple cursors, multiple selections, word-wise motions, Select All**, alongside terminal-traditional bindings (up-arrow history, `ctrl-r`, tab completion) [1].
- **Syntax highlighting** [4]: Warp does **not** use ANSI escape codes for styling (incompatible with their editor). Styling is its own model of contiguous **"parts"** (runs of identical style) that splice/merge on edits, with **inheritable vs non-inheritable** styles; underlines are drawn with a **custom Metal rectangle primitive**. Parsing is done by a **custom command parser "loosely based on Nushell"** (also powering Command Inspector, Autosuggestions, Completions). All parsing runs **async with debouncing** to avoid input latency regressions, with specific triggers (space, paste, selection change) bypassing the debounce for instant feedback [4]. tree-sitter is **not** named in this post.

### (e) How AI / agent features wire into the UI (the "Agentic Development Environment")

The defining design choice: **the agent is not a separate panel; it is more blocks in the same `BlockList`** [9].
- When you ask the agent to do something, **a rich content block appears** - the agent's conversation view showing plan/reasoning [9].
- When the agent **runs a command**, that command is an **ordinary terminal block** - "indistinguishable from what you'd type yourself: command grids, output grids, storage." So a single, **wall-clock-ordered scrollback** interleaves human commands and agent actions with no special-case rendering [9].
- This validates aterm's "one unified timeline" decision: Warp proves it scales (thousands of mixed blocks, O(log n) viewport queries) precisely *because* human and agent share the same block primitive [9].

**Agent loop and control (the agentic dev environment)** [10][11]:
- The loop is the standard agentic shape: **describe intent -> agent decomposes into steps -> proposes a command -> runs it (subject to approval) -> reads output -> iterates.** Bounded so the human controls every side effect [11].
- **Approval / autonomy is code-side policy, configured per Profile** [10][11]:
  - **Autonomy levels per shell process:** *Always ask* (every write needs approval), *Ask on first write* (first write to a process needs approval, subsequent writes to that same process auto-approved), *Always allow* (no per-command prompt). A "confidence" mode lets it act when confident and ask when uncertain.
  - **Command allowlist / denylist**, where the **denylist always takes precedence** over allow settings.
  - **Destructive operations** (deletes, force pushes, infra changes) **require explicit confirmation even when auto-run is enabled.**
  - Per-profile control of **which tools / MCP servers** the agent may access.
  - Auto-approve mode runs every suggested command until task end or `Ctrl-C`.
- Each proposed command surfaces the **agent's reasoning + a short explanation + the command**, and the human can **approve / edit / reject / redirect** [11].

This maps almost 1:1 onto aterm's stated agent requirements (multi-step loop, approval policy, autonomy controls, rich transcript, sandboxing) and the prototype's "deterministic code-side risk gate." Warp's denylist-takes-precedence + per-process autonomy is a good template for aterm's gate.

### (f) Proprietary vs independently reproducible

**Reproducible from public info (high confidence):**
- The **PTY-wrap + self-parsed grid** model (fork/borrow a VT engine: `vte` / `alacritty_terminal` grid types are public, BSD/Apache/MIT).
- The **hook-driven block protocol** (preexec/precmd + DCS/OSC-133). The escape-sequence semantics and the JSON-over-DCS idea are documented; you can design your own equivalent.
- The **one-BlockList timeline** with virtualized rendering and a SumTree height index.
- The **input-as-editor** idea (rope/SumTree; `ropey` is a mature public crate; tree-sitter for highlighting).
- The **code-side agent permission model** (autonomy levels, allow/deny, destructive confirmation).

**Proprietary / hard to reproduce:**
- **warpui** itself (the bespoke Metal framework) - internal; do not try to clone it.
- Warp's **cloud services**: AI agents, Warp Drive, team collaboration, account/sync run on Warp-operated proprietary infrastructure, not documented internally and not self-hostable [searched].
- Exact crate boundaries, their Nushell-derived parser, their CRDT wire format - all internal.

**Source-availability caveat (UNVERIFIED - flag for follow-up):** one June-2026 AI-summarized search result claimed Warp open-sourced the *client* under AGPLv3 with UI crates under MIT around May 2026. **I could not corroborate this against a primary source (a real github.com/warpdotdev release, a Warp blog announcement, or a license file).** Warp has historically been **closed-source/proprietary** with only a public issue tracker at `github.com/warpdotdev/Warp`. **Do not rely on this claim**; if a Warp client really is source-available, it changes the "reproducible vs proprietary" calculus and the licensing analysis materially - verify before acting. See Risks.

## Recommendations for aterm

1. **Adopt Warp's core model wholesale: hidden PTY + self-parsed grid + hook-driven block segmentation.** Rationale: it is the single design decision that makes everything else (blocks, agent-in-timeline, per-command actions) possible, and it is fully reproducible from public info. **Confidence: High.**

2. **Use a maintained VT parser/grid rather than forking, and keep one grid per block.** Use the `vte` crate for the parser state machine and either build your own `Grid<Cell>` or borrow the structure from `alacritty_terminal` (public, Apache-2.0/MIT) - but keep per-command grids like Warp, not one global grid. Rationale: Warp forked Alacritty's grid; you get the same benefit with less maintenance by depending on maintained crates. (Confirm exact crate versions in the render-stack research; do not hardcode versions here.) **Confidence: High.**

3. **Make Warp's DCS+JSON hook protocol the primary block-boundary signal; support OSC 133 only as interop.** Rationale: Warp's own posts and bug tracker show OSC-133 is fragile (PROMPT_COMMAND breakage, "A" confusion) [5][6]; their robust path is a private in-band JSON protocol. aterm should define its own versioned JSON-over-DCS events (preexec/precmd/exit/cwd/timing) and treat OSC-133 as best-effort. **Confidence: High.**

4. **Install hooks via a ZDOTDIR shim (zsh) / equivalent injected rc (bash via bash-preexec, fish via functions), never by editing user rc files.** Rationale: this was an explicit KEEP from the prior prototype and is the clean, reversible mechanism Warp-style integration needs; Warp's exact mechanism isn't public but ZDOTDIR is the right answer. Plan a **Warpify-equivalent** bootstrap (a snippet that emits a DCS to re-arm integration) for SSH/docker/sudo subshells from day one. **Confidence: High.**

5. **Build the input as a real editor over a rope, designed for highlighting, not as readline.** Use `ropey` (mature, public) for the buffer and `tree-sitter` (with `tree-sitter-bash`) for highlighting; run parsing **async + debounced** like Warp. Defer CRDT/collaboration - Warp built it in early for collab, which aterm has not committed to; an op-log is nice-to-have, not a day-one requirement. **Confidence: Med** (rope/tree-sitter High; skipping CRDT is a product call).

6. **Do NOT build a bespoke GPU framework. Pick a maintained Rust GPU UI stack.** Rationale: Warp's own "Why building a UI in Rust is so hard" post documents real costs - bidirectional tree traversal pain, `RefCell`/`borrow_mut` crashes in production [2]. aterm's 60fps floor is achievable on a maintained `wgpu`-based stack without owning a framework. (The specific stack - GPUI-as-library vs raw wgpu + glyph atlas vs another - is the render-stack research's call; this domain just says: don't reinvent warpui.) **Confidence: High.**

7. **Model the agent as rich blocks in the same timeline as terminal blocks, with agent-run commands being ordinary terminal blocks.** Rationale: Warp proves this unifies human+agent UX and reuses all the block/scroll/search machinery; it directly satisfies aterm's "one wall-clock timeline" requirement [9]. **Confidence: High.**

8. **Implement the agent permission gate as code-side policy: per-process autonomy levels (always-ask / first-write / always-allow), an allowlist, and a denylist that always wins, plus mandatory confirmation for destructive ops.** Rationale: mirrors Warp's documented model [10][11] and the prototype's deterministic risk gate; feed the same secrets source into both the gate and the output sanitizer (prototype KEEP). **Confidence: High.**

9. **Adopt a hybrid block storage split: live grid for the active block, compact immutable storage for scrollback, with a SumTree (or order-statistics tree) for O(log n) viewport height queries.** Rationale: it is how Warp holds thousands of mixed blocks at 144fps [9]. `ropey`/`sum_tree`-style structures exist; or hand-roll a simple Fenwick/order-statistics index early and optimize later. **Confidence: Med** (the pattern is right; exact structure can start simple).

## Risks & unknowns

- **The "Warp went open-source (AGPLv3) in May 2026" claim is UNVERIFIED and likely an AI-summarization artifact.** It conflicts with Warp's long-standing proprietary posture. If true it would let us read real code (huge); if false, building on it would be a mistake. **Action: verify against `github.com/warpdotdev/Warp` releases + a primary Warp announcement before relying on it.**
- **Exact crate boundaries are not public.** Whether Warp vendored `alacritty_terminal` wholesale or only the `Grid<T>`/`Storage<T>` types, the precise version of their Metal pipeline, and their Nushell-derived parser are all internal. Treat crate recommendations here as *our* choices, not "what Warp uses."
- **warpui vs GPUI conflation.** Search results sometimes blur them. Verified position: separate codebases, shared lineage via Nathan Sobo. Don't assume GPUI source == Warp source.
- **Hook-installation mechanism is inferred.** Warp's posts describe the *protocol* (DCS+JSON, preexec/precmd) but not the *installation* path. ZDOTDIR is our recommendation, not a confirmed Warp detail.
- **Performance numbers are Warp's own, on Warp's workload.** >144fps / 1.9ms / 400fps are real published figures [1] but measured on their stack, their content, their hardware. They are existence proofs that the target is reachable, not benchmarks aterm can assume.
- **OSC-133 fragility is real and load-bearing.** If aterm leans on OSC-133 as primary, expect the same prompt/output breakage Warp's tracker shows [5][6]. The mitigation (own DCS protocol) is in the recommendations.
- **`vtebench` measures throughput/scroll, not perceived UI latency.** Useful but not sufficient for validating the 60fps *interactive* floor; aterm will need its own input-to-photon latency harness.

## Open questions for the product owner

1. **Collaboration/CRDT:** Warp baked an operation-based CRDT into the editor from day one for real-time collab. Is multi-user collaboration ever in scope for aterm? If no, we skip CRDT complexity entirely; if maybe-later, we keep an op-log seam. (Affects editor architecture now.)
2. **Shell breadth at launch:** Warp is zsh/bash/fish/PowerShell/WSL. The prototype was zsh-only with *silent* degradation (an AVOID). What is the day-one shell matrix, and is loud, explicit degradation acceptable for unsupported shells?
3. **Subshell/SSH "Warpify" scope:** Do we commit to a Warpify-equivalent (re-arming integration inside SSH/docker/sudo) at v1, or accept degraded blocks there initially? It is non-trivial.
4. **Agent autonomy defaults:** Warp ships allowlist/denylist + per-process autonomy + mandatory destructive confirmation. What are aterm's *default* policy and the exact destructive-command set the gate must always confirm? (Ties to the prototype's risk gate.)
5. **If Warp's client is genuinely source-available:** do we want to study it directly (and what does its license imply for a GPLv3 project)? This hinges on the verification action above.

## Sources

1. How Warp Works - Warp engineering blog: https://www.warp.dev/blog/how-warp-works
2. Why is building a UI in Rust so hard? - Warp blog: https://www.warp.dev/blog/why-is-building-a-ui-in-rust-so-hard
3. How Warp Works (block model / Alacritty grid fork / hooks) - https://www.warp.dev/blog/how-warp-works
4. How We Built Syntax Highlighting for the Terminal Input Editor - Warp blog: https://www.warp.dev/blog/how-we-built-syntax-highlighting-for-the-terminal-input-editor
5. Warp seems to get confused with OSC-133 A prompt data - Issue #6718, warpdotdev/Warp: https://github.com/warpdotdev/Warp/issues/6718
6. OSC 133 (shell integration / semantic prompt) discussion + Warp known issues - Warp docs known issues: https://docs.warp.dev/support-and-community/troubleshooting-and-support/known-issues/
7. GPUI (Zed's framework; Nathan Sobo lineage) - https://www.gpui.rs/ and README: https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md
8. Rope & SumTree - Zed's Blog (SumTree lineage Warp shares): https://zed.dev/blog/zed-decoded-rope-sumtree
9. The Block Model Behind Warp's Agentic Development Environment - Warp blog: https://www.warp.dev/blog/block-model-behind-warps-agentic-development-environment
10. Agent Permissions - Warp docs: https://docs.warp.dev/agents/autonomy/agent-permissions
11. Agent Mode / agentic loop - Warp: https://www.warp.dev/ai and Agents-in-Warp docs: https://docs.warp.dev/agent-platform/getting-started/agents-in-warp/
12. Warpify subshells (DCS bootstrap) - Warp docs: https://docs.warp.dev/terminal/warpify/subshells/
