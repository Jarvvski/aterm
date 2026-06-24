# aterm shell integration (bash). Sourced LAST by the aterm bash bootstrap (see
# bash-bootstrap.bash), after the user's startup files. Emits nonce-stamped OSC-133
# semantic marks (A/B/C/D) + OSC 7 (cwd) so aterm can segment command blocks
# reliably regardless of the prompt theme. __ATERM_NONCE__ is substituted per
# session.
#
# Version-branched (the tiers from research 04-shell-integration.md section 2):
#   - bash >= 5.3: PS0='${ ...;}' runs preexec in the CURRENT shell (no subshell),
#     so the C mark + the ran-flag side effect both take. PROMPT_COMMAND drives D.
#   - bash 3.2 - 5.2 (incl. macOS system bash): a minimal DEBUG-trap preexec
#     emulation (the technique pioneered by rcaloras/bash-preexec, MIT) tailored to
#     our two dynamic marks. This is the least-reliable tier (the DEBUG trap fires
#     before every simple command; user DEBUG traps or PROMPT_COMMAND rewrites can
#     break it) and drives the "Heuristic" downgrade in the T-2.6 indicator.
#
# SECURITY (ticket T-2.1 contract): every DYNAMIC mark (C/D) is emitted by a SINGLE
# printf, so the `ESC ] 133 ; ... ; aterm_nonce=NONCE BEL` introducer and the nonce
# are always written together. The STATIC A/B marks are baked into PS1 as literal
# prompt escapes (\e ... \a) wrapped in zero-width \[...\] - again introducer and
# nonce inseparable, and with NO command substitution in PS1 so prompt redraw never
# fires the DEBUG trap.

# Idempotency: never install twice (survives re-source, nesting).
[ -n "$ATERM_INTEGRATION_LOADED" ] && return 0
ATERM_INTEGRATION_LOADED=1

__aterm_nonce="__ATERM_NONCE__"

# Static A/B marks for PS1: zero-width (\[...\]) so bash's line-length accounting
# stays correct; \e -> ESC and \a -> BEL are decoded by bash at prompt-display time.
# No `$(...)` here, so redrawing the prompt never triggers the DEBUG trap below.
__aterm_a='\[\e]133;A;aterm_nonce='"$__aterm_nonce"'\a\]'
__aterm_b='\[\e]133;B;aterm_nonce='"$__aterm_nonce"'\a\]'

# Emit OSC 7 (cwd as file://host/path), atomically.
__aterm_cwd() { printf '\033]7;file://%s%s\007' "${HOSTNAME:-localhost}" "$PWD"; }

