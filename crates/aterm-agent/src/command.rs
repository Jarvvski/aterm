//! `ShellCommand`: a zsh-aware parse, computed ONCE for the risk gate then read
//! by every rule. Ported (near-verbatim) from the prototype `ShellCommand.kt`.
//!
//! The model proposes argv tokens; the shell-injection sink space-joins them and
//! a real shell re-splits on whitespace, so this parses what the SHELL will see,
//! not the argv elements: it word-splits ([`ShellCommand::words`]), keeps the raw
//! join for the metacharacter / control-byte scan ([`ShellCommand::raw_joined`]),
//! resolves the real command [`ShellCommand::head`] past env-assignment prefixes
//! and zsh equals-expansion / precommand modifiers, and precomputes the
//! shell-grammar facts the rules key off.
//!
//! Pure SYNTAX, deterministic, no risk policy - so the zsh-aware parse is testable
//! on its own rather than only through a final `Risk` level. *Which* commands are
//! destructive / which paths are secret stays in [`crate::risk`]; this type only
//! says what the shell would DO with the string.

/// A shell command parsed once for the risk gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCommand {
    /// Tokens after whitespace-splitting the space-joined argv (what the shell
    /// actually word-splits to).
    pub words: Vec<String>,
    /// The argv joined with original spacing, for the raw metacharacter /
    /// control-byte scan (preserves edge newlines a whitespace-split would drop).
    pub raw_joined: String,
    /// [`command_base`] of every word, for the deny-set scans (quotes / control
    /// bytes / leading `\`/`=` / path stripped) - so `=rm`, `'rm'`, `\rm`, and a
    /// path-qualified `/bin/rm` all still match the bare `rm`.
    pub bases: Vec<String>,
    /// The real command word, past any env-assignment prefix and stripped of
    /// shell noise.
    pub head: String,
    /// The words after the [`ShellCommand::head`].
    pub rest: Vec<String>,
    /// A leading `VAR=val` / `name+=val` env-assignment prefix is present
    /// (shell-active).
    pub has_assignment_prefix: bool,
    /// The head began with `=` (zsh equals-expansion; shell-active).
    pub has_equals_expansion: bool,
    /// The head is a zsh reserved word / precommand modifier that hides the real
    /// command (shell-active).
    pub is_introducer: bool,
    /// The raw command string contains a shell metacharacter / quote / control
    /// byte (shell-active).
    pub has_shell_metachar: bool,
    /// The raw command string contains `>` (any output redirection / dup).
    pub has_redirect: bool,
    /// A word-initial `~` (home/user expansion; shell-active).
    pub has_leading_tilde: bool,
    /// A line-initial `^` (zsh history quick-substitution; shell-active).
    pub has_history_expansion: bool,
    /// The classic `:(){:|:&};:` fork bomb.
    pub is_fork_bomb: bool,
    /// Command line + cwd + cwd-resolved relative args, for the credential-path
    /// substring check.
    pub path_haystack: String,
}

