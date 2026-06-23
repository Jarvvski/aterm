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

> **OWNER CONFIRMATION REQUIRED.** The signature accent blue
> (`#1A93E8` light / `#4DA6F0` dark) is **DERIVED and WCAG-checked, NOT sampled
> from the live iA Writer app** - the exact iA app accent hex is not
> source-verified (see `07-ia-design-language.md` Risks). Treat it as a
> defensible iA-adjacent choice pending owner sign-off, not as "the iA blue."
> The same caveat applies to every contrast ratio quoted below: they are
> computed estimates, to be re-validated against final hexes with a real WCAG
> library before the tokens are locked.

---

## 1. Philosophy

aterm's UI is **radical restraint**: the timeline of command/agent blocks *is*
the window. There is no toolbar, no tab strip chrome, no sidebar by default, no
native title bar (borderless / transparent-titlebar window). Everything that is
not content is a hairline, whitespace, or the single accent. This is iA
Writer's "omit needless words" applied to a terminal (`07-ia-design-language.md`
§1).

Five rules govern every component:

1. **Chrome-less.** Flat rectangles, hairline separators, no drop shadows
   (the one exception: the agent card carries a single hairline border, never a
   shadow). Blocks are delimited by `hairline` top/bottom rules, not boxes.
2. **One accent, used scarcely.** The accent blue appears only on the caret,
   links, the active routing-target indicator, and the focus ring. Resist
   coloring UI for decoration; scarcity is the iA signature.
3. **Whitespace is the layout.** Generous vertical rhythm between blocks
   (`space.6`+). A thin left gutter carries the only persistent status
   iconography (exit code / running / risk badge).
4. **Paper, two ways.** Two themes ship day one and are non-negotiable: a
   warm-neutral **"paper" light** and a **dark**, both derived from one hue
   family (the Pencil scheme + iA-Writer-Sublime dark) so raw ANSI terminal
   output and aterm's own UI never clash in the same surface.
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

Two themes, one hue family. Light is warmed toward "paper" (off-white, not a
cool gray); dark sits between Pencil `#212121` and iA-Sublime `#1D1F20`. Both
derivations are documented in `07-ia-design-language.md` §3. All accent-on-bg
pairs were contrast-checked (estimates — re-validate before locking).

### Semantic tokens (`[color.light]` / `[color.dark]`)

| Token | Light | Dark | Role |
|---|---|---|---|
| `bg.canvas` | `#FAF9F6` | `#1C1C1C` | the paper / the void; default surface |
| `bg.surface` | `#F1F0EC` | `#262626` | raised block / agent-card fill (subtle) |
| `bg.surface_alt` | `#E9E7E1` | `#303030` | code/output block fill, hover rows |
| `fg.primary` | `#2A2A28` | `#E6E5E1` | body text / grid default foreground |
| `fg.secondary` | `#5C5B57` | `#B8B7B2` | secondary meta, de-emphasized reasoning |
| `fg.muted` | `#8A8984` | `#7A7A75` | Focus-dimmed blocks, placeholders |
| `fg.faint` | `#B5B4AE` | `#4A4A46` | hairline-adjacent text, disabled |
| `accent.primary` | `#1A93E8` | `#4DA6F0` | **THE blue** — caret, links, active target, focus ring *(derived; confirm)* |
| `accent.primary_text` | `#1577C2` | `#4DA6F0` | accent when it must carry small body text (AA on its bg) |
| `accent.primary_weak` | `#D6EAFB` | `#1E3A52` | low-emphasis accent fill (badges, target chip) |
| `hairline` | `#E0DED8` | `#343433` | the 1px separators between blocks (the iA signature) |
| `hairline_strong` | `#C9C7C0` | `#454544` | section dividers |
| `selection_bg` | `#CFE3F7` | `#34465A` | text selection |
| `success` | `#1E8E5A` | `#5FD7A7` | exit 0, Safe gate verdict |
| `caution` | `#B0820E` | `#E0B341` | needs-approval gate, warnings |
| `danger` | `#C2185B` | `#E85A95` | exit≠0, blocked/destructive gate |
| `info` | `#1A93E8` | `#4DA6F0` | = `accent.primary` (one blue, reused) |

`success` / `caution` / `danger` deliberately echo iA's syntax-highlight hue
family (green / yellow / magenta-red), **not** generic web traffic-light colors,
so the risk gate reads as part of the system rather than a bolted-on alert UI.

> **Light paper + heavy ANSI is the riskiest combination.** Many CLI tools'
> default bright cyan/yellow are near-invisible on a warm-paper background. An
> output-time saturation boost (or remap) on the light theme may be required;
> tracked as a renderer concern, not a token (`07-ia-design-language.md` Risks).

### Contrast notes (computed estimates — re-validate)

- Light `fg.primary` on `bg.canvas` ≈ **13.8:1** (AAA).
- Dark `fg.primary` on `bg.canvas` ≈ **13.5:1** (AAA).
- Light `accent.primary #1A93E8` on `bg.canvas` ≈ **3.3:1** — passes AA for
  large/UI/links only; **borderline for small body text**. Use
  `accent.primary_text #1577C2` (≈ 4.7:1) whenever the accent carries 11pt text.
