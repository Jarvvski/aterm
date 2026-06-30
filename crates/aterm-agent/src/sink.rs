//! The execution sinks the turn loop (T-5.8) dispatches to (T-5.9): the seam
//! where a gated, typed tool call actually *runs*.
//!
//! Three sinks, all composing the safety spine rather than re-implementing it:
//!
//! - [`CommandSink`] runs `run_command` as a no-shell subprocess: the argv is
//!   exec'd directly (never space-joined into a shell string) and the whole run is
//!   confined by the [`SandboxRunner`] (T-5.7) - Seatbelt profile + setrlimit +
//!   process-group timeout-kill. The sandbox IS the enforcement boundary for a
//!   command (the gate + confirm already happened upstream in the turn loop, which
//!   never trusts the model's self-reported risk).
//! - [`FileSink`] runs the filesystem tools (`read_file`/`edit_file`/`write_file`/
//!   `list_dir`/`glob`/`grep`) IN-PROCESS. These never touch the sandboxed
//!   subprocess, so the sink applies the gate's path checks itself: it refuses to
//!   touch any path in the single [`Secrets`] deny-set (so a `read_file` of
//!   `~/.ssh/id_rsa` is refused even though the turn loop auto-approves reads - the
//!   defense-in-depth re-gate `turn.rs` documents), and it confines all writes to
//!   the workspace root (a write/edit escaping the root via `..` or a symlinked
//!   parent is denied). `edit_file` is an exactly-one-match string-replace with a
//!   staleness check (an edit to a file that changed since the agent last read it
//!   is rejected, so a concurrent change is never silently clobbered); writes are
//!   atomic (temp file + rename).
//! - [`PtyInjectSink`] is the separate, HARDER-gated path that injects an
//!   agent-proposed command into the live interactive shell over the T-1.1 PTY
//!   writer. Because a real shell will interpret the string, any shell-active
//!   command forces confirmation even when it would otherwise rate `Safe` - on top
//!   of the policy's own shell-active refusal (defense in depth). This path is NOT
//!   one of the seven typed tools; the app drives it when input is routed to the
//!   live shell.
//!
//! [`Sinks`] bundles the command + file sinks into the single [`ToolDispatch`] the
//! turn loop holds. Each blocking sink call runs on a `spawn_blocking` worker, so
//! the dispatcher stays non-blocking and the turn loop's read-only fan-out really
//! is concurrent.
//!
//! Sanitization (T-5.6) is deliberately NOT done here: per the [`ToolOutcome`]
//! contract the dispatcher returns the *raw* tool output and the turn loop runs it
//! through the [`crate::sanitizer::OutputSanitizer`] (against the SAME `Secrets`)
//! before it re-enters context or is rendered. The sink's contribution to "no
//! secret leak" (AC5) is structural: it refuses to read a secret *path* at all, so
//! a credential file's contents never even enter the output buffer the sanitizer
//! would later have to scrub.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::policy::{Approval, ApprovalPolicy};
use crate::risk::{RiskAssessment, RiskReason};
use crate::sandbox::{
    ConfinedCommand, ConfinedOutput, Sandbox, SandboxError, SandboxPolicy, SandboxRunner,
    SeatbeltSandbox,
};
use crate::secrets::Secrets;
use crate::tools::{
    EditFile, Glob, Grep, ListDir, ReadFile, RunCommand, ToolDispatch, ToolInput, ToolOutcome,
    WriteFile,
};

/// Upper bound on directory entries visited by an in-process `glob`/`grep` walk,
/// so a tool call rooted at a huge tree cannot run unbounded. A capped walk emits
/// a trailing note rather than silently truncating.
const MAX_WALK_ENTRIES: usize = 20_000;

/// Upper bound on `grep` match lines returned, so a pathological pattern over a
/// large tree cannot return an unbounded result.
const MAX_GREP_HITS: usize = 2_000;

// ===========================================================================
// CommandSink - no-shell, sandbox-confined `run_command`.
// ===========================================================================

/// Runs `run_command` as a confined subprocess with NO shell. Cloneable so the
/// async dispatcher can move it onto a `spawn_blocking` worker.
#[derive(Clone)]
pub struct CommandSink<S: Sandbox> {
    runner: SandboxRunner<S>,
    /// The session cwd used when a `run_command` carries no `cwd` of its own.
    default_cwd: PathBuf,
}

impl<S: Sandbox> CommandSink<S> {
    /// A command sink over `runner`, defaulting commands without an explicit cwd to
    /// `default_cwd`.
    pub fn new(runner: SandboxRunner<S>, default_cwd: PathBuf) -> Self {
        Self {
            runner,
            default_cwd,
        }
    }

    /// The sandbox backend in use (for logging / UI).
    pub fn sandbox(&self) -> &S {
        self.runner.sandbox()
    }

    /// Confine and run `rc` with no shell, capturing stdout/stderr. RAW (the turn
    /// loop sanitizes). Blocking - call on a worker thread.
    pub fn run(&self, rc: &RunCommand) -> ToolOutcome {
        let Some((program, args)) = rc.command.split_first() else {
            return ToolOutcome::error("run_command: empty command (argv must be non-empty)");
        };
        if program.trim().is_empty() {
            return ToolOutcome::error("run_command: empty program");
        }

        let cwd = match &rc.cwd {
            Some(c) => {
                let p = Path::new(c);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    self.default_cwd.join(p)
                }
            }
            None => self.default_cwd.clone(),
        };

        let confined = ConfinedCommand {
            program: program.clone(),
            args: args.to_vec(),
            cwd: Some(cwd),
        };
        match self.runner.run(&confined, &SandboxPolicy::default()) {
            Ok(out) => format_command_output(&out),
            Err(e) => command_error_outcome(&e),
        }
    }
}

