//! `Secrets`: the single source of truth for what is sensitive. It feeds BOTH
//! the risk gate (which refuses to auto-approve reads of these paths) and the
//! output sanitizer (which redacts these values from anything shown to a model
//! or rendered). Having one source means the gate and the sanitizer can never
//! drift out of agreement.

/// Canonical deny-set of sensitive filesystem paths (suffix / substring match).
///
/// These are *path fragments*; a command touching any of them is never `Safe`.
/// The list over-approximates on purpose. Paths use `~` to denote the home dir;
/// matching normalizes a leading `$HOME`/absolute-home to `~`.
pub const SENSITIVE_PATHS: &[&str] = &[
    "~/.ssh",
    "~/.aws",
    "~/.aws/credentials",
    "~/.aws/config",
    "~/.gnupg",
    "~/.config/gh/hosts.yml",
    "~/.docker/config.json",
    "~/.kube/config",
    "~/.netrc",
    "~/.npmrc",
    "~/.pypirc",
    "~/.git-credentials",
    "id_rsa",
    "id_ed25519",
    "id_ecdsa",
    ".env",
    ".env.local",
    "credentials",
    "secrets.yaml",
    "secrets.yml",
    ".pem",
    ".key",
    "/etc/shadow",
    "/etc/sudoers",
];

/// The single secrets source. Holds the deny-set plus any concrete secret
/// *values* discovered at runtime (env vars, fetched tokens) that must be
/// redacted from output.
#[derive(Debug, Clone, Default)]
pub struct Secrets {
    /// Concrete secret values to redact verbatim (API keys, tokens, passwords).
    values: Vec<String>,
}

impl Secrets {
    /// Empty secrets source (deny-set is always the static `SENSITIVE_PATHS`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed secret values from the environment. Picks up common credential env
    /// vars by name suffix (`*_TOKEN`, `*_KEY`, `*_SECRET`, `*_PASSWORD`).
    pub fn from_env() -> Self {
        let mut s = Self::new();
        for (k, v) in std::env::vars() {
            if v.len() >= 8 && is_secretish_env_name(&k) {
                s.add_value(v);
            }
        }
        s
    }

    /// Register a concrete secret value to be redacted. Empty/short values are
    /// ignored to avoid redacting noise.
    pub fn add_value(&mut self, value: impl Into<String>) {
        let v = value.into();
        if v.len() >= 6 && !self.values.contains(&v) {
            self.values.push(v);
        }
    }

    /// All registered secret values, longest first (so a longer secret is
    /// redacted before a shorter substring of it).
    pub fn values(&self) -> Vec<&str> {
        let mut refs: Vec<&str> = self.values.iter().map(String::as_str).collect();
        refs.sort_by_key(|s| std::cmp::Reverse(s.len()));
        refs
    }

    /// Does `path_fragment` reference any sensitive path? Used by the risk gate.
    ///
    /// A token is sensitive if it *contains* a sensitive path fragment (e.g.
    /// `~/.ssh/id_rsa` contains `~/.ssh`) or its basename matches a sensitive
    /// filename (e.g. `creds.pem` ends with `.pem`). We never treat a sensitive
    /// path as "contained in" a short token — that produced false positives like
    /// `ls` ⊂ `credentials`.
    pub fn is_sensitive_path(path_fragment: &str) -> bool {
        let norm = normalize_home(path_fragment);
        if norm.is_empty() {
            return false;
        }
        let basename = norm.rsplit('/').next().unwrap_or(&norm);
        SENSITIVE_PATHS.iter().any(|p| {
            let pn = normalize_home(p);
            if pn.contains('/') || pn.starts_with('~') {
                // Path-shaped fragment: the token must contain it.
                norm.contains(&pn)
            } else if let Some(ext) = pn.strip_prefix('.') {
                // Extension/dotfile fragment (.pem, .key, .env): match the
                // basename as a suffix or exact dotfile name.
                let _ = ext;
                basename.ends_with(&pn) || basename == pn
            } else {
                // Bare filename fragment (credentials, id_rsa): match the
                // basename exactly, not as a loose substring.
                basename == pn
            }
        })
    }

    /// Does ANY token in `argv` reference a sensitive path?
    pub fn argv_touches_secret(argv: &[String]) -> bool {
        argv.iter().any(|t| Self::is_sensitive_path(t))
    }
}

/// Normalize `$HOME` / `/Users/<u>` / `/home/<u>` prefixes to `~` for matching.
fn normalize_home(p: &str) -> String {
    let p = p.trim();
    if let Some(rest) = p.strip_prefix("$HOME") {
        return format!("~{rest}");
    }
    if let Ok(home) = std::env::var("HOME") {
        if let Some(rest) = p.strip_prefix(&home) {
            return format!("~{rest}");
        }
    }
    p.to_string()
}

fn is_secretish_env_name(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.ends_with("_TOKEN")
        || n.ends_with("_KEY")
        || n.ends_with("_SECRET")
        || n.ends_with("_PASSWORD")
        || n.ends_with("_APIKEY")
        || n.contains("API_KEY")
        || n.contains("ACCESS_KEY")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_path_is_sensitive() {
        assert!(Secrets::is_sensitive_path("~/.ssh/id_rsa"));
        // Absolute path whose basename is a known credential filename.
        assert!(Secrets::is_sensitive_path("/Users/me/.ssh/id_rsa"));
        // Path containing the ~/.aws fragment after $HOME normalization.
        assert!(Secrets::is_sensitive_path("$HOME/.aws/credentials"));
        assert!(Secrets::is_sensitive_path(".env"));
        assert!(Secrets::is_sensitive_path("project/.env.local"));
    }

    #[test]
    fn plain_path_not_sensitive() {
        assert!(!Secrets::is_sensitive_path("/tmp/output.txt"));
        assert!(!Secrets::is_sensitive_path("src/main.rs"));
    }

    #[test]
    fn argv_touches_secret_scans_all_tokens() {
        let argv = vec!["cat".to_string(), "~/.aws/credentials".to_string()];
        assert!(Secrets::argv_touches_secret(&argv));
        let safe = vec!["cat".to_string(), "README.md".to_string()];
        assert!(!Secrets::argv_touches_secret(&safe));
    }

    #[test]
    fn values_sorted_longest_first() {
        let mut s = Secrets::new();
        s.add_value("short1");
        s.add_value("a-much-longer-secret-value");
        let v = s.values();
        assert_eq!(v[0], "a-much-longer-secret-value");
    }

    #[test]
    fn short_values_ignored() {
        let mut s = Secrets::new();
        s.add_value("abc"); // < 6 chars
        assert!(s.values().is_empty());
    }
}
