---
id: T-8.5
epic: EPIC-8-packaging
title: Focus-Mode analog + completions menu
status: ready-for-agent
labels: [ui, polish, deferred]
depends_on: [T-2.7, T-3.5]
---

# Goal

Ship the iA "Focus Mode" analog (dim completed blocks, keep the active/running block + input at full contrast) and the spec-driven completions menu (IDE-style fuzzy menu from Fig completion-spec data) - the polish items deferred from earlier epics.

# Context

- Research: [07-ia-design-language.md](../../research/07-ia-design-language.md) Recommendation 10 (Focus-Mode analog, nice-to-have); [05-unified-input-ux.md](../../research/05-unified-input-ux.md) section 4 + Recommendation 9 (completions: ship history+path in v1, consume MIT Fig spec data for menus later). Owner open-question #5 (completions ambition) - history+path is the v1 floor (in T-3.5); this adds the menu.

# Implementation notes

- Focus Mode: dim non-active blocks to `fg.muted`/lowered opacity (one of the three allowed animations - opacity, <=220ms); keep the running block + input at full contrast. A toggle/setting.
- Completions menu: parse the MIT Fig completion-spec data (declarative schema; consume, do NOT execute the TS) plus filesystem/path + history; render an IDE-style fuzzy menu to the right of the caret (Shell mode). Agent mode repurposes Tab for `@file`/`@command` mention completion. Note the dossier risk: spec consumption needs a spike (parsing a JS/TS object graph without a JS runtime; generator/dynamic specs are awkward).

# Acceptance criteria

- Focus Mode dims completed blocks and keeps the active block + input at full contrast; toggling is smooth (<=220ms) and holds the frame budget.
- Tab in Shell mode opens a fuzzy completion menu sourced from spec data + path + history.
- Agent-mode Tab offers `@file`/`@command` mention completion.
- No frame-budget regression (T-1.8 assertion holds).

# Out of scope

- The base history+path ghost text (T-3.5, already shipped).
- Generator/dynamic-spec execution (explicitly out; consume static schema only).
