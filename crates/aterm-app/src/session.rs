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

use aterm_core::{Engine, PtyDimensions, Snapshot, DEFAULT_SCROLLBACK};
use aterm_ui::{NamedKey, UiCallbacks, Window};

use aterm_core::{InputEvent, InputModel, InputOutcome};

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
    /// surfaced for later wiring (title -> window title, PtyWrite -> the reply
    /// path in T-1.9); for now we log and otherwise discard them.
    fn drain_terminal_events(&mut self) {
        use aterm_core::TerminalEvent;
        while let Ok(event) = self.engine.terminal_events().try_recv() {
            match event {
                TerminalEvent::Title(title) => log::debug!("title: {title}"),
                TerminalEvent::Bell => log::debug!("bell"),
                TerminalEvent::PtyWrite(_reply) => {
                    // TODO(T-1.9): write DA/DSR/CPR replies back to the PTY master.
                }
                other => log::trace!("terminal event: {other:?}"),
            }
        }
    }

    /// Route an input outcome to the engine.
    fn route_outcome(&mut self, outcome: InputOutcome) {
        match outcome {
            InputOutcome::ToPty(bytes) => self.engine.send_input(bytes),
            InputOutcome::Submitted { line, mode } => {
                // Agent-mode submit: hand off to the agent loop. EPIC-5.
                log::info!("agent submit ({mode:?}): {line}");
                // TODO(ticket EPIC-5): dispatch to AgentTurn on the agent thread.
            }
            InputOutcome::None => {}
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

    fn snapshot(&mut self) -> Option<Snapshot> {
        self.drain_terminal_events();
        // The engine's model thread owns the parse loop; here we just read the
        // latest published snapshot. Cloning out of the `Arc` satisfies the
        // current owned-`Option<Snapshot>` callback contract; a later change can
        // hand the renderer the `Arc` directly to drop this per-frame clone.
        Some((*self.engine.latest_snapshot()).clone())
    }

    fn on_key(&mut self, text: Option<&str>, named: Option<NamedKey>) -> Option<Vec<u8>> {
        // Map the winit key into an InputEvent, reduce, and route to the engine.
        let event = match named {
            Some(NamedKey::Enter) => Some(InputEvent::Submit),
            Some(NamedKey::Backspace) => Some(InputEvent::Backspace),
            Some(NamedKey::ArrowLeft) => Some(InputEvent::CursorLeft),
            Some(NamedKey::ArrowRight) => Some(InputEvent::CursorRight),
            Some(NamedKey::Home) => Some(InputEvent::Home),
            Some(NamedKey::End) => Some(InputEvent::End),
            Some(NamedKey::Space) => Some(InputEvent::Insert(" ".to_string())),
            // The mode-toggle hotkey: Tab for the scaffold (real build uses a
            // dedicated chord). Mutates ONLY the mode.
            Some(NamedKey::Tab) => Some(InputEvent::ToggleMode),
            _ => text
                .filter(|t| !t.is_empty())
                .map(|t| InputEvent::Insert(t.to_string())),
        }?;

        let outcome = self.input.reduce(event);
        // Capture the bytes (if any) before routing, so a headless host could
        // observe them; then route to the engine.
        let echoed = match &outcome {
            InputOutcome::ToPty(b) => Some(b.clone()),
            _ => None,
        };
        self.route_outcome(outcome);
        echoed
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
