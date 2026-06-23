---
id: T-7.4
epic: EPIC-7-perf-harness
title: Resize/reflow perf check + shell-matrix hardening
status: ready-for-agent
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
