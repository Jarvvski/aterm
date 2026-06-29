---
id: T-5.3
epic: EPIC-5-agent-loop-safety
title: OpenAiProvider (Responses API)
status: done
labels: [agent, llm, openai]
depends_on: [T-5.1]
---

# Goal

Implement the second `LlmProvider`: a thin typed OpenAI client using the Responses API, streaming, with tool-calling mapped into the provider-neutral event model - so the multi-provider seam is real in v1, not theoretical.

# Context

- Research: [06-agent-architecture.md](../../research/06-agent-architecture.md) open-question #6 + the prototype's `OpenAiResponsesProvider`. Locked: OpenAI uses the Responses API; both providers sit behind one `LlmProvider` trait with a shared turn loop.

# Implementation notes

- Crate: `aterm-agent`. Module `provider::openai`.
- Use the Responses API (`POST /v1/responses`), streaming, with function/tool calling. Own typed request/response structs (community crates reference-only).
- Map streamed events + tool calls into `ProviderEvent` (T-5.1) and stop-reasons into the neutral set. Where OpenAI lacks an Anthropic concept (e.g. summarized thinking), map to the closest neutral variant or omit cleanly - the neutral model is the lowest-common-denominator that still renders the timeline.
- Same off-thread tokio + channel discipline as the Anthropic client.
- Tools are the same typed definitions (T-5.4) re-serialized to OpenAI's schema.

# Acceptance criteria

- Against a mocked server, a streamed Responses reply with a tool call parses into the correct `ProviderEvent` sequence.
- Tool-result continuation produces a correctly-shaped follow-up request.
- The same `TurnRequest` (T-5.1) drives both providers without provider-specific branching in the turn loop (T-5.8).
- Provider selection is config-driven; default remains Anthropic.

# Out of scope

- The turn loop (T-5.8) and tool execution (T-5.9).
- MCP connector (Anthropic-specific; T-6.1).

# Notes

**Landed 2026-06-29.** The real Responses-API client lives in `aterm-agent::provider::openai`
(`OpenAiProvider`), replacing the `NotImplemented` stub. It sits behind the unchanged
`LlmProvider` trait and translates the Responses SSE stream into the SAME neutral
`ProviderEvent` sequence the shared mapper folds, so the turn loop (T-5.8) drives it
with no provider-specific branching. No real network in any test (byte fixtures +
a loopback `std::net` mock server, never `api.openai.com`).

Surface / mapping:

- `build_body` -> `POST /v1/responses`, `stream:true`, `store:false` (we own the loop
  and re-send the full transcript each turn, mirroring the Anthropic client; tool
  calls thread purely by `call_id`). Neutral system prompt -> top-level
  `instructions`; `max_tokens` -> `max_output_tokens`; the effort knob ->
  `reasoning.effort` (+ `summary:"auto"` for summarized reasoning) - NEVER
  `budget_tokens` (guard test asserts it is absent). Tools are FLAT function defs
  (`type`/`name`/`description`/`parameters`/`strict`), not the Chat-Completions
  nested shape; `tool_choice:"auto"`.
- Input items: `input_text` for user / `developer` (the operator channel) messages,
  `output_text` for echoed assistant text, a `function_call` item (with `call_id`,
  `name`, arguments-as-STRING) per echoed tool call, and one `function_call_output`
  item per tool result keyed by `call_id`. A neutral assistant message carrying text
  + tool calls expands to a text item PLUS one `function_call` item each; a
  tool-results message expands to N flat `function_call_output` items (the Responses
  API has no "one message, many results" shape). `ToolResult.is_error` has no
  Responses wire field - the turn loop already folds the error text into `content`,
  which rides in `output`.
- Stream: `response.created` -> `MessageStart`; `response.output_text.delta` ->
  `TextDelta`; `response.reasoning_summary_text.delta` -> `ThinkingDelta` (closest
  neutral analog of summarized thinking); a `function_call` output item ->
  `ToolUseStart` / `ToolUseInputDelta` / `ToolUseStop`; `response.completed` /
  `response.incomplete` -> one `MessageDelta` (stop reason + usage) then
  `MessageStop`; `response.failed` / top-level `error` -> `Error`. Usage maps
  `input_tokens`/`output_tokens` and `input_tokens_details.cached_tokens` ->
  `cache_read_input_tokens` (OpenAI has no cache-creation counter).

`StopReason` precedence in `terminal_reason` (refusal > completed-tool-call >
incomplete-reason > status) is the key nuance: the Responses API signals a tool turn
via output items, not a stop string, so a `completed` turn that emitted a fully-closed
function call maps to `ToolUse`.

Shared-framing refactor (additive, no surface change): the pure SSE byte-framer
(`SseDecoder` + helpers, incl. the multibyte-split correctness) was extracted from
`anthropic.rs` into `provider::sse` (`pub(crate)`) and is now reused by both clients;
the 3 decoder tests moved with it (+1 multi-line-data test).

Adversarial review (3 lenses: wire-shape fidelity, event-mapping/control-flow,
neutrality/safety; find -> verify): 8 raw findings, 3 confirmed and fixed before
landing -
(1) MEDIUM: `Effort::Xhigh`/`Max` were clamped to `"high"`, but `xhigh` is the
documented Responses `reasoning.effort` ceiling (not `high`) - now `Xhigh` -> `"xhigh"`
1:1 and the analog-less `Max` clamps to `"xhigh"`.
(2) MEDIUM: a fully-closed function call followed by a later-item token cap
(`response.incomplete`/`max_output_tokens`) reported `MaxTokens`, so the turn loop
would drop a usable tool call - now a turn with any CLOSED tool call (a
`function_call_arguments.done` was seen) maps to `ToolUse` (regression test
`completed_tool_call_then_incomplete_still_maps_to_tool_use`).
(3) LOW: an authored content-policy refusal arrives as `response.completed` with a
refusal part (status stays `completed`), so it mapped to `EndTurn` not `Refusal` - now
`response.refusal.delta`/`.done` surface the prose as text and flag the turn as
`Refusal` (parity with the Anthropic path; regression test
`authored_refusal_maps_to_refusal_reason`). The other 5 findings were refuted on
verification.

AC coverage: (1) loopback-server tool-use parse + (2) tool-result-continuation request
shape + (5/mapping) all covered by headless tests. (3) "same `TurnRequest` drives both
providers with no branching" is structural - both impl `LlmProvider` and the shared
loop (T-5.8) already drove two `MockProvider` identities unchanged. (4) "provider
selection is config-driven, default Anthropic" is an app/config concern with no code in
this crate - deferred to config wiring (T-8.3); `AnthropicProvider` remains the default
provider (`claude-opus-4-8`), `OpenAiProvider` defaults to `gpt-5`.

Accepted scope cut (same as the Anthropic thinking-signature cut): the neutral event
stream carries no reasoning-item id, so OpenAI reasoning items are not threaded back on
re-send across tool rounds; revisit when the live loop ships (T-5.9+).

22 openai-module tests (177 in `aterm-agent`, 450 workspace-wide; clippy `-D warnings`
clean). No version bump / CHANGELOG: internal plumbing, not yet wired into the running
app, no user-visible behavior. Completes the locked v1 multi-provider seam.
