//! Keystroke -> PTY byte encoding for raw passthrough (ticket T-3.4).
//!
//! A neutral [`KeyStroke`] (decoupled from winit) and a mode-aware [`encode`] that
//! produces the bytes a foreground program (an alt-screen TUI, or anything reading
//! raw stdin) expects: legacy sequences, DECCKM application-cursor mode (arrows +
//! Home/End switch from `CSI` to `SS3`), and the Kitty keyboard protocol's `CSI u`
//! disambiguation when the program has enabled it.
//!
//! ## Who owns what
//!
//! The Kitty protocol NEGOTIATION (the app's `CSI ? u` query, the push/pop/set flag
//! stack, the separate main/alt-screen stacks) is owned by the pinned
//! `alacritty_terminal`: it parses those sequences and exposes the active flags on
//! [`alacritty_terminal::term::TermMode`] (`APP_CURSOR` = DECCKM,
//! `DISAMBIGUATE_ESC_CODES` = Kitty disambiguate). So - unlike the prototype's
//! `KittyKeyboard`, which had to implement that filter itself because JediTerm did
//! not - this module ports only the OUTBOUND ENCODE half and reads the live flags
//! via [`KeyEncodeFlags::from_term_mode`]. The encode tables (esp. the `CSI u`
//! promotion) match the prototype's `KittyKeyboardTest` cases (port parity).
//!
//! The router (T-3.3) calls [`encode`] only on the raw/alt-screen/in-flight paths;
//! a committed Shell-mode submit writes the whole line + newline, not per-key bytes.

use alacritty_terminal::term::TermMode;

const ESC: u8 = 0x1b;

/// A named (non-text) key, decoupled from winit. Mirrors the prototype's `NamedKey`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedKey {
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

/// A neutral keystroke: either a `code_point` (a printable char, or an already
/// control-mapped code such as Ctrl+C = 3) or a `named` key, plus modifier flags.
/// Mirrors the prototype's `KeyStroke`. Exactly one of `code_point`/`named` is the
/// payload; if both are `None` the stroke encodes to nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyStroke {
    pub code_point: Option<u32>,
    pub named: Option<NamedKey>,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

impl KeyStroke {
    /// A bare named key (no modifiers).
    #[must_use]
    pub fn named(named: NamedKey) -> Self {
        Self {
            named: Some(named),
            ..Self::default()
        }
    }

    /// A bare printable code point (no modifiers).
    #[must_use]
    pub fn char(code_point: u32) -> Self {
        Self {
            code_point: Some(code_point),
            ..Self::default()
        }
    }
}

/// The terminal-mode flags that change how a keystroke encodes. Read from the live
/// [`TermMode`] via [`Self::from_term_mode`]; the encode functions take this plain
/// struct so they stay testable without an emulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyEncodeFlags {
    /// DECCKM application-cursor mode (`TermMode::APP_CURSOR`): arrows + Home/End
    /// send `SS3` (`ESC O x`) instead of `CSI` (`ESC [ x`).
    pub app_cursor: bool,
    /// Kitty disambiguate-escape-codes (`TermMode::DISAMBIGUATE_ESC_CODES`): promote
    /// otherwise-ambiguous keystrokes to `CSI u`.
    pub disambiguate: bool,
}

impl KeyEncodeFlags {
    /// Extract the encode-relevant flags from the live terminal mode. This is the
    /// single point that maps `alacritty_terminal`'s `TermMode` bits to encoder
    /// behavior (the ticket's "query Term mode flags").
    #[must_use]
    pub fn from_term_mode(mode: TermMode) -> Self {
        Self {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            disambiguate: mode.contains(TermMode::DISAMBIGUATE_ESC_CODES),
        }
    }
}

/// Encode `stroke` to the bytes a foreground program expects under `flags`. Tries
/// the Kitty `CSI u` promotion first (only when `disambiguate` is set and the key is
/// disambiguation-critical), then falls back to the legacy / DECCKM encoding.
#[must_use]
pub fn encode(stroke: KeyStroke, flags: KeyEncodeFlags) -> Vec<u8> {
    if let Some(csi_u) = encode_kitty(stroke, flags.disambiguate) {
        return csi_u;
    }
    encode_legacy(stroke, flags.app_cursor)
}

