---
title: Issue tracker convention
---

# Issue tracker convention

aterm's backlog is plain Markdown under `docs/tickets/`, version-controlled with the repo. There is no external tracker - the files ARE the tracker. Agents read, claim, and update them the same way humans do. The canonical roster of every epic and ticket is [`docs/tickets/INDEX.md`](../tickets/INDEX.md).

## Layout

```
docs/tickets/
  INDEX.md                              # the roster: every epic + ticket, status, depends_on, build order
  EPIC-<n>-<slug>/                      # one directory per epic (e.g. EPIC-1-terminal-core/)
    TICKET-<id>-<slug>.md               # one file per ticket (e.g. TICKET-1.3-three-thread-split.md)
```

- Epics group tickets by theme and map to the build order in `INDEX.md`. Epic directories are `EPIC-<n>-<kebab-slug>`.
- Each ticket is its own file, `TICKET-<id>-<slug>.md`, where `<id>` is `T-<epic>.<sequence>` (e.g. `T-1.3`). The id is stable and never reused.
- There is no `PRD.md` and no `issues/` subdirectory - a ticket carries its own goal and acceptance criteria inline.

## Ticket file format

YAML frontmatter, then a fixed body:

```markdown
---
id: T-1.3                        # T-<epic>.<sequence>; stable, never reused
epic: EPIC-1-terminal-core       # the parent epic directory
title: Three-thread reader/model/render split + bounded backpressure
status: ready-for-agent          # one of the five triage labels (see triage-labels.md)
labels: [core, perf]             # free-form tags: crate name, domain area
depends_on: [T-1.1, T-1.2]       # ticket ids that must land first ([] if none)
---

# Goal
One paragraph: the outcome this ticket delivers.

# Context
Links to the relevant research doc(s) and ADR(s). Read these before writing - the dossier settled facts you should not reinvent.

# Implementation notes
Concrete crates, types, modules, files to touch. Decisive, not exploratory.

# Acceptance criteria
- [ ] testable bullets; "done" means every box is green.

# Out of scope
Explicit boundaries so the unit stays focused.
```

### Frontmatter fields

| Field | Required | Meaning |
|---|---|---|
| `id` | yes | `T-<epic>.<sequence>` (e.g. `T-2.4`). Stable; never renumber. |
| `epic` | yes | The parent epic directory (matches the folder name). |
| `title` | yes | Imperative one-line summary. Use the exact vocabulary from [`domain.md`](domain.md). |
| `status` | yes | One of the five triage labels - see [`triage-labels.md`](triage-labels.md). |
| `labels` | no | Free-form tags for filtering (crate name, domain area). NOT a triage state. |
| `depends_on` | yes | Array of ticket ids that must land first (`[]` if none). This - not `status` - expresses ordering/blocking. |

## Status vs. ordering

`status` answers *who acts next* and is exactly one of the five canonical triage labels (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`; defined in [`triage-labels.md`](triage-labels.md)). It is NOT a lifecycle/progress field.

**Blocking is expressed by `depends_on`, not by a status.** A ticket whose upstream tickets have not landed still reads `ready-for-agent`; an agent simply picks a `ready-for-agent` ticket whose `depends_on` are all already landed. There is no `blocked` status.

## How an agent works a ticket

1. Pick a ticket whose `status` is `ready-for-agent` **and** whose every `depends_on` ticket has already landed. Do NOT work a ticket in any other status without explicit instruction.
2. Re-read the linked research doc(s) and ADR(s) in `# Context` before writing any code.
3. Do the work on a focused jj commit whose message references the ticket id (see "Landing a change" in `CLAUDE.md`). The commit history + `CHANGELOG` are the record of completion; there is no separate "done" status to set.
4. If you hit a question the dossier left open, or work that would contradict a locked decision (`CLAUDE.md`) or an ADR (`docs/adr/`): set `status` to `needs-info` (missing info / undecided spec) or `ready-for-human` (a product/UX/licensing/security call, or anything that would contradict a locked decision), record the question in a `# Notes` section appended to the ticket, and STOP. Never silently override or guess.

## Updating a ticket

- Append progress, decisions, and blockers under a `# Notes` heading (newest last), each line prefixed with the ISO date and author (e.g. `2026-06-23 (agent:<run-id>):`).
- Routing a stuck ticket back to a human is done by changing `status` to `needs-info` or `ready-for-human` and explaining in `# Notes`.
- Keep the ticket and the code in sync within the same jj commit where practical.
