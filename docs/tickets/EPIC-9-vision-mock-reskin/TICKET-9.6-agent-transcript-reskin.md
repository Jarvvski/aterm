---
id: T-9.6
epic: EPIC-9-vision-mock-reskin
title: Agent transcript re-skin (agent glyph, plan header, tool-call rows, diff colors, summary)
status: ready-for-agent
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

- [ ] An agent turn renders to the mock in both themes: `◇` accent header + step
  meta, PLAN block, tool-call rows (name accent / arg faint), left-bordered output
  blocks, `+`/`-` diff coloring, and a hairline-separated final summary.
- [ ] Colors resolve through tokens (agent = `accent.agent`, tool name =
  `accent.primary`); no literals.
- [ ] Rendering reuses the shared atlas/cell path; T-1.8 no-per-frame-alloc and the
  motion budget hold.
- [ ] Offscreen GPU test covers a multi-step turn (plan + >=1 read + >=1 edit + >=1
  run + summary) in both themes.

# Out of scope

- The inline risk-gate approval UI inside a turn ([T-9.7](TICKET-9.7-risk-gate-reskin.md)).
- The agent loop / tool execution itself (EPIC-5, done); this is presentation.
