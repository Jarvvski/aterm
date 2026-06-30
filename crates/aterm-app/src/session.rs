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
//! and (3) the winit/render main thread that owns this session. The agent runtime
//! thread (EPIC-5) is not spawned yet.

use std::sync::Arc;

use aterm_core::{
    BlockList, Engine, IntegrationStatus, PtyDimensions, Snapshot, DEFAULT_SCROLLBACK,
};
use aterm_ui::{NamedKey, UiCallbacks, Window};

use aterm_core::{InputEvent, InputMode, InputModel, Motion};

use crate::routing::{decide, Disposition, KeyInput, RoutingContext};

/// One terminal session.
pub struct Session {
    engine: Engine,
    input: InputModel,
    window: Option<Arc<Window>>,
}

impl Session {
    /// Spawn a login shell over the three-thread engine and build the session.
    pub fn spawn(cols: u16, rows: u16) -> Result<Self, aterm_core::PtyError> {
        let dims = PtyDimensions {
            cols,
            rows,
            pixel_width: 0,
            pixel_height: 0,
        };
        let engine = Engine::spawn_login_shell(dims, DEFAULT_SCROLLBACK)?;
        Ok(Self {
            engine,
            input: InputModel::new(),
            window: None,
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

    /// Classify a winit key into the routing brain's [`KeyInput`] (ticket T-3.3).
    ///
    /// LIMITATION: the [`UiCallbacks::on_key`] seam does not yet carry keyboard
    /// MODIFIERS, so the real toggle chord (`Cmd-/`) and `Opt-Enter` cannot be
    /// detected here. `Tab` stands in for the toggle (as the prior scaffold did) and
    /// Enter is always non-alt. Wiring the modifier seam (and thus the real chords)
    /// is the remaining work; the brain already decides them correctly.
    fn classify_key(named: Option<NamedKey>) -> KeyInput {
        match named {
            Some(NamedKey::Enter) => KeyInput::Enter { alt: false },
            Some(NamedKey::Escape) => KeyInput::Escape,
            // Placeholder toggle chord (real chord is `Cmd-/`, pending the modifier
            // seam). NOTE: this means `Tab` toggles even inside a TUI - a known
            // interim limitation carried from the scaffold, resolved once `Cmd-/`
            // frees `Tab`.
            Some(NamedKey::Tab) => KeyInput::ToggleHotkey,
            _ => KeyInput::Other,
        }
    }

    /// Build the routing context from live state. `degraded` (integration `None`)
    /// and `alt_screen` are sourced live; `preedit_active` (T-3.2),
    /// `agent_turn_active` (EPIC-5), and `foreground_reading_stdin` are not yet
    /// available and read `false` (documented residuals).
    fn routing_context(&mut self) -> RoutingContext {
        let degraded = matches!(
            self.engine.integration_status().status,
            IntegrationStatus::None
        );
        let alt_screen = self.engine.latest_snapshot().alt_screen;
        RoutingContext {
            mode: self.input.mode(),
            preedit_active: false,
            degraded,
            alt_screen,
            foreground_reading_stdin: false,
            agent_turn_active: false,
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

    /// The raw bytes a key sends to the PTY (the Shell-echo mirror and raw
    /// passthrough). `erased` gates Backspace -> DEL so an empty input box does not
    /// echo a stray DEL. Arrows/Delete/Home/End send nothing yet - full key->bytes
    /// encoding (Kitty protocol / DECCKM) is ticket T-3.4.
    fn raw_key_bytes(named: Option<NamedKey>, text: Option<&str>, erased: bool) -> Option<Vec<u8>> {
        match named {
            Some(NamedKey::Enter) => Some(b"\r".to_vec()),
            Some(NamedKey::Backspace) => erased.then(|| vec![0x7f]),
            Some(NamedKey::Space) => Some(b" ".to_vec()),
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

    fn on_key(&mut self, text: Option<&str>, named: Option<NamedKey>) -> Option<Vec<u8>> {
        // T-3.3 routing brain. The pure `InputModel` reducer (T-3.1) owns the
        // in-progress line; this layer is the caller that decides where a key goes
        // (the caller-owns-submit contract). `decide` (routing.rs) applies the
        // disposition gates; we perform the result here. INTERACTIVITY: until the
        // T-3.6 widget renders the `InputModel`, Shell-mode editing keys are still
        // mirrored raw to the PTY so the shell's own line editor echoes them - the
        // byte stream stays what the prior scaffold sent.
        let key = Self::classify_key(named);
        let ctx = self.routing_context();
        let bytes = match decide(key, &ctx) {
            // The IME owns the key (T-3.2); nothing routes. Unreachable today
            // (`preedit_active` is false until T-3.2 lands).
            Disposition::ImeComposing => None,
            // The hotkey flips ONLY the mode; text/selection/undo preserved (T-3.1).
            Disposition::ToggleMode => {
                self.input.reduce(InputEvent::ToggleMode);
                None
            }
            // EPIC-5 stub: a real agent turn and its interrupt land with the loop.
            Disposition::InterruptAgent => {
                log::info!("agent interrupt (Esc)");
                None
            }
            // Agent submit: read+reset the line, hand it to the agent (EPIC-5 stub).
            Disposition::SubmitAgent => {
                let line = self.input.take();
                log::info!("agent submit: {line}");
                None
            }
            // Shell submit: the shell already received the chars via the Edit mirror
            // below, so submitting is just the carriage return; reset the model.
            Disposition::SubmitShell => {
                let _ = self.input.take();
                Some(b"\r".to_vec())
            }
            // Raw passthrough (alt-screen / foreground stdin / degraded): the keys
            // belong to the PTY (a TUI or a classic ZLE line editor), NOT the input
            // box, so we do not edit the `InputModel`. T-3.4 owns full encoding.
            Disposition::PassthroughToPty => Self::raw_key_bytes(named, text, true),
            // Ordinary editing: update the input line. In Shell mode ALSO mirror raw
            // to the PTY for the shell's echo (until the T-3.6 widget is the source
            // of truth). Agent mode edits the prompt only - no PTY bytes.
            Disposition::Edit => {
                let erased = self.apply_edit_key(named, text);
                if ctx.mode == InputMode::Shell {
                    Self::raw_key_bytes(named, text, erased)
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
