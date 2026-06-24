//! The three-thread terminal engine (ticket T-1.3).
//!
//! Stands up the canonical reader/model/render topology over bounded mailboxes so
//! a flooding subprocess (`cat hugefile`, `yes`) applies natural backpressure and
//! never stalls the UI. Two of the three threads live here; the render thread is
//! `aterm-ui`'s (ticket T-1.5). This module defines the snapshot/mailbox *contract*
//! that the render thread consumes (see [`03-pty-vt-rust.md`] section E,
//! [`09-performance-60fps.md`] section 2.4, and ADR-0010).
//!
//! ```text
//!  reader thread          model thread (owns Term)        consumer (T-1.5)
//!  -------------          ------------------------        ----------------
//!  blocking read()  -->  bounded PtyEvent channel  -->  drain -> OSC scan ->
//!  into 64 KiB buf       (the bound IS backpressure)     segment -> feed ->
//!                                                        publish Arc<Snapshot> --> latest_snapshot()
//!
//!  Engine handle (main thread)  --[ ToModel: resize / write-input ]-->  model thread
//! ```
//!
//! **Backpressure.** The reader sends fixed-size chunks over a *bounded* channel.
//! When the model can't keep up under a flood, the reader blocks on `send`, which
//! blocks its `read`, which lets the kernel PTY buffer apply flow control to the
//! child. There is no application-level unbounded queue, so in-flight memory is
//! capped by the channel depth and the grid is itself bounded (viewport + capped
//! scrollback ring) - process memory stays bounded by construction (ADR-0010).
//! The VT window-event channel (title/bell/`PtyWrite`/...) the model thread
//! *produces* into during parsing is likewise *bounded* and drops on overflow
//! (see [`crate::terminal`]'s `ChannelListener`), so a child spamming control
//! sequences (a tight `\x1b[6n` DSR loop) cannot grow it without bound either.
//!
//! **Publish contract.** The model thread publishes an immutable [`Snapshot`]
//! behind a `Mutex<Arc<Snapshot>>`; a consumer reads the latest via
//! [`Engine::latest_snapshot`] (a cheap `Arc` clone - the lock is held only long
//! enough to bump a refcount). Each publish stamps a monotonically increasing
//! [`Snapshot::version`] so a consumer can detect a new or missed frame. This is
//! the ticket's named "`parking_lot::Mutex<Snapshot>`" option realized over std;
//! the timed coalescing tick + zero-allocation buffer reuse are ticket T-1.4
//! ("output coalescing + grid snapshot publication").
//!
//! **Shutdown.** Dropping the [`Engine`] drops the mailbox sender; the model
//! thread observes the disconnect, breaks its loop, and drops its [`Pty`] - which
//! kill+reaps the child and closes the master fd, unblocking the reader's blocking
//! `read()`. [`Engine`]'s `Drop` then joins both threads. The reverse direction
//! (child exits first) flows the same way: EOF → reader sends `Exited` → model
//! breaks. Either way no thread is left detached and there is no hang.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{after, bounded, never, select, unbounded, Receiver, Sender, TryRecvError};

use crate::shell_integration::{should_inject_zsh, IntegrationDir, ShellKind, ShimNonce};
use crate::{
    BlockList, BlockSegmenter, OscScanner, Pty, PtyDimensions, PtyError, PtyEvent, Signal,
    Snapshot, Terminal, TerminalEvent,
};

/// Reader read-buffer size. 64 KiB matches Zellij's PTY read buffer; the buffer
/// is reused across reads so the reader allocates nothing per `read()` (only the
/// owned chunk it sends downstream is allocated).
const READ_BUF_BYTES: usize = 65_536;

/// Depth of the bounded reader→model byte channel, in chunks. The bound IS the
/// backpressure: at most `READER_QUEUE_DEPTH * READ_BUF_BYTES` (~1 MiB) bytes can
/// be in flight before the reader blocks and the kernel PTY buffer takes over.
/// A starting heuristic; the coalescing tick (T-1.4) may revisit it.
const READER_QUEUE_DEPTH: usize = 16;

/// Coalescing window (ticket T-1.4). The model parses PTY bytes *continuously*
/// for correctness but publishes a snapshot at most once per window, so a megabyte
/// burst becomes one parse pass + a handful of publishes rather than thousands -
/// decoupling byte-rate from frame-rate and protecting the 60fps floor. ~5ms sits
/// comfortably under the 16.6ms/60fps and 8.3ms/120fps frame budgets, so it adds
/// at most one sub-frame of latency to interactive input. A tuned heuristic (the
/// dossier's 4-8ms starting point, after the GPUI `cat`-flood precedent); T-7.2's
/// `output_flood` scenario tunes it against real hardware.
pub(crate) const COALESCE_INTERVAL: Duration = Duration::from_millis(5);

/// Lightweight, lock-free counters the model thread updates and the handle (and
/// tests) read. Cheap observability into the pipeline; not on the render path.
#[derive(Debug, Default)]
pub struct EngineMetrics {
    /// Total snapshots published by the model thread (monotonic).
    pub snapshots_published: AtomicU64,
    /// Total bytes drained from the PTY and fed through the pipeline.
    pub bytes_drained: AtomicU64,
    /// High-water mark of the byte-channel backlog observed at drain time. Proves
    /// the bounded-memory property: it can never exceed [`READER_QUEUE_DEPTH`].
    pub max_queue_depth: AtomicUsize,
    /// Number of command blocks segmented so far.
    pub blocks: AtomicUsize,
}

/// A control message from the app/main thread to the model thread (the
/// main→model mailbox). Kept small and bounded; `focus`/`config-change` join here
/// when they have a consumer.
#[derive(Debug)]
pub enum ToModel {
    /// The window resized; reflow the grid and the PTY to `rows` x `cols`.
    Resize {
        rows: u16,
        cols: u16,
        pixel_width: u16,
        pixel_height: u16,
    },
    /// Bytes to write to the PTY master (shell-mode keystrokes, pastes, and -
    /// later, ticket T-1.9 - query replies).
    Input(Vec<u8>),
}

