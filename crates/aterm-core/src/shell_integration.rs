//! Shell-integration shim extraction.
//!
//! aterm injects a tiny shell snippet that emits nonce-stamped OSC-133 marks
//! around the prompt and command, so [`crate::osc`] can segment blocks reliably
//! regardless of the user's prompt theme. The shim is bundled in the binary and
//! written to a temp path the spawned shell sources via `$SHELL`-specific hooks.
//!
//! Only the zsh shim is filled in here (macOS default). TODO(ticket EPIC-2):
//! bash/fish shims, and the actual sourcing handshake (writing the file +
//! exporting the rc hook) wired into [`crate::pty`] spawn.

/// A per-session nonce stamped into every mark the shim emits. Lets the OSC
/// scanner trust *our* marks and ignore a foreign terminal's integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimNonce(pub String);

impl ShimNonce {
    /// Generate a fresh nonce from process + time entropy (no external dep).
    /// This is an obfuscation/disambiguation token, NOT a security boundary.
    pub fn generate() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id() as u128;
        Self(format!("{:016X}", nanos ^ (pid << 64)))
    }
}

/// The supported login shells we know how to inject a shim for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Zsh,
    Bash,
    Fish,
    /// Unknown shell: no shim, fall back to heuristic segmentation.
    Other,
}

impl ShellKind {
    /// Classify by the program path (`$SHELL`).
    pub fn from_path(path: &str) -> Self {
        let base = path.rsplit('/').next().unwrap_or(path);
        match base {
            "zsh" => ShellKind::Zsh,
            "bash" => ShellKind::Bash,
            "fish" => ShellKind::Fish,
            _ => ShellKind::Other,
        }
    }
}

/// Produce the zsh shim text that emits nonce-stamped OSC-133 marks. Returns
/// `None` for shells we don't have a shim for yet.
///
/// The marks follow the OSC-133 convention used by [`crate::osc`]:
///   A = prompt start, B = command start, C = pre-exec, D[;exit] = command done.
pub fn shim_for(shell: ShellKind, nonce: &ShimNonce) -> Option<String> {
    match shell {
        ShellKind::Zsh => Some(zsh_shim(&nonce.0)),
        // TODO(ticket EPIC-2): bash (PROMPT_COMMAND + trap DEBUG) and fish
        // (fish_prompt / fish_preexec hooks).
        _ => None,
    }
}

fn zsh_shim(nonce: &str) -> String {
    // ESC ] ... BEL marks. `%{...%}` keeps zsh's prompt-width accounting correct.
    // Built without a format string to avoid `{`/`%{` escaping noise.
    let template = r#"# aterm shell integration (zsh) — nonce __ATERM_NONCE__
__aterm_nonce="__ATERM_NONCE__"
__aterm_osc() { printf '\033]133;%s;aterm_nonce=%s\007' "$1" "$__aterm_nonce"; }
__aterm_precmd() { __aterm_osc D";$?"; __aterm_osc A; }
__aterm_preexec() { __aterm_osc C; }
autoload -Uz add-zsh-hook
add-zsh-hook precmd __aterm_precmd
add-zsh-hook preexec __aterm_preexec
PROMPT="%{$(__aterm_osc B)%}$PROMPT"
"#;
    template.replace("__ATERM_NONCE__", nonce)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_shell_by_basename() {
        assert_eq!(ShellKind::from_path("/bin/zsh"), ShellKind::Zsh);
        assert_eq!(ShellKind::from_path("/usr/local/bin/bash"), ShellKind::Bash);
        assert_eq!(
            ShellKind::from_path("/opt/homebrew/bin/fish"),
            ShellKind::Fish
        );
        assert_eq!(ShellKind::from_path("/bin/sh"), ShellKind::Other);
    }

    #[test]
    fn nonce_is_stable_within_instance() {
        let n = ShimNonce::generate();
        assert_eq!(n, n.clone());
        assert!(!n.0.is_empty());
    }

    #[test]
    fn zsh_shim_carries_nonce_and_marks() {
        let nonce = ShimNonce("ABC123".into());
        let shim = shim_for(ShellKind::Zsh, &nonce).unwrap();
        assert!(shim.contains("ABC123"));
        assert!(shim.contains("133"));
        assert!(shim.contains("add-zsh-hook precmd"));
    }

    #[test]
    fn no_shim_for_unknown_shell() {
        assert!(shim_for(ShellKind::Other, &ShimNonce("x".into())).is_none());
    }
}
