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

use crossbeam_channel::{bounded, select, unbounded, Receiver, Sender, TryRecvError};

use crate::{
    BlockList, BlockSegmenter, OscScanner, Pty, PtyDimensions, PtyError, PtyEvent, Snapshot,
    Terminal, TerminalEvent,
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

/// Upper bound on chunks drained into one publish, so a sustained flood cannot
/// monopolize the model loop and starve the control mailbox. Equal to the channel
/// depth: one batch can drain a full backlog, after which control is serviced.
const DRAIN_BATCH: usize = READER_QUEUE_DEPTH;

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
    reader: Option<JoinHandle<()>>,
    model: Option<JoinHandle<()>>,
}

impl Engine {
    /// Spawn a login shell on a fresh PTY and start the engine.
    pub fn spawn_login_shell(dims: PtyDimensions, scrollback: usize) -> Result<Self, PtyError> {
        Self::spawn_with_pty(Pty::spawn_login_shell(dims)?, dims, scrollback)
    }

    /// Spawn an arbitrary `program` with `args` on a fresh PTY and start the
    /// engine. Used for the integration tests (`yes`, `cat`, `echo`) and any
    /// future non-login-shell host.
    pub fn spawn_command(
        program: &str,
        args: &[&str],
        dims: PtyDimensions,
        scrollback: usize,
    ) -> Result<Self, PtyError> {
        let pty = Pty::spawn(program, args, dims, std::iter::empty::<(&str, &str)>())?;
        Self::spawn_with_pty(pty, dims, scrollback)
    }

    /// Wire the reader + model threads around an already-spawned [`Pty`].
    fn spawn_with_pty(pty: Pty, dims: PtyDimensions, scrollback: usize) -> Result<Self, PtyError> {
        // Clone the reader and take the writer *before* the Pty moves into the
        // model thread (both take `&self`).
        let reader = pty.try_clone_reader()?;
        let writer = pty.take_writer()?;

        let terminal =
            Terminal::with_scrollback(dims.rows as usize, dims.cols as usize, scrollback);
        let events = terminal.events().clone();

        let metrics = Arc::new(EngineMetrics::default());
        let latest = Arc::new(Mutex::new(Arc::new(Snapshot::empty(
            dims.rows as usize,
            dims.cols as usize,
        ))));

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
            osc: OscScanner::untrusted(),
            segmenter: BlockSegmenter::new(),
            blocks: BlockList::new(),
            stream_offset: 0,
            version: 0,
            latest: Arc::clone(&latest),
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
    /// Running byte offset into the logical output stream (for block spans).
    stream_offset: usize,
    /// Monotonic publish counter; stamped into each published snapshot.
    version: u64,
    latest: Arc<Mutex<Arc<Snapshot>>>,
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

    /// Scan for OSC marks, segment blocks, and feed the passthrough bytes to the
    /// VT parser. (The real nonce-gated OSC-133/7 *filter* is ticket T-2.1; this
    /// reuses the existing scanner, relocated here from the app's stopgap so the
    /// model thread - which owns the `BlockList` - does the segmentation.)
    fn process_output(&mut self, bytes: &[u8]) {
        let scan = self.osc.scan(bytes);
        for mark in &scan.marks {
            self.segmenter
                .apply(mark, self.stream_offset, &mut self.blocks);
        }
        self.stream_offset += scan.passthrough.len();
        self.terminal.feed(&scan.passthrough);
        self.metrics
            .bytes_drained
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        self.metrics
            .blocks
            .store(self.blocks.len(), Ordering::Relaxed);
    }

    /// Build and publish the latest snapshot, stamping the next version.
    fn publish(&mut self) {
        self.version += 1;
        let mut snap = self.terminal.snapshot();
        snap.version = self.version;
        let arc = Arc::new(snap);
        *self.latest.lock().unwrap_or_else(|e| e.into_inner()) = arc;
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
    /// byte flood cannot starve resize/input. Returns `true` if the mailbox has
    /// disconnected (the [`Engine`] was dropped) and the loop should shut down.
    fn drain_control(&mut self, mailbox: &Receiver<ToModel>) -> bool {
        loop {
            match mailbox.try_recv() {
                Ok(msg) => {
                    if self.handle_mailbox(msg) {
                        self.publish();
                    }
                }
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => return true,
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

/// The model thread: drain bytes (in bounded batches) and control messages,
/// publishing a snapshot whenever the grid changes.
fn run_model(mut model: Model, byte_rx: &Receiver<PtyEvent>, mailbox: &Receiver<ToModel>) {
    loop {
        select! {
            recv(mailbox) -> msg => match msg {
                Ok(m) => {
                    if model.handle_mailbox(m) {
                        model.publish();
                    }
                }
                // Engine dropped -> shut down.
                Err(_) => break,
            },
            recv(byte_rx) -> msg => match msg {
                Ok(ev) => {
                    // Record the backlog at drain time (proves boundedness).
                    model
                        .metrics
                        .max_queue_depth
                        .fetch_max(byte_rx.len(), Ordering::Relaxed);

                    let mut exited = model.consume(ev);
                    // Opportunistically drain a bounded batch so a megabyte burst
                    // becomes one publish, not thousands (the timed coalescing
                    // tick is T-1.4; this is just the natural drain-what's-there).
                    let mut drained = 1usize;
                    while !exited && drained < DRAIN_BATCH {
                        match byte_rx.try_recv() {
                            Ok(ev) => {
                                exited = model.consume(ev);
                                drained += 1;
                            }
                            Err(_) => break,
                        }
                    }
                    model.publish();
                    if exited {
                        break;
                    }
                    // Service pending control promptly (anti-starvation) and pick
                    // up an Engine-drop even when select! keeps favoring bytes.
                    if model.drain_control(mailbox) {
                        break;
                    }
                }
                // Reader gone (PTY closed) -> shut down.
                Err(_) => break,
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
    fn control_sequence_flood_keeps_event_channel_bounded() {
        use crate::terminal::EVENT_CHANNEL_CAP;
        // AC1's yes/cat flood emits no control sequences, leaving the VT
        // window-event channel's bound untested. `yes $'\x1b[6n'` floods DSR
        // (cursor-position) queries; alacritty raises a `PtyWrite` event per
        // query, synchronously on the model thread. With NO drain (the bare
        // engine has no app draining terminal_events), the bounded + drop-on-full
        // event channel must still cap the backlog while the model keeps
        // publishing. An unbounded channel (the original bug) would grow into the
        // thousands here.
        let engine = Engine::spawn_command("/usr/bin/yes", &["\x1b[6n"], dims(), 1_000)
            .expect("spawn yes with a DSR payload");
        wait_for_version_at_least(&engine, 1, Duration::from_secs(5));

        let p1 = engine.metrics().snapshots_published.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(400));
        let p2 = engine.metrics().snapshots_published.load(Ordering::Relaxed);
        assert!(
            p2 > p1,
            "model must keep publishing under a control-sequence flood: {p1} -> {p2}"
        );

        // Deliberately do NOT drain terminal_events: the bounded channel caps the
        // backlog on its own. `len()` must be in [1, cap] - positive (the DSR path
        // really is producing events) and never exceeding the bound.
        let backlog = engine.terminal_events().len();
        assert!(
            backlog > 0,
            "the DSR flood should have produced VT events into the channel"
        );
        assert!(
            backlog <= EVENT_CHANNEL_CAP,
            "VT event backlog {backlog} exceeded the bounded cap {EVENT_CHANNEL_CAP}"
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