/// The Kitty keyboard-protocol `CSI u` encoding, or `None` to fall back to the
/// legacy/text path. Ports the prototype `KittyKeyboard.encode` verbatim: only the
/// disambiguation-critical set is promoted - modified Enter/Tab/Backspace/Escape
/// (e.g. Shift+Enter -> `ESC[13;2u`, the newline-vs-submit case) and any code point
/// carrying ctrl/alt/meta (e.g. Ctrl+I -> `ESC[105;5u`, distinct from Tab). Returns
/// `None` when `disambiguate` is off, for unmodified named keys, arrows/function
/// keys, and plain/shift-only printables.
#[must_use]
pub fn encode_kitty(stroke: KeyStroke, disambiguate: bool) -> Option<Vec<u8>> {
    if !disambiguate {
        return None;
    }
    let mods = 1
        + u32::from(stroke.shift)
        + (u32::from(stroke.alt) * 2)
        + (u32::from(stroke.ctrl) * 4)
        + (u32::from(stroke.meta) * 8);

    if let Some(named) = stroke.named {
        let cp = match named {
            NamedKey::Enter => 13,
            NamedKey::Tab => 9,
            NamedKey::Backspace => 127,
            NamedKey::Escape => 27,
            // Arrows / function keys keep the legacy CSI/SS3 encoding.
            _ => return None,
        };
        if mods == 1 {
            return None; // unmodified -> legacy single byte
        }
        return Some(csi_u(cp, mods));
    }

    let cp = stroke.code_point?;
    if cp == 0 {
        return None;
    }
    if !(stroke.ctrl || stroke.alt || stroke.meta) {
        return None; // plain or shift-only printable -> send as text
    }
    // A control code (1..=26) maps back to its letter (Ctrl+I = 9 -> 'i' = 105);
    // anything else is lowercased (Ctrl+Shift+A -> 'a').
    let base = if (1..=26).contains(&cp) {
        96 + cp
    } else {
        char::from_u32(cp).map_or(cp, |c| c.to_ascii_lowercase() as u32)
    };
    Some(csi_u(base, mods))
}

fn csi_u(cp: u32, mods: u32) -> Vec<u8> {
    if mods == 1 {
        format!("\x1b[{cp}u").into_bytes()
    } else {
        format!("\x1b[{cp};{mods}u").into_bytes()
    }
}

/// The legacy / DECCKM encoding (no Kitty disambiguation). `app_cursor` switches the
/// cursor + Home/End keys from `CSI` to `SS3`. Returns bytes, or an EMPTY `Vec` for
/// an empty stroke or an un-encodable code point (a UTF-16 surrogate / `> U+10FFFF`).
/// An un-encodable code point is dropped entirely, regardless of modifiers, rather
/// than emitting a lone `ESC` prefix with no payload (which would open an escape
/// sequence on the PTY and corrupt the next byte).
#[must_use]
pub fn encode_legacy(stroke: KeyStroke, app_cursor: bool) -> Vec<u8> {
    if let Some(named) = stroke.named {
        return encode_named_legacy(named, app_cursor);
    }
    let Some(cp) = stroke.code_point else {
        return Vec::new();
    };
    // An un-encodable code point is not a real character: drop the whole stroke so
    // an `alt`/`meta` prefix never produces a dangling ESC. (Control bytes 0..=26
    // are valid `char`s, so the ctrl path below is unaffected.)
    if char::from_u32(cp).is_none() {
        return Vec::new();
    }
    // Ctrl maps a key to its control byte (Ctrl+C -> 0x03, Ctrl+Z -> 0x1a); an
    // already-control code point is preserved (`& 0x1f` is idempotent on 1..=26).
    let byte_cp = if stroke.ctrl { cp & 0x1f } else { cp };
    let mut out = Vec::new();
    // Alt/Meta prefixes the sequence with ESC (the classic meta-sends-escape).
    if stroke.alt || stroke.meta {
        out.push(ESC);
    }
    push_utf8(&mut out, byte_cp);
    out
}

