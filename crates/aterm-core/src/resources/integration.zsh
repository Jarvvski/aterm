# aterm shell integration (zsh). Sourced LAST, after the user's startup files, by
# the ZDOTDIR bootstrap (see zshenv). Emits nonce-stamped OSC-133 semantic marks
# (A/B/C/D) + OSC 7 (cwd) so aterm can segment command blocks reliably regardless
# of the prompt theme. __ATERM_NONCE__ is substituted with the per-session nonce.
#
# SECURITY (ticket T-2.1 contract): every mark is emitted by a SINGLE printf, so
# the `ESC ] 133 ; ... ; aterm_nonce=NONCE BEL` introducer and the nonce are always
# written together - the nonce value is NEVER emitted as bytes detached from its
# introducer. aterm's OSC filter trusts only marks carrying this exact nonce.

# Idempotency: never install twice (survives `exec zsh`, double-source, nesting).
[ -n "$ATERM_INTEGRATION_LOADED" ] && return
ATERM_INTEGRATION_LOADED=1

__aterm_nonce="__ATERM_NONCE__"

# Emit an OSC-133 mark atomically: introducer + body + nonce in one printf. The `A`
# (prompt start) also carries the zsh version (ticket T-2.3 AC2) so aterm can name it.
__aterm_mark() {
  if [[ "$1" == A ]]; then
    printf '\033]133;A;aterm_ver=%s;aterm_nonce=%s\007' "$ZSH_VERSION" "$__aterm_nonce"
  else
    printf '\033]133;%s;aterm_nonce=%s\007' "$1" "$__aterm_nonce"
  fi
}

# Emit OSC 7 (cwd as file://host/path), atomically.
__aterm_cwd() { printf '\033]7;file://%s%s\007' "${HOST:-localhost}" "$PWD"; }

# Percent-encode a command line for the cmdline= field: keep the RFC-3986
# unreserved set, encode everything else (crucially ';' and controls, so the
# command text can never break out of the OSC). `local LC_ALL=C` forces a BYTE view,
# so a multibyte UTF-8 character is encoded as its constituent bytes (matching the
# bash/fish shims and what osc.rs percent_decode round-trips) rather than producing a
# value > 0xFF for the whole character.
__aterm_encode() {
  local s=$1 out= c i n LC_ALL=C
  for (( i = 1; i <= ${#s}; i++ )); do
    c=${s[i]}
    if [[ "$c" == [A-Za-z0-9._~-] ]]; then
      out+=$c
    else
      # Mask to a byte so a sign-extended high byte can never widen the %02X.
      printf -v n '%d' "'$c"
      out+=$(printf '%%%02X' "$(( n & 0xFF ))")
    fi
  done
  printf '%s' "$out"
}

# `$?` on the FIRST precmd is stale (no command has run yet); this guard stops us
# fabricating a bogus D mark before the first command.
__aterm_ran=0

# preexec: a command is about to run -> C (output start), carrying the encoded
# command line. Emitted as one printf (atomic nonce).
__aterm_preexec() {
  printf '\033]133;C;aterm_nonce=%s;cmdline=%s\007' "$__aterm_nonce" "$(__aterm_encode "$1")"
  __aterm_ran=1
}

# precmd: a prompt is about to be drawn. Emit D for the command that just finished
# (with its exit code), refresh the cwd, then (re)wrap PS1 with the A/B marks. The
# re-wrap defends against starship/powerlevel10k/zsh-defer replacing PS1 after us:
# if PS1 no longer carries our nonce a framework just reset it, so we re-capture it
# as the new base and re-wrap. This is idempotent - PS1 never grows unbounded.
__aterm_precmd() {
  local __aterm_ec=$?
  if [ "$__aterm_ran" = 1 ]; then
    printf '\033]133;D;%s;aterm_nonce=%s\007' "$__aterm_ec" "$__aterm_nonce"
  fi
  __aterm_ran=0
  __aterm_cwd
  if [[ "$PS1" != *"aterm_nonce=$__aterm_nonce"* ]]; then
    __aterm_user_ps1=$PS1
  fi
  # A = prompt start (zero-width %{...%} so zsh's width accounting is unaffected),
  # then the user's prompt, then B = command start.
  PS1="%{$(__aterm_mark A)%}${__aterm_user_ps1}%{$(__aterm_mark B)%}"
}

autoload -Uz add-zsh-hook
add-zsh-hook preexec __aterm_preexec
add-zsh-hook precmd __aterm_precmd