/// Render a confined run's captured output into a `tool_result` body. Stdout first,
/// stderr labelled when non-empty, and a trailing status line for a non-zero exit,
/// a signal kill, or a timeout. A non-success run is marked `is_error` so the model
/// sees it failed (the bytes are still returned as data).
fn format_command_output(out: &ConfinedOutput) -> ToolOutcome {
    let mut body = String::new();
    body.push_str(&String::from_utf8_lossy(&out.stdout));

    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.trim().is_empty() {
        push_line_break(&mut body);
        body.push_str("[stderr]\n");
        body.push_str(&stderr);
    }

    if out.timed_out {
        push_line_break(&mut body);
        body.push_str("[timed out and the process group was killed]");
        return ToolOutcome::error(body);
    }
    match out.exit_code {
        Some(0) => ToolOutcome::ok(body),
        Some(code) => {
            push_line_break(&mut body);
            body.push_str(&format!("[exit status: {code}]"));
            ToolOutcome::error(body)
        }
        None => {
            push_line_break(&mut body);
            body.push_str("[killed by signal]");
            ToolOutcome::error(body)
        }
    }
}

/// A sandbox-setup / spawn failure rendered as a tool-level error result (fed back
/// to the model), not a transport error.
fn command_error_outcome(e: &SandboxError) -> ToolOutcome {
    ToolOutcome::error(format!("run_command: could not run confined: {e}"))
}

/// Append a newline to `body` only if it is non-empty and does not already end in
/// one, so appended sections start on their own line without doubling blank lines.
fn push_line_break(body: &mut String) {
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
}

// ===========================================================================
// FileSink - in-process filesystem tools with the gate's path checks.
// ===========================================================================

/// Runs the filesystem tools in-process. Holds the workspace `root` (the boundary
/// all *writes* are confined to), a clone of the single [`Secrets`] deny-set (so a
/// sensitive path is never read or written), and a small per-path content-hash map
/// recording what each file looked like when last read/written - the baseline the
/// `edit_file` staleness check compares against. Cloning shares the baseline map
/// (it is behind an `Arc`).
#[derive(Clone)]
pub struct FileSink {
    root: PathBuf,
    secrets: Secrets,
    /// Canonical path -> content hash observed at last read/write. The baseline for
    /// the `edit_file` staleness check.
    baselines: Arc<Mutex<HashMap<PathBuf, u64>>>,
}