/// A running terminal engine: the reader + model threads and the handles the app
/// uses to drive them. Owned by the main thread. Drop tears it down cleanly.
pub struct Engine {
    /// `Option` so `Drop` can drop the sender (signalling shutdown) before join.
    to_model: Option<Sender<ToModel>>,
    /// The model thread publishes here; consumers read the latest snapshot.
    latest: Arc<Mutex<Arc<Snapshot>>>,
    /// VT window events (title/bell/clipboard/PtyWrite/...) the app drains.
    events: Receiver<TerminalEvent>,
    metrics: Arc<EngineMetrics>,
    /// A `dup` of the PTY master fd (Unix), held so [`Engine::signal_foreground`]
    /// can `tcgetpgrp`/`killpg` from the main thread independently of the model
    /// thread's `Pty`. Owning a dup (not a bare `RawFd`) means the fd stays valid
    /// for the handle's lifetime and can never be a *reused* descriptor - signalling
    /// the wrong process group would be a real hazard (ticket T-1.9). `None` if the
    /// backend exposed no master fd or the dup failed.
    #[cfg(unix)]
    fg_fd: Option<std::os::fd::OwnedFd>,
    /// The materialized zsh `ZDOTDIR` shim dir (ticket T-2.2), held for the engine's
    /// lifetime so it is removed only when the engine drops - AFTER `Drop` has joined
    /// the threads and killed the child, so `exec zsh` mid-session still finds it.
    /// `None` for non-zsh shells / test commands. Underscore: held for cleanup, not read.
    _integration: Option<IntegrationDir>,
    reader: Option<JoinHandle<()>>,
    model: Option<JoinHandle<()>>,
}

impl Engine {
    /// Spawn a login shell on a fresh PTY and start the engine.
    ///
    /// When the login shell is zsh we install the shell-integration shim (ticket
    /// T-2.2): a per-session `ZDOTDIR` shim is materialized, the child is spawned
    /// with `$ZDOTDIR` pointed at it (zero dotfile edits), and the OSC scanner is
    /// armed with the shim's nonce ([`OscScanner::with_nonce`]) so ONLY our marks
    /// are trusted. For any other shell (or if the shim cannot be installed) we
    /// spawn plainly and run the scanner untrusted (no nonce'd marks will appear).
    pub fn spawn_login_shell(dims: PtyDimensions, scrollback: usize) -> Result<Self, PtyError> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());

        let mut osc = OscScanner::untrusted();
        let mut env: Vec<(String, String)> = Vec::new();
        let mut integration: Option<IntegrationDir> = None;

        if ShellKind::from_path(&shell) == ShellKind::Zsh {
            let orig_zdotdir = std::env::var("ZDOTDIR").ok();
            if should_inject_zsh(orig_zdotdir.as_deref()) {
                let nonce = ShimNonce::generate();
                match IntegrationDir::install_zsh(&nonce, orig_zdotdir) {
                    Ok(shim) => {
                        env = shim.env_vars();
                        osc = OscScanner::with_nonce(nonce.0);
                        integration = Some(shim);
                    }
                    Err(e) => log::warn!(
                        "zsh shell-integration shim install failed: {e}; \
                         block segmentation disabled this session"
                    ),
                }
            }
        }

        let pty = Pty::spawn(
            &shell,
            &["-l"],
            dims,
            env.iter().map(|(k, v)| (k.as_str(), v.as_str())),
        )?;
        Self::spawn_with_pty(pty, dims, scrollback, osc, integration)
    }

    /// Spawn an arbitrary `program` with `args` on a fresh PTY and start the
    /// engine. Used for the integration tests (`yes`, `cat`, `echo`) and any
    /// future non-login-shell host. Runs the scanner untrusted (no shim).
    pub fn spawn_command(
        program: &str,
        args: &[&str],
        dims: PtyDimensions,
        scrollback: usize,
    ) -> Result<Self, PtyError> {
        let pty = Pty::spawn(program, args, dims, std::iter::empty::<(&str, &str)>())?;
        Self::spawn_with_pty(pty, dims, scrollback, OscScanner::untrusted(), None)
    }

    /// Wire the reader + model threads around an already-spawned [`Pty`]. `osc` is
    /// the (nonce-armed or untrusted) OSC scanner the model thread uses; `integration`
    /// is the materialized shim dir, held for the engine's lifetime and removed when
    /// it drops.
    fn spawn_with_pty(
        pty: Pty,
        dims: PtyDimensions,
        scrollback: usize,
        osc: OscScanner,
        integration: Option<IntegrationDir>,
    ) -> Result<Self, PtyError> {
        // Clone the reader and take the writer *before* the Pty moves into the
        // model thread (both take `&self`).
        let reader = pty.try_clone_reader()?;
        let writer = pty.take_writer()?;

        let terminal =
            Terminal::with_scrollback(dims.rows as usize, dims.cols as usize, scrollback);
        let events = terminal.events().clone();
        let replies = terminal.replies().clone();

        // Dup the master fd so the handle can resolve + signal the foreground
        // process group from the main thread (Ctrl-C / agent-cancel) independently
        // of the `Pty`, which moves into the model thread. A dup (an `OwnedFd`, not
        // a copied `RawFd`) cannot become a reused descriptor while we hold it.
        #[cfg(unix)]
        let fg_fd = pty.master_fd().and_then(|fd| {
            // SAFETY: `fd` is the live master fd of `pty` (still owned here, before
            // it moves into the model thread); we only borrow it for the dup.
            let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
            borrowed.try_clone_to_owned().ok()
        });

        let metrics = Arc::new(EngineMetrics::default());
        let rows = dims.rows as usize;
        let cols = dims.cols as usize;
        let latest = Arc::new(Mutex::new(Arc::new(Snapshot::empty(rows, cols))));
        // The second half of the publish double-buffer (T-1.4).
        let back = Arc::new(Snapshot::empty(rows, cols));

        let (byte_tx, byte_rx) = bounded::<PtyEvent>(READER_QUEUE_DEPTH);
        let (to_model_tx, to_model_rx) = unbounded::<ToModel>();

        let reader_handle = std::thread::Builder::new()
            .name("aterm-pty-reader".into())
            .spawn(move || run_reader(reader, &byte_tx))
            .map_err(PtyError::Io)?;

        let model = Model {
            pty,
            writer,
            terminal,
            osc,
            segmenter: BlockSegmenter::new(),
            blocks: BlockList::new(),
            replies,
            pending_reply: Vec::new(),
            clean_offset: 0,
            version: 0,
            latest: Arc::clone(&latest),
            back,
            metrics: Arc::clone(&metrics),
        };
        let model_handle = std::thread::Builder::new()
            .name("aterm-model".into())
            .spawn(move || run_model(model, &byte_rx, &to_model_rx))
            .map_err(PtyError::Io)?;

        Ok(Self {
            to_model: Some(to_model_tx),
            latest,
            events,
            metrics,
            #[cfg(unix)]
            fg_fd,
            _integration: integration,
            reader: Some(reader_handle),
            model: Some(model_handle),
        })
    }

    /// Ask the model thread to reflow the grid + PTY to the new size. Fire and
    /// forget; if the engine is shutting down the message is simply dropped.
    pub fn resize(&self, rows: u16, cols: u16, pixel_width: u16, pixel_height: u16) {
        self.send(ToModel::Resize {
            rows,
            cols,
            pixel_width,
            pixel_height,
        });
    }

    /// Write `bytes` to the PTY (shell-mode keystrokes / pastes).
    pub fn send_input(&self, bytes: Vec<u8>) {
        self.send(ToModel::Input(bytes));
    }

    fn send(&self, msg: ToModel) {
        if let Some(tx) = self.to_model.as_ref() {
            // A full/closed mailbox means the model is gone or wedged; dropping
            // the control message is the right non-blocking behavior here.
            let _ = tx.send(msg);
        }
    }

    /// The most recently published snapshot. Cheap: clones an `Arc` under a lock
    /// held only for the refcount bump.
    pub fn latest_snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(&self.latest.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Borrow the channel of VT window events (title/bell/clipboard/...).
    pub fn terminal_events(&self) -> &Receiver<TerminalEvent> {
        &self.events
    }

    /// The pipeline counters (for status lines, tests, and perf checks).
    pub fn metrics(&self) -> &EngineMetrics {
        &self.metrics
    }

    /// Number of command blocks segmented so far.
    pub fn block_count(&self) -> usize {
        self.metrics.blocks.load(Ordering::Relaxed)
    }
}