impl ShellCommand {
    /// No command at all (empty/blank argv): the gate treats this as Safe.
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Parse `command` (argv tokens) as the shell will see it, resolving relative
    /// path args against `cwd` for the credential-path check (so `cat credentials`
    /// in `~/.aws` cannot evade the deny-set).
    pub fn parse(command: &[String], cwd: Option<&str>) -> Self {
        // Tokenize into shell WORDS, not argv elements: the shell-injection sink
        // space-joins the argv into one string and the real shell re-splits it on
        // whitespace, so a single element can smuggle a whole command line
        // (`["rm -rf ~/important"]`, `["sudo rm -rf /"]`).
        let words: Vec<String> = command
            .iter()
            .flat_map(|el| el.split_whitespace())
            .map(str::to_string)
            .collect();
        if words.is_empty() {
            return Self::empty();
        }

        let joined = words.join(" ");
        // The whitespace split drops edge newlines/spaces from a token
        // (`"ok\n"` -> `"ok"`), so the raw join is what preserves the
        // command-smuggling the sink's space-join sends to the shell verbatim.
        let raw_joined = command.join(" ");

        // Resolve the REAL command head past leading shell grammar the naive
        // "first token is the command" model misses:
        //  - `VAR=val` env-assignment prefix(es): a real shell strips them and
        //    runs the following word, hijacking the environment (PATH /
        //    LD_PRELOAD / DYLD_INSERT_LIBRARIES) and shifting the command word
        //    right - hiding the true head (`CC=clang make`, `LC_ALL=C env`).
        //  - `=word` zsh equals-expansion (EQUALS on by default): runs the
        //    resolved path of `word`; the leading `=` likewise corrupts head
        //    detection (`=python3 app.py`).
        let cmd_index = words
            .iter()
            .position(|w| !is_assignment_prefix(w))
            .unwrap_or(words.len());
        let head_token_raw = words.get(cmd_index).map(String::as_str).unwrap_or("");
        let head = command_base(head_token_raw);

        // Credential-path haystack: the normalized command + cwd + each relative
        // arg resolved against cwd, so a relative read against a sensitive
        // directory cannot dodge the deny-set substring scan.
        let mut path_haystack = joined.clone();
        if let Some(cwd) = cwd {
            if !cwd.trim().is_empty() {
                path_haystack.push(' ');
                path_haystack.push_str(cwd);
                for token in words.iter().skip(1) {
                    if !token.starts_with('-') {
                        path_haystack.push(' ');
                        path_haystack.push_str(&resolve_against(cwd, token));
                    }
                }
            }
        }

        let bases: Vec<String> = words.iter().map(|w| command_base(w)).collect();
        let rest: Vec<String> = words.iter().skip(cmd_index + 1).cloned().collect();

        ShellCommand {
            has_assignment_prefix: cmd_index > 0,
            has_equals_expansion: head_token_raw.starts_with('='),
            is_introducer: COMMAND_INTRODUCERS.contains(&head.as_str()),
            has_shell_metachar: raw_joined.chars().any(is_shell_significant),
            has_redirect: raw_joined.contains('>'),
            // Only a word-initial tilde expands, so a trailing `~` (backup-file
            // name) is left alone.
            has_leading_tilde: words.iter().any(|w| w.starts_with('~')),
            // Only the first word triggers `^old^new`; a `^` inside a later arg
            // (a `grep ^anchor` regex) is left alone.
            has_history_expansion: words.first().is_some_and(|w| w.starts_with('^')),
            is_fork_bomb: joined.replace(' ', "").contains(":(){:|:&};:"),
            words,
            raw_joined,
            bases,
            head,
            rest,
            path_haystack,
        }
    }

    /// Convenience: parse a single raw command line (no cwd context).
    pub fn parse_line(line: &str) -> Self {
        Self::parse(&[line.to_string()], None)
    }

    fn empty() -> Self {
        ShellCommand {
            words: Vec::new(),
            raw_joined: String::new(),
            bases: Vec::new(),
            head: String::new(),
            rest: Vec::new(),
            has_assignment_prefix: false,
            has_equals_expansion: false,
            is_introducer: false,
            has_shell_metachar: false,
            has_redirect: false,
            has_leading_tilde: false,
            has_history_expansion: false,
            is_fork_bomb: false,
            path_haystack: String::new(),
        }
    }
}

/// zsh reserved words / precommand modifiers that take a following command,
/// hiding the real command word from head-based detection (`man zshmisc`
/// "Precommand Modifiers" plus `time`/`repeat`/`coproc`). External wrapper
/// binaries (nohup/nice/timeout/setsid) are out of scope - a token deny-list
/// cannot enumerate them, so they fall through to RequireConfirm. The classifier
/// is a best-effort token/grammar gate, NOT a complete boundary (the sandbox in
/// T-5.7 is the boundary).
const COMMAND_INTRODUCERS: &[&str] = &[
    "command",
    "exec",
    "builtin",
    "noglob",
    "nocorrect",
    "time",
    "repeat",
    "coproc",
    "-",
];