/// `ESC [ x` (CSI) in normal mode, `ESC O x` (SS3) in application-cursor mode, for
/// the arrows and Home/End; fixed sequences for the rest.
fn encode_named_legacy(named: NamedKey, app_cursor: bool) -> Vec<u8> {
    // The cursor/edit keys whose form depends on DECCKM (`final` byte after CSI/SS3).
    let cursor_final = match named {
        NamedKey::Up => Some(b'A'),
        NamedKey::Down => Some(b'B'),
        NamedKey::Right => Some(b'C'),
        NamedKey::Left => Some(b'D'),
        NamedKey::Home => Some(b'H'),
        NamedKey::End => Some(b'F'),
        _ => None,
    };
    if let Some(f) = cursor_final {
        // CSI = ESC [ ; SS3 = ESC O.
        return vec![ESC, if app_cursor { b'O' } else { b'[' }, f];
    }
    match named {
        NamedKey::Enter => vec![b'\r'],
        NamedKey::Tab => vec![b'\t'],
        NamedKey::Backspace => vec![0x7f],
        NamedKey::Escape => vec![ESC],
        // `CSI <n> ~` editing keys (DECCKM-independent).
        NamedKey::Insert => csi_tilde(2),
        NamedKey::Delete => csi_tilde(3),
        NamedKey::PageUp => csi_tilde(5),
        NamedKey::PageDown => csi_tilde(6),
        // F1-F4 are SS3 P/Q/R/S; F5-F12 are `CSI <n> ~` (standard xterm).
        NamedKey::F1 => vec![ESC, b'O', b'P'],
        NamedKey::F2 => vec![ESC, b'O', b'Q'],
        NamedKey::F3 => vec![ESC, b'O', b'R'],
        NamedKey::F4 => vec![ESC, b'O', b'S'],
        NamedKey::F5 => csi_tilde(15),
        NamedKey::F6 => csi_tilde(17),
        NamedKey::F7 => csi_tilde(18),
        NamedKey::F8 => csi_tilde(19),
        NamedKey::F9 => csi_tilde(20),
        NamedKey::F10 => csi_tilde(21),
        NamedKey::F11 => csi_tilde(23),
        NamedKey::F12 => csi_tilde(24),
        // Unreachable: the cursor/edit keys are handled above.
        NamedKey::Up
        | NamedKey::Down
        | NamedKey::Left
        | NamedKey::Right
        | NamedKey::Home
        | NamedKey::End => unreachable!("handled by cursor_final"),
    }
}

fn csi_tilde(n: u32) -> Vec<u8> {
    format!("\x1b[{n}~").into_bytes()
}

