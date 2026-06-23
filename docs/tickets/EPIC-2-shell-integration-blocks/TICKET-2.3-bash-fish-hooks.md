---
id: T-2.3
epic: EPIC-2-shell-integration-blocks
title: bash + fish hooks (version-branched)
status: ready-for-agent
labels: [core, shell-integration]
depends_on: [T-2.2]
---

# Goal

Ship full bash + fish integration from day one, version-branched, so the prototype's zsh-only limitation is gone. bash via `ENV`/`--rcfile` bootstrap (PS0+PROMPT_COMMAND on 5.3+, else bundled bash-preexec for 3.1+), fish via `XDG_DATA_DIRS` vendor_conf.d injection.

# Context

- Research: [04-shell-integration.md](../../research/04-shell-integration.md) section 2 (bash, fish) + summary table, Recommendation 3. Corrected fact: bash-preexec supports bash 3.1+, so macOS's frozen 3.2 is covered (degraded label).

# Implementation notes

- Crate: `aterm-core`. Extend `integration` (T-2.2).
- **bash**: launch with `ENV` pointing at our bootstrap (or `--rcfile`), which re-sources the user's startup files in correct order (preserve `/etc/profile`), then installs hooks. Detect the running version (not the path).
  - bash >= 5.3: `PS0='${ preexec;}'` (command-substitution-in-current-shell, no subshell) + `PROMPT_COMMAND` for precmd; read `$?`/`PIPESTATUS` first in precmd.
  - bash 3.1-5.2 (incl. macOS 3.2): bundle `bash-preexec` (MIT) using the DEBUG trap + PROMPT_COMMAND; filter the DEBUG trap (fires on every simple command); subshell support off. Mark 3.2 as least reliable (drives the "Heuristic" downgrade in T-2.6).
- **fish**: prepend our integration dir to `XDG_DATA_DIRS`; fish auto-sources `*.fish` in `vendor_conf.d`; the script cleans up the env var. Hooks: `fish_prompt` event (A/B), `fish_preexec` (C), `fish_postexec` + `$status`/`$pipestatus` (D). Min fish 3.2.0 for vendor injection; modern fish has built-in OSC 133.
- All shims emit the same nonce'd OSC 133/7 contract as zsh (T-2.2), so T-2.1 consumes them uniformly.

# Acceptance criteria

- bash 5.3 (if available on the runner) emits correct marks via PS0/PROMPT_COMMAND.
- macOS system bash 3.2 emits marks via bundled bash-preexec; the version is detected and reported for the indicator.
- fish >= 3.2 emits marks via vendor_conf.d injection; `$pipestatus` populates the exit code.
- The user's real bash/fish config still loads (sentinel test as in T-2.2).
- An unknown shell (dash/nu/pwsh) gets no injection and is reported as "Unknown" to T-2.6.

# Out of scope

- SSH/Docker subshell integration (explicitly deferred per dossier; shown as "remote - no integration").
- The OSC filter (T-2.1) and indicator UI (T-2.6).
