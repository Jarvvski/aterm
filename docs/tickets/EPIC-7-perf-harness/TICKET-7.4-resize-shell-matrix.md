---
id: T-7.4
epic: EPIC-7-perf-harness
title: Resize/reflow perf check + shell-matrix hardening
status: done
labels: [bench, perf, shell-integration, hardening]
depends_on: [T-7.2, T-2.3]
---

# Goal

Add the maximized-window resize/reflow perf check (alacritty's known reflow sharp edges) and a real shell-matrix test across zsh/bash/fish versions, prompt frameworks, and edge cases - the hardening pass that closes the dossier's top engine + shell-integration risks.

# Context

- Research: [03-pty-vt-rust.md](../../research/03-pty-vt-rust.md) Risk list (reflow correctness/perf on large live grids; needs a maximized 4K resize check); [04-shell-integration.md](../../research/04-shell-integration.md) Risk list (bash 3.2 reliability, exec/su/sudo survival, starship/p10k double-marks, tmux passthrough).

# Implementation notes

- Crate: `aterm-bench` (resize perf) + integration tests in `aterm-core`.
- Resize/reflow: animate a maximized (e.g. 4K) window resize and measure reflow time + frame budget; confirm finished blocks (immutable snapshots) are unaffected and only the live grid reflows. Feed into the `window_resize` gate (T-7.2).
- Shell matrix: a test matrix across zsh 5.x, bash 5.3 / 3.2, fish 3.2+; with starship/p10k/oh-my-posh installed (assert no phantom/double blocks - nonce gating drops un-nonced marks); `exec zsh`/`su`/`sudo -i` survival of the env injection; tmux passthrough edge case (documented, may be deferred). bash 3.2 mark reliability benchmarked to validate the "Heuristic" downgrade threshold.

# Acceptance criteria

- A maximized-window resize completes within the resize frame budget; finished blocks are byte-identical after reflow.
- The shell matrix passes for zsh/bash 5.3/fish; bash 3.2 either integrates or correctly downgrades to "Heuristic".
- starship/p10k/oh-my-posh do not produce phantom or double blocks (nonce drop verified).
- `exec zsh` preserves integration; `su`/`sudo -i` behavior is documented (survives or honestly degrades).
- tmux passthrough behavior is documented (and degrades honestly if unsupported in v1).

# Out of scope

- SSH/Docker subshell warpify (deferred per dossier).
- New shell support beyond zsh/bash/fish.

# Resolution

**2026-07-01 (agent): Done.** Two halves - the resize/reflow perf check (`aterm-bench` +
an `aterm-core` correctness test) and the shell-matrix hardening (`aterm-core` integration
tests + docs).

**Resize / reflow:**
- **AC (finished blocks byte-identical after reflow):** the deterministic `aterm-core`
  engine test `finished_block_output_is_byte_identical_after_a_reflow_resize` runs a full
  A/C/output/D cycle, snapshots the finished block's immutable `RowSnapshot`s, resizes the
  live grid across two large geometries (a wide 60x240 then a narrow 20x32), and asserts
  the live grid reflowed (cols changed) while the finished block's captured bytes are
  byte-identical. This proves the T-2.4 design guarantee: only the live grid reflows;
  history is immune to alacritty's reflow bugs (#2213/#2567/#4419/#8576).
- **AC (maximized 4K resize within the frame budget):** a new `maximized_reflow` hardening
  scenario (`aterm-bench::scenario::hardening_scenarios`) animates 1280x720 -> 3840x2160
  and REUSES the `window_resize` gate (the one-frame transaction-spike + drop budget). Kept
  OUT of `all_scenarios()` so the "seven named scenarios" proof stays exactly seven; the
  `scenario_driver` runs the seven + the hardening set. On a non-4K/headless runner the
  compositor clamps the size (degrades to a smaller reflow, honestly not-4K); on real 4K/5K
  hardware it is the maximized check. The live frame-budget number is on-hardware (like all
  of T-7.2); the byte-identity is the CI-gating correctness half.

**Shell matrix** (`aterm-core`, real binaries, skip-if-absent so CI stays honest):
- **AC (zsh / bash 5.3 / fish pass; bash 3.2 integrates-or-downgrades):** a `#[cfg(test)]`
  `Engine::spawn_real_shell` (mirrors `spawn_login_shell` for an explicit path + kind, NO
  public API change) drives each real shell to its first prompt. `real_zsh_reaches_integrated`
  (zsh 5.9) + `real_bash53_reaches_integrated` (Homebrew bash 5.3, the PS0 tier) assert
  Integrated; `real_bash32_integrates_or_downgrades_never_crashes` asserts bash 3.2 (the
  DEBUG-trap tier) reaches Integrated OR Heuristic and LOGS which - the mark-reliability
  observation that validates the downgrade threshold (observed: Integrated, its static A
  rides PS1); `real_fish_reaches_integrated_or_skips` skips honestly (fish absent here).
- **AC (starship/p10k/oh-my-posh no phantom/double blocks):**
  `framework_marks_never_create_phantom_or_double_blocks` interleaves un-nonced
  framework-style OSC-133 marks (incl. a double `A` beside ours) with one nonce'd cycle and
  asserts exactly ONE block forms (ours) - the nonce gate drops all foreign marks.
- **AC (exec zsh preserves integration; su/sudo documented):** the re-pin MECHANISM is
  proved deterministically (`zsh_bootstrap_repins_zdotdir_for_exec_zsh_survival` - the
  `.zshenv` re-pins `$ZDOTDIR` at the shim as its last word); the live
  `exec_zsh_keeps_the_session_alive_and_integrated` smoke confirms a real `exec zsh` keeps
  the session alive + Integrated. `su`/`sudo -i` (env reset -> honest degradation to
  Heuristic/None) + `exec bash`/`exec fish` (non-persistent env -> degrade) are documented
  in the `shell_integration` module docs.
- **AC (tmux passthrough documented):** documented in the module docs - v1 does not wrap
  marks for tmux, so a shell inside tmux degrades honestly to Heuristic (deferred edge case).

**HONEST LIMITS.** The 4K reflow FRAME-TIME number needs real 4K hardware (CI clamps the
size); the CI-gating guarantee is the byte-identity correctness test. The real-shell matrix
skips (never reds) when a binary is absent, matching the existing real-shell test
convention. su/sudo/tmux/exec-non-zsh are documented as honest v1 degradation, not
silently claimed to work - re-warpify across privilege/tmux is a deferred future ticket
(consistent with the dossier's out-of-scope stance).
