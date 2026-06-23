//! `OutputSanitizer`: redacts secret VALUES from text before it is shown to a
//! model or rendered. Two correctness requirements drive the design:
//!
//!   1. Redact BEFORE truncation. If we truncated first, a secret straddling the
//!      truncation boundary could leak its tail. So redaction always runs on the
//!      full text, and truncation (if any) runs after.
//!
//!   2. Tolerate a soft-wrap `\n` inside a secret. A terminal may have wrapped a
//!      long token across a line, inserting a newline mid-secret. We match
//!      secrets ignoring interior whitespace so a wrapped token is still caught.

use crate::secrets::Secrets;

/// Replacement token written in place of a redacted secret.
pub const REDACTION: &str = "[REDACTED]";

/// Sanitizes text using a [`Secrets`] source.
pub struct OutputSanitizer<'a> {
    secrets: &'a Secrets,
}

impl<'a> OutputSanitizer<'a> {
    pub fn new(secrets: &'a Secrets) -> Self {
        Self { secrets }
    }

    /// Redact all known secret values from `text`, then optionally truncate to
    /// `max_len` bytes (on a char boundary). Redaction always happens first.
    pub fn sanitize(&self, text: &str, max_len: Option<usize>) -> String {
        let redacted = self.redact(text);
        match max_len {
            Some(n) if redacted.len() > n => truncate_on_char_boundary(&redacted, n),
            _ => redacted,
        }
    }

    /// Redact secret values from `text`. Matches each secret both verbatim and
    /// with interior whitespace collapsed, so a soft-wrapped secret is caught.
    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_string();
        for secret in self.secrets.values() {
            if secret.is_empty() {
                continue;
            }
            // Verbatim pass.
            out = out.replace(secret, REDACTION);
            // Soft-wrap-tolerant pass: if the secret contains no whitespace, also
            // catch occurrences where a single `\n` (and surrounding spaces) was
            // injected between characters.
            if !secret.chars().any(char::is_whitespace) {
                out = redact_softwrapped(&out, secret);
            }
        }
        out
    }
}

/// Replace occurrences of `secret` where interior soft-wrap whitespace
/// (`\n`, `\r`, spaces) was injected between its characters.
fn redact_softwrapped(text: &str, secret: &str) -> String {
    let secret_chars: Vec<char> = secret.chars().collect();
    if secret_chars.is_empty() {
        return text.to_string();
    }
    let text_chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;

    while i < text_chars.len() {
        if let Some(consumed) = match_softwrapped(&text_chars[i..], &secret_chars) {
            out.push_str(REDACTION);
            i += consumed;
        } else {
            out.push(text_chars[i]);
            i += 1;
        }
    }
    out
}

/// Try to match `secret` at the start of `hay`, allowing interior runs of
/// soft-wrap whitespace (newline/CR/space) between secret chars. Returns the
/// number of `hay` chars consumed on success.
fn match_softwrapped(hay: &[char], secret: &[char]) -> Option<usize> {
    let mut hi = 0;
    for (si, &sc) in secret.iter().enumerate() {
        // Between secret chars (not before the first), skip soft-wrap whitespace.
        if si > 0 {
            while hi < hay.len() && is_softwrap_ws(hay[hi]) {
                hi += 1;
            }
        }
        if hi >= hay.len() || hay[hi] != sc {
            return None;
        }
        hi += 1;
    }
    Some(hi)
}

fn is_softwrap_ws(c: char) -> bool {
    c == '\n' || c == '\r' || c == ' '
}

fn truncate_on_char_boundary(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secrets_with(vals: &[&str]) -> Secrets {
        let mut s = Secrets::new();
        for v in vals {
            s.add_value(*v);
        }
        s
    }

    #[test]
    fn redacts_verbatim_secret() {
        let s = secrets_with(&["sk-supersecret-token-123"]);
        let san = OutputSanitizer::new(&s);
        let out = san.redact("token is sk-supersecret-token-123 ok");
        assert_eq!(out, "token is [REDACTED] ok");
    }

    #[test]
    fn redacts_before_truncation() {
        // Secret straddles the would-be truncation point; redaction-first means
        // it cannot leak its tail.
        let s = secrets_with(&["ABCDEFGHIJ"]);
        let san = OutputSanitizer::new(&s);
        // "pre " (4) + secret; truncate at 10 bytes.
        let out = san.sanitize("pre ABCDEFGHIJ tail", Some(10));
        assert!(!out.contains("GHIJ"));
        assert!(out.starts_with("pre [REDA")); // truncated AFTER redaction
    }

    #[test]
    fn tolerates_softwrap_newline_in_secret() {
        let s = secrets_with(&["ABCDEFGHIJKL"]);
        let san = OutputSanitizer::new(&s);
        // The terminal wrapped the token: "ABCDEF\nGHIJKL".
        let out = san.redact("key=ABCDEF\nGHIJKL end");
        assert_eq!(out, "key=[REDACTED] end");
    }

    #[test]
    fn tolerates_softwrap_with_spaces() {
        let s = secrets_with(&["TOKEN12345"]);
        let san = OutputSanitizer::new(&s);
        let out = san.redact("v=TOKEN\n 12345!");
        assert_eq!(out, "v=[REDACTED]!");
    }

    #[test]
    fn no_secrets_passes_through() {
        let s = Secrets::new();
        let san = OutputSanitizer::new(&s);
        assert_eq!(san.redact("nothing to hide"), "nothing to hide");
    }

    #[test]
    fn multiple_secrets_redacted() {
        let s = secrets_with(&["first-secret", "second-secret"]);
        let san = OutputSanitizer::new(&s);
        let out = san.redact("a first-secret b second-secret c");
        assert_eq!(out, "a [REDACTED] b [REDACTED] c");
    }
}