/// Append `cp` as UTF-8 (an invalid code point appends nothing).
fn push_utf8(out: &mut Vec<u8>, cp: u32) {
    if let Some(c) = char::from_u32(cp) {
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ascii(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    // --- AC1: arrows are CSI in normal mode, SS3 in DECCKM -------------------

    #[test]
    fn arrows_are_csi_in_normal_mode_and_ss3_in_app_cursor_mode() {
        let normal = KeyEncodeFlags {
            app_cursor: false,
            disambiguate: false,
        };
        let app = KeyEncodeFlags {
            app_cursor: true,
            disambiguate: false,
        };
        for (key, fin) in [
            (NamedKey::Up, 'A'),
            (NamedKey::Down, 'B'),
            (NamedKey::Right, 'C'),
            (NamedKey::Left, 'D'),
        ] {
            assert_eq!(
                ascii(&encode(KeyStroke::named(key), normal)),
                format!("\x1b[{fin}"),
                "{key:?} normal -> CSI"
            );
            assert_eq!(
                ascii(&encode(KeyStroke::named(key), app)),
                format!("\x1bO{fin}"),
                "{key:?} DECCKM -> SS3"
            );
        }
        // Home/End follow the same CSI/SS3 switch.
        assert_eq!(
            ascii(&encode(KeyStroke::named(NamedKey::Home), normal)),
            "\x1b[H"
        );
        assert_eq!(
            ascii(&encode(KeyStroke::named(NamedKey::Home), app)),
            "\x1bOH"
        );
        assert_eq!(
            ascii(&encode(KeyStroke::named(NamedKey::End), normal)),
            "\x1b[F"
        );
        assert_eq!(
            ascii(&encode(KeyStroke::named(NamedKey::End), app)),
            "\x1bOF"
        );
    }

    // --- AC2: control bytes drive line-discipline signals --------------------

    #[test]
    fn ctrl_letters_encode_to_control_bytes() {
        let flags = KeyEncodeFlags::default();
        // Ctrl+C -> 0x03, whether the UI sends the letter or a pre-mapped control.
        let from_letter = KeyStroke {
            code_point: Some(u32::from('c')),
            ctrl: true,
            ..KeyStroke::default()
        };
        let from_control = KeyStroke {
            code_point: Some(3),
            ctrl: true,
            ..KeyStroke::default()
        };
        assert_eq!(encode(from_letter, flags), vec![0x03]);
        assert_eq!(encode(from_control, flags), vec![0x03]);
        // Ctrl+Z -> 0x1a (SIGTSTP).
        let ctrl_z = KeyStroke {
            code_point: Some(u32::from('z')),
            ctrl: true,
            ..KeyStroke::default()
        };
        assert_eq!(encode(ctrl_z, flags), vec![0x1a]);
    }

    // --- AC3 / AC5: Kitty CSI u disambiguation (port parity) -----------------

    #[test]
    fn kitty_disambiguation_matches_prototype_cases() {
        // These mirror KittyKeyboardTest::pushEnablesDisambiguateAndEncodes.
        // Shift+Enter -> the distinct newline sequence (Claude Code / codex case).
        assert_eq!(
            ascii(
                &encode_kitty(
                    KeyStroke {
                        named: Some(NamedKey::Enter),
                        shift: true,
                        ..KeyStroke::default()
                    },
                    true
                )
                .unwrap()
            ),
            "\x1b[13;2u"
        );
        // Plain Enter stays legacy (submit) -> no CSI u.
        assert_eq!(encode_kitty(KeyStroke::named(NamedKey::Enter), true), None);
        // Ctrl+I (control char 9) disambiguates from Tab as the 'i' key.
        assert_eq!(
            ascii(
                &encode_kitty(
                    KeyStroke {
                        code_point: Some(9),
                        ctrl: true,
                        ..KeyStroke::default()
                    },
                    true
                )
                .unwrap()
            ),
            "\x1b[105;5u"
        );
        // Ctrl+C -> 'c' (99) with the ctrl modifier (5).
        assert_eq!(
            ascii(
                &encode_kitty(
                    KeyStroke {
                        code_point: Some(3),
                        ctrl: true,
                        ..KeyStroke::default()
                    },
                    true
                )
                .unwrap()
            ),
            "\x1b[99;5u"
        );
        // A plain printable goes through the text path (no CSI u).
        assert_eq!(encode_kitty(KeyStroke::char(97), true), None);
    }

    #[test]
    fn no_kitty_encoding_when_flag_inactive() {
        // KittyKeyboardTest::noEncodingWhenFlagsInactive: disambiguate off -> None,
        // and the unified encode() falls back to legacy (Shift+Enter -> CR).
        let shift_enter = KeyStroke {
            named: Some(NamedKey::Enter),
            shift: true,
            ..KeyStroke::default()
        };
        assert_eq!(encode_kitty(shift_enter, false), None);
        assert_eq!(encode(shift_enter, KeyEncodeFlags::default()), vec![b'\r']);
    }

    #[test]
    fn disambiguate_promotes_through_the_unified_encode() {
        // AC3 end-to-end: with the flag live, encode() returns the CSI u directly.
        let flags = KeyEncodeFlags {
            app_cursor: false,
            disambiguate: true,
        };
        let shift_enter = KeyStroke {
            named: Some(NamedKey::Enter),
            shift: true,
            ..KeyStroke::default()
        };
        assert_eq!(ascii(&encode(shift_enter, flags)), "\x1b[13;2u");
    }

    // --- legacy named keys + text --------------------------------------------

    #[test]
    fn basic_named_keys_and_function_keys() {
        let f = KeyEncodeFlags::default();
        assert_eq!(encode(KeyStroke::named(NamedKey::Enter), f), vec![b'\r']);
        assert_eq!(encode(KeyStroke::named(NamedKey::Tab), f), vec![b'\t']);
        assert_eq!(encode(KeyStroke::named(NamedKey::Backspace), f), vec![0x7f]);
        assert_eq!(encode(KeyStroke::named(NamedKey::Escape), f), vec![0x1b]);
        assert_eq!(
            ascii(&encode(KeyStroke::named(NamedKey::Delete), f)),
            "\x1b[3~"
        );
        assert_eq!(
            ascii(&encode(KeyStroke::named(NamedKey::PageUp), f)),
            "\x1b[5~"
        );
        assert_eq!(ascii(&encode(KeyStroke::named(NamedKey::F1), f)), "\x1bOP");
        assert_eq!(
            ascii(&encode(KeyStroke::named(NamedKey::F5), f)),
            "\x1b[15~"
        );
        assert_eq!(
            ascii(&encode(KeyStroke::named(NamedKey::F12), f)),
            "\x1b[24~"
        );
    }

    #[test]
    fn printable_and_alt_prefixed_text() {
        let f = KeyEncodeFlags::default();
        // Plain 'a'.
        assert_eq!(encode(KeyStroke::char(u32::from('a')), f), vec![b'a']);
        // Alt+a -> ESC a (meta sends escape).
        let alt_a = KeyStroke {
            code_point: Some(u32::from('a')),
            alt: true,
            ..KeyStroke::default()
        };
        assert_eq!(encode(alt_a, f), vec![ESC, b'a']);
        // A multibyte code point round-trips as UTF-8.
        assert_eq!(encode(KeyStroke::char(u32::from('é')), f), "é".as_bytes());
    }

    #[test]
    fn un_encodable_code_point_is_dropped_not_a_dangling_esc() {
        // A UTF-16 surrogate / out-of-range code point is not a real character and
        // must yield NO bytes regardless of modifiers - never a lone ESC prefix
        // (which would open an escape sequence on the PTY and corrupt the next byte).
        let f = KeyEncodeFlags::default();
        let surrogate = 0xD800; // a lone surrogate: char::from_u32 -> None
        let too_big = 0x11_0000; // > U+10FFFF
        for cp in [surrogate, too_big] {
            assert_eq!(
                encode(KeyStroke::char(cp), f),
                Vec::<u8>::new(),
                "plain dropped"
            );
            let alt = KeyStroke {
                code_point: Some(cp),
                alt: true,
                ..KeyStroke::default()
            };
            assert!(
                encode(alt, f).is_empty(),
                "alt + un-encodable must NOT emit a lone ESC (cp={cp:#x})"
            );
            let ctrl_alt = KeyStroke {
                code_point: Some(cp),
                ctrl: true,
                alt: true,
                ..KeyStroke::default()
            };
            assert!(
                encode(ctrl_alt, f).is_empty(),
                "ctrl+alt + un-encodable must not emit ESC+NUL (cp={cp:#x})"
            );
        }
    }

    // --- AC4: a TUI fixture sees the expected bytes --------------------------

    #[test]
    fn tui_fixture_round_trip_sees_expected_bytes() {
        // Simulate the keys a vim-like app (alt-screen, DECCKM on) reads: arrow up,
        // 'i' to insert, Escape, Ctrl+C. Concatenated, the fixture must see exactly
        // these bytes.
        let app = KeyEncodeFlags {
            app_cursor: true,
            disambiguate: false,
        };
        let mut seen = Vec::new();
        seen.extend(encode(KeyStroke::named(NamedKey::Up), app)); // SS3 A
        seen.extend(encode(KeyStroke::char(u32::from('i')), app)); // 'i'
        seen.extend(encode(KeyStroke::named(NamedKey::Escape), app)); // ESC
        seen.extend(encode(
            KeyStroke {
                code_point: Some(u32::from('c')),
                ctrl: true,
                ..KeyStroke::default()
            },
            app,
        )); // 0x03
            // ESC O A (up, SS3) + 'i' + ESC + 0x03 (Ctrl+C).
        assert_eq!(seen, b"\x1bOAi\x1b\x03".to_vec());
    }

    // --- flag extraction from the live TermMode ------------------------------

    #[test]
    fn flags_are_read_from_term_mode() {
        assert_eq!(
            KeyEncodeFlags::from_term_mode(TermMode::APP_CURSOR),
            KeyEncodeFlags {
                app_cursor: true,
                disambiguate: false
            }
        );
        assert_eq!(
            KeyEncodeFlags::from_term_mode(TermMode::DISAMBIGUATE_ESC_CODES),
            KeyEncodeFlags {
                app_cursor: false,
                disambiguate: true
            }
        );
        assert_eq!(
            KeyEncodeFlags::from_term_mode(TermMode::NONE),
            KeyEncodeFlags::default()
        );
    }
}
