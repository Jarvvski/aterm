---
id: T-2.5
epic: EPIC-2-shell-integration-blocks
title: Block lifecycle state machine + alt-screen suppression
status: ready-for-human
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

# Notes

**Landed 2026-06-24** (jj, not pushed). All 5 acceptance criteria are implemented
and verified by tests in `crates/aterm-core/src/block.rs`; the engine drives the
state machine in lockstep with the grid in `crates/aterm-core/src/engine.rs`.

- **State machine** (`BlockSegmenter::apply`): Idle->Prompt->Command->Output->Idle.
  `cmdline=` sets the authoritative command text (`pending_command`, staged on the
  `CommandLine` mark, consumed when the block opens on `C`). AC1 + the `cmdline=`
  wiring are covered by `ac1_normal_cycle_*` and `ac1_cmdline_mark_sets_block_command`.
- **Alt-screen suppression**: `apply()` drops ALL marks while `alt_screen` is true,
  so a TUI's own OSC-133 chatter creates no phantom blocks; the launching command is
  flagged interactive on the false->true edge (`set_alt_screen`). Covered by
  `ac2_alt_screen_yields_one_interactive_block_no_phantoms`.
- **Fire-time correctness (the architectural crux)**: `Model::process_output`
  interleaves VT-feed and mark-application by stream offset - it feeds the grid up to
  each mark's offset *before* applying it, so the alt-screen flag read at fire time
  reflects the true emulator state (fixes the case where a `C` and the alt-screen-enter
  CSI share one read chunk). With no marks present this collapses to a single feed, so
  there is no flood/coalesce/reply regression (verified by the existing engine tests).
- **Hardening**: missing-`D` recovery (a fresh `A` auto-closes a dangling block with
  exit=unknown - `ac3_*`); empty-Enter makes no block (`ac4_*`); nonce-mismatched marks
  never reach the segmenter (`ac5_*`, end-to-end through `OscScanner::with_nonce`).

**Adversarial review applied** (this turn). The 2-lens review surfaced 3 confirmed
findings; 2 cheap, real hardenings were fixed and pinned with tests
(`altscreen_entry_clears_staged_command_line`, `altscreen_entry_only_flags_the_open_output_block`):
- `set_alt_screen` now flags interactive only when `phase == Output` (never a stale
  block left running by an earlier missing `D`) and clears any staged `pending_command`
  on alt-screen entry (closes a latent cross-alt-screen command leak).

**Follow-ups for the human (why `ready-for-human`):**
1. **Grid-row output capture on `D` is deferred.** `BlockList::set_block_output` (T-2.4)
   exists and is ready, but the engine does not yet snapshot the grid region for a
   finished block's `output_span` into `RowSnapshot`s. Until it does, `Block::output`
   is always empty and `Block::is_thin()` falls back to a **conservative** byte-span
   check (`output_span.end == start`): exact for a truly silent command, but a no-output
   command that emits a non-stripped zero-width control (an OSC 7 cwd report, OSC 1337)
   advances the clean offset and is rendered as an empty card rather than a thin marker.
   This errs toward a card, never toward collapsing real output. When grid-row capture
   lands, key `is_thin` off `output.is_empty()` alone and drop the byte-span clause.
2. **`Interactive` is a `Block.interactive: bool` flag, not a block variant.** The
   ticket's `Interactive` *variant* is folded into the deferred `Block`-enum redesign
   that is the T-2.4 owner-confirm item; the flag delivers the behavior (one compact
   block, no captured output, no phantoms) non-breakingly until that decision is made.
