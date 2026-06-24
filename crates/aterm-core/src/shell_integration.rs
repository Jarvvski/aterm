//! Shell-integration shim extraction (ticket T-2.2).
//!
//! aterm injects a tiny zsh snippet that emits nonce-stamped OSC-133 marks
//! (A/B/C/D) and OSC 7 (cwd) around the prompt and command, so [`crate::osc`] can
//! segment blocks reliably regardless of the user's prompt theme. The injection is
//! zero dotfile-edits: at spawn we materialize a per-session `ZDOTDIR` dir (a `.zshenv`
//! bootstrap + the integration script) and point the child's `$ZDOTDIR` at it; the
//! bootstrap restores the real `ZDOTDIR`, re-sources the user's startup files, and
//! installs our integration last. The temp dir is removed when [`IntegrationDir`]
//! drops.
//!
//! The shim sets a per-session [`ShimNonce`] into every mark, which the OSC filter
//! ([`crate::osc::OscScanner::with_nonce`]) requires - so a foreign program's marks
//! (or an attacker echoing OSC-133) are dropped. The scripts emit each mark with a
//! single `printf`, so the nonce is never written detached from its `ESC ]`
//! introducer (the T-2.1 contract). Only zsh is handled here; bash/fish are T-2.3.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The embedded zsh integration script (the marks logic). `__ATERM_NONCE__` is
/// substituted per session.
const INTEGRATION_ZSH: &str = include_str!("resources/integration.zsh");
/// The embedded `.zshenv` bootstrap. `__ATERM_INTEGRATION_PATH__` is substituted
/// with the absolute path of the materialized integration script.
const ZSHENV_BOOTSTRAP: &str = include_str!("resources/zshenv");

/// A per-session nonce stamped into every mark the shim emits, so the OSC scanner
/// trusts *our* marks and ignores a foreign terminal's integration (or an attacker
/// echoing OSC-133). This is a disambiguation/anti-spoof token, NOT a cryptographic
/// secret; it just must be unguessable-enough and `[A-Za-z0-9]+`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimNonce(pub String);

impl ShimNonce {
    /// Generate a fresh nonce from process + time entropy (no external dep).
    pub fn generate() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = u128::from(std::process::id());
        // Two hex words of mixed entropy -> 32 chars of [0-9A-F], well within
        // [A-Za-z0-9]+ and long enough to not collide across sessions.
        Self(format!(
            "{:016X}{:016X}",
            nanos,
            pid.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ))
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
    /// Classify by the program path (`$SHELL` or the child's argv0).
    pub fn from_path(path: &str) -> Self {
        let base = path.rsplit('/').next().unwrap_or(path);
        // Strip a leading login-shell '-' (e.g. "-zsh").
        let base = base.strip_prefix('-').unwrap_or(base);
        match base {
            "zsh" => ShellKind::Zsh,
            "bash" => ShellKind::Bash,
            "fish" => ShellKind::Fish,
            _ => ShellKind::Other,
        }
    }
}

/// The zsh integration script for `nonce` (the marks logic, loaded last).
#[must_use]
pub fn zsh_integration_script(nonce: &ShimNonce) -> String {
    INTEGRATION_ZSH.replace("__ATERM_NONCE__", &nonce.0)
}

/// Whether to inject the zsh shim given the user's original `$ZDOTDIR`.
///
/// The standard case (no custom `ZDOTDIR`, so zsh reads `$HOME`) always injects.
/// When the user HAS a custom `ZDOTDIR` we inject only if it actually looks like a
/// zsh config dir (carries at least one of `.zshenv/.zprofile/.zshrc/.zlogin`) -
/// the footgun guard from the research, so we never hijack an unusual/system
/// `ZDOTDIR` we do not understand.
#[must_use]
pub fn should_inject_zsh(orig_zdotdir: Option<&str>) -> bool {
    match orig_zdotdir {
        None => true,
        Some(dir) => ["/.zshenv", "/.zprofile", "/.zshrc", "/.zlogin"]
            .iter()
            .any(|f| Path::new(&format!("{dir}{f}")).exists()),
    }
}

