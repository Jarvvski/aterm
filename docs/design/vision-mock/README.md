# aterm vision mock (imported)

Imported from the Claude Design project
`claude.ai/design/p/a0f6be82-b9c9-422f-bdd2-1dbb5c75e94d` via the design MCP on
2026-07-01. This is the **UI north star**: "how aterm SHOULD end up looking."
Adopted as authoritative by [ADR-0011](../../adr/0011-vision-mock-ui-north-star.md).

- `preview.html` - a **self-contained, runtime-free** static render of all eight
  states in both themes (Light/Dark toggle, `⌘L`). Open it in any browser; it has
  no external dependencies, so it survives deletion of the cloud design project.
  It is a snapshot for viewing, not the editable source.
- `aterm.dc.html` - the showcase/gallery wrapper that renders the eight states.
- `AtermWindow.dc.html` - the actual component: the window frame plus all eight
  `screen` states (`launch`, `shell`, `modes`, `agent`, `gate`, `complete`,
  `settings`, `editor`) and the unified input bar, with the warm two-theme
  palette, the shell/agent two-accent model, and the mode chip.

These are Design Composer (`x-dc`) source files, not runnable in isolation
(they expect the design runtime's `support.js`). They are the reference of
record for EPIC-9 (re-skin) and EPIC-10/11/12 (the new feature surfaces). Where
this mock and the older `docs/design/design-system.md` disagree, ADR-0011 makes
the mock win; T-9.1 reconciles the tokens.

The values distilled from the mock (warm palette, semantic tokens, the eight
states) are captured in [ADR-0011](../../adr/0011-vision-mock-ui-north-star.md)
so downstream tickets cite a stable summary rather than re-parsing the HTML.
