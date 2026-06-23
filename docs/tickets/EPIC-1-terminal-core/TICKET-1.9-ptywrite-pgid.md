---
id: T-1.9
epic: EPIC-1-terminal-core
title: Event::PtyWrite reply channel + foreground pgid tracking
status: ready-for-agent
labels: [core, pty]
depends_on: [T-1.1, T-1.2]
---

# Goal

Wire terminal query replies (DA/DSR/cursor-position) from `Term`'s `Event::PtyWrite` back to the PTY master writer, and track the foreground process-group id so Ctrl-C / agent-cancel hit the right process.

# Context

- Research: [03-pty-vt-rust.md](../../research/03-pty-vt-rust.md) Recommendations 8-9 and section A (resize/signals/process groups). Risk: exact `portable-pty` 0.9 fd API for signal-to-foreground-group needs re-verification in code (`as_raw_fd` + `nix::sys::signal::killpg`).

# Implementation notes

- Crate: `aterm-core`.
- `Event::PtyWrite(String)` from the `EventListener` (T-1.2) must be forwarded to the PTY writer (T-1.1) so programs probing the terminal (DA/DSR) get replies. Dropping these breaks programs that query the terminal.
- Foreground pgid: track via the master fd / procinfo (wezterm's procinfo is the reference; Zed vendors one struct from a wezterm fork for this). Expose `foreground_pgid() -> Option<pid_t>`.
- Signals: Ctrl-C/Ctrl-Z normally flow as control bytes written to the master (line discipline -> SIGINT/SIGTSTP for the foreground group). For agent-initiated cancellation, also expose `signal_foreground(Signal)` via `nix::sys::signal::killpg(pgid, sig)` using the raw fd. Dependency: `nix` (pin), Unix-gated.

# Acceptance criteria

- A program issuing a DA query (`\x1b[c`) over the PTY receives a reply written back (assert the master writer saw the response bytes).
- `foreground_pgid()` returns the pgid of a running foreground child (e.g. `sleep 5`) and updates when it exits.
- `signal_foreground(SIGINT)` interrupts a running `sleep` (child exits with the signal).
- Cross-platform: `signal_foreground`/pgid are `#[cfg(unix)]`; the crate still compiles on a non-Unix target (stub/unsupported).

# Out of scope

- Mapping keystrokes to control bytes (T-3.4).
- Agent cancel UX (T-5.11).
