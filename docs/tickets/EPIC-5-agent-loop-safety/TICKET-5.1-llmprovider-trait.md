---
id: T-5.1
epic: EPIC-5-agent-loop-safety
title: LlmProvider trait + provider-neutral event model
status: done
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

# Notes

**Landed 2026-06-29.** Contract confirmed with the owner before coding (the
three load-bearing choices below), then implemented in `aterm-agent::provider`
(rewrite) with the ripple into `turn.rs` + `lib.rs`. All ACs met; no UI/render
residual, so genuinely `done`.

Two-layer event model:

- **`ProviderEvent`** (low-level, mirrors an Anthropic Messages SSE stream 1:1):
  `MessageStart`, `TextDelta`, `ThinkingDelta`, `ToolUseStart{id,name}`,
  `ToolUseInputDelta{json}`, `ToolUseStop`, `MessageDelta{stop_reason,usage}`,
  `MessageStop`, `Error`. Each concrete provider (T-5.2/5.3) owns the SSE ->
  `ProviderEvent` translation; nothing provider-specific leaks past it.
- **`AgentEvent`** (timeline-facing) produced by the shared **`AgentEventMapper`**
  reducer, which buffers each tool call's streamed input JSON and emits one
  complete `ToolProposed(ToolCall{id,name,input})` per call. `TurnComplete`
  carries the `stop_reason` so the turn loop (T-5.8) knows whether to loop.

Confirmed contract decisions (owner-approved):

1. Granular SSE-aligned `ProviderEvent` + `AgentEvent` + `AgentEventMapper`
   (replacing the scaffold's coarse `ProviderDelta`).
2. `StopReason` = closed set {EndTurn, MaxTokens, StopSequence, ToolUse,
   PauseTurn, Refusal} **+ `Other(String)`** for forward-compat. Pure
   `from_anthropic` / `from_openai` mappers, exhaustively tested (satisfies the
   "both providers' raw reasons" AC without the HTTP clients).
3. Added a scriptable **`MockProvider`**; kept `AnthropicProvider`/`OpenAiProvider`
   as `NotImplemented` stubs (real clients are T-5.2/5.3).

Deliberate divergences from the Kotlin prototype (per aterm's locked decisions):
the prototype is SINGLE-tool (`propose_command` -> `CommandProposal`); aterm is
locked MULTI-TOOL, so the mapper stays generic and hardcodes no tool name.
Thinking deltas ARE modeled here (the prototype dropped them) because the AC
requires the timeline to render thinking. `Effort` extended to
`Low/Medium/High/Xhigh/Max` to match Opus 4.8's `output_config.effort` (NOT
`budget_tokens`).

Mapper edge cases covered by tests: streamed-JSON reassembly; an unterminated
tool flushed at `MessageStop`; empty-args -> `{}` (not an error); malformed JSON
-> surfaced `AgentEvent::Error` (NOT a silent drop, a divergence from the
prototype); a dropped `ToolUseStop` flushed defensively on the next
`ToolUseStart`; multi-tool ordering in one turn; refusal as a *successful* turn
ending in `StopReason::Refusal`; inertness after `TurnComplete`. 106 aterm-agent
tests pass; a 3-lens adversarial review (12 raw findings) confirmed 0 defects.

`aterm-app` does not yet construct these providers, so no app wiring changed;
the turn loop's stub-only await-then-drain is fine for now (a real streaming
provider must be spawned + drained concurrently - flagged for T-5.8). Unblocks
T-5.2 / T-5.3 / T-5.4 / T-5.8.
