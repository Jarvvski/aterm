//! aterm-bench - benches over aterm-core.
//!
//! Throughput benches (criterion) live in `benches/engine.rs`; the Tier-1
//! instruction-count benches (iai-callgrind) in `benches/tier1.rs`. This library
//! exposes the deterministic byte-payload [`fixtures`] both share, so the payloads
//! are reviewable *source* (not opaque checked-in binary blobs) and byte-identical
//! across every run - the property the Tier-1 instruction-count gate depends on
//! (ticket T-1.7).
//!
//! [`scenario`] declares the Tier-2 stress scenarios + the pass/fail gate (the pure,
//! headless-tested core of the "60fps always" proof, ticket T-7.2); the on-hardware
//! `scenario_driver` binary replays them against the real `aterm-ui` app loop.
//! [`latency`] is the sibling for input latency (ticket T-7.3): the pure
//! keystroke->glyph measure + gate, with the on-hardware `latency_driver` binary.

pub mod latency;
pub mod scenario;

/// Deterministic VT byte payloads for the benches. Built programmatically so they
/// are checked in, reviewable, and identical every run (no randomness, no clock,
/// no environment). Sizes are a few KiB each - representative of a frame's worth of
/// PTY output, large enough that an instruction-count regression in a hot path
/// shows up clearly.
pub mod fixtures {
    /// Plaintext scroll: many CRLF-terminated lines of plain text - the real-world
    /// common case (a build log, `cat` of a source file). Stresses the parser's
    /// plain path plus grid scroll.
    #[must_use]
    pub fn plaintext_scroll() -> Vec<u8> {
        let line = b"The quick brown fox jumps over the lazy dog 0123456789 abcdefghij\r\n";
        let mut buf = Vec::with_capacity(line.len() * 400);
        for _ in 0..400 {
            buf.extend_from_slice(line);
        }
        buf
    }

    /// SGR-heavy: each short token wrapped in a distinct fg/bg color escape, then
    /// reset - the dense-style stress (syntax-highlighted output, `ls --color`).
    #[must_use]
    pub fn sgr_heavy() -> Vec<u8> {
        let mut buf = Vec::with_capacity(16 * 1024);
        for i in 0..1500 {
            let fg = 30 + (i % 8);
            let bg = 40 + ((i / 8) % 8);
            buf.extend_from_slice(format!("\x1b[1;{fg};{bg}mtok\x1b[0m ").as_bytes());
            if i % 12 == 11 {
                buf.extend_from_slice(b"\r\n");
            }
        }
        buf
    }

    /// Unicode + CJK wide chars + accents - the wide-cell / multi-byte path.
    #[must_use]
    pub fn unicode() -> Vec<u8> {
        let unit = "hello world cafe \u{e9} \u{65e5}\u{672c}\u{8a9e} \u{4e2d}\u{6587} \
                    \u{d55c}\u{ad6d}\u{c5b4} crab rocket sparkle\r\n";
        let mut buf = Vec::with_capacity(unit.len() * 200);
        for _ in 0..200 {
            buf.extend_from_slice(unit.as_bytes());
        }
        buf
    }

    /// Alt-screen TUI redraw: enter the alternate screen, repaint a full grid with
    /// absolute cursor moves (a vim/htop-style repaint), then leave. Stresses
    /// alt-screen mode, cursor addressing, and full-grid invalidation.
    #[must_use]
    pub fn alt_screen() -> Vec<u8> {
        let mut buf = Vec::with_capacity(16 * 1024);
        buf.extend_from_slice(b"\x1b[?1049h"); // enter alt screen
        for repaint in 0..10 {
            buf.extend_from_slice(b"\x1b[2J\x1b[H"); // clear + home
            for row in 1..=24 {
                buf.extend_from_slice(format!("\x1b[{row};1H").as_bytes()); // cursor to (row,1)
                buf.extend_from_slice(
                    format!("row {row:02} repaint {repaint:02} ||||||||||||||||||||").as_bytes(),
                );
            }
        }
        buf.extend_from_slice(b"\x1b[?1049l"); // leave alt screen
        buf
    }

    /// A small in-place edit: cursor-address a few existing rows and overwrite a
    /// few cells, WITHOUT scrolling or clearing - the realistic per-frame change (a
    /// cursor move plus a few glyphs) that produces PARTIAL line damage, the path
    /// the damage-tracking renderer rides every frame. Deliberately small (unlike
    /// the bulk payloads): it is the *delta*, not a screenful.
    #[must_use]
    pub fn partial_edit() -> Vec<u8> {
        let mut buf = Vec::with_capacity(256);
        for (i, row) in [3usize, 7, 12, 18].iter().enumerate() {
            buf.extend_from_slice(format!("\x1b[{row};1Hedit {i} on row {row}").as_bytes());
        }
        buf
    }

    /// A zsh-style prompt cycle: OSC 7 (cwd), OSC 133 A/B (prompt), C (command
    /// start), command output, D (command done) - repeated so the OSC scanner and
    /// block segmenter see realistic mark density.
    #[must_use]
    pub fn prompt_cycle() -> Vec<u8> {
        let unit = b"\x1b]7;file://host/Users/dev/project\x07\
\x1b]133;A\x07user@host project % \x1b]133;B\x07ls -la\x1b]133;C\x07\
\x1b[1;34msrc\x1b[0m  Cargo.toml  README.md\r\ntotal 24\r\n\x1b]133;D;0\x07";
        let mut buf = Vec::with_capacity(unit.len() * 200);
        for _ in 0..200 {
            buf.extend_from_slice(unit);
        }
        buf
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn fixtures_are_deterministic_and_nonempty() {
            // Byte-identical across calls - the property the instruction-count
            // gate depends on (no rng, clock, or environment).
            let generators: [fn() -> Vec<u8>; 6] = [
                plaintext_scroll,
                sgr_heavy,
                unicode,
                alt_screen,
                partial_edit,
                prompt_cycle,
            ];
            for f in generators {
                let a = f();
                let b = f();
                assert_eq!(a, b, "fixture must be byte-identical across calls");
                assert!(!a.is_empty(), "fixture must be non-empty");
            }
        }
    }
}
