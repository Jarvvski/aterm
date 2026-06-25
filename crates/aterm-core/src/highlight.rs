//! Shell command-line syntax highlighting + fish-style ghost text (ticket T-3.5,
//! the pure half).
//!
//! This is the computation the async overlay worker drives off the render thread:
//! a pure, allocation-light pass over the in-progress input line. It NEVER runs on
//! the keystroke path - the worker (aterm-ui) calls these functions, debounced, and
//! applies the result via [`InputModel::set_highlight`] / [`InputModel::set_ghost`];
//! the render path only reads the last-good overlay, so it cannot block here.
//!
//! [`highlight_for`] produces **non-inheritable** spans: the full set is recomputed
//! from the text each call, so a character typed after a styled run is reclassified
//! rather than tinted by the preceding token. Agent mode gets no shell highlight
//! (prose), and the ghost is Shell-only (agent ghost is owner-open-question #4,
//! defaulted off).
//!
//! See `05-unified-input-ux.md` sections 1 + 4. The async worker, the debounce
//! timer, and the overlay render are aterm-ui's half (T-3.5 / T-3.6), not here.

use crate::history::{HistoryRing, HistoryScope};
use crate::input::{GhostText, Highlight, InputMode, SpanKind, StyleSpan};

/// Compute the style overlay for `text` in `mode`. Shell mode is syntax-highlighted
/// ([`highlight_command_line`]); Agent mode is prose and gets no spans. This is the
/// mode-aware entry the worker calls (so a `ToggleMode` recompute flips highlight on
/// or off without touching the text).
#[must_use]
pub fn highlight_for(text: &str, mode: InputMode) -> Highlight {
    match mode {
        InputMode::Shell => Highlight {
            spans: highlight_command_line(text),
        },
        InputMode::Agent => Highlight::default(),
    }
}

/// Tokenize a shell command line into non-overlapping [`StyleSpan`]s over CHAR
/// offsets. Recognizes command vs argument vs flag, single/double-quoted strings
/// (an unterminated quote becomes an [`SpanKind::ErrorUnderline`] run to end of
/// text), and the operators `| & ; < > && ||` (a command separator resets the next
/// word to a command, a redirection does not). Quote handling is intentionally
/// simple (quote-to-matching-quote, no backslash escapes) - enough for tinting and
/// the unterminated-quote error, not a full shell grammar.
#[must_use]
pub fn highlight_command_line(text: &str) -> Vec<StyleSpan> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut spans = Vec::new();
    let mut i = 0;
    // True when the next word starts a command (line start / after a separator).
    let mut expect_command = true;

    while i < n {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        // Operators (`&&`/`||` are two chars; the rest one).
        if let Some((len, is_separator)) = operator_at(&chars, i) {
            spans.push(StyleSpan {
                start: i,
                end: i + len,
                kind: SpanKind::Operator,
            });
            i += len;
            // A command separator (| & ; && ||) starts a fresh command; a
            // redirection (< >) is followed by a filename argument, not a command.
            expect_command = is_separator;
            continue;
        }
        // Quoted strings.
        if c == '\'' || c == '"' {
            let (end, terminated) = scan_quote(&chars, i, c);
            spans.push(StyleSpan {
                start: i,
                end,
                kind: if terminated {
                    SpanKind::QuotedString
                } else {
                    SpanKind::ErrorUnderline
                },
            });
            i = end;
            expect_command = false;
            continue;
        }
        // A bare word: run until whitespace, a quote, or an operator.
        let start = i;
        while i < n {
            let ch = chars[i];
            if ch.is_whitespace() || ch == '\'' || ch == '"' || operator_at(&chars, i).is_some() {
                break;
            }
            i += 1;
        }
        let kind = if expect_command {
            SpanKind::Command
        } else if chars[start] == '-' {
            SpanKind::Flag
        } else {
            SpanKind::Argument
        };
        spans.push(StyleSpan {
            start,
            end: i,
            kind,
        });
        expect_command = false;
    }
    spans
}

