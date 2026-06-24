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

# Notes

2026-06-24 (agent): Landed. All four ACs met and verified headlessly (no
window/GPU/hardware needed).

**Reply path (deadlock-safe by construction).** `Event::PtyWrite` now travels a
dedicated bounded reply channel (`Terminal::replies()`, `REPLY_CHANNEL_CAP`),
*not* the app-facing event channel - so the `TerminalEvent::PtyWrite` variant was
removed (it was a stub the app never consumed). The model thread drains the reply
channel after each `feed` and writes the bytes back to the master. Because the
master fd is *blocking* (portable-pty never sets `O_NONBLOCK`) and the model thread
is the only writer that must never block, the write is guarded: a non-blocking
`poll(POLLOUT)` probe, then a SINGLE `write()` (never `write_all`) which writes what
fits and returns short rather than blocking; any unwritten tail is held in
`pending_reply` and resumed next cycle (so a reply is never truncated, which would
corrupt the child's input). Memory stays bounded: `pending_reply` holds at most one
reply, the channel is bounded + drop-on-full. AC verified by
`da_query_gets_a_reply_written_back_to_the_pty` (a real DA `\x1b[c` round-trips
into the grid) and `dsr_flood_does_not_deadlock_the_reply_path` (`yes $'\x1b[6n'`
fills the input buffer and exercises the short-write path without hanging).

**Foreground signalling.** `Engine::foreground_pgid()` / `signal_foreground(Signal)`
resolve + signal the terminal's foreground process group via `tcgetpgrp`/`killpg`
on a **dup** of the master fd (an `OwnedFd` the handle owns), so the main thread can
signal independently of the model thread's `Pty` and the fd can never be a *reused*
descriptor. A `pgid <= 1` guard refuses to signal (`killpg(0)` would hit our own
group; `1` is init). New platform-neutral `Signal` enum maps to `nix::Signal`;
`nix` is unix-target-gated (0.28, unifying with the tree; features `term`+`signal`).
Non-Unix compiles as a stub (`None` / `Err`). Verified by
`foreground_pgid_and_signal_interrupt_sleep` (observes `sleep 10` reaped early after
SIGINT) + `foreground_pgid_reports_a_running_child` +
`signal_foreground_interrupts_a_running_sleep`.

**Review.** Adversarial review (3 lenses x skeptic verify, 16 findings) confirmed
the fd dup soundness, the pgid guard, the `Signal` mapping, memory-boundedness, and
the non-unix stubs. It found ONE real defect - a residual mid-write deadlock window
(`poll(POLLOUT)` promises only 1 byte, `write_all` could block on a partial write) -
which the single-`write()`+`pending_reply` design above fixes by construction. Stayed
within the locked 3-thread model (no writer thread added).

AC2's "updates when it exits" is satisfied by `foreground_pgid` being a *live*
`tcgetpgrp` query (never cached); an explicit post-exit-transition test was omitted
as OS-dependent/flaky (the value tcgetpgrp returns after the session ends varies).
`fmt`/`clippy`/full-workspace `build`/`test` all green; objc2/nix graphs unchanged
(no version explosion); `nix` is MIT.
