//! Shell-integration shim extraction (tickets T-2.2 zsh, T-2.3 bash + fish).
//!
//! aterm injects a tiny per-shell snippet that emits nonce-stamped OSC-133 marks
//! (A/B/C/D) and OSC 7 (cwd) around the prompt and command, so [`crate::osc`] can
//! segment blocks reliably regardless of the user's prompt theme. The injection is
//! zero dotfile-edits, with a per-shell load mechanism (research 04 section 2):
//!
//! - **zsh** ([`IntegrationDir::install_zsh`]): a per-session `ZDOTDIR` dir (a
//!   `.zshenv` bootstrap + the integration script); the child's `$ZDOTDIR` points at
//!   it; the bootstrap drives the user's startup files by explicit path, then loads
//!   our integration last (and re-pins `ZDOTDIR` so it survives `exec zsh`).
//! - **bash** ([`IntegrationDir::install_bash`]): launched non-login with
//!   `--rcfile <bootstrap>`; the bootstrap reconstructs the login+interactive startup
//!   sequence (preserving `/etc/profile`) then loads our integration last. The
//!   integration version-branches: `PS0` + `PROMPT_COMMAND` on bash >= 5.3, a minimal
//!   `DEBUG`-trap preexec emulation on 3.2 - 5.2.
//! - **fish** ([`IntegrationDir::install_fish`]): our dir is prepended to
//!   `XDG_DATA_DIRS`; fish auto-sources `fish/vendor_conf.d/*.fish`; the script
//!   cleans the env var back up. Hooks use the `fish_prompt`/`fish_preexec`/
//!   `fish_postexec` events.
//!
//! In all cases the per-session temp dir is removed when [`IntegrationDir`] drops.
//!
//! The shim sets a per-session [`ShimNonce`] into every mark, which the OSC filter
//! ([`crate::osc::OscScanner::with_nonce`]) requires - so a foreign program's marks
//! (or an attacker echoing OSC-133) are dropped. The scripts emit each mark with a
//! single `printf` (or, for the static zsh/bash A/B marks, a single literal prompt
//! string), so the nonce is never written detached from its `ESC ]` introducer (the
//! T-2.1 contract).
//!
//! ## Environment survival across `exec` / `su` / `sudo`, and tmux (ticket T-7.4)
//!
//! The shim's reach depends on which env var carries it and whether the transition
//! resets the environment (research 04 Risk list). Behavior, honestly:
//!
//! - **`exec zsh`**: integration SURVIVES. The `.zshenv` bootstrap re-pins `$ZDOTDIR`
//!   at the shim dir (not the user's), so a re-exec'd zsh re-enters the bootstrap and
//!   re-sources the (idempotent) integration. Proved by the deterministic
//!   `zsh_bootstrap_repins_zdotdir_for_exec_zsh_survival` test + the live
//!   `exec_zsh_keeps_the_session_alive_and_integrated` smoke.
//! - **`exec bash` / `exec fish`**: integration is NOT preserved automatically - bash's
//!   `--rcfile` is a spawn arg (not inherited by an `exec bash`) and fish's
//!   `XDG_DATA_DIRS` entry is stripped back out by our own vendor script after loading.
//!   A re-exec'd bash/fish falls back to the confirm-window timeout -> `Heuristic`
//!   (honest degradation, no phantom blocks). Re-warpifying a re-exec'd non-zsh shell is
//!   deferred (needs a persistent env var like zsh's `ZDOTDIR`).
//! - **`su` / `sudo -i`**: a login shell under another user RESETS the environment -
//!   `ZDOTDIR` is typically cleared by the login/`su -`, and `sudo` sanitizes the env
//!   (its `env_reset` default) so `XDG_DATA_DIRS` and `ZDOTDIR` do not carry through.
//!   Integration therefore does NOT follow into a `su`/`sudo -i` shell in v1; the
//!   indicator honestly reports the inner shell as un-integrated (`Heuristic`/`None`)
//!   rather than pretending. This matches the dossier's stance (warpify-across-privilege
//!   is out of v1 scope); a future ticket could inject via the target user's environment.
//! - **tmux**: OSC-133 marks must ride tmux's passthrough (`\ePtmux;...`) or tmux must
//!   have `allow-passthrough on`; tmux also has documented EL0/clear-line quirks that
//!   drop prompt marks (tmux#3064/#4918). aterm does NOT wrap marks for tmux in v1, so a
//!   shell running INSIDE tmux inside aterm degrades to `Heuristic` (the marks are eaten
//!   by tmux, the confirm window elapses) - honest degradation, not a phantom-block mess.
//!   tmux passthrough support is a deferred edge case (users run aterm's own shell
//!   directly far more often than aterm-inside-tmux).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The embedded zsh integration script (the marks logic). `__ATERM_NONCE__` is
/// substituted per session.
const INTEGRATION_ZSH: &str = include_str!("resources/integration.zsh");
/// The embedded `.zshenv` bootstrap. `__ATERM_INTEGRATION_PATH__` is substituted
/// with the absolute path of the materialized integration script.
const ZSHENV_BOOTSTRAP: &str = include_str!("resources/zshenv");
/// The embedded bash integration script (version-branched marks logic).
/// `__ATERM_NONCE__` is substituted per session.
const INTEGRATION_BASH: &str = include_str!("resources/integration.bash");
/// The embedded bash `--rcfile` bootstrap. `__ATERM_INTEGRATION_PATH__` is
/// substituted with the absolute path of the materialized integration script.
const BASH_BOOTSTRAP: &str = include_str!("resources/bash-bootstrap.bash");
/// The embedded fish `vendor_conf.d` script. `__ATERM_NONCE__` and
/// `__ATERM_FISH_DATA_DIR__` are substituted per session.
const INTEGRATION_FISH: &str = include_str!("resources/integration.fish");

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

