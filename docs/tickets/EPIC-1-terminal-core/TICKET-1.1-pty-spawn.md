---
id: T-1.1
epic: EPIC-1-terminal-core
title: PTY spawn/resize/signals over portable-pty
status: ready-for-agent
labels: [core, pty]
depends_on: []
---

# Goal

Spawn a hidden login shell on a PTY in `aterm-core`, with a clean owned API for resize, writing input, sending signals, and reaping the child. This is the bottom of the engine; everything else feeds off it.

# Context

- Research: [03-pty-vt-rust.md](../../research/03-pty-vt-rust.md) section A (spawn/control), and Recommendation 2 (use `portable-pty`, not alacritty's bundled `tty`).
- ADR: [ADR-0010 (PTY I/O concurrency)](../../adr/0010-pty-io-concurrency.md) - blocking `portable-pty` reader on a dedicated OS thread with bounded-channel backpressure (NOT tokio); the agent subsystem (ADR-0005) runs on tokio separately and the two are intentionally not unified. See also [ADR-0007 (terminal engine)](../../adr/0007-terminal-engine.md) and [ADR-0003 (workspace + 3-thread model)](../../adr/0003-workspace-layout.md).

# Implementation notes

- Crate: `aterm-core`. New module `pty`.
- Dependency: `portable-pty = "0.9"` (pin exactly; MIT).
- Use `native_pty_system().openpty(PtySize { rows, cols, pixel_width, pixel_height })` -> `PtyPair`.
- `CommandBuilder` for the shell: launch as a true login shell (argv0 `-zsh` / `zsh -l`) per the dossier; the exact login-vs-interactive choice is owner open-question #3 in 03 - default to login shell, leave a config seam. Set `ZDOTDIR`/`ENV`/`XDG_DATA_DIRS` via `.env(...)` here is OUT OF SCOPE (T-2.2 owns the shim env); this ticket only needs the hook point.
- Expose a typed `Pty` facade: `spawn(shell, size, env) -> Pty`; `resize(PtySize)` (ioctl TIOCSWINSZ); `take_writer() -> Box<dyn Write + Send>`; `try_clone_reader() -> Box<dyn Read + Send>`; `child` handle with `process_id()`/`try_wait()`/`kill()`.
- On Unix, retain the master fd via `as_raw_fd` so T-1.9 can `killpg`/`tcsetattr`. Do not implement pgid tracking here (T-1.9).
- Resize must be debounceable by the caller (model thread); this ticket just exposes `resize`.

# Acceptance criteria

- A unit/integration test spawns `/bin/echo hello` (or `zsh -c 'print hi'`) over the PTY and reads `hello` back from the reader.
- A test resizes the PTY and asserts no error; (SIGWINCH delivery is verified indirectly in T-1.2's grid resize).
- Writing bytes to the writer is echoed back through the reader for an interactive `cat`.
- Child `kill()` followed by `try_wait()` returns an exit status; no zombie left.
- `cargo clippy -p aterm-core -- -D warnings` clean.

# Out of scope

- VT parsing / grid (T-1.2).
- The reader thread + channels (T-1.3).
- Shell-integration env injection (T-2.2).
- Signal-to-foreground-pgroup and PtyWrite reply (T-1.9).
