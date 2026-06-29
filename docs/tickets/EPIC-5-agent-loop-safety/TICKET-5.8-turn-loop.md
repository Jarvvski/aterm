---
id: T-5.8
epic: EPIC-5-agent-loop-safety
title: Agentic turn loop (shared, provider-neutral)
status: done
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

# Notes

**Landed 2026-06-29.** The shared, provider-neutral loop lives in
`aterm-agent::turn` (`AgentTurn::run`), extending the prior skeleton rather than a
keyword-named `loop` module. All six ACs are met with headless tests; no
on-hardware/visual residual, so genuinely `done`. No real network in any test -
the loop is driven by `MockProvider` scripts (one per round) and custom in-test
providers; no test hangs (concurrency/cancel tests use a `tokio` Barrier + a 5s
`timeout`).

Surface:

- `AgentTurn::run(request, registry, dispatch, approver, cancel, events)` runs
  plan -> act -> observe: `stream_round` streams one provider turn (driving
  `stream_turn` + the shared `AgentEventMapper` via `tokio::join!`, fixing the old
  skeleton's bounded-channel deadlock and swallowing the per-round `TurnComplete`
  so a consumer sees ONE final turn boundary); if the round stops on `ToolUse`,
  `execute_round` gates + (confirms) + executes + sanitizes each call and builds
  ONE `Message::tool_results`; the loop appends the reconstructed assistant turn +
  the tool results and re-issues. Terminates on a non-`ToolUse` stop, no tool
  activity, cancel, or the round cap (`Other("max_tool_rounds")`). `pause_turn` is
  resumed inside the provider (T-5.2), so it never reaches the loop.
- Gate (`AgentTurn::gate`, model risk-claim ignored): `run_command` -> the real
  argv risk gate (`ApprovalPolicy::decide_command`); `edit_file`/`write_file` ->
  `RequireConfirm(FileWrite)` (a write is never provably safe - the gate
  over-approximates per the locked AUTO-SAFE stance); read-only tools ->
  `AutoApprove` (output is sanitized before re-entering context; deeper
  sensitive-path-read gating is the file sink's job, T-5.9). The match is total
  over `ToolInput` (no wildcard), so a future tool can't silently auto-run.
- Prompt-injection structural separation: tool results re-enter ONLY as data-role
  `tool_result` blocks (`Message::tool_results` / `Role::Tool`), never as user
  instructions. The gate + sanitizer share the single `Secrets` source.
- Read-only tools fan out concurrently; mutations serialize. Concurrency uses a
  dependency-free `join_all_concurrent` (a hand-rolled `poll_fn` over pinned
  futures) - no `futures` dep and no `tokio::spawn`, so the dispatch futures may
  borrow the non-`'static` dispatcher.
- `CancelToken` (a `watch`-backed, cloneable signal) + two `biased` `select!`s
  (around the stream phase and the execute phase) give prompt, clean Esc/cancel
  abort (ties to T-3.3); on cancel no `TurnComplete` is emitted and the in-flight
  futures are dropped (no spawn -> no leak).

Decisions / divergences (additive, same pattern as T-5.2's `Message`/`ContentBlock`
extensions - not a relitigation):

1. `MockProvider` gained `scripted(Vec<Vec<ProviderEvent>>)` (one script per
   round), `requests()` (captures received `TurnRequest`s so a test can assert the
   tool_result round-trip shape), and `with_identity()` (to prove provider
   neutrality). `new()` stays back-compatible (single round). An exhausted script
   sends nothing -> the loop reads it as an empty `end_turn` round (no infinite
   loop on a mis-scripted test).
2. Added `AgentEvent::ToolProposalFailed { id, name, error }` (review fix #1). The
   mapper previously turned a malformed-JSON tool block into a bare
   `AgentEvent::Error` with no id, so the loop could not feed an `is_error`
   tool_result back; the new variant carries the id, and the loop reconstructs a
   placeholder `tool_use` paired with the error result. `AgentEvent` is matched
   only here + in `stream_round` (wildcard), so the variant is non-breaking.

Adaptive-thinking replay across rounds is a known, accepted scope cut: the neutral
`ProviderEvent` stream carries no thinking-block SIGNATURE, so thinking blocks are
not replayed across tool rounds. This is acceptable for now because the loop is not
yet wired into the live app (app wiring is T-5.9+); full interleaved-thinking
replay is a provider-level follow-up to revisit when the live Anthropic loop ships.

Adversarial review (3 lenses: control-flow, concurrency/cancel, safety + prompt
injection; find -> verify): 3 raw findings, 2 confirmed and fixed before landing -
(1) MEDIUM: a malformed-JSON tool call was silently dropped (premature turn-end on
a malformed-only round; lost feedback in a mixed round) - fixed via
`AgentEvent::ToolProposalFailed` + placeholder-tool_use round-trip + the loop
treating malformed calls as tool activity (regression tests:
`malformed_only_round_feeds_back_an_error_and_continues`,
`mixed_round_runs_the_valid_call_and_reports_the_malformed_one`). (2) LOW: the
cancel test only exercised the execute-phase cancel arm - added
`cancel_aborts_during_the_streaming_phase` to pin the stream-phase arm. The third
finding was refuted on verification.

20 turn-module tests (155 in `aterm-agent`, 428 workspace-wide; clippy `-D
warnings` clean). No version bump / CHANGELOG: internal plumbing, not yet wired
into the running app, no user-visible behavior. Unblocks T-5.10 (timeline
transcript) and T-5.11 (approval UX).