/// The bash integration script for `nonce` (version-branched marks logic).
#[must_use]
pub fn bash_integration_script(nonce: &ShimNonce) -> String {
    INTEGRATION_BASH.replace("__ATERM_NONCE__", &nonce.0)
}

/// The bash `--rcfile` bootstrap, pointing at `integration_path` (the absolute path
/// of the materialized [`bash_integration_script`]).
#[must_use]
pub fn bash_bootstrap_script(integration_path: &Path) -> String {
    BASH_BOOTSTRAP.replace(
        "__ATERM_INTEGRATION_PATH__",
        &integration_path.to_string_lossy(),
    )
}

/// The fish `vendor_conf.d` integration script for `nonce`. `data_dir` is the dir
/// aterm prepends to `XDG_DATA_DIRS`; the script removes it again so child processes
/// do not inherit the injection.
#[must_use]
pub fn fish_integration_script(nonce: &ShimNonce, data_dir: &Path) -> String {
    INTEGRATION_FISH
        .replace("__ATERM_NONCE__", &nonce.0)
        .replace("__ATERM_FISH_DATA_DIR__", &data_dir.to_string_lossy())
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

/// Create a fresh, owned, 0700 per-session temp dir named `<prefix>-<pid>-<nonce>`.
///
/// `create_dir` (NOT `create_dir_all`) so a pre-existing dir is an error, never
/// silently trusted: we point a SHELL at this dir, so we must own it. The 32-char
/// nonce makes a collision/pre-seed practically impossible; this closes the window
/// regardless (an attacker would have to win the race AND guess the nonce). The
/// 0700 (best-effort) keeps another user from injecting into our shell.
fn make_session_dir(prefix: &str, nonce: &ShimNonce) -> io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{}", std::process::id(), nonce.0));
    fs::create_dir(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    Ok(dir)
}

/// A materialized per-session shell-integration shim directory. Knows which
/// [`ShellKind`] it is for and carries the precomputed env vars + spawn args the
/// child shell needs to load it; [`Drop`] removes the temp dir.
#[derive(Debug)]
pub struct IntegrationDir {
    dir: PathBuf,
    kind: ShellKind,
    env: Vec<(String, String)>,
    args: Vec<String>,
}

impl IntegrationDir {
    /// Materialize a zsh `ZDOTDIR` shim for `nonce`: a `.zshenv` bootstrap + the
    /// integration script. `orig_zdotdir` is the user's pre-existing `$ZDOTDIR`
    /// (`None` = the default `$HOME`), preserved so the bootstrap can restore it.
    /// Spawn args: `-l` (login); env: `ZDOTDIR` -> the shim (kept pinned so the
    /// integration survives `exec zsh`) + `ATERM_REAL_ZDOTDIR` when one existed.
    pub fn install_zsh(nonce: &ShimNonce, orig_zdotdir: Option<String>) -> io::Result<Self> {
        let dir = make_session_dir("aterm-zdotdir", nonce)?;

        let integration_path = dir.join("aterm-integration.zsh");
        fs::write(&integration_path, zsh_integration_script(nonce))?;

        let zshenv = ZSHENV_BOOTSTRAP.replace(
            "__ATERM_INTEGRATION_PATH__",
            &integration_path.to_string_lossy(),
        );
        fs::write(dir.join(".zshenv"), zshenv)?;

        let mut env = vec![("ZDOTDIR".to_string(), dir.to_string_lossy().into_owned())];
        if let Some(orig) = orig_zdotdir {
            env.push(("ATERM_REAL_ZDOTDIR".to_string(), orig));
        }
        Ok(Self {
            dir,
            kind: ShellKind::Zsh,
            env,
            args: vec!["-l".to_string()],
        })
    }

    /// Materialize a bash shim for `nonce`: the integration script + a `--rcfile`
    /// bootstrap. bash is launched NON-login + interactive with `--rcfile <bootstrap>`
    /// (the bootstrap reconstructs the login startup it thereby skips, preserving
    /// `/etc/profile`). No env vars are needed - the bootstrap path is a spawn arg.
    pub fn install_bash(nonce: &ShimNonce) -> io::Result<Self> {
        let dir = make_session_dir("aterm-bash", nonce)?;

        let integration_path = dir.join("aterm-integration.bash");
        fs::write(&integration_path, bash_integration_script(nonce))?;

        let bootstrap_path = dir.join("aterm-bootstrap.bash");
        fs::write(&bootstrap_path, bash_bootstrap_script(&integration_path))?;

        let args = vec![
            "--rcfile".to_string(),
            bootstrap_path.to_string_lossy().into_owned(),
            "-i".to_string(),
        ];
        Ok(Self {
            dir,
            kind: ShellKind::Bash,
            env: Vec::new(),
            args,
        })
    }

    /// Materialize a fish shim for `nonce`: a `fish/vendor_conf.d/aterm.fish` script.
    /// fish is launched login + interactive; our dir is prepended to `XDG_DATA_DIRS`
    /// so fish auto-sources it (the script then removes the dir again).
    /// `orig_xdg_data_dirs` is the user's pre-existing `$XDG_DATA_DIRS` (`None` ->
    /// the XDG spec default `/usr/local/share:/usr/share`).
    pub fn install_fish(nonce: &ShimNonce, orig_xdg_data_dirs: Option<String>) -> io::Result<Self> {
        let dir = make_session_dir("aterm-fish-data", nonce)?;

        // fish searches `<XDG_DATA_DIR>/fish/vendor_conf.d/*.fish`.
        let conf_d = dir.join("fish").join("vendor_conf.d");
        fs::create_dir_all(&conf_d)?;
        fs::write(
            conf_d.join("aterm.fish"),
            fish_integration_script(nonce, &dir),
        )?;

        let rest = orig_xdg_data_dirs.unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
        let xdg = format!("{}:{}", dir.to_string_lossy(), rest);
        Ok(Self {
            dir,
            kind: ShellKind::Fish,
            env: vec![("XDG_DATA_DIRS".to_string(), xdg)],
            args: vec!["-l".to_string(), "-i".to_string()],
        })
    }

    /// The shim dir (also the value the zsh shim sets as `$ZDOTDIR`).
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Which shell this shim is for.
    #[must_use]
    pub fn kind(&self) -> ShellKind {
        self.kind
    }

    /// The env vars to set on the spawned shell (zsh: `ZDOTDIR` [+ real];
    /// fish: `XDG_DATA_DIRS`; bash: none).
    #[must_use]
    pub fn env_vars(&self) -> Vec<(String, String)> {
        self.env.clone()
    }

    /// The argv (after the program name) the shell must be spawned with to load the
    /// shim (zsh: `-l`; bash: `--rcfile <bootstrap> -i`; fish: `-l -i`).
    #[must_use]
    pub fn shell_args(&self) -> Vec<String> {
        self.args.clone()
    }
}

impl Drop for IntegrationDir {
    fn drop(&mut self) {
        // Best-effort: the dir holds only our own scripts; ignore errors (e.g. the
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
        assert!(
            s.contains("aterm_ver=") && s.contains("ZSH_VERSION"),
            "A reports the zsh version (ticket T-2.3 AC2)"
        );
    }

    #[test]
    fn zsh_bootstrap_repins_zdotdir_for_exec_zsh_survival() {
        // T-7.4 AC: `exec zsh` must preserve integration. The mechanism is the .zshenv
        // bootstrap RE-PINNING $ZDOTDIR back at the shim dir (after sourcing the user's
        // real config against their own dir), so a re-exec'd zsh re-enters this bootstrap
        // and re-sources the integration. A restored ZDOTDIR would silently lose
        // integration after exec (the kitty #6330 failure mode). This proves the
        // mechanism deterministically; the live `exec_zsh_*` engine test is the on-shell
        // smoke.
        let bootstrap = ZSHENV_BOOTSTRAP;
        assert!(
            bootstrap.contains("ATERM_REAL_ZDOTDIR"),
            "the bootstrap sources the user's real config dir"
        );
        assert!(
            bootstrap.contains("export ZDOTDIR=\"$__aterm_shim\""),
            "the bootstrap must re-pin ZDOTDIR at the shim so `exec zsh` re-integrates"
        );
        // The shim re-pin must be the LAST ZDOTDIR export (after the user-dir export) so
        // the session keeps the shim through any later re-exec.
        let last_shim = bootstrap.rfind("ZDOTDIR=\"$__aterm_shim\"");
        let last_real = bootstrap.rfind("ZDOTDIR=\"$__aterm_real\"");
        assert!(
            matches!((last_shim, last_real), (Some(s), Some(r)) if s > r),
            "the shim re-pin must come AFTER the user-dir export so the shim wins"
        );
        // The integration is idempotent, so a nested / re-exec'd zsh re-source is safe.
        let integ = zsh_integration_script(&ShimNonce("N".into()));
        assert!(
            integ.contains("ATERM_INTEGRATION_LOADED"),
            "integration must be idempotent across `exec zsh`"
        );
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

    // ---- bash (T-2.3) ----------------------------------------------------------

    #[test]
    fn bash_integration_script_carries_nonce_marks_and_both_version_tiers() {
        let nonce = ShimNonce("BASHABC123".into());
        let s = bash_integration_script(&nonce);
        assert!(s.contains("BASHABC123"), "nonce substituted");
        assert!(!s.contains("__ATERM_NONCE__"), "placeholder fully replaced");
        assert!(s.contains("]7;file://"), "emits OSC 7 cwd");
        assert!(s.contains("cmdline="), "C carries the encoded command line");
        assert!(
            s.contains("ATERM_INTEGRATION_LOADED"),
            "idempotency guard present"
        );
        assert!(
            s.contains("PROMPT_COMMAND"),
            "drives precmd via PROMPT_COMMAND"
        );
        // Both version-branched tiers are present (research 04 section 2).
        assert!(
            s.contains("BASH_VERSINFO"),
            "version-branched on BASH_VERSINFO"
        );
        assert!(
            s.contains("aterm_ver=") && s.contains("BASH_VERSION"),
            "A reports the bash version so the indicator can flag the 3.2 tier (T-2.3 AC2)"
        );
        assert!(s.contains("PS0="), "bash >= 5.3 tier installs a PS0 hook");
        assert!(
            s.contains("DEBUG"),
            "bash 3.2 - 5.2 tier installs a DEBUG-trap preexec emulation"
        );
    }

    #[test]
    fn bash_marks_are_atomic_with_their_introducer() {
        // T-2.1 contract: every line that emits a 133 mark carrying the nonce must
        // also carry the `ESC ]` introducer (printf `\033]` for C/D, or the `\e]`
        // prompt escape for the static A/B PS1 marks) - the nonce is never detached.
        let nonce = ShimNonce("BNONCE9".into());
        let s = bash_integration_script(&nonce);
        for line in s.lines() {
            if line.contains("]133;") && line.contains("aterm_nonce=") {
                assert!(
                    line.contains("\\033") || line.contains("\\e"),
                    "a mark line must include the ESC introducer with the nonce \
                     (atomicity); offending line: {line}"
                );
            }
        }
    }

    #[test]
    fn bash_bootstrap_reconstructs_startup_and_loads_integration_last() {
        let s = bash_bootstrap_script(Path::new("/shim/aterm-integration.bash"));
        // Preserves system + login startup the --rcfile launch would otherwise skip.
        assert!(s.contains("/etc/profile"), "preserves /etc/profile");
        assert!(s.contains(".bash_profile"));
        assert!(s.contains(".bashrc"));
        // Loads our integration LAST, by the substituted absolute path.
        assert!(!s.contains("__ATERM_INTEGRATION_PATH__"));
        assert!(s.contains("/shim/aterm-integration.bash"));
    }

    #[test]
    fn install_bash_materializes_rcfile_bootstrap_and_cleans_up_on_drop() {
        let nonce = ShimNonce("BASHINSTALL01".into());
        let path = {
            let shim = IntegrationDir::install_bash(&nonce).expect("materialize bash shim");
            assert_eq!(shim.kind(), ShellKind::Bash);
            let dir = shim.path().to_path_buf();
            let integ = dir.join("aterm-integration.bash");
            let boot = dir.join("aterm-bootstrap.bash");
            assert!(integ.is_file(), "integration script written");
            assert!(boot.is_file(), "bootstrap written");
            // The bootstrap points at the integration script's absolute path.
            let boot_src = fs::read_to_string(&boot).unwrap();
            assert!(boot_src.contains(&integ.to_string_lossy().into_owned()));
            // No env vars; launched non-login + interactive via --rcfile <bootstrap>.
            assert!(shim.env_vars().is_empty(), "bash needs no env vars");
            let args = shim.shell_args();
            assert_eq!(args.first().map(String::as_str), Some("--rcfile"));
            assert!(args
                .iter()
                .any(|a| a == &boot.to_string_lossy().into_owned()));
            assert!(args.iter().any(|a| a == "-i"));
            assert!(
                !args.iter().any(|a| a == "-l"),
                "bash is launched NON-login; the bootstrap reconstructs the login files"
            );
            dir
        };
        assert!(!path.exists(), "shim dir removed when IntegrationDir drops");
    }

    // ---- fish (T-2.3) ----------------------------------------------------------

    #[test]
    fn fish_integration_script_carries_nonce_marks_events_and_url_encoding() {
        let nonce = ShimNonce("FISHABC123".into());
        let s = fish_integration_script(&nonce, Path::new("/data/aterm-fish-xyz"));
        assert!(s.contains("FISHABC123"), "nonce substituted");
        assert!(!s.contains("__ATERM_NONCE__"), "nonce placeholder replaced");
        assert!(
            !s.contains("__ATERM_FISH_DATA_DIR__"),
            "data-dir placeholder replaced"
        );
        assert!(
            s.contains("/data/aterm-fish-xyz"),
            "data dir substituted for the XDG cleanup"
        );
        assert!(s.contains("]7;file://"), "emits OSC 7 cwd");
        assert!(
            s.contains("aterm_ver=") && s.contains("$version"),
            "A reports the fish version (ticket T-2.3 AC2)"
        );
        assert!(
            s.contains("--on-event fish_prompt"),
            "A on the prompt event"
        );
        assert!(
            s.contains("--on-event fish_preexec"),
            "C on the preexec event"
        );
        assert!(
            s.contains("--on-event fish_postexec"),
            "D on the postexec event"
        );
        assert!(
            s.contains("string escape --style=url"),
            "cmdline percent-encoded so it cannot break out of the OSC"
        );
        assert!(s.contains("ATERM_INTEGRATION_LOADED"), "idempotency guard");
    }

    #[test]
    fn fish_marks_are_atomic_with_their_introducer() {
        let nonce = ShimNonce("FNONCE7".into());
        let s = fish_integration_script(&nonce, Path::new("/d"));
        for line in s.lines() {
            if line.contains("]133;") && line.contains("aterm_nonce=") {
                assert!(
                    line.contains("\\033") || line.contains("\\e"),
                    "a mark line must include the ESC introducer with the nonce \
                     (atomicity); offending line: {line}"
                );
            }
        }
    }

    #[test]
    fn install_fish_materializes_vendor_conf_d_and_prepends_xdg() {
        let nonce = ShimNonce("FISHINSTALL01".into());
        let path = {
            let shim = IntegrationDir::install_fish(&nonce, Some("/opt/share".into()))
                .expect("materialize fish shim");
            assert_eq!(shim.kind(), ShellKind::Fish);
            let dir = shim.path().to_path_buf();
            // fish auto-sources `<XDG_DATA_DIR>/fish/vendor_conf.d/*.fish`.
            let script = dir.join("fish").join("vendor_conf.d").join("aterm.fish");
            assert!(script.is_file(), "vendor_conf.d/aterm.fish written");
            // XDG_DATA_DIRS prepends our dir, preserving the user's existing entries.
            let env = shim.env_vars();
            let (_, xdg) = env
                .iter()
                .find(|(k, _)| k == "XDG_DATA_DIRS")
                .expect("XDG_DATA_DIRS set");
            assert!(xdg.starts_with(&dir.to_string_lossy().into_owned()));
            assert!(xdg.ends_with(":/opt/share"));
            // fish is launched login + interactive.
            assert_eq!(shim.shell_args(), vec!["-l".to_string(), "-i".to_string()]);
            dir
        };
        assert!(!path.exists(), "shim dir removed when IntegrationDir drops");
    }

    #[test]
    fn install_fish_defaults_xdg_when_user_has_none() {
        let nonce = ShimNonce("FISHDEFXDG".into());
        let shim = IntegrationDir::install_fish(&nonce, None).expect("materialize");
        let env = shim.env_vars();
        let (_, xdg) = env
            .iter()
            .find(|(k, _)| k == "XDG_DATA_DIRS")
            .expect("XDG_DATA_DIRS set");
        assert!(
            xdg.ends_with(":/usr/local/share:/usr/share"),
            "falls back to the XDG spec default base dirs; got {xdg}"
        );
    }
}
