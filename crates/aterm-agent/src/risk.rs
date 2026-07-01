//! Deterministic, code-side, prompt-injection-resistant risk gate. Ported
//! (near-verbatim) from the prototype `Risk.kt` + `CommandLineRisk.kt` +
//! `RiskGloss.kt`.
//!
//! INVARIANT: the command came from an LLM that may have read untrusted content,
//! so we NEVER trust a model's self-reported risk - we classify the parsed tokens
//! ourselves and OVER-APPROXIMATE toward danger. Reading the API key, Keychain, or
//! known credential paths is Dangerous (defends against the agent exfiltrating its
//! own key under prompt injection), as is invoking an interpreter with inline code
//! (`sh -c`, `python -c`, `node -e`), `eval`/`source`/`.`, a build tool, or
//! `find -exec` - that re-opens an arbitrary-execution channel the argv-no-shell
//! design otherwise closes - and dumping the environment (`env`/`printenv`), which
//! would leak the key when the dev-fallback env vars are set.
//!
//! The command is parsed ONCE into a [`ShellCommand`] (the zsh-aware word-split,
//! head resolution, and shell-grammar scan), then each rule below is a small check
//! reading those facts. The credential-path deny-set comes from the injected
//! [`Secrets`] - the SINGLE source shared with the [`crate::sanitizer::OutputSanitizer`],
//! borrowed not copied, so the two never drift (T-5.6).
//!
//! This is a best-effort token/grammar classifier, NOT a complete boundary - a
//! real OS-level sandbox (T-5.7) is the boundary.

use crate::command::ShellCommand;
use crate::secrets::Secrets;

/// Risk level. Ordered: `Safe < Caution < Dangerous`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk {
    Safe,
    Caution,
    Dangerous,
}

/// A machine-readable reason a command was escalated above `Safe`. The set of
/// reasons is what the gate exposes to the approval UI ("why is this risky?");
/// [`gloss_for`] renders each in plain English.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RiskReason {
    /// Deletes or overwrites files (`rm`, `dd`, `mkfs`, `shred`, ...).
    Destructive,
    /// Reaches out over the network (`curl`, `wget`, `ssh`, ...).
    Network,
    /// Runs with elevated privileges (`sudo`, `su`, `doas`, `chown`, `chmod 777`).
    Privilege,
    /// Installs or removes packages (a package manager + a mutating subcommand).
    PackageMutator,
    /// Reads or writes a secret file / dumps the environment / Keychain tool.
    SecretAccess,
    /// Redirects output to a file (`>`).
    RedirectOverwrite,
    /// Uses shell operators or expansion (metachar / quote / control byte /
    /// assignment prefix / equals-expansion / precommand modifier / leading `~` /
    /// history `^`).
    ShellChaining,
    /// Could spawn processes uncontrollably (the classic fork bomb).
    ForkBomb,
    /// Runs arbitrary code (interpreter with a script/inline-code, build tool,
    /// `eval`/`source`/`.`, `find -exec`, launcher like `xargs`).
    CodeExecution,
    /// Writes a file to disk (the editor's gated `aterm-write` helper head).
    FileWrite,
    /// Runs on a remote host over SSH (a [`RemoteContext`] was supplied).
    RemoteExecution,
    /// Calls an MCP server tool (T-6.1/T-6.2) whose local effects we cannot
    /// statically classify - it may run a command or write files on the machine
    /// (local stdio) or execute server-side (the remote connector). Over-
    /// approximated to a Caution baseline that can never auto-run.
    McpTool,
}

impl RiskReason {
    /// Reasons that on their own make a command [`Risk::Dangerous`]. The rest are
    /// a [`Risk::Caution`] baseline (`Network`, `PackageMutator`, `RedirectOverwrite`,
    /// `ShellChaining`, `FileWrite`, `RemoteExecution`). Mirrors the prototype's
    /// `DANGEROUS_REASONS`.
    fn is_dangerous(self) -> bool {
        matches!(
            self,
            RiskReason::Destructive
                | RiskReason::Privilege
                | RiskReason::SecretAccess
                | RiskReason::ForkBomb
                | RiskReason::CodeExecution
        )
    }

    /// Reasons meaning the command STRING would be interpreted by a real shell
    /// once the shell-injection sink injects it - so even at `Safe` level the
    /// policy must never auto-approve them. Mirrors `SHELL_ACTIVE_REASONS`.
    /// `RemoteExecution` is included as belt-and-suspenders (a remote command is
    /// already >= Caution, but the policy refuses on the reason directly so a
    /// future classifier change can't silently regress it into an auto-run).
    pub(crate) fn is_shell_active(self) -> bool {
        matches!(
            self,
            RiskReason::ShellChaining
                | RiskReason::RedirectOverwrite
                | RiskReason::ForkBomb
                | RiskReason::RemoteExecution
        )
    }
}

/// Outcome of classification: a level plus the reasons that drove it (insertion
/// order, deduplicated - the prototype's `LinkedHashSet`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskAssessment {
    pub level: Risk,
    pub reasons: Vec<RiskReason>,
}

impl RiskAssessment {
    /// A command that classified as plainly safe carries no reasons.
    pub fn is_safe(&self) -> bool {
        self.level == Risk::Safe && self.reasons.is_empty()
    }

    /// True iff any reason makes this command shell-active (used by the policy).
    pub(crate) fn is_shell_active(&self) -> bool {
        self.reasons.iter().any(|r| r.is_shell_active())
    }
}