/// Named printable characters a real shell interprets once argv is joined into a
/// command string. `>` is handled separately ([`ShellCommand::has_redirect`]);
/// quoting (`'` `"` `\`) and control bytes (incl. NUL / newline) are added by
/// [`is_shell_significant`]; `~` and `=`/`+=` are checked positionally.
const SHELL_METACHARS: &[char] = &[
    ';', '&', '|', '`', '$', '(', ')', '{', '}', '<', '*', '?', '[', ']', '!',
];

/// A leading `name=val` / `name+=val` token is a shell environment-assignment
/// prefix, not the command word. Mirrors the prototype regex `^[A-Za-z0-9_]+\+?=`:
/// `name` spans zsh's full assignment-word grammar - regular identifiers AND
/// all-numeric positional names (`9=x` sets `$9`). (Array-element forms
/// `name[i]=` carry `[`/`]`, already caught by [`is_shell_significant`].)
fn is_assignment_prefix(tok: &str) -> bool {
    let bytes = tok.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    if i == 0 {
        return false; // need at least one identifier char before `=`
    }
    if i < bytes.len() && bytes[i] == b'+' {
        i += 1; // optional append-assignment `+=`
    }
    i < bytes.len() && bytes[i] == b'='
}

/// The command word a real shell actually runs after it strips shell noise the
/// deny-sets would otherwise miss: control bytes (incl. NUL, which the shell
/// drops), surrounding quotes, a leading `\` (alias suppression) or `=` (zsh
/// equals-expansion), and the directory part of a path. So `rm `, `'rm'`, `\rm`,
/// and `=security` resolve to `rm`/`rm`/`rm`/`security`.
fn command_base(token: &str) -> String {
    let mut s: String = token
        .chars()
        .filter(|c| {
            let n = *c as u32;
            n >= 0x20 && n != 0x7f
        })
        .collect();
    s = s.replace(['\'', '"'], "");
    if let Some(rest) = s.strip_prefix('\\') {
        s = rest.to_string();
    }
    if let Some(rest) = s.strip_prefix('=') {
        s = rest.to_string();
    }
    // substringAfterLast('/'): the part after the last `/`, or the whole string
    // when there is no `/`.
    match s.rfind('/') {
        Some(idx) => s[idx + 1..].to_string(),
        None => s,
    }
}

/// A character a real shell treats specially, so a token containing it cannot be
/// vouched safe for auto-run: the named metacharacters, quoting (`'` `"` `\`), and
/// any control byte (NUL, tab, CR, LF, ESC, DEL). `>` (redirect), leading `~`, and
/// `=`/`+=` assignment are handled separately. Non-ASCII is intentionally NOT
/// flagged - a real shell does not interpret homoglyphs.
fn is_shell_significant(c: char) -> bool {
    let n = c as u32;
    SHELL_METACHARS.contains(&c) || c == '\'' || c == '"' || c == '\\' || n < 0x20 || n == 0x7f
}

