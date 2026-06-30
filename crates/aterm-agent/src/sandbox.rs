//! `Sandbox`: the mandatory OS-level boundary beneath the risk gate (T-5.7).
//!
//! The gate ([`crate::risk`]) is a best-effort *classifier*, not a security
//! boundary - it can be fooled by an un-enumerated interpreter or a novel shell
//! construct (ADR-0006; `06-agent-architecture.md` (d).3). Because the AUTO-SAFE
//! default enlarges the trust surface, every agent-run command is *also* confined
//! by an OS boundary. On macOS that boundary is **Seatbelt via `sandbox-exec`**
//! with a generated `.sb` profile ([`SeatbeltSandbox`]); on every platform a
//! confined command additionally runs under `setrlimit` resource caps and a
//! process-group **timeout-kill** ([`SandboxRunner`]).
//!
//! The profile clamps exactly the three axes the threat model names, on top of an
//! `(allow default)` base (so arbitrary commands stay runnable):
//!   1. **writes** - denied everywhere, re-allowed only for the project/cwd (+ a
//!      few harmless `/dev` write-sinks), with the secret paths carved back out so
//!      a credential file living inside the tree still cannot be clobbered;
//!   2. **secret reads AND writes** - the credential paths from the *single*
//!      [`Secrets`] source are denied (so the gate's deny-set, the sanitizer's
//!      redaction set, AND the OS boundary all derive from one list and cannot
//!      drift - the single-source invariant, extended to the kernel for both
//!      directions);
//!   3. **network egress** - outbound denied by default (only local-IPC unix
//!      sockets kept; TCP loopback is NOT re-allowed), punchable by an explicit
//!      IP allowlist.
//!
//! Seatbelt is last-match-wins with `(allow|deny) default` acting as the *fallback*
//! when no specific rule matches - so the deny clauses here hold regardless of
//! their position relative to `(allow default)`. `sandbox-exec` is deprecated but
//! is the only documented way to apply a Seatbelt profile to an arbitrary process
//! and is what Anthropic's own sandbox-runtime uses (ADR-0006); the [`Sandbox`]
//! trait keeps a future native-API/VM backend swappable.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::secrets::Secrets;

/// How permissive the sandbox should be for one command. Default denies all
/// network egress and grants no extra writable paths (most restrictive); the
/// runner always adds the command's own cwd as writable on top of this.
#[derive(Debug, Clone, Default)]
pub struct SandboxPolicy {
    /// Paths the command may write to, *in addition* to its cwd. Canonicalized
    /// (symlinks resolved) into `(subpath ...)` clauses; everything else is
    /// read-only / denied. Empty by default - the strictest write posture.
    pub writable_paths: Vec<PathBuf>,
    /// Whether *all* outbound network is permitted. `false` (default) denies
    /// egress (keeping only local-IPC unix sockets; TCP loopback is not re-allowed)
    /// and consults [`SandboxPolicy::network_allowlist`]; `true` drops the egress
    /// clamp entirely.
    pub allow_network: bool,
    /// Host specs (`"1.2.3.4:443"`, `"localhost:*"`) allowed to receive outbound
    /// connections even when [`SandboxPolicy::allow_network`] is `false`. Best-effort:
    /// Seatbelt filters on the socket syscall and sees IPs, not hostnames, so an
    /// entry that is a hostname will not resolve at the kernel layer - the honest
    /// v1 egress story is deny-all + IP allowlist (owner open-question #2).
    pub network_allowlist: Vec<String>,
}

/// A command ready to be confined: program + args + cwd. The args are passed to
/// the program directly (no shell), so no shell metacharacter is ever interpreted
/// by the sandbox layer.
#[derive(Debug, Clone)]
pub struct ConfinedCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
}

/// Per-command OS resource caps, installed via `setrlimit` in the child *before*
/// exec (so they survive `sandbox-exec`'s in-place exec into the real command).
/// `None` leaves a limit untouched. Applied regardless of whether Seatbelt is
/// enforcing, so even a minimal/no-op profile still bounds the blast radius.
#[derive(Debug, Clone, Copy)]
pub struct ResourceLimits {
    /// `RLIMIT_CPU` - CPU seconds; the kernel sends `SIGXCPU` then `SIGKILL`.
    pub cpu_seconds: Option<u64>,
    /// `RLIMIT_AS` - address-space bytes. Enforced on Linux; **advisory on macOS**
    /// (the kernel does not hard-cap address space), so it is set best-effort.
    pub address_space_bytes: Option<u64>,
    /// `RLIMIT_NOFILE` - open file descriptors.
    pub open_files: Option<u64>,
}

impl Default for ResourceLimits {
    /// Conservative caps for an agent-run command: 30 CPU-seconds, 2 GiB address
    /// space, 256 open files. The wall-clock timeout (see [`SandboxRunner`]) is a
    /// separate, complementary bound (CPU time != wall time for an I/O-blocked
    /// runaway).
    fn default() -> Self {
        Self {
            cpu_seconds: Some(30),
            address_space_bytes: Some(2 * 1024 * 1024 * 1024),
            open_files: Some(256),
        }
    }
}

impl ResourceLimits {
    /// No caps at all (every field `None`). For tests / callers that want only the
    /// timeout-kill and sandbox profile.
    pub fn none() -> Self {
        Self {
            cpu_seconds: None,
            address_space_bytes: None,
            open_files: None,
        }
    }

