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

use std::io;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{after, bounded, never, select, unbounded, Receiver, Sender, TryRecvError};

use crate::integration::{Integration, IntegrationMonitor};
use crate::osc::{Mark, PromptKind};
use crate::shell_integration::{should_inject_zsh, IntegrationDir, ShellKind, ShimNonce};
use crate::{
    BlockList, BlockSegmenter, HeuristicSegmenter, OscScanner, Pty, PtyDimensions, PtyError,
    PtyEvent, Signal, Snapshot, Terminal, TerminalEvent,
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

/// How long, after spawn, the model waits for the first nonce-matched OSC-133 `A`
/// before giving up and concluding a supported shell's hooks are silent (ticket
/// T-2.6). A working shell prints its first prompt - and thus emits `A` - within
/// tens of milliseconds, so this is generous: it bounds only how long the indicator
/// shows the transient "probing" state for a genuinely broken/un-hooked session
/// before it commits to the labeled heuristic fallback. A tuning knob, not a
/// protocol constant.
pub(crate) const INTEGRATION_CONFIRM_WINDOW: Duration = Duration::from_secs(5);

/// Per-command output-capture byte ceiling (ticket T-2.7). While a command runs, its
/// clean output bytes are buffered so the finished block's rows can be captured by
/// replay at `D`; a command whose output exceeds this keeps the head and drops the
/// tail (rare - 8 MiB is tens of thousands of text lines; streaming / packed capture
/// for unbounded output is the follow-up). The buffer is freed once captured.
const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;

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
    /// The model thread publishes the block list here; the render thread reads it to
    /// virtualize the timeline (ticket T-2.7). Mirrors `latest` (a `Mutex<Arc<_>>`
    /// swapped on change): a cheap `Arc` clone for the consumer, an immutable
    /// `BlockList` snapshot re-published only when the list actually changes (block
    /// mutations are human-paced, not per-frame).
    latest_blocks: Arc<Mutex<Arc<BlockList>>>,
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
    /// The materialized shell-integration shim dir (tickets T-2.2 zsh, T-2.3
    /// bash/fish), held for the engine's lifetime so it is removed only when the
    /// engine drops - AFTER `Drop` has joined the threads and killed the child, so
    /// `exec zsh` mid-session still finds it. `None` for unsupported shells / test
    /// commands. Underscore: held for cleanup, not read.
    _integration: Option<IntegrationDir>,
    /// The detected login [`ShellKind`] (`Other` for an unrecognised shell or a test
    /// command). Feeds the T-2.6 integration indicator (Integrated / Heuristic /
    /// None) together with [`Engine::integration_active`].
    shell_kind: ShellKind,
    /// The shell's self-reported version (ticket T-2.3 AC2), captured from the first
    /// nonce-matched `A;aterm_ver=` mark and published here for the T-2.6 indicator's
    /// "why?" string (e.g. "bash 3.2 - upgrade for reliable blocks"). `None` until the
    /// shell reports it (an unintegrated shell never does).
    shell_version: Arc<Mutex<Option<String>>>,
    /// Whether a nonce-armed integration shim was actually installed this session.
    integration_active: bool,
    /// The current [`Integration`] state, published lock-free by the model thread as
    /// it confirms marks / times out (ticket T-2.6). The handle decodes it for the UI
    /// indicator via [`Engine::integration_status`]. Seeded at construction so a read
    /// before the model's first event already reflects the spawn configuration.
    integration_code: Arc<AtomicU8>,
    reader: Option<JoinHandle<()>>,
    model: Option<JoinHandle<()>>,
}

impl Engine {
    /// Spawn a login shell on a fresh PTY and start the engine.
    ///
    /// For each supported shell we install a shell-integration shim (zsh: T-2.2;
    /// bash/fish: T-2.3): a per-session shim dir is materialized, the child is
    /// spawned with the env/args that load it (zero dotfile edits), and the OSC
    /// scanner is armed with the shim's nonce ([`OscScanner::with_nonce`]) so ONLY
    /// our marks are trusted. For an unsupported shell (or if the shim cannot be
    /// installed) we spawn plainly with `-l` and run the scanner untrusted - no
    /// nonce'd marks will appear, and the detected [`ShellKind`] (`Other`) drives the
    /// "Unknown" integration state.
    pub fn spawn_login_shell(dims: PtyDimensions, scrollback: usize) -> Result<Self, PtyError> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let kind = ShellKind::from_path(&shell);

        let nonce = ShimNonce::generate();
        // Try to install the shim for the detected shell. `should_inject_zsh` guards
        // the one footgun (a foreign custom `ZDOTDIR`); bash/fish have no equivalent
        // hijack risk, so they always inject when detected.
        let install: io::Result<Option<IntegrationDir>> = match kind {
            ShellKind::Zsh => {
                let orig_zdotdir = std::env::var("ZDOTDIR").ok();
                if should_inject_zsh(orig_zdotdir.as_deref()) {
                    IntegrationDir::install_zsh(&nonce, orig_zdotdir).map(Some)
                } else {
                    Ok(None)
                }
            }
            ShellKind::Bash => IntegrationDir::install_bash(&nonce).map(Some),
            ShellKind::Fish => {
                IntegrationDir::install_fish(&nonce, std::env::var("XDG_DATA_DIRS").ok()).map(Some)
            }
            ShellKind::Other => Ok(None),
        };

