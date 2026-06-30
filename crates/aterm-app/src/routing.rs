//! The input disposition brain (ticket T-3.3): given a key and the live session
//! context, decide WHERE the input goes - the shell PTY, the agent, a mode toggle,
//! an agent interrupt, or ordinary editing of the input line.
//!
//! Pure and headless-testable: [`decide`] is a priority-ordered set of gates over
//! plain inputs, no I/O. `Session` (session.rs) builds the [`RoutingContext`] from
//! live state, calls [`decide`], and performs the chosen [`Disposition`]. The pure
//! [`aterm_core::InputModel`] never decides whether Enter submits (ticket T-3.1
//! caller-owns-submit); this brain is that caller.
//!
//! Gate order (`05-unified-input-ux.md` sections 2, 4; Recommendations 5, 10-11):
//! 1. preedit-active -> [`Disposition::ImeComposing`] (the IME owns Enter/Tab/Esc;
//!    never submit or route mid-composition).
//! 2. the mode-toggle hotkey -> [`Disposition::ToggleMode`] (flips `mode` only).
//! 3. Esc during an agent turn -> [`Disposition::InterruptAgent`] (owner
//!    open-question #2 DEFAULT: Esc always interrupts the agent; flagged).
//! 4. alt-Enter (Opt-Enter) -> [`Disposition::SubmitAgent`] regardless of mode
//!    (owner open-question #7 default - the prototype's one-shot send-to-agent).
//! 5. degraded (integration `None`) / alt-screen / in-flight -> raw
//!    [`Disposition::PassthroughToPty`] (a classic ZLE line editor or a foreground
//!    TUI owns the keys; T-3.4 encodes the bytes).
//! 6. Enter -> route by mode: Shell submits to the PTY, Agent dispatches to the agent.
//! 7. anything else -> [`Disposition::Edit`] (the key edits the input line).
//!
//! ## What this ticket wires vs. defers
//!
//! The DECISION is complete and tested here, and the live wiring now sources the
//! real chords: [`classify`] maps a [`KeyPress`] (carrying keyboard MODIFIERS + the
//! logical character) to a [`KeyInput`], so the configurable `Cmd-/` toggle (via
//! [`KeyBinding`]) and `Opt-Enter` work and `Tab` is freed. `alt_screen`, the
//! degraded/`None` integration state, and `foreground_reading_stdin` (a foreign
//! foreground process group) are all sourced live. Still gated on other tickets:
//! `preedit_active` (T-3.2 IME) and `agent_turn_active` (EPIC-5 agent loop) read
//! `false` until those land. See session.rs and the ticket Notes.

use aterm_core::{keys, InputMode};
use aterm_ui::{KeyPress, Mods, NamedKey};

/// The routing-relevant classification of a key event. The session maps a winit key
/// to one of these; everything routing cares about is captured here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyInput {
    /// Return/Enter. `alt` is the Opt/Alt modifier (alt-Enter = one-shot to agent).
    Enter { alt: bool },
    /// The Escape key.
    Escape,
    /// The resolved mode-toggle chord - the configurable [`KeyBinding`] (the
    /// dossier default `Cmd-/`), matched by [`classify`].
    ToggleHotkey,
    /// Any other key: editing the input box, or raw passthrough in a
    /// degraded/alt-screen/in-flight context.
    Other,
}

/// The live session state the gates read. The session fills this each keystroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutingContext {
    /// Where Enter routes when nothing higher-priority claims the key.
    pub mode: InputMode,
    /// An IME composition is active (T-3.2): the IME owns Enter/Tab/Esc.
    pub preedit_active: bool,
    /// Shell integration is degraded to `None` (no shim): classic raw/ZLE
    /// passthrough (T-2.6).
    pub degraded: bool,
    /// A full-screen (alt-screen) program is active: keys belong to it.
    pub alt_screen: bool,
    /// A foreground program is reading stdin (in-flight): keys pass through.
    pub foreground_reading_stdin: bool,
    /// An agent turn is in progress (so Esc can interrupt it).
    pub agent_turn_active: bool,
}

