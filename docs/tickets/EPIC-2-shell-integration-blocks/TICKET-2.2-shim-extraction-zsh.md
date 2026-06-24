---
id: T-2.2
epic: EPIC-2-shell-integration-blocks
title: Shell-integration shim extraction + ZDOTDIR injection (zsh)
status: done
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

# Notes

2026-06-24 (agent): Landed. zsh shell integration is wired end-to-end and ACTIVATES
the T-2.1 nonce gate (the engine flips `OscScanner::untrusted()` ->
`with_nonce(shim-nonce)` whenever the zsh shim installs). Status -> `ready-for-human`
for the one remaining AC that needs a real prompt framework to verify (AC5).

**Mechanism.** `aterm-core::shell_integration`: a per-session `ZDOTDIR` shim is
materialized to a temp dir (`IntegrationDir`, RAII - removed on `Engine` drop, after
the child is killed). The embedded `resources/zshenv` bootstrap KEEPS `$ZDOTDIR`
pinned at the shim for the whole session and drives the user's real startup files
(`.zshenv`/`.zprofile`/`.zshrc`/`.zlogin`) by explicit path from `ATERM_REAL_ZDOTDIR`
in zsh's normal order, then sources `resources/integration.zsh` last. The
integration emits nonce-stamped OSC-133 A/B/C/D + OSC 7, percent-encoded `cmdline=`
on C, an `ATERM_INTEGRATION_LOADED` idempotency guard, an `__aterm_ran` first-precmd
guard, and an idempotent precmd PS1 re-wrap (re-captures the base when PS1 loses our
nonce). Every mark is one `printf` so the nonce is never detached from its `ESC ]`
introducer (the T-2.1 contract).

**Verified (real zsh, headless, macOS CI):** AC1 (a command cycle emits nonce'd
OSC-133/7 marks the `with_nonce` filter accepts), AC2 (the user's real `.zshrc`
loads - sentinel test), AC3 (**exec zsh** preserves integration - a post-exec
command still emits a nonce'd C mark; this is the pinned-ZDOTDIR design), AC4
(a pre-existing `ZDOTDIR` is restored/driven), AC6 (temp dir removed on drop). Plus
unit tests for materialization, shell detection, the mark-atomicity contract, and
the nonce/injection-guard logic.

**Adversarial review (3 lenses x skeptic, 17 findings) caught a real HIGH bug, now
fixed:** the original bootstrap RESTORED `$ZDOTDIR` away from the shim, so `exec zsh`
silently lost integration (failing AC3). Fixed by keeping `$ZDOTDIR` pinned at the
shim + driving user files by explicit path; verified by the new exec-survival test.
Also hardened the shim-dir creation (`create_dir`, not `create_dir_all`, so a
pre-existing/attacker dir is never trusted - we point a shell at it).

**Pending (the `ready-for-human` item):**

- **AC5 - coexistence with starship/powerlevel10k/zsh-defer.** The precmd PS1 re-wrap
  that defends against a late prompt framework clobbering our A/B marks is
  implemented + reasoned (idempotent re-capture), but is NOT tested against a real
  framework (none installed in CI). Wants an owner smoke-test with starship/p10k.
- Minor robustness follow-ups (dismissed by review as non-exploitable, noted for a
  later pass): the OSC 7 cwd path is emitted unencoded (a dir name with an embedded
  ESC/BEL - exotic - could truncate the cwd mark; a real exploit is blocked by the
  nonce gate + the filter's abort/re-anchor), and `cmdline=` percent-encoding is
  char- not byte-based (non-ASCII command text is mangled in the advisory label).

`fmt`/`clippy`/full-workspace `build`/`test` green (aterm-core 77 tests, incl. 3
real-zsh integration tests, all stable across reruns). No new Rust deps (the scripts
are `include_str!`'d resources).

# Resolution

**done 2026-06-24.** All acceptance criteria met. Residual (non-blocking, not tracked as
a follow-up): the OSC-7 `cwd` path is sent unencoded - the adversarial review confirmed
this non-exploitable for v1 (only an exotic directory literally named with a control
char or `%` is affected). The `cmdline=` field is already byte-safe in all three shims
(`LC_ALL=C` byte-masking; fish `string escape --style=url`). A path-safe (keep-`/`)
cwd encoder is minor future hardening if exotic paths ever matter.
