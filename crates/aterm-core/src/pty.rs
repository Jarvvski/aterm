//! PTY: spawn a hidden login shell on a pseudo-terminal and hand the caller an
//! owned facade for resize, input, signals, and child reaping.
//!
//! This is the very bottom of the engine (ticket T-1.1): a thin, typed wrapper
//! over [`portable_pty`]. It deliberately owns NO threads and NO channels - the
//! reader thread + bounded backpressure are the three-thread model's job
//! (ticket T-1.3). Callers obtain raw byte I/O via [`Pty::try_clone_reader`] /
//! [`Pty::take_writer`] and drive it however they like.
//!
//! Drop order matters: spawning drops the slave half immediately so that when the
//! child exits, a read on the master returns EOF (no lingering slave fd keeps the
//! pty open). Dropping the [`Pty`] best-effort kills and reaps the child (so it is
//! leak-free by construction - no orphaned shell, no zombie), then drops the
//! master, which closes the last slave reference and unblocks any reader.

use std::io::{Read, Write};

use portable_pty::{
    Child, ChildKiller, CommandBuilder, MasterPty, NativePtySystem, PtyPair, PtySize, PtySystem,
};

pub use portable_pty::ExitStatus;

#[cfg(unix)]
use std::os::unix::io::RawFd;

/// Errors from PTY setup / control.
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
    #[error("failed to resize pty: {0}")]
    Resize(String),
    #[error("foreground signal failed: {0}")]
    Signal(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// A signal aterm can deliver to the terminal's foreground process group - the
/// Ctrl-C / agent-cancel path (ticket T-1.9). Platform-neutral at the API surface;
/// it maps to the corresponding OS signal on Unix and is unsupported elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// `SIGINT` - interrupt (Ctrl-C).
    Interrupt,
    /// `SIGQUIT` - quit (Ctrl-\).
    Quit,
    /// `SIGTERM` - polite terminate.
    Terminate,
    /// `SIGKILL` - force kill (uncatchable).
    Kill,
    /// `SIGTSTP` - stop (Ctrl-Z).
    Stop,
    /// `SIGCONT` - continue a stopped group.
    Continue,
}

#[cfg(unix)]
impl Signal {
    /// Map to the `nix` signal. Kept private so `nix` does not leak into the API.
    fn to_nix(self) -> nix::sys::signal::Signal {
        use nix::sys::signal::Signal as S;
        match self {
            Signal::Interrupt => S::SIGINT,
            Signal::Quit => S::SIGQUIT,
            Signal::Terminate => S::SIGTERM,
            Signal::Kill => S::SIGKILL,
            Signal::Stop => S::SIGTSTP,
            Signal::Continue => S::SIGCONT,
        }
    }
}

/// A chunk of bytes read from the PTY, or an end-of-stream signal.
///
/// The shared I/O vocabulary between the reader and whoever consumes its output.
/// The reader thread that actually produces these lives in the three-thread model
/// (ticket T-1.3); this type lives here so both sides agree on the shape.
#[derive(Debug)]
pub enum PtyEvent {
    /// Raw bytes read from the shell.
    Output(Vec<u8>),
    /// The shell closed its end (EOF) or the reader errored out.
    Exited,
}

/// Grid dimensions for the PTY (in cells), plus pixel size for apps that care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// A live PTY hosting a child process (normally a login shell).
///
/// Owns the master half and the [`Child`] handle. The slave half is dropped at
/// spawn time so the master sees EOF when the child exits.
pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    dims: PtyDimensions,
    /// The master pty fd, retained so the foreground-pgroup signalling work
    /// (ticket T-1.9) can `killpg`/`tcsetattr` against it. `None` if the platform
    /// or backend does not expose one.
    #[cfg(unix)]
    master_fd: Option<RawFd>,
}

