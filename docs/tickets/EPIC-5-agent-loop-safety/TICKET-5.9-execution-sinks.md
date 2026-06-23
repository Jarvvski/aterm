---
id: T-5.9
epic: EPIC-5-agent-loop-safety
title: Command-execution sinks (no-shell runner + gated PTY inject)
status: ready-for-agent
labels: [agent, tools, safety]
depends_on: [T-5.5, T-5.7, T-1.1]
---

# Goal

Implement the execution sinks the turn loop dispatches to: a no-shell subprocess runner (argv exec'd directly, no shell) for `run_command`, the filesystem tools, and the separate, harder-gated path that injects a command into the live interactive shell. All execution goes through the gate + sandbox.

# Context

- Research: [06-agent-architecture.md](../../research/06-agent-architecture.md) section (b) notes (argv not shell; the live-PTY inject is a more dangerous path scrutinized harder - any shell metachar/redirect/chaining/history-expansion/fork-bomb forces RequireConfirm even at Safe). Prototype: `CommandRunner`.

# Implementation notes

- Crate: `aterm-agent`. Module `sink`.
- `run_command`: `execvp`-style argv, NO shell, wrapped by the `Sandbox` (T-5.7). Captures stdout/stderr, runs through the sanitizer (T-5.6), returns a result for `tool_result`.
- Filesystem tools (`read_file`/`edit_file`/`write_file`/`list_dir`/`glob`/`grep`): execute with the gate's path checks; `edit_file` does exactly-one-match str-replace with a staleness check (reject if the file changed since last read); writes are atomic via a gated write helper.
- Live-PTY inject sink (optional/separate): inject an agent-proposed command into the hidden shell as a gated block (uses the T-1.1 writer). This is the more dangerous path: the gate's `SHELL_ACTIVE_REASONS` force RequireConfirm for any shell-active string even at Safe.
- Every sink call: gate decision first (T-5.5), then sandbox wrap (T-5.7), then execute, then sanitize (T-5.6).

# Acceptance criteria

- `run_command(["ls","-la"])` runs with no shell and returns sanitized output; a Safe one auto-runs under AUTO-SAFE.
- `edit_file` rejects a stale edit (file changed since read) and requires exactly one match.
- A command with shell metacharacters routed to the live-PTY inject sink forces confirmation even if otherwise Safe.
- All execution is sandbox-wrapped (verify a write outside cwd is denied, ties to T-5.7).
- Output is sanitized before return (no secret leak).

# Out of scope

- The gate/sandbox/sanitizer implementations (T-5.5/T-5.7/T-5.6) - this composes them.
- MCP tool execution (Epic 6) - routes through the same gate.
