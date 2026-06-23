---
title: Phase-2 Design Docs - Consistency Review
status: review
reviewer: consistency-critic
date: 2026-06-23
---

# Phase-2 Design Docs - Consistency Review

A cross-check of the Phase-2 design docs (architecture, 9 ADRs, design-system +
tokens, 52 tickets across 8 epics, CLAUDE.md, CONTEXT.md, the three `docs/agents/`
convention docs) against each other, against the 6 locked decisions, and against the
research dossier in `docs/research/`.

**Bottom line: the docs are strongly consistent on the substance.** Every locked
decision is honored exactly; all 6 locked decisions have an ADR; the crate names,
the 3-thread model, the dependency arrows, the agent/safety architecture, the
multi-provider seam, the auto-safe default, and the model id (`claude-opus-4-8`)
are uniform everywhere. The findings below are real but almost all of them are
documentation-convention/cross-reference drift, not architecture errors. The two
that will actually trip an implementing agent are the ticket-convention mismatch
(#C1) and the integration-indicator label drift (#C2).

---

## Contradictions (docs disagree with each other)

### C1. The issue-tracker convention contradicts the actual ticket layout (highest priority)
`docs/agents/issue-tracker.md` documents the backlog as
`docs/tickets/<feature-slug>/PRD.md` + `issues/NN-<slug>.md`, with a per-issue
frontmatter schema (`feature:`, `assignee:`, `created:`, `updated:`, status drawn
from the 5 triage labels). But the **actual** backlog is `EPIC-N-<slug>/TICKET-<id>-<slug>.md`
with a *different* frontmatter (`id`, `epic`, `title`, `status: ready-for-agent`,
`labels`, `depends_on`) and a fixed Goal/Context/Implementation/Acceptance/Out-of-scope
body (documented in `tickets/INDEX.md`). There is no `PRD.md` anywhere and no
`issues/` subdirectory. An agent told to "file/claim a ticket the way humans do"
gets two mutually exclusive instructions.
- **Propagated into:** `CLAUDE.md` line 86 ("`docs/tickets/<feature-slug>/` (one
  feature per dir, `PRD.md` + `issues/NN-<slug>.md`)") and `CONTEXT.md` line 46
  (`tickets/ # the backlog: <feature-slug>/PRD.md + issues/NN-<slug>.md`).
- **Fix:** pick one. The `EPIC/TICKET` layout is the one actually built and indexed,
  so rewrite `issue-tracker.md` to describe the epic/ticket structure and its real
  frontmatter, and update the two lines in CLAUDE.md / CONTEXT.md to match. Also
  reconcile the status vocabulary: tickets use `ready-for-agent | blocked | needs-info`
  (INDEX "Status meanings") while triage-labels.md defines `needs-triage | needs-info |
  ready-for-agent | ready-for-human | wontfix`. `blocked` is used in INDEX but is not a
  triage label; `needs-triage/ready-for-human/wontfix` are triage labels never used by
  a ticket. Decide whether ticket `status` and triage `status` are the same field.

### C2. Integration-indicator state labels drift between the design system and everything else
The shell-integration status triad is named **two different ways**:
- `design-system.md` §7 and `tokens.toml`-adjacent spec call the three states
  **Active / Degraded / Off** with labels `shell ✓` / `shell ~` / `shell ✗`.
- ADR-0008, `docs/agents/domain.md`, `00-overview.md`, and ticket **T-2.6** (which
  defines the actual enum `IntegrationStatus { Integrated, Heuristic, None }`) call
  them **Integrated / Heuristic / None**.

An agent implementing T-2.6 builds `IntegrationStatus::{Integrated,Heuristic,None}`,
then implementing T-4.6/T-3.6 reads the design system and looks for Active/Degraded/Off.
- **Fix:** make the design system use the canonical `Integrated / Heuristic / None`
  enum names (the `shell ✓/~/✗` glyph labels can stay as the *rendered* affordance, but
  the spec should name the states by the enum). This is the domain-vocabulary rule in
  CLAUDE.md ("use the exact terms from `domain.md`").

### C3. INDEX.md cites the wrong ADR number for the workspace layout
`tickets/INDEX.md` line 41: "Workspace layout is locked (see **ADR-0001** / dossier
'Canonical Cargo workspace layout')." Workspace layout is **ADR-0003**; ADR-0001 is
Language and target platform. Single-character fix, but it is the canonical pointer
every agent follows.
- **Fix:** change ADR-0001 -> ADR-0003 on that line.

