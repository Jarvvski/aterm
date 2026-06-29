---
id: T-5.4
epic: EPIC-5-agent-loop-safety
title: Typed tool definitions (run_command/read_file/edit_file/...)
status: done
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

# Notes

**Landed 2026-06-29.** Implemented in `aterm-agent::tools` (new module). All ACs
met; the contract is purely the typed surface (no execution/gating/sandbox), so
no UI/on-hardware residual - genuinely `done`.

Surface:

- A typed input struct per tool (`RunCommand`/`ReadFile`/`EditFile`/`WriteFile`/
  `ListDir`/`Glob`/`Grep`), each `#[serde(deny_unknown_fields)]` so the parse
  mirrors `additionalProperties: false`. `run_command.command` is a
  `Vec<String>` argv - there is deliberately NO shell-string tool (a test asserts
  `bash`/`shell`/`sh` are not registered, and that a bare-string `command`
  fails to parse).
- `ToolKind` (the discriminant) owns each tool's stable wire `name`,
  `description`, JSON-Schema `input_schema`, `parallel_safe` flag, and
  `spec()` -> `ToolSpec`. `parse_input` round-trips a raw `tool_use.input` into
  the typed `ToolInput`.
- `ToolRegistry` advertises the default 7-tool set (`specs()` for providers) and
  `parse()`s a streamed `ToolCall` back to a typed `ToolInput`, rejecting an
  unadvertised name (`ToolError::UnknownTool`) or invalid input
  (`ToolError::InvalidInput`).
- `ToolDispatch` is the turn-loop seam (the real impl - gate + sinks + sandbox -
  is T-5.9); only a test stub lives here. Held as a concrete `D: ToolDispatch`
  (mirroring `P: LlmProvider`), so the async-fn-in-trait is not dyn.
- Parallel-safety per the research table: read-only `read_file`/`list_dir`/
  `glob`/`grep` are parallel-safe; `run_command`/`edit_file`/`write_file` are
  serialized.

Decisions / divergences:

1. Extended `provider::ToolSpec` (a T-5.1 surface) with `strict: bool` - additive,
   not a relitigation of T-5.1. It carries the strict-tool-use intent to the
   provider clients; every custom typed tool sets it `true`.
2. **Strict-subset schema fix (from adversarial review).** The strict
   structured-output JSON-Schema subset rejects array-length / numeric-bound /
   string-length / `pattern` / `format` keywords, and our hand-rolled
   reqwest+serde clients have no SDK layer to strip them. The initial schemas
   carried `minItems`/`maxItems` on `read_file.range` (would 400 a strict
   request) and `minItems` on `run_command.command` (valid for Anthropic but
   rejected by OpenAI strict). Both removed; the invariants they expressed
   (argv non-empty, `range` is exactly two integers) are enforced at the parse
   layer (`range: Option<[i64; 2]>` rejects a 1-/3-element array). A guard test,
   `schemas_use_only_strict_supported_keywords`, walks every schema and fails on
   any forbidden keyword so this cannot silently regress.
3. Anthropic SERVER-side tools (`web_search`/`web_fetch`) are intentionally NOT
   modelled here - their declaration is a provider-version-specific wire type
   (e.g. `web_search_20260209`), not a neutral `input_schema` custom tool, so they
   belong to the Anthropic client (T-5.2).

19 tools-module tests (124 in `aterm-agent`, 397 workspace-wide; clippy `-D
warnings` clean). A 3-lens adversarial review surfaced 13 raw findings; 1
confirmed (the strict-subset schema defect above), fixed before landing.
Unblocks T-5.7 (sandbox keys off the typed tools) and feeds T-5.8 (turn loop) /
T-5.2/T-5.3 (advertise the specs).
