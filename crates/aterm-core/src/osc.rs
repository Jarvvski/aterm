//! OSC pre-parser: detects OSC 133 (shell-integration prompt marks) and OSC 7
//! (working-directory reports), nonce-gated, and strips aterm's own marks from
//! the byte stream before they reach the VT parser.
//!
//! This runs *ahead* of the alacritty VT engine so we own block segmentation
//! semantics (OSC-133 A/B/C/D) and CWD tracking (OSC-7) rather than depending on
//! the terminal's internal handling. It is intentionally small and pure so it is
//! cheap to unit-test with no PTY.
//!
//! Wire format recap:
//!   OSC = ESC `]` ... ST   where ST = ESC `\` (0x1b 0x5c) or BEL (0x07).
//!   OSC 133 ; A | B | C | D[;<exit>] ...   (prompt / command-start / pre-exec / cmd-done)
//!   OSC 7  ; file://<host><path>            (current working directory)
//!
//! Nonce gating: when aterm injects its own shell shim, it stamps a per-session
//! nonce into the marks it emits (e.g. `OSC 133 ; A ; aterm_nonce=<HEX> ST`). We
//! only trust marks carrying our nonce, and we strip exactly those from the
//! stream so the user's shell prompt never shows our control bytes. Marks from a
//! foreign shell-integration (a different terminal's) are passed through
//! untouched and ignored.

/// OSC-133 prompt-mark kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// `A` — start of prompt.
    PromptStart,
    /// `B` — start of command input (end of prompt).
    CommandStart,
    /// `C` — start of command output (pre-exec → command is now running).
    OutputStart,
    /// `D` — command finished. Carries an optional exit code.
    CommandDone { exit_code: Option<i32> },
}

/// A typed mark recovered from the byte stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    /// OSC 133 prompt/command lifecycle mark.
    Prompt(PromptKind),
    /// OSC 7 working-directory report (decoded path).
    Cwd(String),
}

/// Result of scanning a chunk: the bytes that should continue to the VT parser
/// (our own marks stripped) plus the typed marks we recovered.
#[derive(Debug, Default)]
pub struct ScanResult {
    /// Stream with aterm's nonce-stamped OSC-133 marks removed. Foreign marks and
    /// all other bytes are preserved verbatim.
    pub passthrough: Vec<u8>,
    /// Marks recovered, in stream order.
    pub marks: Vec<Mark>,
}

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

/// Stateless-per-call OSC scanner.
///
/// NOTE: this scans a *single contiguous chunk*. An OSC sequence split across
/// two PTY reads is not yet stitched. TODO(ticket EPIC-2): carry a small partial
/// buffer across `scan` calls so a mark spanning a chunk boundary is still
/// recognized. For the common case (a prompt mark arrives whole) this is fine,
/// and the segmentation tests below exercise the whole-chunk path.
#[derive(Debug, Clone)]
pub struct OscScanner {
    /// Per-session nonce we stamp into our own marks. `None` → trust any
    /// well-formed OSC-133 mark (useful before the shim handshake completes).
    nonce: Option<String>,
}

impl OscScanner {
    /// New scanner that only trusts marks carrying `nonce`.
    pub fn with_nonce(nonce: impl Into<String>) -> Self {
        Self {
            nonce: Some(nonce.into()),
        }
    }

    /// New scanner that trusts any well-formed OSC-133/7 mark (no nonce gate).
    pub fn untrusted() -> Self {
        Self { nonce: None }
    }

    /// Scan one chunk. Strips our nonce-stamped OSC-133 marks; emits typed marks.
    pub fn scan(&self, input: &[u8]) -> ScanResult {
        let mut out = ScanResult {
            passthrough: Vec::with_capacity(input.len()),
            marks: Vec::new(),
        };

        let mut i = 0;
        while i < input.len() {
            // Look for OSC introducer: ESC ']'.
            if input[i] == ESC && i + 1 < input.len() && input[i + 1] == b']' {
                if let Some((body, end, stripped_len)) = read_osc(&input[i..]) {
                    let handled = self.handle_osc(body, &mut out);
                    if handled.is_some() && self.should_strip(body) {
                        // Drop the whole sequence (introducer..terminator) from
                        // the passthrough stream.
                        i += stripped_len;
                        continue;
                    }
                    // Recognized but not ours (or not strippable): pass through
                    // verbatim and advance past it.
                    out.passthrough.extend_from_slice(&input[i..i + end]);
                    i += end;
                    continue;
                }
                // Malformed / truncated OSC: fall through and emit the byte.
            }
            out.passthrough.push(input[i]);
            i += 1;
        }
        out
    }