/// A materialized per-session zsh `ZDOTDIR` shim directory. Holds the temp dir +
/// the user's original `$ZDOTDIR`; [`Drop`] removes the temp dir.
#[derive(Debug)]
pub struct IntegrationDir {
    dir: PathBuf,
    orig_zdotdir: Option<String>,
}

impl IntegrationDir {
    /// Materialize a zsh `ZDOTDIR` shim for `nonce` into a fresh per-session temp
    /// dir: a `.zshenv` bootstrap + the integration script. `orig_zdotdir` is the
    /// user's pre-existing `$ZDOTDIR` (`None` = the default `$HOME`), preserved so
    /// the bootstrap can restore it.
    pub fn install_zsh(nonce: &ShimNonce, orig_zdotdir: Option<String>) -> io::Result<Self> {
        let dir =
            std::env::temp_dir().join(format!("aterm-zdotdir-{}-{}", std::process::id(), nonce.0));
        // `create_dir` (NOT `create_dir_all`) so a pre-existing dir is an error,
        // never silently trusted: we point a SHELL at this dir, so we must own it.
        // The 32-char nonce makes a collision/pre-seed practically impossible; this
        // closes the window regardless (an attacker would have to win the race AND
        // guess the nonce). 0700 follows immediately.
        fs::create_dir(&dir)?;
        // Best-effort 0700: the shim dir holds only our scripts, but it should not
        // be world-writable (another user could otherwise inject into our zsh).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        }

        let integration_path = dir.join("aterm-integration.zsh");
        fs::write(&integration_path, zsh_integration_script(nonce))?;

        let zshenv = ZSHENV_BOOTSTRAP.replace(
            "__ATERM_INTEGRATION_PATH__",
            &integration_path.to_string_lossy(),
        );
        fs::write(dir.join(".zshenv"), zshenv)?;

        Ok(Self { dir, orig_zdotdir })
    }

    /// The shim dir (the value to set as the child's `$ZDOTDIR`).
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// The env vars to set on the spawned zsh: `ZDOTDIR` -> the shim dir (kept
    /// pinned there for the whole session so the integration survives `exec zsh`),
    /// and (when the user had one) `ATERM_REAL_ZDOTDIR` -> their original config dir,
    /// which the bootstrap drives the user's startup files from by explicit path.
    #[must_use]
    pub fn env_vars(&self) -> Vec<(String, String)> {
        let mut v = vec![(
            "ZDOTDIR".to_string(),
            self.dir.to_string_lossy().into_owned(),
        )];
        if let Some(orig) = &self.orig_zdotdir {
            v.push(("ATERM_REAL_ZDOTDIR".to_string(), orig.clone()));
        }
        v
    }
}