impl Pty {
    /// Spawn the user's login shell (`$SHELL`, falling back to `/bin/zsh`) as a
    /// true login shell (`-l`). Terminal capabilities (`TERM`) and the aterm
    /// marker (`ATERM`) are advertised; no shell-integration env is injected here
    /// - that is ticket T-2.2's job, which calls [`Pty::spawn`] with its env.
    ///
    /// The login-vs-interactive choice is owner open-question #3 in the dossier
    /// (`03-pty-vt-rust.md`); we default to login and leave [`Pty::spawn`] as the
    /// config seam.
    pub fn spawn_login_shell(dims: PtyDimensions) -> Result<Self, PtyError> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        Self::spawn(&shell, &["-l"], dims, std::iter::empty::<(&str, &str)>())
    }

    /// Spawn `program` with `args` on a fresh PTY sized `dims`, applying the
    /// extra `env` on top of sensible terminal defaults.
    ///
    /// `env` is the hook point for shell-integration injection (ticket T-2.2):
    /// entries are applied AFTER the `TERM`/`ATERM` defaults, so a caller may
    /// override them. This function spawns no threads.
    pub fn spawn<I, K, V>(
        program: &str,
        args: &[&str],
        dims: PtyDimensions,
        env: I,
    ) -> Result<Self, PtyError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<std::ffi::OsStr>,
        V: AsRef<std::ffi::OsStr>,
    {
        let pty_system = NativePtySystem::default();
        let pair: PtyPair = pty_system
            .openpty(dims.into())
            .map_err(|e| PtyError::Open(e.to_string()))?;

        let mut cmd = CommandBuilder::new(program);
        for a in args {
            cmd.arg(a);
        }
        // Terminal capability + aterm marker defaults; caller env can override.
        cmd.env("TERM", "xterm-256color");
        cmd.env("ATERM", "1");
        for (k, v) in env {
            cmd.env(k, v);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| PtyError::Spawn(e.to_string()))?;

        // Retain the master fd before we move the master into `self`.
        #[cfg(unix)]
        let master_fd = pair.master.as_raw_fd();

        let master = pair.master;
        // Drop the slave so the master observes EOF once the child exits. Holding
        // it open would keep the pty alive and the reader would block forever.
        drop(pair.slave);

        Ok(Self {
            master,
            child,
            dims,
            #[cfg(unix)]
            master_fd,
        })
    }

    /// Resize the PTY (on window resize). Updates the kernel window size via
    /// `TIOCSWINSZ` so the shell re-flows and `SIGWINCH` fires. Debouncing is the
    /// caller's responsibility (the model thread coalesces resizes).
    pub fn resize(&mut self, dims: PtyDimensions) -> Result<(), PtyError> {
        self.master
            .resize(dims.into())
            .map_err(|e| PtyError::Resize(e.to_string()))?;
        self.dims = dims;
        Ok(())
    }

    /// Current dimensions.
    pub fn dimensions(&self) -> PtyDimensions {
        self.dims
    }

    /// Take the writer half (the child's stdin). Per `portable-pty`, it is invalid
    /// to take the writer more than once; the caller owns it thereafter.
    pub fn take_writer(&self) -> Result<Box<dyn Write + Send>, PtyError> {
        self.master
            .take_writer()
            .map_err(|e| PtyError::Writer(e.to_string()))
    }

    /// Clone a reader over the child's output. The reader thread (ticket T-1.3)
    /// owns it.
    pub fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, PtyError> {
        self.master
            .try_clone_reader()
            .map_err(|e| PtyError::Reader(e.to_string()))
    }

    /// The child's process id, if the backend exposes one.
    pub fn process_id(&self) -> Option<u32> {
        self.child.process_id()
    }

    /// Poll the child without blocking. `Ok(None)` means still running.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, PtyError> {
        Ok(self.child.try_wait()?)
    }

    /// Block until the child exits, returning its status.
    pub fn wait(&mut self) -> Result<ExitStatus, PtyError> {
        Ok(self.child.wait()?)
    }

    /// Terminate the child process.
    pub fn kill(&mut self) -> Result<(), PtyError> {
        Ok(self.child.kill()?)
    }

    /// A detachable killer so another thread can signal the child while this
    /// handle is blocked in [`Pty::wait`] (used by the signal path, ticket T-1.9).
    pub fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        self.child.clone_killer()
    }

    /// The retained master pty fd (Unix), for the foreground-pgroup signal work
    /// in ticket T-1.9. `None` if the backend does not expose one.
    #[cfg(unix)]
    pub fn master_fd(&self) -> Option<RawFd> {
        self.master_fd
    }
}