    /// Install these limits on the *current* process via `setrlimit`. Intended to
    /// run inside a `Command::pre_exec` closure (the forked child, before exec):
    /// only single `setrlimit` syscalls, no allocation in the error path. The
    /// address-space cap is best-effort (advisory on macOS), so its failure is
    /// swallowed; a failure to set the CPU or fd cap aborts the spawn.
    #[cfg(unix)]
    fn install(&self) -> std::io::Result<()> {
        use nix::sys::resource::{setrlimit, Resource};
        let to_io = |e: nix::errno::Errno| std::io::Error::from_raw_os_error(e as i32);
        if let Some(cpu) = self.cpu_seconds {
            setrlimit(Resource::RLIMIT_CPU, cpu, cpu).map_err(to_io)?;
        }
        if let Some(nofile) = self.open_files {
            setrlimit(Resource::RLIMIT_NOFILE, nofile, nofile).map_err(to_io)?;
        }
        if let Some(addr) = self.address_space_bytes {
            // Advisory on macOS; do not fail the spawn if the kernel rejects it.
            let _ = setrlimit(Resource::RLIMIT_AS, addr, addr);
        }
        Ok(())
    }
}

/// The captured result of a confined run.
#[derive(Debug, Clone)]
pub struct ConfinedOutput {
    /// The process exit code, or `None` if it was killed by a signal (incl. the
    /// timeout-kill or a `setrlimit` `SIGXCPU`).
    pub exit_code: Option<i32>,
    /// Raw stdout bytes (the caller runs them through the [`crate::sanitizer`]).
    pub stdout: Vec<u8>,
    /// Raw stderr bytes.
    pub stderr: Vec<u8>,
    /// `true` iff the wall-clock timeout fired and the process group was killed.
    pub timed_out: bool,
}

impl ConfinedOutput {
    /// Exited cleanly with code 0 and was not timed out.
    pub fn success(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out
    }
}