/// The decision: what to do with the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// The IME owns the key; the routing layer does nothing.
    ImeComposing,
    /// Flip the input mode (text preserved). The hotkey calls `InputModel::toggle_mode`.
    ToggleMode,
    /// Interrupt the in-progress agent turn.
    InterruptAgent,
    /// Send the input text to the agent (Enter in Agent mode, or alt-Enter anywhere).
    SubmitAgent,
    /// Pass the raw key through to the PTY (alt-screen / foreground stdin / degraded).
    /// T-3.4 owns the key->bytes encoding.
    PassthroughToPty,
    /// Submit the committed input line to the shell PTY (Enter in Shell mode).
    SubmitShell,
    /// Ordinary editing of the input line (the default).
    Edit,
}

/// Decide the [`Disposition`] for `key` given the live `ctx`. Pure: a priority-
/// ordered set of gates, no I/O. The model's risk/opinion never enters here.
#[must_use]
pub fn decide(key: KeyInput, ctx: &RoutingContext) -> Disposition {
    // 1. IME composition owns everything (Enter/Tab/Esc never submit or route).
    if ctx.preedit_active {
        return Disposition::ImeComposing;
    }
    // 2. The mode-toggle hotkey is always available; it flips mode only.
    if matches!(key, KeyInput::ToggleHotkey) {
        return Disposition::ToggleMode;
    }
    // 3. Esc interrupts an in-progress agent turn (owner Q#2 default: Esc always
    //    interrupts). Placed above passthrough so an interrupt is never swallowed by
    //    a TUI; flagged as the default policy pending owner confirmation.
    if matches!(key, KeyInput::Escape) && ctx.agent_turn_active {
        return Disposition::InterruptAgent;
    }
    // 4. alt-Enter (Opt-Enter): one-shot to the agent regardless of mode (Q#7).
    if matches!(key, KeyInput::Enter { alt: true }) {
        return Disposition::SubmitAgent;
    }
    // 5. Degraded/raw, alt-screen, or a foreground program reading stdin: the keys
    //    belong to the PTY (a ZLE line editor or a TUI). T-3.4 encodes the bytes.
    if ctx.degraded || ctx.alt_screen || ctx.foreground_reading_stdin {
        return Disposition::PassthroughToPty;
    }
    // 6. Enter routes by mode.
    if matches!(key, KeyInput::Enter { alt: false }) {
        return match ctx.mode {
            InputMode::Shell => Disposition::SubmitShell,
            InputMode::Agent => Disposition::SubmitAgent,
        };
    }
    // 7. Ordinary editing of the input line.
    Disposition::Edit
}

/// The key half of a [`KeyBinding`]: either a logical character (matched against
/// [`KeyPress::ch`], e.g. `/`) or a named key (matched against [`KeyPress::named`],
/// e.g. Enter/Tab).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindKey {
    /// A logical character; matched case-insensitively so a `cmd+k` spec fires on
    /// the `k` the OS reports (it does not require the spec to predict the case).
    Char(char),
    /// A named key.
    Named(NamedKey),
}

/// A configurable key chord - currently just the mode-toggle hotkey (ticket T-3.3),
/// default `Cmd-/`. Rebindable via config: the `ATERM_TOGGLE_KEY` env override
/// today (e.g. `ctrl+t`), the `config.toml` loader later (EPIC-8).
///
/// Matching is **exact on modifiers**: every required modifier must be held and no
/// others, so `Cmd-/` does not also fire on `Cmd-Shift-/` (which is `Cmd-?`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyBinding {
    pub key: BindKey,
    pub mods: Mods,
}

impl KeyBinding {
    /// The dossier default toggle chord: `Cmd-/` (rejecting `Ctrl-Space` = macOS
    /// IME switch and `Cmd-.` = SIGINT muscle memory; see the ticket).
    #[must_use]
    pub fn default_toggle() -> Self {
        Self {
            key: BindKey::Char('/'),
            mods: Mods {
                cmd: true,
                ..Mods::default()
            },
        }
    }

    /// The default autonomy-cycle chord (ticket T-5.11): `Cmd-Shift-A` (A = autonomy).
    /// Distinct from the `Cmd-/` mode toggle so the two postures (routing target vs
    /// safety tier) never collide. Owner-confirm: the exact chord is a UX choice.
    #[must_use]
    pub fn default_autonomy_cycle() -> Self {
        Self {
            key: BindKey::Char('a'),
            mods: Mods {
                cmd: true,
                shift: true,
                ..Mods::default()
            },
        }
    }

