//! The terminal session: the thin "wire" layer between the headless engine and
//! the UI. It owns the [`aterm_core::Engine`] (the three-thread reader/model
//! pipeline) and the pure [`InputModel`] reducer, and implements
//! [`aterm_ui::UiCallbacks`] so the UI event loop pulls the latest snapshot each
//! frame and pushes keystrokes here, which route to the engine in Shell mode.
//!
//! The engine owns the PTY, the VT terminal model, the OSC scanner, and the block
//! list on its own model thread (ticket T-1.3); this session no longer parses VT
//! bytes on the render thread. The three threads are: (1) the PTY reader thread
//! and (2) the model thread, both inside `aterm-core`'s [`aterm_core::Engine`],
//! and (3) the winit/render main thread that owns this session. The agent turn runs
//! on a fourth executor - the [`AgentRuntime`]'s tokio worker threads (ticket
//! T-5.11) - off the render thread, streaming its steps back through the engine's
//! agent-injector mailbox.

use std::path::PathBuf;
use std::sync::Arc;

use aterm_agent::{AutonomyState, Secrets};
use aterm_core::{
    keys, BlockList, Engine, IntegrationStatus, PtyDimensions, Snapshot, DEFAULT_SCROLLBACK,
};
use aterm_ui::{ImeEvent, KeyPress, NamedKey, UiCallbacks, Window};

use aterm_core::{InputEvent, InputMode, InputModel, Motion, Preedit};

use crate::agent_runtime::{AgentRuntime, TurnHandle};
use crate::config::Config;
use crate::routing::{classify, decide, keystroke_for, Disposition, KeyBinding, RoutingContext};

/// One terminal session.
///
/// **Field order is load-bearing for clean shutdown (ticket T-5.11).** Rust drops
/// struct fields in declaration order, so `agent` (the tokio runtime) and `turn` are
/// declared BEFORE `engine`: an in-flight turn's tokio task owns an
/// [`aterm_core::AgentInjector`] - a clone of the engine's model-mailbox sender - and
/// while that clone is alive the model thread never sees its mailbox disconnect, so
/// [`Engine`]'s drop (which joins the model thread) would hang. Dropping the runtime
/// FIRST cancels that task and releases the injector, so the subsequent `engine` drop
/// observes the disconnect and joins cleanly (preserving aterm-core's zero-hang
/// shutdown invariant). Do NOT move `engine` above `agent`/`turn`.
pub struct Session {
    /// The off-render-thread agent-turn runtime (ticket T-5.11): owns the tokio
    /// executor plus the workspace root and the single [`Secrets`] source a turn is
    /// gated and sandboxed against. Declared first so it (and its injector-holding
    /// tasks) drop BEFORE `engine` - see the struct-level note.
    agent: AgentRuntime,
    /// The in-flight agent turn, if one is running (ticket T-5.11). `Some` while a turn
    /// streams its steps into the timeline; the handle bridges the keyboard to the
    /// loop's approval/cancel seam and tells the router whether Esc should interrupt.
    turn: Option<TurnHandle>,
    engine: Engine,
    input: InputModel,
    window: Option<Arc<Window>>,
    /// The configured mode-toggle hotkey (ticket T-3.3), consulted by
    /// [`crate::routing::classify`] each keystroke. Default `Cmd-/`.
    toggle_key: KeyBinding,
    /// The configured autonomy-cycle hotkey (ticket T-5.11). Default `Cmd-Shift-A`.
    autonomy_key: KeyBinding,
    /// The live, session-scoped autonomy posture (ticket T-5.11). Constructed fresh
    /// per session at the configured baseline, so a runtime widening never carries
    /// into a new session (AC5); the cycle hotkey mutates it (AC4), the always-visible
    /// indicator reads it, and its `policy()` gates each live turn (ticket T-5.11).
    autonomy: AutonomyState,
}

