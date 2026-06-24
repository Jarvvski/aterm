---
id: T-2.6
epic: EPIC-2-shell-integration-blocks
title: Three-state integration indicator + heuristic fallback
status: ready-for-human
labels: [ui, shell-integration]
depends_on: [T-2.3, T-2.5]
---

# Goal

Surface a visible three-state integration status (Integrated / Heuristic / None) with a "why", confirmed only after a nonce-matched OSC 133;A is seen, plus a labeled heuristic block detector for unsupported/unintegrated shells. Never degrade silently (the prototype's #1 sin).

# Context

- Research: [04-shell-integration.md](../../research/04-shell-integration.md) section 4 + Recommendations 4, 7. Owner open-question #5/#3 (heuristic fallback vs honest "no blocks") - default to the labeled heuristic per the dossier lean.

# Implementation notes

- Crate: `aterm-core` owns the state (`IntegrationStatus { Integrated, Heuristic, None }` + a reason string); `aterm-ui` renders the indicator.
- Confirm "Integrated" only after the first nonce-matched `133;A` within a short window after spawn. If the shell is supported but no marks arrive -> "Heuristic" + enable the regex/heuristic block detector (newline + cursor-at-col-0 prompt detection). If unsupported shell -> "None".
- Indicator UI: a single glyph + tooltip in the block gutter or status strip (iA-restrained, uses tokens from Epic 4); a one-click "why?" explaining what is missing (e.g. "running fish 3.1 - upgrade to 3.2+ for native blocks", or "bash 3.2 - marks may be unreliable").
- Heuristic detector lives in `aterm-core` (block module) and produces clearly-labeled approximate blocks.

# Acceptance criteria

- A zsh/bash/fish session with working hooks shows "Integrated" only after a nonce-matched A.
- A supported shell with hooks disabled shows "Heuristic" and produces labeled approximate blocks.
- An unsupported shell (dash) shows "None".
- The "why?" reason string is populated for each non-Integrated case.
- Status transitions are observable and never silent.

# Out of scope

- The shims (T-2.2, T-2.3) and lifecycle (T-2.5).
- Final token/visual polish (Epic 4 supplies tokens; this wires the state).

# Notes

**Landed 2026-06-24** (jj, not pushed). The three-state shell-integration indicator
plus a labeled-heuristic fallback block detector; degrades loudly, never silently.

- **State (`aterm-core/src/integration.rs`).** `IntegrationStatus { Integrated,
  Heuristic, None }` (exactly three, the indicator glyph keys off it) paired with a
  typed `IntegrationReason` (`Confirmed`/`Probing`/`HooksSilent`/`ShimInstallFailed`/
  `UnsupportedShell`) whose `why()` is the one-click explanation. The decision logic is
  the pure `IntegrationMonitor` (no clock, no threads), exhaustively unit-tested - the
  engine feeds it the two facts it cannot derive: `confirm()` (a nonce-matched `A`) and
  `note_window_elapsed()` (the confirmation window passed). `Integration` is `Copy` and
  encodes to one `u8` so the model thread publishes it lock-free.
- **Engine wiring (`engine.rs`).** `Engine::integration_status()` reads the published
  atomic. The model confirms on a nonce-matched OSC-133 `A` **only when a shim is
  installed** (the nonce-armed scanner) - so a forged `A` in untrusted command output
  cannot flip the indicator to Integrated (AC1 safety gate). A one-shot
  confirmation-window timer (`INTEGRATION_CONFIRM_WINDOW` = 5s; the prompt usually
  arrives in tens of ms) drives Probing -> HooksSilent if no mark arrives; a
  nonce-matched `A` first disarms it -> Integrated.
- **AC mapping.** AC1 nonce-matched-A -> Integrated: monitor `ac1_*` + engine
  `nonce_matched_a_confirms_integrated_at_the_engine` + the skip-if real-shell
  `login_shell_reaches_integrated_after_first_prompt`, with the forged-A negatives
  (`forged_a_without_a_shim_never_confirms_integrated`,
  `nonce_mismatched_a_never_confirms_integrated`). AC2 supported-but-silent ->
  Heuristic + approximate blocks: monitor `ac2_*` + `heuristic_session_produces_approximate_blocks`.
  AC3 unsupported -> None: monitor `ac3_*` + `unsupported_command_host_reports_integration_none`.
  AC4 why populated: `ac4_every_non_integrated_reason_has_a_why`. AC5 observable
  transitions: `ac5_status_transitions_are_observable_*`.
- **Heuristic detector (`block.rs` `HeuristicSegmenter`).** Structural, NOT
  prompt-text/sigil matching: a settled prompt = output quiescent + cursor mid-line
  (`col > 0`) + a newline seen since the last prompt + the line was not `\r`-redrawn.
  Each command cycle becomes one finished, `approximate`-labeled block. Gated to run
  only while `heuristic_active()` and OFF the alt screen (so a TUI cannot fabricate
  blocks). Driven from the model thread: `observe_output` (stream newline/`\r` signal)
  + `note_prompt_if_idle` (at the idle coalesce flush).
- **UI seam (`aterm-ui`).** `UiCallbacks::integration_status()`, `Frame.integration`,
  and a pure `IntegrationIndicator` presentation (glyph + label + "why?" + token
  color), unit-tested. `Session` returns the live engine status.

**Adversarial review applied** (two rounds, ultracode). Round 1 (5 lenses) confirmed
the indicator core correct and surfaced 8 heuristic-detector findings; the detector was
then **rewritten** from a sigil + idle-grid-sample design (which missed fast commands,
fabricated blocks on typing, produced zero blocks for non-sigil `❯` prompts, and was
not alt-screen-gated) to the structural stream-driven design above. Round 2 (3 lenses)
confirmed the rewrite fixed those but caught a real residual: a running command that
stalls mid-line (a `\r`-redrawn progress bar) looked like a prompt and over-segmented.
Fixed with the `\r`-redraw guard + a regression test
(`heuristic_progress_bar_redraw_does_not_fabricate_blocks`) and a tightened
exact-count engine test.

**Follow-ups for the human (why `ready-for-human`):**
1. **The indicator's on-screen DRAWING is deferred to EPIC-4.** This ticket wires the
   live state to the renderer (`Frame.integration` + the tested `IntegrationIndicator`
   glyph/label/why/color mapping); placing it in the block gutter / status strip and
   the hover tooltip are EPIC-4 visual polish (it supplies the final tokens). None of
   the five ACs require pixels - they are all state-behavior - and all are tested.
2. **The heuristic fallback has an inherent residual + is an open OWNER product
   question.** A pure output-stream heuristic cannot perfectly tell a settled shell
   prompt from a command that printed a partial line (no `\r`, no trailing newline) and
   is *waiting* - a `Password:` prompt, `read -p`, a bespoke inline progress indicator -
   so such a command may get one extra labeled-`approximate` block. The `\r`-guard kills
   the dominant progress-bar case; this residual is fundamental. This is exactly
   `04-shell-integration.md` open-question #3 (labeled-heuristic fallback vs. an honest
   "no blocks" mode), an explicit product call - flagged for the owner. The whole
   detector is isolated (the `HeuristicSegmenter` + the `publish`/`observe_output` gate),
   so swapping to "no blocks" is a small change if the owner prefers it.
3. **`INTEGRATION_CONFIRM_WINDOW` (5s) is an untuned heuristic** - revisit against the
   T-7.x perf/shell matrix on real hardware.
4. **AC2 bash-version surfacing (carried from T-2.3).** The bash tier is detected
   in-shell (`BASH_VERSINFO`) but the version is not yet reported to Rust, so the
   indicator cannot yet say "bash 3.2 - upgrade for reliable blocks". `IntegrationReason::
   why()` is generic; the UI can prepend the `ShellKind`. Surfacing the version (a mark
   attribute or a `bash --version` probe) is the remaining piece.