    /// Whether `key` is exactly this chord: the modifiers match exactly and the base
    /// key matches (a character case-insensitively, a named key exactly).
    #[must_use]
    pub fn matches(&self, key: &KeyPress) -> bool {
        self.mods == key.mods
            && match self.key {
                BindKey::Char(c) => key.ch.is_some_and(|k| k.eq_ignore_ascii_case(&c)),
                BindKey::Named(n) => key.named == Some(n),
            }
    }

    /// Parse a chord like `"cmd+/"`, `"ctrl+t"`, or `"alt+enter"`: case-insensitive,
    /// `+`-separated. Modifier tokens: `cmd`/`super`/`meta`/`win`, `alt`/`opt`/
    /// `option`, `ctrl`/`control`, `shift`. The one remaining token is the base key:
    /// a named key (`enter`/`return`, `tab`, `escape`/`esc`, `space`) or a single
    /// character. Returns `None` on an empty, ambiguous, or unrecognized spec. (The
    /// full named-key vocabulary arrives with the EPIC-8 config loader; this covers
    /// reasonable toggle chords + the env override.)
    #[must_use]
    pub fn parse(spec: &str) -> Option<Self> {
        let mut mods = Mods::default();
        let mut key: Option<BindKey> = None;
        for tok in spec.split('+') {
            let t = tok.trim();
            if t.is_empty() {
                return None;
            }
            match t.to_ascii_lowercase().as_str() {
                "cmd" | "super" | "meta" | "win" => mods.cmd = true,
                "alt" | "opt" | "option" => mods.alt = true,
                "ctrl" | "control" => mods.ctrl = true,
                "shift" => mods.shift = true,
                lower => {
                    if key.is_some() {
                        return None; // more than one base key
                    }
                    key = Some(parse_bindkey(t, lower)?);
                }
            }
        }
        Some(Self { key: key?, mods })
    }
}

/// Parse the base-key token of a [`KeyBinding`]: a known named key by word, else a
/// single character (`orig` preserves the user's case for the char).
fn parse_bindkey(orig: &str, lower: &str) -> Option<BindKey> {
    let named = match lower {
        "enter" | "return" => Some(NamedKey::Enter),
        "tab" => Some(NamedKey::Tab),
        "escape" | "esc" => Some(NamedKey::Escape),
        "space" => Some(NamedKey::Space),
        _ => None,
    };
    if let Some(n) = named {
        return Some(BindKey::Named(n));
    }
    let mut chars = orig.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None; // multi-char token that is not a known key name
    }
    Some(BindKey::Char(c))
}

/// Classify a [`KeyPress`] into the neutral [`KeyInput`] the brain decides on, given
/// the configured mode-toggle `binding`. The toggle binding wins first (so its chord
/// is a toggle even though it carries a character); then Enter carries the Opt/Alt
/// modifier (the one-shot-to-agent), Escape is itself, and everything else - now
/// including a freed `Tab` - is `Other` (ordinary editing, or raw passthrough by
/// context). This is the winit->routing boundary the modifier seam unblocks.
#[must_use]
pub fn classify(key: &KeyPress, binding: &KeyBinding) -> KeyInput {
    if binding.matches(key) {
        return KeyInput::ToggleHotkey;
    }
    match key.named {
        Some(NamedKey::Enter) => KeyInput::Enter { alt: key.mods.alt },
        Some(NamedKey::Escape) => KeyInput::Escape,
        _ => KeyInput::Other,
    }
}