/// Apply a T-3.2 IME event to the input model. Pure (no engine / no window), so the
/// composition semantics are unit-testable without spawning a session:
///
/// - `Enabled` - composition may begin; the preedit arrives in following events.
/// - `Preedit` - set the transient composition overlay. winit sends an EMPTY preedit to
///   mean "cleared" (it precedes every `Commit`), so an empty string clears it.
/// - `Commit` - insert the final text as inert characters and clear the preedit (one
///   undo unit; replaces any selection).
/// - `Disabled` - the IME was turned off / focus was lost; drop any dangling preedit.
fn apply_ime(input: &mut InputModel, event: ImeEvent) {
    match event {
        ImeEvent::Enabled => {}
        ImeEvent::Preedit { text, cursor } => {
            if text.is_empty() {
                input.set_preedit(None);
            } else {
                input.set_preedit(Some(Preedit { text, cursor }));
            }
        }
        ImeEvent::Commit(text) => input.commit_ime(&text),
        ImeEvent::Disabled => input.set_preedit(None),
    }
}

/// Map the agent's autonomy tier onto the UI-local indicator enum. The crate boundary
/// keeps `aterm-ui` free of `aterm-agent`, so the app does this translation (ticket
/// T-5.11), exactly as it maps `InputMode` onto the routing chip's `PromptMode`.
fn ui_autonomy(mode: aterm_agent::AutonomyMode) -> aterm_ui::AutonomyMode {
    match mode {
        aterm_agent::AutonomyMode::AskAlways => aterm_ui::AutonomyMode::AskAlways,
        aterm_agent::AutonomyMode::AutoSafe => aterm_ui::AutonomyMode::AutoSafe,
        aterm_agent::AutonomyMode::AutoRunInSession => aterm_ui::AutonomyMode::AutoRunInSession,
    }
}

impl Session {
    /// Spawn a login shell over the three-thread engine and build the session with
    /// the configured hotkeys and the baseline autonomy posture. A fresh session
    /// always starts at `cfg.default_autonomy` (the AUTO-SAFE baseline), so a prior
    /// session's runtime widening never carries over (ticket T-5.11 AC5).
    pub fn spawn(cfg: &Config) -> Result<Self, aterm_core::PtyError> {
        let dims = PtyDimensions {
            cols: cfg.initial_cols,
            rows: cfg.initial_rows,
            pixel_width: 0,
            pixel_height: 0,
        };
        let engine = Engine::spawn_login_shell(dims, DEFAULT_SCROLLBACK)?;
        // The agent works in - and confines its writes to - the current working
        // directory; the single Secrets deny-set feeds the gate, the sandbox, and the
        // sanitizer alike (ticket T-5.11; key custody is T-8.3, out of scope).
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let agent = AgentRuntime::new(root, Secrets::new()).map_err(aterm_core::PtyError::Io)?;
        Ok(Self {
            engine,
            input: InputModel::new(),
            window: None,
            toggle_key: cfg.toggle_mode,
            autonomy_key: cfg.autonomy_cycle,
            autonomy: AutonomyState::new(cfg.default_autonomy),
            agent,
            turn: None,
        })
    }

    /// Number of command blocks segmented so far (used by tests / status line).
    pub fn block_count(&self) -> usize {
        self.engine.block_count()
    }

    /// Drain the VT engine's window events so its channel does not grow. Most are
    /// surfaced for later wiring (title -> window title); for now we log and
    /// otherwise discard them. (DA/DSR/CPR replies are no longer here - the engine
    /// writes them straight back to the PTY on the model thread; ticket T-1.9.)
    fn drain_terminal_events(&mut self) {
        use aterm_core::TerminalEvent;
        while let Ok(event) = self.engine.terminal_events().try_recv() {
            match event {
                TerminalEvent::Title(title) => log::debug!("title: {title}"),
                TerminalEvent::Bell => log::debug!("bell"),
                other => log::trace!("terminal event: {other:?}"),
            }
        }
    }

