//! OSC pre-parser filter: intercepts shell-integration marks (OSC 133, plus VS
//! Code's OSC 633 and OSC 7 working-directory reports) *before* they reach the
//! `alacritty_terminal` VT parser, strips aterm's own marks to zero width, tags
//! each surviving mark with an offset into the clean passthrough stream, and gates
//! trust on a per-session nonce (ticket T-2.1).
//!
//! It runs ahead of the VT engine because alacritty does NOT parse OSC 133 (see
//! alacritty issue #5850), so this filter is load-bearing for block segmentation.
//! It is a small, pure, *stateful* byte scanner: stateful so a sequence split
//! across two PTY reads is stitched (`scan` carries a bounded partial buffer), and
//! pure so it is cheap to unit-test with no PTY (see `03-pty-vt-rust.md` section D
//! and `04-shell-integration.md`).
//!
//! Wire format recap:
//!   OSC = ESC `]` ... ST   where ST = ESC `\` (0x1b 0x5c) or BEL (0x07).
//!   OSC 133 ; A | B | C[;cmdline=ENC] | D[;<exit>]   (prompt lifecycle)
//!   OSC 7   ; file://<host><path>                     (working directory)
//!   OSC 633 ; A|B|C|D[;<exit>] | E;<escaped-cmd>[;n] | P;Cwd=<path>  (VS Code)
//!   OSC 1337 ; ShellIntegrationVersion=...            (telemetry only)
//!
//! Nonce gating: aterm's injected shim (ticket T-2.2) stamps a per-session random
//! nonce into the marks it emits as `aterm_nonce=<HEX>`. In nonce mode we trust and
//! strip only marks carrying that nonce; un-nonced/foreign marks (e.g. starship or
//! p10k double-marks, or a malicious program in command output emitting a fake
//! `133;D` to spoof a block boundary) are dropped - not emitted, not trusted. This
//! is a layer of the prompt-injection defense. Before the shim handshake completes
//! the scanner runs in *untrusted* mode and opportunistically trusts any
//! well-formed mark (so blocks work pre-handshake).

use std::borrow::Cow;

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
    /// OSC 133 / OSC 633 prompt/command lifecycle mark.
    Prompt(PromptKind),
    /// OSC 7 / OSC 633 `P;Cwd=` working-directory report (decoded path).
    Cwd(String),
    /// An explicitly reported command line (OSC 133 `C;cmdline=` or OSC 633 `E`),
    /// decoded. The block lifecycle (ticket T-2.5) captures this into the block;
    /// this filter only detects and decodes it.
    CommandLine(String),
    /// The shell's self-reported version string, carried on the first prompt's `A`
    /// mark as `aterm_ver=<version>` (ticket T-2.3 AC2 / T-2.6): bash `$BASH_VERSION`,
    /// zsh `$ZSH_VERSION`, fish `$version`. Nonce-gated (only our shim emits it) and
    /// length-bounded + sanitized to a version-like token, so the engine can surface
    /// "bash 3.2 - upgrade for reliable blocks" to the integration indicator.
    ShellVersion(String),
}

/// Result of scanning a chunk: the bytes that continue to the VT parser (aterm's
/// own marks stripped) plus the typed marks, each tagged with its byte offset into
/// the *cumulative clean stream* (i.e. the logical output the emulator sees). The
/// block state machine (T-2.5) fires a mark once the grid has drained to its
/// offset, keeping marks in lockstep with the grid.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ScanResult {
    /// Stream with aterm's nonce-stamped block marks removed. Foreign marks, OSC 7,
    /// OSC 1337, and all other bytes are preserved verbatim.
    pub passthrough: Vec<u8>,
    /// Recovered marks as `(clean_stream_offset, mark)`, in stream order.
    pub marks: Vec<(usize, Mark)>,
}

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

/// Upper bound on a buffered, not-yet-terminated OSC sequence. A sequence longer
/// than this is treated as not-a-mark and flushed to the passthrough (alacritty
/// bounds its own OSC buffer), so a stray `ESC ]` cannot make us swallow the
/// stream unboundedly. Generous vs real marks (a long `cmdline` is still well
/// under this); a tuning knob, not a protocol constant.
const MAX_OSC_LEN: usize = 8192;

/// Stateful OSC scanner.
///
/// `scan` carries a small partial buffer so an OSC sequence split across two PTY
/// reads is stitched, and a running clean-stream position so each mark's offset is
/// absolute (cumulative across calls). Construct once per session and feed every
/// chunk through the same instance.
#[derive(Debug, Clone)]
pub struct OscScanner {
    /// Per-session nonce we trust. `None` → untrusted mode: trust any well-formed
    /// mark (used before the shim handshake; the engine starts here until T-2.2).
    nonce: Option<String>,
    /// Tail of the previous chunk that ended mid-OSC, prepended to the next chunk.
    partial: Vec<u8>,
    /// Total clean (passthrough) bytes emitted across all `scan` calls so far.
    clean_pos: usize,
}

