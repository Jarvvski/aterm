---
id: T-9.7
epic: EPIC-9-vision-mock-reskin
title: Risk-gate approval UI re-skin (caution-bordered card, split Approve+menu, reject, approved/rejected states)
status: done
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

- [x] The pending gate renders to the mock in both themes: proposed command, the
  caution card with `△` + title + reason, the split Approve+menu, Reject, and the
  keyboard hint. Color is always paired with a text label (color-blind safety).
- [x] Approve / Approve-once / Always-approve / Reject each drive the existing
  T-5.11 approval + autonomy path; approved/rejected resolved states render per the
  mock; "always approve" surfaces the auto-run note.
- [x] `Enter`/`Esc` map to approve/reject and honor the IME preedit gate.
- [x] "Always approve" / "Full auto" changes autonomy state only through T-5.11 and
  never bypass the Seatbelt sandbox; a test asserts the sandbox still wraps an
  auto-approved command.
- [x] Gate cross-fade uses the existing `motion.fast` slot; motion budget + T-1.8
  assertion hold. Offscreen test covers pending/approved/rejected in both themes.

# Out of scope

- The risk-gate classifier and Secrets deny-set (T-5.5/T-5.6, done).
- The Seatbelt sandbox implementation (T-5.7, done).
- The full Settings -> Autonomy screen ([EPIC-12](../EPIC-12-settings-screen/)).

## Notes

Landed 2026-07-02. The re-skin is three layers:

- **Projection** (`aterm-app`). A parked `RequireConfirm` verdict is projected into a
  UI-agnostic `PendingApproval` (`agent_runtime.rs`): the tool name, the SANITIZED argv
  (`OutputSanitizer` against the single `Secrets` source, redact-before-truncate at the same
  160-byte cap as T-9.6, so no raw secret crosses the crate arrow - a test asserts an argv
  secret is redacted), the risk level, the glossed reason(s), and a cosmetic "always approve"
  family pattern. `TurnHandle::pending_card` snapshots the parked `ApprovalRequest` under the
  lock. The `Session` caches ONE card on the park transition (`refresh_pending_card` in
  `tick`) and borrows it into the frame each present, so a parked frame allocates nothing
  (T-1.8).
- **Renderer** (`aterm-ui/src/approval_render.rs`, a new front-end over the shared atlas): a
  `bg.elev` panel + hairline (occludes the timeline), the proposed command (`accent.primary`
  tool + `fg.primary` argv, WRAPPED so a long argv is never hidden off-screen), a
  `caution`-bordered `caution_weak` card with a `△` (danger-toned for a `Dangerous`/`Blocked`
  verdict) + `fg.primary` title + wrapped `fg.secondary` reason, a split **Approve** (accent
  fill, WHITE text) + `▾` dropdown ("Approve once" / "Always approve `<pattern>`"), a
  **Reject** (hairline border), and a `fg.faint` hint. Damage-gated alloc-free; one rect +
  one glyph draw. Resolved states render as timeline `Approval` blocks (`✓`/`✕`), injected by
  the session BEFORE the loop is unblocked so the decision precedes the tool's result.
- **Keyboard** (`Session::handle_gate_key` + the pure, unit-tested `gate_key_intent`):
  `Enter`/`y` approve once, `Esc`/`n` reject (honoring the IME preedit gate), `↓`/`Tab` open
  the dropdown (`↑`/`↓` choose, `Enter` selects), `Ctrl-C` aborts the whole turn. Every key is
  swallowed while parked, gated on the LIVE parked state so no key slips through in the tick
  gap.

Font substitutions (coverage-tested): the header `△` (U+25B3) -> `nf-fa-exclamation-triangle`
(U+F071), the split-button `▾` (U+25BE) -> `nf-fa-caret-down` (U+F0D7), the hint `⏎` -> the
word "enter" (all `.notdef` in the bundled Mono face).

Adversarial-review fixes (a review workflow found + verified 5 defects, all fixed before
landing): (1) the card was `!alt_screen`-gated while the keyboard resolved regardless - a
blind approve/reject over a background alt-screen TUI; the card is now a MODAL SAFETY overlay
that draws even over alt-screen, so visibility matches the keyboard lock; (2) a long argv ran
off the clamped panel with no ellipsis - the command now WRAPS to the panel budget (the argv
is the thing the user must read); (3) the Approve ink used `legible_against(accent, accent,
21.0)`, whose max-contrast endpoint is BLACK on the mid-tone accent - it now seeds white and
corrects only if needed, guarded by a both-themes test; (4) Esc lost the in-park whole-turn
cancel (it now rejects-this-call per the AC) - `Ctrl-C` restores the abort; (5) a
modifier-chorded `y`/`n` resolved the gate - the aliases now require no modifier (still
swallowed when chorded), matching the Tab-popover convention.

**OWNER-CONFIRM (divergences from the mock, flagged not silently overridden):**
- **"Always approve" is TIER-scoped, not per-pattern.** Per the settled decision "Approve now
  + widen future", `AlwaysApprove` approves this call and widens the SESSION autonomy to
  `AutoRunInSession` (T-5.11) - it does NOT install a per-command allow-rule. So the mock's
  "Similar commands (`rm -rf …`) will now run without asking" is only accurate for `Caution`
  commands: `Dangerous`/shell-active NEVER auto-run in any tier (the locked invariant), so a
  future `rm -rf` still asks. The auto-run note reflects the tier honestly. The widening takes
  effect on FUTURE turns (this turn's policy was fixed at start); the pattern is a cosmetic
  label. A test asserts the widest tier auto-approves a non-shell-active `Caution` yet the
  execution sink stays the enforcing Seatbelt sandbox (AC4).
- **`Esc` = reject (this call), not cancel (the turn)** - per AC text; the whole-turn abort
  moved to `Ctrl-C` (see fix 4 above), a superset of the prior behavior.

Deferred (documented, not silently dropped):
- **Mouse hit-testing** on the Approve/Reject buttons + the dropdown ([T-9.8], absent today);
  the card is keyboard-driven for now.
- The `motion.fast` cross-fade on the gate's appearance/resolution is the shared
  [`Animation::CrossFade`] slot (asserted by the components motion-budget test); the actual
  per-frame tween animation is EPIC-wide motion work, not wired per-overlay yet.
