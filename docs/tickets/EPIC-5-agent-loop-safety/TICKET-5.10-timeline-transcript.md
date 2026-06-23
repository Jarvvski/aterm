---
id: T-5.10
epic: EPIC-5-agent-loop-safety
title: Timeline transcript model (AgentTurn/AgentStep, tool_use_id join)
status: ready-for-agent
labels: [agent, ui, block-model]
depends_on: [T-5.8, T-2.4]
---

# Goal

Model an agent turn as timestamp-interleaved steps that live as block variants in the SAME single wall-clock timeline as human command blocks, with streaming mapped to incremental entry mutation (never relaying out the whole timeline per delta), and the rendered view kept separate from the API history.

# Context

- Research: [06-agent-architecture.md](../../research/06-agent-architecture.md) section (e) (transcript/UI data model). Locked single-timeline design: agent steps are block variants in the same `BlockList` (T-2.4), wall-clock ordered.

# Implementation notes

- Crate: `aterm-agent` (the data model) + `aterm-core` (block variants added to `BlockList`) + `aterm-ui` (render in the timeline).
- `AgentTurn { id, started_at, steps: [AgentStep], status }`; `AgentStep` = `UserPrompt | Thinking{summary} | AssistantText | ToolCall{tool_use_id, name, input, risk, decision} | ToolResult{tool_use_id, output(sanitized), is_error} | Approval{tool_use_id, mode, resolved_by}`. Each step carries its own timestamp so a long-running ToolCall interleaves with human typing elsewhere.
- `tool_use_id` is the join key between ToolCall, Approval, ToolResult.
- Keep two representations: the API conversation history (raw `content` blocks + `tool_result` user messages, thinking echoed back unchanged) vs the rendered timeline (glossed risk reasons, approval state, sanitized output). Derive the former from the turn; never conflate.
- Streaming -> incremental mutation: `TextDelta`/`ThinkingDelta` append to the current step; a `ToolUseStart` opens a ToolCall. The render watches a dirty-flag/version on the current entry only; never re-lays-out the whole timeline per delta (60fps).
- `message_delta.usage` attaches to the AgentTurn for a cost readout.

# Acceptance criteria

- An agent turn renders as ordered steps interleaved by timestamp with human blocks in one timeline.
- Streaming a long assistant message mutates only the current entry (assert no full-timeline relayout per delta; ties to T-2.7/T-1.8).
- ToolCall/Approval/ToolResult join correctly by `tool_use_id`.
- The API history derived from the turn is a valid provider conversation (round-trips through T-5.2 mock).
- Token usage is attributed to the turn.

# Out of scope

- The approval UX/controls (T-5.11) and component styling (T-4.6).
