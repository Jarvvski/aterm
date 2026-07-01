---
id: T-9.5
epic: EPIC-9-vision-mock-reskin
title: Launch + modes empty states and the tab-completion popover
status: ready-for-agent
labels: [ui, input]
depends_on: [T-9.1, T-9.4]
---

# Goal

Render the mock's two quiet informational states - `launch` (a fresh, historyless
window) and `modes` (the one-input-two-destinations explainer) - and the
`complete` tab-completion popover: a fuzzy finder that hugs the prompt, matched
letters in accent, a count + hint header, and a `>` pointer on the active row.

# Context

- North star: [ADR-0011](../../adr/0011-vision-mock-ui-north-star.md). Visual
  source: [`AtermWindow.dc.html`](../../design/vision-mock/AtermWindow.dc.html) -
  the `<!-- launch -->`, `<!-- modes -->`, and `<!-- tab completion -->` states
  plus the `completeOpen` popover in the input-bar block.
- The completion popover is the *visual* half of the completions menu tracked by
  [T-8.5](../EPIC-8-packaging/TICKET-8.5-focus-mode-completions.md); its data
  sources (history/path/Fig spec) stay in T-8.5. Ghost text is T-3.5 (done).

# Implementation notes

- **Launch**: centered "aterm", a two-line `fg.secondary` tagline ("A quiet place
  to run things." / "Type a command below, or press ⌘I to ask the agent
  instead."), and a `fg.faint` "no history yet" line. Shown when the active
  timeline is empty.
- **Modes**: an uppercase `fg.faint` eyebrow, a `fg.secondary` paragraph with the
  shell/agent split (a hairline divider between the `❯` Shell and `◇` Agent
  columns), and a "Currently routing to <mode>" line. This is an explainer surface
  reachable on demand, not a persistent state; wire it behind whatever affordance
  the app already uses for help/onboarding, or leave it as a documented state the
  app can select.
- **Completion popover**: anchored just above the input's left edge, on `bg.elev`
  with a `hairline` border + soft shadow. Header row in `fg.faint`: "N/12 · tab
  ⏎ accept · up/down move · esc". Each row: the `>` pointer (accent) on the active
  row else blank, the candidate with fuzzy-matched letters in `accent.primary`
  semibold and the rest in `fg.secondary`, then a `fg.faint` description. Active
  row on an `accent.primary` weak-tint fill. Reuse the fuzzy match highlight model;
  keyboard nav (Tab/up/down/Enter/Esc) matches the mock.

# Acceptance criteria

- [ ] Launch renders centered when the timeline is empty, in both themes.
- [ ] The modes explainer renders to spec in both themes with the two-column
  shell/agent split.
- [ ] The completion popover renders on `bg.elev` above the prompt with the header,
  accent-highlighted match letters, `>` pointer, and active-row tint; keyboard nav
  (tab/up/down/enter/esc) works and accepting fills the input.
- [ ] Motion budget + T-1.8 no-per-frame-alloc assertion hold; a render/widget test
  covers the popover and both empty states in both themes.

# Out of scope

- Completion data sources and the Fig-spec ingest ([T-8.5](../EPIC-8-packaging/TICKET-8.5-focus-mode-completions.md)).
- Agent-mode `@file`/`@command` mention completion (also T-8.5).
