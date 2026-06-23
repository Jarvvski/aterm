---
id: T-5.2
epic: EPIC-5-agent-loop-safety
title: AnthropicProvider (Messages API, SSE, adaptive thinking)
status: ready-for-agent
labels: [agent, llm, anthropic]
depends_on: [T-5.1]
---

# Goal

Implement the thin typed Anthropic Messages-API client behind `LlmProvider`: HTTP over reqwest+tokio, SSE streaming, `claude-opus-4-8` with adaptive thinking + the effort param, parsing tool_use and feeding tool_result back. The default provider.

# Context

- Research: [06-agent-architecture.md](../../research/06-agent-architecture.md) section (a) verified API facts, Recommendations 1-3, 8. Locked: call the Messages API directly (reject the Agent SDK - Commercial-Terms/GPLv3 conflict); use `claude-opus-4-8` with `thinking:{type:"adaptive"}` + `output_config:{effort:"high"}` (NOT `budget_tokens`); stream SSE; loop on `stop_reason:"tool_use"`.
- BEFORE implementing, load the `claude-api` skill to confirm current model ids, headers, thinking/effort params, and SSE event shapes - do not rely on memory.

# Implementation notes

- Crate: `aterm-agent`. Module `provider::anthropic`.
- Dependencies: `reqwest` (rustls), `tokio`, `serde`/`serde_json`, an SSE line parser (`eventsource-stream` or hand-rolled over reqwest's byte stream). Pin versions.
- `POST https://api.anthropic.com/v1/messages`, `x-api-key`, header `anthropic-version: 2023-06-01` (pinned). Own typed request/response structs for the small stable surface; community crates are reference only, not a dependency.
- Tool definitions: `{name, description, input_schema}` + `"strict": true` sibling; `tool_choice` `{type:"auto"}` default with `disable_parallel_tool_use` available. Tools are defined in T-5.4; this client serializes them.
- Stream `"stream": true`: parse `message_start` -> `content_block_start`/`delta`/`stop` -> `message_delta` (stop_reason, usage) -> `message_stop`, mapping each to `ProviderEvent` (T-5.1). Handle `pause_turn` by re-sending to resume.
- Thinking: `thinking:{type:"adaptive", display:"summarized"}` + `output_config:{effort:"high"}`. Echo thinking blocks back unchanged on the same model when continuing a turn.
- Run on a tokio runtime OFF the render thread; events land on the UI via channel.

# Acceptance criteria

- Against a mocked HTTP server, a streamed response with a tool_use block is parsed into the correct `ProviderEvent` sequence.
- A `tool_result` round-trip continues the turn (assert the follow-up request body shape: one user message with all tool_result blocks; failed tool -> `is_error:true`).
- `pause_turn` triggers a resume re-send, not a "continue" message.
- Adaptive thinking + effort are set; `budget_tokens` is never sent.
- The client compiles with no dependency on a window/UI.

# Out of scope

- OpenAI (T-5.3), the turn loop (T-5.8), tool execution (T-5.9).
- API-key custody (T-8.3) - the client takes a key from a provided source.
