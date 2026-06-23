//! The terminal session: owns the live PTY, the VT terminal model, the OSC mark
//! scanner, the block list, and the input model. It implements
//! [`aterm_ui::UiCallbacks`] so the UI event loop pulls a snapshot each frame and
//! pushes keystrokes here, which route to the PTY in Shell mode.
//!
//! This is the "wire" layer: it connects core (PTY/VT/blocks) to ui (window/
//! renderer) to agent (input mode). The 3-thread model is: (1) the PTY reader
//! thread inside `aterm-core`, (2) the winit/render main thread that owns this
//! session, and (3) an agent runtime thread (not spawned in the scaffold).
//! TODO(ticket EPIC-1.3): formalize the agent thread + a coalescing pump.

use std::io::{Read, Write};
use std::sync::Arc;

use crossbeam_channel::Receiver;

use aterm_core::{
    BlockList, BlockSegmenter, OscScanner, Pty, PtyDimensions, PtyEvent, Snapshot, Terminal,
};
use aterm_ui::{NamedKey, UiCallbacks, Window};

use aterm_core::{InputEvent, InputModel, InputOutcome};

/// One terminal session.
pub struct Session {
    pty: Pty,
    /// The child's stdin (taken once from the pty master). Shell-mode keystrokes
    /// are written here; the model thread (T-1.3) will eventually own this.
    writer: Box<dyn Write + Send>,
    pty_rx: Receiver<PtyEvent>,
    terminal: Terminal,
    osc: OscScanner,
    blocks: BlockList,
    segmenter: BlockSegmenter,
    input: InputModel,
    /// Running byte offset into the logical output stream (for block spans).
    stream_offset: usize,
    window: Option<Arc<Window>>,
}

impl Session {
    /// Spawn a login shell and build the session.
    pub fn spawn(cols: u16, rows: u16) -> Result<Self, aterm_core::PtyError> {
        let dims = PtyDimensions {
            cols,
            rows,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pty = Pty::spawn_login_shell(dims)?;
        let writer = pty.take_writer()?;
        // Stopgap reader thread: drains the pty master into a channel the model
        // side drains each frame. T-1.1 deliberately keeps `Pty` thread-free; the
        // bounded reader/model/render split is ticket T-1.3, which replaces this.
        // TODO(T-1.3): replace with the bounded reader thread + coalescing pump.
        // T-1.3 owns the channel + its (bounded) backpressure contract, so the
        // stopgap builds an unbounded channel locally rather than via a core API.
        let (tx, rx) = crossbeam_channel::unbounded::<PtyEvent>();
        let mut reader = pty.try_clone_reader()?;
        std::thread::Builder::new()
            .name("aterm-pty-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => {
                            let _ = tx.send(PtyEvent::Exited);
                            break;
                        }
                        Ok(n) => {
                            if tx.send(PtyEvent::Output(buf[..n].to_vec())).is_err() {
                                break; // receiver gone
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            // An I/O fault on the master is not a clean exit; log it
                            // so it is not silently conflated with a normal EOF.
                            log::warn!("pty reader error: {e}");
                            let _ = tx.send(PtyEvent::Exited);
                            break;
                        }
                    }
                }
            })
            .map_err(aterm_core::PtyError::Io)?;
        Ok(Self {
            pty,
            writer,
            pty_rx: rx,
            terminal: Terminal::new(rows as usize, cols as usize),
            osc: OscScanner::untrusted(),
            blocks: BlockList::new(),
            segmenter: BlockSegmenter::new(),
            input: InputModel::new(),
            stream_offset: 0,
            window: None,
        })
    }

    /// Drain any pending PTY output: scan for OSC marks, segment blocks, and feed
    /// the passthrough bytes to the VT parser. Non-blocking.
    fn pump_pty(&mut self) {
        while let Ok(event) = self.pty_rx.try_recv() {
            match event {
                PtyEvent::Output(bytes) => {
                    let scan = self.osc.scan(&bytes);
                    // Apply marks at the current stream offset (coarse; a finer
                    // implementation would interleave mark offsets within the
                    // chunk). TODO(ticket EPIC-2): per-mark offsets.
                    for mark in &scan.marks {
                        self.segmenter
                            .apply(mark, self.stream_offset, &mut self.blocks);
                    }
                    self.stream_offset += scan.passthrough.len();
                    self.terminal.feed(&scan.passthrough);
                }
                PtyEvent::Exited => {
                    log::info!("shell exited");
                }
            }
        }
        self.drain_terminal_events();
    }

    /// Drain the VT engine's window events so the unbounded channel does not grow.
    /// Most are surfaced for later wiring (title -> window title, PtyWrite -> the
    /// PTY reply path in T-1.9); for now we log and otherwise discard them.
    fn drain_terminal_events(&mut self) {
        use aterm_core::TerminalEvent;
        while let Ok(event) = self.terminal.events().try_recv() {
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

    /// Number of command blocks segmented so far (used by tests / status line).
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Forward an input outcome's bytes to the PTY.
    fn route_outcome(&mut self, outcome: InputOutcome) {
        match outcome {
            InputOutcome::ToPty(bytes) => {
                if let Err(e) = self
                    .writer
                    .write_all(&bytes)
                    .and_then(|()| self.writer.flush())
                {
                    log::warn!("pty write failed: {e}");
                }
            }
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

    fn snapshot(&mut self) -> Option<Snapshot> {
        self.pump_pty();
        Some(self.terminal.snapshot())
    }

    fn on_key(&mut self, text: Option<&str>, named: Option<NamedKey>) -> Option<Vec<u8>> {
        // Map the winit key into an InputEvent, reduce, and route to the PTY.
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
        // observe them; then route to the live PTY.
        let echoed = match &outcome {
            InputOutcome::ToPty(b) => Some(b.clone()),
            _ => None,
        };
        self.route_outcome(outcome);
        echoed
    }

    fn on_resize(&mut self, cols: u16, rows: u16, width: u32, height: u32) {
        self.terminal.resize(rows as usize, cols as usize);
        let _ = self.pty.resize(PtyDimensions {
            cols,
            rows,
            // Pixel dims are advisory (TIOCSWINSZ ws_xpixel/ypixel); clamp rather
            // than silently wrap if a surface somehow exceeds u16.
            pixel_width: u16::try_from(width).unwrap_or(u16::MAX),
            pixel_height: u16::try_from(height).unwrap_or(u16::MAX),
        });
    }
}
