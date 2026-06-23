---
id: T-2.2
epic: EPIC-2-shell-integration-blocks
title: Shell-integration shim extraction + ZDOTDIR injection (zsh)
status: ready-for-agent
labels: [core, shell-integration]
depends_on: [T-1.1]
---

# Goal

Bundle the integration scripts as embedded resources, write them to a per-session temp dir at spawn, and inject zsh integration via a `ZDOTDIR` shim - zero dotfile edits, surviving `exec zsh`, idempotent, cleaned up on exit. This emits the nonce'd OSC 133/7 marks T-2.1 consumes.

# Context

- Research: [04-shell-integration.md](../../research/04-shell-integration.md) sections 2 (zsh ZDOTDIR shim), 4 (detection), Recommendations 2, 9. The prototype's zsh shim design is the reference: zero-width `%{...%}` PS1 wrap of A/B, `precmd`/`preexec` for C/D, `__aterm_ran` first-precmd guard, percent-encoded `cmdline=`, re-wrap PS1 from `precmd` to defend against starship/p10k/zsh-defer.

# Implementation notes

- Crate: `aterm-core`. Module `integration`. Scripts embedded via `include_str!` and materialized to a per-session temp `ZDOTDIR` dir.
- zsh `.zshenv` in the shim dir: restore the original `ZDOTDIR`, source the user's real startup files in order, then install integration last. Preserve/restore a pre-existing user/system `ZDOTDIR`; only inject when at least one of `.zshenv/.zprofile/.zshrc/.zlogin` exists in the original `ZDOTDIR`.
- The shim sets a per-session nonce (random `[A-Za-z0-9]+`) into the env so every mark carries `tag=NONCE` (must match T-2.1's expectation). Emit OSC 7 via `printf '\033]7;file://%s%s\007' "${HOST}" "${PWD}"`.
- Idempotency guard `ATERM_INTEGRATION_LOADED`. Clean up the temp dir on session exit.
- Spawn integration: set `.env("ZDOTDIR", shim_dir)` on the `CommandBuilder` (the T-1.1 hook point) when the resolved shell is zsh.
- Shell detection (the fix for silent zsh-only degradation): resolve the launch shell from `$SHELL`/login shell, verify against the PTY child's argv0; map basename -> {zsh, bash, fish, other}. This ticket handles zsh; bash/fish in T-2.3; "other" -> no injection.

# Acceptance criteria

- Spawning zsh with the shim produces nonce'd OSC 133 A/B/C/D + OSC 7 for a command cycle (assert via the T-2.1 filter, or by capturing the raw stream).
- The user's real `.zshrc` still loads (a sentinel exported in a test `.zshrc` is present in the child env).
- `exec zsh` inside the session preserves integration (ZDOTDIR survives).
- A pre-existing `ZDOTDIR` is restored for the user's config and not clobbered.
- starship/p10k emitting their own PS1 does not drop our marks (precmd re-wrap), and any marks they emit without our nonce are droppable downstream.
- Temp shim dir is removed on exit.

# Out of scope

- bash + fish hooks (T-2.3).
- The filter that consumes the marks (T-2.1).
- The visible indicator (T-2.6).
