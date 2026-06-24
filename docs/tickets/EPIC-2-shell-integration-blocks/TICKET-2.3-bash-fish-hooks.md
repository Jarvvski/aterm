---
id: T-2.3
epic: EPIC-2-shell-integration-blocks
title: bash + fish hooks (version-branched)
status: done
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

# Notes

**Landed 2026-06-24** (jj, not pushed). bash + fish now emit the same nonce-stamped
OSC-133 A/B/C/D + OSC 7 contract as zsh, consumed uniformly by T-2.1. Code in
`crates/aterm-core/src/shell_integration.rs` (+ `resources/integration.bash`,
`resources/bash-bootstrap.bash`, `resources/integration.fish`) and the engine branch
in `crates/aterm-core/src/engine.rs`.

- **IntegrationDir** generalized to carry `kind` + precomputed env + spawn args;
  `install_bash`/`install_fish` join `install_zsh`; a shared `make_session_dir`
  (`create_dir` not `_all`, 0700) backs all three. The zsh path is behaviourally
  unchanged (verified by the existing zsh live tests).
- **bash** is launched NON-login with `--rcfile <bootstrap>`; the bootstrap
  reconstructs the login+interactive startup (`/etc/profile` -> the first personal
  login file -> `.bashrc` fallback) then sources our hooks last. Version-branched:
  `PS0='${ __aterm_preexec;}'` (current-shell command substitution) on bash >= 5.3, a
  minimal `DEBUG`-trap preexec emulation on 3.2 - 5.2. A/B are baked into PS1 as static
  literal `\e]...\a` escapes (no command substitution), so a prompt redraw never trips
  the DEBUG trap. cmdline= is captured from the history list only when the index
  actually advances (so a HISTCONTROL-skipped / history-off command emits C with no
  cmdline rather than a stale one). The command line is percent-encoded BYTE-wise
  (`LC_ALL=C` + a `& 0xFF` mask, because bash 3.2 sign-extends high bytes).
- **fish** injects via `XDG_DATA_DIRS` -> `fish/vendor_conf.d/aterm.fish`; the script
  removes its own dir from the var again. Hooks: `fish_prompt` (A + cwd), `fish_preexec`
  (B + C with `string escape --style=url` cmdline), `fish_postexec` (D with `$status`).
- **Engine** branches `spawn_login_shell` on `ShellKind`, spawns with the shim's
  `shell_args()`, arms the OSC scanner with the nonce, and exposes `shell_kind()` +
  `integration_active()`. An unrecognised shell (dash/nu/pwsh) runs raw with `-l` and
  reports `ShellKind::Other` (AC5).

**Adversarial review applied** (Workflow, this turn). 4 confirmed findings, all FIXED
and EMPIRICALLY VALIDATED against real bash 3.2.57 + 5.3.12 via a PTY (the new
`bash_shim_emits_nonce_marks_through_real_bash` + `assert_bash_lifecycle_behaviors`
tests; content-only string tests cannot catch these):
1. **HIGH - empty-Enter phantom block (DEBUG-trap tier).** The trap fired C+D on every
   empty Enter (its own `__aterm_precmd` tripped the still-armed gate; with a user
   `PROMPT_COMMAND`, that PROMPT_COMMAND's commands did too). Fixed by disarming the
   gate at the START of the prompt cycle (in `__aterm_precmd`) and re-arming only at
   its END (`__aterm_arm_prompt`), plus a name guard skipping our own hook functions.
   Confirmed empirically: empty Enter emits zero marks; the bash 5.3 PS0 path was
   already correct.
2. **MEDIUM - UTF-8 cmdline corruption.** Char-wise iteration encoded only the first
   byte of a multibyte char (cafe-acute -> `caf%E9`, garbled on decode). Fixed byte-wise
   (`LC_ALL=C` + `& 0xFF`); applied the same fix to the zsh shim, which shared the bug.
3. **MEDIUM + 4. LOW - stale cmdline under HISTCONTROL ignorespace / disabled history.**
   `history 1` returned the PREVIOUS command for a skipped/unrecorded line. Fixed by
   the history-index-advance check above (emit C with no cmdline rather than stale).

**Follow-ups for the human (why `ready-for-human`):**
1. **fish live test unrun here** (no fish on the dev/CI host). `fish_shim_emits_nonce_marks_through_real_fish`
   is skip-if-absent; it needs validation on a fish >= 3.2 host. The fish script is
   reasoned-correct (events, `string escape --style=url`, XDG cleanup) but not yet
   exercised end-to-end.
2. **AC2 "version detected AND reported".** The bash tier is detected in-shell
   (`BASH_VERSINFO`) and the correct mechanism is chosen per tier, but the bash VERSION
   is not surfaced to Rust - the engine reports `ShellKind` + `integration_active`, not
   the version. T-2.6 (the indicator) is where the "Heuristic" downgrade for bash 3.2
   lives; it will need the version surfaced (e.g. a tier attribute on the first mark, or
   a `bash --version` probe at spawn). Flagged for the T-2.6 design.
3. **fish D uses `$status`, not `$pipestatus`** (AC3 wording). Functionally identical
   for the single positional exit code on `D;<code>`; carrying full `$pipestatus` as a
   non-standard extra attribute is the optional enhancement from the research, deferred.
4. **DEBUG-trap tier caveats** (documented in `integration.bash`): it is the
   least-reliable tier (drives T-2.6's "Heuristic" label); a user rc that later installs
   its own `DEBUG` trap would override ours. The broader on-host shell matrix is T-7.4.

# Resolution

**done 2026-06-24.** AC2 "version detected AND reported" resolved: each shim now emits
`aterm_ver=<version>` on the first prompt's `A` mark (bash `$BASH_VERSION`, zsh
`$ZSH_VERSION`, fish `$version`), the nonce-gated OSC scanner parses it into
`Mark::ShellVersion`, and `Engine::shell_version()` surfaces it for the T-2.6 indicator.
The fish live end-to-end test stays skip-if-absent (validated where fish >= 3.2 is
present); the shim is exercised by the real-shell tests on hosts that have it.