impl Drop for IntegrationDir {
    fn drop(&mut self) {
        // Best-effort: the dir holds only our two scripts; ignore errors (e.g. the
        // temp root was already swept).
        let _ = fs::remove_dir_all(&self.dir);
    }
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
        // A login shell's argv0 is prefixed with '-'.
        assert_eq!(ShellKind::from_path("-zsh"), ShellKind::Zsh);
    }

    #[test]
    fn nonce_is_stable_within_instance_and_alphanumeric() {
        let n = ShimNonce::generate();
        assert_eq!(n, n.clone());
        assert!(!n.0.is_empty());
        assert!(
            n.0.chars().all(|c| c.is_ascii_alphanumeric()),
            "nonce must be [A-Za-z0-9]+, got {:?}",
            n.0
        );
    }

    #[test]
    fn integration_script_carries_nonce_marks_and_osc7() {
        let nonce = ShimNonce("ABC123".into());
        let s = zsh_integration_script(&nonce);
        assert!(s.contains("ABC123"), "nonce substituted");
        assert!(!s.contains("__ATERM_NONCE__"), "placeholder fully replaced");
        // OSC-133 A/B/C/D + OSC 7 + the precmd/preexec hooks + idempotency guard.
        assert!(s.contains("133;C") || s.contains("133;%s") || s.contains("__aterm_mark"));
        assert!(s.contains("]7;file://"), "emits OSC 7 cwd");
        assert!(s.contains("add-zsh-hook precmd"));
        assert!(s.contains("add-zsh-hook preexec"));
        assert!(
            s.contains("ATERM_INTEGRATION_LOADED"),
            "idempotency guard present"
        );
        assert!(s.contains("cmdline="), "C carries the encoded command line");
    }

    #[test]
    fn marks_are_emitted_atomically_with_their_introducer() {
        // T-2.1 contract: the nonce must NEVER be emitted as bytes detached from an
        // `ESC ]` introducer. Every printf that writes the nonce must, in the same
        // format string, contain the `\033]` introducer. (We check the raw template
        // so a future edit that splits a mark across two printfs is caught.)
        let nonce = ShimNonce("NONCE99".into());
        let s = zsh_integration_script(&nonce);
        for line in s.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("printf") && line.contains("aterm_nonce=") {
                assert!(
                    line.contains("\\033]"),
                    "a printf emitting the nonce must include the ESC] introducer \
                     in the same call (atomicity); offending line: {line}"
                );
            }
        }
    }

    #[test]
    fn install_zsh_materializes_dir_and_cleans_up_on_drop() {
        let nonce = ShimNonce("TESTNONCE".into());
        let path = {
            let shim = IntegrationDir::install_zsh(&nonce, Some("/home/user".into()))
                .expect("materialize zsh shim");
            let dir = shim.path().to_path_buf();
            assert!(dir.join(".zshenv").is_file(), ".zshenv written");
            assert!(
                dir.join("aterm-integration.zsh").is_file(),
                "integration script written"
            );
            // .zshenv references the absolute integration path (placeholder gone).
            let zshenv = fs::read_to_string(dir.join(".zshenv")).unwrap();
            assert!(!zshenv.contains("__ATERM_INTEGRATION_PATH__"));
            assert!(zshenv.contains(
                &dir.join("aterm-integration.zsh")
                    .to_string_lossy()
                    .into_owned()
            ));
            // env_vars carry ZDOTDIR -> shim dir + the original to restore.
            let env = shim.env_vars();
            assert!(env
                .iter()
                .any(|(k, v)| k == "ZDOTDIR" && v == &dir.to_string_lossy()));
            assert!(env
                .iter()
                .any(|(k, v)| k == "ATERM_REAL_ZDOTDIR" && v == "/home/user"));
            dir
        };
        // Dropped -> temp dir removed.
        assert!(
            !path.exists(),
            "shim dir must be removed when IntegrationDir drops"
        );
    }

    #[test]
    fn no_orig_zdotdir_omits_the_real_dir_var() {
        let nonce = ShimNonce("N".into());
        let shim = IntegrationDir::install_zsh(&nonce, None).expect("materialize");
        let env = shim.env_vars();
        assert!(env.iter().any(|(k, _)| k == "ZDOTDIR"));
        assert!(
            !env.iter().any(|(k, _)| k == "ATERM_REAL_ZDOTDIR"),
            "with no original ZDOTDIR the bootstrap defaults to $HOME"
        );
    }

    #[test]
    fn should_inject_default_home_but_guards_custom_zdotdir() {
        // Default (no custom ZDOTDIR) always injects.
        assert!(should_inject_zsh(None));
        // A custom ZDOTDIR with no zsh startup files is NOT hijacked.
        let empty = std::env::temp_dir().join(format!("aterm-empty-{}", std::process::id()));
        let _ = fs::create_dir_all(&empty);
        assert!(!should_inject_zsh(Some(&empty.to_string_lossy())));
        // ... but one that has a .zshrc is a real config dir -> inject.
        fs::write(empty.join(".zshrc"), "# user rc\n").unwrap();
        assert!(should_inject_zsh(Some(&empty.to_string_lossy())));
        let _ = fs::remove_dir_all(&empty);
    }
}
