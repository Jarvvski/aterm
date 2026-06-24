//! `Secrets`: the single source of truth for what is sensitive. ONE instance
//! feeds BOTH the risk gate (which refuses to auto-approve reads of the
//! deny-set paths) and the [`crate::sanitizer::OutputSanitizer`] (which redacts
//! the secret *values* from anything shown to a model or rendered).
//!
//! The single-source invariant is the load-bearing structural property: the
//! gate borrows the same `Secrets` the sanitizer borrows, so adding a sensitive
//! path or a secret value is reflected by both defenses at once - they cannot
//! drift out of agreement. Concretely, both the deny-set (`sensitive_paths`)
//! and the secret values live as instance state on this one struct; nothing
//! consults a private copy.
//!
//! Path matching is **case-insensitive substring** (a deny-set entry is a
//! pattern matched against a command token / resolved arg path) - the SAME rule
//! the prototype's `Secrets.isSensitivePath` applies, so a UI-driven direct read
//! refuses exactly the paths a `cat <path>` proposal is flagged for. The match
//! over-approximates toward flagging: a false positive costs one extra
//! confirmation prompt, while a miss could leak a key.

/// Default deny-set of sensitive path *substring patterns* used to seed every
/// [`Secrets`] (matched case-insensitively against each command token / arg).
///
/// Credential files/dirs and the env-var NAMES a shell command could dump
/// (`printenv ANTHROPIC_API_KEY`); the values themselves are redacted via the
/// sanitizer, not matched here. The list over-approximates on purpose. This is
/// only the *seed* - the authoritative deny-set is the per-instance
/// `sensitive_paths`, which callers may extend at runtime (e.g. aterm's own
/// config's absolute path once resolved).
pub const SENSITIVE_PATHS: &[&str] = &[
    // SSH / GPG private-key material and known-hosts trust.
    ".ssh/",
    "id_rsa",
    "id_ed25519",
    "id_ecdsa",
    "authorized_keys",
    "/root/.ssh",
    ".gnupg",
    // Cloud / package / registry credentials. Whole dirs (trailing `/`) so a
    // read of anything under them - `.aws/sso/cache/*.json`, `ls ~/.aws` - is
    // still caught.
    ".aws/",
    ".netrc",
    ".git-credentials",
    ".npmrc",
    ".pypirc",
    ".pgpass",
    ".docker/config.json",
    ".kube/config",
    "gh/hosts.yml",
    ".config/gcloud",
    ".terraform.d/credentials",
    "login.keychain",
    "/Keychains/",
    // Env files + the agent provider key env-var NAMES, so a command that names
    // one as an argument (`printenv ANTHROPIC_API_KEY`) is flagged; their VALUES
    // are redacted via the sanitizer. NOTE: the bare-dump verb form (`env` /
    // `printenv` with no argument) cannot be caught by a per-token path pattern -
    // that is the classifier's env-dump head rule (T-5.5).
    ".env",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    // Generic key/cert/credential file shapes. The leading-dot patterns keep
    // these from matching bare words (`monkey` has no `.key`); the bare
    // `credentials` / `secrets.yaml` shapes are ubiquitous deploy/CI secret
    // files, matched as substrings (over-approximating toward a prompt).
    ".pem",
    ".key",
    "credentials",
    "secrets.yaml",
    "secrets.yml",
    // Shell startup files + other high-trust write targets: overwriting any of
    // these is privilege-relevant, so a read/write of them is flagged.
    ".zshrc",
    ".zprofile",
    ".bashrc",
    ".bash_profile",
    "sudoers",
    "crontab",
    "/etc/shadow",
    // Remote / orchestration credential locations the agent can reach over SSH:
    // the k8s service-account token mount, a Vault token file, and the cloud
    // metadata endpoint (AWS/GCP IMDS at this link-local IP serves instance
    // credentials). Substring patterns, so any URL/path embedding them is caught.
    "/var/run/secrets/kubernetes.io/serviceaccount",
    "vault-token",
    "169.254.169.254",
    // aterm's own config holds the API key in plaintext, so reading it is
    // exfiltration. This relative fragment matches every resolved location
    // (~/.config/aterm/config.toml, ~/.aterm/config.toml, $XDG_CONFIG_HOME/...).
    "aterm/config.toml",
];