/// Errors from sandbox setup / confined execution.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// The runner is not available on this platform (non-Unix stub).
    #[error("sandbox runner not supported on this platform: {0}")]
    Unsupported(&'static str),
    /// The policy / command could not be turned into a runnable argv.
    #[error("invalid sandbox policy: {0}")]
    InvalidPolicy(String),
    /// Spawning the confined process failed (e.g. `sandbox-exec` missing).
    #[error("confined spawn failed: {0}")]
    Spawn(#[from] std::io::Error),
}

/// The confinement seam. Implementations rewrite a [`ConfinedCommand`] into the
/// argv that actually runs (e.g. `sandbox-exec -p <profile> <program> <args...>`).
/// The trait abstracts the mechanism so a no-op test backend, the real Seatbelt
/// backend, or a future native-API/VM backend are interchangeable.
pub trait Sandbox: Send + Sync {
    /// Human-readable backend name (for logging / UI).
    fn name(&self) -> &'static str;

    /// Whether this backend actually confines anything.
    fn is_enforcing(&self) -> bool;

    /// Produce the final argv to spawn for `cmd` under `policy`. The first element
    /// is the program to exec.
    fn wrap(
        &self,
        cmd: &ConfinedCommand,
        policy: &SandboxPolicy,
    ) -> Result<Vec<String>, SandboxError>;
}

/// Standard `/dev` write-sinks every process expects to be able to write even
/// under the strictest write posture; allowing them does not let the command
/// persist anything outside the project (they are not the filesystem). `/dev/tty`
/// is deliberately NOT here - writing the controlling terminal is a TTY-injection
/// vector, and the runner gives the command piped stdout/stderr + `/dev/null` stdin,
/// so it never needs the tty.
const DEV_WRITE_SINKS: &[&str] = &["/dev/null", "/dev/stdout", "/dev/stderr"];

/// macOS Seatbelt sandbox via `sandbox-exec`. Generates a `.sb` profile (see the
/// module docs) and wraps the command as `sandbox-exec -p <profile> <argv>`.
#[derive(Debug, Clone)]
pub struct SeatbeltSandbox {
    /// Credential path patterns whose *reads AND writes* the profile denies.
    /// Sourced from the single [`Secrets`] deny-set, so this OS boundary cannot
    /// drift from the gate's classification or the sanitizer's redaction (the
    /// single-source invariant, extended to the kernel for both directions: a
    /// secret can be neither exfiltrated nor clobbered, even one living inside the
    /// otherwise-writable project tree). These are substring patterns, rendered as
    /// case-insensitive `(regex ...)` predicates.
    deny_patterns: Vec<String>,
}

impl Default for SeatbeltSandbox {
    /// The safe default: deny reads of the full default credential deny-set.
    fn default() -> Self {
        Self::new()
    }
}

impl SeatbeltSandbox {
    /// A Seatbelt sandbox seeded with the default [`Secrets`] credential deny-set.
    pub fn new() -> Self {
        Self::from_secrets(&Secrets::new())
    }

    /// A Seatbelt sandbox whose read-deny set is taken from `secrets` - the SAME
    /// instance the gate and sanitizer borrow. A path added to that `Secrets` (e.g.
    /// aterm's own resolved config path) is therefore denied at the kernel too.
    pub fn from_secrets(secrets: &Secrets) -> Self {
        Self {
            deny_patterns: secrets.sensitive_paths().to_vec(),
        }
    }

    /// A Seatbelt sandbox that denies no secret paths (still clamps writes to the
    /// cwd + egress). For tests and the rare caller that wants only the
    /// write-confine / network boundary.
    pub fn without_secret_denies() -> Self {
        Self {
            deny_patterns: Vec::new(),
        }
    }

    /// Generate the `.sb` profile text for one command. Pure and portable (no
    /// syscalls, no platform gating) so the confinement *intent* is unit-testable
    /// on every platform; only running it needs macOS. `cwd` is auto-added as a
    /// writable subpath alongside `policy.writable_paths`.
    pub fn profile(&self, policy: &SandboxPolicy, cwd: Option<&Path>) -> String {
        let mut p = String::with_capacity(512 + self.deny_patterns.len() * 48);
        p.push_str("(version 1)\n");
        p.push_str("(allow default)\n\n");

        // --- 1. writes: deny everywhere, re-allow only the project/cwd, the
        //         caller's extra writable paths, and the harmless /dev sinks, then
        //         carve the secret paths back OUT (last-match-wins) so a credential
        //         file living inside the writable tree still cannot be clobbered.
        p.push_str(";; writes: deny everywhere, re-allow only project/cwd + /dev sinks\n");
        p.push_str("(deny file-write*)\n");
        p.push_str("(allow file-write*\n");
        for sink in DEV_WRITE_SINKS {
            p.push_str(&format!("  (literal \"{sink}\")\n"));
        }
        for canon in writable_subpaths(policy, cwd) {
            p.push_str(&format!("  (subpath \"{}\")\n", escape_sbpl(&canon)));
        }
        p.push_str(")\n");
        if !self.deny_patterns.is_empty() {
            p.push_str(";; ...but never a secret path, even inside the writable tree\n");
            p.push_str("(deny file-write*\n");
            for pat in &self.deny_patterns {
                p.push_str(&format!("  (regex #\"{}\")\n", secret_path_regex(pat)));
            }
            p.push_str(")\n");
        }
        p.push('\n');

        // --- 2. secret reads: deny the single Secrets deny-set (case-insensitive).
        if !self.deny_patterns.is_empty() {
            p.push_str(";; secret reads: deny (shared with the gate's Secrets deny-set)\n");
            p.push_str("(deny file-read*\n");
            for pat in &self.deny_patterns {
                p.push_str(&format!("  (regex #\"{}\")\n", secret_path_regex(pat)));
            }
            p.push_str(")\n\n");
        }

        // --- 3. network egress: deny outbound by default, keeping ONLY local IPC
        //         (unix sockets - mDNSResponder, launchd, etc.). TCP loopback is NOT
        //         re-allowed (it is an exfil-to-local-proxy channel); a caller that
        //         needs it adds an explicit `127.0.0.1:<port>` to the allowlist.
        if !policy.allow_network {
            p.push_str(";; egress: deny outbound, keep only local IPC (unix sockets)\n");
            p.push_str("(deny network-outbound)\n");
            p.push_str("(allow network-outbound (remote unix))\n");
            for host in &policy.network_allowlist {
                p.push_str(&format!(
                    "(allow network-outbound (remote ip \"{}\"))\n",
                    escape_sbpl(host)
                ));
            }
        }
        p
    }
}

impl Sandbox for SeatbeltSandbox {
    fn name(&self) -> &'static str {
        "seatbelt"
    }

    fn is_enforcing(&self) -> bool {
        true
    }

    fn wrap(
        &self,
        cmd: &ConfinedCommand,
        policy: &SandboxPolicy,
    ) -> Result<Vec<String>, SandboxError> {
        if cmd.program.trim().is_empty() {
            return Err(SandboxError::InvalidPolicy("empty program".into()));
        }
        let profile = self.profile(policy, cmd.cwd.as_deref());
        let mut argv = Vec::with_capacity(cmd.args.len() + 4);
        argv.push("sandbox-exec".to_string());
        argv.push("-p".to_string());
        argv.push(profile);
        argv.push(cmd.program.clone());
        argv.extend(cmd.args.iter().cloned());
        Ok(argv)
    }
}

/// No-op confinement: runs the command unconfined (still subject to the
/// [`SandboxRunner`]'s `setrlimit` + timeout-kill, which apply regardless of the
/// backend). The test/Linux backend - and the honest fallback when Seatbelt is
/// unavailable.
#[derive(Debug, Clone, Default)]
pub struct NoSandbox;

impl Sandbox for NoSandbox {
    fn name(&self) -> &'static str {
        "none"
    }

    fn is_enforcing(&self) -> bool {
        false
    }

    fn wrap(
        &self,
        cmd: &ConfinedCommand,
        _policy: &SandboxPolicy,
    ) -> Result<Vec<String>, SandboxError> {
        if cmd.program.trim().is_empty() {
            return Err(SandboxError::InvalidPolicy("empty program".into()));
        }
        let mut argv = Vec::with_capacity(cmd.args.len() + 1);
        argv.push(cmd.program.clone());
        argv.extend(cmd.args.iter().cloned());
        Ok(argv)
    }
}

/// Runs a [`ConfinedCommand`] through a [`Sandbox`] backend, then spawns it with
/// the resource caps + process-group timeout-kill that apply on every platform.
/// This is the primitive the execution sinks (T-5.9) wrap around the agent's
/// `run_command` tool; it never goes through a shell (argv is exec'd directly).
#[derive(Debug, Clone)]
pub struct SandboxRunner<S: Sandbox> {
    sandbox: S,
    limits: ResourceLimits,
    timeout: Duration,
}

