---
id: T-5.10
epic: EPIC-5-agent-loop-safety
title: Timeline transcript model (AgentTurn/AgentStep, tool_use_id join)
status: done
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

# Notes

Landed: `crates/aterm-agent/src/transcript.rs` (the transcript model + 9 unit
tests), the `Block` struct->enum redesign in `crates/aterm-core/src/block.rs`
(`Block::{Command(CommandBlock), Agent(AgentBlock)}` + the agent-step render
projection + 3 core tests), the timeline render binding in
`crates/aterm-ui/src/{timeline.rs,timeline_render.rs,components.rs}` (+ 3 UI
tests), a small enrichment to `AgentEvent::ToolResult` (carries `is_error`) in
`provider.rs`/`turn.rs` (+ 1 end-to-end round-trip test), `lib.rs` re-exports, and
the `CHANGELOG.md` entry. Gate green (fmt / clippy `-D warnings` / build / full
workspace test - 638 tests). An independent adversarial review (13 agents,
find->refute) confirmed ZERO surviving defects across all five ACs, the block-enum
migration safety, the crate-arrow purity, and the AgentEvent enrichment.

Owner-confirm decisions taken before coding (not silently chosen):

1. **Name collision.** The locked vocab (`06-agent-architecture.md` §e) names the
   transcript `AgentTurn`, but that name is the landed turn-loop DRIVER
   (`turn::AgentTurn<'a,P>`). Per owner: keep the driver's name; the transcript model
   is `AgentTranscript`. The `AgentStep` variant set and every field keep the locked
   names verbatim.
2. **Block enum redesign.** Per owner, did the full `Block` struct->enum redesign now
   (the T-2.4 owner-confirm item, "to be designed alongside Epic-5's agent variants"):
   `Block` is now `Command(CommandBlock) | Agent(AgentBlock)`. The former
   `interactive`/`approximate` flag stand-ins moved onto `CommandBlock`. The segmenter
   + engine were made interleave-safe (route through `last_running_command_mut()` /
   `set_last_command_output()` instead of assuming "running block == last block").

AC coverage:

- **AC1** - agent steps interleave with command blocks in ONE wall-clock timeline:
  `timeline::agent_steps_render_interleaved_with_command_blocks_in_order` (layout emits
  Command/Output rows for a command, `Agent(line)` rows for an agent step, in order) +
  `block::agent_steps_interleave_with_command_blocks_in_append_order`.
- **AC2** - streaming mutates only the current entry: `transcript::streaming_deltas_
  extend_the_open_step_not_push_new_ones` (model), `block::append_agent_text_is_a_point_
  update_touching_only_the_tail` (HeightIndex point-update; earlier entries' geometry
  unchanged), and `timeline_render::{agent_text_delta_invalidates_the_damage_gate,
  an_agent_delta_does_not_relayout_the_earlier_command_block}` (the damage gate redraws
  the tail but the head block's placement/rows are byte-identical).
- **AC3** - `tool_use_id` join: `transcript::tool_call_approval_and_result_join_by_
  tool_use_id`.
- **AC4** - derived API history round-trips through the T-5.2 mock: `turn::transcript_
  derived_history_reproduces_and_round_trips_through_the_mock` drives a REAL turn, folds
  its events into a transcript, and proves `derive_history()` reproduces exactly what the
  loop sent the provider AND is accepted verbatim when fed back through a fresh
  `MockProvider` (plus `transcript::derive_history_*` shape/validity tests).
- **AC5** - usage attributed to the turn: `transcript::usage_accumulates_onto_the_turn`.

Two-representation design (locked): `AgentTranscript` owns the data model;
`derive_history()` produces the provider API history (raw assistant + `tool_result`
blocks, thinking/approval excluded - matching the loop), while `AgentStep::to_block()`
projects each step into an agent-domain-FREE `aterm_core::AgentBlock` for the rendered
timeline. The one-way crate arrow (`aterm-agent -> aterm-core`) holds: `aterm-core` names
no agent/LLM type; `aterm-agent` constructs the core block, never the reverse.

Residuals (recorded follow-ups, not silently shipped):

1. Wall-clock interleave is by APPEND order, not an explicit sort on the step `ts`
   field: the timeline (and the transcript) relies on the caller pushing steps as they
   are emitted (the locked single-timeline design; insertion order IS wall-clock order).
   A future batch-insert path would need to insert in chronological order, not sort
   after the fact.
2. `AgentTranscript`/`AgentBlock` are implemented + tested but not yet WIRED into
   `aterm-app`: no UI path builds a transcript from a live turn or pushes its projected
   blocks into the engine's `BlockList` yet. The app integration (build the transcript
   from the loop's events + the gate/approval info, project + push into the timeline)
   rides T-5.11.
3. Thinking steps are NOT echoed into the derived API history (the current turn loop +
   `ContentBlock` model do not round-trip thinking blocks); they render in the timeline
   only. Faithful thinking-block echo is a follow-up if/when the provider model needs it.
4. The agent gutter marker reuses an already-bundled, font-coverage-tested glyph
   (`nf-fa-caret-right`) with a neutral color + "agent" label as a placeholder; real
   agent-card iconography/styling is EPIC-4 (T-4.6).
