# aterm bash bootstrap. aterm launches bash as an interactive NON-login shell with
# `--rcfile <this>` (instead of `-l`), which makes bash read this file in place of
# ~/.bashrc and skip the login startup files. We then reconstruct the user's normal
# startup sequence by hand - preserving system-wide config - and install aterm's
# integration LAST so prompt frameworks loaded along the way cannot drop our marks.
# No user dotfiles are modified. __ATERM_INTEGRATION_PATH__ is substituted with the
# absolute path of the integration script in the same per-session dir.

# Only an interactive shell gets hooks (and only an interactive shell reads
# --rcfile at all); bail safely otherwise.
case $- in
  *i*) ;;
  *) return 0 2>/dev/null || exit 0 ;;
esac

# Reconstruct what a login+interactive bash would have read, in order. We were
# launched non-login, so /etc/profile and the personal login file were skipped;
# source them now, preserving system-wide config (the --rcfile / -l tradeoff noted
# in research 04-shell-integration.md section 2).
[ -r /etc/profile ] && . /etc/profile
__aterm_sourced_login=0
for __aterm_f in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
  if [ -r "$__aterm_f" ]; then
    . "$__aterm_f"
    __aterm_sourced_login=1
    break
  fi
done
# A personal login file normally chains ~/.bashrc itself. If the user had none,
# fall back to the interactive rc so their config still loads (AC: real config
# still loads).
if [ "$__aterm_sourced_login" = 0 ] && [ -r "$HOME/.bashrc" ]; then
  . "$HOME/.bashrc"
fi
unset __aterm_f __aterm_sourced_login

# Install aterm's integration LAST (after the user's .bashrc / prompt framework).
. '__ATERM_INTEGRATION_PATH__'
