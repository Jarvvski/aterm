# aterm shell integration (fish). aterm prepends its per-session data dir to
# XDG_DATA_DIRS; fish auto-sources this file from `<dir>/fish/vendor_conf.d/`. It
# emits nonce-stamped OSC-133 semantic marks (A/B/C/D) + OSC 7 (cwd) so aterm can
# segment command blocks regardless of the prompt theme. No user files are touched.
# __ATERM_NONCE__ and __ATERM_FISH_DATA_DIR__ are substituted per session.
#
# SECURITY (ticket T-2.1 contract): every mark is emitted by a SINGLE printf, so the
# `ESC ] 133 ; ... ; aterm_nonce=NONCE BEL` introducer and the nonce are always
# written together - the nonce is never emitted detached from its introducer. The
# command line is percent-encoded (string escape --style=url) so it can never break
# out of the OSC.

# Interactive only, and never install twice.
if status is-interactive
    and not set -q ATERM_INTEGRATION_LOADED
    set -gx ATERM_INTEGRATION_LOADED 1
    set -g __aterm_nonce __ATERM_NONCE__

    # Remove aterm's injected dir from XDG_DATA_DIRS so child processes don't inherit
    # it (it was only needed for THIS fish to auto-source vendor_conf.d). A nested
    # fish is therefore un-integrated - acceptable, and it avoids polluting every
    # program's data-dir lookup (kitty does the same).
    if set -q XDG_DATA_DIRS
        set -l __aterm_kept (string match -v -- '__ATERM_FISH_DATA_DIR__' (string split : -- "$XDG_DATA_DIRS"))
        if test (count $__aterm_kept) -gt 0
            set -gx XDG_DATA_DIRS (string join : -- $__aterm_kept)
        else
            set -e XDG_DATA_DIRS
        end
        set -e __aterm_kept
    end

    # Emit an OSC-133 mark atomically: introducer + body + nonce in one printf.
    function __aterm_mark
        printf '\033]133;%s;aterm_nonce=%s\007' $argv[1] $__aterm_nonce
    end

    # fish_prompt event: a prompt is about to be drawn -> refresh cwd (OSC 7) and
    # emit A (prompt start).
    function __aterm_prompt --on-event fish_prompt
        printf '\033]7;file://%s%s\007' $hostname "$PWD"
        __aterm_mark A
    end

    # fish_preexec event: the command was submitted and is about to run -> B
    # (command accepted) then C (output start) carrying the percent-encoded command
    # line. $argv[1] is the full command line fish passes to the event.
    function __aterm_preexec --on-event fish_preexec
        __aterm_mark B
        printf '\033]133;C;aterm_nonce=%s;cmdline=%s\007' $__aterm_nonce (string escape --style=url -- "$argv[1]")
    end

    # fish_postexec event: the command finished -> D with its exit status. Capture
    # $status FIRST, before any other command clobbers it.
    function __aterm_postexec --on-event fish_postexec
        set -l __aterm_ec $status
        printf '\033]133;D;%s;aterm_nonce=%s\007' $__aterm_ec $__aterm_nonce
    end
end
