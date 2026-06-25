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
//! The DECISION is complete and tested here. The session wiring is honest about
//! what is not yet sourced live: the real toggle chord (`Cmd-/`) and `Opt-Enter`
//! need keyboard MODIFIERS, which the `aterm-ui` `on_key` seam does not yet pass
//! (so session uses a `Tab` placeholder for the toggle and cannot see `alt`);
//! `preedit_active` is always false until the IME lands (T-3.2); `agent_turn_active`
//! is always false until the agent loop lands (EPIC-5); `foreground_reading_stdin`
//! is not yet detected. `alt_screen` and the degraded/`None` integration state ARE
//! sourced live. See session.rs and the ticket Notes.

use aterm_core::InputMode;

/// The routing-relevant classification of a key event. The session maps a winit key
/// to one of these; everything routing cares about is captured here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyInput {
    /// Return/Enter. `alt` is the Opt/Alt modifier (alt-Enter = one-shot to agent).
    Enter { alt: bool },
    /// The Escape key.
    Escape,
    /// The resolved mode-toggle chord (the dossier's `Cmd-/`; a `Tab` placeholder
    /// until the modifier seam lands).
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
}