/// Map a UI [`KeyPress`] to the core [`keys::KeyStroke`] for raw passthrough
/// encoding (ticket T-3.4's encoder, adopted on the alt-screen / foreground /
/// degraded paths). Returns `None` when there is nothing to send to the PTY:
///
/// - **`Cmd`/Super held**: on macOS Command is an app-level modifier (menu chords,
///   the mode toggle) and is never forwarded to the foreground program, so a
///   Cmd-modified press maps to nothing here.
/// - an unmapped named key (a key the encoder has no sequence for), or a press that
///   is neither a named key nor a character.
///
/// `Space` is a printable (`U+0020`), not one of the encoder's named keys, so it is
/// mapped to a code point. Ctrl/Alt/Shift carry through; `meta` stays false (we do
/// not map Super onto the legacy meta-sends-escape).
#[must_use]
pub fn keystroke_for(key: &KeyPress) -> Option<keys::KeyStroke> {
    if key.mods.cmd {
        return None;
    }
    let (named, code_point) = match key.named {
        // Space is a printable, not an encoder named key.
        Some(NamedKey::Space) => (None, Some(u32::from(' '))),
        Some(n) => match map_named(n) {
            Some(k) => (Some(k), None),
            None => return None, // a named key the encoder does not handle
        },
        None => (None, key.ch.map(u32::from)),
    };
    if named.is_none() && code_point.is_none() {
        return None;
    }
    Some(keys::KeyStroke {
        code_point,
        named,
        ctrl: key.mods.ctrl,
        alt: key.mods.alt,
        shift: key.mods.shift,
        meta: false,
    })
}

