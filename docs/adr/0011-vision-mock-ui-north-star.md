# ADR-0011: Adopt the imported vision mock as the UI north star

## Status

Accepted (2026-07-01)

Supersedes the visual specifics of `docs/design/design-system.md` where they
conflict (see "Consequences"). It does NOT touch the locked *product* decisions
(unified input, full-agentic loop, multi-provider seam, auto-safe autonomy,
mandatory Seatbelt sandbox, GPLv3) - those are unchanged and this ADR is
consistent with all of them.

## Context

A concrete visual mock of aterm was designed in Claude Design and imported into
the repo at `docs/design/vision-mock/` (`AtermWindow.dc.html` +
`aterm.dc.html`) on 2026-07-01 via the design MCP. The owner declared it "how
aterm SHOULD end up looking" - i.e. the authoritative UI target, not one option
among several.

The mock is a single `AtermWindow` component rendered in eight `screen` states:
`launch`, `shell`, `modes`, `agent`, `gate`, `complete` (tab completion),
`settings`, `editor`. It is faithful to every locked product decision (one
shell-first input box, a mode chip that flips where Enter routes with text
preserved, command/agent blocks in one timeline, a deterministic risk gate with
approve/reject, auto-safe autonomy, Anthropic+OpenAI providers). But in several
respects it **diverges from the older `design-system.md`**, which until now was
normative for `aterm-tokens` and `aterm-ui`:

1. **Warmer palette.** The mock's dark canvas is a warm near-black `#1b1915`
   (vs the neutral `#1C1C1C` in `tokens.toml`); light is warm paper `#faf7ef`
   (vs `#FAF9F6`); the accent is a softer blue `#3d88cc` dark / `#2f7dc2` light
   (vs `#4DA6F0` / `#1A93E8`). The mock also names an elevated-surface color
   (`--bg-elev`) for popovers/menus that the token set did not model, and warm
   semantic colors: warn `#d59a4a`, err `#d47257`, ok `#82ac79` (dark).
2. **A second accent for Agent mode.** The mock signals Shell vs Agent with two
   accents - shell blue (`--accent`) and agent purple (`--agent` `#9d86d6` dark
   / `#7458bd` light) - driving the prompt glyph (`❯` vs `◇`), the caret tint,
   and the mode chip. `design-system.md` §1 rule 2 said "one accent, used
   scarcely" and §5/OQ5 defaulted to an always-blue caret with the target
   signalled only by a neutral->weak chip.
3. **A custom title bar and a sessions sidebar.** The mock draws a 44px title
   bar (traffic-light dots, a centered active-session name + cwd, a sidebar
   toggle glyph) and a 210px sessions sidebar. `design-system.md` §1 said "no
   toolbar, no tab strip chrome, no sidebar by default, no native title bar."
4. **New surfaces.** The mock adds an **editor mode** (open a file into a calm
   centered writing pane; the terminal folds away) and a rendered **settings /
   preferences screen** - neither existed in any epic.

## Decision

**Adopt the imported vision mock (`docs/design/vision-mock/`) as the
authoritative UI north star. Where the mock and the pre-existing
`design-system.md` disagree, the mock wins.** Concretely:

1. **The warm palette from the mock becomes the token set.** `tokens.toml` and
   `design-system.md` are rewritten to the mock's values (canvas, surface,
   elevated surface, ink three-step, hairline, accent, agent, warn/err/ok),
   preserving the token *structure* (semantic names, both themes, the ANSI-16
   sets re-tuned to the warmer family). Contrast is re-validated against the new
   hexes. This work is **T-9.1**; nothing downstream hardcodes hex.
2. **Agent mode gets a first-class second accent.** The one-accent rule is
   relaxed to a **two-accent mode model**: `accent.primary` (shell/blue) and a
   new `accent.agent` (purple). The pairing "prompt glyph + caret tint + mode
   chip, all in the mode color" replaces the always-blue-caret default. Scarcity
   is preserved in spirit: exactly two mode accents, no decorative color beyond
   the semantic warn/err/ok set.
3. **A custom title bar and an optional sessions sidebar are sanctioned.** The
   window keeps a hidden *native* titlebar (per T-8.1) and draws its own 44px
   bar; the sidebar is toggleable and not shown by default on a single session.
   This overturns the "no title bar / no sidebar" clause of `design-system.md`
   §1.
4. **Editor mode and a settings screen are sanctioned surfaces**, scoped to
   their own epics (EPIC-11, EPIC-12). Editor mode is a distinct top-level
   surface, not a third `InputModel` mode (Shell|Agent is unchanged per
   ADR-0004).

The translation of the mock into the live UI is organized as four epics:

- **EPIC-9 - vision-mock re-skin.** The token reconciliation (T-9.1) plus
  bringing every already-shipped surface (window frame/title bar, command
  block, unified input + mode chip, launch/modes states, agent transcript, risk
  gate, tab-completion popover) into visual parity with the mock in both themes.
- **EPIC-10 - multi-session + sessions sidebar.** The session model and the
  sidebar/title-bar binding.
- **EPIC-11 - editor mode.** The centered writing surface, mode transition, and
  file load/save.
- **EPIC-12 - settings screen.** The typographic preferences screen and its
  bindings.

## Consequences

- `design-system.md` is no longer the top of the visual hierarchy; **this ADR +
  the imported mock are.** `design-system.md` and `tokens.toml` are updated by
  T-9.1 to match, and thereafter continue to own *intent* and *values*
  respectively - but reconciled to the mock. The several "OWNER CONFIRMATION
  REQUIRED" notes in `design-system.md` about the derived accent are resolved:
  the accent is now the mock's blue, by owner decision.
- The "one accent" invariant in any older doc or ticket is now "two mode
  accents"; reviewers should not flag agent-purple as a violation.
- The "no title bar / no sidebar" invariant is retired; a custom title bar and a
  toggleable sidebar are allowed. T-8.1 (hidden native titlebar) still holds -
  the custom bar is drawn inside a titlebar-less window.
- New scope enters the backlog: multi-session, editor mode, settings UI. These
  are additive; none blocks the re-skin (EPIC-9), which is the critical path to
  "aterm looks like the mock."
- The 60fps floor and the motion budget (<=3 animations, <=220ms) are unchanged
  and bind every new surface; the mock's transitions (block-meta fade on hover,
  chip/gate cross-fade) fit inside that budget.
- The mock lists a **"Local" provider** and a **"Full auto"** autonomy option in
  its settings screen that go beyond the locked set (ADR-0005 lists Anthropic +
  OpenAI; ADR-0006 makes auto-safe the default and every command sandboxed).
  These are flagged in EPIC-12 as owner-confirm items and are NOT auto-adopted
  by this ADR.

## Alternatives considered

- **Keep `design-system.md` normative; adopt only the mock's layout.** Rejected
  by the owner: the mock is the target look, warm palette and agent accent
  included, not just a layout reference.
- **Fold everything into one large "make it look like the mock" epic.**
  Rejected: multi-session, editor mode, and settings are each substantial new
  functionality, not re-skins. Splitting them keeps EPIC-9 a pure visual-parity
  pass on a shippable critical path and lets the feature epics land
  independently.
- **Rewrite the tokens inline in this ADR.** Rejected: an ADR records the
  decision; the concrete hex propagation into `tokens.toml` + `design-system.md`
  is implementation (T-9.1), tested and landed as code.
