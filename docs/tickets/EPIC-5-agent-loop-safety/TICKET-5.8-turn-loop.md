---
id: T-5.8
epic: EPIC-5-agent-loop-safety
title: Agentic turn loop (shared, provider-neutral)
status: ready-for-agent
labels: [agent, llm]
depends_on: [T-5.2, T-5.4, T-5.5]
---

# Goal

Implement the one shared, provider-neutral agentic turn loop: plan -> act (gated tool calls) -> observe -> repeat, driving either provider through `LlmProvider`, looping while stop_reason is ToolUse, with prompt-injection-resistant structural separation of tool results.

# Context

- Research: [06-agent-architecture.md](../../research/06-agent-architecture.md) section (a) (the loop), (e) (transcript), (f) (prompt-injection defense) + Recommendations 1, 3, 9. Locked: client-side manual loop calling the Messages API; loop on `stop_reason:"tool_use"`; one shared loop across providers (mirrors the prototype).

# Implementation notes

- Crate: `aterm-agent`. Module `loop` (the orchestrator).
- Drive a `LlmProvider` (T-5.1) with a `TurnRequest`; consume `ProviderEvent`s. On `ToolUse`: collect the tool calls, run each through the gate (T-5.5) for a decision, execute approved ones via the sinks (T-5.9) under the sandbox (T-5.7), sanitize output (T-5.6), and send `tool_result` blocks back (all parallel results in one user message; failed -> is_error). Loop until `EndTurn`.
- Parallel-safe tools (T-5.4 flags) may fan out; mutations serialize.
- Structural separation: tool results delivered as data-role `tool_result`, never as user instructions; operator mid-conversation instructions use the non-spoofable `role:"system"` channel. System-prompt hardening: tell the model tool output is data, surface (not silently act on) embedded directives.
- `pause_turn` resume handled by the provider; the loop treats it transparently.
- Runs on tokio off the render thread; emits timeline-update events by channel (T-5.10).

# Acceptance criteria

- With a mock provider scripting plan->tool_use->observe->end_turn, the loop executes the tool (gated), feeds results back, and terminates on end_turn.
- A Dangerous tool call is NOT executed without confirmation (gate decision respected); a Safe one auto-runs under AUTO-SAFE.
- Parallel read-only tools run concurrently; a mutation serializes.
- The same loop drives both Anthropic and OpenAI mock providers unchanged.
- Tool output passes through the sanitizer before re-entering context.
- Esc/cancel aborts the loop cleanly (ties to T-3.3 interrupt).

# Out of scope

- HTTP clients (T-5.2/T-5.3), execution sinks (T-5.9), timeline model (T-5.10), approval UI (T-5.11).