impl<S: Sandbox> SandboxRunner<S> {
    /// A runner with the default [`ResourceLimits`] and a 30s wall-clock timeout.
    pub fn new(sandbox: S) -> Self {
        Self {
            sandbox,
            limits: ResourceLimits::default(),
            timeout: Duration::from_secs(30),
        }
    }

    /// Override the resource caps.
    #[must_use]
    pub fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Override the wall-clock timeout after which the process group is killed.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The backend in use (for logging / UI).
    pub fn sandbox(&self) -> &S {
        &self.sandbox
    }

    /// Confine and run `cmd` under `policy`, capturing stdout/stderr and reaping
    /// the whole process group on timeout. Blocking - intended to run on a worker
    /// thread (the sinks own that).
    pub fn run(
        &self,
        cmd: &ConfinedCommand,
        policy: &SandboxPolicy,
    ) -> Result<ConfinedOutput, SandboxError> {
        let argv = self.sandbox.wrap(cmd, policy)?;
        if argv.is_empty() {
            return Err(SandboxError::InvalidPolicy("empty argv".into()));
        }
        run_confined(&argv, cmd.cwd.as_deref(), self.limits, self.timeout)
    }
}

/// Spawn `argv` (no shell) in its own process group with the resource caps
/// installed in the child, draining stdout/stderr on reader threads, and kill the
/// whole group if it outlives `timeout`. Unix implementation.
#[cfg(unix)]
fn run_confined(
    argv: &[String],
    cwd: Option<&Path>,
    limits: ResourceLimits,
    timeout: Duration,
) -> Result<ConfinedOutput, SandboxError> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    // Coarse poll cadence for the single-reaper wait loop. The runner is off the
    // render/UI thread (the sinks own a worker thread), so a few ms of latency on
    // detecting completion is irrelevant; correctness (no PID-reuse race) wins.
    const POLL_INTERVAL: Duration = Duration::from_millis(10);
    // After the command exits we give the pipe readers this long to drain on their
    // own before returning with whatever was captured. The common case finishes
    // instantly; the grace only elapses when a descendant escaped the process group
    // (setsid/double-fork) and still holds a write end (see the drain note below).
    const DRAIN_GRACE: Duration = Duration::from_millis(500);

    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    if let Some(c) = cwd {
        command.current_dir(c);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Own process group so the timeout-kill reaps the command AND any subprocesses
    // it spawned (they inherit the pgid), not just the direct child. Setting
    // pre_exec below also forces std onto the fork/exec path, where process_group
    // is honored reliably.
    command.process_group(0);
    // SAFETY: the closure runs in the forked child before exec and only issues
    // `setrlimit` syscalls (async-signal-safe, no allocation), returning the errno
    // verbatim. It captures `limits` by Copy.
    unsafe {
        command.pre_exec(move || limits.install());
    }

    let mut child = command.spawn()?;
    let pid = child.id();

    // Drain both pipes concurrently (so a chatty command cannot deadlock by filling
    // a pipe buffer while we wait) into SHARED buffers, so the captured bytes are
    // readable even if a reader thread is still blocked on the pipe at the end.
    let (out_thread, out_buf) = spawn_reader(child.stdout.take());
    let (err_thread, err_buf) = spawn_reader(child.stderr.take());

    // Single-reaper poll loop: we are the ONLY thing that reaps `child`. We only
    // `killpg` on the timeout/error branch, where `try_wait` proved the leader is
    // still alive - so the SIGKILL goes to a live group and cannot race a reap that
    // freed the pid (the PID/PGID-reuse window a post-reap kill would open). On a
    // normal exit we do NOT signal the group: an agent command's own backgrounded
    // jobs are its business, and AC4's "no orphans" is the *timeout* contract.
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let exit_code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_process_group(pid);
                    timed_out = true;
                    // Leader was still alive and is now signalled; reap it (the
                    // SIGKILL to the group means this returns promptly).
                    break child.wait().ok().and_then(|s| s.code());
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(_) => break None,
        }
    };

    // Bounded drain. Normally the pipes EOF the instant every writer exits and the
    // readers finish at once. But a descendant that escaped the process group
    // (setsid/double-fork) - or a backgrounded job after a normal exit - can keep a
    // write end open forever, so a plain `join()` would hang the runner permanently.
    // Instead we wait a bounded grace for the readers to finish, then take whatever
    // was captured and leave any still-blocked reader detached (it ends when its fd
    // finally closes). All output written before the command returned is already in
    // the shared buffer, so the snapshot is complete up to that point.
    let drain_deadline = Instant::now() + DRAIN_GRACE;
    while (!out_thread.is_finished() || !err_thread.is_finished())
        && Instant::now() < drain_deadline
    {
        std::thread::sleep(POLL_INTERVAL);
    }
    let stdout = out_buf.lock().map(|b| b.clone()).unwrap_or_default();
    let stderr = err_buf.lock().map(|b| b.clone()).unwrap_or_default();
    if out_thread.is_finished() {
        let _ = out_thread.join();
    }
    if err_thread.is_finished() {
        let _ = err_thread.join();
    }

    Ok(ConfinedOutput {
        exit_code,
        stdout,
        stderr,
        timed_out,
    })
}