    /// Build the routing context from live state. `degraded` (integration `None`),
    /// `alt_screen`, `foreground_reading_stdin` (a foreground process group other than
    /// the hidden shell owns the terminal), `agent_turn_active` (a live agent turn is
    /// running, ticket T-5.11), and `preedit_active` (an IME composition is in progress,
    /// ticket T-3.2 - so Enter confirms the candidate and never submits) are all sourced
    /// live.
    fn routing_context(&mut self) -> RoutingContext {
        let degraded = matches!(
            self.engine.integration_status().status,
            IntegrationStatus::None
        );
        let alt_screen = self.engine.latest_snapshot().alt_screen;
        RoutingContext {
            mode: self.input.mode(),
            preedit_active: self.input.preedit().is_some(),
            degraded,
            alt_screen,
            foreground_reading_stdin: self.engine.foreground_is_foreign(),
            agent_turn_active: self.turn.as_ref().is_some_and(TurnHandle::is_active),
        }
    }

    /// Apply an editing key to the [`InputModel`]; returns whether a backspace
    /// actually erased a character (so the Shell-echo mirror only sends DEL when
    /// something was deleted - the scaffold's guard). Enter/Tab/Escape never reach
    /// here (the routing brain dispatches them).
    fn apply_edit_key(&mut self, named: Option<NamedKey>, text: Option<&str>) -> bool {
        match named {
            Some(NamedKey::Backspace) => {
                let erases = self.input.caret() > 0 || !self.input.selection().is_empty();
                self.input.reduce(InputEvent::Backspace);
                erases
            }
            Some(NamedKey::Delete) => {
                self.input.reduce(InputEvent::Delete);
                false
            }
            Some(NamedKey::ArrowLeft) => {
                self.input.reduce(InputEvent::Move(Motion::Left, false));
                false
            }
            Some(NamedKey::ArrowRight) => {
                self.input.reduce(InputEvent::Move(Motion::Right, false));
                false
            }
            Some(NamedKey::ArrowUp) => {
                self.input.reduce(InputEvent::Move(Motion::Up, false));
                false
            }
            Some(NamedKey::ArrowDown) => {
                self.input.reduce(InputEvent::Move(Motion::Down, false));
                false
            }
            Some(NamedKey::Home) => {
                self.input.reduce(InputEvent::Move(Motion::Home, false));
                false
            }
            Some(NamedKey::End) => {
                self.input.reduce(InputEvent::Move(Motion::End, false));
                false
            }
            Some(NamedKey::Space) => {
                self.input.reduce(InputEvent::Insert(" ".to_string()));
                false
            }
            // Tab requests shell completion: it acts on the PTY (the shell's own
            // completer), not the input line, so the model is left untouched here -
            // the `\t` byte is sent by `raw_key_bytes` in the Shell-mode mirror.
            // Freed from the toggle now that `Cmd-/` is the real chord (T-3.3).
            Some(NamedKey::Tab) => false,
            // Esc with no agent turn (and integrated, not alt-screen) is ordinary
            // input; the input box has no Esc action yet, so it is a no-op.
            Some(NamedKey::Escape) => false,
            _ => {
                if let Some(t) = text.filter(|t| !t.is_empty()) {
                    self.input.reduce(InputEvent::Insert(t.to_string()));
                }
                false
            }
        }
    }

    /// The bytes a key sends to the PTY for the **Shell-mode prompt-echo mirror**
    /// only (the raw passthrough path now encodes via `routing::keystroke_for` +
    /// `keys::encode`). `erased` gates Backspace -> DEL so an empty input box does
    /// not echo a stray DEL. Arrows/Delete/Home/End send nothing here on purpose:
    /// at the prompt those move the `InputModel` caret, not the shell's line - the
    /// shell-echo mirror is a deliberately minimal cooked stand-in until the T-3.6
    /// widget is the source of truth.
    fn raw_key_bytes(named: Option<NamedKey>, text: Option<&str>, erased: bool) -> Option<Vec<u8>> {
        match named {
            Some(NamedKey::Enter) => Some(b"\r".to_vec()),
            Some(NamedKey::Backspace) => erased.then(|| vec![0x7f]),
            Some(NamedKey::Space) => Some(b" ".to_vec()),
            Some(NamedKey::Tab) => Some(b"\t".to_vec()),
            Some(
                NamedKey::Delete
                | NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::ArrowUp
                | NamedKey::ArrowDown
                | NamedKey::Home
                | NamedKey::End,
            ) => None,
            _ => text
                .filter(|t| !t.is_empty())
                .map(|t| t.as_bytes().to_vec()),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Tear the agent turn down BEFORE the engine's model thread is joined (ticket
        // T-5.11). A live turn's tokio task holds an `AgentInjector` - a clone of the
        // engine's model-mailbox sender - and while it is alive `Engine::drop`'s
        // `model.join()` would wait forever on a mailbox disconnect that can never
        // come, hanging the winit thread. Cancel the turn (fail-closed denies any
        // parked approval and unblocks the loop), then shut the runtime down (bounded),
        // which drops the task and releases the injector. The subsequent field-drop of
        // `engine` then observes the disconnect and joins cleanly. (Field order -
        // `agent`/`turn` before `engine` - is a backstop; this is the explicit teardown.)
        if let Some(turn) = self.turn.take() {
            turn.cancel();
        }
        self.agent.shutdown();
    }
}

impl UiCallbacks for Session {
    fn on_ready(&mut self, window: Arc<Window>) {
        self.window = Some(window);
    }