impl FileSink {
    /// A file sink rooted at `root`, gating against `secrets`.
    pub fn new(root: PathBuf, secrets: Secrets) -> Self {
        Self {
            root,
            secrets,
            baselines: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    // --- read-only tools ---------------------------------------------------

    /// `read_file`: refuse a sensitive path, else read the file (optionally a
    /// 1-indexed inclusive line `range`). Records the read content as the staleness
    /// baseline. RAW output.
    pub fn read_file(&self, rf: &ReadFile) -> ToolOutcome {
        if self.secrets.is_sensitive_path(&rf.path) {
            return refused("read_file", &rf.path);
        }
        let abs = self.resolve(&rf.path);
        let bytes = match std::fs::read(&abs) {
            Ok(b) => b,
            Err(e) => return ToolOutcome::error(format!("read_file: {}: {e}", rf.path)),
        };
        self.record_baseline(&abs, &bytes);
        let text = String::from_utf8_lossy(&bytes);
        let body = match rf.range {
            Some([start, end]) => slice_lines(&text, start, end),
            None => text.into_owned(),
        };
        ToolOutcome::ok(body)
    }

    /// `list_dir`: refuse a sensitive directory, else list its entries (a trailing
    /// `/` marks a subdirectory), sorted.
    pub fn list_dir(&self, ld: &ListDir) -> ToolOutcome {
        if self.secrets.is_sensitive_path(&ld.path) {
            return refused("list_dir", &ld.path);
        }
        let abs = self.resolve(&ld.path);
        let rd = match std::fs::read_dir(&abs) {
            Ok(rd) => rd,
            Err(e) => return ToolOutcome::error(format!("list_dir: {}: {e}", ld.path)),
        };
        let mut names: Vec<String> = Vec::new();
        for entry in rd.flatten() {
            let mut name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                name.push('/');
            }
            names.push(name);
        }
        names.sort();
        ToolOutcome::ok(names.join("\n"))
    }

    /// `glob`: walk the tree under `root` (or the call's `root`), skipping sensitive
    /// paths, returning the root-relative paths matching `pattern` (`*`, `?`, and
    /// `**` segment wildcards), sorted. The walk is capped.
    pub fn glob(&self, g: &Glob) -> ToolOutcome {
        let base = match &g.root {
            Some(r) => self.resolve(r),
            None => self.root.clone(),
        };
        let mut out: Vec<String> = Vec::new();
        let mut stack = vec![base.clone()];
        let mut visited = 0usize;
        let mut capped = false;
        while let Some(dir) = stack.pop() {
            let rd = match std::fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(_) => continue,
            };
            for entry in rd.flatten() {
                visited += 1;
                if visited > MAX_WALK_ENTRIES {
                    capped = true;
                    break;
                }
                let path = entry.path();
                if self.secrets.is_sensitive_path(&path.to_string_lossy()) {
                    continue;
                }
                let rel = path.strip_prefix(&base).unwrap_or(&path);
                let rel_str = rel.to_string_lossy();
                if glob_match(&g.pattern, &rel_str) {
                    out.push(rel_str.into_owned());
                }
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    stack.push(path);
                }
            }
            if capped {
                break;
            }
        }
        out.sort();
        if capped {
            out.push(format!("[walk capped at {MAX_WALK_ENTRIES} entries]"));
        }
        ToolOutcome::ok(out.join("\n"))
    }

    /// `grep`: a fixed-string (literal substring) line search under `path` (or
    /// `root`), skipping sensitive paths and non-UTF-8 files, honouring an `i` flag
    /// for case-insensitivity. Emits `relpath:lineno:line` per hit, capped.
    ///
    /// v1 is a LITERAL substring search, not a full regex engine (aterm pulls in no
    /// regex dependency yet); the tool still returns real matches. Full
    /// regular-expression support is a recorded follow-up.
    pub fn grep(&self, gr: &Grep) -> ToolOutcome {
        let base = match &gr.path {
            Some(p) => self.resolve(p),
            None => self.root.clone(),
        };
        let case_insensitive = gr.flags.as_deref().is_some_and(|f| f.contains('i'));
        let needle = if case_insensitive {
            gr.pattern.to_lowercase()
        } else {
            gr.pattern.clone()
        };
        if needle.is_empty() {
            return ToolOutcome::error("grep: empty pattern");
        }

        // Labels are made relative to the search base: the base directory for a
        // tree search, or the file's own directory for a single-file search (so the
        // file name still shows, rather than stripping to empty).
        let label_root = if base.is_file() {
            base.parent()
                .map_or_else(|| self.root.clone(), Path::to_path_buf)
        } else {
            base.clone()
        };

        let mut files: Vec<PathBuf> = Vec::new();
        let mut visited = 0usize;
        let mut capped = false;
        if base.is_file() {
            files.push(base.clone());
        } else {
            let mut stack = vec![base.clone()];
            while let Some(dir) = stack.pop() {
                let rd = match std::fs::read_dir(&dir) {
                    Ok(rd) => rd,
                    Err(_) => continue,
                };
                for entry in rd.flatten() {
                    visited += 1;
                    if visited > MAX_WALK_ENTRIES {
                        capped = true;
                        break;
                    }
                    let path = entry.path();
                    if self.secrets.is_sensitive_path(&path.to_string_lossy()) {
                        continue;
                    }
                    if entry.file_type().is_ok_and(|t| t.is_dir()) {
                        stack.push(path);
                    } else {
                        files.push(path);
                    }
                }
                if capped {
                    break;
                }
            }
        }

        let mut hits: Vec<String> = Vec::new();
        'outer: for file in &files {
            let Ok(bytes) = std::fs::read(file) else {
                continue;
            };
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue; // skip binary / non-UTF-8
            };
            let rel = file.strip_prefix(&label_root).unwrap_or(file);
            let label = rel.to_string_lossy();
            for (i, line) in text.lines().enumerate() {
                let hay = if case_insensitive {
                    line.to_lowercase()
                } else {
                    line.to_string()
                };
                if hay.contains(&needle) {
                    hits.push(format!("{label}:{}:{line}", i + 1));
                    if hits.len() >= MAX_GREP_HITS {
                        capped = true;
                        break 'outer;
                    }
                }
            }
        }

        if hits.is_empty() {
            return ToolOutcome::ok("no matches");
        }
        if capped {
            hits.push(format!("[results capped at {MAX_GREP_HITS} matches]"));
        }
        ToolOutcome::ok(hits.join("\n"))
    }

    // --- mutating tools ----------------------------------------------------

    /// `write_file`: refuse a sensitive path, confine the target to the workspace
    /// root, then atomically (temp file + rename) create/overwrite it. Records the
    /// new content as the staleness baseline.
    pub fn write_file(&self, wf: &WriteFile) -> ToolOutcome {
        if self.secrets.is_sensitive_path(&wf.path) {
            return refused("write_file", &wf.path);
        }
        let target = match self.confined_target(&wf.path) {
            Ok(t) => t,
            Err(e) => return ToolOutcome::error(format!("write_file: {e}")),
        };
        if let Err(e) = atomic_write(&target, wf.content.as_bytes()) {
            return ToolOutcome::error(format!("write_file: {}: {e}", wf.path));
        }
        self.record_baseline(&target, wf.content.as_bytes());
        ToolOutcome::ok(format!("wrote {} bytes to {}", wf.content.len(), wf.path))
    }

    /// `edit_file`: refuse a sensitive path, confine to root, then perform an
    /// exactly-one-match `old_str` -> `new_str` replacement, atomically. Two guards
    /// before any write:
    ///   1. STALENESS - if the agent read this file earlier and it has changed on
    ///      disk since (its content hash differs from the recorded baseline), the
    ///      edit is rejected so a concurrent change is never clobbered.
    ///   2. EXACTLY ONE MATCH - `old_str` must occur exactly once; zero ("not
    ///      found") or many ("ambiguous") are rejected.
    pub fn edit_file(&self, ef: &EditFile) -> ToolOutcome {
        if self.secrets.is_sensitive_path(&ef.path) {
            return refused("edit_file", &ef.path);
        }
        if ef.old_str.is_empty() {
            return ToolOutcome::error("edit_file: old_str must not be empty");
        }
        let target = match self.confined_target(&ef.path) {
            Ok(t) => t,
            Err(e) => return ToolOutcome::error(format!("edit_file: {e}")),
        };
        let current = match std::fs::read(&target) {
            Ok(b) => b,
            Err(e) => return ToolOutcome::error(format!("edit_file: {}: {e}", ef.path)),
        };

        // (1) staleness: only meaningful if we have a baseline from an earlier read.
        let key = canonical_key(&target);
        let current_hash = hash_bytes(&current);
        if let Ok(map) = self.baselines.lock() {
            if let Some(&baseline) = map.get(&key) {
                if baseline != current_hash {
                    return ToolOutcome::error(format!(
                        "edit_file: {} changed on disk since it was last read - \
                         re-read it before editing (stale edit rejected)",
                        ef.path
                    ));
                }
            }
        }

        let text = match std::str::from_utf8(&current) {
            Ok(t) => t,
            Err(_) => return ToolOutcome::error(format!("edit_file: {} is not UTF-8", ef.path)),
        };

        // (2) exactly one match.
        let count = text.matches(&ef.old_str).count();
        if count == 0 {
            return ToolOutcome::error(format!("edit_file: old_str not found in {}", ef.path));
        }
        if count > 1 {
            return ToolOutcome::error(format!(
                "edit_file: old_str matched {count} times in {} - it must match exactly once; \
                 include more surrounding context to disambiguate",
                ef.path
            ));
        }

        let updated = text.replacen(&ef.old_str, &ef.new_str, 1);
        if let Err(e) = atomic_write(&target, updated.as_bytes()) {
            return ToolOutcome::error(format!("edit_file: {}: {e}", ef.path));
        }
        self.record_baseline(&target, updated.as_bytes());
        ToolOutcome::ok(format!("edited {} (1 replacement)", ef.path))
    }

    // --- helpers -----------------------------------------------------------

    /// Resolve a model-supplied path: absolute as-is, relative against the root. No
    /// tilde expansion (the no-shell design never invokes a shell to expand `~`).
    fn resolve(&self, p: &str) -> PathBuf {
        let path = Path::new(p);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        }
    }

    /// Resolve a WRITE target and confine it to the workspace root. Canonicalizes
    /// the parent directory (which must exist) so a `..` escape or a symlinked
    /// parent that would leave the root is denied; then re-attaches the file name.
    fn confined_target(&self, p: &str) -> Result<PathBuf, String> {
        let abs = self.resolve(p);
        let parent = abs
            .parent()
            .ok_or_else(|| format!("{p}: path has no parent directory"))?;
        let cparent = std::fs::canonicalize(parent)
            .map_err(|e| format!("{p}: parent directory unavailable: {e}"))?;
        let croot = std::fs::canonicalize(&self.root)
            .map_err(|e| format!("workspace root unavailable: {e}"))?;
        if !cparent.starts_with(&croot) {
            return Err(format!("{p}: path escapes the workspace root"));
        }
        let name = abs
            .file_name()
            .ok_or_else(|| format!("{p}: path has no file name"))?;
        Ok(cparent.join(name))
    }

    /// Record `bytes`' content hash as the staleness baseline for `abs` (keyed by
    /// its canonical path, so a later read/edit resolves to the same key).
    fn record_baseline(&self, abs: &Path, bytes: &[u8]) {
        if let Ok(mut map) = self.baselines.lock() {
            map.insert(canonical_key(abs), hash_bytes(bytes));
        }
    }
}