/// The remote-session facts the gate needs to classify a command that runs on
/// another host over SSH (an `ssh host '<inner>'` one-shot). When supplied, the
/// model's *local* cwd is discarded and [`RemoteContext::remote_cwd`] (the dir the
/// command runs in ON THE REMOTE HOST, or `None` when unknown) resolves relative
/// path args - so a relative credential read is never checked against the wrong
/// (local) directory. A `RemoteContext` always forces [`RiskReason::RemoteExecution`]
/// (a Caution baseline that can never auto-run), and an unknown `remote_cwd`
/// over-approximates every relative-path argument / relative-path head to
/// [`RiskReason::SecretAccess`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteContext {
    pub host: String,
    pub remote_cwd: Option<String>,
}

impl RemoteContext {
    /// A remote context with an unknown remote cwd (the common one-shot case).
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            remote_cwd: None,
        }
    }

    /// A remote context whose remote cwd is known.
    pub fn with_cwd(host: impl Into<String>, remote_cwd: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            remote_cwd: Some(remote_cwd.into()),
        }
    }
}

/// The default deterministic classifier. Holds no state; the deny-set is borrowed
/// from the single [`Secrets`] source on each call (never a private copy), so the
/// gate and the sanitizer cannot drift.
#[derive(Debug, Clone, Default)]
pub struct DefaultRiskClassifier;

impl DefaultRiskClassifier {
    /// Classify a parsed `command` (argv tokens). `cwd` is the directory the
    /// command would run in (folded into the credential-path check). When `remote`
    /// is supplied the command runs on another host: the local `cwd` is DISCARDED
    /// for the remote cwd, the assessment always carries [`RiskReason::RemoteExecution`]
    /// (never auto-runs), and an unknown remote cwd over-approximates relative-path
    /// args to [`RiskReason::SecretAccess`].
    pub fn classify(
        &self,
        command: &[String],
        cwd: Option<&str>,
        remote: Option<&RemoteContext>,
        secrets: &Secrets,
    ) -> RiskAssessment {
        // For a remote command the model's cwd describes the LOCAL machine, not
        // the host the command runs on - resolving a relative path against it
        // could clear a remote credential read (or falsely flag a local one), so
        // discard it and use the remote cwd (None = unknown).
        let effective_cwd = match remote {
            Some(r) => r.remote_cwd.as_deref(),
            None => cwd,
        };
        let parsed = ShellCommand::parse(command, effective_cwd);
        if parsed.is_empty() {
            return RiskAssessment {
                level: Risk::Safe,
                reasons: Vec::new(),
            };
        }

        let mut reasons = ReasonSet::new();

        // A command on another host is a Caution baseline unconditionally: even a
        // plain remote `ls` runs on a machine the gate cannot see (different FS,
        // privileges, prod data), so it can never auto-run. A genuinely
        // destructive remote command still elevates to Dangerous from its own
        // reason below; RemoteExecution is deliberately NOT a dangerous reason.
        if let Some(r) = remote {
            reasons.add(RiskReason::RemoteExecution);
            // Unknown remote cwd: we cannot resolve relative paths against the
            // remote host, so over-approximate to SecretAccess (Dangerous). Two
            // cases: (a) any relative-path ARGUMENT could be a credential file (a
            // bare `credentials`, `vault-token`); (b) a relative-PATH HEAD
            // (`./deploy.sh`, `bin/tool`) is an unknown executable on the remote
            // box. An absolute `/...` or `~...` path still flows through the
            // normal credential-path check below; a bare command-name head (`ls`)
            // is a $PATH lookup, not a path, so it stays Caution (RemoteExecution).
            if r.remote_cwd.is_none() {
                let relative_arg = parsed
                    .rest
                    .iter()
                    .any(|it| !it.starts_with('-') && !it.starts_with('/') && !it.starts_with('~'));
                let first_word = parsed.words.first().map(String::as_str).unwrap_or("");
                let relative_path_head = first_word.contains('/')
                    && !first_word.starts_with('/')
                    && !first_word.starts_with('~');
                if relative_arg || relative_path_head {
                    reasons.add(RiskReason::SecretAccess);
                }
            }
        }

        // Shell-active grammar. argv is structured and executed with no shell, but
        // the shell-injection sink joins it back into one string and injects it
        // into the real interactive shell, where these become load-bearing - so
        // any of them must refuse auto-approve. Over-approximating is the safe
        // direction: the cost is an extra confirmation, never an unreviewed shell
        // command.
        if parsed.has_assignment_prefix || parsed.has_equals_expansion || parsed.is_introducer {
            reasons.add(RiskReason::ShellChaining);
        }
        if parsed.has_shell_metachar {
            reasons.add(RiskReason::ShellChaining);
        }
        if parsed.has_leading_tilde {
            reasons.add(RiskReason::ShellChaining);
        }
        if parsed.has_history_expansion {
            reasons.add(RiskReason::ShellChaining);
        }
        if parsed.has_redirect {
            reasons.add(RiskReason::RedirectOverwrite);
        }
        if parsed.is_fork_bomb {
            reasons.add(RiskReason::ForkBomb);
        }

        // Token deny-sets over EVERY word's base (so `=rm`, `'rm'`, `\rm`, a
        // path-qualified `/bin/rm`, and a real command behind an introducer all
        // still match).
        for base in &parsed.bases {
            let b = base.as_str();
            if in_set(DESTRUCTIVE, b) {
                reasons.add(RiskReason::Destructive);
            }
            if in_set(NETWORK, b) {
                reasons.add(RiskReason::Network);
            }
            if in_set(PRIVILEGE, b) {
                reasons.add(RiskReason::Privilege);
            }
            if in_set(SECRET_TOOLS, b) {
                reasons.add(RiskReason::SecretAccess);
            }
        }

        // Code-execution gate. Running an interpreter on a script/module
        // (`python3 run.py`), an inline program (`-c`/`-e`, incl. glued
        // `-ccode`), a build tool (`make`, `gcc`), `eval`/`source`/`.`, or an
        // exec-spawning tool (`find -exec`, `xargs prog`) is arbitrary code the
        // token scan can't inspect. Auto-run must never clear it; bare/version
        // invocations (`python3`, `node -v`) stay Safe.
        let head = parsed.head.as_str();
        let interpreter_runs_code = in_set(CODE_INTERPRETERS, head)
            && parsed.rest.iter().any(|it| {
                !it.starts_with('-')
                    || in_set(CODE_FLAGS, it)
                    || INLINE_CODE_PREFIXES.iter().any(|p| it.starts_with(p))
            });
        let build_or_exec_tool = in_set(EXEC_TOOLS, head);
        let find_exec = head == "find" && parsed.rest.iter().any(|it| in_set(FIND_EXEC_FLAGS, it));
        if interpreter_runs_code || build_or_exec_tool || find_exec {
            reasons.add(RiskReason::CodeExecution);
        }

        // Dumping the environment can leak the API key when the env-var fallback
        // is in use (T-5.5 pre-work finding: the scaffold listed `env`/`printenv`
        // as Safe with no head rule, so a bare `env` auto-ran and dumped the key).
        if in_set(ENV_DUMP, head) {
            reasons.add(RiskReason::SecretAccess);
        }

        if in_set(PACKAGE_MANAGERS, head)
            && parsed
                .rest
                .iter()
                .any(|it| in_set(PACKAGE_MUTATING_SUBCOMMANDS, it))
        {
            reasons.add(RiskReason::PackageMutator);
        }

        if head == "chmod" && parsed.words.iter().any(|w| w.contains("777")) {
            reasons.add(RiskReason::Privilege);
        }

        // File-write helper. `aterm-write <abs-path>` (content on stdin) is the
        // editor's gated save path - a FileWrite Caution baseline. The
        // Dangerous-when-sensitive elevation comes FOR FREE from the
        // credential-path check below: `aterm-write ~/.ssh/config` also hits the
        // deny-set -> SecretAccess (Dangerous), with no second by-path rule.
        if in_set(WRITE_TOOLS, head) {
            reasons.add(RiskReason::FileWrite);
        }

        // Credential-path check (relative args resolved against cwd inside the
        // parse) against the single shared [`Secrets`] deny-set so the gate and
        // the sanitizer can't drift. Matched case-INSENSITIVELY: macOS's default
        // filesystem is case-insensitive, so `~/.SSH/id_rsa` names the same file -
        // a case-sensitive scan would under-classify it. Lower-casing both sides
        // over-approximates (the safe direction).
        let haystack = parsed.path_haystack.to_ascii_lowercase();
        if secrets
            .sensitive_paths()
            .iter()
            .any(|p| !p.is_empty() && haystack.contains(&p.to_ascii_lowercase()))
        {
            reasons.add(RiskReason::SecretAccess);
        }

        reasons.into_assessment()
    }

