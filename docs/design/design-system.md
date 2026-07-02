---
title: aterm Design System
phase: 2-design
status: design
owns: the iA-derived visual language as a concrete, buildable system
mirrors: docs/design/tokens.toml (machine-readable; values MUST stay identical)
---

# aterm Design System

The concrete design language for aterm. Derived from iA Writer's visual
ethos (see `07-ia-design-language.md`) and the GPU text pipeline constraints
(see `08-text-glyph-rendering.md`). Every value here has a typed mirror in
`tokens.toml`, which the `aterm-tokens` crate reifies as Rust constants. When
this doc and `tokens.toml` disagree, `tokens.toml` is the source of truth for
values and this doc is the source of truth for *intent*. Keep them in lockstep.

This document is normative for `aterm-tokens` (the token values) and
`aterm-ui` (the component specs). It does not restate research; it cites it.

> **NORTH STAR: the vision mock (ADR-0011).** The palette and the two-accent mode
> model below are the imported vision mock
> (`docs/design/vision-mock/AtermWindow.dc.html`), adopted as the authoritative UI
> target by [ADR-0011](../adr/0011-vision-mock-ui-north-star.md); where this doc
> and the mock once disagreed, the mock won. That ADR **resolves** the former
> "OWNER CONFIRMATION REQUIRED / derived accent" note: the accent is now the
> mock's blue by owner decision. Every contrast ratio quoted below is **recomputed
> against the new hexes** by `aterm-tokens`' `wcag_*` tests (a real WCAG
> computation, not an estimate); the one intentional sub-AA tone (`fg.muted`) is
> annotated with its permitted use.

---

## 1. Philosophy

aterm's UI is **radical restraint**: the timeline of command/agent blocks *is*
the window. Everything that is not content is a hairline, whitespace, or a mode
accent. This is iA Writer's "omit needless words" applied to a terminal
(`07-ia-design-language.md` §1). Per ADR-0011 the window does draw its own **44px
custom title bar** (inside a hidden native titlebar, T-8.1) and an **optional,
toggleable sessions sidebar** (not shown by default on a single session) - the
former "no title bar / no sidebar" clause is retired. There is still no toolbar
and no tab-strip chrome.

The title bar (T-9.2) carries, left to right: three decorative traffic-light dots
in the warm chrome hues (`chrome.close` red / `chrome.minimize` amber /
`chrome.zoom` green - tokens, not scattered hex), a sidebar-toggle glyph in
`fg.muted`, and an absolutely-centered active title (`fg.primary`) + `  -  <cwd>`
(`fg.muted`), over a bottom `hairline` rule. The renderer reserves this 44px band
so the timeline lays out below it. The mock's rounded corners + soft drop shadow
are a titlebar-less-window property that cannot be drawn into a native-decorated
opaque surface, so they land with the borderless packaging (T-8.1); the toggle
glyph's pointer *click* awaits mouse hit-testing (today the intent is driven by
`Cmd-B`).

Five rules govern every component:

1. **Chrome-less.** Flat rectangles, hairline separators, no drop shadows on the
   timeline (the exceptions: the agent card's single hairline border, and the
   floating popovers - gate menu, completion menu - which sit on `bg.elev` with a
   soft shadow, per the mock). Blocks are delimited by `hairline` top/bottom
   rules, not boxes. (Implementation note: the shipped popovers - the T-9.5
   completion finder - render flat on `bg.elev` with a hairline border; the soft
   drop shadow is deferred with the other window-shadow work in T-8.1, since a
   shadow needs a transparent surface.)
2. **Two mode accents, used scarcely.** Per ADR-0011 the old one-accent rule is
   relaxed to a two-accent **mode** model: shell blue (`accent.primary`) and agent
   purple (`accent.agent`), resolved as "the current mode color" by
   `mode_accent`. The mode accent appears only on the prompt glyph, the caret
   tint, the mode chip, links, and the focus ring. Scarcity is preserved in
   spirit: exactly two mode accents, no decorative color beyond the semantic
   success/caution/danger set.
