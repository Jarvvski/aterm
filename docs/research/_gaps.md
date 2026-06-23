---
title: Completeness Critique - Gaps, Contradictions, and Follow-up Tickets
domain: meta-critique
status: research
---

# aterm Research Dossier - Completeness Critique

A completeness-critic pass over docs 01-12. The dossier is strong on the "crown-jewel" domains (Warp internals, PTY/VT engine, shell integration, agent loop, 60fps proof, licensing). The gaps below are what a future engineer or AI agent would hit and find unanswered, contradictory, or unscoped. Ordered by severity: P0 = blocks the next decision / contradicts another doc; P1 = a load-bearing domain with no owning doc; P2 = a real gap that can be deferred but must be tracked; P3 = polish / verification.

Severity legend: **P0** decision-blocking or contradictory; **P1** missing load-bearing domain; **P2** deferrable but must be tracked; **P3** verification / polish.

---

## P0 - Contradictions and decision-blocking gaps

### G1. The render-stack decision is internally CONTRADICTORY across four docs. (P0, highest priority)

This is the single most damaging gap because the render stack is explicitly the gating decision (doc 02's own title) and four docs give materially different answers:

- **02 (render-stack-eval)**: "**Build on GPUI now**" as the primary recommendation (weighted score 62 vs 57), behind a thin UI seam. Treats GPUI's licensing as "Apache-2.0 ... GPLv3-compatible cleanly."
- **12 (licensing)**: "**Choose wgpu + cosmic-text/swash (or parley) over GPUI**" because GPUI's default release build *statically links GPL-3.0-or-later* via `gpui -> sum_tree -> ztracing -> zlog` (zed issue #55470), which "**removes GPUI's headline advantage**" and makes relicensing impossible. Calls this "the single biggest input to the render-stack decision."
- **11 (competitive)**: Recommends "a **GPUI-class** retained-mode element tree over wgpu/Metal" - i.e. GPUI *or* a custom equivalent, leaving it open.
- **03 (pty-vt), 08 (text-glyph), 09 (performance)**: All architect around a **custom wgpu + alacritty_terminal + cosmic-text/glyphon** renderer with a `CADisplayLink`-driven loop, a renderer trait, and a `metal`-crate fallback - i.e. they implicitly assume the *non-GPUI* path. Doc 09's entire frame-pacing/present-path/keep-warm design is written for a renderer aterm *owns*, not for GPUI (which owns its own present path and already solved those problems internally per doc 02).

**Why this is P0**: docs 03/08/09 have already designed a custom-renderer architecture (three threads, CADisplayLink bridge, snapshot/damage, instance buffers) that is **largely redundant or wrong if GPUI is chosen** (GPUI gives you Zed's frame pacing "for free" per doc 02). Conversely doc 02's "time-to-first-pixel" argument for GPUI collapses if the team must hand-build the grid fast-path and Nerd-Font constraint table anyway (doc 08). The dossier never reconciles these. Doc 02 also did not weight the GPL-static-link contamination that doc 12 found - it scored GPUI's license a clean 5/5.

**Recommended resolution**: A decision memo + ADR that picks ONE path and propagates it. The evidence in the dossier actually points to **custom wgpu** (licensing cleanliness per 12, control over the grid fast-path per 08, frame-pacing ownership already designed in 09, and "not precluding Linux/Windows" favoring wgpu uniformity) - but doc 02's time-to-first-pixel and CJK-IME-already-solved arguments for GPUI are real and must be explicitly overridden, not ignored. Whatever is chosen, docs 03/08/09's architecture sections must be annotated as "assumes custom-wgpu path" or rewritten.

**Ticket**: `0002-render-stack` ADR that (a) re-scores GPUI with the doc-12 GPL contamination as a license cost, (b) reconciles with the custom-renderer architecture in 03/08/09, (c) decides, and (d) flags every downstream doc section that the decision invalidates.

### G2. The "Warp open-sourced under AGPL (Apr 2026)" claim is treated as BOTH unverified AND settled fact, across docs. (P0)

- **01 (warp-internals)**: flags the open-source claim as "**UNVERIFIED ... likely an AI-summarization artifact**," conflicts with Warp's proprietary posture, "**Do not rely on this claim**," with a hard action item to verify against primary sources.
- **11 (competitive)** and **12 (licensing)**: both treat it as **established fact** ("On 2026-04-28 Warp open-sourced its client under AGPL-3.0"), and doc 12 builds an entire **AGPL-contamination threat model** on it (do-not-read-Warp-source rule, clean-room obligation).

These cannot both be right. Either Warp is AGPL (then doc 01's "reproducible vs proprietary" section and its skepticism are stale, and the AGPL contamination rule in 12 is load-bearing) or it isn't (then doc 12's central threat model and doc 11's positioning are built on sand). This directly affects the legal go/no-go and whether engineers may even *look at* Warp's repo.

**Ticket**: One verification task - fetch `github.com/warpdotdev/Warp` LICENSE + the claimed blog post, confirm the license breakdown, then make docs 01/11/12 agree. If true, doc 01's source-availability caveat is removed and the AGPL rule is promoted to a CLAUDE.md invariant.

### G3. Threads-vs-tokio runtime decision is left open in multiple docs and they assume DIFFERENT answers. (P0)

- **03 (pty-vt)**: recommends **threads** (blocking PTY reads + bounded channels), explicitly leaves "Threads vs tokio for I/O?" as an open question, and notes it "must align with the renderer/runtime decision."
- **06 (agent-architecture)**: assumes **tokio** throughout (`reqwest` + `tokio` + SSE, "agent's async work runs on a tokio runtime OFF the render thread").
- **09 (performance)**: assumes a **thread-based** render/IO/main split driven by CADisplayLink.

So the app simultaneously needs a tokio reactor (agent HTTP/SSE) and a thread-based PTY/render core. Nobody has specified how these coexist: one tokio runtime on a dedicated thread feeding the UI by channel? `tokio` only in `aterm-agent` and threads in `aterm-core`? This is an architecture-defining choice that three docs each half-answer.

**Ticket**: An ADR `0005-concurrency-model` defining the runtime topology: which crate owns a tokio runtime, how PTY threads / render thread / tokio reactor communicate, and the channel/snapshot contract between them.

### G4. Multi-window / tabs / splits / session model is entirely ABSENT. (P0 for architecture, even if v1 ships single-window)

No doc addresses whether aterm is single-window or supports tabs, splits, or multiple windows. This is not a feature nicety - it is structural:
- The `BlockList`/timeline (01, 03), the `InputModel` (05), the agent `AgentTurn` timeline (06), and the PTY/shell session (03, 04) are all implicitly **singletons** in the current designs. If even *one* additional tab or split is ever wanted, every one of these becomes per-surface, and the three-thread model (09) becomes per-surface threads (as Ghostty does - doc 11 even notes Ghostty's "per terminal surface" threading but the dossier never adopts the implication).
- "Session persistence" / restore-on-relaunch is never mentioned (WezTerm's multiplexer is cited in 11 as "worth studying" then dropped).

Deciding this late forces a painful refactor from singletons to a surface registry.

**Ticket**: A scoping decision + ADR: v1 window/tab/split matrix, and a `Surface` abstraction (owning one PTY + grid + blocklist + input) even if v1 ships exactly one, so the singleton assumption is never baked in.

---

## P1 - Missing load-bearing domains (no owning doc)

### G5. Accessibility has NO owning doc, despite being doc 02's decision-flipping criterion. (P1)

Doc 02 says accessibility is "**the load-bearing weakness**" and "the single decision-changing fact" for GPUI-vs-custom - yet there is no accessibility research doc. The dossier never answers: Is screen-reader support a launch requirement? What is the a11y story for a self-drawn, non-NSTextView block timeline + agentic transcript? AccessKit is mentioned (02) as the cross-platform answer and parley is noted to integrate it, but nobody has scoped what an accessible terminal-of-blocks even looks like (VoiceOver reading command output, navigating blocks, announcing agent steps, the routing-target indicator). This gap *also* blocks G1, since a11y is the stated tiebreaker.

**Ticket**: An accessibility research doc: VoiceOver/AccessKit integration plan for a self-drawn block UI, what's launch-blocking vs deferred, and how it interacts with the render-stack choice.

### G6. Copy / paste / clipboard has NO owning doc. (P1)

Clipboard is fundamental to a terminal and is mentioned only in passing: doc 03 notes `alacritty_terminal`'s `Event::ClipboardStore`/`ClipboardLoad` (OSC 52) and doc 05 mentions paste-is-inert in the input model. Nobody owns: block-aware copy (copy command, copy output, copy block - a Warp signature feature per doc 11), bracketed paste mode, the multiline-paste-auto-execute hazard (doc 05 references the Warp #7419 class bug but only for the input box), OSC 52 read/write security policy (clipboard read is an exfiltration vector for the agent/untrusted output - ties to doc 06's threat model), and rich vs plain clipboard for agent prose. This is both a feature gap and a security gap.

**Ticket**: A clipboard/copy-paste doc covering block-granular copy, bracketed/safe paste, OSC 52 policy (especially read-clipboard denial for untrusted contexts), and selection model across grid + proportional prose.

### G7. Search-in-scrollback / find has NO owning doc. (P1)

A core terminal feature with zero coverage. The block model (01, 03) and SumTree height index are designed, but searching *within* thousands of blocks (and within the FlatStorage compressed scrollback) is never addressed - neither the UX (find bar, match highlighting in the iA aesthetic per doc 07) nor the engine (searching immutable row snapshots, regex, search across both human blocks and agent transcript). Doc 01 lists "searched" as a benefit of blocks but no doc designs it.

**Ticket**: A search doc: scrollback/block search engine (over FlatStorage + live grid), find-in-transcript, UX in the iA design system, and performance under the 60fps floor while searching a 1M-line ring.

### G8. Configuration format / settings system has NO owning doc. (P1)

Settings are referenced piecemeal - doc 10 mentions "config (TOML)" in `aterm-agent` and an `aterm-tokens` crate, doc 06 needs autonomy policy + API key custody + network egress config, doc 04 needs shell-matrix + integration toggles, doc 07 needs theme selection, doc 02/08 need font config - but no doc designs the config system: file location, format (TOML assumed but never decided), schema, layering (defaults -> user -> project-local like `.warp`/`.editorconfig`?), hot-reload, validation, and how the agent's risk-gate policy and allow/deny lists are expressed in config. Config is also a security surface (the API key in config per doc 06's `sensitivePaths`).

**Ticket**: A config-system doc: format, location, schema/versioning, layering, hot-reload, and the security treatment of secrets in config.

### G9. Telemetry / crash reporting / error handling strategy has NO owning doc. (P1)

Doc 11 positions aterm as "no telemetry" vs Warp's "telemetry-heavy," which is a *product value* - but that makes the engineering question sharper, not moot: with no telemetry, how do we get crash reports and the Tier-2 frame-time/latency data (doc 09) that "60fps always" depends on? Doc 09's benchmark harness produces local JSON; there is no story for opt-in crash reporting, panic handling in a 60fps render thread (a panic on the render thread vs the model thread vs a PTY thread have very different recovery semantics), or how the deterministic risk gate's decisions are audited/logged locally. "Local-first, no account" (11) and "prove 60fps in the field" (09) are in unexamined tension.

**Ticket**: An observability doc reconciling the no-telemetry value with: opt-in local crash/perf capture, per-thread panic recovery policy, structured local logging (the `zlog`/`tracing` choice also surfaced in the GPUI license issue), and the risk-gate audit log.

### G10. Update / distribution mechanism is mentioned but not designed. (P1)

Doc 10 names `cargo-packager-updater` "for self-update later" and defers signing/notarization, but there is no update-mechanism design: update channel/feed, signature verification of updates (ties to notarization in 10), GPLv3 source-offer obligations on each update (doc 12 requires "corresponding source"), and how an unsigned v1 (.app, Gatekeeper right-click-open per 10) reconciles with a self-updater. For a security-sensitive app that runs agent commands, the update channel is itself an attack surface.

**Ticket**: An update-mechanism doc: feed/channel, update signing/verification, GPLv3 source-offer per release, and the v1-unsigned -> signed transition.

### G11. OSC 8 hyperlinks and inline images (kitty/iTerm2 graphics) - scope undecided. (P1/P2)

Doc 11 notes kitty's graphics protocol and ligatures as references; doc 04 covers OSC 7/133/633/1337 thoroughly but **never OSC 8 (hyperlinks)** - a now-standard sequence (iTerm2, kitty, WezTerm, VS Code, Ghostty all support it) that interacts with the block model (clickable links in output), the agent (links the agent emits), and the iA design (link color = the one accent, per doc 07). Inline image protocols (kitty graphics, iTerm2 `OSC 1337 File=`, Sixel) are also unaddressed - relevant since the block model could render images as rich blocks (doc 01's "rich content blocks") and the text-glyph doc (08) already designs a BGRA color atlas.

**Ticket**: A doc (or section) deciding OSC 8 hyperlink support (likely yes, v1) and inline-image protocol scope (likely deferred), with the block-model + design-system implications.

### G12. Mouse interaction / selection model is under-specified. (P1)

Selection is touched in fragments (doc 02 "selection proven by Zed," doc 05 selection in InputModel, doc 03 `SelectionRange` in alacritty) but no doc owns the *interaction model*: text selection across block boundaries vs within a block vs across the human/agent timeline, mouse reporting passthrough to TUIs (alt-screen apps want raw mouse events - doc 03 says "route mouse straight to PTY" in alt-screen but not the block-mode selection model), click-to-position-cursor, scroll behavior (block-aware vs line), and how selection coexists with the GPU instanced renderer (08). This is closely tied to G6 (clipboard).

**Ticket**: A selection/mouse-interaction doc: cross-block selection, mouse reporting modes, scroll semantics, and rendering of selection in the GPU pipeline.

---

## P2 - Real gaps, deferrable but must be tracked

### G13. Cross-doc font inconsistency: which font crate/stack actually loads iMWriting, and are Duo/Quattro shipped? (P2)

- Doc 08 says the prior prototype bundled **only Mono**; Duo/Quattro "**were NOT vendored**" and must be added or fall back.
- Doc 07's entire typography mapping (Duo for prose, Quattro for chrome) and doc 10's `resources/fonts/` tree **assume Duo/Quattro exist**.
- Doc 02 wonders whether bundled Nerd-Font PUA glyphs render in GPUI's atlas (unverified); doc 08 designs a Ghostty-style Nerd-Font constraint table and a sprite face. Whether the bundled TTFs even contain the full Powerline/symbol set is flagged unverified in 08.

Net: the proportional-prose requirement (07) has no shipped font (08), and the constraint-table/sprite-face effort (08) is "easy to under-scope."

**Ticket**: Verify/obtain the iMWriting Duo + Quattro Nerd patches (or decide a system proportional fallback); inspect the actual TTFs for glyph coverage; scope the Nerd-Font constraint-table generation.

### G14. The agent's filesystem tools (06) and the shell session (03/04) operate on DIFFERENT cwd/state with no reconciliation. (P2)

Doc 06's `read_file`/`edit_file`/`run_command` take explicit paths/cwd and run via a no-shell subprocess runner or "injected into the live PTY." Doc 04 tracks cwd via OSC 7 from the *interactive shell*. Nobody specifies: when the agent runs `run_command`, does it inherit the live shell's cwd (the one OSC 7 last reported)? Does an agent `cd`-equivalent affect the human's shell? Is the agent's subprocess environment the same login-shell environment (03's login-shell question) or a clean env? This is both a correctness and a security question (the secrets/env-leak threat in 06 depends on what env the agent subprocess inherits).

**Ticket**: Specify the agent-execution context: cwd source, environment inheritance, and whether agent commands share or fork the human shell's state. Reconcile docs 03/04/06.

### G15. "Universal Input" interactive-program routing (the hard part of doc 05) is an acknowledged unknown with no plan. (P2)

Doc 05 admits "**Warp's interactive-program input routing is undocumented**" and the story rests on OSC-133 `inFlight`/`altScreen` raw-passthrough detection whose robustness "is a real unknown." This is the crux of the unified-input promise: when a foreground program (vim, ssh password prompt, `less`, a REPL) is reading stdin, the single input box must route keystrokes raw to the PTY, not to its own editor or the agent. The dossier flags it but proposes no concrete detection mechanism beyond "alt-screen flag + in-flight." Password prompts specifically (non-alt-screen, echo-off) are a known-hard case never addressed.

**Ticket**: A focused spike/doc on foreground-stdin detection: how to know a non-alt-screen program is reading input (termios echo-off / ICANON probing via the master fd? foreground-pgid + tty state?), and the password-prompt UX (the input box must not show ghost text / not route to agent).

### G16. macOS-specific keyboard/shortcut conflicts and the global hotkey are unspecified. (P2)

Doc 05 explicitly leaves the mode-toggle hotkey as an open product question (`⌘/` proposed, `⌃Space`/`⌘.` rejected) and never resolves it. Beyond that: no doc covers the full keybinding system (rebindable? a keymap config - ties to G8), conflicts between aterm shortcuts and shell/TUI key needs, the Kitty keyboard protocol encoder (05 mentions porting it but it's not owned anywhere as a deliverable), or whether aterm registers any *global* (system-wide) hotkey (a Warp/iTerm2 "quake mode" dropdown is a common terminal feature - never mentioned).

**Ticket**: A keybinding doc: default keymap, rebinding/config, Kitty-protocol key encoding ownership, shell/TUI conflict policy, and whether a global show/hide hotkey is in scope.

### G17. Testing strategy beyond unit tests + the perf harness is thin. (P2)

Doc 09 has an excellent two-tier *performance* harness and doc 10 wires `cargo test` + clippy + fmt CI. But there is no strategy for: VT-emulation correctness testing (esptest/vttest-style conformance - critical since aterm wraps real programs and doc 03 flags alacritty reflow bugs), shell-integration integration tests across the zsh/bash 3.2/bash 5.3/fish matrix (doc 04's "real test matrix - unverified here" for `exec`/`su`/`sudo`), agent-loop testing (recorded API fixtures? the risk gate is unit-tested per 06 but the end-to-end loop isn't), IME/CJK testing (doc 05's known traps), and snapshot/visual-regression testing for the renderer + design system. Doc 02's recommended GPUI spike and doc 09's render-path spike are also un-ticketed prerequisites.

**Ticket**: A test-strategy doc covering VT conformance, shell-integration matrix tests, agent-loop fixture testing, IME testing, and visual-regression - layered on top of the existing perf harness.

### G18. Performance budget for the AGENT + UI is not in the 60fps model. (P2)

Doc 09's frame-budget model is built for terminal grid work (VT parse, grid mutation, instance build). Doc 06's `agent_stream_while_typing` scenario appears in doc 09's stress table, but the budget breakdown (input <=2ms, frame build <=2ms, etc.) never accounts for: streaming-markdown layout of agent prose (08 says proportional layout is "low-frequency" but a fast token stream during typing is exactly the doc-09 stress scenario), the SumTree/virtualized-list re-layout when many agent blocks insert, or syntax highlighting of the input box (05 says async but it competes for the same cores). The two perf-relevant docs (06, 08, 09) assert prose layout is cheap but never budget it against the floor.

**Ticket**: Extend the perf budget to cover agent-prose streaming layout, virtualized-list re-layout on block insert, and input highlighting, with explicit sub-budgets and a benchmark scenario.

### G19. Onboarding / first-run / shell-integration-install UX is unspecified. (P2)

Doc 04 designs the no-dotfile injection mechanism beautifully but never the *first-run experience*: what does the user see on first launch, how is the API key first entered (06 says Keychain/BYOK but not the onboarding flow), how is the three-state integration indicator (04) introduced, and how does the "why integration is degraded" affordance surface to a new user. For a terminal that replaces muscle memory (own input editor, mode toggle), first-run teaching is product-critical and on-brand-sensitive (doc 07's iA restraint vs needing to teach the hotkey).

**Ticket**: A first-run/onboarding doc: API-key entry flow, shell-integration status introduction, mode-toggle discoverability (doc 05 mentions placeholder text + a fade-after-N-uses status word), within the iA aesthetic.

---

## P3 - Verification and polish

### G20. Many load-bearing version/API claims are self-flagged unverified; collect them into one verification checklist. (P3)

Scattered across docs as "Risks": `alacritty_terminal` 0.26 exact `Handler`/`Event` signatures (03); `portable-pty` raw-fd/pgid signal API (03); GPUI CJK IME on `gpui 0.2.2` + `gpui-component` version (02); winit Pinyin `set_marked_text` crash status (05); bash 5.3 `${ ...;}` syntax against official release notes (04); `rmcp` crate maturity (06); `sandbox-exec` removal timeline (06); cargo-packager exact TOML key spellings + whether `Info.plist` is needed for the titlebar (10); the doc-10 `vte` 0.13 vs doc-03 `vte` 0.15 version mismatch (the two engine docs cite different vte versions). A single "spike before coding" checklist would prevent these from being rediscovered per-crate.

**Ticket**: A consolidated pre-implementation verification checklist (one file) aggregating every "verify before coding / re-check at pin time" item from all 12 docs' Risks sections, including the `vte` version discrepancy between docs 03 and 09/10.

### G21. The iA accent blue and all WCAG contrast ratios are self-admittedly unverified estimates. (P3)

Doc 07 states the accent `#1A93E8`/`#4DA6F0` is "**NOT source-verified**," all contrast ratios are "computed estimates, not from a source," the Duo/Quattro metrics are qualitative, and the ANSI palettes are "taste, not spec." These are fine as a starting point but are flagged for re-validation; light-theme + bright-ANSI legibility (07's "riskiest combination") needs real-output eyeballing.

**Ticket**: Lock design tokens: sample/confirm the accent, run a real WCAG lib over final hexes, measure Duo/Quattro advances from the actual TTFs, and eyeball both ANSI palettes against real tool output on both themes.

### G22. Provider abstraction (Anthropic-only vs pluggable) recurs as an open question with no decision. (P3)

Docs 06 and 12 both raise multi-provider as an open product question (the prototype had `AnthropicProvider` + `OpenAiResponsesProvider`). It affects the agent client design now (a lowest-common-denominator abstraction forecloses Anthropic-specific features like adaptive thinking and the MCP connector that doc 06 leans on). Minor, but it should be decided before the agent client is written, not after.

**Ticket**: Decide Anthropic-only-v1 vs pluggable-provider and record as an ADR; if pluggable, define the provider seam before coding the client.

---

## Summary table

| ID | Gap | Severity | Suggested artifact |
|----|-----|----------|--------------------|
| G1 | Render-stack recommendation contradicts itself across 02/11/12 vs 03/08/09 | P0 | ADR 0002-render-stack (reconcile + decide) |
| G2 | "Warp is AGPL" treated as both unverified (01) and fact (11/12) | P0 | Verification task -> reconcile 01/11/12 |
| G3 | Threads-vs-tokio runtime topology assumed differently in 03/06/09 | P0 | ADR 0005-concurrency-model |
| G4 | Multi-window/tabs/splits/session model entirely absent | P0 | Surface-abstraction scoping + ADR |
| G5 | Accessibility (doc 02's own tiebreaker) has no owning doc | P1 | Accessibility research doc |
| G6 | Copy/paste/clipboard + OSC 52 policy unowned | P1 | Clipboard doc |
| G7 | Search-in-scrollback unowned | P1 | Search doc |
| G8 | Config/settings system unowned | P1 | Config-system doc |
| G9 | Telemetry/crash/panic-recovery vs no-telemetry value unowned | P1 | Observability doc |
| G10 | Update mechanism named but not designed | P1 | Update-mechanism doc |
| G11 | OSC 8 hyperlinks + inline images scope undecided | P1/P2 | Hyperlink/image scope doc |
| G12 | Mouse/selection interaction model under-specified | P1 | Selection/mouse doc |
| G13 | Duo/Quattro fonts not shipped; constraint-table under-scoped | P2 | Font acquisition + glyph-coverage task |
| G14 | Agent tool cwd/env vs shell session unreconciled | P2 | Agent-execution-context spec |
| G15 | Interactive-program/password stdin routing unsolved | P2 | Foreground-stdin detection spike |
| G16 | Keybinding system + global hotkey unspecified | P2 | Keybinding doc |
| G17 | Testing strategy beyond unit/perf thin (VT conformance, shell matrix, IME) | P2 | Test-strategy doc |
| G18 | Agent/UI work not in the 60fps budget model | P2 | Extend perf budget + scenario |
| G19 | First-run/onboarding UX unspecified | P2 | Onboarding doc |
| G20 | Many version/API claims unverified; vte version mismatch (03 vs 09/10) | P3 | Consolidated verification checklist |
| G21 | iA accent + WCAG ratios + font metrics unverified estimates | P3 | Lock-design-tokens task |
| G22 | Provider abstraction (Anthropic-only vs pluggable) undecided | P3 | ADR provider seam |

## Cross-cutting observation

The dossier's twelve domains were researched in parallel and it shows: each doc is internally excellent but the **seams between docs are where the gaps live** - the render-stack decision (G1), the runtime model (G3), the agent-vs-shell state (G14), and the agent-vs-grid perf budget (G18) all fall in the cracks between two strong docs that each assumed the other would resolve it. Before Phase 2 (architecture/ADRs), the highest-leverage move is a **single integration/reconciliation pass** that forces the docs to agree on the four P0 items, because every Phase-2 ADR depends on them. The P1 missing-domain docs (a11y, clipboard, search, config, observability, update) are the next tier and are mostly independent of each other, so they can be researched in parallel like the original twelve.