    fn snapshot_version(&mut self) -> u64 {
        // Cheap: an Arc clone under a short lock, then a field read. The pacing
        // loop calls this every wake to detect new output before deciding whether
        // to pay for the full grid clone in `snapshot`.
        self.engine.latest_snapshot().version
    }

    fn snapshot(&mut self) -> Option<Arc<Snapshot>> {
        self.drain_terminal_events();
        // The engine's model thread owns the parse loop; here we just hand the
        // renderer the latest published snapshot as a cheap `Arc` clone (a refcount
        // bump under a short lock) - NO per-frame deep copy of the grid. This is
        // the consumer side of the engine's zero-alloc publish (ticket T-1.5 AC5).
        Some(self.engine.latest_snapshot())
    }

    fn blocks(&mut self) -> Option<Arc<BlockList>> {
        // The live, virtualized timeline's data (ticket T-2.7): the model thread
        // publishes the block list, here handed to the renderer as a cheap `Arc`
        // clone (a refcount bump under a short lock - NO per-frame deep copy), the
        // consumer side of the model thread's block publish.
        Some(self.engine.latest_blocks())
    }

    fn integration_status(&mut self) -> aterm_core::Integration {
        // The live three-state shell-integration indicator (ticket T-2.6): a cheap
        // atomic load the engine's model thread keeps current. The renderer maps it
        // to a glyph + "why?" tooltip.
        self.engine.integration_status()
    }

    fn input(&self) -> Option<&InputModel> {
        // The unified-input box (ticket T-3.6) reads the live buffer this Session owns
        // and drives in `on_key` (the reducer mutations + the mode toggle). Borrowed, not
        // cloned - the renderer only reads it; this finally makes the in-progress line
        // (and, in Agent mode, the previously feedback-less prompt) visible on screen.
        Some(&self.input)
    }

    fn autonomy_mode(&self) -> Option<aterm_ui::AutonomyMode> {
        // The always-visible autonomy indicator (ticket T-5.11 AC4): the renderer draws
        // the live session posture as a chip beside the routing chip. Mapped onto the
        // UI-local enum so `aterm-ui` never names an `aterm-agent` type.
        Some(ui_autonomy(self.autonomy.mode()))
    }