- Dark `accent.primary #4DA6F0` on `bg.canvas` ≈ **6.4:1** (AA body, comfortable).

### ANSI 16-color palettes (`[ansi.light]` / `[ansi.dark]`)

Terminal output must look correct *and* belong to the theme. Bright = the
lighter/more-saturated sibling. On light "paper", ANSI index 7 (white) is the
dark text and index 0 (black) is darkest — standard for light terminal themes;
the default output foreground is `fg.primary`.

**Light "paper":**

| Idx | Name | Hex | | Idx | Name | Hex |
|---|---|---|---|---|---|---|
| 0 | black | `#2A2A28` | | 8 | bright_black | `#5C5B57` |
| 1 | red | `#C30771` | | 9 | bright_red | `#E0306F` |
| 2 | green | `#10A778` | | 10 | bright_green | `#1EB886` |
| 3 | yellow | `#A8800E` | | 11 | bright_yellow | `#C39A14` |
| 4 | blue | `#1A6FB0` | | 12 | bright_blue | `#1A93E8` |
| 5 | magenta | `#7C3F9E` | | 13 | bright_magenta | `#9B5BC0` |
| 6 | cyan | `#138D9E` | | 14 | bright_cyan | `#20A5BA` |
| 7 | white | `#5C5B57` | | 15 | bright_white | `#2A2A28` |

**Dark:**

| Idx | Name | Hex | | Idx | Name | Hex |
|---|---|---|---|---|---|---|
| 0 | black | `#1C1C1C` | | 8 | bright_black | `#5A5A55` |
| 1 | red | `#E85A95` | | 9 | bright_red | `#F277A8` |
| 2 | green | `#5FD7A7` | | 10 | bright_green | `#7DE6BC` |
| 3 | yellow | `#E0B341` | | 11 | bright_yellow | `#F3E430` |
| 4 | blue | `#4DA6F0` | | 12 | bright_blue | `#74BFF7` |
| 5 | magenta | `#B893BE` | | 13 | bright_magenta | `#CBAAD0` |
| 6 | cyan | `#4FB8CC` | | 14 | bright_cyan | `#6FCFE0` |
| 7 | white | `#E6E5E1` | | 15 | bright_white | `#FFFFFF` |

> **ANSI tuning is taste, not spec.** Eyeball both sets against real output
> (`ls --color`, `vim`, `htop`, `git diff`) on both themes before shipping.

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

- **Default — thin vertical bar**, 2px wide, color `accent.primary`. This is the
  brand caret (iA's "blue cursor blinking").
- **Block caret** option for vi-normal-mode / raw-mode apps. When the PTY
  requests `DECSCUSR` block/underline, honor it. Block caret = `accent.primary`
  at ~70% opacity with the glyph drawn in `bg.canvas` (inverse).
- **Soft blink**: a smooth opacity ramp, not a hard toggle. Blink is **suppressed
  while typing** and resumes after idle (standard terminal behavior).
- The caret does **not** carry the routing target; the target is signaled by the
  SHELL/AGENT chip (§7). Whether the caret *also* tints by target is an open
  owner question (`07-ia-design-language.md` OQ5) — default: keep the caret
  always-blue, signal target via the chip.

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

One human-entered command and its output.

- **Left gutter** (~`space.4` wide): the only status iconography.
  - running → pulsing `accent.primary` dot;
  - exit 0 → thin `success` tick;
  - exit≠0 → `danger` dot + exit code in `type.caption`.
- **Delimiter**: `hairline` rules top and bottom. No box, no shadow.
- **Command line**: `font.grid`, `fg.primary`. Re-rendered (not raw) so it can
  carry the gutter and a timestamp (`type.caption`, `fg.secondary`).
- **Output**: `font.grid`, theme ANSI palette, full width (no measure cap), on
  `bg.canvas`. Prefer canvas over `bg.surface_alt` for flatness; use
  `bg.surface_alt` only for an explicit "raised output" treatment.
- **Collapsed state**: long output collapses to N lines with a `fg.muted`
  "… +123 lines" affordance.

### Prompt (the unified input box)

Shell-first, **one box** (locked decision 3). A hotkey toggles where Enter
routes; typed text is preserved across the toggle (a pure `InputModel` reducer
concern in `aterm-app`, not a UI concern).

- Single full-width input, `font.grid`, `fg.primary`, caret = thin blue bar.
- **Routing-target indicator** — a `status chip` at the input's left edge, the
  visible mode indicator the spec mandates (no banner):
  - `SHELL` → neutral: `bg.surface` fill, `fg.secondary` text.
  - `AGENT` → accent: `accent.primary_weak` fill, `accent.primary` text.
  - Toggling cross-fades the chip (`motion.fast`); the caret tint may shift
    subtly (see §5 / open question). Text is preserved across toggle.
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
5. **Routing-target caret tint** — recolor the caret on SHELL/AGENT toggle, or
   keep it always-blue and signal target only via the chip (current default)?
6. **Confirm the accent blue** — accept derived `#1A93E8` / `#4DA6F0`, or sample
   from the live iA Writer app for fidelity? (The headline confirmation item.)