    /// Classify a finished, possibly MULTI-LINE command buffer (the input-editor
    /// submit, ported from `classifyCommandBuffer`). A single per-buffer
    /// classification is UNSAFE: the gate's HEAD-keyed rules (CodeExecution,
    /// PackageMutator, env-dump) only inspect the FIRST command's head, so a
    /// benign first line could smuggle a dangerous second past via an embedded
    /// newline. So we SPLIT on newlines into candidate command lines, classify
    /// EACH with the SAME [`DefaultRiskClassifier::classify`], and return the MAX
    /// level with the UNION of reasons. Any line that requires confirmation gates
    /// the WHOLE submit; no second line can hide a dangerous head behind a `\n`.
    /// Blank lines carry no command and are skipped; an empty/all-blank buffer is
    /// `Safe`. A single-line buffer is exactly `classify(&[line], ...)`.
    pub fn classify_buffer(
        &self,
        buffer: &str,
        cwd: Option<&str>,
        remote: Option<&RemoteContext>,
        secrets: &Secrets,
    ) -> RiskAssessment {
        let lines: Vec<&str> = buffer
            .split('\n')
            .filter(|l| !l.trim().is_empty())
            .collect();
        if lines.is_empty() {
            return RiskAssessment {
                level: Risk::Safe,
                reasons: Vec::new(),
            };
        }
        let mut level = Risk::Safe;
        let mut reasons = ReasonSet::new();
        for line in lines {
            let a = self.classify(&[line.to_string()], cwd, remote, secrets);
            if a.level > level {
                level = a.level;
            }
            for r in a.reasons {
                reasons.add(r);
            }
        }
        RiskAssessment {
            level,
            reasons: reasons.into_vec(),
        }
    }
}

/// Plain-English gloss for a [`RiskReason`] - the gate already knows *why* it
/// flagged a command, so the proposal card can say it in words instead of a bare
/// enum name. The `match` is exhaustive so adding a new reason without a gloss is
/// a compile error (a test enumerates every variant as a second guard). Ported
/// from `RiskGloss.kt`.
pub fn gloss_for(reason: RiskReason) -> &'static str {
    match reason {
        RiskReason::Destructive => "deletes or overwrites files",
        RiskReason::Network => "reaches out over the network",
        RiskReason::Privilege => "runs with elevated privileges",
        RiskReason::PackageMutator => "installs or removes packages",
        RiskReason::SecretAccess => "reads or writes a secret file",
        RiskReason::RedirectOverwrite => "redirects output to a file",
        RiskReason::ShellChaining => "uses shell operators or expansion",
        RiskReason::ForkBomb => "could spawn processes uncontrollably",
        RiskReason::CodeExecution => "runs arbitrary code",
        RiskReason::FileWrite => "writes a file to disk",
        RiskReason::RemoteExecution => "runs on the remote host over SSH",
        RiskReason::McpTool => "calls an MCP server tool with unverifiable effects",
    }
}