# Percent-encode a command line for cmdline=: keep the RFC-3986 unreserved set,
# encode everything else (crucially ';' and controls, so the command text can never
# break out of the OSC). `local LC_ALL=C` forces a BYTE view, so a multibyte UTF-8
# character is encoded as its constituent bytes (cafe-acute -> caf%C3%A9, matching
# the fish/zsh shims and what osc.rs percent_decode round-trips) rather than the
# value of only its first byte.
__aterm_encode() {
  local s=$1 out= c i n LC_ALL=C
  for (( i = 0; i < ${#s}; i++ )); do
    c=${s:i:1}
    case $c in
      [A-Za-z0-9._~-]) out+=$c ;;
      # Mask to a byte: bash 3.2 `printf "'<byte>"` sign-extends bytes >= 0x80
      # (0xC3 -> -61), so format `n & 0xFF` to get the true 0x00-0xFF value.
      *) printf -v n '%d' "'$c"; out+=$(printf '%%%02X' "$(( n & 0xFF ))") ;;
    esac
  done
  printf '%s' "$out"
}

# The just-entered command line, captured into __aterm_cmd - but ONLY when the
# history list actually advanced this prompt. A command skipped by HISTCONTROL
# (ignorespace/ignoredups), or a shell with history disabled, leaves the index
# unchanged: we then leave __aterm_cmd empty (C is emitted with NO cmdline=) rather
# than re-reporting the STALE previous line. Called directly (never via $()), so the
# __aterm_last_hist / __aterm_cmd writes persist in the current shell.
__aterm_last_hist=""
__aterm_cmd=""
__aterm_capture_command() {
  local h idx
  h=$(HISTTIMEFORMAT= builtin history 1 2>/dev/null)
  h=${h#"${h%%[![:space:]]*}"} # left-trim
  idx=${h%%[![:digit:]]*}      # leading integer history index
  if [ -z "$idx" ] || [ "$idx" = "$__aterm_last_hist" ]; then
    __aterm_cmd=""
    return 0
  fi
  __aterm_last_hist=$idx
  h=${h#"$idx"}                          # drop the index
  __aterm_cmd=${h#"${h%%[![:space:]]*}"} # left-trim to the command text
}

# `$?` on the FIRST precmd is stale (no command has run yet); this guard stops us
# fabricating a bogus D before the first command.
__aterm_ran=0

# preexec: a command is about to run -> C (output start) carrying the encoded command
# line when we have it. One printf (atomic nonce). Sets __aterm_ran so precmd emits D.
__aterm_preexec() {
  __aterm_capture_command
  if [ -n "$__aterm_cmd" ]; then
    printf '\033]133;C;aterm_nonce=%s;cmdline=%s\007' "$__aterm_nonce" "$(__aterm_encode "$__aterm_cmd")"
  else
    printf '\033]133;C;aterm_nonce=%s\007' "$__aterm_nonce"
  fi
  __aterm_ran=1
}

# precmd: a prompt is about to be drawn. Read $? FIRST, emit D for the command that
# just finished, refresh cwd, then (re)wrap PS1 with the A/B marks if a framework
# (starship/p10k/etc.) replaced it since we last wrapped. The nonce check keeps the
# re-wrap idempotent so PS1 never grows unbounded.
__aterm_precmd() {
  local __aterm_ec=$?
  if [ "$__aterm_ran" = 1 ]; then
    printf '\033]133;D;%s;aterm_nonce=%s\007' "$__aterm_ec" "$__aterm_nonce"
  fi
  __aterm_ran=0
  # Disarm the DEBUG-trap preexec gate at the START of the prompt cycle (used only by
  # the bash 3.2 - 5.2 tier; a harmless no-op assignment otherwise). The trap fires
  # for every command in PROMPT_COMMAND too, so disarming here - then re-arming in
  # __aterm_arm_prompt AFTER the whole chain - means neither our own precmd nor the
  # user's PROMPT_COMMAND commands can masquerade as the user's command.
  __aterm_at_prompt=0
  __aterm_cwd
  if [[ "$PS1" != *"aterm_nonce=$__aterm_nonce"* ]]; then
    __aterm_user_ps1=$PS1
    PS1="${__aterm_a}${__aterm_user_ps1}${__aterm_b}"
  fi
}

# Install precmd via PROMPT_COMMAND, FIRST in the chain so it captures the command's
# real $? before any user PROMPT_COMMAND code runs. Idempotent.
case ";${PROMPT_COMMAND};" in
  *";__aterm_precmd;"* | *";__aterm_precmd ;"*) ;;
  *)
    if [ -n "$PROMPT_COMMAND" ]; then
      PROMPT_COMMAND="__aterm_precmd;${PROMPT_COMMAND}"
    else
      PROMPT_COMMAND="__aterm_precmd"
    fi
    ;;
esac

if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 3) )); then
  # bash >= 5.3: ${ cmd;} runs in the CURRENT shell, so PS0 drives preexec directly
  # (no subshell) and the __aterm_cmd / __aterm_ran side effects persist. PS0 is
  # expanded right after a (non-empty) command is read, before it executes - never on
  # an empty Enter, so no empty-command guard is needed in this tier.
  PS0='${ __aterm_preexec;}'
else
  # bash 3.2 - 5.2 (incl. macOS system bash): emulate preexec with a DEBUG trap. The
  # trap fires before every simple command - including each command in PROMPT_COMMAND
  # - so we gate on __aterm_at_prompt, which is disarmed at the START of the prompt
  # cycle (in __aterm_precmd) and re-armed only at its END (here), AFTER the whole
  # PROMPT_COMMAND chain. We additionally skip our own two hook functions by name, so
  # the very first trap fire of a cycle (which is __aterm_precmd, while the gate is
  # still armed from the prior prompt) cannot fire a phantom C on an empty Enter.
  __aterm_at_prompt=0
  __aterm_preexec_invoke() {
    [ "${BASH_SUBSHELL:-0}" = 0 ] || return 0
    [ -z "$COMP_LINE" ] || return 0
    case "$BASH_COMMAND" in
      __aterm_precmd | __aterm_arm_prompt) return 0 ;;
    esac
    [ "$__aterm_at_prompt" = 1 ] || return 0
    __aterm_at_prompt=0
    __aterm_preexec
  }
  trap '__aterm_preexec_invoke' DEBUG
  __aterm_arm_prompt() { __aterm_at_prompt=1; }
  PROMPT_COMMAND="${PROMPT_COMMAND};__aterm_arm_prompt"
fi
