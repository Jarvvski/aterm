---
id: T-5.7
epic: EPIC-5-agent-loop-safety
title: Seatbelt sandbox (sandbox-exec) + setrlimit + timeout-kill
status: ready-for-agent
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