/// A tool-level "refused: sensitive path" error result.
fn refused(tool: &str, path: &str) -> ToolOutcome {
    ToolOutcome::error(format!(
        "{tool}: refused - {path} matches the sensitive-path deny-set"
    ))
}

/// The canonical absolute path used as the staleness-baseline map key (so a read
/// and a later edit of the same file resolve to the same key even when the root or
/// path is reached through a symlink). Falls back to the input when the file does
/// not (yet) exist.
fn canonical_key(abs: &Path) -> PathBuf {
    std::fs::canonicalize(abs).unwrap_or_else(|_| abs.to_path_buf())
}

/// Stable content hash for the staleness baseline (detects any content change
/// regardless of mtime granularity).
fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// Monotonic counter making each atomic-write temp file name unique even under
/// concurrent writes (mutations serialize in the turn loop, but the helper is
/// robust on its own).
static WRITE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to `target` atomically: write a temp file in the SAME directory
/// (so the rename stays on one filesystem and is atomic), fsync it, then rename
/// over the target. A leftover temp file on error is best-effort removed.
fn atomic_write(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = target.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "target has no parent")
    })?;
    let seq = WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".aterm-write-{}-{seq}.tmp", std::process::id()));

    let write_result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Take 1-indexed inclusive lines `[start, end]` from `text`. Out-of-range bounds
/// clamp; `start > end` (or `start` past the end) yields empty.
fn slice_lines(text: &str, start: i64, end: i64) -> String {
    let total = text.lines().count();
    if total == 0 {
        return String::new();
    }
    let start = start.max(1) as usize;
    let end = end.max(0) as usize;
    if start > total || end < start {
        return String::new();
    }
    let end = end.min(total);
    text.lines()
        .skip(start - 1)
        .take(end - start + 1)
        .collect::<Vec<_>>()
        .join("\n")
}

// --- glob matching ---------------------------------------------------------

/// Match a `/`-separated `path` against a glob `pattern`. Within a segment `*`
/// matches any run of non-`/` characters and `?` matches one; a whole `**` segment
/// matches zero or more path segments (so `**/*.rs` matches `a/b/c.rs` and `c.rs`).
fn glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').collect();
    let txt: Vec<&str> = path.split('/').collect();
    seg_match(&pat, &txt)
}

fn seg_match(pat: &[&str], txt: &[&str]) -> bool {
    match pat.split_first() {
        None => txt.is_empty(),
        Some((&"**", rest)) => (0..=txt.len()).any(|i| seg_match(rest, &txt[i..])),
        Some((seg, rest)) => {
            !txt.is_empty() && one_segment_match(seg, txt[0]) && seg_match(rest, &txt[1..])
        }
    }
}