impl OscScanner {
    /// New scanner that trusts only marks carrying `nonce` (`aterm_nonce=<nonce>`).
    pub fn with_nonce(nonce: impl Into<String>) -> Self {
        Self {
            nonce: Some(nonce.into()),
            partial: Vec::new(),
            clean_pos: 0,
        }
    }

    /// New scanner that trusts any well-formed OSC-133/633/7 mark (no nonce gate).
    pub fn untrusted() -> Self {
        Self {
            nonce: None,
            partial: Vec::new(),
            clean_pos: 0,
        }
    }

    /// Scan one chunk. Stitches any sequence split from the previous chunk, strips
    /// our trusted block marks, and emits typed marks at their clean-stream offset.
    pub fn scan(&mut self, input: &[u8]) -> ScanResult {
        // Prepend any buffered partial OSC from the previous chunk. Zero-copy in
        // the common (no split) case.
        let combined: Cow<[u8]> = if self.partial.is_empty() {
            Cow::Borrowed(input)
        } else {
            let mut v = std::mem::take(&mut self.partial);
            v.extend_from_slice(input);
            Cow::Owned(v)
        };
        let buf: &[u8] = &combined;

        let mut out = ScanResult {
            passthrough: Vec::with_capacity(buf.len()),
            marks: Vec::new(),
        };

        let mut i = 0;
        while i < buf.len() {
            if buf[i] != ESC {
                out.passthrough.push(buf[i]);
                i += 1;
                continue;
            }
            // buf[i] == ESC.
            match buf.get(i + 1) {
                None => {
                    // Trailing lone ESC: it may begin `ESC ]` in the next chunk
                    // (or `ESC [` SGR, which will simply pass through). Buffer it.
                    self.partial = vec![ESC];
                    break;
                }
                Some(&b']') => match read_osc(&buf[i..]) {
                    OscRead::Done { body, consumed } => {
                        let offset = self.clean_pos + out.passthrough.len();
                        let recognized = self.handle_osc(body, offset, &mut out);
                        if !(recognized && self.should_strip(body)) {
                            out.passthrough.extend_from_slice(&buf[i..i + consumed]);
                        }
                        i += consumed;
                    }
                    OscRead::Aborted { at } => {
                        // A fresh ESC aborted this OSC. Discard the malformed,
                        // unterminated prefix (`buf[i..i+at]`): an aborted control
                        // string renders nothing, and feeding a dangling OSC
                        // introducer to the VT parser would wedge it mid-OSC and
                        // swallow later output. Re-anchor at the embedded ESC so a
                        // fresh `ESC ]` is parsed as its OWN body - this prevents an
                        // unterminated OSC in untrusted output from absorbing the
                        // shell's next genuine (nonced) mark. `at >= 2`, so we make
                        // progress.
                        i += at;
                    }
                    OscRead::Incomplete => {
                        // Genuine split across the read boundary. Buffer the tail to
                        // stitch with the next chunk, unless it is implausibly long
                        // (then give up and pass it through; alacritty bounds its
                        // own OSC buffer).
                        let tail = &buf[i..];
                        if tail.len() <= MAX_OSC_LEN {
                            self.partial = tail.to_vec();
                        } else {
                            out.passthrough.extend_from_slice(tail);
                        }
                        break;
                    }
                },
                Some(_) => {
                    // ESC followed by non-`]` (CSI `ESC [`, a stray `ESC \`, ...):
                    // not an OSC introducer. Emit the ESC; following bytes are
                    // handled normally.
                    out.passthrough.push(ESC);
                    i += 1;
                }
            }
        }

        self.clean_pos += out.passthrough.len();
        out
    }

    /// Is this OSC body trusted (eligible to emit a mark / be stripped)?
    fn is_trusted(&self, body: &[u8]) -> bool {
        match &self.nonce {
            None => true,
            Some(n) => contains_nonce(body, n.as_bytes()),
        }
    }

    /// Should this recognized OSC body be stripped from the passthrough? Only our
    /// trusted block-protocol marks (133/633) are stripped; OSC 7 (cwd, a standard
    /// sequence) and OSC 1337 (telemetry) are left in place (both zero-width to the
    /// emulator), and foreign/un-nonced marks pass through untouched.
    fn should_strip(&self, body: &[u8]) -> bool {
        if body.starts_with(b"133;") || body.starts_with(b"633;") {
            return self.is_trusted(body);
        }
        false
    }

