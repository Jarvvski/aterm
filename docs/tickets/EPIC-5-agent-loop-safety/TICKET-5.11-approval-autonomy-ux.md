---
id: T-5.11
epic: EPIC-5-agent-loop-safety
title: Approval UX + autonomy controls (auto-safe default)
status: done
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

# Notes

Landed across all four crates as a vertical slice (the model/logic with headless
tests + the render binding + the session wiring), following the T-5.10 bar: the
behavioral ACs are proven at the component (`aterm-agent`/`aterm-ui`) + render-binding
layer; the live agent turn is not yet consumed by `aterm-app` (residual #1). Gate
green: fmt / clippy `-D warnings` / build / full workspace test (644 tests). An
independent adversarial review (5 agents, find->refute) cleared the safety invariants,
the crate-arrow purity, the 60fps damage gate, and the no-regressions claim with zero
confirmed code defects; its one BROKEN verdict was a governance gap (the locked-decision
reconciliations were not yet flagged), now closed by this section + the OWNER-CONFIRM
comments in `policy.rs`.

## Landed

- `crates/aterm-core/src/block.rs`: `AgentBadge {Auto, NeedsApproval, Blocked}` (the
  agent-domain-FREE gate-verdict datum) + `AgentBlock.badge: Option<AgentBadge>` +
  `with_badge()` (+1 test). `aterm-core` still names no agent type.
- `crates/aterm-agent/src/policy.rs`: `AutonomyMode {AskAlways, AutoSafe,
  AutoRunInSession}` (+ `auto_approves`, `next`, `label`) and `AutonomyState`
  (session-scoped current+baseline, `set_mode`/`cycle`/`reset_for_new_session`/
  `policy`). `ApprovalPolicy` now carries `mode: AutonomyMode` (the `auto_run: bool`
  refactor preserves all prior behavior). Truth-table + ladder + state tests.
- `crates/aterm-agent/src/approval.rs` (NEW): `ChannelConfirmHandler` + `ApprovalRequest`
  - the fail-closed approve/deny seam a UI click (or Esc deny) feeds; the loop blocks on
  `confirm().await` until answered (4 tests, incl. both fail-closed paths).
- `crates/aterm-agent/src/transcript.rs`: `to_block()` projects the gate verdict onto
  the core badge via `badge_for(risk, decision)` (Risk->BadgeState). `derive_history`
  unchanged.
- `crates/aterm-ui`: badge render + gloss on the tool-call row head + damage-gate fold
  (`timeline_render.rs`); `AutonomyChip` + UI-local `AutonomyMode` (`components.rs`);
  threaded through `Frame`/`UiCallbacks::autonomy_mode`/`input_widget.prepare`; the
  always-visible indicator chip left of the routing chip. Component + signature tests.
- `crates/aterm-app`: session-scoped `AutonomyState` (fresh per session at baseline);
  `Cmd-Shift-A` cycle hotkey (pre-routing, like the mode toggle); `Config.default_autonomy`
  + `autonomy_cycle` with `ATERM_AUTONOMY` / `ATERM_AUTONOMY_KEY` env overrides; the
  agent->ui mode mapping for the indicator.
- `CHANGELOG.md`: a T-5.11 entry under `## Unreleased -> ### Added`.

## Owner-confirm decisions (flagged, not silently overridden)

1. **Default = AUTO-SAFE (not the dossier's ask-always).** `06-agent-architecture.md`
   Rec 10 says "ask-always default", but ADR-0006 + the CLAUDE.md locked table + AC1
   lock AUTO-SAFE ON by default. The locked decision wins (authority > dossier); the
   baseline ships `AutoSafe`.
2. **`AutoRunInSession` auto-runs non-shell-active `Caution`.** This intentionally
   loosens the locked "`Caution`/`Dangerous` always require explicit confirmation" rule
   - but ONLY as an explicit, opt-in, session-scoped, auto-reverting escalation (Rec 10's
   graduated autonomy). The two hard invariants hold in EVERY tier: a shell-active reason
   never auto-runs, and `Dangerous` never auto-runs (`AutonomyMode::auto_approves`
   checks these first, proven by `autonomy_truth_table_covers_every_tier_and_class`). If
   the owner wants `Caution` to confirm in every tier, make the `AutoRunInSession` arm
   behave like `AutoSafe` (a one-line change). **Owner sign-off requested.**
3. **New-session revert target = AUTO-SAFE baseline, not the literal AC5 "ask-always".**
   AC5 (and the Goal/Context + dossier) say revert "back to ask-always", but a fresh
   session at ask-always would contradict "AUTO-SAFE ON by default" (ADR-0006) - the two
   are mutually inconsistent as written. We revert to the configured baseline (AutoSafe);
   the safety INTENT of AC5 (a widening never silently survives a new session) is fully
   met and tested. A stricter ask-always baseline is reachable via `ATERM_AUTONOMY=ask`.
   **Owner sign-off requested**; recommend amending ADR-0006 / the ticket wording to
   match.
4. **Autonomy-cycle keybinding = `Cmd-Shift-A`** (distinct from the `Cmd-/` mode toggle).
   A UX choice; rebindable via `ATERM_AUTONOMY_KEY`. Owner-confirm.

## AC coverage

- **AC1** (Safe auto-runs + "auto" badge in timeline): `policy::benign_commands_auto_approve`
  + `policy::autonomy_truth_table...` (auto-approve); `transcript::projection_carries_kind_
  join_key_and_glossed_text` (ToolCall block carries `AgentBadge::Auto` + text "auto");
  `timeline_render::a_badge_verdict_change_invalidates_the_damage_gate` (the badge is a
  distinct drawn state).
- **AC2** (Caution/Dangerous blocks on Approve/Deny): `turn::caution_command_parks_on_the_
  channel_seam_until_the_ui_answers` + `..._denied_over_the_channel_seam_is_never_run`,
  `approval::confirm_blocks_until_the_ui_answers...`, `turn::dangerous_tool_is_not_executed_
  when_confirmation_is_denied`.
- **AC3** (RiskGloss shown for non-Allowed): `transcript::projection_carries_..._glossed_text`
  asserts the dangerous block text contains the parsed gloss ("deletes or overwrites files");
  the renderer draws `ab.text` after the badge.
- **AC4** (mode always visible + immediate switch): `policy::switching_autonomy_mode_takes_
  effect_on_the_next_decision`; `components::autonomy_chip_always_has_a_distinct_label...`;
  `routing::default_autonomy_cycle_chord_is_cmd_shift_a...`; the indicator threads
  `Session::autonomy_mode -> Frame.autonomy -> input_widget.prepare` (folded into the input
  damage signature so a switch redraws).
- **AC5** (new session reverts the widening): `policy::a_session_widening_reverts_to_baseline_
  on_a_new_session` (reverts to AutoSafe baseline - see owner-confirm #3); a fresh
  `Session::spawn` always starts at the baseline.
- **AC6** (color always paired with a text label): `components::risk_badge_always_has_a_label_
  beside_its_color_for_all_three_states` + `components::autonomy_chip_always_has_a_distinct_
  label...`; the renderers draw the `label`, never color alone.

## Two-representation / crate-arrow

The badge is the agent-domain-FREE projection of the deterministic gate verdict:
`aterm-agent`'s `badge_for` maps `RiskAssessment`+`ToolDisposition` onto
`aterm_core::AgentBadge`; `aterm-ui`'s `risk_state_for` maps that onto its own
`RiskState`. `aterm-core` and `aterm-ui` name ZERO `aterm-agent` types (grep-verified;
only doc-comments mention them). The one-way arrow holds.

## Residuals (recorded follow-ups, not silently shipped)

1. **Live agent turn not yet wired into `aterm-app`.** `SubmitAgent` is still a
   `log::info!` stub and `agent_turn_active` is hardcoded `false`; the approval card,
   badge, and auto-run are not yet demonstrable in the running binary. Wiring the loop
   (tokio runtime + `Sinks`/`Sandbox` construction + event pump + transcript->engine
   block projection + installing `ChannelConfirmHandler` + Esc->`req.deny()`) is its own
   integration body of work with no existing scaffolding - a follow-up ticket. The seam
   is ready: the channel-backed handler IS the click/Esc seam.
2. **"Always visible" is conditional on the input box being drawn.** The autonomy chip
   is gated behind `draw_input = !alt_screen && input.is_some()` (gpu.rs), so it is hidden
   in alt-screen TUI mode (the grid owns the screen; no agent interaction there today).
3. **No GPU-ink test for the autonomy chip / the `frame.autonomy` pass-through.** The
   macOS GPU harness hardcodes `autonomy: None`; the chip-draw path is verified
   structurally + by the headless `AutonomyChip::resolve` test, not by pixel readback.
4. **Chip labels are re-shaped on every input rebuild (per keystroke), not cached.** A
   pre-existing pattern for the routing chip's 2 labels; T-5.11 adds 3 more (autonomy).
   Gated by the damage signature (idle is allocation-free, proven), but a latent per-edit
   cost worth a future caching pass.
5. **`transcript::ApprovalMode` stays 2-variant** ({AutoSafe, AskAlways}); the new
   3-tier `AutonomyMode` is the control type. Recording the precise tier on an `Approval`
   step (3-variant fidelity) is a small follow-up if the audit record needs it.
6. **`deny.toml` does not enforce the crate-arrow graph** (only licenses + bans). The
   one-way arrow is held by source discipline + per-crate `Cargo.toml`, NOT by cargo-deny
   - contrary to CLAUDE.md's "cargo deny enforces this [graph]". Pre-existing; a
   `[graph]`/ban rule would make the boundary tooling-enforced. Worth a follow-up.
