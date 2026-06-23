---
id: T-5.4
epic: EPIC-5-agent-loop-safety
title: Typed tool definitions (run_command/read_file/edit_file/...)
status: ready-for-agent
labels: [agent, tools]
depends_on: [T-5.1]
---

# Goal

Define the typed custom tool set (argv, never a shell string) with JSON Schema input, parallel-safety flags, and the dispatch seam - so the harness gets structured args it can gate, render, audit, and parallelize. No bare bash tool.

# Context

- Research: [06-agent-architecture.md](../../research/06-agent-architecture.md) section (b) (tool-set table) + Recommendation 5. Locked: tools are `run_command/read_file/edit_file/list_dir/glob/grep`; argv not shell; every call gated.

# Implementation notes

- Crate: `aterm-agent`. Module `tools`.
- Define each tool with a typed Rust struct for its input + a generated/maintained `input_schema` (JSON Schema), `strict: true`:
  - `run_command { command: string[] (argv), cwd?: string }` - NOT parallel-safe (serialized).
  - `read_file { path, range? }` - parallel-safe.
  - `edit_file { path, old_str, new_str }` (exactly-one-match str-replace; staleness check) - NOT parallel-safe.
  - `write_file { path, content }` - NOT parallel-safe.
  - `list_dir { path }` / `glob { pattern, root? }` / `grep { pattern, path?, flags? }` - parallel-safe, read-only.
- Each tool carries a `parallel_safe: bool` so the scheduler can fan out read-only tools and serialize mutations.
- A `ToolRegistry` exposes the definitions to providers (T-5.2/T-5.3) and a dispatch trait the turn loop (T-5.8) calls. Actual execution lives in the sinks (T-5.9); gating in T-5.5; this ticket defines the contracts.
- Server-side tools (web_search/web_fetch) are declared but executed by the provider; their output is untrusted (prompt-injection defense in the loop/sanitizer).

# Acceptance criteria

- Every tool serializes to a valid JSON Schema with `strict: true` accepted by both providers.
- `run_command` input is a `string[]` argv; there is no shell-string tool.
- Parallel-safety flags are correct per the table.
- The registry round-trips: a provider tool_use with a given name+input deserializes to the typed struct.
- Dispatch is a trait the turn loop can call with a gate + sink injected.

# Out of scope

- Risk classification (T-5.5), execution (T-5.9), sandbox (T-5.7).
