---
id: T-9.7
epic: EPIC-9-vision-mock-reskin
title: Risk-gate approval UI re-skin (caution-bordered card, split Approve+menu, reject, approved/rejected states)
status: ready-for-agent
labels: [ui, agent, safety]
depends_on: [T-9.1, T-5.11]
---

# Goal

Re-skin the risk-gate approval affordance to the mock's `gate` state: the proposed
`run_command` shown inline, then a `caution`-bordered card ("Destructive command -
needs your approval" + a plain-language reason), a split **Approve** button with a
dropdown (Approve once / Always approve `<pattern> ...`), a **Reject** button, and
the `⏎ approve · esc reject` hint - plus the resolved approved / rejected states.

# Context

- North star: [ADR-0011](../../adr/0011-vision-mock-ui-north-star.md). Visual
  source: [`AtermWindow.dc.html`](../../design/vision-mock/AtermWindow.dc.html)
  `<!-- gate -->` state (pending / approved / rejected + the dropdown).
- Logic this presents: the deterministic `Risk gate` verdict and the approval UX +
  autonomy controls from [T-5.11](../EPIC-5-agent-loop-safety/TICKET-5.11-approval-ux.md)
  and [T-4.6](../EPIC-4-design-system/TICKET-4.6-component-specs.md). Domain:
  `Risk gate` (Safe/Caution/Dangerous), auto-safe default. This is presentation
  only - the classifier and the Seatbelt sandbox are unchanged.

# Implementation notes

- **Proposed command**: `run_command` label (`accent.primary`) + the argv in
  `fg.primary`, in the turn's gutter alignment.
- **Caution card** (only for Caution/Dangerous - Safe auto-runs silently): 1px
  `caution` border on a `caution_weak`/warn-bg fill, `radius` ~9px, a `△` in
  `caution`, a `fg.primary` title, and a `fg.secondary` reason (the gate's parsed
  reason, e.g. "This permanently deletes files and can't be undone").
- **Actions**: a split primary button - "Approve" (accent fill, white text) + a
  `▾` that opens a menu on `bg.elev` with "Approve once" and "Always approve
  `<pattern> ...`"; a secondary "Reject" (transparent, `hairline` border,
  `fg.secondary`); and a `fg.faint` "⏎ approve · esc reject" hint. `Enter`
  approves, `Esc` rejects (respect the IME preedit gate - do not submit mid-preedit).
- **Resolved states**: approved -> a `success` `✓` line ("Approved · <effect> ·
  exit 0"), and if "always approve" was chosen, a `fg.faint` note that similar
  commands will now auto-run + a pointer to Settings -> Autonomy. Rejected -> a
  `danger` `✕` line ("Rejected · the command was not run").
- "Always approve `<pattern>`" and "Full auto" widen the trust surface: they must
  route through the existing T-5.11 autonomy state and MUST still run inside the
  mandatory Seatbelt sandbox; the gate never returns Safe for a shell-active
  command. Do not weaken the gate here - this ticket only renders and wires the UI
  to the existing decision surface.

# Acceptance criteria

- [ ] The pending gate renders to the mock in both themes: proposed command, the
  caution card with `△` + title + reason, the split Approve+menu, Reject, and the
  keyboard hint. Color is always paired with a text label (color-blind safety).
- [ ] Approve / Approve-once / Always-approve / Reject each drive the existing
  T-5.11 approval + autonomy path; approved/rejected resolved states render per the
  mock; "always approve" surfaces the auto-run note.
- [ ] `Enter`/`Esc` map to approve/reject and honor the IME preedit gate.
- [ ] "Always approve" / "Full auto" changes autonomy state only through T-5.11 and
  never bypass the Seatbelt sandbox; a test asserts the sandbox still wraps an
  auto-approved command.
- [ ] Gate cross-fade uses the existing `motion.fast` slot; motion budget + T-1.8
  assertion hold. Offscreen test covers pending/approved/rejected in both themes.

# Out of scope

- The risk-gate classifier and Secrets deny-set (T-5.5/T-5.6, done).
- The Seatbelt sandbox implementation (T-5.7, done).
- The full Settings -> Autonomy screen ([EPIC-12](../EPIC-12-settings-screen/)).
