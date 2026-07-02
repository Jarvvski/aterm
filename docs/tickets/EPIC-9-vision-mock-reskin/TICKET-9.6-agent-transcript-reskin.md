---
id: T-9.6
epic: EPIC-9-vision-mock-reskin
title: Agent transcript re-skin (agent glyph, plan header, tool-call rows, diff colors, summary)
status: done
labels: [ui, agent, timeline]
depends_on: [T-9.1, T-5.10]
---

# Goal

Re-skin the agent turn's timeline presentation to the mock's `agent` state: an
agent-accent `◇` header with an "agent · N steps" meta, an uppercase PLAN
paragraph, a sequence of tool-call rows (tool name in accent + argument in faint,
with output/diff in a `hairline` left-bordered block), `+`/`-` diff lines in
`success`/`danger`, and a final summary separated by a `hairline`.

# Context

- North star: [ADR-0011](../../adr/0011-vision-mock-ui-north-star.md). Visual
  source: [`AtermWindow.dc.html`](../../design/vision-mock/AtermWindow.dc.html)
  `<!-- agent -->` state.
- Data model this styles: the `AgentTurn` / `AgentStep` transcript from
  [T-5.10](../EPIC-5-agent-loop-safety/TICKET-5.10-timeline-transcript.md) and the
  agent-card styling from [T-4.6](../EPIC-4-design-system/TICKET-4.6-component-specs.md)
  (this ticket updates that look to the mock). Domain: `Turn loop`, `Tool`,
  `AgentStep`.

# Implementation notes

- **Header row**: `◇` in `accent.agent`, the user's request in `fg.primary`, and a
  right-aligned `fg.faint` "agent · N steps" meta. A single `hairline` above,
  matching the command-block rhythm (T-9.3).
- **Plan**: an uppercase `fg.faint` "plan" eyebrow, then the plan prose in
  `fg.secondary`, indented to the glyph gutter (~29px), `font.prose`.
- **Tool-call rows**: each `Tool` call renders as `tool_name` in `accent.primary`
  + its argument (path / command) in `fg.faint`. `edit_file` shows a right-aligned
  "+N -M" count. The tool's output/preview sits in a block with a `hairline`
  left border and `fg.faint` text; diff bodies color `-` lines `danger` and `+`
  lines `success`. Test results color `FAILED` `danger` / `ok` `success`.
- **Final summary**: separated by a top `hairline`, in `fg.primary`, `font.prose`,
  capped at the prose measure (72ch).
- Resolve the mode/agent accent via the T-9.1 resolver; no hardcoded hex. Reuse
  the shared glyph atlas / cell renderer (do not fork a second text path).

# Acceptance criteria

- [x] An agent turn renders to the mock in both themes: `◇` accent header + step
  meta, PLAN block, tool-call rows (name accent / arg faint), left-bordered output
  blocks, `+`/`-` diff coloring, and a hairline-separated final summary.
- [x] Colors resolve through tokens (agent = `accent.agent`, tool name =
  `accent.primary`); no literals.
- [x] Rendering reuses the shared atlas/cell path; T-1.8 no-per-frame-alloc and the
  motion budget hold.
- [x] Offscreen GPU test covers a multi-step turn (plan + >=1 read + >=1 edit + >=1
  run + summary) in both themes.

## Notes

Landed 2026-07-02. The timeline already rendered agent steps as flat, marked rows
(the T-5.10 data binding); this re-skins them per `AgentBlockKind` to the mock's
`agent` state. Three layers:

- **Projection enrichment** (`aterm-core` `AgentBlock` + the live `StreamProjector`
  in `aterm-app/src/agent_runtime.rs`). The mock's tool row needs the tool NAME +
  its ARGUMENT, but the projection deliberately strips the raw tool input for
  secret-safety. So `AgentBlock` gained additive, agent-domain-free fields -
  `tool_name`, `tool_arg`, `edit_stats`, and an `AgentTextRole` (Body/Plan/Summary)
  - and the live projector fills them: the arg is run through the `OutputSanitizer`
  against the same `Secrets` the turn is gated with (redact-before-truncate, 160-byte
  cap), so a secret in an argv is redacted BEFORE it reaches `aterm-core`/`aterm-ui`
  (the crate arrow + crown-jewel discipline hold - a test asserts an argv secret is
  redacted). `ToolInput::display_arg()` / `edit_stats()` (in `aterm-agent`) derive the
  terse arg + the edit's `(added, removed)` line counts. The `text_role` is set as the
  turn streams: prose before the first tool call is the PLAN, prose after is a
  candidate SUMMARY. Because the projector cannot know at stream time which post-tool
  paragraph is the LAST, the renderer disambiguates by lookahead: only the turn's final
  agent step gets the summary hairline + `fg.primary` emphasis; a mid-turn reflection
  stays quiet `fg.secondary` body prose with no rule (an adversarial-review fix - without
  it a turn with mid-turn commentary fragmented into multiple cards).
- **Renderer** (`aterm-ui/src/timeline_render.rs`, the `TimelineRow::Agent` arm,
  rewritten to dispatch by kind): the `UserPrompt` header draws the agent-accent `◊`
  glyph + request + a right-aligned "agent - N steps" meta; `AssistantText` draws a
  PLAN eyebrow (Plan) or emphasized `fg.primary` prose (Summary) else de-emphasized
  `fg.secondary`; `ToolCall` draws name (`accent.primary`) + sanitized arg (`fg.muted`)
  + a right-aligned "+A -M" (success/danger) + a small inline verdict badge for a
  non-auto call; `ToolResult` draws a hairline LEFT-bordered block with `+`/`-` diff
  and FAILED/ok coloring; `Approval` draws a `✓`/`✕` resolution line (used by T-9.7).
  A turn reads as ONE grouped card: `block_draws_top_hairline` suppresses the boundary
  rule between intra-turn steps, keeping it only above a command block, the header, and
  the summary. The new fields are folded into the damage-gate signature; the whole layer
  is still one rect + one glyph draw.

Font substitutions (coverage-tested): the header `◇` (U+25C7) -> `◊` (U+25CA), the
same substitute the input box uses; the resolved-gate `✓`/`✕` (U+2713/U+2715) ->
Nerd-Font PUA `nf-fa-check`/`nf-fa-times`.

Deferred (documented, not silently dropped):
- **Tight step spacing.** The mock groups a turn's steps with no rule between them;
  we suppress the inter-step HAIRLINE but keep the one-row inter-block GAP (gaps are
  baked into the virtualized scroll coordinate; making them turn-aware is a layout
  change beyond a re-skin). The result reads as a quiet grouped card, just slightly
  airier than the mock.
- **The "N steps" count** is computed from the VISIBLE blocks of the turn, so a tool
  call scrolled below the viewport is not counted (a slight undercount on a very long
  turn) - the O(visible) virtualization is preserved rather than paying an O(n) scan.
- The **recorded** `AgentTranscript::blocks()` projection carries name/stats/role but
  NOT the arg (it has no `Secrets` to sanitize with, and it is not the live display
  path - the `StreamProjector` is); if it is ever wired to a display, thread `Secrets`
  through it first.
- Hover/click affordances on agent rows need mouse hit-testing ([T-9.8], absent today).

# Out of scope

- The inline risk-gate approval UI inside a turn ([T-9.7](TICKET-9.7-risk-gate-reskin.md)).
- The agent loop / tool execution itself (EPIC-5, done); this is presentation.