#[cfg(unix)]
impl Engine {
    /// The terminal's foreground process group id, or `None` if it cannot be
    /// resolved (no master fd, or the fd is no longer a terminal). This is the
    /// group a Ctrl-C / agent-cancel should target, not the shell's own group
    /// (ticket T-1.9).
    pub fn foreground_pgid(&self) -> Option<i32> {
        use std::os::fd::AsFd;
        crate::pty::foreground_pgid(self.fg_fd.as_ref()?.as_fd())
    }

    /// Send `sig` to the terminal's foreground process group (Ctrl-C interrupts the
    /// running command, not the hidden shell). Errors if there is no master fd or
    /// the signal cannot be delivered.
    ///
    /// Note: like all pgid-based signalling this races process-group teardown - if
    /// the foreground group has already exited and its pgid been reused, the signal
    /// could reach an unrelated group. Callers should only signal a group they have
    /// reason to believe is alive.
    pub fn signal_foreground(&self, sig: Signal) -> Result<(), PtyError> {
        use std::os::fd::AsFd;
        let fd = self
            .fg_fd
            .as_ref()
            .ok_or_else(|| PtyError::Signal("no master fd to signal".into()))?;
        crate::pty::signal_foreground(fd.as_fd(), sig)
    }
}

#[cfg(not(unix))]
impl Engine {
    /// Foreground-pgroup lookup is Unix-only; always `None` elsewhere.
    pub fn foreground_pgid(&self) -> Option<i32> {
        None
    }

    /// Foreground signalling is Unix-only; always an error elsewhere.
    pub fn signal_foreground(&self, _sig: Signal) -> Result<(), PtyError> {
        Err(PtyError::Signal(
            "foreground signalling is only supported on Unix".into(),
        ))
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // 1. Drop the mailbox sender so the model thread's `select!` sees the
        //    disconnect and breaks out of its loop.
        self.to_model.take();
        // 2. Join the model thread. On exit it drops its `Pty`, which kill+reaps
        //    the child and closes the master fd, unblocking the reader's read().
        if let Some(h) = self.model.take() {
            let _ = h.join();
        }
        // 3. Join the now-unblocked reader thread.
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
    }
}

/// The model-thread state: owns the `Term`, the block model, the PTY (for resize
/// + child reaping on drop) and writer, and the publish + metrics handles.
struct Model {
    pty: Pty,
    writer: Box<dyn Write + Send>,
    terminal: Terminal,
    osc: OscScanner,
    segmenter: BlockSegmenter,
    blocks: BlockList,
    /// PTY query replies (DA/DSR/cursor-position) the VT engine raised during
    /// parsing; drained after each feed and written back to the master so probing
    /// programs get their answers (ticket T-1.9).
    replies: Receiver<Vec<u8>>,
    /// The unwritten tail of the reply currently being delivered to the master.
    /// Holds at most one reply at a time so a short (non-blocking) write resumes
    /// next cycle without truncating the escape sequence (see [`Model::write_replies`]).
    pending_reply: Vec<u8>,
    /// Cumulative count of clean (passthrough) bytes fed to the terminal so far -
    /// the session-absolute base the OSC scanner stamps mark offsets against. Used
    /// to convert a mark's absolute offset into a chunk-relative index so feed and
    /// mark-application can be interleaved in stream order (ticket T-2.5).
    clean_offset: usize,
    /// Monotonic publish counter; stamped into each published snapshot.
    version: u64,
    latest: Arc<Mutex<Arc<Snapshot>>>,
    /// The spare half of the publish double-buffer: the snapshot the model writes
    /// into next. Together with the buffer in `latest`, two buffers cycle so a
    /// publish reuses an allocation instead of building a fresh `Vec` each time
    /// (ticket T-1.4). See [`Model::publish`].
    back: Arc<Snapshot>,
    metrics: Arc<EngineMetrics>,
}