/// Insertion-ordered, deduplicated reason accumulator (the prototype's
/// `LinkedHashSet`).
struct ReasonSet(Vec<RiskReason>);

impl ReasonSet {
    fn new() -> Self {
        Self(Vec::new())
    }

    fn add(&mut self, r: RiskReason) {
        if !self.0.contains(&r) {
            self.0.push(r);
        }
    }

    fn into_vec(self) -> Vec<RiskReason> {
        self.0
    }

    fn into_assessment(self) -> RiskAssessment {
        let level = if self.0.iter().any(|r| r.is_dangerous()) {
            Risk::Dangerous
        } else if self.0.is_empty() {
            Risk::Safe
        } else {
            Risk::Caution
        };
        RiskAssessment {
            level,
            reasons: self.0,
        }
    }
}

fn in_set(set: &[&str], s: &str) -> bool {
    set.contains(&s)
}

const DESTRUCTIVE: &[&str] = &[
    "rm", "rmdir", "dd", "mkfs", "shred", "truncate", "fdisk", "diskutil",
];
const NETWORK: &[&str] = &[
    "curl", "wget", "nc", "ncat", "ssh", "scp", "sftp", "ftp", "telnet", "rsync",
];
const PRIVILEGE: &[&str] = &["sudo", "su", "doas", "chown"];
const SECRET_TOOLS: &[&str] = &["security", "keytool"];
const ENV_DUMP: &[&str] = &["env", "printenv"];
const CODE_INTERPRETERS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "dash",
    "fish",
    "ksh",
    "csh",
    "tcsh",
    "python",
    "python2",
    "python3",
    "pypy",
    "perl",
    "ruby",
    "node",
    "nodejs",
    "deno",
    "bun",
    "php",
    "awk",
    "gawk",
    "mawk",
    "osascript",
    "lua",
    "Rscript",
    "tclsh",
    "expect",
    "env",
    "xargs",
];
const CODE_FLAGS: &[&str] = &[
    "-c",
    "-e",
    "-E",
    "-r",
    "-le",
    "-lc",
    "-lce",
    "--eval",
    "-eval",
    "--exec",
    "--command",
    "--run",
];
/// Catch flags glued to their code argument, e.g. `python3 -cimport os`.
const INLINE_CODE_PREFIXES: &[&str] = &["-c", "-e", "--eval", "--exec", "--command"];
/// Tools / builtins that execute code or recipes regardless of inline-code flags.
/// `eval`, `source`, and `.` always run their arguments as shell code.
const EXEC_TOOLS: &[&str] = &[
    "eval", "source", ".", "make", "gmake", "cmake", "ninja", "cc", "gcc", "g++", "c++", "clang",
    "clang++", "ld", "go", "gradle", "gradlew", "mvn", "sbt",
];
const FIND_EXEC_FLAGS: &[&str] = &["-exec", "-execdir", "-ok", "-okdir"];
/// The bundled atomic file-write helper - the editor's only sanctioned write path.
const WRITE_TOOLS: &[&str] = &["aterm-write"];
const PACKAGE_MANAGERS: &[&str] = &[
    "brew", "npm", "pnpm", "yarn", "bun", "pip", "pip3", "uv", "gem", "cargo", "apt", "apt-get",
    "go",
];
const PACKAGE_MUTATING_SUBCOMMANDS: &[&str] = &[
    "install",
    "uninstall",
    "remove",
    "add",
    "rm",
    "update",
    "upgrade",
    "ci",
    "i",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    /// Classify an argv with the default (local, no-remote) context.
    fn classify(parts: &[&str]) -> RiskAssessment {
        DefaultRiskClassifier.classify(&argv(parts), None, None, &Secrets::new())
    }

    fn classify_cwd(parts: &[&str], cwd: &str) -> RiskAssessment {
        DefaultRiskClassifier.classify(&argv(parts), Some(cwd), None, &Secrets::new())
    }

    fn classify_remote(parts: &[&str], remote: &RemoteContext) -> RiskAssessment {
        DefaultRiskClassifier.classify(
            &argv(parts),
            Some("/Users/me/projects"),
            Some(remote),
            &Secrets::new(),
        )
    }

    fn has(a: &RiskAssessment, r: RiskReason) -> bool {
        a.reasons.contains(&r)
    }

    // --- plain / destructive / privilege / network / package -------------------

    #[test]
    fn plain_commands_are_safe() {
        assert_eq!(classify(&["ls", "-la"]).level, Risk::Safe);
        assert_eq!(classify(&["git", "status"]).level, Risk::Safe);
        assert_eq!(classify(&["echo", "hello"]).level, Risk::Safe);
        assert_eq!(classify(&["cat", "README.md"]).level, Risk::Safe);
    }

    #[test]
    fn destructive_is_dangerous() {
        let a = classify(&["rm", "-rf", "/"]);
        assert_eq!(a.level, Risk::Dangerous);
        assert!(has(&a, RiskReason::Destructive));
    }

    #[test]
    fn privilege_plus_destructive() {
        let a = classify(&["sudo", "rm", "foo"]);
        assert_eq!(a.level, Risk::Dangerous);
        assert!(has(&a, RiskReason::Privilege));
        assert!(has(&a, RiskReason::Destructive));
    }

    #[test]
    fn network_is_caution() {
        let a = classify(&["curl", "https://example.com"]);
        assert_eq!(a.level, Risk::Caution);
        assert!(has(&a, RiskReason::Network));
    }

    #[test]
    fn package_mutation_is_caution() {
        assert_eq!(
            classify(&["brew", "install", "ripgrep"]).level,
            Risk::Caution
        );
        assert_eq!(classify(&["npm", "ci"]).level, Risk::Caution);
        // a read-only package subcommand is not flagged
        assert_eq!(classify(&["brew", "list"]).level, Risk::Safe);
    }

    // --- secret access ---------------------------------------------------------

    #[test]
    fn secret_access_is_dangerous() {
        let a = classify(&["security", "find-generic-password", "-w"]);
        assert_eq!(a.level, Risk::Dangerous);
        assert!(has(&a, RiskReason::SecretAccess));

        let b = classify(&["cat", "~/.ssh/id_rsa"]);
        assert_eq!(b.level, Risk::Dangerous);
        assert!(has(&b, RiskReason::SecretAccess));

        let d = classify(&["printenv", "OPENAI_API_KEY"]);
        assert_eq!(d.level, Risk::Dangerous);
        assert!(has(&d, RiskReason::SecretAccess));
    }

    #[test]
    fn unlisted_credential_files_are_dangerous() {
        for path in ["~/.git-credentials", "~/.npmrc", ".docker/config.json"] {
            let a = classify(&["cat", path]);
            assert_eq!(
                a.level,
                Risk::Dangerous,
                "expected Dangerous for cat {path}"
            );
            assert!(has(&a, RiskReason::SecretAccess));
        }
        // also via cwd resolution of a relative arg
        let b = classify_cwd(&["cat", "hosts.yml"], "/Users/me/.config/gh");
        assert_eq!(b.level, Risk::Dangerous);
    }

    #[test]
    fn reading_aterms_own_config_is_dangerous() {
        for path in [
            "~/.config/aterm/config.toml",
            "~/.aterm/config.toml",
            "/Users/me/.config/aterm/config.toml",
        ] {
            let a = classify(&["cat", path]);
            assert_eq!(
                a.level,
                Risk::Dangerous,
                "expected Dangerous for cat {path}"
            );
            assert!(has(&a, RiskReason::SecretAccess));
        }
    }

    #[test]
    fn any_read_under_the_aws_dir_is_dangerous() {
        for cmd in [
            vec!["cat", "/Users/me/.aws/sso/cache/abc.json"],
            vec!["cp", "/Users/me/.aws/credentials", "/tmp/x"],
        ] {
            let a = classify(&cmd);
            assert_eq!(a.level, Risk::Dangerous, "expected Dangerous for {cmd:?}");
            assert!(has(&a, RiskReason::SecretAccess));
        }
    }

    #[test]
    fn relative_secret_path_against_cwd_is_dangerous() {
        let a = classify_cwd(&["cat", "credentials"], "/Users/me/.aws");
        assert_eq!(a.level, Risk::Dangerous);
        assert!(has(&a, RiskReason::SecretAccess));
        // A benign relative read in an ordinary directory stays Safe.
        let b = classify_cwd(&["cat", "notes.txt"], "/Users/me/projects");
        assert_eq!(b.level, Risk::Safe);
    }

    // --- THE env-dump fail-open (T-5.5 pre-work finding) -----------------------

    #[test]
    fn env_dump_is_dangerous() {
        // The canonical credential-exfil shape the scaffold mis-classified as Safe.
        assert_eq!(classify(&["env"]).level, Risk::Dangerous);
        assert_eq!(classify(&["printenv"]).level, Risk::Dangerous);
        assert!(has(&classify(&["env"]), RiskReason::SecretAccess));
        assert!(has(
            &classify(&["printenv", "AWS_SECRET_ACCESS_KEY"]),
            RiskReason::SecretAccess
        ));
        // `env` only as an ARGUMENT (head is echo) is not flagged - the benign
        // carrier the scaffold's blanket SAFE_PROGRAMS listing would have allowed.
        assert_eq!(classify(&["echo", "env"]).level, Risk::Safe);
    }

    // --- code execution --------------------------------------------------------

    #[test]
    fn code_execution_is_dangerous() {
        for cmd in [
            vec!["sh", "-c", "echo hi"],
            vec!["bash", "-lc", "whoami"],
            vec!["python3", "-c", "import os; print(os.environ)"],
            vec!["python3", "-cimport os; print(1)"], // glued flag form
            vec!["node", "-e", "console.log(1)"],
            vec!["perl", "-e", "print 1"],
            vec!["/usr/bin/ruby", "-e", "puts 1"],
            vec!["env", "sh", "-c", "id"],
            vec!["python3", "run.py"], // interpreter runs a script file (no inline flag)
            vec!["node", "app.js"],
            vec!["bash", "deploy.sh"],
            vec!["make"], // build tool
            vec!["make", "install"],
            vec!["gcc", "evil.c", "-o", "evil"],
            vec!["xargs", "bash", "run.sh"], // launcher spawns a program
            vec![
                "find", ".", "-type", "f", "-exec", "bash", "run.sh", "{}", "+",
            ],
        ] {
            let a = classify(&cmd);
            assert_eq!(a.level, Risk::Dangerous, "expected Dangerous for {cmd:?}");
            assert!(
                has(&a, RiskReason::CodeExecution),
                "expected CodeExecution for {cmd:?}"
            );
        }
        // Bare REPL / version / read-only forms stay Safe.
        assert_eq!(classify(&["python3"]).level, Risk::Safe);
        assert_eq!(classify(&["node", "-v"]).level, Risk::Safe);
        assert_eq!(classify(&["python3", "--version"]).level, Risk::Safe);
        assert_eq!(
            classify(&["find", ".", "-name", "Main.kt"]).level,
            Risk::Safe
        );
    }

    #[test]
    fn eval_source_and_dot_are_code_execution() {
        for cmd in [
            vec!["eval", "ls"],
            vec!["source", "deploy.sh"],
            vec![".", "env.sh"],
            vec!["/usr/bin/eval", "x"], // path-qualified
        ] {
            let a = classify(&cmd);
            assert_eq!(a.level, Risk::Dangerous, "expected Dangerous for {cmd:?}");
            assert!(
                has(&a, RiskReason::CodeExecution),
                "expected CodeExecution for {cmd:?}"
            );
        }
    }

    // --- shell-active grammar --------------------------------------------------

    #[test]
    fn redirect_is_caution() {
        let a = classify(&["echo", "hi", ">", "out.txt"]);
        assert_eq!(a.level, Risk::Caution);
        assert!(has(&a, RiskReason::RedirectOverwrite));
    }

    #[test]
    fn glued_pipe_and_chain_are_shell_chaining() {
        for cmd in [
            vec!["ps", "aux|less"], // glued pipe
            vec!["a||b"],           // glued or-list
            vec!["a;b"],            // glued sequence
            vec!["a&b"],            // glued background + next
            vec!["a", "&&", "b"],   // standalone operator token
        ] {
            let a = classify(&cmd);
            assert!(
                has(&a, RiskReason::ShellChaining),
                "expected ShellChaining for {cmd:?}"
            );
            assert_eq!(a.level, Risk::Caution, "expected Caution for {cmd:?}");
        }
    }

    #[test]
    fn command_substitution_is_shell_chaining() {
        for cmd in [
            vec!["echo", "$(whoami)"],
            vec!["echo", "`id`"],
            vec!["ls", "$(dirname /a/b)"],
        ] {
            assert!(
                has(&classify(&cmd), RiskReason::ShellChaining),
                "expected ShellChaining for {cmd:?}"
            );
        }
    }

    #[test]
    fn globs_brace_and_process_substitution_are_shell_active() {
        for cmd in [
            vec!["ls", "*.txt"],
            vec!["cat", "file?.log"],
            vec!["ls", "[abc].kt"],
            vec!["touch", "file{1,2,3}.txt"],
            vec!["diff", "<(ls a)", "<(ls b)"],
        ] {
            let a = classify(&cmd);
            assert!(
                has(&a, RiskReason::ShellChaining),
                "expected ShellChaining for {cmd:?}"
            );
            assert_eq!(a.level, Risk::Caution, "expected Caution for {cmd:?}");
        }
    }

    #[test]
    fn leading_tilde_is_shell_active() {
        for cmd in [
            vec!["ls", "~"],
            vec!["cat", "~/notes.txt"],
            vec!["ls", "~otheruser"],
        ] {
            assert!(
                has(&classify(&cmd), RiskReason::ShellChaining),
                "expected ShellChaining for {cmd:?}"
            );
        }
        // A trailing tilde (backup-file name) does not expand and is left alone.
        assert!(!has(
            &classify(&["ls", "notes.txt~"]),
            RiskReason::ShellChaining
        ));
        assert_eq!(classify(&["ls", "notes.txt~"]).level, Risk::Safe);
    }

    #[test]
    fn caret_history_substitution_is_flagged() {
        let a = classify(&["^date^id"]);
        assert!(has(&a, RiskReason::ShellChaining));
        // A caret inside an argument (regex anchor) is NOT line-initial -> no FP.
        assert_eq!(classify(&["grep", "^needle", "file.txt"]).level, Risk::Safe);
    }

    #[test]
    fn env_assignment_prefix_does_not_hide_the_real_head() {
        let make = classify(&["CC=clang", "make"]);
        assert_eq!(make.level, Risk::Dangerous);
        assert!(has(&make, RiskReason::CodeExecution));
        assert!(has(&make, RiskReason::ShellChaining));

        assert!(has(
            &classify(&["X=1", "env", "sh", "deploy.sh"]),
            RiskReason::CodeExecution
        ));
        assert!(has(
            &classify(&["LC_ALL=C", "env"]),
            RiskReason::SecretAccess
        ));
        assert!(has(
            &classify(&["npm_config_yes=true", "npm", "install", "evil"]),
            RiskReason::PackageMutator
        ));
        assert!(has(
            &classify(&["UMASK=0", "chmod", "777", "dir"]),
            RiskReason::Privilege
        ));
    }

    #[test]
    fn equals_expansion_head_is_classified() {
        let py = classify(&["=python3", "/tmp/app.py"]);
        assert!(has(&py, RiskReason::ShellChaining));
        assert_eq!(py.level, Risk::Dangerous);
        assert!(has(&py, RiskReason::CodeExecution));
        assert!(has(&classify(&["=env"]), RiskReason::SecretAccess));
    }

    #[test]
    fn precommand_modifiers_are_flagged_and_the_real_head_classified() {
        for cmd in [
            vec!["noglob", "python3", "/tmp/x.py"],
            vec!["time", "python3", "/tmp/x.py"],
            vec!["command", "rm", "-rf", "/"],
        ] {
            assert!(
                has(&classify(&cmd), RiskReason::ShellChaining),
                "precommand modifier must be flagged: {cmd:?}"
            );
        }
        // the real command behind the introducer is still classified via bases
        assert!(has(
            &classify(&["command", "rm", "-rf", "/"]),
            RiskReason::Destructive
        ));
    }

    #[test]
    fn numeric_assignment_prefix_is_handled() {
        for cmd in [
            vec!["9=x", "bash", "deploy.sh"],
            vec!["9=x", "env"],
            vec!["0=1", "make"],
        ] {
            assert!(
                has(&classify(&cmd), RiskReason::ShellChaining),
                "numeric assignment must be ShellChaining: {cmd:?}"
            );
        }
        assert!(has(
            &classify(&["9=x", "bash", "deploy.sh"]),
            RiskReason::CodeExecution
        ));
        assert!(has(&classify(&["9=x", "env"]), RiskReason::SecretAccess));
    }

    #[test]
    fn backslash_and_quoted_command_are_classified() {
        let backslash = classify(&["\\rm", "-rf", "/"]);
        assert_eq!(backslash.level, Risk::Dangerous);
        assert!(has(&backslash, RiskReason::Destructive));

        let quoted = classify(&["'rm'", "-rf", "/"]);
        assert!(has(&quoted, RiskReason::Destructive));
    }

    #[test]
    fn interior_whitespace_packed_command_is_classified() {
        let rm_home = classify(&["rm -rf ~/important"]);
        assert_eq!(rm_home.level, Risk::Dangerous);
        assert!(has(&rm_home, RiskReason::Destructive));

        let sudo_rm = classify(&["sudo rm -rf /"]);
        assert_eq!(sudo_rm.level, Risk::Dangerous);
        assert!(has(&sudo_rm, RiskReason::Destructive));
        assert!(has(&sudo_rm, RiskReason::Privilege));

        assert!(has(
            &classify(&["eval echo PWNED"]),
            RiskReason::CodeExecution
        ));
        assert!(has(
            &classify(&["security find-generic-password -w -s login"]),
            RiskReason::SecretAccess
        ));
    }

    #[test]
    fn control_characters_and_nul_are_flagged() {
        let argv = vec!["env\u{0}".to_string()];
        let a = DefaultRiskClassifier.classify(&argv, None, None, &Secrets::new());
        assert!(
            has(&a, RiskReason::ShellChaining),
            "control char must be flagged"
        );
        // the deny-set still matches after the control byte is stripped from base
        assert!(has(&a, RiskReason::SecretAccess));
    }

    // --- aterm-write (file write) ---------------------------------------------

    #[test]
    fn aterm_write_to_ordinary_file_is_caution_file_write_only() {
        let a = classify(&["aterm-write", "/Users/me/notes.md"]);
        assert_eq!(a.level, Risk::Caution);
        assert!(has(&a, RiskReason::FileWrite));
        assert!(
            !has(&a, RiskReason::SecretAccess),
            "an ordinary file is not a secret path"
        );
    }

    #[test]
    fn aterm_write_to_credential_or_startup_path_is_dangerous() {
        for path in [
            "~/.ssh/config",
            "~/.zshrc",
            "/etc/sudoers",
            "~/.ssh/authorized_keys",
        ] {
            let a = classify(&["aterm-write", path]);
            assert_eq!(
                a.level,
                Risk::Dangerous,
                "expected Dangerous for aterm-write {path}"
            );
            assert!(
                has(&a, RiskReason::FileWrite),
                "head check still fires for {path}"
            );
            assert!(
                has(&a, RiskReason::SecretAccess),
                "the sensitive path elevates {path}"
            );
        }
    }

    #[test]
    fn non_write_command_has_no_file_write_reason() {
        assert!(!has(&classify(&["cat", "notes.md"]), RiskReason::FileWrite));
        assert!(!has(
            &classify(&["echo", "aterm-write"]),
            RiskReason::FileWrite
        ));
        assert_eq!(classify(&["cat", "notes.md"]).level, Risk::Safe);
    }

    // --- fork bomb / chmod -----------------------------------------------------

    #[test]
    fn fork_bomb_is_flagged() {
        let a = classify(&[":(){ :|:& };:"]);
        assert!(has(&a, RiskReason::ForkBomb));
    }

    #[test]
    fn chmod_777_is_privilege() {
        let a = classify(&["chmod", "777", "dir"]);
        assert!(has(&a, RiskReason::Privilege));
        assert_eq!(a.level, Risk::Dangerous);
    }

    // --- remote (RemoteContext) ------------------------------------------------

    #[test]
    fn plain_remote_command_is_caution_from_remote_execution() {
        let a = classify_remote(&["ls", "-la", "/var/log"], &RemoteContext::new("prod"));
        assert_eq!(a.level, Risk::Caution);
        assert!(has(&a, RiskReason::RemoteExecution));
    }

    #[test]
    fn remote_destructive_is_dangerous_from_inner_command() {
        let a = classify_remote(&["rm", "-rf", "/data"], &RemoteContext::new("prod"));
        assert_eq!(a.level, Risk::Dangerous);
        assert!(has(&a, RiskReason::Destructive));
        assert!(has(&a, RiskReason::RemoteExecution));
    }

    #[test]
    fn unknown_remote_cwd_over_approximates_relative_path_arg() {
        // The model's LOCAL cwd is discarded, so even a benign-looking local cwd
        // does not relax this.
        let a = classify_remote(&["cat", "credentials"], &RemoteContext::new("prod"));
        assert_eq!(a.level, Risk::Dangerous);
        assert!(has(&a, RiskReason::SecretAccess));
        assert!(has(&a, RiskReason::RemoteExecution));
    }

    #[test]
    fn remote_relative_path_head_is_dangerous_under_unknown_cwd() {
        for head in ["./deploy.sh", "bin/tool", "../x/run"] {
            let a = classify_remote(&[head], &RemoteContext::new("prod"));
            assert_eq!(
                a.level,
                Risk::Dangerous,
                "expected Dangerous for remote relative head {head}"
            );
            assert!(has(&a, RiskReason::SecretAccess));
        }
        // A bare command-name head (ls) is a $PATH lookup, not a path -> Caution.
        let ls = classify_remote(&["ls"], &RemoteContext::new("prod"));
        assert_eq!(ls.level, Risk::Caution);
    }

    #[test]
    fn absolute_non_sensitive_remote_path_is_not_falsely_flagged() {
        let a = classify_remote(&["cat", "/etc/hostname"], &RemoteContext::new("prod"));
        assert_eq!(a.level, Risk::Caution);
        assert!(has(&a, RiskReason::RemoteExecution));
        assert!(!has(&a, RiskReason::SecretAccess));
    }

    #[test]
    fn remote_absolute_sensitive_path_is_still_flagged() {
        let a = classify_remote(&["cat", "/root/.ssh/id_rsa"], &RemoteContext::new("prod"));
        assert_eq!(a.level, Risk::Dangerous);
        assert!(has(&a, RiskReason::SecretAccess));
    }

    #[test]
    fn known_remote_cwd_resolves_relative_args_like_local() {
        let secret = classify(&["cat", "credentials"]); // sanity: local non-cwd is Safe-ish
        let _ = secret;
        let secret = DefaultRiskClassifier.classify(
            &argv(&["cat", "credentials"]),
            None,
            Some(&RemoteContext::with_cwd("prod", "/home/deploy/.aws")),
            &Secrets::new(),
        );
        assert_eq!(secret.level, Risk::Dangerous);
        assert!(has(&secret, RiskReason::SecretAccess));

        let benign = DefaultRiskClassifier.classify(
            &argv(&["cat", "notes.txt"]),
            None,
            Some(&RemoteContext::with_cwd("prod", "/home/deploy/work")),
            &Secrets::new(),
        );
        assert_eq!(benign.level, Risk::Caution);
        assert!(
            !has(&benign, RiskReason::SecretAccess),
            "a known remote cwd is not over-approximated"
        );
    }

    #[test]
    fn local_classify_is_unchanged_when_remote_is_none() {
        let a = classify(&["ls", "-la"]);
        assert_eq!(a.level, Risk::Safe);
        assert!(!has(&a, RiskReason::RemoteExecution));
    }

    // --- single Secrets source (AC: deny-set borrowed, not copied) -------------

    #[test]
    fn gate_reads_the_shared_mutated_secrets_instance() {
        let mut s = Secrets::new();
        // `cat vault-keys` is otherwise Safe (cat inert, no shell-active chars).
        assert_eq!(
            DefaultRiskClassifier
                .classify(&argv(&["cat", "vault-keys"]), None, None, &s)
                .level,
            Risk::Safe
        );
        s.add_sensitive_path("vault-keys");
        let a = DefaultRiskClassifier.classify(&argv(&["cat", "vault-keys"]), None, None, &s);
        assert_eq!(
            a.level,
            Risk::Dangerous,
            "the gate must read THIS instance's deny-set"
        );
        assert!(has(&a, RiskReason::SecretAccess));
    }

    // --- multi-line buffer gate (classify_buffer) ------------------------------

    #[test]
    fn single_line_buffer_matches_ordinary_classification() {
        let c = DefaultRiskClassifier;
        let s = Secrets::new();
        assert_eq!(
            c.classify_buffer("ls -la", None, None, &s),
            c.classify(&["ls -la".to_string()], None, None, &s)
        );
        assert_eq!(
            c.classify_buffer("git status", None, None, &s).level,
            Risk::Safe
        );
        assert_eq!(
            c.classify_buffer("rm -rf /", None, None, &s).level,
            Risk::Dangerous
        );
    }

    #[test]
    fn empty_or_blank_buffer_is_safe() {
        let c = DefaultRiskClassifier;
        let s = Secrets::new();
        assert_eq!(c.classify_buffer("", None, None, &s).level, Risk::Safe);
        assert_eq!(
            c.classify_buffer("   \n\t\n  ", None, None, &s).level,
            Risk::Safe
        );
        assert!(c.classify_buffer("", None, None, &s).reasons.is_empty());
    }

    #[test]
    fn a_smuggled_code_execution_second_line_is_caught_by_the_split() {
        // THE moat: classifying the whole buffer as one string makes the head
        // `echo`, so the head-keyed code-exec rule never sees `python3` and would
        // slip through at Caution. The per-line split exposes it -> Dangerous.
        let c = DefaultRiskClassifier;
        let s = Secrets::new();
        let a = c.classify_buffer("echo hi\npython3 evil.py", None, None, &s);
        assert_eq!(
            a.level,
            Risk::Dangerous,
            "the smuggled code-exec second line must elevate to Dangerous"
        );
        assert!(has(&a, RiskReason::CodeExecution));
    }

    #[test]
    fn max_level_and_union_of_reasons_across_lines() {
        let c = DefaultRiskClassifier;
        let s = Secrets::new();
        let a = c.classify_buffer("curl https://example.com\nrm -rf /tmp/x", None, None, &s);
        assert_eq!(
            a.level,
            Risk::Dangerous,
            "MAX(Caution, Dangerous) = Dangerous"
        );
        assert!(has(&a, RiskReason::Network));
        assert!(has(&a, RiskReason::Destructive));
    }

    #[test]
    fn blank_lines_between_commands_are_skipped() {
        let c = DefaultRiskClassifier;
        let s = Secrets::new();
        let a = c.classify_buffer("ls\n\n\nrm -rf /", None, None, &s);
        assert_eq!(a.level, Risk::Dangerous);
        assert!(has(&a, RiskReason::Destructive));
    }

    // --- gloss -----------------------------------------------------------------

    #[test]
    fn every_reason_has_a_nonempty_gloss() {
        // Enumerating every variant guards the exhaustive `match` (a new reason
        // without a gloss must be a compile error or fail here).
        for r in [
            RiskReason::Destructive,
            RiskReason::Network,
            RiskReason::Privilege,
            RiskReason::PackageMutator,
            RiskReason::SecretAccess,
            RiskReason::RedirectOverwrite,
            RiskReason::ShellChaining,
            RiskReason::ForkBomb,
            RiskReason::CodeExecution,
            RiskReason::FileWrite,
            RiskReason::RemoteExecution,
            RiskReason::McpTool,
        ] {
            assert!(!gloss_for(r).is_empty(), "missing gloss for {r:?}");
        }
    }
}