/// The single secrets source. Holds the sensitive-path deny-set AND any concrete
/// secret *values* discovered at runtime (env vars, fetched tokens). The gate
/// borrows it for path classification; the sanitizer borrows it for value
/// redaction - one instance, so the two defenses can never drift.
#[derive(Debug, Clone)]
pub struct Secrets {
    /// Sensitive path substring patterns. Seeded from [`SENSITIVE_PATHS`]; extendable.
    sensitive_paths: Vec<String>,
    /// Concrete secret values to redact verbatim (API keys, tokens, passwords).
    values: Vec<String>,
}

impl Default for Secrets {
    fn default() -> Self {
        Self {
            sensitive_paths: SENSITIVE_PATHS.iter().map(|p| (*p).to_string()).collect(),
            values: Vec::new(),
        }
    }
}

impl Secrets {
    /// Secrets source seeded with the default [`SENSITIVE_PATHS`] deny-set and no
    /// concrete values yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed secret values from the environment. Picks up common credential env
    /// vars by name (`*_TOKEN`, `*_KEY`, `*_SECRET`, `*_PASSWORD`, `*API_KEY*`).
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

    /// Extend the deny-set with another sensitive path pattern (e.g. aterm's
    /// own config file's resolved absolute path). Because the gate borrows this
    /// same instance, the addition takes effect immediately for classification
    /// too - the single-source guarantee.
    pub fn add_sensitive_path(&mut self, fragment: impl Into<String>) {
        let f = fragment.into();
        let f = f.trim();
        if !f.is_empty() && !self.sensitive_paths.iter().any(|p| p == f) {
            self.sensitive_paths.push(f.to_string());
        }
    }

    /// All registered secret values, longest first (so a longer secret is
    /// redacted before a shorter substring of it).
    pub fn values(&self) -> Vec<&str> {
        let mut refs: Vec<&str> = self.values.iter().map(String::as_str).collect();
        refs.sort_by_key(|s| std::cmp::Reverse(s.len()));
        refs
    }

    /// The current sensitive-path deny-set (read-only view).
    pub fn sensitive_paths(&self) -> &[String] {
        &self.sensitive_paths
    }

    /// Does `path_fragment` hit this instance's deny-set? Used by the risk gate.
    /// Pure case-insensitive **substring** match (`token.contains(pattern)`) -
    /// the same rule the prototype applies, so a credential path reached through
    /// any wrapper (a URL embedding the IMDS IP, an absolute `/root/.ssh/...`, a
    /// case-munged `~/.SSH/id_rsa` on a case-insensitive macOS volume) is caught.
    /// Over-approximates toward flagging; never reads the environment (pure).
    pub fn is_sensitive_path(&self, path_fragment: &str) -> bool {
        let haystack = path_fragment.to_ascii_lowercase();
        if haystack.is_empty() {
            return false;
        }
        self.sensitive_paths.iter().any(|p| {
            let pn = p.to_ascii_lowercase();
            !pn.is_empty() && haystack.contains(&pn)
        })
    }

    /// Does ANY token in `argv` reference a sensitive path?
    pub fn argv_touches_secret(&self, argv: &[String]) -> bool {
        argv.iter().any(|t| self.is_sensitive_path(t))
    }
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
    fn ssh_aws_env_paths_are_sensitive() {
        let s = Secrets::new();
        assert!(s.is_sensitive_path("~/.ssh/id_rsa"));
        assert!(s.is_sensitive_path("/Users/me/.ssh/id_rsa"));
        assert!(s.is_sensitive_path("$HOME/.aws/credentials"));
        assert!(s.is_sensitive_path(".env"));
        assert!(s.is_sensitive_path("project/.env.local"));
        assert!(s.is_sensitive_path("/root/.ssh/authorized_keys"));
    }

    #[test]
    fn benign_tokens_not_sensitive() {
        // Substring matching must not over-fire on ordinary command tokens, or
        // AUTO-SAFE would never trigger. The leading-dot patterns (`.key`,
        // `.env`) in particular must not match bare words.
        let s = Secrets::new();
        assert!(!s.is_sensitive_path("/tmp/output.txt"));
        assert!(!s.is_sensitive_path("src/main.rs"));
        assert!(!s.is_sensitive_path("ls"));
        assert!(!s.is_sensitive_path("README.md"));
        assert!(!s.is_sensitive_path("git"));
        assert!(!s.is_sensitive_path("monkey")); // contains "key" but not ".key"
    }