    /// Parse an OSC body into typed mark(s), pushing trusted ones at `offset`.
    /// Returns whether the body was a recognized OSC we own semantics for.
    fn handle_osc(&self, body: &[u8], offset: usize, out: &mut ScanResult) -> bool {
        if let Some(rest) = body.strip_prefix(b"133;") {
            self.parse_133(rest, offset, self.is_trusted(body), out);
            return true;
        }
        if let Some(rest) = body.strip_prefix(b"633;") {
            self.parse_633(rest, offset, self.is_trusted(body), out);
            return true;
        }
        if let Some(rest) = body.strip_prefix(b"7;") {
            // OSC 7 cwd: standard + low-risk, ingested regardless of nonce.
            if let Some(path) = parse_osc7_path(rest) {
                out.marks.push((offset, Mark::Cwd(path)));
            }
            return true;
        }
        if body.starts_with(b"1337;") {
            // Telemetry only (ShellIntegrationVersion etc.); recognized so callers
            // can reason about it, but we emit no block mark and do not depend on
            // it. Left in the passthrough (alacritty ignores it).
            return true;
        }
        false
    }

    /// Parse the OSC-133 body after `133;`: `A` | `B` | `C[;cmdline=ENC]` | `D[;n]`.
    fn parse_133(&self, rest: &[u8], offset: usize, trusted: bool, out: &mut ScanResult) {
        let Some(&first) = rest.first() else {
            return;
        };
        let kind = match first {
            b'A' => {
                // `A` may carry `;aterm_ver=<version>` (ticket T-2.3 AC2): the shell's
                // self-reported version, emitted once on the first prompt. Trusted +
                // sanitized so the indicator can surface "bash 3.2".
                if trusted {
                    if let Some(ver) = extract_shell_version(rest) {
                        out.marks.push((offset, Mark::ShellVersion(ver)));
                    }
                }
                PromptKind::PromptStart
            }
            b'B' => PromptKind::CommandStart,
            b'C' => {
                // `C` may carry `;cmdline=ENC`; emit the command line first so the
                // segmenter has it before the block opens at OutputStart.
                if trusted {
                    if let Some(cmd) = extract_cmdline(rest) {
                        out.marks.push((offset, Mark::CommandLine(cmd)));
                    }
                }
                PromptKind::OutputStart
            }
            b'D' => PromptKind::CommandDone {
                exit_code: parse_exit(rest),
            },
            _ => return,
        };
        if trusted {
            out.marks.push((offset, Mark::Prompt(kind)));
        }
    }

    /// Parse the OSC-633 (VS Code) body after `633;`. Opportunistic: maps to the
    /// same marks as 133, decodes the `E` command line, and reads `P;Cwd=`.
    fn parse_633(&self, rest: &[u8], offset: usize, trusted: bool, out: &mut ScanResult) {
        let Some(&first) = rest.first() else {
            return;
        };
        let kind = match first {
            b'A' => Some(PromptKind::PromptStart),
            b'B' => Some(PromptKind::CommandStart),
            b'C' => Some(PromptKind::OutputStart),
            b'D' => Some(PromptKind::CommandDone {
                exit_code: parse_exit(rest),
            }),
            b'E' => {
                // `E;<escaped-cmdline>[;<nonce>]` with VS Code escaping.
                if trusted {
                    out.marks
                        .push((offset, Mark::CommandLine(decode_633_command(rest))));
                }
                None
            }
            b'P' => {
                if trusted {
                    if let Some(cwd) = extract_633_cwd(rest) {
                        out.marks.push((offset, Mark::Cwd(cwd)));
                    }
                }
                None
            }
            _ => None,
        };
        if trusted {
            if let Some(k) = kind {
                out.marks.push((offset, Mark::Prompt(k)));
            }
        }
    }
}

/// Outcome of reading an OSC at an `ESC ]` introducer (`buf[0..2] == ESC ']'`).
enum OscRead<'a> {
    /// Properly terminated by BEL or ST (`ESC \`). `body` excludes the introducer
    /// and terminator; `consumed` includes the terminator.
    Done { body: &'a [u8], consumed: usize },
    /// Aborted: a fresh `ESC` that is NOT the `ESC \` ST terminator appeared before
    /// any terminator. Per ECMA-48 a new ESC aborts the in-progress control string,
    /// so the bytes so far are a malformed OSC. `at` is the index of that ESC
    /// within `buf`; the caller discards `buf[..at]` and re-scans from `at`. This is
    /// the load-bearing security boundary: a stray/unterminated OSC in untrusted
    /// output can never absorb (and thus leak or borrow the nonce from) the next
    /// genuine mark.
    Aborted { at: usize },
    /// No terminator and no aborting ESC in this buffer: a genuine sequence split
    /// across a read boundary. The caller buffers the tail to stitch with the next
    /// chunk.
    Incomplete,
}

