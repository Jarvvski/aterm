---
id: T-3.3
epic: EPIC-3-unified-input
title: Routing brain (disposition gates) + hotkey toggle
status: ready-for-agent
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