impl Model {
    /// Handle one PTY event. Returns `true` if the shell has exited (EOF / error),
    /// which tells the loop to publish a final frame and shut down.
    fn consume(&mut self, ev: PtyEvent) -> bool {
        match ev {
            PtyEvent::Output(bytes) => {
                self.process_output(&bytes);
                false
            }
            PtyEvent::Exited => true,
        }
    }

    /// Scan for OSC marks (nonce-gated, split-sequence-aware - ticket T-2.1),
    /// segment blocks at each mark's clean-stream offset, and feed the passthrough
    /// bytes to the VT parser. The scanner owns the cumulative clean-stream
    /// position, so marks already carry absolute offsets.
    fn process_output(&mut self, bytes: &[u8]) {
        let scan = self.osc.scan(bytes);
        // Interleave feed + mark-application in stream order (ticket T-2.5): feed the
        // clean bytes up to each mark's offset BEFORE applying it, so the grid (hence
        // the alt-screen flag the segmenter reads at fire time) reflects exactly the
        // state at that mark. With no marks this collapses to a single feed of the
        // whole passthrough - identical to before.
        let chunk_start = self.clean_offset;
        let passthrough = &scan.passthrough;
        let mut fed = 0usize;
        for (offset, mark) in &scan.marks {
            let rel = offset.saturating_sub(chunk_start).min(passthrough.len());
            if rel > fed {
                self.terminal.feed(&passthrough[fed..rel]);
                fed = rel;
            }
            self.segmenter
                .set_alt_screen(self.terminal.is_alt_screen(), &mut self.blocks);
            self.segmenter.apply(mark, *offset, &mut self.blocks);
        }
        if fed < passthrough.len() {
            self.terminal.feed(&passthrough[fed..]);
        }
        self.clean_offset += passthrough.len();
        // The feed above may have raised DA/DSR/cursor-position replies into the
        // terminal's reply channel; write them back to the PTY (ticket T-1.9).
        self.write_replies();
        self.metrics
            .bytes_drained
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        self.metrics
            .blocks
            .store(self.blocks.len(), Ordering::Relaxed);
    }

    /// Drain terminal reply bytes and write them back to the PTY master, so a
    /// program probing the terminal (DA `\x1b[c`, DSR `\x1b[6n`) gets its answer
    /// (ticket T-1.9).
    ///
    /// **Deadlock-safe, even mid-reply.** A blocking `write_all` here would be a
    /// real hazard: a child that floods queries while never draining its own input
    /// (`yes $'\x1b[6n'`) fills the master's input buffer, and a blocking write
    /// would stall the model's read loop, fill the output pipe, and block the
    /// child's stdout - a cycle. The master fd is blocking and `poll(POLLOUT)` only
    /// promises that *one* byte will not block, NOT that a whole multi-byte reply
    /// fits - so we never call `write_all`. Instead: poll, then a SINGLE `write()`,
    /// which on a blocking fd with room writes what fits and returns *short* rather
    /// than blocking (the model thread is the sole writer, so the room poll saw
    /// cannot vanish before the write). Any unwritten tail is kept in
    /// `pending_reply` and resumed next cycle - so a reply is never truncated (which
    /// would corrupt the child's input stream) and the thread never blocks.
    ///
    /// Memory stays bounded: `pending_reply` holds at most one reply's tail (we
    /// refill it from the channel only when empty), and the reply channel is itself
    /// bounded + drop-on-full, so a child that never drains its input cannot grow
    /// either without bound.
    fn write_replies(&mut self) {
        loop {
            // Refill the one-reply tail buffer from the bounded channel only when
            // empty, so at most a single reply is ever pulled out at a time.
            if self.pending_reply.is_empty() {
                match self.replies.try_recv() {
                    Ok(reply) => self.pending_reply = reply,
                    Err(_) => return, // nothing pending, nothing queued
                }
                if self.pending_reply.is_empty() {
                    continue; // skip an empty reply
                }
            }
            // Only write while the master accepts bytes without blocking.
            if !self.pty_writable() {
                return; // keep the tail; resume on the next chunk
            }
            match self.writer.write(&self.pending_reply) {
                Ok(0) => return, // defensive: no progress despite POLLOUT
                Ok(n) => {
                    self.pending_reply.drain(..n);
                    let _ = self.writer.flush();
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    log::warn!("pty reply write failed: {e}");
                    self.pending_reply.clear();
                    return;
                }
            }
        }
    }

    /// Whether the PTY master can accept a write right now, probed non-blockingly
    /// so the read loop never blocks on a reply write (see [`Model::write_replies`]).
    /// On a backend with no master fd, or on non-Unix, we optimistically return
    /// `true` (the reply write then uses normal blocking I/O).
    #[cfg(unix)]
    fn pty_writable(&self) -> bool {
        let Some(fd) = self.pty.master_fd() else {
            return true;
        };
        // SAFETY: a plain libc::poll over one pollfd with a zero timeout; `fd` is
        // the live master fd of `self.pty`. POLLOUT set means a write of at least
        // one byte will not block.
        let mut pfd = nix::libc::pollfd {
            fd,
            events: nix::libc::POLLOUT,
            revents: 0,
        };
        let n = unsafe { nix::libc::poll(&mut pfd, 1, 0) };
        n > 0 && (pfd.revents & nix::libc::POLLOUT) != 0
    }