/// Read an OSC starting at `buf[0] == ESC, buf[1] == ']'`.
fn read_osc(buf: &[u8]) -> OscRead<'_> {
    let mut j = 2; // skip ESC ']'
    while j < buf.len() {
        match buf[j] {
            BEL => {
                return OscRead::Done {
                    body: &buf[2..j],
                    consumed: j + 1,
                }
            }
            ESC => match buf.get(j + 1) {
                // `ESC \` is the ST terminator.
                Some(b'\\') => {
                    return OscRead::Done {
                        body: &buf[2..j],
                        consumed: j + 2,
                    }
                }
                // Any other ESC (a fresh `ESC ]` introducer, `ESC [` CSI, ...)
                // aborts the in-progress OSC at this position.
                Some(_) => return OscRead::Aborted { at: j },
                // A trailing ESC at the buffer edge might be a split `ESC \` ST;
                // treat as incomplete so it can stitch with the next chunk.
                None => return OscRead::Incomplete,
            },
            _ => j += 1,
        }
    }
    OscRead::Incomplete
}

/// Parse an optional exit code from a `D[;<code>[;...]]` body.
fn parse_exit(rest: &[u8]) -> Option<i32> {
    if rest.get(1) != Some(&b';') {
        return None;
    }
    let tail = &rest[2..];
    let end = tail.iter().position(|&c| c == b';').unwrap_or(tail.len());
    std::str::from_utf8(&tail[..end]).ok()?.trim().parse().ok()
}

/// Extract `cmdline=ENC` from an OSC-133 `C` body and percent-decode it.
fn extract_cmdline(rest: &[u8]) -> Option<String> {
    rest.split(|&c| c == b';')
        .find_map(|f| f.strip_prefix(b"cmdline="))
        .map(percent_decode)
}

/// Maximum length of a sanitized shell-version token (ticket T-2.3). Generous for any
/// real version string (`5.2.15(1)-release`, `5.9`, `3.7.1`); bounds an adversarial
/// nonce-leak attempt from bloating the published string.
const MAX_SHELL_VERSION_LEN: usize = 48;

