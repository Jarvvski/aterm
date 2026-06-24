# Triage labels

Every ticket carries exactly one of these six canonical labels in its `status` frontmatter field (see `issue-tracker.md`). They describe *who acts next*, not how hard the work is. Use the literal strings below - tooling and agents match on them exactly.

| Label | Who acts next | Meaning |
|---|---|---|
| `needs-triage` | a triager (human) | Newly filed, not yet assessed. Scope, validity, and priority are unknown. No one should start work until it is triaged into another state. |
| `needs-info` | the reporter / a human | Cannot proceed without more information - a missing repro, an ambiguous spec, an undecided design question. An agent that hits an unanswered question moves the ticket here, records the question in `## Comments`, and stops. |
| `ready-for-agent` | a Claude agent | Triaged, well-specified, and self-contained enough that an agent can implement it end to end against the dossier and the locked decisions. This is the ONLY state an agent may claim and work autonomously. |
| `ready-for-human` | a human | Requires human judgment, credentials, or hardware an agent lacks - a product/UX call, a licensing or security decision, a token/accent value to sample, a self-hosted-runner action, or anything that would contradict a locked decision or ADR. Agents route here instead of guessing. |
| `done` | nobody (closed) | Every acceptance criterion is met and the work has landed. Any residual is *not* a blocker on this ticket: forward-looking validation (on-hardware perf, a GPU/visual pass) or polish is consolidated into its proper future ticket, recorded in `## Notes`. Reopen via `needs-triage` only if a regression appears. This is the terminal success state - `ready-for-human` is NOT a parking lot for landed work. |
| `wontfix` | nobody (closed) | Considered and explicitly declined - out of scope, superseded, or a duplicate. Record the reason in `## Comments`. Reopen by moving back to `needs-triage`. |

## Rules

- A ticket has exactly one label at a time.
- Only `ready-for-agent` authorizes autonomous agent work. If an agent is uncertain whether a ticket is truly self-contained, treat it as `needs-info` or `ready-for-human` rather than proceeding.
- Move a ticket to `done` once its acceptance criteria are met and the change has landed - do not leave landed work in `ready-for-human`. If a follow-up remains, push it to the future ticket that properly owns it (and say so in `## Notes`) rather than holding this ticket open.
- When an agent cannot continue, it must (a) move the ticket to `needs-info` or `ready-for-human`, (b) explain why in a `## Comments` entry, and (c) stop - never silently park work or guess past a locked decision.
- A triager promotes `needs-triage` to one of the actionable states only after scope and acceptance criteria are clear enough to act on.
