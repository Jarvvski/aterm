# ADR-0008: Shell integration - OSC-133/7 via env-var shims, nonce-gated, visible 3-state indicator

## Status

Accepted

## Context

Command boundaries, cwd, and exit codes are the signal that turns raw PTY output into the
block timeline. The dossier ([04-shell-integration.md](../research/04-shell-integration.md))
established that the right signal is the FinalTerm OSC-133 protocol (semantic marks A/B/C/D)
plus OSC 7 (cwd), injected without editing the user's dotfiles via launcher-time env-var
indirection, and that the prior prototype's worst sin here was silent zsh-only degradation.
`alacritty_terminal` does not parse OSC 133 (its issue #5850 is open), so the mark filter is
aterm's own, sitting before/around the emulator ([ADR-0007](0007-terminal-engine.md)).

## Decision

- **Consume OSC 133 (A/B/C/D) + OSC 7 as the canonical lifecycle**; ingest OSC 633 (VS Code)
  and OSC 1337 (iTerm2) opportunistically (map 633 A/B/C/D/E/P onto our model).
- **Inject hooks via launcher-time env-var indirection, never editing dotfiles**, one
  technique per shell, **for zsh, bash, and fish from day one**, version-branched:
  - **zsh:** a `ZDOTDIR` shim (sources the user's real config, then installs our hooks last);
    wrap `PS1` in zero-width `%{...%}` A/B marks and re-wrap from a `precmd` hook (defends
    against starship/p10k/zsh-defer clobbering `PS1`); a `__aterm_ran` guard so the first
    precmd's stale `$?` does not fabricate a D.
  - **bash:** an `ENV`/`--rcfile` bootstrap that re-sources the user's startup files in the
    correct order. Hooks via `PS0='${ preexec;}'` + `PROMPT_COMMAND` on bash >= 5.3, else the
    bundled `bash-preexec` shim (which supports **bash 3.1+**, so macOS's frozen 3.2 is
    covered) for older versions; degrade the indicator on 3.2 where the DEBUG-trap path is
    least reliable.
  - **fish:** `XDG_DATA_DIRS` vendor_conf.d injection + `fish_prompt`/`fish_preexec`/
    `fish_postexec` + `$status`/`$pipestatus`.
- **Nonce-gate every mark** (`tag=NONCE`, per aterm session). Marks whose nonce is
  absent/mismatched come from a nested un-integrated shell or a hostile program and must not
  mutate the outer block; this also lets us drop a prompt framework's own un-nonced OSC-133
  marks (defeats double-fire).
- **A visible, three-state integration indicator: Integrated / Heuristic / None**, with a
  one-click "why". Confirm "Integrated" only after seeing a nonce-matched `133;A`. If a
  supported shell yields no marks, fall back to a clearly-labeled heuristic mode (newline +
  cursor-at-col-0). **Degrade loudly, never silently** - this is the explicit fix for the
  prototype's #1 sin.
- **Suppress block creation on the alt screen, decided at FIRE TIME** (read the current
  alt-screen flag against the drained emulator state, because the toggling CSI may still be
  unparsed passthrough when the mark is first seen).
- Bundle the integration scripts as embedded resources, written to a per-session temp
  ZDOTDIR/bootstrap dir at spawn, idempotent (`ATERM_INTEGRATION_LOADED` guard), cleaned up on
  exit.

## Consequences

- Full zsh/bash/fish coverage from day one directly fixes the prototype's silent zsh-only
  degradation; the three-state indicator makes any shortfall honest and discoverable.
- Nonce gating defends against nested-shell mark folding and program spoofing, and turns
  prompt-framework double-marks into a non-issue (their marks lack our nonce, so we drop them).
- The mark filter is load-bearing and must handle split sequences across reads and both BEL
  and ST terminators exactly; it is unit-tested in `aterm-core` independent of any window.
- Edges remain that need a real test matrix: `exec`/`su`/`sudo -i` may not preserve our env
  vars; bash 3.2 DEBUG-trap reliability is unbenchmarked (mitigated by the degraded label and
  preferring Homebrew bash 5.x); tmux passthrough has documented mark-dropping quirks.
- SSH/Docker subshell integration is deferred past v1 and shown honestly as "remote - no
  integration"; the heuristic fallback covers unsupported/unintegrated shells with a clear
  label rather than a broken block UI.

## Alternatives considered

- **Editing the user's dotfiles** (the historical iTerm2 curl-installer approach). Rejected:
  invasive and non-idempotent; env-var indirection is the battle-tested no-dotfile approach
  (kitty's mechanism).
- **Inferring command boundaries from VT output** (prompt-regex heuristics as the primary
  signal). Rejected as the primary path: hooks are authoritative; heuristics are only the
  clearly-labeled fallback when integration is unavailable.
- **Silent degradation when integration is missing** (the prototype's behavior). Explicitly
  rejected: the visible three-state indicator replaces it.
- **Depending on OSC 633/1337 natively.** Rejected as the canonical path: 133+7 is the
  universal floor; 633/1337 are opportunistic ingest only.