/// Extract `aterm_ver=<version>` from an OSC-133 `A` body and sanitize it to a short
/// version-like token (ticket T-2.3 AC2). Even though it is nonce-gated (only our shim
/// emits it), the value is shell-expanded, so we keep only printable, non-`;` ASCII and
/// cap the length - it is surfaced verbatim in the UI's "why?" string.
fn extract_shell_version(rest: &[u8]) -> Option<String> {
    let raw = rest
        .split(|&c| c == b';')
        .find_map(|f| f.strip_prefix(b"aterm_ver="))?;
    let cleaned: String = raw
        .iter()
        .filter(|&&b| b.is_ascii_graphic() || b == b' ')
        .take(MAX_SHELL_VERSION_LEN)
        .map(|&b| b as char)
        .collect();
    let trimmed = cleaned.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Decode an OSC-633 `E;<escaped-cmd>[;<nonce>]` body to the command text.
fn decode_633_command(rest: &[u8]) -> String {
    let Some(after) = rest.strip_prefix(b"E;") else {
        return String::new(); // bare `E` → empty command
    };
    let end = after.iter().position(|&c| c == b';').unwrap_or(after.len());
    decode_vscode(&after[..end])
}

/// Extract `Cwd=<path>` from an OSC-633 `P;...` property body.
fn extract_633_cwd(rest: &[u8]) -> Option<String> {
    let params = rest.strip_prefix(b"P;")?;
    params
        .split(|&c| c == b';')
        .find_map(|f| f.strip_prefix(b"Cwd="))
        .map(|v| String::from_utf8_lossy(v).into_owned())
}

/// Decode an OSC-7 body (`file://host/path`) into a filesystem path.
fn parse_osc7_path(rest: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(rest).ok()?;
    let after_scheme = s.strip_prefix("file://")?;
    // Drop the authority (host) up to the first '/'.
    let path = match after_scheme.find('/') {
        Some(idx) => &after_scheme[idx..],
        None => after_scheme,
    };
    Some(percent_decode(path.as_bytes()))
}

/// VS Code OSC-633 `E` command escaping: `\\` → `\`, `\xHH` → byte `0xHH`
/// (so `\x3b` → `;`, the field separator that must not appear literally).
fn decode_vscode(s: &[u8]) -> String {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'\\' && i + 1 < s.len() {
            match s[i + 1] {
                b'\\' => {
                    out.push(b'\\');
                    i += 2;
                    continue;
                }
                b'x' | b'X' if i + 3 < s.len() => {
                    if let (Some(h), Some(l)) = (hex_val(s[i + 2]), hex_val(s[i + 3])) {
                        out.push((h << 4) | l);
                        i += 4;
                        continue;
                    }
                }
                _ => {}
            }
        }
        out.push(s[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Minimal percent-decoding (no external dep).
fn percent_decode(bytes: &[u8]) -> String {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
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

/// Does an OSC body carry `aterm_nonce=<nonce>` as a complete `;`-delimited param?
///
/// Anchored to a full field (not an unanchored substring) so neither a key whose
/// suffix is `aterm_nonce=` (e.g. `notaterm_nonce=<nonce>`) nor a value with the
/// nonce as a prefix (`aterm_nonce=<nonce>EXTRA`) can pass the trust gate. This is
/// defense-in-depth: the nonce is a per-session secret, so a substring match is
/// not independently exploitable, but exact-field matching removes the footgun.
fn contains_nonce(body: &[u8], nonce: &[u8]) -> bool {
    let mut expected: Vec<u8> = b"aterm_nonce=".to_vec();
    expected.extend_from_slice(nonce);
    body.split(|&c| c == b';')
        .any(|field| field == expected.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a BEL-terminated OSC sequence.
    fn osc(body: &str) -> Vec<u8> {
        let mut v = vec![ESC, b']'];
        v.extend_from_slice(body.as_bytes());
        v.push(BEL);
        v
    }

    fn marks_only(r: &ScanResult) -> Vec<Mark> {
        r.marks.iter().map(|(_, m)| m.clone()).collect()
    }

    #[test]
    fn prompt_lifecycle_marks_at_correct_offsets_and_stripped() {
        // AC: a real zsh-style A->B->C->D cycle (+ OSC 7) yields correct marks at
        // correct offsets into the CLEAN stream, with our marks removed (zero
        // width, so cursor math is unaffected).
        let mut s = OscScanner::untrusted();
        let mut stream = Vec::new();
        stream.extend(osc("7;file://host/Users/me")); // cwd (left in passthrough)
        stream.extend(osc("133;A"));
        stream.extend(b"user@host $ "); // 12 clean bytes
        stream.extend(osc("133;B"));
        stream.extend(b"ls -la"); // 6 clean bytes
        stream.extend(osc("133;C"));
        stream.extend(b"file1\nfile2\n"); // 12 clean bytes
        stream.extend(osc("133;D;0"));

        let r = s.scan(&stream);

        // OSC 7 stays in the passthrough; the four 133 marks are stripped.
        let osc7 = osc("7;file://host/Users/me");
        let mut expected_pass = osc7.clone();
        expected_pass.extend_from_slice(b"user@host $ ls -lafile1\nfile2\n");
        assert_eq!(r.passthrough, expected_pass);

        // Offsets are positions in the clean stream: cwd at 0, A right after the
        // OSC-7 bytes, B after "user@host $ ", C after "ls -la", D after output.
        let o7 = osc7.len();
        assert_eq!(
            r.marks,
            vec![
                (0, Mark::Cwd("/Users/me".to_string())),
                (o7, Mark::Prompt(PromptKind::PromptStart)),
                (o7 + 12, Mark::Prompt(PromptKind::CommandStart)),
                (o7 + 18, Mark::Prompt(PromptKind::OutputStart)),
                (
                    o7 + 30,
                    Mark::Prompt(PromptKind::CommandDone { exit_code: Some(0) })
                ),
            ]
        );
    }

    #[test]
    fn parses_nonzero_exit_code() {
        let mut s = OscScanner::untrusted();
        let r = s.scan(&osc("133;D;127"));
        assert_eq!(
            marks_only(&r),
            vec![Mark::Prompt(PromptKind::CommandDone {
                exit_code: Some(127)
            })]
        );
    }

    #[test]
    fn both_terminators_parse() {
        // BEL terminator.
        let mut s1 = OscScanner::untrusted();
        assert_eq!(
            marks_only(&s1.scan(&osc("133;A"))),
            vec![Mark::Prompt(PromptKind::PromptStart)]
        );
        // ST (ESC '\') terminator.
        let mut s2 = OscScanner::untrusted();
        let mut seq = vec![ESC, b']'];
        seq.extend(b"133;B");
        seq.extend([ESC, b'\\']);
        let r = s2.scan(&seq);
        assert_eq!(marks_only(&r), vec![Mark::Prompt(PromptKind::CommandStart)]);
        assert!(r.passthrough.is_empty());
    }

    #[test]
    fn mark_split_across_two_chunks_is_stitched() {
        // AC: a mark split across two read chunks parses correctly, at the right
        // global offset, with the clean stream contiguous across the boundary.
        let mut s = OscScanner::untrusted();

        // Chunk 1: clean "abc", then an OSC that is cut off mid-sequence.
        let mut c1 = b"abc".to_vec();
        c1.extend([ESC, b']']);
        c1.extend(b"133;A"); // no terminator yet
        let r1 = s.scan(&c1);
        assert_eq!(r1.passthrough, b"abc");
        assert!(r1.marks.is_empty(), "incomplete mark must not fire yet");

        // Chunk 2: the terminator, then more clean text.
        let mut c2 = vec![BEL];
        c2.extend(b"def");
        let r2 = s.scan(&c2);
        // The stitched mark fires at clean offset 3 (right after "abc").
        assert_eq!(r2.marks, vec![(3, Mark::Prompt(PromptKind::PromptStart))]);
        assert_eq!(r2.passthrough, b"def");
    }

    #[test]
    fn split_at_esc_bracket_boundary_is_stitched() {
        // The nastiest split: the chunk ends on a lone ESC, the ']' arrives next.
        let mut s = OscScanner::untrusted();
        let r1 = s.scan(&[b'x', ESC]);
        assert_eq!(r1.passthrough, b"x");
        assert!(r1.marks.is_empty());

        let mut c2 = vec![b']'];
        c2.extend(b"133;C");
        c2.push(BEL);
        let r2 = s.scan(&c2);
        assert_eq!(r2.marks, vec![(1, Mark::Prompt(PromptKind::OutputStart))]);
        assert!(r2.passthrough.is_empty());
    }

    #[test]
    fn lone_trailing_esc_that_is_not_osc_passes_through() {
        // A trailing ESC buffered, then a CSI (SGR) sequence next chunk: must pass
        // through verbatim (not be mistaken for / swallowed as an OSC).
        let mut s = OscScanner::untrusted();
        let r1 = s.scan(&[b'a', ESC]);
        assert_eq!(r1.passthrough, b"a");
        let r2 = s.scan(b"[1mX");
        assert_eq!(r2.passthrough, &[ESC, b'[', b'1', b'm', b'X']);
        assert!(r2.marks.is_empty());
    }

    #[test]
    fn nonce_gating_trusts_strips_only_our_marks() {
        // AC: a wrong/absent nonce is dropped (mark not emitted) and the bytes pass
        // through; a correctly-nonced mark is trusted and stripped.
        let mut s = OscScanner::with_nonce("DEADBEEF");
        let mut stream = Vec::new();
        stream.extend(osc("133;A")); // absent nonce -> dropped, passes through
        stream.extend(osc("133;B;aterm_nonce=WRONGNON")); // wrong nonce -> dropped
        stream.extend(osc("133;C;aterm_nonce=DEADBEEF")); // ours -> trusted+stripped
        let r = s.scan(&stream);

        assert_eq!(
            marks_only(&r),
            vec![Mark::Prompt(PromptKind::OutputStart)],
            "only the correctly-nonced mark is emitted"
        );
        // The two untrusted marks pass through verbatim; ours is stripped.
        let mut expected = osc("133;A");
        expected.extend(osc("133;B;aterm_nonce=WRONGNON"));
        assert_eq!(r.passthrough, expected);
    }

    #[test]
    fn osc7_cwd_parsed_and_left_in_passthrough() {
        let mut s = OscScanner::untrusted();
        let r = s.scan(&osc("7;file://host/Users/me/dev%20dir"));
        assert_eq!(
            marks_only(&r),
            vec![Mark::Cwd("/Users/me/dev dir".to_string())]
        );
        assert_eq!(r.passthrough, osc("7;file://host/Users/me/dev%20dir"));
    }

    #[test]
    fn osc633_e_decodes_vscode_escaping() {
        // AC: an OSC 633;E with VS Code escaping decodes to the correct command.
        // `\x3b` -> ';', `\\` -> '\', `\xAB` -> byte 0xAB.
        let mut s = OscScanner::untrusted();
        // Command: `echo a;b\c` then a high byte, with a trailing VS Code nonce.
        let r = s.scan(&osc("633;E;echo a\\x3bb\\\\c\\xAB;somenonce"));
        let mut expected_cmd = b"echo a;b\\c".to_vec();
        expected_cmd.push(0xAB);
        let expected = String::from_utf8_lossy(&expected_cmd).into_owned();
        assert_eq!(marks_only(&r), vec![Mark::CommandLine(expected)]);
        // 633 marks are stripped in untrusted mode.
        assert!(r.passthrough.is_empty());
    }

    #[test]
    fn osc633_abcd_map_to_prompt_kinds() {
        let mut s = OscScanner::untrusted();
        let mut stream = Vec::new();
        stream.extend(osc("633;A"));
        stream.extend(osc("633;B"));
        stream.extend(osc("633;C"));
        stream.extend(osc("633;D;3"));
        let r = s.scan(&stream);
        assert_eq!(
            marks_only(&r),
            vec![
                Mark::Prompt(PromptKind::PromptStart),
                Mark::Prompt(PromptKind::CommandStart),
                Mark::Prompt(PromptKind::OutputStart),
                Mark::Prompt(PromptKind::CommandDone { exit_code: Some(3) }),
            ]
        );
    }

    #[test]
    fn osc633_p_cwd_parsed() {
        let mut s = OscScanner::untrusted();
        let r = s.scan(&osc("633;P;Cwd=/home/me/project"));
        assert_eq!(
            marks_only(&r),
            vec![Mark::Cwd("/home/me/project".to_string())]
        );
    }

    #[test]
    fn osc133_c_with_cmdline_emits_command_then_output_start() {
        let mut s = OscScanner::untrusted();
        let r = s.scan(&osc("133;C;cmdline=ls%20-la"));
        assert_eq!(
            marks_only(&r),
            vec![
                Mark::CommandLine("ls -la".to_string()),
                Mark::Prompt(PromptKind::OutputStart),
            ]
        );
    }

    #[test]
    fn osc1337_telemetry_passes_through_no_mark() {
        let mut s = OscScanner::untrusted();
        let r = s.scan(&osc("1337;ShellIntegrationVersion=14;shell=zsh"));
        assert!(r.marks.is_empty());
        assert_eq!(
            r.passthrough,
            osc("1337;ShellIntegrationVersion=14;shell=zsh")
        );
    }

    #[test]
    fn non_osc_bytes_pass_through_untouched() {
        let mut s = OscScanner::untrusted();
        let data = b"plain output with \x1b[1m SGR \x1b[0m and text";
        let r = s.scan(data);
        assert_eq!(r.passthrough, data);
        assert!(r.marks.is_empty());
    }

    #[test]
    fn over_long_unterminated_osc_is_flushed_not_swallowed() {
        // A stray `ESC ]` followed by a flood with no terminator must not buffer
        // unboundedly; past MAX_OSC_LEN it is flushed to the passthrough.
        let mut s = OscScanner::untrusted();
        let mut data = vec![ESC, b']'];
        data.extend(std::iter::repeat_n(b'Z', MAX_OSC_LEN + 100));
        let r = s.scan(&data);
        assert!(r.marks.is_empty());
        assert_eq!(
            r.passthrough, data,
            "an over-long unterminated OSC is passed through, not swallowed"
        );
        // And nothing is left buffered.
        assert!(s.partial.is_empty());
    }

    #[test]
    fn cumulative_offsets_across_chunks() {
        // Offsets accumulate across scan calls (the clean-stream position is
        // global), so the block state machine can address marks consistently.
        let mut s = OscScanner::untrusted();
        let r1 = s.scan(b"hello"); // 5 clean bytes, no marks
        assert!(r1.marks.is_empty());
        let r2 = s.scan(&osc("133;A")); // mark at global offset 5
        assert_eq!(r2.marks, vec![(5, Mark::Prompt(PromptKind::PromptStart))]);
    }

    // --- Security regressions (T-2.1 adversarial review): a fresh `ESC` must
    //     abort the in-progress OSC, so an unterminated OSC in untrusted command
    //     output can never stitch across the chunk boundary and absorb the
    //     shell's next genuine nonced mark. ---

    #[test]
    fn attack_unterminated_osc7_does_not_leak_nonce() {
        // Untrusted output prints an UNTERMINATED OSC 7 (non-strippable); the
        // shell's genuine nonced 133;A arrives next chunk. They must NOT merge:
        // the nonce must never reach the passthrough, and the genuine A is parsed
        // on its own (trusted + stripped).
        let mut s = OscScanner::with_nonce("DEADBEEF");
        let mut c1 = b"x".to_vec();
        c1.extend([ESC, b']']);
        c1.extend(b"7;file://h/tmp"); // unterminated
        let r1 = s.scan(&c1);

        let mut c2 = vec![ESC, b']'];
        c2.extend(b"133;A;aterm_nonce=DEADBEEF");
        c2.push(BEL);
        let r2 = s.scan(&c2);

        let mut all_passthrough = r1.passthrough.clone();
        all_passthrough.extend_from_slice(&r2.passthrough);
        assert!(
            !all_passthrough.windows(8).any(|w| w == b"DEADBEEF"),
            "the per-session nonce must never reach the passthrough, got {all_passthrough:?}"
        );
        assert_eq!(r2.marks, vec![(1, Mark::Prompt(PromptKind::PromptStart))]);
    }

    #[test]
    fn attack_unterminated_forged_mark_does_not_borrow_nonce() {
        // Untrusted output prints an UNTERMINATED forged 133;D (no nonce); the
        // shell's genuine nonced 133;A arrives next. The forged D must NOT be
        // trusted by borrowing the genuine mark's nonce; only the genuine A fires.
        let mut s = OscScanner::with_nonce("DEADBEEF");
        let mut c1 = b"evil".to_vec();
        c1.extend([ESC, b']']);
        c1.extend(b"133;D;0"); // unterminated forged CommandDone, no nonce
        let r1 = s.scan(&c1);
        assert!(r1.marks.is_empty());
        assert_eq!(r1.passthrough, b"evil");

        let mut c2 = vec![ESC, b']'];
        c2.extend(b"133;A;aterm_nonce=DEADBEEF");
        c2.push(BEL);
        let r2 = s.scan(&c2);

        assert_eq!(
            r2.marks,
            vec![(4, Mark::Prompt(PromptKind::PromptStart))],
            "only the genuine A fires; the forged D must not borrow the stitched nonce"
        );
        assert!(r2.passthrough.is_empty());
    }

    #[test]
    fn single_chunk_embedded_esc_aborts_first_osc() {
        // A second `ESC ]` inside a body (no terminator before it) aborts the
        // first OSC and starts a fresh one - even within a single chunk.
        let mut s = OscScanner::untrusted();
        let mut stream = vec![ESC, b']'];
        stream.extend(b"133;C"); // unterminated
        stream.extend([ESC, b']']);
        stream.extend(b"133;A");
        stream.push(BEL);
        let r = s.scan(&stream);
        assert_eq!(marks_only(&r), vec![Mark::Prompt(PromptKind::PromptStart)]);
        assert!(r.passthrough.is_empty());
    }

    #[test]
    fn untrusted_unterminated_mark_does_not_swallow_next() {
        // Even in untrusted mode (today's engine default), an unterminated mark in
        // command output must not swallow the shell's next genuine mark or desync
        // its offset.
        let mut s = OscScanner::untrusted();
        let mut c1 = b"out".to_vec();
        c1.extend([ESC, b']']);
        c1.extend(b"133;C"); // unterminated
        let r1 = s.scan(&c1);
        assert_eq!(r1.passthrough, b"out");
        assert!(r1.marks.is_empty());

        let mut c2 = vec![ESC, b']'];
        c2.extend(b"133;A");
        c2.push(BEL);
        let r2 = s.scan(&c2);
        assert_eq!(r2.marks, vec![(3, Mark::Prompt(PromptKind::PromptStart))]);
    }

    #[test]
    fn nonce_match_is_anchored_to_full_field() {
        // A key whose suffix is the nonce param, or a value with the nonce as a
        // prefix, must NOT pass the trust gate - only an exact `;`-delimited field.
        let mut s = OscScanner::with_nonce("DEADBEEF");
        let mut stream = Vec::new();
        stream.extend(osc("133;A;notaterm_nonce=DEADBEEF")); // suffix-key: rejected
        stream.extend(osc("133;B;aterm_nonce=DEADBEEFEXTRA")); // prefix-value: rejected
        stream.extend(osc("133;C;aterm_nonce=DEADBEEF")); // exact: accepted
        let r = s.scan(&stream);
        assert_eq!(marks_only(&r), vec![Mark::Prompt(PromptKind::OutputStart)]);
    }

    #[test]
    fn osc133_a_with_version_emits_shell_version_then_prompt_start() {
        // T-2.3 AC2: the shell reports its version on the first prompt's `A`.
        let mut s = OscScanner::untrusted();
        let r = s.scan(&osc("133;A;aterm_ver=5.2.15(1)-release"));
        assert_eq!(
            marks_only(&r),
            vec![
                Mark::ShellVersion("5.2.15(1)-release".to_string()),
                Mark::Prompt(PromptKind::PromptStart),
            ]
        );
    }

    #[test]
    fn shell_version_is_trimmed_and_length_bounded() {
        let mut s = OscScanner::untrusted();
        let r = s.scan(&osc("133;A;aterm_ver=  5.9  ")); // surrounding spaces trimmed
        assert!(
            matches!(marks_only(&r).first(), Some(Mark::ShellVersion(v)) if v == "5.9"),
            "version is trimmed: {:?}",
            marks_only(&r)
        );

        let long = "9".repeat(200);
        let mut s2 = OscScanner::untrusted();
        let r2 = s2.scan(&osc(&format!("133;A;aterm_ver={long}")));
        match marks_only(&r2).first() {
            Some(Mark::ShellVersion(v)) => assert!(v.len() <= MAX_SHELL_VERSION_LEN),
            other => panic!("expected a bounded ShellVersion, got {other:?}"),
        }
    }

    #[test]
    fn shell_version_rides_the_nonce_trust_gate() {
        // The version is nonce-gated like every other mark: a forged `A;aterm_ver`
        // without the session nonce yields nothing; with it, the version is trusted.
        let mut s = OscScanner::with_nonce("SECRET");
        let forged = s.scan(&osc("133;A;aterm_ver=6.6.6"));
        assert!(
            marks_only(&forged).is_empty(),
            "an un-nonced version mark must be dropped"
        );
        let good = s.scan(&osc("133;A;aterm_ver=5.2;aterm_nonce=SECRET"));
        assert_eq!(
            marks_only(&good),
            vec![
                Mark::ShellVersion("5.2".to_string()),
                Mark::Prompt(PromptKind::PromptStart),
            ]
        );
    }
}