/// The terminal's foreground process group id, read from the pty `fd` via
/// `tcgetpgrp(3)` (ticket T-1.9). `None` if there is no foreground group or the
/// call fails (e.g. the fd is no longer a terminal). Both the master and slave
/// fds of a pty report the same controlling-terminal foreground group, so the
/// retained master fd is a valid argument.
#[cfg(unix)]
pub(crate) fn foreground_pgid(fd: std::os::fd::BorrowedFd<'_>) -> Option<i32> {
    nix::unistd::tcgetpgrp(fd).ok().map(|pid| pid.as_raw())
}

/// Send `sig` to the terminal's foreground process group (`killpg` on the result
/// of `tcgetpgrp`) so Ctrl-C / agent-cancel hits the right process, not the
/// session leader (ticket T-1.9). Returns [`PtyError::Signal`] if the foreground
/// group cannot be resolved or the signal cannot be delivered.
#[cfg(unix)]
pub(crate) fn signal_foreground(
    fd: std::os::fd::BorrowedFd<'_>,
    sig: Signal,
) -> Result<(), PtyError> {
    let pgid =
        nix::unistd::tcgetpgrp(fd).map_err(|e| PtyError::Signal(format!("tcgetpgrp: {e}")))?;
    // SAFETY-CRITICAL guard: `killpg` with a pgrp <= 1 is platform-specific and
    // dangerous - `killpg(0, ..)` signals the CALLER's own process group (us!) and
    // 1 is init. A genuine terminal foreground group always has a pgid > 1, so a
    // value <= 1 means "no real foreground group"; refuse rather than signal.
    if pgid.as_raw() <= 1 {
        return Err(PtyError::Signal(format!(
            "refusing to signal non-foreground pgid {}",
            pgid.as_raw()
        )));
    }
    nix::sys::signal::killpg(pgid, sig.to_nix())
        .map_err(|e| PtyError::Signal(format!("killpg({}): {e}", pgid.as_raw())))
}

impl Drop for Pty {
    /// Best-effort terminate-and-reap so the facade leaks nothing: a spawned shell
    /// is a session leader (`setsid` + `TIOCSCTTY`), so it does NOT die when this
    /// process exits, and `std::process::Child` (portable-pty's Unix child) does
    /// not reap on drop. We `kill` (no-op/`Err` if already gone, ignored) then
    /// `wait` to reap the zombie. Graceful pgroup signalling is ticket T-1.9; this
    /// is the safety floor. `wait` returns promptly because the child is dead.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crossbeam_channel::{Receiver, RecvTimeoutError};
    use std::time::{Duration, Instant};