/// Spawn a thread that drains `pipe` to EOF into a shared buffer, returning the
/// handle (to poll `is_finished` / join) and the buffer (readable at any time, so a
/// still-blocked reader does not strand the bytes captured so far).
#[cfg(unix)]
fn spawn_reader<R: std::io::Read + Send + 'static>(
    pipe: Option<R>,
) -> (
    std::thread::JoinHandle<()>,
    std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
) {
    use std::sync::{Arc, Mutex};
    let buf = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&buf);
    let handle = std::thread::spawn(move || {
        if let Some(mut pipe) = pipe {
            let mut chunk = [0u8; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut g) = sink.lock() {
                            g.extend_from_slice(&chunk[..n]);
                        }
                    }
                }
            }
        }
    });
    (handle, buf)
}

/// `SIGKILL` the process group led by `pid`. Guards against signalling pgid <= 1
/// (which would target init or, via `killpg(0)`, our OWN group). A dead group
/// (`ESRCH`) is not an error.
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;
    let Ok(raw) = i32::try_from(pid) else {
        return;
    };
    if raw <= 1 {
        return;
    }
    let _ = killpg(Pid::from_raw(raw), Signal::SIGKILL);
}

/// Non-Unix stub: the confined runner needs `setrlimit`/`killpg`/process groups,
/// which are POSIX. v1 is macOS-only; this keeps the crate compiling elsewhere.
#[cfg(not(unix))]
fn run_confined(
    _argv: &[String],
    _cwd: Option<&Path>,
    _limits: ResourceLimits,
    _timeout: Duration,
) -> Result<ConfinedOutput, SandboxError> {
    Err(SandboxError::Unsupported(
        "the confined runner requires a Unix platform (setrlimit/killpg)",
    ))
}

