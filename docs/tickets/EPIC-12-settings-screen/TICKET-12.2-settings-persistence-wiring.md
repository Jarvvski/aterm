---
id: T-12.2
epic: EPIC-12-settings-screen
title: Settings persistence + live application
status: ready-for-agent
labels: [ui, settings, config]
depends_on: [T-12.1, T-8.3]
---

# Goal

Bind every Settings control to live application state and persist it through the
config layer, so a change on the Settings screen (T-12.1) takes effect immediately
and survives a restart: theme switches live, font size re-flows every block live,
and the default provider + autonomy selections write through to the agent-loop
config.

# Context

- Visual source of record: [`docs/design/vision-mock/AtermWindow.dc.html`](../../design/vision-mock/AtermWindow.dc.html), `screen="settings"` - each control mutates the same state that drives the rest of the app (theme, `--fs` font size, provider, autonomy).
- [ADR-0011](../../adr/0011-vision-mock-north-star.md) - vision mock as UI north star.
- Persistence rides on [T-8.3](../EPIC-8-packaging/TICKET-8.3-config-keychain.md) (config load + API-key Keychain custody): Settings is the UI over that config, not a second config mechanism.
- Domain: the `LlmProvider` seam and the risk gate / AUTO-SAFE default are locked (see [`domain.md`](../../agents/domain.md), CLAUDE.md, [ADR-0005](../../adr/0005-agent-loop-and-providers.md), [ADR-0006](../../adr/0006-safety-gate-and-sandbox.md)).

# Implementation notes

- Wire each T-12.1 control to the live config object loaded by T-8.3, and persist changes back through that same layer.
- **Theme**: switching updates the active theme immediately (the token resolver already supports both themes; this just flips the selection and repaints), and persists as the launch theme.
- **Font size**: applies across every block live (the mock drives a `--fs` variable used by the whole timeline); persist the chosen size.
- **Default provider**: writes the selected provider (Anthropic / OpenAI / Local) into the agent-loop config through the locked `LlmProvider` seam; does not change the seam, only the default selection.
- **Autonomy**: writes the selected policy (Ask each time / Auto-run safe / Full auto) into the agent config. Auto-run safe is the AUTO-SAFE default. The autonomy setting selects *policy*, and must stay consistent with the deterministic risk gate: it never lets a `Caution`/`Dangerous` verdict bypass confirmation except where a policy explicitly permits it, and it never disables the mandatory sandbox. See Notes on "Full auto".
- Keep the crate boundary: config types live where T-8.3 puts them; `aterm-ui` reads/writes config values, it does not own persistence.

# Acceptance criteria

- [ ] Changing the theme on Settings repaints the app immediately and the choice persists across restart.
- [ ] Changing font size re-flows every block live and persists across restart.
- [ ] Changing the default provider updates the agent-loop config (through the `LlmProvider` seam) and persists.
- [ ] Changing autonomy updates the agent config and persists; "Auto-run safe" remains the default; the risk gate's confirmation behavior for `Caution`/`Dangerous` and the mandatory sandbox are unaffected by the toggle (except as bounded by the flagged "Full auto" decision).
- [ ] No secret/API-key value is read or written by this ticket (that is T-8.3 / Keychain).

# Out of scope

- The Settings screen rendering + widgets (T-12.1).
- The config-file format, load order, and Keychain custody mechanics (T-8.3 owns those).
- Any new provider or agent-loop behavior beyond selecting the existing default.

# Notes

2026-07-01 (agent:fork): **Flag for owner - "Full auto" autonomy vs the locked safety model.**
The mock offers a "Full auto" autonomy segment, and its risk-gate state also has an
"Always approve `rm -rf ...`" affordance that sets autonomy to full. This sits in tension
with the locked decision (CLAUDE.md / [ADR-0006](../../adr/0006-safety-gate-and-sandbox.md)):
AUTO-SAFE is ON, but `Caution`/`Dangerous` verdicts "always require explicit confirmation",
and the Seatbelt sandbox is mandatory. This ticket does NOT implement a gate bypass. What
"Full auto" is permitted to mean - e.g. auto-approve only within an explicit allowlist the
user opted into, still sandboxed, versus a broader trust surface - is an owner/product +
security call. Implement the persistence plumbing for the setting, but treat any behavior
that would let a `Dangerous` verdict run without confirmation as `ready-for-human`: flag it
here and STOP rather than wiring an unconditional bypass.