    /// Should this OSC body be stripped from passthrough? Only our own
    /// nonce-stamped OSC-133 marks are stripped; OSC-7 is informational and left
    /// in place; foreign marks pass through.
    fn should_strip(&self, body: &[u8]) -> bool {
        if !body.starts_with(b"133;") {
            return false;
        }
        match &self.nonce {
            None => true, // untrusted mode: strip any 133 mark we recognized
            Some(n) => contains_nonce(body, n.as_bytes()),
        }
    }

    /// Parse an OSC body into a typed mark and push it if trusted. Returns `Some`
    /// if the body was a recognized OSC-133 or OSC-7 (regardless of trust).
    fn handle_osc(&self, body: &[u8], out: &mut ScanResult) -> Option<()> {
        if let Some(rest) = body.strip_prefix(b"133;") {
            // Trust gate for emitting the mark.
            let trusted = match &self.nonce {
                None => true,
                Some(n) => contains_nonce(rest, n.as_bytes()),
            };
            let kind = parse_133(rest)?;
            if trusted {
                out.marks.push(Mark::Prompt(kind));
            }
            return Some(());
        }
        if let Some(rest) = body.strip_prefix(b"7;") {
            if let Some(path) = parse_osc7_path(rest) {
                out.marks.push(Mark::Cwd(path));
            }
            return Some(());
        }
        None
    }
}

/// Read an OSC starting at `buf[0] == ESC, buf[1] == ']'`.
/// Returns `(body, total_len_including_terminator, stripped_len)`.
/// `body` excludes the `ESC ]` introducer and the ST/BEL terminator.
fn read_osc(buf: &[u8]) -> Option<(&[u8], usize, usize)> {
    // buf[0]=ESC, buf[1]=']'
    let mut j = 2;
    let body_start = j;
    while j < buf.len() {
        match buf[j] {
            BEL => {
                let body = &buf[body_start..j];
                let total = j + 1; // include BEL
                return Some((body, total, total));
            }
            ESC if j + 1 < buf.len() && buf[j + 1] == b'\\' => {
                let body = &buf[body_start..j];
                let total = j + 2; // include ESC '\'
                return Some((body, total, total));
            }
            _ => j += 1,
        }
    }
    None // unterminated within this chunk
}

/// Parse the part of an OSC-133 body after the `133;` prefix.
fn parse_133(rest: &[u8]) -> Option<PromptKind> {
    let first = *rest.first()?;
    match first {
        b'A' => Some(PromptKind::PromptStart),
        b'B' => Some(PromptKind::CommandStart),
        b'C' => Some(PromptKind::OutputStart),
        b'D' => {
            // `D` or `D;<exit>` (exit may be followed by other `;`-params).
            let exit = rest
                .get(1)
                .filter(|&&c| c == b';')
                .and_then(|_| {
                    let tail = &rest[2..];
                    let end = tail.iter().position(|&c| c == b';').unwrap_or(tail.len());
                    std::str::from_utf8(&tail[..end]).ok()
                })
                .and_then(|s| s.trim().parse::<i32>().ok());
            Some(PromptKind::CommandDone { exit_code: exit })
        }
        _ => None,
    }
}

/// Decode an OSC-7 body (`file://host/path`) into a filesystem path.
fn parse_osc7_path(rest: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(rest).ok()?;
    let after_scheme = s.strip_prefix("file://")?;
    // Drop the authority (host) component up to the first '/'.
    let path = match after_scheme.find('/') {
        Some(idx) => &after_scheme[idx..],
        None => after_scheme,
    };
    Some(percent_decode(path))
}

/// Minimal percent-decoding for OSC-7 paths (no external dep).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex_val(bytes[i + 1]);
            let lo = hex_val(bytes[i + 2]);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Does an OSC body carry `aterm_nonce=<nonce>` somewhere in its params?
