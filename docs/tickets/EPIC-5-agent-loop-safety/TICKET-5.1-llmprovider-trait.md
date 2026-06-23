---
id: T-5.1
epic: EPIC-5-agent-loop-safety
title: LlmProvider trait + provider-neutral event model
status: ready-for-agent
labels: [agent, llm]
depends_on: []
---

# Goal

Define the `LlmProvider` trait and the provider-neutral streaming-event model that both Anthropic and OpenAI providers implement, plus a provider-neutral event mapper - the seam that lets one shared turn loop drive either backend. This is a LOCKED v1 requirement (multi-provider seam), mirroring the prior prototype.

# Context

- Research: [06-agent-architecture.md](../../research/06-agent-architecture.md) section (a) (own the loop) + open-question #6 (multi-provider). Locked decision: MULTI-PROVIDER seam in v1 - AnthropicProvider AND OpenAiProvider behind one `LlmProvider` trait, a provider-neutral event mapper, one shared turn loop. Provider default: Anthropic Claude (`claude-opus-4-8`); OpenAI uses the Responses API.

# Implementation notes

- Crate: `aterm-agent`. Module `provider`.
- Define (confirm exact signatures before coding per the interface-design rule):
  - `trait LlmProvider`: `async fn stream_turn(&self, req: TurnRequest) -> impl Stream<Item = ProviderEvent>` (or a channel of events), with provider-specific config injected at construction (model id, effort, thinking).
  - A provider-neutral `TurnRequest` (messages, tools, system, tool_results) and `ProviderEvent` enum covering: `MessageStart`, `TextDelta`, `ThinkingDelta`, `ToolUseStart{id,name}`, `ToolUseInputDelta`, `ToolUseStop`, `MessageDelta{stop_reason, usage}`, `MessageStop`, `Error`.
  - Map provider stop-reasons to a neutral set: `EndTurn`, `MaxTokens`, `StopSequence`, `ToolUse`, `PauseTurn` (re-send to resume, do NOT inject "continue"), `Refusal`.
- The event mapper translates Anthropic SSE events and OpenAI Responses events into `ProviderEvent`. Keep the neutral surface small and stable.
- No network code here - just the trait + types + mapper interfaces. Heavily unit-testable, no window.

# Acceptance criteria

- The trait + event model compile in `aterm-agent` with no dependency on `aterm-ui`/window.
- A mock provider yields a scripted `ProviderEvent` stream that the (future) turn loop can consume.
- Stop-reason mapping is exhaustively unit-tested for both providers' raw reasons.
- The neutral event model carries enough to render the timeline (text, thinking, tool calls, usage) without provider-specific leakage.

# Out of scope

- The Anthropic/OpenAI HTTP implementations (T-5.2, T-5.3).
- The turn loop (T-5.8).