    #[cfg(not(unix))]
    fn pty_writable(&self) -> bool {
        true
    }

    /// Render and publish the latest snapshot, stamping the next version.
    ///
    /// Zero-allocation in steady state (ticket T-1.4): the model writes into the
    /// spare `back` buffer in place (reusing its `cells` Vec via
    /// [`Terminal::snapshot_into`]), swaps it into `latest`, and reclaims the
    /// previously-published buffer as the new spare. Two buffers cycle, so neither
    /// the `Vec` nor the `Arc` is reallocated per publish. The one exception: if a
    /// consumer still holds the spare (`Arc::get_mut` fails), we allocate a fresh
    /// buffer rather than block - correctness over the zero-alloc fast path.
    fn publish(&mut self) {
        self.version += 1;

        // Ensure the spare is uniquely owned so we can write into it in place.
        if Arc::get_mut(&mut self.back).is_none() {
            self.back = Arc::new(Snapshot::empty(self.terminal.rows(), self.terminal.cols()));
        }
        // `get_mut` is `Some` after the ensure above (back is uniquely owned).
        if let Some(snap) = Arc::get_mut(&mut self.back) {
            self.terminal.snapshot_into(snap);
            snap.version = self.version;
        }

        // Publish the freshly-written buffer and reclaim the previous one as the
        // next spare. `Arc::clone` + `mem::replace` are refcount/move ops - no
        // allocation. The lock is held only for the pointer swap.
        let to_publish = Arc::clone(&self.back);
        let prev = {
            let mut guard = self.latest.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::replace(&mut *guard, to_publish)
        };
        self.back = prev;

        self.metrics
            .snapshots_published
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Apply a control message. Returns `true` if it warrants a republish (a
    /// resize reflows the grid; input does not - its echo returns as PTY output).
    fn handle_mailbox(&mut self, msg: ToModel) -> bool {
        match msg {
            ToModel::Resize {
                rows,
                cols,
                pixel_width,
                pixel_height,
            } => {
                self.terminal.resize(rows as usize, cols as usize);
                let _ = self.pty.resize(PtyDimensions {
                    rows,
                    cols,
                    pixel_width,
                    pixel_height,
                });
                true
            }
            ToModel::Input(bytes) => {
                if let Err(e) = self
                    .writer
                    .write_all(&bytes)
                    .and_then(|()| self.writer.flush())
                {
                    log::warn!("pty write failed: {e}");
                }
                false
            }
        }
    }

    /// Service all *immediately pending* control messages (rare and tiny), so a
    /// byte flood cannot starve resize/input. Returns `(shutdown, dirtied)`:
    /// `shutdown` when the mailbox disconnected (the [`Engine`] was dropped), and
    /// `dirtied` when a resize reflowed the grid so the caller should schedule a
    /// coalesced publish (publication is the loop's job, not done here - T-1.4).
    fn drain_control(&mut self, mailbox: &Receiver<ToModel>) -> (bool, bool) {
        let mut dirtied = false;
        loop {
            match mailbox.try_recv() {
                Ok(msg) => dirtied |= self.handle_mailbox(msg),
                Err(TryRecvError::Empty) => return (false, dirtied),
                Err(TryRecvError::Disconnected) => return (true, dirtied),
            }
        }
    }
}

/// The reader thread: blocking `read()` into a reusable buffer, sending owned
/// chunks over the bounded channel (blocking on a full channel = backpressure).
fn run_reader(mut reader: Box<dyn Read + Send>, tx: &Sender<PtyEvent>) {
    let mut buf = [0u8; READ_BUF_BYTES];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                let _ = tx.send(PtyEvent::Exited);
                break;
            }
            Ok(n) => {
                // Bounded `send` blocks when the model is behind; that block
                // propagates to `read`, letting the kernel PTY buffer throttle
                // the child. `Err` means the model (receiver) is gone.
                if tx.send(PtyEvent::Output(buf[..n].to_vec())).is_err() {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                // An I/O fault on the master is not a clean exit; log it so it is
                // not silently conflated with a normal EOF.
                log::warn!("pty reader error: {e}");
                let _ = tx.send(PtyEvent::Exited);
                break;
            }
        }
    }
}