/// The fish-style ghost-text suggestion for `text` in `mode`, drawn from `history`
/// within `scope`. Shell mode only (agent ghost is defaulted off). Returns the FULL
/// most-recent prefix match as the suggestion (the visible tail is derived live by
/// [`InputModel::ghost_tail`]), or `None` when there is no match (or the line is
/// blank / in Agent mode). `suggest` guarantees the match strictly extends `text`,
/// so a non-empty tail always exists.
#[must_use]
pub fn ghost_for(
    text: &str,
    mode: InputMode,
    history: &HistoryRing,
    scope: HistoryScope,
) -> Option<GhostText> {
    if mode != InputMode::Shell {
        return None;
    }
    let entry = history.suggest(scope, text)?;
    Some(GhostText {
        suggestion: entry.text.clone(),
    })
}

/// If an operator begins at `chars[i]`, return `(length, is_command_separator)`.
/// `&&`/`||` are two chars; `| & ;` are command separators; `< >` are redirections.
fn operator_at(chars: &[char], i: usize) -> Option<(usize, bool)> {
    let c = chars[i];
    let next = chars.get(i + 1).copied();
    match c {
        '&' if next == Some('&') => Some((2, true)),
        '|' if next == Some('|') => Some((2, true)),
        '|' | '&' | ';' => Some((1, true)),
        '<' | '>' => Some((1, false)),
        _ => None,
    }
}