        let mut osc = OscScanner::untrusted();
        let integration = match install {
            Ok(Some(shim)) => {
                osc = OscScanner::with_nonce(nonce.0);
                Some(shim)
            }
            Ok(None) => None,
            Err(e) => {
                log::warn!(
                    "{kind:?} shell-integration shim install failed: {e}; \
                     block segmentation disabled this session"
                );
                None
            }
        };

        // Spawn args: the shim's loader args when installed, else a plain login shell.
        let env = integration
            .as_ref()
            .map(IntegrationDir::env_vars)
            .unwrap_or_default();
        let arg_strings = integration
            .as_ref()
            .map(IntegrationDir::shell_args)
            .unwrap_or_else(|| vec!["-l".to_string()]);
        let args: Vec<&str> = arg_strings.iter().map(String::as_str).collect();

        let pty = Pty::spawn(
            &shell,
            &args,
            dims,
            env.iter().map(|(k, v)| (k.as_str(), v.as_str())),
        )?;
        // The nonce-armed scanner is active iff a shim loaded.
        let shim_installed = integration.is_some();
        Self::spawn_with_pty(
            pty,
            dims,
            scrollback,
            IntegrationSetup {
                osc,
                dir: integration,
                shell_kind: kind,
                shim_installed,
                confirm_window: INTEGRATION_CONFIRM_WINDOW,
            },
        )
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
        // A raw command host is not a recognised login shell: ShellKind::Other ->
        // the integration indicator reports "None" (no blocks). No shim, untrusted
        // scanner.
        Self::spawn_with_pty(
            pty,
            dims,
            scrollback,
            IntegrationSetup {
                osc: OscScanner::untrusted(),
                dir: None,
                shell_kind: ShellKind::Other,
                shim_installed: false,
                confirm_window: INTEGRATION_CONFIRM_WINDOW,
            },
        )
    }

    /// Test-only: spawn an arbitrary `program` while declaring the integration
    /// configuration the engine would otherwise derive from a real login-shell spawn
    /// (ticket T-2.6). Lets a deterministic test drive the Heuristic/None paths and
    /// the confirm gate without a real shell or the production confirmation window:
    /// `shell_kind` + `nonce` (`Some` -> a nonce-armed scanner + shim "installed";
    /// `None` -> an untrusted scanner + no shim) + a short `confirm_window`.
    #[cfg(test)]
    pub(crate) fn spawn_command_with_integration(
        program: &str,
        args: &[&str],
        dims: PtyDimensions,
        scrollback: usize,
        shell_kind: ShellKind,
        nonce: Option<&str>,
        confirm_window: Duration,
    ) -> Result<Self, PtyError> {
        let pty = Pty::spawn(program, args, dims, std::iter::empty::<(&str, &str)>())?;
        let (osc, shim_installed) = match nonce {
            Some(n) => (OscScanner::with_nonce(n), true),
            None => (OscScanner::untrusted(), false),
        };
        Self::spawn_with_pty(
            pty,
            dims,
            scrollback,
            IntegrationSetup {
                osc,
                dir: None,
                shell_kind,
                shim_installed,
                confirm_window,
            },
        )
    }

    /// Wire the reader + model threads around an already-spawned [`Pty`]. `setup`
    /// carries the integration wiring (the OSC scanner, the held shim dir, and the
    /// integration-indicator inputs); see [`IntegrationSetup`].
    fn spawn_with_pty(
        pty: Pty,
        dims: PtyDimensions,
        scrollback: usize,
        setup: IntegrationSetup,
    ) -> Result<Self, PtyError> {
        let IntegrationSetup {
            osc,
            dir: integration,
            shell_kind,
            shim_installed,
            confirm_window,
        } = setup;
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
        // The block-list publish slot (T-2.7): seeded empty so a consumer that reads
        // before the first block opens sees a coherent (empty) list, not a sentinel.
        let latest_blocks = Arc::new(Mutex::new(Arc::new(BlockList::new())));
        // The shell-version slot (T-2.3 AC2): filled from the first `aterm_ver=` mark.
        let shell_version = Arc::new(Mutex::new(None::<String>));

        // Integration indicator state (T-2.6). The monitor's decision logic is pure;
        // the model thread drives it (confirm on a nonce-matched A, time out the
        // confirmation window) and publishes the resulting Integration through this
        // atomic, which the handle decodes for the UI. `shim_installed` is the
        // nonce-armed-scanner fact (the callers pass `integration.is_some()`).
        let monitor = IntegrationMonitor::new(shell_kind, shim_installed);
        let integration_code = Arc::new(AtomicU8::new(monitor.integration().code()));

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
            heuristic: HeuristicSegmenter::new(),
            blocks: BlockList::new(),
            latest_blocks: Arc::clone(&latest_blocks),
            blocks_touched: false,
            capture_buf: Vec::new(),
            capturing: false,
            live_capture: None,
            shell_version: Arc::clone(&shell_version),
            integration: monitor,
            integration_code: Arc::clone(&integration_code),
            confirm_window,
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
            latest_blocks,
            events,
            metrics,
            #[cfg(unix)]
            fg_fd,
            integration_active: shim_installed,
            _integration: integration,
            shell_kind,
            shell_version,
            integration_code,
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

    /// The most recently published block list (ticket T-2.7), for the virtualized
    /// timeline renderer. Cheap: clones an `Arc` under a lock held only for the
    /// refcount bump - the same zero-copy publish discipline as [`Self::
    /// latest_snapshot`]. The returned `BlockList` is an immutable point-in-time view;
    /// the model thread keeps publishing fresh ones as blocks open/close.
    pub fn latest_blocks(&self) -> Arc<BlockList> {
        Arc::clone(&self.latest_blocks.lock().unwrap_or_else(|e| e.into_inner()))
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

    /// The detected login [`ShellKind`]. `Other` means an unrecognised shell with no
    /// integration - the T-2.6 indicator reports it as "Unknown".
    #[must_use]
    pub fn shell_kind(&self) -> ShellKind {
        self.shell_kind
    }

    /// The shell's self-reported version string, if it has reported one (ticket
    /// T-2.3 AC2). With [`Self::shell_kind`] this lets the integration indicator name
    /// the exact shell + version (e.g. "bash 3.2 - upgrade for reliable blocks"). Read
    /// cheaply under a short lock; `None` until the first `aterm_ver=` mark arrives.
    #[must_use]
    pub fn shell_version(&self) -> Option<String> {
        self.shell_version
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Whether a nonce-armed integration shim was installed for this session. `false`
    /// for an unsupported shell or when shim install failed (the T-2.6 indicator then
    /// shows "None" / "Heuristic" rather than "Integrated").
    #[must_use]
    pub fn integration_active(&self) -> bool {
        self.integration_active
    }

    /// The current shell-integration indicator state (ticket T-2.6): the three-state
    /// status (Integrated / Heuristic / None) plus the "why?" reason. Read cheaply
    /// (one relaxed atomic load); the model thread updates it as it confirms the
    /// shim's nonce-matched marks or times out waiting for them, so a UI polling this
    /// observes the live transitions (Probing -> Integrated, or Probing -> Heuristic).
    #[must_use]
    pub fn integration_status(&self) -> Integration {
        Integration::from_code(self.integration_code.load(Ordering::Relaxed))
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

/// The integration-wiring inputs [`Engine::spawn_with_pty`] needs, bundled so its
/// signature stays small (ticket T-2.6 added several). `osc` is the (nonce-armed or
/// untrusted) OSC scanner; `dir` is the materialized shim dir, held for the engine's
/// lifetime and removed when it drops; `shell_kind` + `shim_installed` seed the
/// integration monitor; `confirm_window` bounds how long the model waits for the
/// first nonce-matched `A` before falling back to the heuristic.
struct IntegrationSetup {
    osc: OscScanner,
    dir: Option<IntegrationDir>,
    shell_kind: ShellKind,
    shim_installed: bool,
    confirm_window: Duration,
}

/// The model-thread state: owns the `Term`, the block model, the PTY (for resize
/// + child reaping on drop) and writer, and the publish + metrics handles.
struct Model {
    pty: Pty,
    writer: Box<dyn Write + Send>,
    terminal: Terminal,
    osc: OscScanner,
    segmenter: BlockSegmenter,
    /// The labeled-heuristic fallback segmenter (ticket T-2.6). Sampled each publish
    /// while [`IntegrationMonitor::heuristic_active`] (a supported shell whose marks
    /// never confirmed), it approximates command blocks from the idle grid; dormant
    /// otherwise.
    heuristic: HeuristicSegmenter,
    blocks: BlockList,
    /// The render thread's view of [`Self::blocks`], an immutable `Arc<BlockList>`
    /// re-published only when the list changed (ticket T-2.7). See [`Model::
    /// publish_blocks`].
    latest_blocks: Arc<Mutex<Arc<BlockList>>>,
    /// Set whenever [`Self::blocks`] was mutated since the last block publish (a mark
    /// opened/closed a block, or the heuristic appended one). Gates the clone in
    /// [`Model::publish_blocks`] so an idle session that only streams output re-clones
    /// nothing.
    blocks_touched: bool,
    /// Clean output bytes of the currently-running command, buffered from its `C` mark
    /// so the finished block's rows can be captured by replay at `D` (ticket T-2.7).
    /// Empty + dormant unless a command's output is being captured. Capped at
    /// [`MAX_CAPTURE_BYTES`]; freed once captured.
    capture_buf: Vec<u8>,
    /// Whether a command's output is currently being captured (between its `C` and
    /// `D`). Cleared - and the buffer discarded - if the command turns out to be a
    /// full-screen (alt-screen) app, which has no captured output.
    capturing: bool,
    /// Incremental live-capture terminal for the RUNNING command (ticket T-4.6): created
    /// at its `C`, fed the same clean bytes as `capture_buf` as they arrive, and
    /// snapshotted each publish so the running block carries its LIVE output (the block
    /// model is the source of truth for output, not the evicting grid). `None` unless a
    /// command is running + capturing; dropped at `D`. The authoritative final capture at
    /// `D` still comes from replaying `capture_buf` (unchanged), so this only ever
    /// supplies the in-flight rows.
    live_capture: Option<Terminal>,
    /// The handle's shell-version slot (ticket T-2.3 AC2); filled once from the first
    /// `aterm_ver=` mark.
    shell_version: Arc<Mutex<Option<String>>>,
    /// Integration-indicator decision machine (ticket T-2.6). The model confirms it
    /// on a nonce-matched `A` and times it out via the confirmation-window timer in
    /// [`run_model`], publishing the result through `integration_code`.
    integration: IntegrationMonitor,
    /// The handle's view of [`Self::integration`], published lock-free on change.
    integration_code: Arc<AtomicU8>,
    /// How long [`run_model`] waits for the first nonce-matched `A` before concluding
    /// the hooks are silent (ticket T-2.6). [`INTEGRATION_CONFIRM_WINDOW`] in
    /// production; tests inject a short window to exercise the heuristic path fast.
    confirm_window: Duration,
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
                let seg = &passthrough[fed..rel];
                self.terminal.feed(seg);
                self.capture_feed(seg);
                fed = rel;
            }
            self.segmenter
                .set_alt_screen(self.terminal.is_alt_screen(), &mut self.blocks);
            self.segmenter.apply(mark, *offset, &mut self.blocks);
            // A nonce-matched `A` confirms the shim's hooks are live (ticket T-2.6).
            // Gate on the shim being installed: only the nonce-armed scanner yields
            // trusted marks, so an untrusted scanner (no shim) must not let a forged
            // `A` in command output falsely flip the indicator to "Integrated".
            if self.integration.shim_installed()
                && matches!(mark, Mark::Prompt(PromptKind::PromptStart))
            {
                self.integration.confirm();
            }
            // The shell's self-reported version (ticket T-2.3 AC2), emitted once on the
            // first `A`. Only nonce-trusted marks reach here, so it is safe to surface.
            if let Mark::ShellVersion(ver) = mark {
                let mut guard = self.shell_version.lock().unwrap_or_else(|e| e.into_inner());
                if guard.is_none() {
                    *guard = Some(ver.clone());
                }
            }
            // Finished-block output capture (ticket T-2.7): buffer this command's clean
            // output from its `C`, capture it by replay on `D`. Gated OFF the alt screen
            // so a full-screen app's bytes are never captured (its block is Interactive,
            // output-less). The byte stream - not the live grid - is the capture source,
            // so output that scrolled out of the grid still survives.
            if !self.terminal.is_alt_screen() {
                match mark {
                    Mark::Prompt(PromptKind::OutputStart) => {
                        self.capturing = true;
                        self.capture_buf.clear();
                        // Open a fresh incremental live-capture terminal for this
                        // command so its running block streams live output (T-4.6).
                        self.live_capture = Some(Terminal::new_capture(self.terminal.cols()));
                    }
                    Mark::Prompt(PromptKind::CommandDone { .. }) => self.capture_finish(),
                    // Missing-`D` recovery: the segmenter just closed the orphan; capture
                    // whatever output it produced before it was interrupted.
                    Mark::Prompt(PromptKind::PromptStart) if self.capturing => {
                        self.capture_finish();
                    }
                    _ => {}
                }
            }
        }
        if fed < passthrough.len() {
            let seg = &passthrough[fed..];
            self.terminal.feed(seg);
            self.capture_feed(seg);
        }
        self.clean_offset += passthrough.len();
        // Feed the heuristic detector its newline signal (ticket T-2.6) - "a command
        // actually ran since the last prompt" - but ONLY in the labeled-heuristic
        // fallback and OFF the alt screen, so a full-screen TUI cannot drive block
        // boundaries. The common mark-driven path skips this entirely.
        if self.integration.heuristic_active() && !self.terminal.is_alt_screen() {
            self.heuristic.observe_output(passthrough);
        }
        // A mark in this chunk mutated the block list (opened/closed/flagged a block
        // via the segmenter), so the next publish must re-publish the blocks snapshot
        // for the render thread (ticket T-2.7). The heuristic's own appends happen in
        // `publish` and set the flag there.
        self.blocks_touched |= !scan.marks.is_empty();
        // The feed above may have raised DA/DSR/cursor-position replies into the
        // terminal's reply channel; write them back to the PTY (ticket T-1.9).
        self.write_replies();
        self.metrics
            .bytes_drained
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        self.metrics
            .blocks
            .store(self.blocks.len(), Ordering::Relaxed);
        // Publish any integration-state change (e.g. a just-confirmed `A`) so the UI
        // indicator sees it without waiting for the next publish tick (T-2.6).
        self.publish_integration();
    }

    /// Buffer a segment of clean command output for capture, while a command's output
    /// is being captured (ticket T-2.7). Discards the in-flight capture if a
    /// full-screen app has taken the screen (its block is Interactive, output-less).
    /// Capped at [`MAX_CAPTURE_BYTES`]; excess is dropped (head kept).
    fn capture_feed(&mut self, bytes: &[u8]) {
        if !self.capturing {
            return;
        }
        if self.terminal.is_alt_screen() {
            self.capturing = false;
            self.capture_buf.clear();
            self.live_capture = None; // a full-screen app has no captured output
            return;
        }
        let room = MAX_CAPTURE_BYTES.saturating_sub(self.capture_buf.len());
        if room > 0 {
            let take = room.min(bytes.len());
            self.capture_buf.extend_from_slice(&bytes[..take]);
            // Feed the SAME bytes to the incremental live terminal (T-4.6) so the
            // running block's live rows stay current without re-replaying the buffer.
            if let Some(t) = self.live_capture.as_mut() {
                t.feed(&bytes[..take]);
            }
        }
    }

    /// Capture the running command's buffered output into its block's immutable row
    /// snapshot by replaying the bytes (ticket T-2.7), then free the buffer. No-op when
    /// nothing is being captured (e.g. the command turned out to be a full-screen app).
    fn capture_finish(&mut self) {
        if !self.capturing {
            return;
        }
        self.capturing = false;
        // Drop the incremental live terminal; the authoritative final capture below
        // comes from replaying the full byte buffer (unchanged from T-2.7).
        self.live_capture = None;
        if !self.blocks.is_empty() {
            let idx = self.blocks.len() - 1;
            let rows = Terminal::capture_output_rows(self.terminal.cols(), &self.capture_buf);
            self.blocks.set_block_output(idx, rows);
            self.blocks_touched = true;
        }
        // Free the buffer between commands so a large command does not pin memory.
        self.capture_buf = Vec::new();
    }

    /// Publish the current [`Integration`] to the handle's lock-free slot (T-2.6).
    /// Cheap (one relaxed store); called whenever the monitor may have changed.
    fn publish_integration(&self) {
        self.integration_code
            .store(self.integration.integration().code(), Ordering::Relaxed);
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
    fn publish(&mut self, idle: bool) {
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

        // Labeled-heuristic fallback (ticket T-2.6): at an IDLE flush, once a
        // supported shell's marks have failed to confirm, detect command-cycle
        // boundaries structurally - the cursor settled mid-line (`col > 0`) after
        // output has gone quiet is the shell sitting at a prompt (the dossier's
        // "newline + cursor-at-col-0" signal; the newline count comes from
        // `observe_output`). Gated on `idle` so a mid-flood publish never mistakes a
        // streaming pause for a prompt, on `heuristic_active` so the common
        // mark-driven path pays nothing, and OFF the alt screen so a full-screen TUI's
        // cursor cannot fabricate blocks (the hazard the mark-driven segmenter guards
        // against). Reuses the freshly-rendered `back` snapshot.
        if idle && self.integration.heuristic_active() {
            let (cursor_col, alt_screen) = {
                let snap = &*self.back;
                (snap.cursor.col, snap.alt_screen)
            };
            if !alt_screen {
                let before = self.blocks.len();
                self.heuristic
                    .note_prompt_if_idle(cursor_col, self.clean_offset, &mut self.blocks);
                if self.blocks.len() != before {
                    // The heuristic appended an approximate block - re-publish below.
                    self.blocks_touched = true;
                }
                self.metrics
                    .blocks
                    .store(self.blocks.len(), Ordering::Relaxed);
            }
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

        // Stream the running command's LIVE output into its block (ticket T-4.6): snapshot
        // the incremental live-capture terminal and set it as the running (tail) block's
        // output, so the timeline renders the in-flight command from the block model. The
        // final, authoritative capture still lands at `D` (`capture_finish`). Gated on an
        // actively-capturing command with a running tail block, so an idle session pays
        // nothing here.
        if self.capturing {
            if let Some(t) = self.live_capture.as_ref() {
                if let Some(idx) = self.blocks.len().checked_sub(1) {
                    if self.blocks.get(idx).is_some_and(|b| b.is_running()) {
                        self.blocks.set_block_output(idx, t.live_output_rows());
                        self.blocks_touched = true;
                    }
                }
            }
        }

        // Publish the block list too, if it changed since the last publish (ticket
        // T-2.7). It rides the same publish() call as the snapshot but under its OWN
        // lock, so the (snapshot, blocks) pair is *eventually consistent*, not atomic:
        // each side is internally coherent, but a consumer can briefly observe a new
        // snapshot paired with the previous block list (it self-heals next frame).
        // Today's only consumer reads a SumTree count that is unused while alt-screen,
        // so the skew is invisible; a future consumer that draws block geometry keyed
        // off the snapshot's alt_screen flag should reconcile (e.g. stamp the publish
        // version onto both). Cheap (a flag check) when nothing changed.
        self.publish_blocks();
    }

    /// Re-publish the block list to the render thread when it changed since the last
    /// publish (ticket T-2.7). Clones the current [`BlockList`] into a fresh immutable
    /// `Arc` and swaps it into `latest_blocks` (the lock held only for the pointer
    /// swap). Skipped entirely - no clone, no lock - when `blocks_touched` is clear,
    /// so an output-only flood (no new/closed blocks) pays nothing here.
    fn publish_blocks(&mut self) {
        if !self.blocks_touched {
            return;
        }
        let snapshot = Arc::new(self.blocks.clone());
        {
            let mut guard = self.latest_blocks.lock().unwrap_or_else(|e| e.into_inner());
            *guard = snapshot;
        }
        self.blocks_touched = false;
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
                // Reflow the incremental live-capture terminal too (T-4.6) so a running
                // command's live rows re-wrap to the new width, like the main grid.
                if let Some(t) = self.live_capture.as_mut() {
                    t.resize(t.rows(), cols as usize);
                }
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

    // One-shot confirmation-window timer (ticket T-2.6): armed only while we are
    // probing a supported shell with a live shim. If the first nonce-matched `A`
    // arrives first we disarm it (integration confirmed); if it fires first the
    // shell's hooks are silent and we fall back to the labeled heuristic. A session
    // that cannot reach "Integrated" (unsupported shell, or a shim that failed to
    // install) never arms it - there is nothing to wait for.
    let mut integration_timer: Receiver<Instant> = if model.integration.awaiting_confirmation() {
        after(model.confirm_window)
    } else {
        never()
    };

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
                        // Final coherent frame, then shut down. Not an idle prompt
                        // flush - the shell is gone, so no heuristic sampling.
                        model.publish(false);
                        break;
                    }
                    // Anti-starvation: service control + detect Engine-drop.
                    let (shutdown, _) = model.drain_control(mailbox);
                    if shutdown {
                        break;
                    }
                    // A nonce-matched `A` confirmed integration during this drain:
                    // stop waiting for it (ticket T-2.6).
                    if model.integration.is_confirmed() {
                        integration_timer = never();
                    }
                    // Flush if the window elapsed during this drain (sustained
                    // flood); otherwise keep coalescing and let the timer flush
                    // once the bytes go idle. A sustained-flood flush is NOT idle -
                    // output is still streaming - so it does not sample the heuristic.
                    if Instant::now() >= dl {
                        model.publish(false);
                        deadline = None;
                        timer = never();
                    }
                }
                Err(_) => break, // reader gone (PTY closed) -> shut down
            },
            recv(timer) -> _ => {
                // The window elapsed with no further bytes: the burst went idle.
                // Publish the merged state once (an IDLE flush, so the heuristic
                // detector samples for a settled prompt). Disarmed so it does not
                // refire.
                model.publish(true);
                deadline = None;
                timer = never();
            },
            recv(integration_timer) -> _ => {
                // The confirmation window elapsed with no nonce-matched `A`: a
                // supported shell's hooks are silent. Commit to the labeled heuristic
                // fallback and publish the transition for the indicator (ticket
                // T-2.6). Disarmed so it fires once.
                model.integration.note_window_elapsed();
                model.publish_integration();
                // Seed the now-active heuristic detector from the current grid: the
                // shell is typically sitting idle at its first prompt, which produced
                // no pending output to trigger a coalesced publish - so without this
                // idle sample the detector would not anchor that first prompt. `publish`
                // samples only when the heuristic is active, so this is a no-op when it
                // confirmed instead.
                model.publish(true);
                deadline = None;
                timer = never();
                integration_timer = never();
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

    #[test]
    fn unsupported_command_host_reports_integration_none() {
        // AC3 (T-2.6): a raw command host is `ShellKind::Other` - the integration
        // indicator reports None, with a populated "why", and no shim is active.
        // Deterministic: needs no real shell.
        let engine = Engine::spawn_command("/bin/cat", &[], dims(), 1_000).expect("spawn cat");
        assert_eq!(engine.shell_kind(), ShellKind::Other);
        assert!(!engine.integration_active(), "no shim for a raw command");
        let integ = engine.integration_status();
        assert_eq!(integ.status, crate::IntegrationStatus::None);
        assert!(integ.why().is_some(), "AC4: the None state carries a why");
        drop(engine);
    }

    #[test]
    fn login_shell_reaches_integrated_after_first_prompt() {
        // AC1 + AC5 (T-2.6), end-to-end: a real supported login shell installs the
        // shim, emits a nonce-matched `A` on its first prompt, and the engine
        // transitions the indicator from probing to Integrated. Skip when $SHELL is
        // unsupported or the shim could not arm (so CI hosts stay honest without
        // flaking) - mirrors the real-shell PTY tests' skip-if pattern.
        let engine = Engine::spawn_login_shell(dims(), 1_000).expect("spawn login shell");
        if engine.shell_kind() == ShellKind::Other || !engine.integration_active() {
            eprintln!(
                "skip login_shell_reaches_integrated: no nonce-armed shim for $SHELL \
                 (kind={:?}, active={})",
                engine.shell_kind(),
                engine.integration_active()
            );
            return;
        }
        // The first prompt's `A` arrives within milliseconds; poll for the
        // Integrated transition well inside the 5s confirmation window.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut status = engine.integration_status().status;
        while status != crate::IntegrationStatus::Integrated && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            status = engine.integration_status().status;
        }
        assert_eq!(
            status,
            crate::IntegrationStatus::Integrated,
            "a supported login shell should confirm integration via its first prompt's \
             nonce-matched A (never silently)"
        );
        drop(engine);
    }

    /// Poll `integration_status().status` until it equals `want` or `timeout`.
    fn wait_for_status(
        engine: &Engine,
        want: crate::IntegrationStatus,
        timeout: Duration,
    ) -> crate::IntegrationStatus {
        let deadline = Instant::now() + timeout;
        loop {
            let s = engine.integration_status().status;
            if s == want || Instant::now() >= deadline {
                return s;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn nonce_matched_a_confirms_integrated_at_the_engine() {
        // AC1, deterministic at the engine layer: a child emitting a correctly-nonced
        // OSC-133 `A` through the nonce-armed scanner confirms integration -> the
        // indicator transitions to Integrated. (The skip-if real-shell test above is
        // the end-to-end counterpart; this pins the wiring without a real shell.)
        let nonce = "ATERMTESTNONCE0";
        let script = format!("printf '\\033]133;A;aterm_nonce={nonce}\\007'; sleep 1");
        let engine = Engine::spawn_command_with_integration(
            "/bin/sh",
            &["-c", &script],
            dims(),
            1_000,
            ShellKind::Bash,
            Some(nonce),
            Duration::from_millis(300),
        )
        .expect("spawn sh emitting a nonced A");
        let status = wait_for_status(
            &engine,
            crate::IntegrationStatus::Integrated,
            Duration::from_secs(5),
        );
        assert_eq!(
            status,
            crate::IntegrationStatus::Integrated,
            "a nonce-matched A must confirm Integrated"
        );
        drop(engine);
    }

    #[test]
    fn running_command_block_carries_live_output_before_it_finishes() {
        // T-4.6 real fix, deterministic at the engine layer: while a command is still
        // RUNNING (its `D` has not arrived), its block must already carry the command's
        // LIVE output - the engine streams the incremental live-capture terminal into the
        // running block each publish, so the timeline renders in-flight output from the
        // block model (not the evicting grid). A nonced `A` then `C` open a running block;
        // the marker row is printed; a long sleep keeps the block open while we observe.
        let nonce = "ATERMLIVECAP00";
        let script = format!(
            "printf '\\033]133;A;aterm_nonce={nonce}\\007'; \
             printf '\\033]133;C;aterm_nonce={nonce}\\007'; \
             printf 'live-capture-row\\n'; sleep 5"
        );
        let engine = Engine::spawn_command_with_integration(
            "/bin/sh",
            &["-c", &script],
            dims(),
            1_000,
            ShellKind::Bash,
            Some(nonce),
            Duration::from_millis(300),
        )
        .expect("spawn sh emitting live output before D");

        // Poll until the last (running) block carries the marker row, well inside the 5s
        // sleep - i.e. BEFORE the command finishes.
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut found = false;
        while Instant::now() < deadline {
            let blocks = engine.latest_blocks();
            if let Some(b) = blocks.iter().last() {
                let has_row = b.output.iter().any(|r| {
                    r.cells
                        .iter()
                        .map(|c| c.c)
                        .collect::<String>()
                        .contains("live-capture-row")
                });
                if b.is_running() && has_row {
                    found = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            found,
            "a still-running command's block must carry its live output before D (the \
             incremental live capture streams into the block model)"
        );
        drop(engine);
    }

    #[test]
    fn forged_a_without_a_shim_never_confirms_integrated() {
        // AC1 crown-jewel gate (review): in a supported-shell session whose shim did
        // NOT install (untrusted scanner), a forged un-nonced `133;A` in output must
        // NOT flip the indicator to Integrated - it stays Heuristic. This is the
        // `shim_installed()` confirm gate, tested where it actually lives (the engine).
        let engine = Engine::spawn_command_with_integration(
            "/bin/sh",
            &["-c", "printf '\\033]133;A\\007'; sleep 1"],
            dims(),
            1_000,
            ShellKind::Bash,
            None, // no shim -> untrusted scanner -> confirm gate must reject the A
            Duration::from_millis(200),
        )
        .expect("spawn sh emitting a forged A");
        // Give the A time to be parsed and the (short) confirmation window to elapse.
        std::thread::sleep(Duration::from_millis(600));
        let integ = engine.integration_status();
        assert_ne!(
            integ.status,
            crate::IntegrationStatus::Integrated,
            "a forged A with no shim must never confirm Integrated"
        );
        assert_eq!(
            integ.status,
            crate::IntegrationStatus::Heuristic,
            "a supported shell with no shim stays Heuristic"
        );
        drop(engine);
    }

    #[test]
    fn nonce_mismatched_a_never_confirms_integrated() {
        // A child emitting an `A` stamped with the WRONG nonce (the nonce-armed
        // scanner drops it) must not confirm - the indicator never reaches Integrated.
        let script = "printf '\\033]133;A;aterm_nonce=WRONGNONCE\\007'; sleep 1";
        let engine = Engine::spawn_command_with_integration(
            "/bin/sh",
            &["-c", script],
            dims(),
            1_000,
            ShellKind::Bash,
            Some("REALNONCE0"),
            Duration::from_millis(200),
        )
        .expect("spawn sh emitting a mismatched-nonce A");
        std::thread::sleep(Duration::from_millis(600));
        assert_ne!(
            engine.integration_status().status,
            crate::IntegrationStatus::Integrated,
            "a wrong-nonce A is dropped by the scanner and must not confirm"
        );
        drop(engine);
    }

    #[test]
    fn heuristic_session_produces_approximate_blocks() {
        // AC2, end-to-end at the engine: a supported shell with no live hooks
        // (Heuristic) must still produce labeled approximate blocks. A no-shim
        // session is heuristic-active immediately; the `/bin/sh` script prints a
        // prompt (cursor mid-line), runs a "command" (newlines), and redraws the
        // prompt - one command cycle per redraw - with pauses so each settles into an
        // idle publish the detector samples.
        let script = "printf 'P> '; sleep 0.4; printf 'a\\nb\\nP> '; sleep 0.4; \
                      printf 'c\\nP> '; sleep 0.8";
        let engine = Engine::spawn_command_with_integration(
            "/bin/sh",
            &["-c", script],
            dims(),
            1_000,
            ShellKind::Bash,
            None, // no shim -> Heuristic (ShimInstallFailed), detector active now
            Duration::from_millis(100),
        )
        .expect("spawn sh simulating a no-hooks shell");
        assert_eq!(
            engine.integration_status().status,
            crate::IntegrationStatus::Heuristic,
            "a supported shell with no shim is Heuristic"
        );
        // The script is a deterministic two-cycle scenario (anchor at `P> `, then the
        // `a\nb` cycle, then the `c` cycle), so the heuristic must segment EXACTLY two
        // approximate blocks. Asserting the exact count (not just >= 1) guards against
        // the over-segmentation a mid-line/progress-pause regression would cause.
        let deadline = Instant::now() + Duration::from_secs(6);
        while engine.block_count() < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        // Let any (erroneous) extra segmentation settle, then pin the exact count.
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            engine.block_count(),
            2,
            "the heuristic must segment exactly two command cycles, no more (no \
             mid-output over-segmentation)"
        );
        drop(engine);
    }

    #[test]
    fn latest_blocks_publishes_the_segmented_list_to_consumers() {
        // T-2.7 seam: the model thread publishes the BlockList to the render thread.
        // A no-shim supported shell is heuristic-active; the same two-cycle script as
        // `heuristic_session_produces_approximate_blocks` must surface TWO approximate
        // blocks through `latest_blocks()` (the render-thread view), not just the
        // `block_count` metric.
        let script = "printf 'P> '; sleep 0.4; printf 'a\\nb\\nP> '; sleep 0.4; \
                      printf 'c\\nP> '; sleep 0.8";
        let engine = Engine::spawn_command_with_integration(
            "/bin/sh",
            &["-c", script],
            dims(),
            1_000,
            ShellKind::Bash,
            None,
            Duration::from_millis(100),
        )
        .expect("spawn sh simulating a no-hooks shell");
        // Before any prompt the published list is the seeded-empty one.
        assert_eq!(
            engine.latest_blocks().len(),
            0,
            "seeded empty before any block"
        );

        let deadline = Instant::now() + Duration::from_secs(6);
        while engine.latest_blocks().len() < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let blocks = engine.latest_blocks();
        assert_eq!(
            blocks.len(),
            2,
            "the published block list must carry the two segmented cycles"
        );
        assert!(
            blocks.iter().all(|b| b.approximate),
            "a heuristic session's published blocks are labeled approximate"
        );
        drop(engine);
    }

    #[test]
    fn finished_block_captures_its_output_rows() {
        // T-2.7 end to end: a full A;C;output;D cycle through the nonce-armed scanner
        // closes a block whose immutable output rows are captured (by byte replay).
        let nonce = "ATERMCAP0000";
        let script = format!(
            "printf '\\033]133;A;aterm_nonce={n}\\007'; \
             printf '\\033]133;C;aterm_nonce={n}\\007'; \
             printf 'alpha\\nbravo\\ncharlie\\n'; \
             printf '\\033]133;D;0;aterm_nonce={n}\\007'; \
             sleep 2",
            n = nonce
        );
        let engine = Engine::spawn_command_with_integration(
            "/bin/sh",
            &["-c", &script],
            dims(),
            1_000,
            ShellKind::Bash,
            Some(nonce),
            Duration::from_secs(5),
        )
        .expect("spawn sh emitting a full command cycle");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let blocks = engine.latest_blocks();
            if let Some(b) = blocks.get(0) {
                if !b.is_running() && !b.output.is_empty() {
                    let text: Vec<String> = b
                        .output
                        .iter()
                        .map(|r| r.cells.iter().map(|c| c.c).collect())
                        .collect();
                    assert!(
                        text.iter().any(|t| t.contains("alpha")),
                        "captured output should contain the first line, got {text:?}"
                    );
                    assert!(
                        text.iter().any(|t| t.contains("charlie")),
                        "captured output should contain the last line, got {text:?}"
                    );
                    drop(engine);
                    return;
                }
            }
            assert!(
                Instant::now() < deadline,
                "the finished block's output rows were not captured in time"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn shell_version_is_surfaced_from_the_first_prompt() {
        // T-2.3 AC2: the shell reports its version on the first `A` (aterm_ver=); the
        // engine surfaces it through `shell_version()` for the indicator.
        let nonce = "ATERMVER0000";
        let script =
            format!("printf '\\033]133;A;aterm_ver=5.2.15;aterm_nonce={nonce}\\007'; sleep 2");
        let engine = Engine::spawn_command_with_integration(
            "/bin/sh",
            &["-c", &script],
            dims(),
            1_000,
            ShellKind::Bash,
            Some(nonce),
            Duration::from_secs(5),
        )
        .expect("spawn sh reporting its version");

        let deadline = Instant::now() + Duration::from_secs(5);
        while engine.shell_version().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            engine.shell_version().as_deref(),
            Some("5.2.15"),
            "the engine should surface the shell's reported version"
        );
        drop(engine);
    }
}