    fn on_key(&mut self, key: KeyPress<'_>) -> Option<Vec<u8>> {
        // T-3.3 routing brain. The pure `InputModel` reducer (T-3.1) owns the
        // in-progress line; this layer is the caller that decides where a key goes
        // (the caller-owns-submit contract). `classify` maps the modifier-carrying
        // `KeyPress` to a neutral `KeyInput` (the real `Cmd-/` toggle / `Opt-Enter`),
        // then `decide` applies the disposition gates; we perform the result here.
        // INTERACTIVITY: until the T-3.6 widget is the source of truth, Shell-mode
        // editing keys are still mirrored raw to the PTY so the shell's own line
        // editor echoes them - the byte stream stays what the prior scaffold sent.

        // The autonomy-cycle hotkey (T-5.11) is a SESSION-posture flip, checked before
        // the routing brain - parallel to how the mode toggle flips routing only. It
        // steps the safety tier (taking effect on the next gate decision, AC4) and
        // sends no PTY bytes.
        if self.autonomy_key.matches(&key) {
            self.autonomy.cycle();
            log::info!("autonomy -> {}", self.autonomy.mode().label());
            return None;
        }

        // While the agent is parked on an approval (T-5.11), the keyboard ANSWERS it
        // instead of editing the line: Enter / `y` approve (the gated call runs), `n`
        // deny (it is fed back as an error result and the turn continues), Esc cancels
        // the whole turn. Every other key is swallowed so a pending safety decision can
        // never be bypassed by stray input. This IS the click/Esc seam the fail-closed
        // channel-backed `ConfirmHandler` was built for - resolved on the winit thread.
        if let Some(turn) = self.turn.as_ref() {
            if turn.is_active() && turn.has_pending_approval() {
                if matches!(key.named, Some(NamedKey::Escape)) {
                    turn.cancel();
                } else if matches!(key.named, Some(NamedKey::Enter))
                    || key.ch.is_some_and(|c| c.eq_ignore_ascii_case(&'y'))
                {
                    turn.approve_pending();
                } else if key.ch.is_some_and(|c| c.eq_ignore_ascii_case(&'n')) {
                    turn.deny_pending();
                }
                return None;
            }
        }

        let ctx = self.routing_context();
        let bytes = match decide(classify(&key, &self.toggle_key), &ctx) {
            // The IME owns the key while a composition is active (T-3.2): nothing routes
            // or submits. The composition itself is driven by `on_ime` (winit delivers
            // committed/candidate keys as `Ime` events, not `KeyboardInput`); this gate
            // catches any raw key that still arrives mid-composition (notably Enter, so
            // it confirms the candidate instead of submitting - the Zed #23003 trap).
            Disposition::ImeComposing => None,
            // The hotkey flips ONLY the mode; text/selection/undo preserved (T-3.1).
            Disposition::ToggleMode => {
                self.input.reduce(InputEvent::ToggleMode);
                None
            }
            // Interrupt the in-flight turn (Esc): cancel it and fail-closed deny any
            // parked approval so the loop unblocks. Only reached when a turn is active
            // and NOT parked on an approval (that case is handled above, pre-routing).
            Disposition::InterruptAgent => {
                if let Some(turn) = self.turn.as_ref() {
                    turn.cancel();
                    log::info!("agent interrupt (Esc)");
                }
                None
            }
            // Agent submit (T-5.11): hand the line to the live turn runtime, which
            // streams the turn's steps into the timeline through the engine's agent
            // injector, gated by the CURRENT autonomy posture. The turn runs off the
            // render thread - this returns at once.
            Disposition::SubmitAgent => {
                if self.turn.as_ref().is_some_and(TurnHandle::is_active) {
                    // A turn is already running: PRESERVE the typed line (do not drain
                    // the input) so a follow-up is not silently lost; the resubmit is
                    // ignored for now (queueing is a follow-up).
                    log::info!("agent busy; keeping the typed line (resubmit ignored)");
                } else {
                    // No turn active: now it is safe to consume the line and submit it.
                    let line = self.input.take();
                    if line.trim().is_empty() {
                        log::debug!("agent submit: empty line ignored");
                    } else if let Some(injector) = self.engine.agent_injector() {
                        let policy = self.autonomy.policy();
                        log::info!("agent submit ({}): {line}", self.autonomy.mode().label());
                        self.turn = Some(self.agent.start_turn(line, policy, injector));
                    } else {
                        log::warn!("agent submit dropped: the engine is shutting down");
                    }
                }
                None
            }
            // Shell submit: the shell already received the chars via the Edit mirror
            // below, so submitting is just the carriage return; reset the model.
            Disposition::SubmitShell => {
                let _ = self.input.take();
                Some(b"\r".to_vec())
            }
            // Raw passthrough (alt-screen / a running foreground command / degraded
            // ZLE): the keys belong to the foreground program, NOT the input box, so
            // we do not edit the `InputModel`. Encode them to the correct PTY bytes
            // (legacy / DECCKM / Kitty) with the T-3.4 encoder, reading the live
            // key-mode flags off the snapshot.
            Disposition::PassthroughToPty => {
                let snap = self.engine.latest_snapshot();
                let flags = keys::KeyEncodeFlags {
                    app_cursor: snap.app_cursor,
                    disambiguate: snap.disambiguate,
                };
                keystroke_for(&key)
                    .map(|stroke| keys::encode(stroke, flags))
                    .filter(|bytes| !bytes.is_empty())
            }
            // Ordinary editing: update the input line. In Shell mode ALSO mirror raw
            // to the PTY for the shell's echo (until the T-3.6 widget is the source
            // of truth). Agent mode edits the prompt only - no PTY bytes.
            Disposition::Edit => {
                let erased = self.apply_edit_key(key.named, key.text);
                if ctx.mode == InputMode::Shell {
                    Self::raw_key_bytes(key.named, key.text, erased)
                } else {
                    None
                }
            }
        };

        // Mirror to the PTY and surface the bytes so a headless host can observe them.
        if let Some(b) = &bytes {
            self.engine.send_input(b.clone());
        }
        bytes
    }

