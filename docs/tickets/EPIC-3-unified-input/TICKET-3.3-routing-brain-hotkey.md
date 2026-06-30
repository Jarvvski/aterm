---
id: T-3.3
epic: EPIC-3-unified-input
title: Routing brain (disposition gates) + hotkey toggle
status: done
labels: [app, input]
depends_on: [T-3.1, T-2.1]
---

# Goal

Implement the input disposition brain that decides where a keystroke/Enter goes, with the gate ordering: preedit-active -> alt-Enter one-shot-to-agent -> degraded/raw -> alt-screen passthrough -> in-flight -> Enter routes by `mode`. Plus the mode-toggle hotkey that flips only `InputModel.mode`.

# Context

- Research: [05-unified-input-ux.md](../../research/05-unified-input-ux.md) sections 2 (the trap), 4 (hotkey, in-flight policy) and Recommendations 5, 10-11. Owner open-questions #1 (toggle key default `Cmd-/`, rebindable?), #2 (agent-turn input policy), #7 (`Opt-Enter` one-shot). The dossier locks: typed text preserved across toggle; visible mode indicator (caret tint + glyph), no banner.

# Implementation notes

- Crate: `aterm-app` (wires `aterm-ui` input + `aterm-agent` + `aterm-core` shell). Module `routing`.
- Disposition gates, in priority order:
  1. **preedit-active** (T-3.2): Enter/Tab/Esc owned by the IME; never submit/route.
  2. **alt-Enter (`Opt-Enter`)**: one-shot send-to-agent regardless of `mode` (keep the prototype's `SubmitToAgent`).
  3. **degraded/raw**: if shell integration not live (T-2.6 "None"), fall back to classic ZLE/raw passthrough; show it (not silent).
  4. **alt-screen**: route keys straight to PTY (T-3.4 encodes them).
  5. **in-flight (foreground program reading stdin)**: passthrough to PTY.
  6. **Enter**: route by `InputModel.mode` - Shell submits the committed command to the PTY; Agent sends the text to the agent loop (Epic 5).
- Hotkey toggle: default `Cmd-/` (dossier proposal; rejecting `Ctrl-Space` = macOS IME switch, `Cmd-.` = SIGINT muscle memory). Make it rebindable via config. The toggle calls `InputModel.toggle_mode()` only - NO text mutation.
- Agent-turn input policy (owner open-question #2): keep the input box live during agent turns (queue next message; `Esc` always interrupts the agent) rather than the prototype's full swallow. If the owner has not confirmed, default to live + Esc-interrupts and flag.

# Acceptance criteria

- Pressing the toggle hotkey flips mode and the text is unchanged (joint test with T-3.1).
- Enter in Shell mode submits to the PTY; Enter in Agent mode dispatches to the agent (stub the agent sink if Epic 5 not landed).
- `Opt-Enter` sends to the agent even in Shell mode.
- During IME composition, Enter does not submit (joint with T-3.2).
- While a foreground TUI is reading stdin or alt-screen is active, keys pass through to the PTY.
- `Esc` interrupts an in-progress agent turn (stub-verifiable).
- When integration is "None", input is visibly in raw/degraded mode.

# Out of scope

- Key-to-bytes encoding (T-3.4).
- The mode indicator visuals (T-3.6).
- The agent loop itself (Epic 5).

# Notes

**2026-06-25 (agent): the disposition brain landed + replaced the session stopgap;
ticket STAYS `ready-for-agent` (the keyboard-modifier seam remains).** The pure
decision logic is implemented and tested, and `session.rs` now routes through it
instead of the ad-hoc stopgap, preserving the shell-echo interactivity byte-for-byte.

- New `aterm-app::routing`: `decide(KeyInput, &RoutingContext) -> Disposition` - the
  priority-ordered gates (preedit -> toggle -> Esc-interrupt -> alt-Enter ->
  degraded/alt-screen/in-flight passthrough -> Enter-by-mode -> edit). 9 unit tests
  cover all 7 ACs at the decision level + the gate-precedence edges. The model's
  opinion never enters the decision.
- `session.rs` rewired: `on_key` builds the context from live state and performs the
  disposition. Shell-mode editing still mirrors raw to the PTY (the shell echoes it -
  the T-3.6 widget does not render the `InputModel` yet), so the PTY byte stream is
  what the scaffold sent; a 3-lens adversarial review confirmed no interactivity
  regression. The new alt-screen/degraded passthrough also fixes a latent stopgap
  bug (alt-screen + Agent-mode keys now reach the TUI instead of the hidden box).

**Honest AC status:**
- AC2 (Enter Shell->PTY / Agent->agent) - MET (agent dispatch is the EPIC-5 log stub
  the AC permits).
- AC5 (alt-screen passthrough) + AC7 (integration `None` -> degraded/raw) - MET and
  sourced LIVE (`Snapshot.alt_screen`, `IntegrationStatus::None`).
- AC1 (toggle flips mode, text unchanged) - the toggle WORKS and `InputModel::toggle_mode`
  preserves text, but it is bound to a `Tab` PLACEHOLDER; the real `Cmd-/` chord needs
  the modifier seam (below).
- AC6 (Esc interrupts an agent turn) - decision MET + stub-verifiable; a live turn is
  EPIC-5.
- AC3 (Opt-Enter -> agent) - the brain decides it, but it is NOT runtime-reachable: the
  `aterm-ui` `on_key` seam does not pass keyboard modifiers, so `alt` is always false.
- AC4 (IME composition -> no submit) - decision tested; `preedit_active` is always false
  until the IME lands (joint with T-3.2).

**Remaining to reach `done`:** wire the keyboard-MODIFIER seam through
`aterm-ui::UiCallbacks::on_key` (track winit `ModifiersChanged`) so the real `Cmd-/`
toggle chord + `Opt-Enter` work and `Tab` is freed; live IME `preedit` (T-3.2); a real
agent turn for SubmitAgent/InterruptAgent (EPIC-5); `foreground_reading_stdin`
detection; full key->bytes encoding for passthrough (T-3.4). 9 routing tests; full gate
green at `-D warnings`. No version bump (no user-visible behaviour change - the byte
stream is preserved).

**Done 2026-06-30 (agent).** The modifier seam + the two remaining live signals landed
across four focused commits (two `aterm-core` enablers, the seam, the adoption). T-3.3's
own scope is complete; the only residuals are blocked on *other* tickets (T-3.2, EPIC-5).

- **Modifier-aware key seam (the headline).** New neutral `aterm_ui::KeyPress { named,
  ch, text, mods: Mods }` carries the logical character + Cmd/Opt/Ctrl/Shift through
  `UiCallbacks::on_key`; `app.rs` tracks winit `ModifiersChanged` and folds it into each
  press. A new pure `routing::classify(&KeyPress, &KeyBinding) -> KeyInput` resolves the
  real chords: the configurable `Cmd-/` toggle (matched on `ch` since macOS suppresses
  `text` under Command), `Opt-Enter` (alt-Enter), Escape, and everything else (incl. a
  now-freed `Tab`) -> `Other`. The `Tab` placeholder is gone; `Tab` again sends `\t`
  (shell completion). **AC1** (toggle flips mode, text preserved) and **AC3** (Opt-Enter
  -> agent) are now runtime-reachable, not just decided.
- **Rebindable toggle (`KeyBinding`).** `Cmd-/` default via `KeyBinding::default_toggle`;
  `KeyBinding::parse("ctrl+t")` + the `ATERM_TOGGLE_KEY` env override make it rebindable
  today (the `config.toml` loader is EPIC-8). Exact-modifier match (so `Cmd-/` is not
  `Cmd-?`); char match is case-insensitive.
- **`foreground_reading_stdin` is sourced live (AC5 completion).** `aterm-core` captures
  the shell's pgid at spawn and adds `Engine::foreground_is_foreign()` (compares it to the
  live `tcgetpgrp` foreground pgid); `routing_context` feeds it, so keys typed while a
  foreground command runs pass through to that program (not the input box) - not just the
  alt-screen case.
- **Key encoder adopted on passthrough (T-3.4's hand-off).** The alt-screen / foreground /
  degraded path now maps the press to `keys::KeyStroke` (`routing::keystroke_for`) and runs
  it through `keys::encode`, reading the live DECCKM/Kitty flags off the `Snapshot` (new
  `app_cursor`/`disambiguate` fields). Arrows (CSI<->SS3), `Ctrl-C`/`Ctrl-Z`, Home/End/Page/
  F-keys all reach TUIs and degraded ZLE correctly - the old arrow stubs sent nothing.
  `Cmd`/Super is treated as an app-level modifier and never forwarded to the program. The
  Kitty `CSI u` branch is wired but dormant (the Kitty protocol is not enabled in our `Term`
  config - a deliberate separate decision; the encoder falls back to legacy/DECCKM).

**Residuals (blocked on other tickets, decision tested):**
- **AC4** (IME composition -> no submit): `preedit_active` is always `false` until the live
  IME feed lands (**T-3.2**); the gate is tested.
- **AC6** (Esc interrupts a *live* agent turn): `agent_turn_active` is always `false` until
  the agent loop lands (**EPIC-5**); the interrupt decision is tested + stub-verifiable.

**Tests / gate.** 18 `routing` unit tests (9 `decide` + 6 `classify`/`KeyBinding` + 3
`keystroke_for` incl. an end-to-end encode), plus 2 `aterm-core` engine tests
(`pgrp_is_foreign` exhaustive + a live `foreground_is_foreign` via `/bin/cat`) and the
`Snapshot` key-mode test. `mise run fmt && lint && build && test` green; clippy clean at
`-D warnings`. User-visible, so CHANGELOG `## Unreleased` gained two entries (no version
bump - the repo accumulates under Unreleased until a release is cut).