fn contains_nonce(body: &[u8], nonce: &[u8]) -> bool {
    const KEY: &[u8] = b"aterm_nonce=";
    let needle: Vec<u8> = KEY.iter().chain(nonce.iter()).copied().collect();
    body.windows(needle.len()).any(|w| w == needle.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn osc(body: &str) -> Vec<u8> {
        let mut v = vec![ESC, b']'];
        v.extend_from_slice(body.as_bytes());
        v.push(BEL);
        v
    }

    #[test]
    fn detects_prompt_lifecycle_untrusted() {
        let s = OscScanner::untrusted();
        let mut stream = Vec::new();
        stream.extend(osc("133;A"));
        stream.extend(b"user@host $ ");
        stream.extend(osc("133;B"));
        stream.extend(b"ls -la");
        stream.extend(osc("133;C"));
        stream.extend(b"file1\nfile2\n");
        stream.extend(osc("133;D;0"));

        let r = s.scan(&stream);
        assert_eq!(
            r.marks,
            vec![
                Mark::Prompt(PromptKind::PromptStart),
                Mark::Prompt(PromptKind::CommandStart),
                Mark::Prompt(PromptKind::OutputStart),
                Mark::Prompt(PromptKind::CommandDone { exit_code: Some(0) }),
            ]
        );
        // Untrusted mode strips all recognized 133 marks.
        assert_eq!(r.passthrough, b"user@host $ ls -lafile1\nfile2\n");
    }

    #[test]
    fn parses_nonzero_exit_code() {
        let s = OscScanner::untrusted();
        let r = s.scan(&osc("133;D;127"));
        assert_eq!(
            r.marks,
            vec![Mark::Prompt(PromptKind::CommandDone {
                exit_code: Some(127)
            })]
        );
    }

    #[test]
    fn st_terminator_works() {
        let s = OscScanner::untrusted();
        let mut stream = vec![ESC, b']'];
        stream.extend(b"133;A");
        stream.extend([ESC, b'\\']); // ST
        let r = s.scan(&stream);
        assert_eq!(r.marks, vec![Mark::Prompt(PromptKind::PromptStart)]);
        assert!(r.passthrough.is_empty());
    }

    #[test]
    fn nonce_gating_trusts_only_our_marks() {
        let s = OscScanner::with_nonce("DEADBEEF");
        let mut stream = Vec::new();
        // Foreign mark (no nonce): recognized but NOT trusted, passed through.
        stream.extend(osc("133;A"));
        // Ours (nonce-stamped): trusted + stripped.
        stream.extend(osc("133;B;aterm_nonce=DEADBEEF"));
        let r = s.scan(&stream);
        assert_eq!(r.marks, vec![Mark::Prompt(PromptKind::CommandStart)]);
        // Foreign A passed through verbatim; our B stripped.
        assert_eq!(r.passthrough, osc("133;A"));
    }

    #[test]
    fn parses_osc7_cwd() {
        let s = OscScanner::untrusted();
        let r = s.scan(&osc("7;file://host/Users/me/dev%20dir"));
        assert_eq!(r.marks, vec![Mark::Cwd("/Users/me/dev dir".to_string())]);
        // OSC-7 is informational; left in the passthrough stream.
        assert_eq!(r.passthrough, osc("7;file://host/Users/me/dev%20dir"));
    }

    #[test]
    fn non_osc_bytes_pass_through_untouched() {
        let s = OscScanner::untrusted();
        let data = b"plain output with \x1b[1m SGR \x1b[0m and text";
        let r = s.scan(data);
        assert_eq!(r.passthrough, data);
        assert!(r.marks.is_empty());
    }

    #[test]
    fn truncated_osc_is_emitted_not_swallowed() {
        let s = OscScanner::untrusted();
        // ESC ] 133;A with no terminator in this chunk.
        let data = [ESC, b']', b'1', b'3', b'3', b';', b'A'];
        let r = s.scan(&data);
        // No mark (unterminated), bytes preserved so a later stitch could work.
        assert!(r.marks.is_empty());
        assert_eq!(r.passthrough, data);
    }
}
