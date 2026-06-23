---
id: T-5.3
epic: EPIC-5-agent-loop-safety
title: OpenAiProvider (Responses API)
status: ready-for-agent
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