/// Classic single-segment wildcard match: `*` any run, `?` one char, else literal.
fn one_segment_match(pattern: &str, s: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let c: Vec<char> = s.chars().collect();
    let (mut pi, mut si) = (0usize, 0usize);
    let (mut star, mut star_si) = (None, 0usize);
    while si < c.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == c[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            star_si = si;
            pi += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            star_si += 1;
            si = star_si;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

// ===========================================================================
// PtyInjectSink - the separate, harder-gated live-shell inject path.
// ===========================================================================

/// How a command proposed for injection into the live interactive shell is
/// resolved by the inject gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InjectDisposition {
    /// Safe and not shell-active: may be injected without asking.
    AutoInject,
    /// Requires explicit confirmation; carries the reasons.
    NeedsConfirm(Vec<RiskReason>),
}

impl InjectDisposition {
    /// Whether this disposition auto-injects.
    pub fn is_auto(&self) -> bool {
        matches!(self, InjectDisposition::AutoInject)
    }
}

/// Injects an agent-proposed command into the live interactive shell over the
/// T-1.1 PTY writer - the separate, HARDER-gated execution path. Unlike the
/// no-shell [`CommandSink`], a real shell interprets whatever is injected, so the
/// gate refuses to auto-inject ANY shell-active command, even one that would
/// otherwise rate `Safe` (defense in depth on top of [`ApprovalPolicy`]'s own
/// shell-active refusal). Gates against the same single [`Secrets`] source as the
/// rest of the spine.
pub struct PtyInjectSink {
    policy: ApprovalPolicy,
    secrets: Secrets,
}

impl PtyInjectSink {
    /// An inject sink with the default AUTO-SAFE policy, gating against `secrets`.
    pub fn new(secrets: Secrets) -> Self {
        Self {
            policy: ApprovalPolicy::new(),
            secrets,
        }
    }

    /// An inject sink with an explicit policy (e.g. the ask-always tier).
    pub fn with_policy(policy: ApprovalPolicy, secrets: Secrets) -> Self {
        Self { policy, secrets }
    }

    /// Gate a command line proposed for live-shell injection. Classifies against the
    /// single `Secrets` (so it cannot drift from the rest of the spine) via the
    /// multi-line buffer gate, then applies the inject-only hard rule: a shell-active
    /// command never auto-injects.
    pub fn gate(&self, line: &str) -> InjectDisposition {
        match self.policy.decide(line, &self.secrets) {
            Approval::AutoApprove => InjectDisposition::AutoInject,
            Approval::RequireConfirm(a) => InjectDisposition::NeedsConfirm(a.reasons),
        }
    }

    /// Defense-in-depth gate over an ALREADY-computed assessment: independently
    /// refuse to auto-inject any shell-active assessment - even one a (future, buggy)
    /// classifier rated `Safe` - before deferring to the policy. This is what makes
    /// the inject path strictly harder-gated than the no-shell subprocess path.
    pub fn gate_assessment(&self, assessment: &RiskAssessment) -> InjectDisposition {
        if assessment.is_shell_active() {
            return InjectDisposition::NeedsConfirm(assessment.reasons.clone());
        }
        match self.policy.decide_assessment(assessment.clone()) {
            Approval::AutoApprove => InjectDisposition::AutoInject,
            Approval::RequireConfirm(a) => InjectDisposition::NeedsConfirm(a.reasons),
        }
    }

    /// Inject `line` into the live shell: write it followed by a newline so the
    /// shell executes it. The caller reaches here only after [`gate`](Self::gate)
    /// returned [`InjectDisposition::AutoInject`] or the user confirmed.
    pub fn inject(&self, writer: &mut dyn Write, line: &str) -> std::io::Result<()> {
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()
    }
}

// ===========================================================================
// Sinks - the ToolDispatch the turn loop holds.
// ===========================================================================

/// The execution dispatcher the turn loop (T-5.8) drives: routes each typed tool
/// call to the [`CommandSink`] (run_command) or the [`FileSink`] (filesystem
/// tools). Each blocking sink call runs on a `spawn_blocking` worker so the
/// dispatcher stays async and the loop's read-only fan-out is genuinely concurrent.
#[derive(Clone)]
pub struct Sinks<S: Sandbox> {
    command: CommandSink<S>,
    files: FileSink,
}

impl<S: Sandbox + Clone> Sinks<S> {
    /// Build sinks over an explicit sandbox runner, workspace `root`, and the
    /// single `secrets` source (shared by the command sink's confinement, the file
    /// sink's path checks, and - upstream - the gate + sanitizer).
    pub fn with_sandbox(runner: SandboxRunner<S>, root: PathBuf, secrets: Secrets) -> Self {
        Self {
            command: CommandSink::new(runner, root.clone()),
            files: FileSink::new(root, secrets),
        }
    }

    /// The command sink (for logging / direct use).
    pub fn command(&self) -> &CommandSink<S> {
        &self.command
    }

    /// The file sink (for direct use / tests).
    pub fn files(&self) -> &FileSink {
        &self.files
    }
}

impl Sinks<SeatbeltSandbox> {
    /// The production default on macOS: a Seatbelt-confined command sink seeded
    /// from the SAME `secrets` the file sink uses, so the kernel deny-set, the
    /// in-process path checks, and the sanitizer all derive from one source.
    pub fn seatbelt(root: PathBuf, secrets: Secrets) -> Self {
        let runner = SandboxRunner::new(SeatbeltSandbox::from_secrets(&secrets));
        Self::with_sandbox(runner, root, secrets)
    }
}

impl<S: Sandbox + Clone + Send + 'static> ToolDispatch for Sinks<S> {
    async fn dispatch(&self, input: ToolInput) -> ToolOutcome {
        match input {
            ToolInput::RunCommand(rc) => {
                let sink = self.command.clone();
                run_blocking(move || sink.run(&rc)).await
            }
            ToolInput::ReadFile(rf) => {
                let sink = self.files.clone();
                run_blocking(move || sink.read_file(&rf)).await
            }
            ToolInput::EditFile(ef) => {
                let sink = self.files.clone();
                run_blocking(move || sink.edit_file(&ef)).await
            }
            ToolInput::WriteFile(wf) => {
                let sink = self.files.clone();
                run_blocking(move || sink.write_file(&wf)).await
            }
            ToolInput::ListDir(ld) => {
                let sink = self.files.clone();
                run_blocking(move || sink.list_dir(&ld)).await
            }
            ToolInput::Glob(g) => {
                let sink = self.files.clone();
                run_blocking(move || sink.glob(&g)).await
            }
            ToolInput::Grep(gr) => {
                let sink = self.files.clone();
                run_blocking(move || sink.grep(&gr)).await
            }
        }
    }
}

