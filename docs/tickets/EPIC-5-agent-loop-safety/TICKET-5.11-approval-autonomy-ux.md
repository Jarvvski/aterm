---
id: T-5.11
epic: EPIC-5-agent-loop-safety
title: Approval UX + autonomy controls (auto-safe default)
status: ready-for-agent
labels: [agent, ui, safety]
depends_on: [T-5.5, T-5.10, T-4.6]
---

# Goal

Render every gated command in the single timeline as a proposal with its parsed risk reasons, Approve/Deny, and a visible autonomy-mode indicator - with AUTO-SAFE as the shipped default, session-scoped widening, and a revert-to-ask-always on new session.

# Context

- Research: [06-agent-architecture.md](../../research/06-agent-architecture.md) section (d).4 + Recommendation 10; [07-ia-design-language.md](../../research/07-ia-design-language.md) section 5 (risk-gate badge). Locked: AUTO-SAFE ON by default; Caution/Dangerous always require explicit confirmation; auto-run is session-scoped and reverts to ask-always on a new session. Owner open-question #3 (loudness) - default quiet caution chip per the dossier.

# Implementation notes

- Crate: `aterm-ui` (the UX) reading the `ToolCall`/`Approval` steps (T-5.10) and the gate decision (T-5.5), styled per the risk-gate badge spec (T-4.6).
- Render a gated command as a nested mini command block with the risk-gate badge (Allowed silent/`auto` / Needs-approval `caution` "APPROVE?" + parsed reason from `RiskGloss` / Blocked `danger` "BLOCKED"). Always color + text label.
- Approve/Deny controls for Needs-approval/Blocked; the current autonomy mode is visibly indicated (a chip/status). AUTO-SAFE auto-runs Safe+no-shell-active commands without a click but still renders them in the timeline (auditable).
- Autonomy controls: switch between ask-always / auto-safe (default) / auto-run-in-session; the session widening reverts to ask-always on a new session and never clears shell-active strings.
- Esc interrupts an agent turn (ties to T-3.3).

# Acceptance criteria

- Under AUTO-SAFE, a Safe command auto-runs and still appears in the timeline with an "auto" badge.
- A Caution/Dangerous command blocks on an explicit Approve/Deny.
- The parsed risk reason (RiskGloss text) is shown for non-Allowed verdicts.
- The autonomy mode is always visible; switching modes takes effect immediately.
- Starting a new session reverts auto-run-in-session back to ask-always.
- Color is always paired with a text label (color-blind safety).

# Out of scope

- The gate/classifier (T-5.5) and timeline model (T-5.10).
- API-key custody (T-8.3).
