---
id: T-9.3
epic: EPIC-9-vision-mock-reskin
title: Command block re-skin to the mock (prompt glyph, hover block-meta, exit status, hairline rhythm)
status: ready-for-agent
labels: [ui, timeline]
depends_on: [T-9.1]
---

# Goal

Bring the command block and timeline into visual parity with the mock's `shell`
state: an accent `❯` prompt glyph, the command in `fg.primary`, a right-aligned
`block-meta` (a status dot + duration, "exit N · Ns" on failure) that fades in on
hover, output indented under the command in `fg.secondary`, and a single
`hairline` rule separating consecutive blocks.

# Context

- North star: [ADR-0011](../../adr/0011-vision-mock-ui-north-star.md). Visual
  source: [`AtermWindow.dc.html`](../../design/vision-mock/AtermWindow.dc.html)
  `<!-- shell -->` state.
- Existing implementation this re-skins: the timeline compositor from
  [T-2.7](../EPIC-2-shell-integration-blocks/) / [T-4.6](../EPIC-4-design-system/TICKET-4.6-component-specs.md)
  (`aterm-ui/src/timeline_render.rs`, `components.rs`). This ticket changes the
  *look* to the mock; the block data model and virtualization are unchanged.
- Domain: `Block` / `Command block` / `Timeline` (`docs/agents/domain.md`).

# Implementation notes

- **Prompt glyph**: `❯` in `accent.primary` (shell), aligned in the gutter; the
  command text on the same baseline in `fg.primary` (`font.grid`).
- **block-meta** (right-aligned, `fg.faint`, ~0.82em): a 6px status dot + the
  duration. Dot color = `fg.faint` for a plain/instant command, `success` for
  exit 0 on a longer command, `danger` for non-zero; on failure the text reads
  "exit N · Ns". Per the mock the meta is **hover-revealed** (opacity 0 -> 1 on
  block hover, ~180ms) - reuse the focus-dim animation slot, do not add a fourth
  animation kind.
- **Output**: indented ~29px under the command (the gutter + glyph width), in
  `fg.secondary`, `white-space: pre-wrap`, comfortable line-height (~1.75). ANSI
  colors resolve through the re-tuned palette (T-9.1); inline `FAILED`/error text
  uses `danger`.
- **Separators**: a single `hairline` top rule per block (so the first block has
  none above it - the exact bug the mock's `agent` state jokes about; keep it
  correct). No boxes, no shadows on blocks.
- Preserve the existing gutter status-glyph contract from T-4.6 where it already
  works (running pulse dot); reconcile it with the mock's dot-in-meta treatment so
  running / exit-0 / exit-non-0 remain distinguishable by color **and** shape or
  label (color-blind safety, unchanged requirement).

# Acceptance criteria

- [ ] The shell timeline renders to the mock in both themes: accent `❯`, command
  in `fg.primary`, hover-revealed meta with the correct dot color per exit state,
  indented `fg.secondary` output, single hairline between blocks (none above the
  first).
- [ ] Failure blocks show "exit N · Ns" and use `danger` for the dot and any error
  text; success-with-duration uses `success`.
- [ ] Meta hover fade reuses an existing animation slot; the <=3-animation / <=220ms
  motion budget and the T-1.8 no-per-frame-alloc assertion both still hold.
- [ ] Offscreen GPU test covers a mixed timeline (ok, failed, instant) in both
  themes.

# Out of scope

- The agent transcript ([T-9.6](TICKET-9.6-agent-transcript-reskin.md)) and the
  risk gate ([T-9.7](TICKET-9.7-risk-gate-reskin.md)).
- Focus-Mode dimming of completed blocks ([T-8.5](../EPIC-8-packaging/TICKET-8.5-focus-mode-completions.md)).