    #[test]
    fn sensitive_path_match_is_case_insensitive() {
        // AC4: macOS FS is case-insensitive, so a case-munged path to a
        // credential file must still be classified sensitive. These inputs
        // exercise the path-shaped (`.ssh/`, `.aws/`), dotfile/ext (`.env`,
        // `.pem`) AND bare-filename (`id_rsa`, `authorized_keys`) shapes - the
        // last two reach the match ONLY through their own pattern, so they pin
        // the lowercasing for every shape, not just the path-prefixed ones.
        let s = Secrets::new();
        assert!(s.is_sensitive_path("~/.SSH/id_rsa"));
        assert!(s.is_sensitive_path("project/.ENV"));
        assert!(s.is_sensitive_path("MyKey.PEM"));
        assert!(s.is_sensitive_path("$HOME/.AWS/credentials"));
        assert!(s.is_sensitive_path("ID_RSA"));
        assert!(s.is_sensitive_path("dir/AUTHORIZED_KEYS"));
        // A non-credential path stays non-sensitive regardless of case.
        assert!(!s.is_sensitive_path("SRC/MAIN.RS"));
    }

    #[test]
    fn cloud_and_k8s_metadata_endpoints_are_sensitive() {
        // The IMDS IP and k8s SA mount are SUBSTRING patterns: a realistic
        // credential-exfil command embeds them in a URL / longer path, so the
        // match must fire on the embedding, not only the bare literal.
        let s = Secrets::new();
        assert!(s.is_sensitive_path(
            "http://169.254.169.254/latest/meta-data/iam/security-credentials/"
        ));
        assert!(s.is_sensitive_path("169.254.169.254:80"));
        assert!(s.is_sensitive_path("/var/run/secrets/kubernetes.io/serviceaccount/token"));
    }

    #[test]
    fn provider_key_env_names_and_aterm_config_are_sensitive() {
        // The deny-set carries env-var NAMES so the gate flags a command that
        // would dump them, plus aterm's own key-bearing config file.
        let s = Secrets::new();
        assert!(s.is_sensitive_path("ANTHROPIC_API_KEY"));
        assert!(s.is_sensitive_path("$OPENAI_API_KEY"));
        assert!(s.is_sensitive_path("~/.config/aterm/config.toml"));
    }

    #[test]
    fn bare_deploy_credential_files_are_sensitive() {
        // Standalone CI/deploy secret files (no credential-dir prefix). These
        // were present in the pre-port deny-set; the prototype lacks them, so
        // they are re-added explicitly as substring patterns - a read of one
        // must never silently auto-run.
        let s = Secrets::new();
        assert!(s.is_sensitive_path("secrets.yaml"));
        assert!(s.is_sensitive_path("secrets.yml"));
        assert!(s.is_sensitive_path("credentials"));
        assert!(s.is_sensitive_path("deploy/credentials"));
    }

    #[test]
    fn argv_touches_secret_scans_all_tokens() {
        let s = Secrets::new();
        let argv = vec!["cat".to_string(), "~/.aws/credentials".to_string()];
        assert!(s.argv_touches_secret(&argv));
        let safe = vec!["cat".to_string(), "README.md".to_string()];
        assert!(!s.argv_touches_secret(&safe));
    }

    #[test]
    fn added_sensitive_path_is_classified() {
        // The deny-set is per-instance and extendable; an added fragment is
        // honored immediately (the runtime half of the single-source property).
        let mut s = Secrets::new();
        assert!(!s.is_sensitive_path("vault-keys"));
        s.add_sensitive_path("vault-keys");
        assert!(s.is_sensitive_path("vault-keys"));
        // Default deny-set is still present alongside the addition.
        assert!(s.is_sensitive_path("~/.ssh/id_rsa"));
    }

    #[test]
    fn default_seeds_the_static_deny_set() {
        let s = Secrets::new();
        assert_eq!(s.sensitive_paths().len(), SENSITIVE_PATHS.len());
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
