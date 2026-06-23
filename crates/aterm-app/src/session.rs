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
        let (tx, rx) = aterm_core::pty::channel();
        let pty = Pty::spawn_login_shell(dims, tx)?;
        Ok(Self {
            pty,
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
                    self.terminal.advance(&scan.passthrough);
                }
                PtyEvent::Exited => {
                    log::info!("shell exited");
                }
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
                if let Err(e) = self.pty.write(&bytes) {
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
            pixel_width: width as u16,
            pixel_height: height as u16,
        });
    }
}