    fn on_ime(&mut self, event: ImeEvent) {
        // T-3.2 IME feed. Populate/clear the pure `InputModel`'s preedit (a transient
        // overlay that never touches the committed buffer) or commit the final text.
        // `preedit.is_some()` then makes `routing_context` report `preedit_active`, so
        // the routing brain (T-3.3) gives the IME Enter/Tab/Esc and never submits mid-
        // composition (the Zed #23003 trap). No PTY bytes flow here - even in Shell mode
        // the composed text only becomes shell input when the line is submitted.
        apply_ime(&mut self.input, event);
    }

    fn on_resize(&mut self, cols: u16, rows: u16, width: u32, height: u32) {
        // Pixel dims are advisory (TIOCSWINSZ ws_xpixel/ypixel); clamp rather than
        // silently wrap if a surface somehow exceeds u16.
        self.engine.resize(
            rows,
            cols,
            u16::try_from(width).unwrap_or(u16::MAX),
            u16::try_from(height).unwrap_or(u16::MAX),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The Session-layer IME semantics (ticket T-3.2), tested on a bare `InputModel`
    // through the pure `apply_ime` boundary - no PTY/engine/window needed. The routing
    // gate (`preedit_active` -> Enter never submits) is covered in `routing.rs`
    // (`ime_composition_owns_enter_and_never_submits`); the core mutators in `input.rs`.

    #[test]
    fn preedit_event_populates_then_an_empty_preedit_clears_it() {
        let mut input = InputModel::new();
        input.reduce(InputEvent::Insert("ko".to_string()));
        apply_ime(
            &mut input,
            ImeEvent::Preedit {
                text: "ni".to_string(),
                cursor: Some((2, 2)),
            },
        );
        assert!(
            input.preedit().is_some(),
            "an active composition drives preedit_active"
        );
        assert_eq!(input.text(), "ko", "preedit does not touch the buffer");
        // winit sends an empty preedit right before a commit; it must CLEAR, not show.
        apply_ime(
            &mut input,
            ImeEvent::Preedit {
                text: String::new(),
                cursor: None,
            },
        );
        assert!(input.preedit().is_none(), "an empty preedit clears");
    }

    #[test]
    fn commit_inserts_the_final_text_and_clears_preedit() {
        // AC: Commit inserts the final text (inert) and does not leave a dangling preedit.
        let mut input = InputModel::new();
        apply_ime(
            &mut input,
            ImeEvent::Preedit {
                text: "に".to_string(),
                cursor: None,
            },
        );
        apply_ime(&mut input, ImeEvent::Commit("日本".to_string()));
        assert_eq!(input.text(), "日本");
        assert!(input.preedit().is_none());
    }

    #[test]
    fn disabled_clears_a_dangling_preedit() {
        let mut input = InputModel::new();
        apply_ime(
            &mut input,
            ImeEvent::Preedit {
                text: "x".to_string(),
                cursor: None,
            },
        );
        apply_ime(&mut input, ImeEvent::Disabled);
        assert!(
            input.preedit().is_none(),
            "blur/disable drops the composition so it cannot wedge the routing gate"
        );
    }
}