/// Render one credential substring pattern as a Seatbelt regex that matches any
/// path *containing* it, case-insensitively (used for both the read-deny and the
/// write-deny clauses): ASCII letters expand to `[Aa]`-style classes (Seatbelt's
/// regex engine is case-sensitive and ignores `(?i)`, verified on macOS 15), regex
/// metacharacters are escaped, everything else is literal. The match is a
/// partial/search match, so no anchoring or `.*` is needed.
fn secret_path_regex(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() * 4);
    for ch in pattern.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' => {
                out.push('[');
                out.push(ch.to_ascii_uppercase());
                out.push(ch.to_ascii_lowercase());
                out.push(']');
            }
            '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            '"' => {
                // Protect the enclosing #"..." literal (no default pattern has one).
                out.push('\\');
                out.push('"');
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Escape a string for a Seatbelt `"..."` literal (backslash + quote).
fn escape_sbpl(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The set of writable subpath strings for a profile: the command's cwd (if any)
/// plus the policy's extra paths, each canonicalized (symlinks resolved) so the
/// `(subpath ...)` predicate matches the canonical path the kernel sees. A path
/// that cannot be canonicalized (does not exist yet) falls back to its absolute
/// lexical form; a still-relative path is dropped (a relative subpath silently
/// never matches, so emitting it would be a confusing no-op).
fn writable_subpaths(policy: &SandboxPolicy, cwd: Option<&Path>) -> Vec<String> {
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(c) = cwd {
        paths.push(c.to_path_buf());
    }
    paths.extend(policy.writable_paths.iter().cloned());

    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let canon = std::fs::canonicalize(&p)
            .ok()
            .or_else(|| std::path::absolute(&p).ok())
            .unwrap_or(p);
        if canon.is_absolute() {
            if let Some(s) = canon.to_str() {
                let s = s.to_string();
                if !out.contains(&s) {
                    out.push(s);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd() -> ConfinedCommand {
        ConfinedCommand {
            program: "ls".into(),
            args: vec!["-la".into()],
            cwd: None,
        }
    }

    // --- pure, portable: profile generation + helpers (run on Linux + macOS) ---

    #[test]
    fn no_sandbox_passes_command_through() {
        let argv = NoSandbox.wrap(&cmd(), &SandboxPolicy::default()).unwrap();
        assert_eq!(argv, vec!["ls", "-la"]);
        assert!(!NoSandbox.is_enforcing());
        assert_eq!(NoSandbox.name(), "none");
    }

    #[test]
    fn empty_program_is_rejected() {
        let empty = ConfinedCommand {
            program: "   ".into(),
            args: vec![],
            cwd: None,
        };
        assert!(matches!(
            NoSandbox.wrap(&empty, &SandboxPolicy::default()),
            Err(SandboxError::InvalidPolicy(_))
        ));
        assert!(matches!(
            SeatbeltSandbox::new().wrap(&empty, &SandboxPolicy::default()),
            Err(SandboxError::InvalidPolicy(_))
        ));
    }

    #[test]
    fn seatbelt_wrap_builds_sandbox_exec_argv() {
        let sb = SeatbeltSandbox::new();
        assert!(sb.is_enforcing());
        assert_eq!(sb.name(), "seatbelt");
        let argv = sb.wrap(&cmd(), &SandboxPolicy::default()).unwrap();
        assert_eq!(argv[0], "sandbox-exec");
        assert_eq!(argv[1], "-p");
        assert!(argv[2].starts_with("(version 1)"));
        // program + args follow the profile.
        assert_eq!(&argv[3..], &["ls".to_string(), "-la".to_string()]);
    }

    #[test]
    fn profile_denies_writes_then_reallows_cwd_only() {
        let sb = SeatbeltSandbox::new();
        let cwd = std::env::temp_dir();
        let prof = sb.profile(&SandboxPolicy::default(), Some(&cwd));
        assert!(prof.contains("(allow default)"));
        assert!(prof.contains("(deny file-write*)"));
        // cwd is re-allowed (canonicalized); /dev sinks present.
        let canon = std::fs::canonicalize(&cwd).unwrap();
        assert!(prof.contains(&format!("(subpath \"{}\")", canon.display())));
        assert!(prof.contains("(literal \"/dev/null\")"));
        // No blanket write allow for /tmp or $HOME.
        assert!(!prof.contains("(allow file-write*)\n)"));
    }

    #[test]
    fn profile_extra_writable_paths_are_canonicalized() {
        let dir = std::env::temp_dir();
        let policy = SandboxPolicy {
            writable_paths: vec![dir.clone()],
            ..Default::default()
        };
        let prof = SeatbeltSandbox::new().profile(&policy, None);
        let canon = std::fs::canonicalize(&dir).unwrap();
        assert!(prof.contains(&format!("(subpath \"{}\")", canon.display())));
    }

    #[test]
    fn profile_denies_secret_reads_from_the_single_secrets_source() {
        let sb = SeatbeltSandbox::from_secrets(&Secrets::new());
        let prof = sb.profile(&SandboxPolicy::default(), None);
        assert!(prof.contains("(deny file-read*"));
        // .ssh/ -> case-insensitive char classes, dot escaped.
        assert!(prof.contains(r#"(regex #"\.[Ss][Ss][Hh]/")"#));
        // id_rsa -> char classes, underscore literal.
        assert!(prof.contains(r#"(regex #"[Ii][Dd]_[Rr][Ss][Aa]")"#));
    }

    #[test]
    fn sandbox_read_deny_tracks_a_runtime_added_secret() {
        // The single-source invariant extended to the kernel: a path added to the
        // Secrets instance the sandbox is built from is denied in the profile too.
        let mut secrets = Secrets::new();
        secrets.add_sensitive_path("megacorp-vault");
        let prof = SeatbeltSandbox::from_secrets(&secrets).profile(&SandboxPolicy::default(), None);
        assert!(prof.contains("[Mm][Ee][Gg][Aa][Cc][Oo][Rr][Pp]-[Vv][Aa][Uu][Ll][Tt]"));
    }

    #[test]
    fn without_secret_denies_omits_the_read_and_write_deny_clauses() {
        let prof =
            SeatbeltSandbox::without_secret_denies().profile(&SandboxPolicy::default(), None);
        assert!(!prof.contains("(deny file-read*"));
        // The only file-write* deny is the blanket one; no secret-carve-out block.
        assert_eq!(prof.matches("(deny file-write*").count(), 1);
    }

    #[test]
    fn profile_denies_writing_secret_paths_inside_the_writable_tree() {
        // The read-deny set is mirrored as a write-deny so a credential file living
        // inside cwd cannot be clobbered (last-match-wins beats the cwd allow).
        let cwd = std::env::temp_dir();
        let prof = SeatbeltSandbox::new().profile(&SandboxPolicy::default(), Some(&cwd));
        // Two file-write* denies: the blanket one, then the secret carve-out.
        assert_eq!(prof.matches("(deny file-write*").count(), 2);
        // The secret write-deny carries the credential regexes.
        let write_deny = prof.split("(deny file-write*").nth(2).unwrap();
        assert!(write_deny.contains(r#"(regex #"\.[Ss][Ss][Hh]/")"#));
    }

    #[test]
    fn profile_denies_egress_by_default_keeping_only_local_ipc() {
        let prof = SeatbeltSandbox::new().profile(&SandboxPolicy::default(), None);
        assert!(prof.contains("(deny network-outbound)"));
        // Local IPC (unix sockets) is kept, but TCP loopback is NOT re-allowed.
        assert!(prof.contains("(allow network-outbound (remote unix))"));
        assert!(!prof.contains("localhost"));
    }

    #[test]
    fn profile_allows_full_egress_when_requested() {
        let policy = SandboxPolicy {
            allow_network: true,
            ..Default::default()
        };
        let prof = SeatbeltSandbox::new().profile(&policy, None);
        assert!(!prof.contains("(deny network-outbound)"));
    }

    #[test]
    fn profile_network_allowlist_punches_holes() {
        let policy = SandboxPolicy {
            network_allowlist: vec!["93.184.216.34:443".into()],
            ..Default::default()
        };
        let prof = SeatbeltSandbox::new().profile(&policy, None);
        assert!(prof.contains("(deny network-outbound)"));
        assert!(prof.contains("(allow network-outbound (remote ip \"93.184.216.34:443\"))"));
    }

    #[test]
    fn secret_path_regex_is_case_insensitive_and_escaped() {
        assert_eq!(secret_path_regex(".ssh/"), r"\.[Ss][Ss][Hh]/");
        assert_eq!(secret_path_regex(".env"), r"\.[Ee][Nn][Vv]");
        assert_eq!(secret_path_regex("169.254.169.254"), r"169\.254\.169\.254");
        assert_eq!(secret_path_regex("id_rsa"), "[Ii][Dd]_[Rr][Ss][Aa]");
    }

    #[test]
    fn escape_sbpl_escapes_quote_and_backslash() {
        assert_eq!(escape_sbpl(r#"a"b\c"#), r#"a\"b\\c"#);
    }

    #[test]
    fn resource_limits_defaults_are_sane() {
        let l = ResourceLimits::default();
        assert_eq!(l.cpu_seconds, Some(30));
        assert_eq!(l.open_files, Some(256));
        assert!(l.address_space_bytes.unwrap() >= 1 << 30);
        assert!(ResourceLimits::none().cpu_seconds.is_none());
    }

    #[test]
    fn confined_output_success_predicate() {
        let ok = ConfinedOutput {
            exit_code: Some(0),
            stdout: vec![],
            stderr: vec![],
            timed_out: false,
        };
        assert!(ok.success());
        let killed = ConfinedOutput {
            timed_out: true,
            ..ok.clone()
        };
        assert!(!killed.success());
    }

    // --- the runner mechanism via NoSandbox (Unix: Linux + macOS in CI) ---

    #[cfg(unix)]
    #[test]
    fn runner_captures_stdout_and_exit_status() {
        let runner = SandboxRunner::new(NoSandbox);
        let out = runner
            .run(
                &ConfinedCommand {
                    program: "/bin/echo".into(),
                    args: vec!["hello".into()],
                    cwd: None,
                },
                &SandboxPolicy::default(),
            )
            .unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(out.stdout, b"hello\n");
        assert!(out.success());
        assert!(!out.timed_out);
    }

    #[cfg(unix)]
    #[test]
    fn runner_applies_setrlimit_in_the_child() {
        // AC6: resource limits apply even with a no-op profile. `ulimit -n` in the
        // spawned shell must reflect the RLIMIT_NOFILE we installed via pre_exec.
        let runner = SandboxRunner::new(NoSandbox).with_limits(ResourceLimits {
            open_files: Some(48),
            ..ResourceLimits::none()
        });
        let out = runner
            .run(
                &ConfinedCommand {
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), "ulimit -n".into()],
                    cwd: None,
                },
                &SandboxPolicy::default(),
            )
            .unwrap();
        let printed = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            printed.trim(),
            "48",
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn runner_kills_a_runaway_group_at_the_timeout_no_orphans() {
        // AC4: a runaway is killed at the timeout, and the WHOLE group is reaped -
        // a grandchild spawned by the command must not survive. The command prints
        // a backgrounded grandchild's pid, then both sleep well past the timeout.
        let runner = SandboxRunner::new(NoSandbox).with_timeout(Duration::from_millis(600));
        let start = std::time::Instant::now();
        let out = runner
            .run(
                &ConfinedCommand {
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), "sleep 30 & echo $!; sleep 30".into()],
                    cwd: None,
                },
                &SandboxPolicy::default(),
            )
            .unwrap();
        assert!(out.timed_out, "command should have timed out");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timeout-kill should return promptly, took {:?}",
            start.elapsed()
        );
        // The grandchild pid was printed before the kill; give the group-kill a
        // beat, then assert the grandchild is gone (kill -0 fails -> reaped).
        let pid: i32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let alive = unsafe { nix::libc::kill(pid, 0) } == 0;
        assert!(
            !alive,
            "grandchild pid {pid} survived the group kill (orphan)"
        );
    }

    #[cfg(unix)]
    #[test]
    fn runner_does_not_hang_when_a_backgrounded_job_holds_the_pipe() {
        // The foreground exits cleanly and fast but leaves a backgrounded job
        // holding the stdout pipe write end open. A naive read-to-EOF + join would
        // hang forever; the bounded drain must return promptly with the output that
        // WAS written (the pid line), capped by the drain grace, not the 20s timeout.
        let runner = SandboxRunner::new(NoSandbox).with_timeout(Duration::from_secs(20));
        let start = std::time::Instant::now();
        let out = runner
            .run(
                &ConfinedCommand {
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), "sleep 30 & echo $!; exit 0".into()],
                    cwd: None,
                },
                &SandboxPolicy::default(),
            )
            .unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.timed_out);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "must not hang on the held-open pipe, took {:?}",
            start.elapsed()
        );
        // The bytes written before the pipe-holder kept it open were still captured.
        let pid: i32 = String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .expect("the pid line written before exit was captured despite the held pipe");
        // Clean up the leaked background job (a normal exit does not signal it).
        unsafe { nix::libc::kill(pid, nix::libc::SIGKILL) };
    }

    // --- real Seatbelt confinement (macOS only - the headline ACs) ---

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_confines_writes_to_cwd() {
        // AC1: a write to the cwd succeeds; a write to $HOME / /tmp is denied.
        let tmp = std::env::temp_dir().join(format!("atermr-sbtest-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let runner = SandboxRunner::new(SeatbeltSandbox::new());
        let policy = SandboxPolicy::default();

        let in_cwd = runner
            .run(
                &ConfinedCommand {
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), "echo ok > ./inside.txt".into()],
                    cwd: Some(tmp.clone()),
                },
                &policy,
            )
            .unwrap();
        assert!(in_cwd.success(), "write to cwd should succeed: {in_cwd:?}");
        assert!(tmp.join("inside.txt").exists());

        let target = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("atermr-sbtest-denied.txt");
        let _ = std::fs::remove_file(&target);
        let outside = runner
            .run(
                &ConfinedCommand {
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), format!("echo x > {}", target.display())],
                    cwd: Some(tmp.clone()),
                },
                &policy,
            )
            .unwrap();
        assert!(!outside.success(), "write outside cwd should be denied");
        assert!(
            !target.exists(),
            "denied write must not have created the file"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_denies_reading_a_secret_path() {
        // AC2: cat of a path matching the Secrets deny-set is blocked by the profile.
        let tmp = std::env::temp_dir().join(format!("atermr-sbsecret-{}", std::process::id()));
        let ssh = tmp.join(".ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        let key = ssh.join("id_rsa");
        std::fs::write(&key, b"PRIVATE").unwrap();
        let plain = tmp.join("notes.txt");
        std::fs::write(&plain, b"public").unwrap();

        // Allow reads broadly (no extra restriction) - only the secret deny applies.
        let runner = SandboxRunner::new(SeatbeltSandbox::new());
        let policy = SandboxPolicy::default();

        let secret = runner
            .run(
                &ConfinedCommand {
                    program: "/bin/cat".into(),
                    args: vec![key.to_string_lossy().into()],
                    cwd: Some(tmp.clone()),
                },
                &policy,
            )
            .unwrap();
        assert!(!secret.success(), "reading .ssh/id_rsa must be denied");

        let public = runner
            .run(
                &ConfinedCommand {
                    program: "/bin/cat".into(),
                    args: vec![plain.to_string_lossy().into()],
                    cwd: Some(tmp.clone()),
                },
                &policy,
            )
            .unwrap();
        assert!(
            public.success(),
            "reading a non-secret file should succeed: {public:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_denies_writing_a_secret_inside_the_cwd() {
        // The write-deny mirror: a credential file that lives INSIDE the otherwise
        // writable project tree still cannot be clobbered (a normal file there can).
        let tmp = std::env::temp_dir().join(format!("atermr-sbwsecret-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join(".git-credentials"), b"https://user:tok@host").unwrap();
        let runner = SandboxRunner::new(SeatbeltSandbox::new());
        let policy = SandboxPolicy::default();

        let clobber = runner
            .run(
                &ConfinedCommand {
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), "echo pwned > ./.git-credentials".into()],
                    cwd: Some(tmp.clone()),
                },
                &policy,
            )
            .unwrap();
        assert!(
            !clobber.success(),
            "overwriting a secret inside cwd must be denied"
        );
        assert_eq!(
            std::fs::read(tmp.join(".git-credentials")).unwrap(),
            b"https://user:tok@host",
            "the secret file must be unchanged"
        );

        let normal = runner
            .run(
                &ConfinedCommand {
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), "echo ok > ./normal.txt".into()],
                    cwd: Some(tmp.clone()),
                },
                &policy,
            )
            .unwrap();
        assert!(
            normal.success(),
            "a non-secret write inside cwd should succeed: {normal:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_applies_resource_limits_through_sandbox_exec() {
        // AC6 through the REAL wrapped path: the pre_exec RLIMIT_NOFILE must survive
        // sandbox-exec's in-place exec, so `ulimit -n` under SeatbeltSandbox reports it.
        let runner = SandboxRunner::new(SeatbeltSandbox::new()).with_limits(ResourceLimits {
            open_files: Some(40),
            ..ResourceLimits::none()
        });
        let out = runner
            .run(
                &ConfinedCommand {
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), "ulimit -n".into()],
                    cwd: None,
                },
                &SandboxPolicy::default(),
            )
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "40",
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_denies_egress_to_a_reachable_host() {
        // AC3: outbound to a non-allowlisted host is denied. Self-skips when the
        // network is unreachable anyway (offline CI), so a pass always means the
        // sandbox - not the lack of a route - blocked the connection.
        let probe = "import socket,sys\n\
             s=socket.socket(); s.settimeout(4)\n\
             sys.exit(0 if s.connect_ex(('1.1.1.1',443))==0 else 7)";
        let control = SandboxRunner::new(NoSandbox)
            .with_timeout(Duration::from_secs(8))
            .run(
                &ConfinedCommand {
                    program: "/usr/bin/python3".into(),
                    args: vec!["-c".into(), probe.into()],
                    cwd: None,
                },
                &SandboxPolicy::default(),
            );
        let unsandboxed_ok = control.map(|o| o.exit_code == Some(0)).unwrap_or(false);
        if !unsandboxed_ok {
            eprintln!("skipping egress test: no network route unsandboxed");
            return;
        }
        let sandbox =
            SandboxRunner::new(SeatbeltSandbox::new()).with_timeout(Duration::from_secs(8));
        // Precondition: the SAME profile must run python at all - otherwise a denied
        // egress could be masked by a profile-parse / spawn failure (sandbox-exec
        // exit 65), making the assertion below a false pass.
        let canary = sandbox
            .run(
                &ConfinedCommand {
                    program: "/usr/bin/python3".into(),
                    args: vec!["-c".into(), "print('ok')".into()],
                    cwd: None,
                },
                &SandboxPolicy::default(),
            )
            .unwrap();
        assert!(
            canary.success(),
            "profile must run python before the egress assertion is meaningful: {canary:?}"
        );
        let sandboxed = sandbox
            .run(
                &ConfinedCommand {
                    program: "/usr/bin/python3".into(),
                    args: vec!["-c".into(), probe.into()],
                    cwd: None,
                },
                &SandboxPolicy::default(),
            )
            .unwrap();
        // exit 7 = connect refused/blocked (our probe's own code), NOT a spawn error.
        assert_eq!(
            sandboxed.exit_code,
            Some(7),
            "egress to 1.1.1.1:443 must be blocked by the profile (probe exits 7 on block): {sandboxed:?}"
        );
    }
}