/// Resolve `arg` against `cwd` for the path haystack: an absolute (`/...`) or
/// tilde (`~...`) arg is left as-is, otherwise it is joined onto `cwd`.
fn resolve_against(cwd: &str, arg: &str) -> String {
    if arg.starts_with('/') || arg.starts_with('~') {
        arg.to_string()
    } else {
        format!("{}/{}", cwd.trim_end_matches('/'), arg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> ShellCommand {
        let owned: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
        ShellCommand::parse(&owned, None)
    }

    #[test]
    fn empty_or_blank_argv_parses_empty() {
        assert!(ShellCommand::parse(&[], None).is_empty());
        assert!(parse(&["   "]).is_empty());
    }

    #[test]
    fn whitespace_packed_single_element_is_word_split() {
        let p = parse(&["rm -rf ~/important"]);
        assert_eq!(p.words, vec!["rm", "-rf", "~/important"]);
        assert_eq!(p.head, "rm");
        assert!(p.has_leading_tilde);
    }

    #[test]
    fn resolves_head_past_env_assignment_prefixes() {
        let cc = parse(&["CC=clang", "make"]);
        assert!(cc.has_assignment_prefix);
        assert_eq!(cc.head, "make");
        assert!(cc.rest.is_empty());

        assert!(parse(&["X+=1", "bash", "deploy.sh"]).has_assignment_prefix);
        assert_eq!(parse(&["X+=1", "bash", "deploy.sh"]).head, "bash");
        assert!(parse(&["9=x", "env"]).has_assignment_prefix); // numeric positional name
        assert_eq!(parse(&["9=x", "env"]).head, "env");
    }

    #[test]
    fn equals_expansion_head_is_flagged_and_stripped() {
        let p = parse(&["=python3", "/tmp/app.py"]);
        assert!(p.has_equals_expansion);
        assert!(!p.has_assignment_prefix);
        assert_eq!(p.head, "python3");
    }

    #[test]
    fn precommand_modifier_head_is_recognized() {
        assert!(parse(&["command", "rm", "-rf", "/"]).is_introducer);
        assert!(parse(&["noglob", "make"]).is_introducer);
        assert!(!parse(&["ls"]).is_introducer);
    }

    #[test]
    fn command_base_strips_quotes_backslash_equals_and_path() {
        assert_eq!(parse(&["'rm'", "-rf", "/"]).head, "rm");
        assert_eq!(parse(&["\\rm", "-rf", "/"]).head, "rm");
        assert_eq!(parse(&["/usr/bin/security"]).head, "security");
        assert!(parse(&["'rm'"]).bases.contains(&"rm".to_string()));
    }

    #[test]
    fn control_bytes_are_stripped_from_bases_but_still_flagged_raw() {
        // A real NUL built at runtime: the shell drops it, desyncing the literal
        // token from the executed command, so the base must see through it while
        // the raw scan flags it.
        let argv = vec![
            "rm\u{0}".to_string(),
            "-rf".to_string(),
            "sandbox".to_string(),
        ];
        let p = ShellCommand::parse(&argv, None);
        assert!(
            p.bases.contains(&"rm".to_string()),
            "the deny-set base sees through the NUL"
        );
        assert!(
            p.has_shell_metachar,
            "the control byte is still flagged on the raw scan"
        );
    }

    #[test]
    fn shell_grammar_facts_are_detected() {
        assert!(parse(&["echo", "a&&b"]).has_shell_metachar);
        assert!(parse(&["echo", "$(whoami)"]).has_shell_metachar);
        assert!(parse(&["echo", "hi", ">", "out"]).has_redirect);
        assert!(parse(&["echo", "hi>out"]).has_redirect);
        assert!(parse(&["ls", "~"]).has_leading_tilde);
        assert!(!parse(&["ls", "notes.txt~"]).has_leading_tilde); // trailing tilde does not expand
        assert!(parse(&["^date^id"]).has_history_expansion);
        assert!(!parse(&["grep", "^needle", "f"]).has_history_expansion); // not line-initial
        assert!(parse(&[":(){:|:&};:"]).is_fork_bomb);
    }

    #[test]
    fn path_haystack_resolves_relative_args_against_cwd_but_skips_flags() {
        let p = ShellCommand::parse(
            &["cat".to_string(), "credentials".to_string()],
            Some("/Users/me/.aws"),
        );
        assert!(p.path_haystack.contains("/Users/me/.aws/credentials"));

        let q = ShellCommand::parse(&["ls".to_string(), "-la".to_string()], Some("/home/x"));
        assert!(
            !q.path_haystack.contains("/home/x/-la"),
            "a flag arg is not resolved as a path"
        );
    }

    #[test]
    fn plain_command_carries_no_shell_grammar_flags() {
        let p = parse(&["git", "status"]);
        assert_eq!(p.head, "git");
        assert_eq!(p.rest, vec!["status"]);
        assert!(!p.has_shell_metachar);
        assert!(!p.has_assignment_prefix);
        assert!(!p.has_redirect);
        assert!(!p.has_leading_tilde);
        assert!(!p.is_fork_bomb);
    }
}
