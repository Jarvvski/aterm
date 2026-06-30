---
id: T-5.9
epic: EPIC-5-agent-loop-safety
title: Command-execution sinks (no-shell runner + gated PTY inject)
status: done
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

# Notes

Landed: `crates/aterm-agent/src/sink.rs` (`CommandSink`, `FileSink`, `PtyInjectSink`,
and the `Sinks` `ToolDispatch` the turn loop holds, plus 20 unit + end-to-end tests),
`lib.rs` re-exports, `CHANGELOG.md`. Gate green (fmt / clippy `-D warnings` / build /
full workspace test - 220 aterm-agent tests).

AC coverage:

- **AC1** - `run_command` execs the argv directly with NO shell (`CommandSink::run`
  builds a `ConfinedCommand` and runs it through the `SandboxRunner`); the no-shell
  property is proven by echoing a literal `$HOME` argv token and asserting it is NOT
  expanded. The AUTO-SAFE auto-run + real execution is proven end to end through the
  actual turn loop (`safe_run_command_auto_runs_and_executes_through_the_loop`): a Safe
  command never reaches the approver and its captured output is fed back.
- **AC2** - `edit_file` rejects a stale edit via a content-hash baseline recorded on
  read (`edit_file_rejects_a_stale_edit`: read -> external change -> edit refused, file
  untouched, then re-read -> same edit succeeds) and enforces exactly-one-match (0 =
  "not found", >1 = "ambiguous"); writes are atomic (temp file + rename).
- **AC3** - `PtyInjectSink` is the separate harder-gated path: `gate()` forces confirm
  for any shell-active command, and `gate_assessment()` independently refuses to
  auto-inject a *synthetic Safe-but-shell-active* assessment (the literal "even if
  otherwise Safe" case), proving the harder gate is not merely the classifier's level.
- **AC4** - the file sink confines every write to the workspace root by canonicalizing
  the parent (defeats `..` and symlinked-parent escapes); an absolute-outside / `..`
  write is denied and the file is not created (`write_outside_the_root_is_denied`,
  portable). The `run_command` subprocess write-confinement is the Seatbelt boundary
  validated by T-5.7's own macOS tests.
- **AC5** - the sink refuses to read/scan any path in the single `Secrets` deny-set, so
  a credential file's *contents* never enter the result (`file_sink_refuses_to_read_a_secret_path`,
  `grep_skips_secret_files...`); secret *values* that do appear are redacted by the
  turn loop before re-entering context, proven end to end
  (`raw_sink_output_is_sanitized_before_it_re_enters_context`).

Design reconciliation (flagged, not a silent override): the ticket says "every sink
call: gate -> sandbox -> execute -> sanitize". The landed turn loop (T-5.8) already owns
the FIRST gate + confirmation and the output **sanitize** (per the `ToolOutcome`
contract: the dispatcher returns RAW output, the loop sanitizes against the same
`Secrets`). So T-5.9's sinks own the *execution-side* enforcement: the **sandbox wrap**
for `run_command` (the gate+confirm having happened upstream), and the **in-process path
checks** for the file tools (which never hit the sandbox subprocess, so the sink itself
is the boundary - secret-path refusal + write-to-root confinement). This matches
`turn.rs`'s own note that "the file sink re-gates a sensitive-path read as defense in
depth, T-5.9". The sinks do NOT double-sanitize (that would contradict the locked
`ToolOutcome` contract and T-5.8's tests).

Residuals (recorded follow-ups, not silently shipped):

1. `grep` is a LITERAL substring search in v1 (case-insensitive with an `i` flag), not a
   full regular-expression engine - aterm pulls in no regex dependency yet, though the
   tool schema advertises a "regular-expression pattern". It returns real matches and is
   secret-path-gated; full regex is a follow-up.
2. `PtyInjectSink` and the `Sinks` dispatcher are implemented + tested but not yet
   CONSTRUCTED/WIRED into `aterm-app` (no UI path builds them yet). Wiring the dispatcher
   into the app's agent turn and the inject sink into the unified-input live-shell route
   rides the app-integration / approval-UX work (T-5.11).
