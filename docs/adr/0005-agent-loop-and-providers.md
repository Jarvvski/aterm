# ADR-0005: Agent loop and providers - client-side manual loop, Messages API direct, multi-provider seam

## Status

Accepted

## Context

aterm is full-agentic from day one: a plan -> act -> observe -> repeat loop that can run
commands, read/edit files, and search the user's machine. The agent reads untrusted output,
so the loop's structure is also a safety surface (see
[ADR-0006](0006-safety-gate-and-sandbox.md)). The dossier
([06-agent-architecture.md](../research/06-agent-architecture.md)) established that there is
no first-party Anthropic Rust SDK, that the Claude Agent SDK ships only as Python/TS, bundles
a native Claude Code binary, and is governed by Anthropic's Commercial Terms of Service
(a conflict for a GPLv3 app), and that the loop itself is small and well-specified over the
Messages API. The prior prototype already had a working two-provider abstraction
(`AnthropicProvider` + an OpenAI Responses provider) that proved the seam.

## Decision

- **Call the LLM Messages API directly over HTTP from a thin typed Rust client**
  (`reqwest` + `tokio` + SSE + `serde`). Run the agent on a tokio runtime **off** the render
  thread; deliver SSE deltas to the UI by channel so the 60fps loop is fed deltas, not whole
  responses, and never re-lays-out the whole timeline per delta.
- **A client-side manual loop:** `POST /v1/messages`, parse `stop_reason == "tool_use"`,
  execute the tool, send `tool_result` blocks back, repeat until `end_turn`. Handle
  `end_turn`/`max_tokens`/`stop_sequence`/`tool_use`/`pause_turn`/`refusal`.
- **Default provider: Anthropic Claude `claude-opus-4-8`** with **adaptive thinking +
  the `effort` param** (`thinking: {type: "adaptive"}`, `output_config: {effort: "high"}`) -
  NOT `budget_tokens` (removed; 400s). Stream over SSE; loop on `stop_reason: "tool_use"`.
  Pin `anthropic-version: 2023-06-01`.
- **MULTI-PROVIDER seam in v1:** one `LlmProvider` trait with `AnthropicProvider` AND
  `OpenAiProvider` (OpenAI uses the Responses API), behind a provider-neutral event mapper
  and one shared turn loop. The mapper normalizes each provider's streaming events into one
  internal event type so the turn loop is provider-agnostic. This mirrors the prior prototype.
- **Reject the Agent SDK.** No Rust binding; it is Claude Code as a library (bundles/shells
  out to a native binary, IPC-marshalling per step threatens the frame floor); it is
  Commercial-Terms-licensed and conflicts with GPLv3; and every capability it adds (the tool
  loop, built-in tools, hooks, sub-agents, MCP, sessions) is a thin layer over the Messages
  API that aterm's differentiators (the deterministic gate, single Secrets source, OSC-133
  timeline) require us to own anyway.
- **Managed Agents is OUT OF SCOPE.** Its tools run in Anthropic's container; aterm must run
  commands on the *user's* machine.
- Tools are typed custom tools (`run_command` takes an argv `string[]`, no shell;
  `read_file`/`edit_file`/`list_dir`/`glob`/`grep`), each gated before execution
  ([ADR-0006](0006-safety-gate-and-sandbox.md)). No bare bash tool.

## Consequences

- aterm owns a hand-rolled, GPLv3-clean LLM client - which is the whole point, since the
  product's value lives in the loop layer (gate, Secrets, sanitizer, timeline, aesthetic).
- The multi-provider seam costs a small abstraction (one trait, one event mapper, one shared
  loop) up front but matches the proven prototype and avoids re-architecting if Anthropic-only
  ever proves insufficient. Provider-specific features (Anthropic adaptive thinking, the MCP
  connector) are accessed through the Anthropic implementation, not the lowest common
  denominator.
- We accept the maintenance burden of tracking API drift (beta headers, server-tool version
  bumps). Mitigation: thin client typed against a small, stable surface; pin the API version;
  treat community crates as reference only, never a dependency.
- Streaming off the render thread is a direct service of the 60fps floor.
- The turn loop is also the safety choke point: every `tool_use` flows through the
  deterministic gate before execution ([ADR-0006](0006-safety-gate-and-sandbox.md)).

## Alternatives considered

- **Embed the Claude Agent SDK.** Rejected on four independent axes: no Rust binding, bundles
  a Claude Code binary (latency-variable child-process IPC per step), Commercial-Terms vs
  GPLv3 conflict, and it owns the exact layer aterm differentiates in.
- **Managed Agents.** Rejected: server-side container execution cannot run commands on the
  user's machine, which is the core requirement.
- **A community Rust Anthropic crate as the client.** Rejected as a dependency (none is
  decision-grade for adaptive thinking / 2026 server-tool versions / the MCP connector); used
  only as reference for the SSE parser and retry/backoff.
- **Anthropic-only for v1.** Rejected by the locked decision in favor of the multi-provider
  seam, which the prototype already validated and which keeps OpenAI a first-class option.
