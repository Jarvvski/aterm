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
