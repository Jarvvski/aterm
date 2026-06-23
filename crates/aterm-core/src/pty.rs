//! PTY: spawns a login shell, reads its output on a background thread, and hands
//! the caller a non-blocking channel of byte chunks plus a writer/resizer handle.
//!
//! The reader thread NEVER blocks the caller: it owns the read end of the PTY and
//! pushes owned `Vec<u8>` chunks down a [`crossbeam_channel`]. The caller drains
//! the channel at its own cadence (the app's coalescing layer batches these into
//! at most one VT-parse per frame — TODO(ticket EPIC-1.4): output coalescing).

use std::io::{Read, Write};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender};
use portable_pty::{CommandBuilder, NativePtySystem, PtyPair, PtySize, PtySystem};

/// Errors from PTY setup / IO.
#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("failed to open pty: {0}")]
    Open(String),
    #[error("failed to spawn shell: {0}")]
    Spawn(String),
    #[error("failed to take pty writer: {0}")]
    Writer(String),
    #[error("failed to clone pty reader: {0}")]
    Reader(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// A chunk of bytes read from the PTY, or an end-of-stream signal.
#[derive(Debug)]
pub enum PtyEvent {
    /// Raw bytes read from the shell.
    Output(Vec<u8>),
    /// The shell closed its end (EOF) or the reader errored out.
    Exited,
}

/// Grid dimensions for the PTY (in cells), plus pixel size for apps that care.
#[derive(Debug, Clone, Copy)]
pub struct PtyDimensions {
    pub rows: u16,
    pub cols: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl Default for PtyDimensions {
    fn default() -> Self {
        Self {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

impl From<PtyDimensions> for PtySize {
    fn from(d: PtyDimensions) -> Self {
        PtySize {
            rows: d.rows,
            cols: d.cols,
            pixel_width: d.pixel_width,
            pixel_height: d.pixel_height,
        }
    }
}

/// A live PTY hosting a login shell.
///
/// Drop order matters: dropping `Pty` drops the writer and the master, which
/// signals the child; the reader thread then observes EOF and exits.
pub struct Pty {
    pair: PtyPair,
    writer: Box<dyn Write + Send>,
    reader_thread: Option<JoinHandle<()>>,
    dims: PtyDimensions,
}

impl Pty {
    /// Spawn a login shell (`$SHELL` or `/bin/zsh`, with `-l`). Output is streamed
    /// to `tx` from a background thread. Returns the live handle.
    pub fn spawn_login_shell(dims: PtyDimensions, tx: Sender<PtyEvent>) -> Result<Self, PtyError> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        Self::spawn(&shell, &["-l"], dims, tx)
    }

    /// Spawn an arbitrary program in the PTY with `args`.
    pub fn spawn(
        program: &str,
        args: &[&str],
        dims: PtyDimensions,
        tx: Sender<PtyEvent>,
    ) -> Result<Self, PtyError> {
        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(dims.into())
            .map_err(|e| PtyError::Open(e.to_string()))?;

        let mut cmd = CommandBuilder::new(program);
        for a in args {
            cmd.arg(a);
        }
        // Advertise terminal capabilities to the shell.
        cmd.env("TERM", "xterm-256color");
        // Marker the shell-integration shim can key off of (EPIC-2).
        cmd.env("ATERM", "1");

        let _child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| PtyError::Spawn(e.to_string()))?;

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| PtyError::Writer(e.to_string()))?;

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| PtyError::Reader(e.to_string()))?;

        // Background reader: owns `reader`, pushes chunks down `tx`, never blocks
        // the caller. Exits on EOF / error or when the receiver is dropped.
        let reader_thread = std::thread::Builder::new()
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
                        Err(_) => {
                            let _ = tx.send(PtyEvent::Exited);
                            break;
                        }
                    }
                }
            })
            .expect("spawn pty reader thread");

        Ok(Self {
            pair,
            writer,
            reader_thread: Some(reader_thread),
            dims,
        })
    }

    /// Write bytes to the shell's stdin. Non-blocking-ish; the OS pty buffer
    /// absorbs typical keystroke volume.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), PtyError> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Resize the PTY (on window resize). Updates the kernel pty window size so
    /// the shell re-flows and SIGWINCH fires.
    pub fn resize(&mut self, dims: PtyDimensions) -> Result<(), PtyError> {
        self.pair
            .master
            .resize(dims.into())
            .map_err(|e| PtyError::Open(e.to_string()))?;
        self.dims = dims;
        Ok(())
    }

    /// Current dimensions.
    pub fn dimensions(&self) -> PtyDimensions {
        self.dims
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Reader thread observes EOF once the master is dropped; join briefly so
        // we don't leak the thread. We don't block forever if it's mid-read.
        if let Some(handle) = self.reader_thread.take() {
            // Detach: joining could block on a read that only ends when the OS
            // tears down the pty. The thread is short-lived after master drop.
            drop(handle);
        }
    }
}

/// Convenience: build a fresh unbounded channel for PTY events.
pub fn channel() -> (Sender<PtyEvent>, Receiver<PtyEvent>) {
    crossbeam_channel::unbounded()
}