    /// Spawn a thread that drains `reader` into a channel of byte chunks until
    /// EOF, so a test can read with a timeout instead of risking a blocking hang.
    fn reader_channel(mut reader: Box<dyn Read + Send>) -> Receiver<Vec<u8>> {
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });
        rx
    }

    /// Accumulate bytes from `rx` until `needle` appears or `timeout` elapses.
    fn read_until(rx: &Receiver<Vec<u8>>, needle: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        let mut acc: Vec<u8> = Vec::new();
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            match rx.recv_timeout(deadline - now) {
                Ok(chunk) => {
                    acc.extend_from_slice(&chunk);
                    if String::from_utf8_lossy(&acc).contains(needle) {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        String::from_utf8_lossy(&acc).into_owned()
    }

    fn small() -> PtyDimensions {
        PtyDimensions {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    #[test]
    fn spawn_echo_reads_output() {
        let pty = Pty::spawn(
            "/bin/echo",
            &["hello"],
            small(),
            std::iter::empty::<(&str, &str)>(),
        )
        .expect("spawn echo");
        let rx = reader_channel(pty.try_clone_reader().expect("clone reader"));
        let out = read_until(&rx, "hello", Duration::from_secs(5));
        assert!(
            out.contains("hello"),
            "expected 'hello' in pty output, got {out:?}"
        );
    }

    #[test]
    fn resize_succeeds() {
        let mut pty = Pty::spawn("/bin/cat", &[], small(), std::iter::empty::<(&str, &str)>())
            .expect("spawn cat");
        pty.resize(PtyDimensions {
            rows: 50,
            cols: 132,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("resize ok");
        assert_eq!(pty.dimensions().rows, 50);
        assert_eq!(pty.dimensions().cols, 132);
        pty.kill().expect("kill cat");
    }

    #[test]
    fn write_echoes_back_through_cat() {
        let mut pty = Pty::spawn("/bin/cat", &[], small(), std::iter::empty::<(&str, &str)>())
            .expect("spawn cat");
        let rx = reader_channel(pty.try_clone_reader().expect("clone reader"));
        let mut writer = pty.take_writer().expect("take writer");
        writer.write_all(b"ping\n").expect("write");
        writer.flush().expect("flush");
        let out = read_until(&rx, "ping", Duration::from_secs(5));
        assert!(
            out.contains("ping"),
            "expected 'ping' echoed back, got {out:?}"
        );
        pty.kill().expect("kill cat");
    }

    #[test]
    fn kill_then_try_wait_reaps() {
        let mut pty = Pty::spawn("/bin/cat", &[], small(), std::iter::empty::<(&str, &str)>())
            .expect("spawn cat");
        assert!(pty.process_id().is_some(), "child should report a pid");
        // Running child: try_wait reports not-yet-exited.
        assert!(matches!(pty.try_wait(), Ok(None)));
        pty.kill().expect("kill");
        // Poll until reaped (no zombie left behind).
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut status = None;
        while Instant::now() < deadline {
            match pty.try_wait().expect("try_wait") {
                Some(s) => {
                    status = Some(s);
                    break;
                }
                None => std::thread::yield_now(),
            }
        }
        assert!(status.is_some(), "killed child should have been reaped");
    }

    #[test]
    fn master_fd_is_exposed() {
        let mut pty = Pty::spawn("/bin/cat", &[], small(), std::iter::empty::<(&str, &str)>())
            .expect("spawn cat");
        let fd = pty
            .master_fd()
            .expect("unix master fd should be exposed for T-1.9");
        assert!(
            fd >= 0,
            "a live master fd is a valid (non-negative) descriptor"
        );
        pty.kill().expect("kill cat");
    }

    #[test]
    fn foreground_pgid_and_signal_interrupt_sleep() {
        use std::os::fd::BorrowedFd;
        // `sleep 10` becomes the pty's foreground (session-leader) process group.
        let mut pty = Pty::spawn(
            "/bin/sleep",
            &["10"],
            small(),
            std::iter::empty::<(&str, &str)>(),
        )
        .expect("spawn sleep");
        let fd = pty.master_fd().expect("master fd for T-1.9");

        // foreground_pgid() resolves the running child's group.
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut pgid = None;
        while Instant::now() < deadline {
            let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
            match foreground_pgid(borrowed) {
                Some(p) if p > 0 => {
                    pgid = Some(p);
                    break;
                }
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        assert!(
            pgid.is_some(),
            "foreground_pgid should resolve the running `sleep` group"
        );

        // SIGINT to the foreground group must interrupt `sleep` so it exits well
        // before its 10s elapse - observed directly via try_wait reaping.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        signal_foreground(borrowed, Signal::Interrupt).expect("signal_foreground(SIGINT)");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exited = false;
        while Instant::now() < deadline {
            match pty.try_wait().expect("try_wait") {
                Some(_status) => {
                    exited = true;
                    break;
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        assert!(
            exited,
            "SIGINT to the foreground group should make `sleep 10` exit early"
        );
    }

    #[test]
    fn drop_terminates_child() {
        // `cat` with no input blocks forever; a cloned reader over the master only
        // sees EOF once every slave closes, i.e. once the child dies. So if simply
        // dropping the `Pty` makes the reader hit EOF, `Drop` did kill+reap the
        // child. Without the `Drop` impl this would hang past the timeout.
        let pty = Pty::spawn("/bin/cat", &[], small(), std::iter::empty::<(&str, &str)>())
            .expect("spawn cat");
        let rx = reader_channel(pty.try_clone_reader().expect("clone reader"));
        drop(pty);

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut disconnected = false;
        while Instant::now() < deadline {
            let now = Instant::now();
            match rx.recv_timeout(deadline - now) {
                Ok(_) => continue, // ignore any stray bytes; wait for EOF
                Err(RecvTimeoutError::Disconnected) => {
                    disconnected = true;
                    break;
                }
                Err(RecvTimeoutError::Timeout) => break,
            }
        }
        assert!(
            disconnected,
            "dropping the Pty should kill the child so the reader hits EOF"
        );
    }
}