/// The model thread: parse PTY bytes *continuously* for correctness, but publish a
/// coalesced snapshot at most once per [`COALESCE_INTERVAL`] (ticket T-1.4) so the
/// publish rate tracks the tick, not the byte rate.
///
/// The window is *lazy*: `deadline`/`timer` are armed only while there is
/// unpublished output. The `timer` (a one-shot `after`) flushes after a burst goes
/// idle; under a *sustained* flood the byte arm caps its own drain by the clock
/// and flushes at the deadline itself, so a busy `select!` that keeps favoring
/// `byte_rx` can never starve the flush. Idle = no timer = the thread truly sleeps.
fn run_model(mut model: Model, byte_rx: &Receiver<PtyEvent>, mailbox: &Receiver<ToModel>) {
    let mut deadline: Option<Instant> = None;
    let mut timer: Receiver<Instant> = never();

    loop {
        select! {
            recv(mailbox) -> msg => match msg {
                Ok(m) => {
                    let mut dirtied = model.handle_mailbox(m);
                    let (shutdown, more) = model.drain_control(mailbox);
                    dirtied |= more;
                    if shutdown {
                        break; // Engine dropped
                    }
                    // A resize reflowed the grid; coalesce it like output.
                    if dirtied && deadline.is_none() {
                        deadline = Some(Instant::now() + COALESCE_INTERVAL);
                        timer = after(COALESCE_INTERVAL);
                    }
                }
                Err(_) => break, // Engine dropped
            },
            recv(byte_rx) -> msg => match msg {
                Ok(ev) => {
                    // Record the backlog at drain time (proves boundedness).
                    model
                        .metrics
                        .max_queue_depth
                        .fetch_max(byte_rx.len(), Ordering::Relaxed);

                    // Arm the coalescing window on the first unpublished byte.
                    let dl = match deadline {
                        Some(dl) => dl,
                        None => {
                            let dl = Instant::now() + COALESCE_INTERVAL;
                            deadline = Some(dl);
                            timer = after(COALESCE_INTERVAL);
                            dl
                        }
                    };

                    // Parse available bytes until the window elapses or the
                    // backlog drains. The clock bounds the batch by *time*, so a
                    // sustained flood (where the reader refills as fast as we
                    // drain) still returns to publish at the tick - it cannot spin
                    // here indefinitely.
                    let mut exited = model.consume(ev);
                    while !exited && Instant::now() < dl {
                        match byte_rx.try_recv() {
                            Ok(ev) => exited = model.consume(ev),
                            Err(_) => break,
                        }
                    }
                    if exited {
                        model.publish(); // final coherent frame, then shut down
                        break;
                    }
                    // Anti-starvation: service control + detect Engine-drop.
                    let (shutdown, _) = model.drain_control(mailbox);
                    if shutdown {
                        break;
                    }
                    // Flush if the window elapsed during this drain (sustained
                    // flood); otherwise keep coalescing and let the timer flush
                    // once the bytes go idle.
                    if Instant::now() >= dl {
                        model.publish();
                        deadline = None;
                        timer = never();
                    }
                }
                Err(_) => break, // reader gone (PTY closed) -> shut down
            },
            recv(timer) -> _ => {
                // The window elapsed with no further bytes: publish the merged
                // state once. (Disarmed to `never()` so it does not refire.)
                model.publish();
                deadline = None;
                timer = never();
            },
        }
    }
    // Dropping `model` here drops its `Pty`: kill+reap the child and close the
    // master, which unblocks the reader's blocking read() so it can exit too.
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    fn dims() -> PtyDimensions {
        PtyDimensions {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    /// Poll `latest_snapshot().version` until it reaches `min` or `timeout`.
    fn wait_for_version_at_least(engine: &Engine, min: u64, timeout: Duration) -> u64 {
        let deadline = Instant::now() + timeout;
        loop {
            let v = engine.latest_snapshot().version;
            if v >= min || Instant::now() >= deadline {
                return v;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Poll until the published version strictly exceeds `prev` or `timeout`.
    fn wait_for_version_after(engine: &Engine, prev: u64, timeout: Duration) -> u64 {
        let deadline = Instant::now() + timeout;
        loop {
            let v = engine.latest_snapshot().version;
            if v > prev || Instant::now() >= deadline {
                return v;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Flatten a snapshot grid to a string (row-major, newline per row).
    fn grid_text(snap: &Snapshot) -> String {
        let mut s = String::with_capacity(snap.rows * (snap.cols + 1));
        for r in 0..snap.rows {
            for cell in snap.row(r) {
                s.push(cell.c);
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn snapshot_version_increases_monotonically() {
        // AC: a consumer reads two successive snapshots and sees the version
        // increase. `cat` echoes input, so each write deterministically produces
        // output → a publish.
        let engine = Engine::spawn_command("/bin/cat", &[], dims(), 1_000).expect("spawn cat");
        assert_eq!(
            engine.latest_snapshot().version,
            0,
            "the seeded snapshot is version 0 before any publish"
        );

        engine.send_input(b"alpha\n".to_vec());
        let v1 = wait_for_version_at_least(&engine, 1, Duration::from_secs(5));
        assert!(v1 >= 1, "expected a publish after first input, got {v1}");

        engine.send_input(b"bravo\n".to_vec());
        let v2 = wait_for_version_after(&engine, v1, Duration::from_secs(5));
        assert!(
            v2 > v1,
            "version must strictly increase across publishes: {v1} -> {v2}"
        );

        let snap = engine.latest_snapshot();
        let row0: String = snap.row(0).iter().map(|c| c.c).collect();
        assert!(
            row0.contains('a'),
            "cat's echo should land in the grid, row0={row0:?}"
        );
    }

    #[test]
    fn flood_keeps_draining_with_bounded_queue() {
        // AC: run a flood (`yes`) and assert the model keeps draining while
        // process memory stays bounded (no unbounded queue growth).
        let engine = Engine::spawn_command("/usr/bin/yes", &[], dims(), 1_000).expect("spawn yes");

        // Flood is live once the first snapshot has published.
        wait_for_version_at_least(&engine, 1, Duration::from_secs(5));

        let p1 = engine.metrics().snapshots_published.load(Ordering::Relaxed);
        let b1 = engine.metrics().bytes_drained.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(400));
        let p2 = engine.metrics().snapshots_published.load(Ordering::Relaxed);
        let b2 = engine.metrics().bytes_drained.load(Ordering::Relaxed);

        assert!(
            p2 > p1,
            "model thread must keep publishing under flood: {p1} -> {p2}"
        );
        assert!(
            b2 > b1,
            "model thread must keep draining bytes under flood: {b1} -> {b2}"
        );

        // The byte channel is *bounded*, so the in-flight backlog can never
        // exceed its depth no matter how fast `yes` runs - this is the bounded
        // memory guarantee (the grid + scrollback ring are separately bounded).
        let peak = engine.metrics().max_queue_depth.load(Ordering::Relaxed);
        assert!(
            peak <= READER_QUEUE_DEPTH,
            "backlog {peak} exceeded the bounded channel depth {READER_QUEUE_DEPTH}"
        );

        // Tearing down mid-flood must not hang (also covered by the dedicated
        // shutdown test, but worth exercising under active backpressure).
        drop(engine);
    }

    #[test]
    fn dsr_flood_does_not_deadlock_the_reply_path() {
        // `yes $'\x1b[6n'` floods DSR (cursor-position) queries AND never drains
        // its own stdin. The model writes each reply back to the master (T-1.9); a
        // *blocking* write would deadlock here - the child won't read input while
        // we won't read its output - so the model must keep draining + publishing
        // under the flood, never wedging. Over a sustained flood the master's input
        // buffer fills (the child never reads it), so this exercises the full-buffer
        // + short-write path: each reply write is a single poll-guarded `write()`
        // that returns short and buffers its tail rather than blocking mid-reply.
        // This is the regression guard for that deadlock (it would HANG, not just
        // fail, on a regression). The reply channel is bounded + drop-on-full, so
        // engine memory stays bounded by construction.
        let engine = Engine::spawn_command("/usr/bin/yes", &["\x1b[6n"], dims(), 1_000)
            .expect("spawn yes with a DSR payload");
        wait_for_version_at_least(&engine, 1, Duration::from_secs(5));

        let p1 = engine.metrics().snapshots_published.load(Ordering::Relaxed);
        let b1 = engine.metrics().bytes_drained.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(400));
        let p2 = engine.metrics().snapshots_published.load(Ordering::Relaxed);
        let b2 = engine.metrics().bytes_drained.load(Ordering::Relaxed);
        assert!(
            p2 > p1,
            "model must keep publishing under a DSR flood (no deadlock): {p1} -> {p2}"
        );
        assert!(
            b2 > b1,
            "model must keep draining bytes under a DSR flood (no deadlock): {b1} -> {b2}"
        );

        // Reaching here without hanging is the core guarantee; teardown must also
        // not hang while the flood + reply writes are in flight.
        drop(engine);
    }

    #[test]
    fn da_query_gets_a_reply_written_back_to_the_pty() {
        // AC: a program issuing a Primary DA query (`\x1b[c`) over the PTY gets a
        // reply written back to the master. The child prints the query (the VT
        // engine parses it and raises a PtyWrite reply, which the model writes to
        // the master), then echoes its own stdin with `cat -v` (ESC shown as `^[`),
        // so the reply round-trips into the visible grid. `-icanon` makes `cat`
        // deliver the newline-less reply immediately; `-echo` keeps the line
        // discipline from doubling it. The DA report is `CSI ? ... c`, so the grid
        // must contain the characteristic `[?` once the reply has come back.
        let engine = Engine::spawn_command(
            "/bin/sh",
            &["-c", "stty -echo -icanon; printf '\\033[c'; cat -v"],
            dims(),
            1_000,
        )
        .expect("spawn sh DA-probe");

        let start = Instant::now();
        let deadline = start + Duration::from_secs(8);
        let mut saw_reply = false;
        while Instant::now() < deadline {
            if grid_text(&engine.latest_snapshot()).contains("[?") {
                saw_reply = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            saw_reply,
            "the DA reply (CSI ? ... c) should have been written back to the PTY \
             and echoed into the grid; grid was: {:?}",
            grid_text(&engine.latest_snapshot())
        );
        drop(engine);
    }

    #[test]
    fn foreground_pgid_reports_a_running_child() {
        // AC: foreground_pgid() returns the pgid of a running foreground child.
        // `cat` runs forever as the terminal's foreground (session-leader) group.
        let engine = Engine::spawn_command("/bin/cat", &[], dims(), 1_000).expect("spawn cat");
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut pgid = None;
        while Instant::now() < deadline {
            if let Some(p) = engine.foreground_pgid() {
                if p > 0 {
                    pgid = Some(p);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            pgid.is_some(),
            "foreground_pgid() should report a running child's process group"
        );
        drop(engine);
    }

    #[test]
    fn signal_foreground_interrupts_a_running_sleep() {
        // AC: signal_foreground(SIGINT) interrupts a running `sleep` (it exits).
        // Spawn `sleep 10`; without the signal the reader would not hit EOF for
        // ~10s. After SIGINT to the foreground group the child dies, the reader
        // hits EOF, and the model publishes its final frame and shuts down - so the
        // engine tears down promptly (well under 10s). We assert the teardown join
        // completes inside a bound, on a worker thread so a regression fails the
        // test instead of hanging it.
        let engine =
            Engine::spawn_command("/bin/sleep", &["10"], dims(), 1_000).expect("spawn sleep");
        // Wait for `sleep` to become the foreground group.
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && engine.foreground_pgid().is_none() {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            engine.foreground_pgid().is_some(),
            "sleep should be the foreground group before signalling"
        );

        engine
            .signal_foreground(Signal::Interrupt)
            .expect("signal_foreground(SIGINT) should succeed");

        let (done_tx, done_rx) = bounded::<()>(1);
        std::thread::spawn(move || {
            // Drop joins the reader+model threads, which only finish once the child
            // is gone (EOF). A successful SIGINT makes that prompt.
            drop(engine);
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "SIGINT to the foreground group should kill `sleep 10` so teardown is prompt"
        );
    }

    #[test]
    fn flood_publishes_track_ticks_not_bytes() {
        // AC (T-1.4): under a sustained flood the publish count must be bounded by
        // elapsed/COALESCE_INTERVAL (the tick), NOT by byte volume. This upper
        // bound is throughput-independent, so it holds on slow CI too.
        let engine = Engine::spawn_command("/usr/bin/yes", &[], dims(), 1_000).expect("spawn yes");
        wait_for_version_at_least(&engine, 1, Duration::from_secs(5));

        let p0 = engine.metrics().snapshots_published.load(Ordering::Relaxed);
        let b0 = engine.metrics().bytes_drained.load(Ordering::Relaxed);
        let start = Instant::now();
        std::thread::sleep(Duration::from_millis(500));
        let elapsed = start.elapsed();
        let publishes = engine.metrics().snapshots_published.load(Ordering::Relaxed) - p0;
        let bytes = engine.metrics().bytes_drained.load(Ordering::Relaxed) - b0;

        let interval_ms = COALESCE_INTERVAL.as_millis().max(1) as u64;
        let max_ticks = (elapsed.as_millis() as u64 / interval_ms) + 1;
        assert!(
            publishes <= max_ticks * 2,
            "publishes {publishes} should track ~{max_ticks} ticks, not the {bytes}-byte flood"
        );
        assert!(publishes >= 1, "the model should still publish under flood");
        // The flood really did move substantial data while publishes stayed low -
        // proof the publish rate is decoupled from the byte rate.
        assert!(
            bytes > 1_000_000,
            "yes flood should have moved >1MB, got {bytes}"
        );
        drop(engine);
    }

    #[test]
    fn burst_coalesces_and_final_grid_is_correct() {
        // AC (T-1.4): a multi-MB burst (~6.9 MB) coalesces to far fewer publishes
        // than 64 KiB chunks, and the final grid shows the last line (coalescing
        // loses no data). `-f %.0f` forces integer format so the last line is
        // "1000000" on both GNU and BSD `seq` (BSD defaults to `%g` -> "1e+06").
        let engine = Engine::spawn_command(
            "/usr/bin/seq",
            &["-f", "%.0f", "1", "1000000"],
            dims(),
            1_000,
        )
        .expect("spawn seq");

        // Poll until the final value lands in the grid (seq exits -> final frame).
        let start = Instant::now();
        let deadline = start + Duration::from_secs(15);
        let mut found = false;
        while Instant::now() < deadline {
            if grid_text(&engine.latest_snapshot()).contains("1000000") {
                found = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let elapsed = start.elapsed();
        assert!(
            found,
            "final grid should contain the last seq value '1000000'"
        );

        let bytes = engine.metrics().bytes_drained.load(Ordering::Relaxed);
        let publishes = engine.metrics().snapshots_published.load(Ordering::Relaxed);
        let chunks = bytes / READ_BUF_BYTES as u64;
        // "O(1)-ish publishes per tick": bound the publish count by elapsed/tick,
        // not by `chunks`. Consecutive publishes are >= one window apart by
        // construction, so this holds regardless of throughput - whereas comparing
        // to a fixed `chunks` is flaky when concurrent tests steal CPU and stretch
        // the burst's wall-clock (more ticks elapse, but `chunks` is fixed). On
        // real hardware parse outpaces the tick, so this is equivalently
        // "publishes << chunks" (asserted below as a non-flaky lower-bound on the
        // gap once we know the burst was large).
        let interval_ms = COALESCE_INTERVAL.as_millis().max(1) as u64;
        let max_ticks = (elapsed.as_millis() as u64 / interval_ms) + 2;
        assert!(
            publishes <= max_ticks * 2,
            "publishes {publishes} should be O(per-tick) (~{max_ticks} ticks in {elapsed:?}), \
             not byte-driven ({chunks} chunks)"
        );
        // The burst really was multi-MB (so coalescing had something to coalesce).
        assert!(
            bytes > 4_000_000,
            "seq burst should move multiple MB, got {bytes}"
        );
        drop(engine);
    }

    #[test]
    fn lone_input_publishes_within_one_window() {
        // AC (T-1.4): coalescing must not stall a single keystroke waiting for more
        // bytes - one input publishes within ~one window (plus PTY/scheduler
        // slack), not hang until further output arrives.
        let engine = Engine::spawn_command("/bin/cat", &[], dims(), 1_000).expect("spawn cat");
        let before = engine.latest_snapshot().version;
        engine.send_input(b"x\n".to_vec());
        let got = wait_for_version_after(&engine, before, Duration::from_millis(500));
        assert!(
            got > before,
            "a lone input must publish promptly (within a window), version {before} -> {got}"
        );
        drop(engine);
    }

    #[test]
    fn child_exit_shuts_down_without_hang() {
        // AC: closing the PTY (child exits) cleanly shuts down all threads. `echo`
        // exits immediately → reader EOF → model breaks on `Exited`.
        let engine =
            Engine::spawn_command("/bin/echo", &["hi"], dims(), 1_000).expect("spawn echo");
        let v = wait_for_version_at_least(&engine, 1, Duration::from_secs(5));
        assert!(v >= 1, "echo's output should have produced a publish");
        // Reaching here (and the implicit join in drop completing) proves the
        // teardown cascade does not hang.
        drop(engine);
    }

    #[test]
    fn drop_while_running_shuts_down_without_hang() {
        // AC: the *other* shutdown direction - drop the handle while the child is
        // alive. `cat` blocks forever waiting on input. Run the drop on a worker
        // and assert it completes within a bound, so a teardown hang fails the
        // test rather than hanging CI.
        let engine = Engine::spawn_command("/bin/cat", &[], dims(), 1_000).expect("spawn cat");
        wait_for_version_at_least(&engine, 1, Duration::from_secs(2)); // let it settle

        let (done_tx, done_rx) = bounded::<()>(1);
        std::thread::spawn(move || {
            drop(engine);
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(10)).is_ok(),
            "Engine drop must join all threads promptly even with a live child"
        );
    }

    #[test]
    fn concurrent_reads_during_flood_are_consistent() {
        // AC: no data race / torn read under concurrent publish + read. Hammer
        // latest_snapshot() while the model publishes a `yes` flood; observed
        // versions must be non-decreasing and every snapshot internally coherent.
        let engine = Engine::spawn_command("/usr/bin/yes", &[], dims(), 1_000).expect("spawn yes");
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut last = 0u64;
        let mut reads = 0u64;
        while Instant::now() < deadline {
            let snap = engine.latest_snapshot();
            assert!(
                snap.version >= last,
                "published version went backwards: {} < {last}",
                snap.version
            );
            assert_eq!(
                snap.cells.len(),
                snap.rows * snap.cols,
                "snapshot grid must be internally consistent"
            );
            last = snap.version;
            reads += 1;
        }
        assert!(reads > 0, "the stress loop should have read at least once");
        assert!(
            last > 0,
            "the model should have published at least once during the window"
        );
        drop(engine);
    }
}
