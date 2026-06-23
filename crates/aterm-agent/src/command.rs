//! `ShellCommand`: a zsh-aware argv parse used by the risk gate.
//!
//! This is intentionally conservative. The risk classifier must over-approximate
//! danger, so the parse exists to (a) resolve the *program* (head) of a command
//! and (b) surface shell-active structure (pipes, redirects, chaining, env
//! assignments, metacharacters) rather than to faithfully emulate zsh.
//!
//! A key subtlety: a model (or a paste) may hand us a single argv element that
//! is itself a whole command line, e.g. `["rm -rf ~"]`. We re-split each element
//! on whitespace so the program reads as `rm`, never the literal `rm -rf ~`.

/// Shell-active structure detected while parsing. Any of these means the input is
/// NOT a plain program invocation and must not be auto-approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShellStructure {
    pub has_pipe: bool,
    pub has_redirect: bool,
    /// `&&`, `||`, `;` command chaining.
    pub has_chaining: bool,
    /// `$(...)` or backtick command substitution.
    pub has_substitution: bool,
    /// `VAR=value cmd` env-assignment prefix.
    pub has_env_assignment: bool,
    /// Background `&`.
    pub has_background: bool,
    /// Glob / brace / tilde expansion characters present.
    pub has_expansion: bool,
    /// Any other shell metacharacter (quotes are fine; these are not).
    pub has_metachar: bool,
}

impl ShellStructure {
    /// True if ANY shell-active structure was detected.
    pub fn is_shell_active(&self) -> bool {
        self.has_pipe
            || self.has_redirect
            || self.has_chaining
            || self.has_substitution
            || self.has_env_assignment
            || self.has_background
            || self.has_expansion
            || self.has_metachar
    }
}

/// A parsed shell command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCommand {
    /// The original input line, verbatim.
    pub raw: String,
    /// Resolved program (head), e.g. `rm`. Empty if the line was empty.
    pub program: String,
    /// All whitespace-resplit tokens including the program at index 0.
    pub argv: Vec<String>,
    /// Detected shell-active structure.
    pub structure: ShellStructure,
}

impl ShellCommand {
    /// Parse a command line, given either a single raw string or a pre-split argv.
    /// Each element is re-split on whitespace, so `["rm -rf ~"]` and `"rm -rf ~"`
    /// parse identically.
    pub fn parse(input: &str) -> Self {
        Self::parse_argv(&[input.to_string()])
    }

    /// Parse from an argv where any element may itself contain whitespace.
    pub fn parse_argv(elements: &[String]) -> Self {
        let raw = elements.join(" ");

        // Whitespace-resplit every element so a single "rm -rf ~" element becomes
        // multiple tokens.
        let mut tokens: Vec<String> = Vec::new();
        for el in elements {
            for tok in el.split_whitespace() {
                tokens.push(tok.to_string());
            }
        }

        let structure = detect_structure(&raw);

        // Program resolution: skip leading env-assignment tokens (VAR=val), then
        // take the first token as the program head.
        let program = tokens
            .iter()
            .find(|t| !is_env_assignment(t))
            .cloned()
            .unwrap_or_default();

        ShellCommand {
            raw,
            program,
            argv: tokens,
            structure,
        }
    }
}

/// Is this token a `VAR=value` env-assignment prefix? (Identifier `=` ...)
fn is_env_assignment(tok: &str) -> bool {
    match tok.find('=') {
        Some(0) | None => false,
        Some(idx) => {
            let name = &tok[..idx];
            !name.is_empty()
                && name
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_alphabetic() || c == '_')
                    .unwrap_or(false)
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
    }
}

/// Scan the raw line for shell-active metacharacters. Quotes are tracked so a
/// metacharacter *inside* a quoted string is not counted as structure, but we
/// still flag the quote-spanning danger conservatively where ambiguous.
fn detect_structure(raw: &str) -> ShellStructure {
    let mut s = ShellStructure::default();
    let bytes = raw.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;

    while i < bytes.len() {
        let c = bytes[i] as char;
        let next = bytes.get(i + 1).map(|&b| b as char);

        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            _ if in_single || in_double => {
                // Inside quotes: only command substitution in double quotes is
                // still active.
                if in_double && (c == '$' && next == Some('(')) {
                    s.has_substitution = true;
                }
                if in_double && c == '`' {
                    s.has_substitution = true;
                }
            }
            '|' => {
                s.has_pipe = true;
                if next == Some('|') {
                    s.has_chaining = true;
                    i += 1;
                }
            }
            '&' => {
                if next == Some('&') {
                    s.has_chaining = true;
                    i += 1;
                } else {
                    s.has_background = true;
                }
            }
            ';' => s.has_chaining = true,
            '>' | '<' => s.has_redirect = true,
            '`' => s.has_substitution = true,
            '$' if next == Some('(') => s.has_substitution = true,
            '~' => s.has_expansion = true,
            '*' | '?' | '{' | '}' | '[' | ']' => s.has_expansion = true,
            '(' | ')' | '!' | '#' => s.has_metachar = true,
            _ => {}
        }
        i += 1;
    }

    // Env-assignment prefix: a leading token of the form VAR=val.
    if let Some(first) = raw.split_whitespace().next() {
        if is_env_assignment(first) {
            s.has_env_assignment = true;
        }
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_element_command_line_resplits() {
        let cmd = ShellCommand::parse_argv(&["rm -rf ~".to_string()]);
        assert_eq!(cmd.program, "rm");
        assert_eq!(cmd.argv, vec!["rm", "-rf", "~"]);
        assert!(cmd.structure.has_expansion); // the ~
    }

    #[test]
    fn plain_program() {
        let cmd = ShellCommand::parse("ls -la /tmp");
        assert_eq!(cmd.program, "ls");
        assert_eq!(cmd.argv, vec!["ls", "-la", "/tmp"]);
        assert!(!cmd.structure.is_shell_active());
    }

    #[test]
    fn pipe_is_shell_active() {
        let cmd = ShellCommand::parse("cat f | grep x");
        assert!(cmd.structure.has_pipe);
        assert!(cmd.structure.is_shell_active());
    }

    #[test]
    fn chaining_detected() {
        assert!(ShellCommand::parse("a && b").structure.has_chaining);
        assert!(ShellCommand::parse("a || b").structure.has_chaining);
        assert!(ShellCommand::parse("a ; b").structure.has_chaining);
    }

    #[test]
    fn redirect_detected() {
        assert!(
            ShellCommand::parse("echo hi > /etc/x")
                .structure
                .has_redirect
        );
    }

    #[test]
    fn substitution_detected() {
        assert!(
            ShellCommand::parse("echo $(whoami)")
                .structure
                .has_substitution
        );
        assert!(ShellCommand::parse("echo `id`").structure.has_substitution);
    }

    #[test]
    fn env_assignment_prefix() {
        let cmd = ShellCommand::parse("FOO=bar make");
        assert!(cmd.structure.has_env_assignment);
        // program resolves past the assignment
        assert_eq!(cmd.program, "make");
    }

    #[test]
    fn background_detected() {
        assert!(ShellCommand::parse("server &").structure.has_background);
    }

    #[test]
    fn empty_input() {
        let cmd = ShellCommand::parse("");
        assert_eq!(cmd.program, "");
        assert!(cmd.argv.is_empty());
    }
}