/// Scan a quoted run starting at the opening quote `chars[start]` (`quote`). Returns
/// `(end_exclusive, terminated)`: the char offset just past the closing quote and
/// whether a close was found; an unterminated quote runs to the end of the text.
fn scan_quote(chars: &[char], start: usize, quote: char) -> (usize, bool) {
    let n = chars.len();
    let mut i = start + 1;
    while i < n {
        if chars[i] == quote {
            return (i + 1, true);
        }
        i += 1;
    }
    (n, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn kinds(spans: &[StyleSpan]) -> Vec<SpanKind> {
        spans.iter().map(|s| s.kind).collect()
    }

    fn text_of(src: &str, span: &StyleSpan) -> String {
        src.chars()
            .skip(span.start)
            .take(span.end - span.start)
            .collect()
    }

    #[test]
    fn first_word_is_command_then_flags_and_args() {
        let src = "echo -n hello";
        let spans = highlight_command_line(src);
        assert_eq!(
            kinds(&spans),
            vec![SpanKind::Command, SpanKind::Flag, SpanKind::Argument]
        );
        assert_eq!(text_of(src, &spans[0]), "echo");
        assert_eq!(text_of(src, &spans[1]), "-n");
        assert_eq!(text_of(src, &spans[2]), "hello");
    }

    #[test]
    fn a_separator_starts_a_new_command_but_a_redirect_does_not() {
        // `ls | grep foo`: grep is a fresh Command; `cat > out`: out is an Argument.
        let pipe = highlight_command_line("ls | grep foo");
        assert_eq!(
            kinds(&pipe),
            vec![
                SpanKind::Command,  // ls
                SpanKind::Operator, // |
                SpanKind::Command,  // grep
                SpanKind::Argument, // foo
            ]
        );
        let redir = highlight_command_line("cat > out");
        assert_eq!(
            kinds(&redir),
            vec![SpanKind::Command, SpanKind::Operator, SpanKind::Argument]
        );
        // `&&` is a two-char separator.
        let chain = highlight_command_line("a && b");
        assert_eq!(
            kinds(&chain),
            vec![SpanKind::Command, SpanKind::Operator, SpanKind::Command]
        );
        assert_eq!(
            chain[1].end - chain[1].start,
            2,
            "&& is one two-char operator"
        );
    }

    #[test]
    fn quotes_are_strings_and_an_unterminated_quote_is_an_error_underline() {
        let ok = highlight_command_line(r#"echo "hi there""#);
        assert_eq!(kinds(&ok), vec![SpanKind::Command, SpanKind::QuotedString]);
        let bad = highlight_command_line(r#"echo "oops"#);
        assert_eq!(
            kinds(&bad),
            vec![SpanKind::Command, SpanKind::ErrorUnderline],
            "an unterminated quote underlines to end of line"
        );
        // The error run covers the opening quote through end of text.
        let err = bad.last().unwrap();
        assert_eq!(text_of(r#"echo "oops"#, err), r#""oops"#);
    }

    #[test]
    fn spans_are_non_inheritable_recomputed_from_the_whole_text() {
        // AC5: typing after a styled run does not inherit its style. The command span
        // stays bounded to the command word; the new token is classified afresh.
        let before = highlight_command_line("echo");
        assert_eq!(kinds(&before), vec![SpanKind::Command]);
        assert_eq!((before[0].start, before[0].end), (0, 4));
        let after = highlight_command_line("echo x");
        assert_eq!(
            kinds(&after),
            vec![SpanKind::Command, SpanKind::Argument],
            "the appended token is an Argument, NOT an extension of the Command"
        );
        assert_eq!(
            (after[0].start, after[0].end),
            (0, 4),
            "command span unchanged"
        );
    }

    #[test]
    fn char_offsets_track_multibyte_text() {
        // Offsets are CHAR offsets: a leading multibyte arg must not shift them.
        let src = "echo café";
        let spans = highlight_command_line(src);
        assert_eq!(kinds(&spans), vec![SpanKind::Command, SpanKind::Argument]);
        assert_eq!(text_of(src, &spans[1]), "café");
    }

    #[test]
    fn highlight_for_is_mode_aware() {
        // AC4 (recompute on toggle): Shell highlights, Agent prose gets none.
        let shell = highlight_for("ls -la", InputMode::Shell);
        assert_eq!(kinds(&shell.spans), vec![SpanKind::Command, SpanKind::Flag]);
        let agent = highlight_for("ls -la", InputMode::Agent);
        assert!(agent.spans.is_empty(), "agent prose has no shell highlight");
    }

    #[test]
    fn empty_input_has_no_spans() {
        assert!(highlight_command_line("").is_empty());
        assert!(highlight_command_line("   ").is_empty());
    }

    // Build a Shell-scoped ring; entries are pushed oldest-first (increasing
    // timestamps), so the LAST entry is the newest for prefix-match recency.
    fn history_with(entries: &[&str]) -> HistoryRing {
        let mut h = HistoryRing::new();
        for (i, e) in entries.iter().enumerate() {
            h.push(
                *e,
                InputMode::Shell,
                SystemTime::UNIX_EPOCH + Duration::from_secs(i as u64),
            );
        }
        h
    }

    #[test]
    fn ghost_is_the_full_most_recent_prefix_match() {
        // AC2: ghost text appears from history (prefix match, newest first). The
        // ghost carries the FULL suggested line; the visible tail is derived live by
        // InputModel::ghost_tail.
        let h = history_with(&["git status", "git commit -m x", "git status -s"]);
        let g = ghost_for(
            "git st",
            InputMode::Shell,
            &h,
            HistoryScope::Mode(InputMode::Shell),
        );
        assert_eq!(
            g,
            Some(GhostText {
                suggestion: "git status -s".to_string()
            }),
            "newest 'git st…' match (full line)"
        );
    }

    #[test]
    fn ghost_is_none_on_blank_line_no_match_or_agent_mode() {
        let h = history_with(&["cargo build"]);
        let shell = HistoryScope::Mode(InputMode::Shell);
        assert_eq!(
            ghost_for("", InputMode::Shell, &h, shell),
            None,
            "blank line"
        );
        assert_eq!(
            ghost_for("zzz", InputMode::Shell, &h, shell),
            None,
            "no match"
        );
        assert_eq!(
            ghost_for("cargo", InputMode::Agent, &h, shell),
            None,
            "agent ghost is off by default"
        );
        // An exact full match has no tail to suggest.
        assert_eq!(ghost_for("cargo build", InputMode::Shell, &h, shell), None);
    }
}