/// Map a winit [`NamedKey`] to the encoder's [`keys::NamedKey`], or `None` for a key
/// it has no passthrough sequence for (`Space` is handled by [`keystroke_for`] as a
/// printable, so it is not here).
fn map_named(n: NamedKey) -> Option<keys::NamedKey> {
    use keys::NamedKey as K;
    Some(match n {
        NamedKey::Enter => K::Enter,
        NamedKey::Tab => K::Tab,
        NamedKey::Backspace => K::Backspace,
        NamedKey::Escape => K::Escape,
        NamedKey::ArrowUp => K::Up,
        NamedKey::ArrowDown => K::Down,
        NamedKey::ArrowLeft => K::Left,
        NamedKey::ArrowRight => K::Right,
        NamedKey::Home => K::Home,
        NamedKey::End => K::End,
        NamedKey::PageUp => K::PageUp,
        NamedKey::PageDown => K::PageDown,
        NamedKey::Insert => K::Insert,
        NamedKey::Delete => K::Delete,
        NamedKey::F1 => K::F1,
        NamedKey::F2 => K::F2,
        NamedKey::F3 => K::F3,
        NamedKey::F4 => K::F4,
        NamedKey::F5 => K::F5,
        NamedKey::F6 => K::F6,
        NamedKey::F7 => K::F7,
        NamedKey::F8 => K::F8,
        NamedKey::F9 => K::F9,
        NamedKey::F10 => K::F10,
        NamedKey::F11 => K::F11,
        NamedKey::F12 => K::F12,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An integrated, idle Shell context (no IME, no agent turn, no alt-screen).
    fn shell_ctx() -> RoutingContext {
        RoutingContext {
            mode: InputMode::Shell,
            preedit_active: false,
            degraded: false,
            alt_screen: false,
            foreground_reading_stdin: false,
            agent_turn_active: false,
        }
    }

    #[test]
    fn enter_routes_by_mode() {
        // AC2: Enter in Shell submits to the PTY; Enter in Agent dispatches to agent.
        let mut ctx = shell_ctx();
        assert_eq!(
            decide(KeyInput::Enter { alt: false }, &ctx),
            Disposition::SubmitShell
        );
        ctx.mode = InputMode::Agent;
        assert_eq!(
            decide(KeyInput::Enter { alt: false }, &ctx),
            Disposition::SubmitAgent
        );
    }

    #[test]
    fn alt_enter_sends_to_agent_even_in_shell_mode() {
        // AC3: Opt-Enter sends to the agent regardless of mode.
        let ctx = shell_ctx();
        assert_eq!(
            decide(KeyInput::Enter { alt: true }, &ctx),
            Disposition::SubmitAgent
        );
        let agent = RoutingContext {
            mode: InputMode::Agent,
            ..shell_ctx()
        };
        assert_eq!(
            decide(KeyInput::Enter { alt: true }, &agent),
            Disposition::SubmitAgent
        );
    }

    #[test]
    fn toggle_hotkey_flips_mode_in_any_state() {
        // AC1: the toggle hotkey is always a ToggleMode (the action preserves text).
        // It wins over passthrough states too (the chord is global).
        for ctx in [
            shell_ctx(),
            RoutingContext {
                alt_screen: true,
                ..shell_ctx()
            },
            RoutingContext {
                degraded: true,
                ..shell_ctx()
            },
        ] {
            assert_eq!(
                decide(KeyInput::ToggleHotkey, &ctx),
                Disposition::ToggleMode
            );
        }
    }

    #[test]
    fn ime_composition_owns_enter_and_never_submits() {
        // AC4: during IME composition, Enter does not submit/route (joint T-3.2).
        let ctx = RoutingContext {
            preedit_active: true,
            ..shell_ctx()
        };
        assert_eq!(
            decide(KeyInput::Enter { alt: false }, &ctx),
            Disposition::ImeComposing
        );
        // Even alt-Enter and the toggle defer to the IME while composing.
        assert_eq!(
            decide(KeyInput::Enter { alt: true }, &ctx),
            Disposition::ImeComposing
        );
        assert_eq!(
            decide(KeyInput::ToggleHotkey, &ctx),
            Disposition::ImeComposing
        );
        assert_eq!(decide(KeyInput::Escape, &ctx), Disposition::ImeComposing);
    }

    #[test]
    fn alt_screen_and_in_flight_and_degraded_pass_through_to_pty() {
        // AC5/AC7: a foreground TUI (alt-screen), a program reading stdin, or a
        // degraded (no-integration) shell all take the keys raw.
        for ctx in [
            RoutingContext {
                alt_screen: true,
                ..shell_ctx()
            },
            RoutingContext {
                foreground_reading_stdin: true,
                ..shell_ctx()
            },
            RoutingContext {
                degraded: true,
                ..shell_ctx()
            },
        ] {
            assert_eq!(decide(KeyInput::Other, &ctx), Disposition::PassthroughToPty);
            // Enter, too, belongs to the PTY/TUI in these states - NOT a shell submit.
            assert_eq!(
                decide(KeyInput::Enter { alt: false }, &ctx),
                Disposition::PassthroughToPty
            );
        }
    }

    #[test]
    fn esc_interrupts_an_in_progress_agent_turn() {
        // AC6: Esc interrupts an in-progress agent turn (stub-verifiable).
        let ctx = RoutingContext {
            agent_turn_active: true,
            ..shell_ctx()
        };
        assert_eq!(decide(KeyInput::Escape, &ctx), Disposition::InterruptAgent);
        // With no agent turn, Esc is just ordinary input (not an interrupt).
        assert_eq!(decide(KeyInput::Escape, &shell_ctx()), Disposition::Edit);
    }

    #[test]
    fn esc_interrupt_outranks_passthrough_but_ime_outranks_esc() {
        // The interrupt must not be swallowed by a TUI (Q#2 default: Esc always
        // interrupts), but an active IME composition still owns Esc (cancels it).
        let tui_turn = RoutingContext {
            agent_turn_active: true,
            alt_screen: true,
            ..shell_ctx()
        };
        assert_eq!(
            decide(KeyInput::Escape, &tui_turn),
            Disposition::InterruptAgent
        );
        let composing_turn = RoutingContext {
            agent_turn_active: true,
            preedit_active: true,
            ..shell_ctx()
        };
        assert_eq!(
            decide(KeyInput::Escape, &composing_turn),
            Disposition::ImeComposing
        );
    }

    #[test]
    fn ordinary_keys_edit_the_input_line() {
        assert_eq!(decide(KeyInput::Other, &shell_ctx()), Disposition::Edit);
        let agent = RoutingContext {
            mode: InputMode::Agent,
            ..shell_ctx()
        };
        assert_eq!(decide(KeyInput::Other, &agent), Disposition::Edit);
    }

    #[test]
    fn alt_enter_outranks_passthrough_and_mode_routing() {
        // alt-Enter is gate 4 (above passthrough/Enter-by-mode): even in an
        // alt-screen TUI, Opt-Enter is a deliberate one-shot to the agent.
        let ctx = RoutingContext {
            alt_screen: true,
            ..shell_ctx()
        };
        assert_eq!(
            decide(KeyInput::Enter { alt: true }, &ctx),
            Disposition::SubmitAgent
        );
    }

    // --- The winit->routing classification (the modifier seam, T-3.3) ---

    fn mods(cmd: bool, alt: bool, ctrl: bool, shift: bool) -> Mods {
        Mods {
            cmd,
            alt,
            ctrl,
            shift,
        }
    }

    fn kp(named: Option<NamedKey>, ch: Option<char>, mods: Mods) -> KeyPress<'static> {
        KeyPress {
            named,
            ch,
            text: None,
            mods,
        }
    }

    #[test]
    fn default_toggle_chord_is_cmd_slash() {
        // AC1: the dossier default toggle is `Cmd-/` and classifies as ToggleHotkey.
        let binding = KeyBinding::default_toggle();
        let cmd_slash = kp(None, Some('/'), mods(true, false, false, false));
        assert!(binding.matches(&cmd_slash));
        assert_eq!(classify(&cmd_slash, &binding), KeyInput::ToggleHotkey);
        // A bare `/` (no Cmd) is ordinary input, never the toggle.
        let bare_slash = kp(None, Some('/'), mods(false, false, false, false));
        assert_eq!(classify(&bare_slash, &binding), KeyInput::Other);
    }

    #[test]
    fn default_autonomy_cycle_chord_is_cmd_shift_a_and_distinct_from_the_toggle() {
        // T-5.11: the autonomy-cycle hotkey is `Cmd-Shift-A`, recognized as its own
        // chord and NEVER colliding with the `Cmd-/` mode toggle (the two postures -
        // routing target vs safety tier - must never fire each other).
        let autonomy = KeyBinding::default_autonomy_cycle();
        let toggle = KeyBinding::default_toggle();

        let cmd_shift_a = kp(None, Some('A'), mods(true, false, false, true));
        assert!(
            autonomy.matches(&cmd_shift_a),
            "Cmd-Shift-A is the cycle chord"
        );
        // Case-insensitive on the character (the OS may report 'A' with Shift held).
        let cmd_shift_lower_a = kp(None, Some('a'), mods(true, false, false, true));
        assert!(autonomy.matches(&cmd_shift_lower_a));

        // It is not the toggle, and the toggle chord is not the cycle.
        assert!(!toggle.matches(&cmd_shift_a));
        let cmd_slash = kp(None, Some('/'), mods(true, false, false, false));
        assert!(!autonomy.matches(&cmd_slash));
        // Exact on modifiers: a bare `A` (no Cmd/Shift) never cycles autonomy.
        let bare_a = kp(None, Some('a'), mods(false, false, false, false));
        assert!(!autonomy.matches(&bare_a));
    }

    #[test]
    fn toggle_match_is_exact_on_modifiers() {
        // `Cmd-Shift-/` (= `Cmd-?`) must NOT fire the `Cmd-/` toggle - exact mods.
        let binding = KeyBinding::default_toggle();
        let cmd_shift_slash = kp(None, Some('/'), mods(true, false, false, true));
        assert!(!binding.matches(&cmd_shift_slash));
        assert_eq!(classify(&cmd_shift_slash, &binding), KeyInput::Other);
    }

    #[test]
    fn classify_reads_opt_enter_and_plain_enter() {
        // AC3: Opt-Enter carries alt=true (one-shot to agent); plain Enter alt=false.
        let binding = KeyBinding::default_toggle();
        let opt_enter = kp(Some(NamedKey::Enter), None, mods(false, true, false, false));
        assert_eq!(
            classify(&opt_enter, &binding),
            KeyInput::Enter { alt: true }
        );
        let enter = kp(
            Some(NamedKey::Enter),
            None,
            mods(false, false, false, false),
        );
        assert_eq!(classify(&enter, &binding), KeyInput::Enter { alt: false });
    }

    #[test]
    fn classify_frees_tab_and_passes_escape() {
        // `Tab` is freed (no longer the placeholder toggle): it is ordinary `Other`.
        let binding = KeyBinding::default_toggle();
        let tab = kp(Some(NamedKey::Tab), None, mods(false, false, false, false));
        assert_eq!(classify(&tab, &binding), KeyInput::Other);
        let esc = kp(
            Some(NamedKey::Escape),
            None,
            mods(false, false, false, false),
        );
        assert_eq!(classify(&esc, &binding), KeyInput::Escape);
    }

    #[test]
    fn rebinding_the_toggle_changes_the_chord() {
        // AC: the toggle is rebindable. With `ctrl+t` bound, Ctrl-T toggles and the
        // old `Cmd-/` no longer does.
        let binding = KeyBinding::parse("ctrl+t").expect("ctrl+t parses");
        let ctrl_t = kp(None, Some('t'), mods(false, false, true, false));
        assert_eq!(classify(&ctrl_t, &binding), KeyInput::ToggleHotkey);
        let cmd_slash = kp(None, Some('/'), mods(true, false, false, false));
        assert_eq!(classify(&cmd_slash, &binding), KeyInput::Other);
    }

    #[test]
    fn binding_parse_round_trips_and_rejects_junk() {
        assert_eq!(
            KeyBinding::parse("cmd+/"),
            Some(KeyBinding::default_toggle())
        );
        // Modifier order is irrelevant; a named base key is recognized.
        assert_eq!(
            KeyBinding::parse("alt+enter"),
            Some(KeyBinding {
                key: BindKey::Named(NamedKey::Enter),
                mods: mods(false, true, false, false),
            })
        );
        // Char match is case-insensitive, so `cmd+k` fires on the OS's lowercase `k`.
        let k_binding = KeyBinding::parse("cmd+k").expect("cmd+k parses");
        assert!(k_binding.matches(&kp(None, Some('k'), mods(true, false, false, false))));
        // Rejections: empty, no base key, two base keys, dangling separators.
        assert_eq!(KeyBinding::parse(""), None);
        assert_eq!(KeyBinding::parse("cmd"), None);
        assert_eq!(KeyBinding::parse("a+b"), None);
        assert_eq!(KeyBinding::parse("cmd+"), None);
    }

    // --- The KeyPress -> keys::KeyStroke mapping for raw passthrough (T-3.4) ---

    #[test]
    fn keystroke_cmd_is_never_forwarded_to_the_pty() {
        // Cmd/Super is an app-level modifier on macOS - it must not reach the program.
        assert!(keystroke_for(&kp(None, Some('k'), mods(true, false, false, false))).is_none());
        // An unmapped named key (CapsLock) also maps to nothing.
        let caps = kp(
            Some(NamedKey::CapsLock),
            None,
            mods(false, false, false, false),
        );
        assert!(keystroke_for(&caps).is_none());
    }

    #[test]
    fn keystroke_maps_space_char_and_named_keys() {
        // Space is a printable, not an encoder named key.
        let space = keystroke_for(&kp(Some(NamedKey::Space), None, Mods::default())).unwrap();
        assert_eq!(space.code_point, Some(u32::from(' ')));
        assert_eq!(space.named, None);
        // A plain character carries through as a code point.
        let a = keystroke_for(&kp(None, Some('a'), Mods::default())).unwrap();
        assert_eq!(a.code_point, Some(u32::from('a')));
        // A named arrow maps to the encoder's named key + carries modifiers.
        let up = keystroke_for(&kp(
            Some(NamedKey::ArrowUp),
            None,
            mods(false, false, true, false),
        ))
        .unwrap();
        assert_eq!(up.named, Some(keys::NamedKey::Up));
        assert!(up.ctrl);
    }

    #[test]
    fn keystroke_feeds_the_encoder_end_to_end() {
        // The whole point of the adoption: Ctrl-C reaches the PTY as 0x03, and an
        // arrow switches CSI<->SS3 with the live DECCKM flag - via the real encoder.
        let ctrl_c = keystroke_for(&kp(None, Some('c'), mods(false, false, true, false))).unwrap();
        assert_eq!(
            keys::encode(ctrl_c, keys::KeyEncodeFlags::default()),
            vec![0x03]
        );

        let up = keystroke_for(&kp(Some(NamedKey::ArrowUp), None, Mods::default())).unwrap();
        let csi = keys::KeyEncodeFlags {
            app_cursor: false,
            disambiguate: false,
        };
        let ss3 = keys::KeyEncodeFlags {
            app_cursor: true,
            disambiguate: false,
        };
        assert_eq!(keys::encode(up, csi), b"\x1b[A".to_vec());
        assert_eq!(keys::encode(up, ss3), b"\x1bOA".to_vec());
    }
}
