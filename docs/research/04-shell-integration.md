---
title: Shell Integration: Semantic Prompts Without Touching Dotfiles
domain: shell-integration
status: research
---

# Shell Integration: Semantic Prompts Without Touching Dotfiles

## TL;DR

- The industry-standard way to get command boundaries, cwd, and exit codes is the **FinalTerm OSC 133 protocol** (semantic marks A/B/C/D) plus **OSC 7** (cwd as a `file://host/path` URL). Both are universally adopted (iTerm2, kitty, WezTerm, Ghostty, VS Code, fish, foot). aterm should consume OSC 133 + OSC 7 as its primary marker lifecycle and treat VS Code's richer **OSC 633** as an optional ingest path. [1][2][3]
- **Inject hooks without editing dotfiles via launcher-time env-var indirection**, one technique per shell: zsh = `ZDOTDIR` shim, bash = `--rcfile`/`ENV` bootstrap, fish = `XDG_DATA_DIRS` vendor-conf injection. This is exactly what kitty does and it is the most battle-tested no-dotfile approach. The prior aterm prototype already had a correct, idempotent zsh `ZDOTDIR` shim; reuse its design. [4][5]
- **The prior prototype's "silent zsh-only degradation" is the thing to fix.** Recommendation: detect the shell at spawn from `$SHELL` + the resolved argv0 of the PTY child, attempt integration for zsh/bash/fish, and surface a **visible, three-state status indicator** (Integrated / Heuristic fallback / Unknown). Never fail silently.
- **bash is the sharp edge.** macOS ships bash 3.2 (GPLv2, frozen 2007); modern bash is 5.x. Reliable preexec/precmd on bash < 5.3 requires the `bash-preexec` DEBUG-trap shim with its known caveats; bash 5.3 (2025) makes it trivial via `PS0='${ preexec;}'` + `PROMPT_COMMAND`. aterm must bundle a `bash-preexec`-style shim and branch on version. [6][7][8]
- **Alt-screen / nested-shell hygiene is mandatory.** A full-screen app (vim, less, tmux, fzf) or a nested un-integrated shell can emit or fail to emit marks and fabricate phantom blocks. aterm must (a) suppress block creation while the alt screen (DECSET 1049) is active, and (b) per-session tag marks with a nonce so a nested shell's marks are not folded into the outer session. The prototype's parser already carries a `tag=` nonce field for this; wire it up. [9]
- **Detection over heuristics, but keep a heuristic fallback.** When integration is confirmed (we see our own nonce'd A/B/C/D), use marks exclusively - no prompt-regex guessing. When unavailable, fall back to a clearly-labeled heuristic mode (newline + cursor-at-col-0 prompt detection) rather than presenting a broken block UI.

## Findings

### 1. The marker protocols

#### OSC 133 (FinalTerm / iTerm2 "semantic prompt")

The de-facto standard. Four marks, each an OSC with command number `133`, terminated by either `BEL` (`\a`, `0x07`) or `ST` (`ESC \`, `\x1b\x5c`). Terminals accept both terminators. [1][3]

| Mark | Sequence | Meaning |
|------|----------|---------|
| A (`FTCS_PROMPT`) | `OSC 133 ; A ST` | Sent **just before** the shell prints its prompt. |
| B (`FTCS_COMMAND_START`) | `OSC 133 ; B ST` | Sent **just after** the prompt, before user input. Boundary between prompt text and typed command. |
| C (`FTCS_COMMAND_EXECUTED`) | `OSC 133 ; C ST` | Sent **just before** command output starts (i.e. command was accepted and is running). |
| D (`FTCS_COMMAND_FINISHED`) | `OSC 133 ; D [; <code>] ST` | Command finished; optional exit code `0..255`. A `D` sent right after `C`/`B` with no output signals an aborted/empty command. |

Extensions seen in the wild and worth supporting on ingest:
- `OSC 133 ; A ; <key>=<value> ...` - iTerm2 and others attach key=value attributes to `A` (e.g. `aid=` app id, `cl=` click-to-move semantics). Tolerate and ignore unknown keys.
- `cmdline=` on `C` - non-standard but useful; some shells carry the percent/escaped command text so the terminal can label the block without scraping the screen. The prior prototype emits this from `preexec` (percent-encoded). [3]

Adopters: iTerm2 (origin), kitty, WezTerm, Ghostty, Windows Terminal, VS Code, fish (built-in), foot, tmux (passthrough + prompt-jump). [1][2][3][9]

#### OSC 7 (current working directory)

`OSC 7 ; file://<hostname><abs-path> ST`. Originated in Apple Terminal. The path is percent-encoded; the host lets the terminal distinguish a local cwd from a remote/SSH one and ignore the latter. Universally supported (iTerm2, kitty, WezTerm, Ghostty, VS Code, Terminal.app). [3][10]

The prior prototype emits `printf '\033]7;file://%s%s\007' "${HOST}" "${PWD}"` and parses host + percent-decoded path - correct. One caveat: `$HOST` in zsh is the short hostname; OSC 7 nominally wants the FQDN, but for the local-vs-remote decision the short name is fine and matching it against `$(hostname)` is the standard local test.

#### OSC 1337 (iTerm2 proprietary, reference only)

iTerm2 also offers `OSC 1337 ; CurrentDir=<path> ST`, `OSC 1337 ; RemoteHost=<user>@<fqdn> ST`, `OSC 1337 ; SetUserVar=<name>=<base64> ST`, and `OSC 1337 ; ShellIntegrationVersion=<n> ; <shell> ST`. These are redundant with OSC 7 + OSC 133 for our needs; ingest the `ShellIntegrationVersion` if present (useful telemetry on which integration loaded) but do not depend on 1337. [3]

#### OSC 633 (VS Code) - optional ingest path

VS Code's superset. Same A/B/C/D skeleton plus: [2]

| Seq | Meaning |
|-----|---------|
| `OSC 633 ; A ST` | Prompt start. |
| `OSC 633 ; B ST` | Prompt end / command start. |
| `OSC 633 ; C ST` | Pre-execution (output start). |
| `OSC 633 ; D [; <exit>] ST` | Execution finished, optional exit code. |
| `OSC 633 ; E ; <cmdline> [; <nonce>] ST` | **Explicit command line**, with a per-session **nonce** to prevent spoofing. Escaping: backslash-hex `\xAB`; mandatory escapes for `;` (`\x3b`), and all bytes `<= 0x20` (esp. newline `\x0a`); literal backslash is `\\`. |
| `OSC 633 ; P ; <Key>=<Value> ST` | Properties: `Cwd=...`, `IsWindows=...`, `HasRichCommandDetection=...`. |

The `E` nonce design is the right model for trustworthy command text and is worth adopting in aterm's own OSC 133 `cmdline=` field (carry an aterm-session nonce, reject `cmdline=`/marks whose nonce does not match). aterm need not natively support 633 - but consuming it lets aterm "just work" when a user's prompt framework (oh-my-posh, starship) is already emitting 633. Map 633;A->A, B->B, C->C, D->D, E->`cmdline`, P;Cwd->OSC7-equivalent. [2]

### 2. No-dotfile injection per shell (the "how", not the "what")

The principle: **never modify the user's rc files. Instead, manipulate the environment the shell starts in so the shell itself loads our bootstrap, which then chain-sources the user's real config and installs our hooks last** (so frameworks that reset `PS1`/`fish_prompt` after us cannot drop our marks).

#### zsh - `ZDOTDIR` shim (the prototype's approach; keep it)

zsh reads startup files from `$ZDOTDIR` (default `$HOME`), in order: `.zshenv` -> [`.zprofile` if login] -> `.zshrc` if interactive -> [`.zlogin` if login]. kitty's mechanism: set `ZDOTDIR` to a kitty-owned dir containing a `.zshenv` that (1) restores the original `ZDOTDIR`, (2) sources the user's real `.zshenv`, then lets zsh continue normally through the user's other files, and (3) at the end installs the integration. "No files are added or modified" to the user's tree. [4]

The prior aterm prototype instead **sources the integration last** (after the user's `.zshrc`) and re-wraps `PS1` from a `precmd` hook every prompt, which defends against starship/powerlevel10k/zsh-defer clobbering `PS1`. That is actually more robust than kitty's once-at-load wrapping for adversarial prompt frameworks. The two key prototype techniques to keep verbatim:
- `PS1=$'%{\033]133;A\007%}'"${PS1}"$'%{\033]133;B\007%}'` - wrap the prompt in zero-width (`%{...%}`) A and B marks so cursor math stays correct.
- `add-zsh-hook precmd`/`preexec` for the C (preexec) and D (precmd, reading `$?` first) marks, with a `__aterm_ran` guard so the first precmd (stale `$?`) does not fabricate a D.

Recommended hybrid: use the `ZDOTDIR` shim for **loading** (zero dotfile edits, survives `exec zsh`), and keep the prototype's **precmd re-wrap of PS1** for robustness against late prompt mutation. Known footgun: if the user already has a global/system `ZDOTDIR`, naive injection can break; preserve and restore it, and only inject when at least one of `.zshenv/.zprofile/.zshrc/.zlogin` exists in the original `ZDOTDIR`. [11]

#### bash - `--rcfile` / `ENV` bootstrap

bash's startup is messier than zsh's:
- An **interactive login** shell reads `/etc/profile` then the first of `~/.bash_profile`, `~/.bash_login`, `~/.profile`.
- An **interactive non-login** shell reads `~/.bashrc`.
- `bash --rcfile <file>` replaces `~/.bashrc` for non-login interactive shells; `bash --posix` with `ENV=<file>` is used for POSIX-mode bootstrap.

kitty launches bash with `ENV` pointing at its bootstrap and runs in a mode that suppresses the normal startup files, then the bootstrap **manually re-sources the user's startup files in the correct order** and installs integration. The known limitation: `--rcfile` disables `/etc/profile` processing, so kitty sequences startup programmatically inside the `ENV` script to preserve system-wide config. [4]

Hook installation inside bash:
- **bash >= 5.3 (Aug 2025):** trivial and clean - `PS0='${ preexec;}'` (the new `${ ...;}` command-substitution-in-current-shell runs `preexec` just before the command executes, no subshell) and `PROMPT_COMMAND='precmd'` for the post-command/precmd hook. `BASH_MONOSECONDS` is available for timing. [7]
- **bash 4.0 - 5.2:** use the **`bash-preexec`** library (`rcaloras/bash-preexec`), which emulates zsh `preexec`/`precmd` via the `DEBUG` trap + `PROMPT_COMMAND`. Caveats to design around: it **requires** exclusive use of the `DEBUG` trap and `PROMPT_COMMAND` (if the user's rc later overrides either, integration breaks); the `DEBUG` trap fires before every simple command, loop body, function call, etc., so the shim filters; subshell support is **off by default** due to functrace bugs. [6][8]
- **bash 3.2 (macOS system bash):** GPLv2-era, no `PS0`. `bash-preexec` still works (DEBUG trap exists) but is the least reliable tier. Detect this and consider downgrading the status indicator to "Heuristic" if marks look unreliable. Most macOS power users run a Homebrew bash 5.x; detect the actual running version, not the path. [6]

#### fish - `XDG_DATA_DIRS` vendor injection (cleanest of the three)

fish auto-loads any `*.fish` in `vendor_conf.d` directories found under `$XDG_DATA_DIRS`. kitty prepends its integration dir to `XDG_DATA_DIRS`, fish auto-sources it, and the script then cleans up the env var. "No files are added or modified." [4] fish has **built-in OSC 133 support** and natively sends the command line with the `C`/output-start mark, so on modern fish the work is minimal. fish hooks: the `fish_prompt` event (fires each prompt -> A/B), `fish_preexec`/`fish_postexec` functions (C and D), and `$status` + `$pipestatus` for the exit code (captured immediately at prompt time). Minimum: fish 3.2.0 for vendor injection; Warp requires fish >= 3.6 for subshell warpify. [4][5][12]

#### Summary table

| Shell | No-dotfile load mechanism | Hook for A/B (prompt) | Hook for C (preexec) | Hook for D (exit) | Min version |
|-------|---------------------------|-----------------------|----------------------|-------------------|-------------|
| zsh | `ZDOTDIR` shim -> source last | wrap `PS1` (zero-width `%{...%}`), re-wrap from `precmd` | `preexec` hook | `precmd` hook reads `$?` | 5.0 |
| bash >= 5.3 | `ENV`/`--rcfile` bootstrap | `PS0` prefix mark, `PROMPT_COMMAND` for A | `PS0='${ preexec;}'` | `PROMPT_COMMAND` reads `$?` | 5.3 trivial |
| bash 4.0-5.2 | same | `bash-preexec` `precmd_functions` | `bash-preexec` `preexec_functions` | `precmd` reads `$?`/`PIPESTATUS` | 4.0 (via shim) |
| bash 3.2 (macOS) | same | `bash-preexec` (least reliable) | same | same | works, degrade label |
| fish | `XDG_DATA_DIRS` vendor_conf.d | `fish_prompt` event | `fish_preexec` | `fish_postexec` + `$status`/`$pipestatus` | 3.2.0 |

### 3. How the incumbents do it

- **Warp ("Warpify"):** Warp removes the shell prompt entirely and uses its own non-shell editor, then feeds the assembled command to the hidden shell and uses **shell callbacks/hooks in zsh, bash, fish** to know when a command starts/ends and to cut blocks. For **subshells** (nested local shell, docker exec, SSH, poetry shell) it watches the *command being run* against a configurable allowlist (`bash`, `fish`, `zsh`, `docker exec`, `gcloud compute ssh`, `eb ssh`, `poetry shell`) and offers to "Warpify" it. Warpify bootstraps by injecting a small marker/hook script into the subshell's environment; for SSH it generates a one-line snippet the user appends to the **remote** rc once. fish must be >= 3.6 for subshell warpify. Env vars Warp sets include session markers like `WARP_IS_LOCAL_SHELL_SESSION` and an honor-PS1 toggle. [5][13]
- **kitty:** auto-injects via the env-var indirection per shell described above; configurable via `shell_integration` with keywords incl. `no-rc-edit` (do not modify launch env) and `no-cursor`/`no-title`. For SSH, the `kitten ssh` wrapper copies the integration to the remote and sets it up there. Default-on for zsh/bash/fish. [4][14]
- **iTerm2:** the **origin** of OSC 133/1337. Historically required the user to run a curl-piped installer that appends `source ~/.iterm2_shell_integration.<shell>` to their rc (dotfile edit) - now offers an auto-inject mode similar to kitty. iTerm2 documents the canonical OSC 133/1337 sequences aterm should mirror. [3]
- **WezTerm:** ships `wezterm.sh` (bash/zsh) and a fish file; **auto-activates** for bash/zsh on its Fedora/Debian/Arch packages by dropping the script into the system profile.d. The script emits OSC 7 (preferring `wezterm set-working-directory` when the `wezterm` CLI is on PATH) and OSC 133. Detection is "is the `wezterm` binary available". [15][16]
- **VS Code:** auto-injects without dotfile edits by setting env (`VSCODE_SHELL_INTEGRATION`, detected via `TERM_PROGRAM=vscode`) when launching supported shells, toggled by `terminal.integrated.shellIntegration.enabled`; emits the richer OSC 633 with the spoof-resistant nonce on `E`. [2]

### 4. Shell detection (replacing silent degradation)

The complaint to fix: the prototype assumed zsh and silently did nothing else. Robust detection:

1. **Resolve the launch shell.** Prefer the user's configured shell (`$SHELL` / `getpwuid` login shell) for the spawn decision, but verify against the **actual argv0 / resolved exe of the PTY child** (read `/proc`-equivalent on macOS via `proc_pidpath`, or just trust the spawn since aterm controls argv0). Map basename -> {zsh, bash, fish, other}.
2. **Branch the injection** per the table above. For an unknown/unsupported shell (dash, ksh, nu, elvish, pwsh) do **not** inject - run it raw and set status = "Unknown / no integration".
3. **Confirm integration is live.** After spawn, watch for the first nonce-matched OSC 133;A within a short window (e.g. the first prompt). If seen -> status = "Integrated". If the shell is supported but no marks arrive -> status = "Degraded - heuristic" and enable the regex/heuristic block detector. If unsupported -> "Unknown".
4. **Surface it.** A small, always-visible indicator (per the prototype's iA aesthetic, a single glyph + tooltip in the block gutter or status line): Integrated / Heuristic / None, with a one-click "why?" explaining what is missing (e.g. "running fish 3.1 - upgrade to 3.2+ for native blocks"). Never silent.

### 5. The marker lifecycle aterm consumes to build blocks

This is the contract between the shell hooks and aterm's block model. Per command cycle, in stream order:

```
OSC 7  ; file://host/cwd            -> set the cwd for the upcoming block
OSC 133; A [;attrs] [;tag=NONCE]    -> PROMPT_START: open a new "prompt region"
   <prompt text streams>
OSC 133; B [;tag=NONCE]             -> COMMAND_START: prompt done; following bytes are the typed command
   <command echo streams>           (or, in aterm's controlled-UI model, aterm injects the command itself)
OSC 133; C [;cmdline=ENC] [;tag=NONCE] -> OUTPUT_START: command accepted & running; cmdline= carries the
                                          authoritative command text (preferred over screen-scraping)
   <command stdout/stderr streams>  -> this is the block's OUTPUT body
OSC 133; D ; <exit> [;tag=NONCE]    -> COMMAND_FINISHED: close the block, attach exit code; color/badge by status
OSC 7  ; file://host/newcwd         -> (from precmd) cwd for the *next* block; also detects `cd` side-effects
```

State machine aterm should run (mirroring the prototype's offset-tagged parser, which strips marks to zero-width and fires events once the emulator has drained to the mark's offset - this keeps marks in lockstep with the grid):

- **Idle** -> on `A`: begin block, record cwd from last OSC 7, start capturing prompt region.
- **Prompt** -> on `B`: split prompt vs command; in aterm's Warp-style controlled UI the command comes from aterm's own input box, so `B`..`C` echo can be suppressed/collapsed.
- **Command** -> on `C`: mark output-start; if `cmdline=` present and nonce matches, set the block's command text authoritatively (do not trust screen scrape).
- **Output** -> on `D`: finalize block with exit code; render success/failure affordance; return to **Idle**.

Hardening rules baked into the lifecycle:
- **Nonce gating.** Every mark carries `tag=NONCE` set per aterm session (random, `[A-Za-z0-9]+`). Marks whose nonce is absent/mismatched come from a **nested un-integrated shell or a hostile program** - do not let them mutate the outer block; optionally open a child sub-session. The prototype's parser already extracts `tag=` defensively; emit and enforce it.
- **Alt-screen suppression.** While the alt screen is active (`CSI ? 1049 h` set, cleared by `1049 l`), suppress block creation - a TUI (vim, less, htop, fzf, tmux) may emit stray `133;*`. This must be decided **at fire time**, not parse time, because the toggling CSI may still be unprocessed passthrough when the parser runs (the prototype documents exactly this). [9]
- **Missing-D recovery.** If an `A` arrives while a block is still Output-open (no `D` seen - e.g. Ctrl-C, shell crash, SSH drop), auto-close the previous block with exit=unknown and start fresh. Never let blocks nest implicitly.
- **Empty-command collapse.** `A`->`B`->`A` (user hit Enter on empty prompt) or `C`->`D` with no output: collapse to a thin marker, don't render an empty card.
- **Exit code source.** Prefer the explicit `D;<code>`. For pipelines aterm may also want `$pipestatus`/`PIPESTATUS`; if desired, carry it as an extra attribute on `D` (non-standard, nonce-gated) rather than overloading the positional code.

## Recommendations for aterm

1. **Consume OSC 133 (A/B/C/D) + OSC 7 as the canonical lifecycle; ingest OSC 633 and OSC 1337 opportunistically.** Rationale: 133+7 is the universal floor every shell can emit; 633/1337 give free wins when a user's prompt framework already speaks them. **Confidence: High.**
2. **Reuse the prototype's zsh shim design, upgraded to a `ZDOTDIR` loader.** Keep the zero-width `%{...%}` PS1 wrap, the `precmd` re-wrap (defends against starship/p10k/zsh-defer), the percent-encoded `cmdline=`, and the `__aterm_ran` first-precmd guard. Load it via a `ZDOTDIR` shim (zero dotfile edits, survives `exec zsh`) rather than appending to `.zshrc`. **Confidence: High.**
3. **Ship full bash + fish support from day one, version-branched.** bash: `ENV`/`--rcfile` bootstrap that re-sources user files in correct order; install hooks via `PS0`+`PROMPT_COMMAND` on bash >= 5.3 else bundle `bash-preexec` (MIT) for 4.0-5.2 (and 3.2 with a degraded label). fish: `XDG_DATA_DIRS` vendor_conf.d injection + `fish_prompt`/`fish_preexec`/`fish_postexec` + `$pipestatus`. Rationale: directly fixes the prototype's #1 complaint (zsh-only). **Confidence: High.**
4. **Implement the three-state visible status indicator (Integrated / Heuristic / None) with a "why".** Confirm "Integrated" only after seeing a nonce-matched `133;A`. Never degrade silently. **Confidence: High.**
5. **Make every mark nonce-tagged (`tag=NONCE`) and enforce it at the consumer.** Protects against nested-shell mark folding and program spoofing; mirrors VS Code's `633;E` nonce. **Confidence: High.**
6. **Suppress block creation on the alt screen and decide it at fire-time.** Use the prototype's offset-tagged, parse-then-fire architecture so the alt-screen flag is read against the drained emulator state. **Confidence: High.**
7. **Keep a labeled heuristic fallback (prompt-at-col-0 + newline) for unsupported/unintegrated shells.** Better a clearly-marked approximate block UI than a blank one. **Confidence: Med.**
8. **For SSH/Docker subshells, adopt Warp's allowlist-prompt model later, not v1.** Watch the launched command against a configurable allowlist and offer to bootstrap; for SSH, generate a remote rc one-liner the user opts into (the only acceptable "edit", and it's the *remote* box, with consent). **Confidence: Med.**
9. **Bundle the integration scripts as embedded resources, written to a per-session temp `ZDOTDIR`/bootstrap dir at spawn**, idempotent (`ATERM_INTEGRATION_LOADED` guard), and cleaned up on exit. **Confidence: High.**

## Risks & unknowns

- **bash on macOS (3.2) reliability.** Could not benchmark `bash-preexec` DEBUG-trap overhead or mark reliability on 3.2 specifically; the DEBUG-trap approach is documented as fragile (fires on every simple command, conflicts with user DEBUG traps, subshells off by default). Mitigation: detect version, prefer Homebrew bash 5.x, degrade the indicator. [6][8]
- **`exec` into a new shell / `su` / `sudo -i`.** `ZDOTDIR`/`ENV`/`XDG_DATA_DIRS` may or may not survive `exec`, `su`, or login shells that reset the environment. `ZDOTDIR` survives `exec zsh`; `ENV` is unset by bash login shells; `XDG_DATA_DIRS` may be sanitized by `sudo`. Each needs a real test matrix - unverified here.
- **Prompt frameworks that re-emit their own OSC 133.** starship, oh-my-posh, p10k can emit 133 themselves; double marks could create phantom blocks. The `precmd` re-wrap guard (`[[ "$PS1" == *$'\033]133;A'* ]] && return`) handles PS1, but a framework emitting marks from its *own* precmd (not via PS1) could double-fire. Needs testing against starship/p10k/oh-my-posh. Nonce gating helps only if we can distinguish - their marks won't carry our nonce, so we can actually drop them. Good.
- **OSC 7 host/FQDN mismatch.** Short vs fully-qualified hostname for the local-vs-remote test is heuristic; an SSH'd host with the same short name as local could be misclassified. Low impact but real.
- **tmux passthrough.** Inside tmux, marks must be wrapped in tmux's passthrough (`\ePtmux;...`) or tmux must have `allow-passthrough on` / its own OSC 133 handling; tmux has documented EL0/clear-line quirks that drop prompt marks. aterm-inside-tmux is an edge case but users will do it. [9][17]
- **bash 5.3 `${ ...;}` syntax** is new (2025); confirmed in one source [7] but not cross-checked against the official bash 5.3 release notes here - verify before relying on `PS0='${ preexec;}'`.
- **Warp internal env-var names** (`WARP_IS_LOCAL_SHELL_SESSION`, honor-PS1 toggle) are from a search summary, not a primary doc fetch (the docs page returned HTTP 429). Treat as indicative, not exact. [5][13]

## Open questions for the product owner

1. **Minimum shell version floor?** Do we hard-support macOS system bash 3.2 (degraded), or require Homebrew bash 5.x / refuse integration below some version? Affects how much `bash-preexec` complexity we carry.
2. **SSH / subshell integration in v1 or later?** Warp-style remote warpify is significant scope. Confirm it's out of the v1 cut (recommended) and just shows "remote - no integration" honestly.
3. **Heuristic fallback at all, or honest "no blocks" mode?** Do we want an approximate prompt-regex block UI for un-integrated shells, or do we present a plain raw-terminal view with a clear "integration unavailable" banner? (Recommendation leans toward the labeled heuristic, but it's a product call.)
4. **Controlled-UI command source vs shell echo.** In the Warp model aterm owns the input box and feeds the command to the shell - do we therefore *suppress* the B..C echo region entirely and rely on `cmdline=`/`633;E`, or render the shell's own echo? Affects how aggressively we trust the nonce'd cmdline.
5. **nu / pwsh / elvish?** Any commitment to non-POSIX shells, or explicitly "Unknown - no integration"?

## Sources

1. [OSC 133 (shell integration / semantic prompt) support - tmux/tmux#3064](https://github.com/tmux/tmux/issues/3064)
2. [Terminal Shell Integration - Visual Studio Code docs (OSC 633)](https://code.visualstudio.com/docs/terminal/shell-integration)
3. [Proprietary Escape Codes - iTerm2 documentation (OSC 133/7/1337)](https://iterm2.com/documentation-escape-codes.html)
4. [Shell integration - kitty (ZDOTDIR/ENV/XDG_DATA_DIRS injection, no-rc-edit)](https://sw.kovidgoyal.net/kitty/shell-integration/)
5. [Warpify subshells - Warp docs](https://docs.warp.dev/terminal/warpify/subshells/)
6. [rcaloras/bash-preexec - preexec and precmd for bash via DEBUG trap](https://github.com/rcaloras/bash-preexec)
7. [Preexec hooks are finally trivial in bash 5.3 - posix.nexus](https://posix.nexus/posts/native-bash-preexec/)
8. [DEBUG trap and PROMPT_COMMAND in Bash - Chuan Ji](https://jichu4n.com/posts/debug-trap-and-prompt_command-in-bash/)
9. [Overly clearing OSC133 flags / alt-screen handling - tmux/tmux#4918](https://github.com/tmux/tmux/issues/4918)
10. [Operating System Commands (OSC) - Terminfo.dev (OSC 7)](https://terminfo.dev/osc)
11. [Shell integration stops working if ZDOTDIR is changed - kovidgoyal/kitty#6330](https://github.com/kovidgoyal/kitty/issues/6330)
12. [status - query fish runtime information (fish $status/$pipestatus)](https://fishshell.com/docs/current/cmds/status.html)
13. [Warp supports subshells with modern IDE, blocks, and autocompletions - Warp blog](https://www.warp.dev/blog/warp-supports-subshells)
14. [kitten ssh - kitty (remote shell integration over SSH)](https://sw.kovidgoyal.net/kitty/kittens/ssh/)
15. [Shell Integration - WezTerm docs (OSC 7 / OSC 133)](https://wezterm.org/shell-integration.html)
16. [wezterm.sh shell integration script - wezterm/wezterm](https://github.com/wezterm/wezterm/blob/main/assets/shell-integration/wezterm.sh)
17. [Tmux Jump between Prompt Output with OSC 133 - tanut aran (Medium)](https://tanutaran.medium.com/tmux-jump-between-prompt-output-with-osc-133-shell-integration-standard-84241b2defb5)