/// Run a blocking sink call on a worker thread, mapping a join failure to a
/// tool-level error result rather than panicking the loop.
async fn run_blocking<F>(f: F) -> ToolOutcome
where
    F: FnOnce() -> ToolOutcome + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(outcome) => outcome,
        Err(e) => ToolOutcome::error(format!("sink: execution task failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::NoSandbox;

    // A unique temp dir per test, cleaned at the end (mirrors sandbox.rs's
    // convention - the crate pulls in no tempfile dependency).
    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "atermr-sink-{tag}-{}-{}",
            std::process::id(),
            WRITE_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Canonicalize so confinement comparisons match (macOS /var -> /private/var).
        std::fs::canonicalize(&dir).unwrap()
    }

    fn run_command(argv: &[&str]) -> RunCommand {
        RunCommand {
            command: argv.iter().map(|s| (*s).to_string()).collect(),
            cwd: None,
        }
    }

    // ---- AC1: run_command runs with NO shell + captures output -------------

    #[test]
    fn run_command_executes_argv_with_no_shell() {
        let root = temp_root("noshell");
        let sink = CommandSink::new(SandboxRunner::new(NoSandbox), root.clone());
        // $HOME is a literal argv token - a shell would expand it; exec'd directly
        // it prints verbatim, proving no shell interpreted the command.
        let out = sink.run(&run_command(&["/bin/echo", "$HOME"]));
        assert!(!out.is_error, "echo should succeed: {out:?}");
        assert_eq!(out.output, "$HOME\n");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn run_command_captures_stdout_and_marks_nonzero_exit() {
        let root = temp_root("exit");
        let sink = CommandSink::new(SandboxRunner::new(NoSandbox), root.clone());
        let ok = sink.run(&run_command(&["/bin/echo", "hi"]));
        assert!(!ok.is_error);
        assert_eq!(ok.output, "hi\n");
        // Listing a nonexistent path exits non-zero -> is_error with a status note
        // (portable across macOS + Linux, and still no shell).
        let bad = sink.run(&run_command(&["/bin/ls", "/atermr-nonexistent-path-zzz"]));
        assert!(bad.is_error);
        assert!(bad.output.contains("exit status"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn run_command_empty_argv_is_an_error_not_a_panic() {
        let root = temp_root("emptyargv");
        let sink = CommandSink::new(SandboxRunner::new(NoSandbox), root.clone());
        let out = sink.run(&RunCommand {
            command: vec![],
            cwd: None,
        });
        assert!(out.is_error);
        assert!(out.output.contains("empty command"));
        std::fs::remove_dir_all(&root).ok();
    }

    // ---- AC2: edit_file staleness + exactly-one-match ----------------------

    #[test]
    fn edit_file_requires_exactly_one_match() {
        let root = temp_root("edit-one");
        let secrets = Secrets::new();
        let files = FileSink::new(root.clone(), secrets);

        // Two occurrences -> ambiguous, rejected, file unchanged.
        std::fs::write(root.join("dup.txt"), "x\nx\n").unwrap();
        let dup = files.edit_file(&EditFile {
            path: "dup.txt".into(),
            old_str: "x".into(),
            new_str: "y".into(),
        });
        assert!(dup.is_error);
        assert!(dup.output.contains("matched 2 times"));
        assert_eq!(
            std::fs::read_to_string(root.join("dup.txt")).unwrap(),
            "x\nx\n"
        );

        // Zero occurrences -> not found.
        let none = files.edit_file(&EditFile {
            path: "dup.txt".into(),
            old_str: "absent".into(),
            new_str: "y".into(),
        });
        assert!(none.is_error);
        assert!(none.output.contains("not found"));

        // Exactly one -> succeeds.
        std::fs::write(root.join("one.txt"), "before MARK after").unwrap();
        let ok = files.edit_file(&EditFile {
            path: "one.txt".into(),
            old_str: "MARK".into(),
            new_str: "DONE".into(),
        });
        assert!(!ok.is_error, "{ok:?}");
        assert_eq!(
            std::fs::read_to_string(root.join("one.txt")).unwrap(),
            "before DONE after"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn edit_file_rejects_a_stale_edit() {
        let root = temp_root("edit-stale");
        let files = FileSink::new(root.clone(), Secrets::new());
        let path = root.join("doc.txt");
        std::fs::write(&path, "alpha ONE omega").unwrap();

        // The agent reads it -> records the staleness baseline.
        let read = files.read_file(&ReadFile {
            path: "doc.txt".into(),
            range: None,
        });
        assert!(!read.is_error);

        // The file changes on disk out from under the agent.
        std::fs::write(&path, "alpha ONE omega CHANGED").unwrap();

        // The edit (still matching exactly once) is now rejected as stale.
        let stale = files.edit_file(&EditFile {
            path: "doc.txt".into(),
            old_str: "ONE".into(),
            new_str: "TWO".into(),
        });
        assert!(stale.is_error, "a stale edit must be rejected");
        assert!(stale.output.contains("changed on disk"));
        // The on-disk file is untouched.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "alpha ONE omega CHANGED"
        );

        // Re-reading refreshes the baseline; the same edit then succeeds.
        files.read_file(&ReadFile {
            path: "doc.txt".into(),
            range: None,
        });
        let ok = files.edit_file(&EditFile {
            path: "doc.txt".into(),
            old_str: "ONE".into(),
            new_str: "TWO".into(),
        });
        assert!(!ok.is_error, "{ok:?}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "alpha TWO omega CHANGED"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    // ---- AC3: live-PTY inject is harder-gated ------------------------------

    #[test]
    fn inject_gate_forces_confirm_for_shell_active_commands() {
        let sink = PtyInjectSink::new(Secrets::new());
        // Plain command auto-injects ...
        assert_eq!(sink.gate("echo hello"), InjectDisposition::AutoInject);
        // ... but anything a shell would interpret must confirm.
        for line in [
            "echo hi > out.txt",
            "ls | wc -l",
            "echo $(whoami)",
            "a && b",
        ] {
            assert!(
                !sink.gate(line).is_auto(),
                "shell-active line must require confirmation on inject: {line}"
            );
        }
    }

    #[test]
    fn inject_gate_refuses_a_safe_but_shell_active_assessment() {
        // Defense in depth (AC3 "even if otherwise Safe"): a synthetic Safe-level
        // assessment that nonetheless carries a shell-active reason must NOT
        // auto-inject - the inject sink refuses on the reason directly.
        let sink = PtyInjectSink::new(Secrets::new());
        let safe_but_active = RiskAssessment {
            level: crate::risk::Risk::Safe,
            reasons: vec![RiskReason::RedirectOverwrite],
        };
        assert!(!sink.gate_assessment(&safe_but_active).is_auto());

        // A genuinely safe, non-shell-active assessment still auto-injects.
        let plain = RiskAssessment {
            level: crate::risk::Risk::Safe,
            reasons: vec![],
        };
        assert_eq!(sink.gate_assessment(&plain), InjectDisposition::AutoInject);
    }

    #[test]
    fn inject_writes_the_line_and_a_newline_to_the_writer() {
        let sink = PtyInjectSink::new(Secrets::new());
        let mut buf: Vec<u8> = Vec::new();
        sink.inject(&mut buf, "git status").unwrap();
        assert_eq!(buf, b"git status\n");
    }

    // ---- AC4: writes are confined (a write outside the root is denied) -----

    #[test]
    fn write_outside_the_root_is_denied() {
        let root = temp_root("confine");
        let files = FileSink::new(root.clone(), Secrets::new());

        // An absolute path outside the workspace root.
        let outside = std::env::temp_dir().join(format!(
            "atermr-escape-{}-{}.txt",
            std::process::id(),
            WRITE_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::remove_file(&outside).ok();
        let denied = files.write_file(&WriteFile {
            path: outside.to_string_lossy().into_owned(),
            content: "pwned".into(),
        });
        assert!(denied.is_error, "a write outside the root must be denied");
        assert!(denied.output.contains("escapes the workspace root"));
        assert!(
            !outside.exists(),
            "the denied write must not create the file"
        );

        // A `..` traversal back out of the root is likewise denied.
        let escape = files.write_file(&WriteFile {
            path: "../atermr-escape-rel.txt".into(),
            content: "pwned".into(),
        });
        assert!(escape.is_error);
        assert!(!root
            .parent()
            .unwrap()
            .join("atermr-escape-rel.txt")
            .exists());

        // A write INSIDE the root succeeds.
        let ok = files.write_file(&WriteFile {
            path: "inside.txt".into(),
            content: "fine".into(),
        });
        assert!(!ok.is_error, "{ok:?}");
        assert_eq!(
            std::fs::read_to_string(root.join("inside.txt")).unwrap(),
            "fine"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    // ---- AC5: no secret leak -----------------------------------------------

    #[test]
    fn file_sink_refuses_to_read_a_secret_path() {
        // The structural half of AC5: a credential file's contents never even enter
        // the output buffer, because the sink refuses the read outright (the turn
        // loop auto-approves reads, so this in-process gate is the only backstop).
        let root = temp_root("secret-read");
        let files = FileSink::new(root.clone(), Secrets::new());
        // A real file whose PATH matches the deny-set.
        std::fs::create_dir_all(root.join(".ssh")).unwrap();
        std::fs::write(root.join(".ssh/id_rsa"), "PRIVATE-KEY-CANARY").unwrap();
        let out = files.read_file(&ReadFile {
            path: ".ssh/id_rsa".into(),
            range: None,
        });
        assert!(out.is_error);
        assert!(out.output.contains("sensitive-path deny-set"));
        assert!(
            !out.output.contains("PRIVATE-KEY-CANARY"),
            "the secret file's contents must never appear in the result"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn grep_skips_secret_files_so_their_contents_never_leak() {
        let root = temp_root("secret-grep");
        let files = FileSink::new(root.clone(), Secrets::new());
        std::fs::write(root.join("public.txt"), "needle here\n").unwrap();
        std::fs::write(root.join(".env"), "needle SECRET-VALUE-CANARY\n").unwrap();
        let out = files.grep(&Grep {
            pattern: "needle".into(),
            path: None,
            flags: None,
        });
        assert!(!out.is_error);
        assert!(out.output.contains("public.txt"));
        assert!(
            !out.output.contains("SECRET-VALUE-CANARY"),
            "a secret file must be skipped, not scanned: {out:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    // ---- read-only tool happy paths ----------------------------------------

    #[test]
    fn read_file_honors_a_line_range() {
        let root = temp_root("range");
        let files = FileSink::new(root.clone(), Secrets::new());
        std::fs::write(root.join("lines.txt"), "L1\nL2\nL3\nL4\nL5\n").unwrap();
        let out = files.read_file(&ReadFile {
            path: "lines.txt".into(),
            range: Some([2, 4]),
        });
        assert!(!out.is_error);
        assert_eq!(out.output, "L2\nL3\nL4");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn list_dir_lists_entries_with_dir_suffix() {
        let root = temp_root("list");
        let files = FileSink::new(root.clone(), Secrets::new());
        std::fs::write(root.join("file.txt"), "x").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        let out = files.list_dir(&ListDir { path: ".".into() });
        assert!(!out.is_error);
        let lines: Vec<&str> = out.output.lines().collect();
        assert!(lines.contains(&"file.txt"));
        assert!(lines.contains(&"sub/"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn glob_matches_recursively() {
        let root = temp_root("glob");
        let files = FileSink::new(root.clone(), Secrets::new());
        std::fs::create_dir_all(root.join("src/inner")).unwrap();
        std::fs::write(root.join("src/a.rs"), "").unwrap();
        std::fs::write(root.join("src/inner/b.rs"), "").unwrap();
        std::fs::write(root.join("src/c.txt"), "").unwrap();
        let out = files.glob(&Glob {
            pattern: "**/*.rs".into(),
            root: None,
        });
        assert!(!out.is_error);
        let hits: Vec<&str> = out.output.lines().collect();
        assert!(hits.contains(&"src/a.rs"));
        assert!(hits.contains(&"src/inner/b.rs"));
        assert!(!hits.iter().any(|h| h.ends_with(".txt")));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn grep_finds_matches_with_line_numbers_and_case_flag() {
        let root = temp_root("grep");
        let files = FileSink::new(root.clone(), Secrets::new());
        std::fs::write(root.join("f.txt"), "first TODO\nsecond\nThird todo\n").unwrap();
        let sensitive = files.grep(&Grep {
            pattern: "TODO".into(),
            path: Some("f.txt".into()),
            flags: None,
        });
        assert!(sensitive.output.contains("f.txt:1:first TODO"));
        assert!(!sensitive.output.contains("Third todo"));
        let insensitive = files.grep(&Grep {
            pattern: "todo".into(),
            path: Some("f.txt".into()),
            flags: Some("i".into()),
        });
        assert!(insensitive.output.contains("f.txt:1:"));
        assert!(insensitive.output.contains("f.txt:3:"));
        std::fs::remove_dir_all(&root).ok();
    }

    // ---- pure helpers -------------------------------------------------------

    #[test]
    fn glob_match_segment_semantics() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "src/main.rs")); // * does not cross /
        assert!(glob_match("**/*.rs", "src/main.rs"));
        assert!(glob_match("**/*.rs", "a/b/c.rs"));
        assert!(glob_match("**/*.rs", "top.rs")); // ** matches zero segments
        assert!(glob_match("src/*.rs", "src/main.rs"));
        assert!(!glob_match("src/*.rs", "src/inner/x.rs"));
        assert!(glob_match("file?.txt", "file1.txt"));
        assert!(!glob_match("file?.txt", "file12.txt"));
    }

    #[test]
    fn slice_lines_clamps_and_handles_out_of_range() {
        let t = "a\nb\nc\nd";
        assert_eq!(slice_lines(t, 2, 3), "b\nc");
        assert_eq!(slice_lines(t, 1, 100), "a\nb\nc\nd"); // end clamps
        assert_eq!(slice_lines(t, 0, 1), "a"); // start clamps to 1
        assert_eq!(slice_lines(t, 5, 6), ""); // start past end
        assert_eq!(slice_lines(t, 3, 2), ""); // start > end
    }

    #[test]
    fn atomic_write_replaces_content() {
        let root = temp_root("atomic");
        let target = root.join("a.txt");
        atomic_write(&target, b"v1").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"v1");
        atomic_write(&target, b"v2-longer").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"v2-longer");
        // No leftover temp files in the directory.
        let leftovers = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".aterm-write-"))
            .count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(&root).ok();
    }

    // ---- end-to-end: the real Sinks driven by the real turn loop -----------
    //
    // These prove the COMPOSED pipeline the ACs really care about: a Safe command
    // auto-runs under AUTO-SAFE and actually executes (AC1), and the raw sink
    // output is sanitized by the turn loop before it re-enters context (AC5).

    use crate::provider::{
        ContentBlock, Effort, MockProvider, ProviderEvent, StopReason, ToolCall, Usage,
    };
    use crate::risk::RiskAssessment as Assessment;
    use crate::tools::ToolRegistry;
    use crate::turn::{AgentTurn, CancelToken, ConfirmDecision, ConfirmHandler};
    use crate::Message;
    use crate::TurnRequest;
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::mpsc;

    fn tool_round(id: &str, name: &str, input_json: &str) -> Vec<ProviderEvent> {
        vec![
            ProviderEvent::MessageStart,
            ProviderEvent::ToolUseStart {
                id: id.to_string(),
                name: name.to_string(),
            },
            ProviderEvent::ToolUseInputDelta {
                json: input_json.to_string(),
            },
            ProviderEvent::ToolUseStop,
            ProviderEvent::MessageDelta {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
            },
            ProviderEvent::MessageStop,
        ]
    }

    fn end_round(text: &str) -> Vec<ProviderEvent> {
        vec![
            ProviderEvent::MessageStart,
            ProviderEvent::TextDelta(text.to_string()),
            ProviderEvent::MessageDelta {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            },
            ProviderEvent::MessageStop,
        ]
    }

    fn req() -> TurnRequest {
        TurnRequest {
            model: "claude-opus-4-8".to_string(),
            system: None,
            messages: vec![Message::user("go")],
            tools: ToolRegistry::default().specs(),
            effort: Effort::Medium,
            max_tokens: 1024,
        }
    }

    /// Counts consultations, so a test can prove an AUTO-SAFE call never asks.
    #[derive(Default)]
    struct CountingApprover {
        calls: Arc<AtomicUsize>,
    }
    impl ConfirmHandler for CountingApprover {
        async fn confirm(&self, _c: &ToolCall, _a: &Assessment) -> ConfirmDecision {
            self.calls.fetch_add(1, Ordering::SeqCst);
            ConfirmDecision::Denied
        }
    }

    #[tokio::test]
    async fn safe_run_command_auto_runs_and_executes_through_the_loop() {
        let root = temp_root("e2e-auto");
        let provider = MockProvider::scripted(vec![
            tool_round(
                "t1",
                "run_command",
                r#"{"command":["/bin/echo","auto-safe-ran"]}"#,
            ),
            end_round("done"),
        ]);
        let secrets = Secrets::new();
        let sinks =
            Sinks::with_sandbox(SandboxRunner::new(NoSandbox), root.clone(), secrets.clone());
        let approver = CountingApprover::default();
        let turn = AgentTurn::new(&provider, &secrets);
        let (etx, _erx) = mpsc::channel(256);

        turn.run(
            req(),
            &ToolRegistry::default(),
            &sinks,
            &approver,
            &CancelToken::new(),
            etx,
        )
        .await
        .unwrap();

        // AUTO-SAFE: the safe command never reached the approver ...
        assert_eq!(approver.calls.load(Ordering::SeqCst), 0);
        // ... and the real sink executed it, feeding the captured output back.
        let reqs = provider.requests();
        let fed_back = reqs[1]
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .find_map(|b| match b {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
            .unwrap();
        assert!(fed_back.contains("auto-safe-ran"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn raw_sink_output_is_sanitized_before_it_re_enters_context() {
        let root = temp_root("e2e-redact");
        // The command echoes a value registered as a secret; the sink returns it
        // RAW, and the turn loop must redact it before feeding it back.
        let mut secrets = Secrets::new();
        secrets.add_value("sk-SINK-LEAK-CANARY-9999");
        // The argv is a single literal token (no `=`, so it stays off the
        // assignment-prefix path and classifies Safe -> auto-runs -> really echoes
        // the secret), so the redaction is exercised on output that genuinely ran.
        let provider = MockProvider::scripted(vec![
            tool_round(
                "t1",
                "run_command",
                r#"{"command":["/bin/echo","sk-SINK-LEAK-CANARY-9999"]}"#,
            ),
            end_round("done"),
        ]);
        let sinks =
            Sinks::with_sandbox(SandboxRunner::new(NoSandbox), root.clone(), secrets.clone());
        let turn = AgentTurn::new(&provider, &secrets);
        let (etx, mut erx) = mpsc::channel(256);

        turn.run(
            req(),
            &ToolRegistry::default(),
            &sinks,
            &CountingApprover::default(),
            &CancelToken::new(),
            etx,
        )
        .await
        .unwrap();

        // The fed-back tool_result is redacted.
        let reqs = provider.requests();
        let fed_back = reqs[1]
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .find_map(|b| match b {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
            .unwrap();
        assert!(
            !fed_back.contains("LEAK-CANARY"),
            "the secret value must be redacted before re-entering context: {fed_back}"
        );

        // The emitted timeline result is redacted too.
        let mut shown = String::new();
        while let Ok(ev) = erx.try_recv() {
            if let crate::provider::AgentEvent::ToolResult { output, .. } = ev {
                shown = output;
            }
        }
        assert!(!shown.contains("LEAK-CANARY"));
        std::fs::remove_dir_all(&root).ok();
    }
}