3. **Whitespace is the layout.** Generous vertical rhythm between blocks
   (`space.6`+). A thin left gutter carries the only persistent status
   iconography (exit code / running / risk badge).
4. **Paper, two ways.** Two themes ship day one and are non-negotiable: a warm
   **"paper" light** (`#FAF7EF`) and a warm near-black **dark** (`#1B1915`), both
   from one warm hue family (the mock's palette) so raw ANSI terminal output and
   aterm's own UI never clash in the same surface.
5. **Focus Mode analog.** The active/running block and the input stay at full
   contrast; completed blocks may dim to `fg.muted`. This is iA's Focus Mode
   mapped to the block timeline. (Nice-to-have for v1; the tokens exist for it.)

The motion budget is a hard constraint, not a stylistic one: aterm has a
**guaranteed 60fps floor (120fps on ProMotion)**. Animation must never threaten
the frame budget (§6).

---

## 2. Typography

iA's three width systems map directly onto the bundled `iM Writing Nerd Font`
faces (`07-ia-design-language.md` §2). Use exactly this three-register split.
**Never put a variable-width face in the terminal grid** - the grid requires a
constant cell advance for column alignment, box-drawing, tables, and TUI apps.

| Role | Face (token `font.*`) | Why |
|---|---|---|
| Terminal grid: input echo, PTY output, code, diffs, proposed commands | **iM Writing Mono NFM** (`font.grid`) — constant advance, single-width Nerd glyphs | grid alignment is mandatory; NFM = "Nerd Font Mono" keeps icon glyphs single-cell |
| Agent prose, explanations, chat-like transcript | **iM Writing Duo** (`font.prose`) — duospace, m/M/w/W get ~1.5× | reads as comfortable body prose while staying visually kin to the grid ("draft-honest") |
| Dense chrome: status strip, chip labels, palette rows, settings | **iM Writing Quattro** (`font.ui`) — near-proportional, 4 widths | most space-efficient; best for small fixed labels |

> **BUNDLE GAP (flag for the font/licensing domain).** The prior prototype
> vendored only the **Mono** NFM variant; **Duo and Quattro were NOT vendored**
> (`08-text-glyph-rendering.md` §3, Rec 11). The prose/UI registers above
> require adding the Duo/Quattro faces to the bundle, or defining an explicit
> system proportional fallback. Until they ship, `font.prose`/`font.ui` fall
> back to the platform proportional UI face.

> **Duo/Quattro metrics are qualitative in iA's essays.** The exact 1.5×
> advance, x-height, and cap-height are not published; measure them from the
> actual TTFs before building any layout that depends on Duo advance widths
> (`07-ia-design-language.md` Risks).

### Type scale

Sizes in **pt** (logical points; the renderer multiplies by the backing scale
factor). Line height is a unitless multiplier. These are the `[type]` tokens.

| Token | Size (pt) | Line height | Face | Use |
|---|---|---|---|---|
| `type.grid` | 13 | 1.30 | `font.grid` | terminal cells — line height drives scroll rhythm |
| `type.body` | 14 | 1.50 | `font.prose` | agent prose (matches iA template leading) |
| `type.heading` | 16 | 1.35 | `font.prose` (medium 500) | block headers / agent step titles |
| `type.label` | 11 | 1.30 | `font.ui` | chips, status strip, gutter |
| `type.caption` | 10.5 | 1.30 | `font.ui` | timestamps, exit codes, secondary meta |

**Weights** available in the family: Regular 400, Medium 500, SemiBold 600,
Bold 700, each with italics. Headings use Medium 500; body uses Regular 400.

**Measure.** Cap agent-prose columns at **72ch** (`type.measure_ch`), iA's
middle line-length option (64 / 72 / 80). **Never cap the terminal grid** — it
follows the PTY column count.

**Ligatures.** ON at rest with an ASCII fast-path bypass; throttled/disabled
during high-throughput streaming to protect the frame floor
(`08-text-glyph-rendering.md` §4, Rec 9). This is a renderer behavior, not a
token; noted here so the visual spec and the renderer agree.

---

## 3. Color

Two themes, one warm hue family, from the vision mock (ADR-0011). Light is warm
"paper" (`#FAF7EF`); dark is a warm near-black (`#1B1915`). The mock has two
background levels - canvas and a single elevated tone - so `bg.surface` and
`bg.elev` share a value (kept as distinct tokens so downstream can diverge). The
tints (`hairline`, `selection_bg`, the `*_weak` fills) are the mock's
alpha-over-canvas values stored **pre-composited to opaque** (they only ever sit
on canvas, and opaque keeps the WCAG/legibility math correct). Every ratio in
the Contrast notes is recomputed, not estimated.

### Semantic tokens (`[color.light]` / `[color.dark]`)

| Token | Light | Dark | Role |
|---|---|---|---|
| `bg.canvas` | `#FAF7EF` | `#1B1915` | the paper / the void; default surface (`--bg`) |
| `bg.surface` | `#F2EDE1` | `#221F19` | raised block / agent-card fill (= `bg.elev`) |
| `bg.surface_alt` | `#E9E2D1` | `#2B2820` | code/output block fill, hover rows (derived) |
| `bg.elev` | `#F2EDE1` | `#221F19` | **elevated surface** — popovers, gate menu, completion (`--bg-elev`) |
| `fg.primary` | `#26231B` | `#ECE6D8` | body text / grid default foreground (`--ink`) |
| `fg.secondary` | `#6C6555` | `#9A9382` | secondary meta, de-emphasized reasoning (`--ink-dim`) |
| `fg.muted` | `#A89F8C` | `#5E584B` | faint meta / placeholders (`--ink-faint`; **sub-AA**, see notes) |
| `fg.faint` | `#BCB4A3` | `#4A453B` | hairline-adjacent text, disabled (derived) |
| `accent.primary` | `#2F7DC2` | `#3D88CC` | **shell accent** (blue) — caret/glyph/chip in shell mode (`--accent`) |
| `accent.agent` | `#7458BD` | `#9D86D6` | **agent accent** (purple) — caret/glyph/chip in agent mode (`--agent`) |
| `accent.primary_text` | `#2B73B4` | `#3D88CC` | accent when it must carry small body text (AA on its bg) |
| `accent.primary_weak` | `#E2E8EA` | `#1F262B` | low-emphasis accent fill (badges, target chip); accent @ 12% over canvas |
| `hairline` | `#E5E2DA` | `#2D2A26` | the 1px separators between blocks (the iA signature) |
| `hairline_strong` | `#D4D1C9` | `#3C3A34` | section dividers |
| `selection_bg` | `#C5D7E3` | `#243645` | text selection (accent @ 26% over canvas) |
| `success` | `#5C8A56` | `#82AC79` | exit 0, Safe gate verdict (`--ok`) |
| `caution` | `#B57D2C` | `#D59A4A` | needs-approval gate, warnings (`--warn`) |
| `caution_weak` | `#F4EDDF` | `#2C251A` | **gate card fill** — warn @ ~8% over canvas (`--warn-bg`) |
| `danger` | `#BF5A40` | `#D47257` | exit≠0, blocked/destructive gate (`--err`) |
| `info` | `#2F7DC2` | `#3D88CC` | = `accent.primary` |

The mode accent resolver (`SemanticColors::mode_accent`) returns `accent.primary`
in shell mode and `accent.agent` in agent mode - the mock's `--mode` custom
property. `success` / `caution` / `danger` are the warm mock semantics (green /
amber / warm-red), **not** generic web traffic-light colors, so the risk gate
reads as part of the system rather than a bolted-on alert UI.

> **Light paper + heavy ANSI is the riskiest combination.** Many CLI tools'
> default bright cyan/yellow are near-invisible on warm paper - by design the
> light `bright_cyan`/`bright_yellow`/`bright_green` sit sub-3:1, and the renderer's
> light legibility remap (`AnsiPalette::with_fg_legibility` against `bg.canvas`)
> lifts them. This is a renderer concern, not a token edit.

### Contrast notes (recomputed against the new hexes — see `aterm-tokens` `wcag_*` tests)

All measured with the crate's WCAG 2.1 `contrast_ratio`. AAA body ≥ 7:1, AA body
≥ 4.5:1, AA large/UI ≥ 3:1.

- `fg.primary` on `bg.canvas`: light **14.65:1**, dark **14.11:1** (AAA).
- `fg.secondary` on `bg.canvas`: light **5.40:1**, dark **5.74:1** (AA body).
- `accent.primary` on `bg.canvas`: light **4.06:1**, dark **4.67:1**. Light clears
  AA large/UI (caret, prompt glyph, mode chip, links); for small 11pt text use
  `accent.primary_text` (light `#2B73B4` = **4.65:1**, dark = **4.67:1**, both AA body).
- `accent.agent` on `bg.canvas`: light **5.06:1**, dark **5.70:1** (AA body).
- `success` **3.75 / 6.79**, `caution` **3.31 / 7.15**, `danger` **4.13 / 5.30**
  (light / dark): all clear AA large/UI (gutter dots, gate chips, badge text).
- **Sub-AA, intentional:** `fg.muted` on `bg.canvas` is light **2.45:1**, dark
  **2.49:1** - the mock's faint tone. **Permitted use only:** de-emphasized,
  non-essential meta (timestamps, exit captions, placeholder text, the "+N lines"
  affordance). It must never be the sole carrier of essential information. Guarded
  by the `fg_muted_is_intentionally_sub_aa` test.

### ANSI 16-color palettes (`[ansi.light]` / `[ansi.dark]`)

Terminal output must look correct *and* belong to the warm theme (T-4.2's
structure; hues warmed to the mock family). Bright = the lighter/more-saturated
sibling. On light "paper", ANSI index 7/15 are dark foregrounds and index 0 is
darkest; the default output foreground is `fg.primary`.

**Light "paper":**

| Idx | Name | Hex | | Idx | Name | Hex |
|---|---|---|---|---|---|---|
| 0 | black | `#26231B` | | 8 | bright_black | `#A89F8C` |
| 1 | red | `#B0502F` | | 9 | bright_red | `#C96A44` |
| 2 | green | `#4E7D48` | | 10 | bright_green | `#77A56A` |
| 3 | yellow | `#9C6A22` | | 11 | bright_yellow | `#DCB45A` |
| 4 | blue | `#2C6EA9` | | 12 | bright_blue | `#3D88CC` |
| 5 | magenta | `#63499F` | | 13 | bright_magenta | `#8F74CF` |
| 6 | cyan | `#2F7D74` | | 14 | bright_cyan | `#57C3B6` |
| 7 | white | `#6C6555` | | 15 | bright_white | `#26231B` |

**Dark:**

| Idx | Name | Hex | | Idx | Name | Hex |
|---|---|---|---|---|---|---|
| 0 | black | `#1B1915` | | 8 | bright_black | `#5E584B` |
| 1 | red | `#D06E54` | | 9 | bright_red | `#E08A70` |
| 2 | green | `#85B078` | | 10 | bright_green | `#9CC590` |
| 3 | yellow | `#D2A15A` | | 11 | bright_yellow | `#E6B56A` |
| 4 | blue | `#4D8FCA` | | 12 | bright_blue | `#6BA6DD` |
| 5 | magenta | `#A98FD6` | | 13 | bright_magenta | `#BBA6E6` |
| 6 | cyan | `#6BB0A6` | | 14 | bright_cyan | `#8FC9BF` |
| 7 | white | `#CFC8B8` | | 15 | bright_white | `#ECE6D8` |

> **ANSI tuning is taste, not spec.** Eyeball both sets against real output
> (`ls --color`, `vim`, `htop`, `git diff`) on both themes; the eyeball pass is
> deferred to EPIC-7's shell-matrix.

---

## 4. Spacing & rhythm

4px base. iA leans on whitespace, so block separation defaults to the larger
steps. The `[space]` tokens:

| Token | px | Use |
|---|---|---|
| `space.0` | 0 | — |
| `space.1` | 4 | inline gaps (chip padding) |
| `space.2` | 8 | label-to-value, icon-to-text |
| `space.3` | 12 | intra-block padding |
| `space.4` | 16 | block content padding (horizontal gutter) |
| `space.6` | 24 | between blocks (vertical rhythm) |
| `space.8` | 32 | section breaks, agent-card outer margin |
| `space.12` | 48 | top/bottom canvas breathing room |

**Radii** (`[space]` → `radius_*`): keep tiny. `radius.sm = 4px` (chips/badges),
`radius.md = 6px` (agent card). iA uses flat rectangles; avoid pill/rounded-heavy
shapes. **Borders are hairlines (1px), never ≥2px.**

---

## 5. Caret

- **Default — thin vertical bar**, 2px wide, color = the **current mode accent**
  (`mode_accent`: shell blue `accent.primary`, agent purple `accent.agent`). Per
  ADR-0011 the caret tints by mode (this resolves former OQ5); with the prompt
  glyph and the mode chip it is the visible mode indicator.
- **Block caret** option for vi-normal-mode / raw-mode apps. When the PTY
  requests `DECSCUSR` block/underline, honor it. Block caret = the mode accent at
  ~70% opacity with the glyph drawn in `bg.canvas` (inverse).
- **Soft blink**: a smooth opacity ramp, not a hard toggle. Blink is **suppressed
  while typing** and resumes after idle (standard terminal behavior).
- The routing target is signaled together by the caret tint, the prompt glyph
  (`❯` shell / `◇` agent), and the SHELL/AGENT chip (§7) - all in the mode color.

The caret is the one place blink/opacity animation runs continuously; budget for
it in the frame loop (it is a single rectangle, cheap).

---

## 6. Motion budget

aterm owns a 60fps floor (120fps ProMotion). Motion must never threaten it.

- **At most 3 animation kinds**, all ≤ **220ms**, decelerate easing
  `cubic-bezier(0.2, 0, 0, 1)`:
  1. **Block insert** — fade in + 4px rise (`motion.base` 140ms).
  2. **Gate badge state change** — cross-fade (`motion.fast` 90ms).
  3. **Focus dim** — opacity ramp of non-active blocks (`motion.slow` 220ms).
- **No decorative spinners.** A running state is a single subtle **pulsing dot**
  at `accent.primary` in the gutter.
- Caret blink (§5) is a soft opacity ramp, exempt from the 3-animation count
  because it is the cursor, not chrome.

Durations are the `[motion]` tokens: `motion.fast = 90`, `motion.base = 140`,
`motion.slow = 220` (ms).

---

## 7. Component specs

All components live in `aterm-ui` and consume `aterm-tokens`. Colors below are
token names; resolve per active theme.

### Command block

One human-entered command and its output (the mock's `shell` state; T-9.3).

- **Left gutter** (~`space.4` wide): the accent **`❯` prompt glyph** (the shell
  mode accent), not a status icon - the status moves to the block-meta.
- **Block-meta** (right-aligned, `type.caption`, `fg.muted`): a status dot +
  duration. The mock reveals it on block hover; the `focus dim` slot (§6, not a
  fourth animation) is reserved for that fade, but - like every other animation
  today - it is not yet time-driven, so the meta currently renders always-on
  (hover-gating lands with the frame clock + pointer plumbing).
  - running → pulsing `accent.primary` dot + "running";
  - exit 0 → `fg.muted` dot + duration (a longer run, ≥ ~1s, earns the loud
    `success` dot);
  - exit≠0 → `danger` dot + "exit N · Ns".
  Color is never the only signal: the "exit N" / "running" / "approx" / "tui"
  labels and the distinct dot shapes (dot / hollow / half / caret) carry each
  state for a color-blind eye.
- **Delimiter**: a single `hairline` top rule per block (none above the first).
  No box, no shadow.
- **Command line**: `font.grid`, `fg.primary`. Re-rendered (not raw).
- **Output**: `font.grid`, indented under the command in `fg.secondary` (the
  mock's dimmed body); explicit ANSI / 256-color / RGB is preserved, on
  `bg.canvas`. Prefer canvas over `bg.surface_alt` for flatness.
- **Collapsed state**: long output collapses to N lines with a `fg.muted`
  "... +123 lines" affordance (ASCII "..."; U+2026 is `.notdef` in the Mono face).

### Prompt (the unified input box)

Shell-first, **one box** (locked decision 3). A hotkey toggles where Enter
routes; typed text is preserved across the toggle (a pure `InputModel` reducer
concern in `aterm-app`, not a UI concern).

- Single full-width input, `font.grid`, `fg.primary`, caret = thin bar in the
  current mode accent (§5). The prompt glyph (left) is `❯` (shell) / `◊` (agent -
  the mock's `◇` U+25C7 is `.notdef` in the bundled Mono face, so the nearest
  present diamond outline stands in), tinted to the mode accent.
- **Mode chip** (right, T-9.4) - a pill in the CURRENT MODE color, the visible
  mode indicator the spec mandates (no banner): a 1px accent border, a ~13%
  accent tint fill over the canvas, and accent text; contents = the mode glyph +
  label ("Shell" / "Agent") + a `⌘I` shortcut hint (the `⌘` is a Nerd-Font PUA
  icon - Unicode U+2318 is `.notdef`). Shell = `accent.primary` (blue), Agent =
  `accent.agent` (purple). The slot is sized to the wider mode so a toggle never
  reflows.
- Toggling cross-fades the chip (`motion.fast`) and shifts the caret + prompt
  glyph to the new mode accent (§5). Text is preserved across toggle.
- `hairline` above the input separates it from the timeline; the input sits in a
  persistent bottom zone with `space.4` padding.

### Agent card

One step in the agentic transcript.

- `bg.surface`, `radius.md`, 1px `hairline` border, `space.4` padding,
  `space.6` vertical gap from neighbors. The single hairline border is the only
  permitted card edge — no shadow.
- **Header row**: step title in `type.heading` (`font.prose` medium 500) + a
  `status chip` (planning / running / done / error).
- **Body**: prose in `font.prose`, `type.body`, capped at `type.measure_ch`
  (72ch). Reasoning/plan text may use `fg.secondary` to de-emphasize versus
  user-facing conclusions (echoes Focus Mode's hierarchy).
- **Proposed tool calls / commands**: render as nested mini command blocks
  (`font.grid`) with a risk-gate badge inline.

Implementation note (T-9.6): the shipped agent turn realizes the mock's `agent`
state directly in the block timeline rather than as a discrete `bg.surface` card - a
turn reads as one grouped unit by SUPPRESSING the inter-step boundary hairline
(kept only above the `◊` header and the closing summary), not by drawing a card
edge. The header is the agent-accent `◊` glyph + request + an "agent - N steps"
meta; the plan carries an uppercase eyebrow; tool rows are the tool name
(`accent.primary`) + its sanitized argument (`fg.muted`) + a right-aligned "+N -M"
on an edit; tool output sits in a hairline LEFT-bordered block with `+`/`-` diff and
FAILED/ok coloring; the summary is `fg.primary`. The `bg.surface` card container +
72ch prose measure remain the target for a future pass (this re-skin stays on the
Mono grid path for pixel-parity with command output).

### Status chip (generic small pill)

- `radius.sm`, `type.label` (`font.ui`), `space.1` / `space.2` padding.
- Variants: **neutral** (`bg.surface` / `fg.secondary`, hairline border),
  **info** (`accent.primary_weak` / `accent.primary`), **success / caution /
  danger** (weak tint fill + the saturated semantic color as text). Only neutral
  carries a hairline border.

### Risk-gate badge

The deterministic, code-side risk-gate verdict (locked decision 4; a KEEP from
the prototype). Default autonomy is **AUTO-SAFE ON**: commands proven `Safe`
with no shell-active reason auto-run; `Caution` / `Dangerous` always require
explicit confirmation.

Three states, mapped to the semantic colors so the verdict is legible at a
glance, and **always paired with a text label (never color alone — color-blind
safety):**

- **Safe** → `success`. Filled dot, label `auto` (or silent — no badge — when
  policy is silent-allow). These auto-run by default.
- **Caution** → `caution`. Filled chip `APPROVE?`, parsed reason in
  `type.caption` on hover/expand. The interactive gate; requires confirmation.
- **Dangerous** → `danger`. Chip `BLOCKED`, reason shown; requires an explicit
  override.

The badge sits inline at the head of the proposed command's mini-block, aligned
to the gutter, so a scanning eye reads gutter color = safety state. How loud the
`Caution` state is (quiet chip vs interrupting banner) is an open owner question
(`07-ia-design-language.md` OQ3); default is the quiet chip to preserve rhythm.

### Shell-integration indicator (3-state)

aterm interprets OSC-133 / OSC-7 marks emitted by the shell-integration shim,
gated by a nonce (`aterm-core`; see `04-shell-integration.md` for the mechanism). The
indicator tells the user whether block detection is trustworthy. A small
`status chip` (`type.label`, `font.ui`) in the status strip, three states:

The states are the `IntegrationStatus { Integrated, Heuristic, None }` enum (see
`domain.md`, ADR-0008, and ticket T-2.6) - name them by the enum; the
`shell ✓/~/✗` glyphs are the rendered affordance, not the state name.

- **Integrated** → `success` weak tint, label `shell ✓`. Shim installed, nonce
  matches, OSC-133 marks observed; block boundaries are authoritative.
- **Heuristic** → `caution` weak tint, label `shell ~`. Shim present but marks
  missing/unverified (nonce mismatch, partial marks); aterm falls back to
  heuristic block segmentation - boundaries may be approximate.
- **None** → neutral (`bg.surface` / `fg.secondary`), label `shell ✗`. No
  integration; pure heuristic mode. Some agent features that depend on reliable
  exit codes / cwd may be limited.

Color + label always together; the chip is quiet (status-strip register), never
an interrupting banner.

---

## 8. Open questions for the owner

Carried from `07-ia-design-language.md`; none block token wiring, but each
changes a default:

1. **Default theme on first launch** — follow macOS system appearance, or
   default to "paper" light (iA's most iconic look)?
2. **Focus-Mode dimming in v1**, or a later polish item? (Tokens exist either way.)
3. **Risk-gate loudness** — `Caution` as a quiet chip (current default) vs a
   full-width interrupting banner.
4. **Honor terminal-app color/caret requests** (DECSCUSR, OSC palette overrides)
   verbatim, or enforce the aterm theme for cohesion?
5. ~~**Routing-target caret tint**~~ — **RESOLVED by ADR-0011:** the caret (and
   the prompt glyph) tint to the current mode accent (shell blue / agent purple).
6. ~~**Confirm the accent blue**~~ — **RESOLVED by ADR-0011:** the accent is the
   mock's blue (`#2F7DC2` light / `#3D88CC` dark), by owner decision; the derived
   iA-adjacent blue is retired.