### C4. design-system.md cites `03-pty-vt-rust.md` for the OSC-133/nonce mechanism
`design-system.md` §7 (shell-integration indicator) says "gated by a nonce
(`aterm-core`; see `03-pty-vt-rust.md` for the mechanism)". The nonce/OSC-133
mechanism is documented in `04-shell-integration.md` (and ADR-0008), not `03`.
- **Fix:** cite `04-shell-integration.md`.

---

## Gaps (a locked decision/epic/component with no home)

### G1. No ADR records the "threads, not tokio, for PTY I/O" decision
CLAUDE.md, the architecture doc, and tickets T-1.1/T-1.3 treat "blocking `portable-pty`
reader on real threads with bounded channels (not async)" as settled, and **T-1.1's
Context explicitly flags this**: "If an ADR for 'threads vs tokio for PTY I/O' is not
yet recorded, flag it - the dossier open question #3 expected this resolved as threads."
No ADR covers it; it is only embedded inside ADR-0007's "alternatives considered"
(rejecting `pty-process`). This was a real dossier open question (#3) the owner needs to
have ratified.
- **Fix (owner-confirm):** either add a short ADR-0010 "PTY I/O concurrency model -
  blocking threads + bounded channels" or explicitly fold the ratification into ADR-0007
  and remove T-1.1's flag. Note this also sits *next to* an unstated tension: the PTY
  side is threads-not-tokio, while the agent side (ADR-0005) runs on a tokio runtime.
  That is fine and intentional (two different subsystems) but no doc says so out loud;
  worth one sentence so an agent does not "unify" them.

### G2. Build-order docs disagree on whether "Epic 0" exists
`00-overview.md`, `CONTEXT.md` (line 22), and CLAUDE.md describe an **Epic 0 (scaffold +
render spike)**, with the spike as a *blocking gate*. The locked render decision (ADR-0002)
**removed the spike gate**, and `tickets/INDEX.md` correctly states "Epic 0 ... is
intentionally absent ... the spike work is folded into Epic 1 (T-1.7, T-1.8)." So the
ticket backlog is right and self-consistent, but CONTEXT.md still lists "Epic 0 scaffold"
in its recommended build order and CONTEXT/CLAUDE still describe a scaffold that "is owned
by a separate build." This is a stale echo of the pre-lock plan, not a contradiction in
the tickets, but a reader of CONTEXT.md will look for an Epic 0 that does not exist.
- **Fix:** update CONTEXT.md's build-order line to drop "Epic 0" (or rename it "scaffold
  (separate, not a ticket epic)") so it matches INDEX.md's framing.

### G3. `_gaps.md` (the dossier completeness critique) is never referenced by Phase-2 docs
Not strictly a gap in the Phase-2 set, but worth noting: the adversarial `_gaps.md`
critique exists and is cited by CLAUDE.md's "Where things live," yet no ADR or ticket
addresses items it raised. Low priority - flag only to confirm nothing in `_gaps.md`
needed a ticket. (Not re-audited here; out of this review's scope.)

### G4. No ticket explicitly owns the system prompt / injection-hardening text
ADR-0006 and T-5.8 require "structural separation of tool results" and "system-prompt
hardening (tell the model tool output is data)." T-5.8's body mentions it, but no ticket
*owns authoring* the system prompt as a deliverable/artifact, and there is no token/spec
file for it. Minor - it can live inside T-5.8 - but if the prompt is meant to be a
reviewable, versioned artifact, say so. Owner-confirm whether that is a separate concern.

---

## Owner-confirm items (carried forward, surfaced so they don't get lost)

These are not doc defects; they are open decisions the docs correctly mark as pending.
Listed so the owner sees the full set in one place.

1. **Accent blue** `#1A93E8` / `#4DA6F0` is DERIVED, not sampled from iA Writer. Flagged
   loudly in `design-system.md` (header banner + OQ6) and `tokens.toml`. Tokens are
   wired but should not be "locked" until sampled/confirmed. All quoted WCAG ratios are
   computed estimates pending a real-library re-check. (design-system OQ6)
2. **Duo/Quattro fonts not yet vendored.** Only Mono NFM is bundled. `font.prose`/`font.ui`
   fall back to `system-ui` until T-4.3 adds them; T-4.3 also flags the OFL+Nerd-Font
   double-license check on the patched Duo/Quattro set. Consistently flagged in
   design-system, tokens.toml, ADR-0009, and T-4.3.
3. **Mode-toggle hotkey default** (proposal `Cmd-/`, rebindable?) - ADR-0004 leaves it a
   product call; T-3.3 should not hardcode it without confirmation.
4. **Risk-gate loudness** (quiet Caution chip vs interrupting banner) - default quiet chip;
   design-system OQ3 / T-4.6.
5. **Network egress policy** for the Seatbelt profile (deny-all+allowlist vs allow+proxy-log)
   - default deny-all+allowlist; T-5.7 flags it as gating the `.sb` profile.
6. **First-launch theme** (follow macOS appearance vs default "paper" light) - design-system OQ1.
7. **Honor DECSCUSR / OSC palette overrides verbatim vs enforce the aterm theme** - design-system OQ4 / caret §5.
8. **Routing-target caret tint** (recolor on toggle vs always-blue + chip only) - design-system OQ5;
   note T-3.6's default ("caret tint + glyph") leans toward *tinting* while design-system §5's
   default leans toward *always-blue, chip-only*. Mild internal tension in the default; pick one.
9. **Bundle identifier** `ai.ameba.aterm` and **min macOS 11.0** - proposals in T-8.1, marked confirm.
10. **T-8.4 signing/notarization** is correctly `needs-info` (the only non-`ready-for-agent`
    ticket) - blocked on the "when distribution matters" milestone decision.

---

## What is consistent (verified, no action)

- **All 6 locked decisions are honored verbatim** across architecture.md, the ADRs, the
  tickets, CLAUDE.md, and CONTEXT.md. No doc relitigates a locked decision.
- **Locked-decision -> ADR coverage is complete:** language/platform (0001), render stack
  (0002), workspace (0003), unified input (0004), agent loop + multi-provider (0005),
  safety gate + auto-safe + mandatory sandbox (0006), terminal engine (0007), shell
  integration (0008), text/glyph (0009). Fonts and GPLv3 are covered inside 0009 / the
  dossier rather than standalone ADRs - acceptable.
- **Crate names and dependency arrows are identical** in every doc: `aterm-core`,
  `aterm-tokens` (leaves), `aterm-agent`->core, `aterm-ui`->{core,tokens},
  `aterm-app`->{ui,agent}, `aterm-bench`->{core,ui}. "Six-crate" wording is uniform.
- **The 3-thread model** (PTY reader / model+VT+block / render) and bounded-channel
  backpressure are described identically in architecture.md §2, CLAUDE.md, T-1.3, and the
  dossier. No "two-thread" or "actor" variants leaked in.
- **Model id `claude-opus-4-8`**, adaptive thinking + `effort` (NOT `budget_tokens`),
  `anthropic-version: 2023-06-01`, Responses API for OpenAI, reject-the-Agent-SDK, and
  Managed-Agents-out-of-scope are uniform across ADR-0005, T-5.2, T-8.3, and CLAUDE.md.
  T-5.2 and T-6.1 both correctly instruct loading the `claude-api` skill before coding.
- **wgpu version is uniform** (`wgpu 29.x` in ADR-0002, `wgpu = "29"` pinned in T-1.5;
  `winit 0.30` everywhere).
- **Token coverage for design-system components is complete:** every component in
  design-system.md §7 (command block, prompt, agent card, status chip, risk-gate badge,
  integration indicator, caret) resolves to tokens that exist in `tokens.toml`
  (`bg.*`, `fg.*`, `accent.*`, `hairline*`, `success/caution/danger`, `space.*`, `radius_*`,
  `motion.*`, `caret.*`, the two `ansi.*` palettes). design-system.md ↔ tokens.toml values
  match (fonts, type scale, both color themes, both ANSI palettes, spacing, motion, caret).
- **Every epic in the build order has tickets** (E1:9, E2:7, E3:7, E4:6, E5:11, E6:3, E7:4,
  E8:5 = 52). Dependency `depends_on` edges respect the crate dependency direction (no
  ticket introduces a core->agent or ui->agent edge).
- The `needs-human` string in issue-tracker.md is a *correct* disclaimer ("`needs-human`
  is not valid; use `ready-for-human`"), not a stray invalid status - no action.
