---
id: T-2.5
epic: EPIC-2-shell-integration-blocks
title: Block lifecycle state machine + alt-screen suppression
status: ready-for-agent
labels: [core, block-model, shell-integration]
depends_on: [T-2.1, T-2.4]
---

# Goal

Drive the BlockList from the marker stream: an Idle->Prompt->Command->Output->Idle state machine with hardening rules, and suppress block creation while the alt screen is active - the decision made at fire-time against the drained emulator state.

# Context

- Research: [04-shell-integration.md](../../research/04-shell-integration.md) section 5 (marker lifecycle + hardening rules) and Recommendation 6; [03-pty-vt-rust.md](../../research/03-pty-vt-rust.md) section D (full-screen apps live outside the block list; alt-screen suppression at fire time).

# Implementation notes

- Crate: `aterm-core`. Module `block` (state machine driving T-2.4) consuming T-2.1's offset-tagged marks.
- State machine: Idle -on A-> begin block (record cwd from last OSC 7); Prompt -on B-> split prompt vs command (in the controlled UI the command comes from our input box, so B..C echo can be suppressed/collapsed); Command -on C-> output-start, set authoritative command text from `cmdline=` if nonce matches; Output -on D-> finalize with exit code, return to Idle.
- Fire marks only once the emulator has drained to the mark's offset (keeps marks in lockstep with the grid - the prototype's offset-tagged parse-then-fire architecture).
- Alt-screen suppression: while `TermMode::ALT_SCREEN` is set, suppress block creation; decide at FIRE TIME (read the current alt-screen flag), because the toggling CSI may still be unprocessed passthrough when the mark is first seen. On alt-screen entry the running command becomes a compact `Interactive` block ("ran vim - 12s"), no captured output; on exit, resume the block list.
- Hardening rules: missing-D recovery (an `A` while Output-open auto-closes the previous block with exit=unknown); empty-command collapse (A->B->A or C->D with no output collapses to a thin marker); prefer explicit `D;<code>` for exit, optionally carry `$pipestatus` as a nonce-gated attribute.

# Acceptance criteria

- A normal command cycle produces one finalized CommandBlock; the state machine returns to Idle.
- Running `vim` (alt-screen) produces a single `Interactive` block, not phantom blocks from any 133 marks the TUI emits.
- A Ctrl-C'd command (A arrives with no D) auto-closes the prior block with exit=unknown.
- An empty Enter collapses to a thin marker, not an empty card.
- A mark whose nonce mismatches (nested un-integrated shell) does not mutate the outer block.

# Out of scope

- The mark filter itself (T-2.1) and the BlockList data structure (T-2.4).
- The integration indicator (T-2.6) and rendering (T-2.7).
