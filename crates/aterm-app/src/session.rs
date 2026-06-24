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

use aterm_core::{BlockList, Engine, PtyDimensions, Snapshot, DEFAULT_SCROLLBACK};
use aterm_ui::{NamedKey, UiCallbacks, Window};

use aterm_core::{InputEvent, InputMode, InputModel, Motion};

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

    fn on_key(&mut self, text: Option<&str>, named: Option<NamedKey>) -> Option<Vec<u8>> {
        // T-3.3 STOPGAP routing. The pure `InputModel` reducer (ticket T-3.1) owns
        // the in-progress line - editing, selection, mode. The real routing brain
        // (disposition gates, IME preedit, the toggle hotkey) is T-3.3 and the input
        // widget that renders the buffer is T-3.6; until they land, Shell-mode
        // keystrokes are ALSO mirrored raw to the PTY so the shell's own line editor
        // echoes them and the app stays interactive. The model is kept live in
        // parallel (so the mode toggle and reducer are exercised) but is not yet the
        // source of truth for what reaches the PTY. The bytes sent here are
        // byte-for-byte what the previous scaffold sent.
        let shell = self.input.mode() == InputMode::Shell;
        let bytes = match named {
            // The caller decides submission: read the line, then reset the model
            // (the T-3.1 caller-owns-submit contract). Shell mode runs the
            // shell-echoed line with a CR; Agent mode hands off to EPIC-5.
            Some(NamedKey::Enter) => {
                let line = self.input.take();
                if shell {
                    Some(b"\r".to_vec())
                } else {
                    log::info!("agent submit: {line}");
                    // TODO(ticket EPIC-5): dispatch to AgentTurn on the agent thread.
                    None
                }
            }
            // Scaffold mode-toggle hotkey (the real chord is a T-3.3 product call);
            // mutates ONLY the mode, never the text.
            Some(NamedKey::Tab) => {
                self.input.reduce(InputEvent::ToggleMode);
                None
            }
            Some(NamedKey::Backspace) => {
                // DEL only when a char is actually erased - mirrors the prior
                // scaffold's `cursor > 0` guard so an empty/at-start prompt sends
                // nothing (the buffer is tiny; reading it pre-reduce is free).
                let erases = self.input.caret() > 0 || !self.input.selection().is_empty();
                self.input.reduce(InputEvent::Backspace);
                (shell && erases).then(|| vec![0x7f])
            }
            Some(NamedKey::Delete) => {
                self.input.reduce(InputEvent::Delete);
                None
            }
            Some(NamedKey::ArrowLeft) => {
                self.input.reduce(InputEvent::Move(Motion::Left, false));
                None
            }
            Some(NamedKey::ArrowRight) => {
                self.input.reduce(InputEvent::Move(Motion::Right, false));
                None
            }
            Some(NamedKey::ArrowUp) => {
                self.input.reduce(InputEvent::Move(Motion::Up, false));
                None
            }
            Some(NamedKey::ArrowDown) => {
                self.input.reduce(InputEvent::Move(Motion::Down, false));
                None
            }
            Some(NamedKey::Home) => {
                self.input.reduce(InputEvent::Move(Motion::Home, false));
                None
            }
            Some(NamedKey::End) => {
                self.input.reduce(InputEvent::Move(Motion::End, false));
                None
            }
            Some(NamedKey::Space) => {
                self.input.reduce(InputEvent::Insert(" ".to_string()));
                shell.then(|| b" ".to_vec())
            }
            _ => {
                let t = text.filter(|t| !t.is_empty())?;
                self.input.reduce(InputEvent::Insert(t.to_string()));
                shell.then(|| t.as_bytes().to_vec())
            }
        };

        // Mirror to the PTY (Shell mode only) and also surface the bytes so a
        // headless host can observe them.
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
