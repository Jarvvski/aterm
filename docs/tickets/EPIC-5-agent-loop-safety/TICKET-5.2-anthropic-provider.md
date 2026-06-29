---
id: T-5.2
epic: EPIC-5-agent-loop-safety
title: AnthropicProvider (Messages API, SSE, adaptive thinking)
status: done
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

# Notes

**Landed 2026-06-29.** Real Messages-API client in `aterm-agent::provider::anthropic`
(`provider.rs` declares `pub mod anthropic`; the OpenAI stub stays in `provider.rs`
for T-5.3). All five ACs met with headless tests; no on-hardware/visual residual,
so genuinely `done`. No real network in any test - the pure SSE pieces are driven
from byte fixtures and the HTTP path from a loopback `std::net` mock server (never
`api.anthropic.com`).

Surface:

- `build_body` serializes a neutral `TurnRequest` into the wire JSON: `model`,
  required `max_tokens`, `stream:true`, `messages`, `thinking:{type:"adaptive",
  display:"summarized"}`, `output_config:{effort}`, optional top-level `system`,
  and (only when tools are present) `tools` with `strict:true` as a SIBLING of
  name/description/input_schema plus `tool_choice:{type:"auto"}`. `budget_tokens`
  is NEVER emitted (a guard test asserts the whole serialized body lacks it).
- SSE decode is split into pure, testable pieces: `SseDecoder` (a byte-buffering
  event framer) and `StreamState` (the SSE-event -> `ProviderEvent` reducer that
  also reconstructs the assistant `content` array for resume). The neutral
  `ProviderEvent` sequence from T-5.1 is preserved exactly; the shared
  `AgentEventMapper` is unchanged.
- `pause_turn` is resumed by RE-SENDING with the accumulated assistant content
  appended as an assistant message (NOT a synthetic "continue" user message),
  bounded by `max_resumes`; the paused hop's `MessageDelta`/`MessageStop` and the
  resumed hop's duplicate `MessageStart` are suppressed so the consumer sees ONE
  continuous turn.
- HTTP errors map to `ProviderError`: 401/403 -> `Auth`, other non-2xx -> `Http`,
  SSE JSON parse failure -> `Decode`. `Debug` for the provider redacts the key.

Decisions / divergences (additive extensions to T-5.1's neutral surface, same
pattern as T-5.4's `ToolSpec.strict` - not a relitigation):

1. `Message.content` `String` -> `Vec<ContentBlock>`, with a new `ContentBlock`
   enum (`Text`/`ToolUse`/`ToolResult`) and `Message::{user,assistant,system,
   assistant_blocks,tool_results}` constructors. The thin string content could not
   carry the `tool_use`/`tool_result` blocks (keyed by id) that AC #2 requires.
   Low-churn: `Message` was only ever constructed as `messages: vec![]`.
2. Added `TurnRequest.max_tokens: u32` (the Messages API requires `max_tokens`;
   OpenAI will map it to `max_output_tokens`) and `Effort::as_str()`.
3. The Anthropic client owns PRIVATE wire mapping (`build_body`/`wire_message`/
   `wire_block`); the neutral `TurnRequest`/`Message`/`ContentBlock` stay
   provider-agnostic so T-5.3 reuses them.

Adversarial review (3 lenses, find -> verify): 9 raw findings, 2 distinct
confirmed and fixed before landing - (a) per-chunk `from_utf8_lossy` corrupted a
multibyte codepoint split across a `reqwest::chunk()` boundary (incl. tool-input
JSON); fixed by buffering RAW BYTES and decoding only complete blank-line-delimited
event blocks (regression test splits a chunk mid-codepoint). (b) token usage reset
each pause/resume hop; fixed by folding `output_tokens` across hops while keeping
the first hop's input/cache count (a resume re-sends context, so summing input
would double-count) - asserted in the pause test.

15 anthropic-module tests (141 in `aterm-agent`, 414 workspace-wide; clippy `-D
warnings` clean). No version bump / CHANGELOG: internal plumbing, providers not yet
wired into the running app (turn loop is T-5.8, app wiring T-5.9+), no user-visible
behavior. Unblocks T-5.8 (turn loop) and T-6.1 (MCP connector).
