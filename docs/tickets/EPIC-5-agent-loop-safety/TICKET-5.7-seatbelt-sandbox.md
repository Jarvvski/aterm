---
id: T-5.7
epic: EPIC-5-agent-loop-safety
title: Seatbelt sandbox (sandbox-exec) + setrlimit + timeout-kill
status: done
labels: [agent, safety, sandbox, macos]
depends_on: [T-5.4]
---

# Goal

Implement the mandatory OS-level boundary for agent-run commands: a macOS Seatbelt sandbox via `sandbox-exec` with a generated `.sb` profile, behind a `Sandbox` trait, plus `setrlimit` resource limits and process-group timeout-kill. Because the AUTO-SAFE default enlarges the trust surface, this sandbox is MANDATORY, not optional.

# Context

- Research: [06-agent-architecture.md](../../research/06-agent-architecture.md) section (d).3 + Recommendation 6 + Risk list. Locked: Seatbelt via `sandbox-exec` (deprecated but the only documented mechanism; Anthropic's own sandbox-runtime uses it) behind a `Sandbox` trait, plus `setrlimit` + timeout-kill. Owner open-question #2 (network egress policy: deny-all+allowlist vs allow+proxy-log) drives the profile - default deny-all + allowlist; flag if unconfirmed.

# Implementation notes

- Crate: `aterm-agent`. Module `sandbox`.
- `trait Sandbox { fn wrap(&self, cmd: Command, profile: SandboxProfile) -> Command }` so a future native-API/VM backend can replace `sandbox-exec`.
- `SeatbeltSandbox` implementation: generate a `.sb` profile that restricts filesystem writes to the project/cwd, denies reads of the secret paths (from T-5.6), and filters network egress (deny-all + allowlist by default). Wrap the subprocess with `sandbox-exec -f <profile>`.
- Resource limits regardless of Seatbelt: `setrlimit` (CPU time, address space, open files) and a process-group kill on timeout. Dependency: `nix` for setrlimit/killpg.
- This wraps the execution sinks (T-5.9); every agent-run command goes through it.

# Acceptance criteria

- A command run under the sandbox cannot write outside the project/cwd (assert a write to `/tmp` or `$HOME` is denied).
- A command cannot read a secret path (assert `cat ~/.ssh/id_rsa` is blocked by the profile).
- Network egress to a non-allowlisted host is denied by default.
- A runaway command is killed at the timeout (process group reaped, no orphans).
- The `Sandbox` trait abstracts the mechanism (a no-op/test backend swaps in for unit tests).
- Resource limits apply even if the profile is minimal.

# Out of scope

- The gate/classifier (T-5.5) - this is the boundary beneath it.
- The actual sinks (T-5.9) - this wraps them.

# Notes

**Done 2026-06-30 (agent).** Module `aterm-agent::sandbox`, built directly against this
machine's real `sandbox-exec` (the SBPL semantics below were empirically verified on macOS
15.7, not assumed), then put through a 4-lens adversarial review whose confirmed findings are
folded in. `nix` added target-gated `[cfg(unix)]` (`resource` + `signal`).

- **`SeatbeltSandbox`** generates the `.sb` profile: `(allow default)` base + three clamps -
  (1) `(deny file-write*)` then re-allow only the cwd / `policy.writable_paths` (+ `/dev/null`
  `/dev/stdout` `/dev/stderr`; `/dev/tty` deliberately excluded - TTY-injection vector); (2)
  deny **reads AND writes** of the credential paths, sourced from the *single* `Secrets`
  deny-set (so a secret can be neither exfiltrated nor clobbered, even one inside the writable
  tree - the single-source invariant extended to the kernel for both directions); (3)
  `(deny network-outbound)` keeping only local-IPC unix sockets (TCP loopback is NOT
  re-allowed - it is an exfil-to-local-proxy channel), punchable by `policy.network_allowlist`.
  Substring patterns become case-insensitive `(regex #"[Aa]..")` predicates (Seatbelt's regex
  is case-sensitive and ignores `(?i)`; macOS FS is case-insensitive). `(allow default)` is the
  *fallback*, so the deny clauses hold regardless of position.
- **`SandboxRunner`** is the concrete confined runner (the chosen scope): wraps argv via the
  `Sandbox`, installs `setrlimit` caps in a `pre_exec` (verified to survive `sandbox-exec`'s
  in-place exec), runs in its own process group, single-reaper poll loop, `killpg` SIGKILL on
  the wall-clock timeout (no orphans), captures stdout/stderr via shared-buffer reader threads
  with a **bounded drain** (a descendant that `setsid`s out of the group while holding a pipe
  cannot hang the runner). `ResourceLimits` (CPU 30s / AS 2 GiB advisory-on-macOS / 256 fds),
  `ConfinedOutput`, `NoSandbox` test backend.

**Acceptance criteria - all met:**
- AC1 (no write outside cwd) - `seatbelt_confines_writes_to_cwd` (real Seatbelt; cwd write OK,
  `$HOME`/`/tmp` denied). Plus the write-deny mirror: `seatbelt_denies_writing_a_secret_inside_the_cwd`.
- AC2 (cannot read a secret) - `seatbelt_denies_reading_a_secret_path` (`cat .ssh/id_rsa` denied).
- AC3 (egress denied by default) - `seatbelt_denies_egress_to_a_reachable_host` (canary-gated so
  a pass means the profile blocked it, not a missing route / parse error) + profile-content test.
- AC4 (runaway killed at timeout, group reaped, no orphans) - `runner_kills_a_runaway_group_at_the_timeout_no_orphans`
  (grandchild pid proven dead) + `runner_does_not_hang_when_a_backgrounded_job_holds_the_pipe`.
- AC5 (trait abstracts the mechanism) - `NoSandbox` swaps in for every runner-mechanism test.
- AC6 (limits apply even with a minimal profile) - `runner_applies_setrlimit_in_the_child` +
  `seatbelt_applies_resource_limits_through_sandbox_exec` (combined real path; `ulimit -n` == set).

25 sandbox tests (13 portable profile/helper + 4 Unix runner-mechanism + 6 macOS real-Seatbelt
+ 2 misc); `mise run fmt && lint && build && test` green, clippy clean at `-D warnings`.

**Flagged (owner open-question #2, network egress):** the default is deny-all + IP allowlist
per the dossier; allow+proxy-log was not chosen. The allowlist is best-effort (Seatbelt sees
IPs, not hostnames). Unconfirmed by the owner - defaulted + flagged per the ticket.

**Residual:** wiring this runner into the agent's `run_command` tool is T-5.9 (this lands the
boundary; T-5.9 wraps the sink around it). The INDEX T-5.7 row flip was deferred to avoid
colliding with unrelated in-flight backlog edits in `INDEX.md`.
