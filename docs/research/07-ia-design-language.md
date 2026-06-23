---
title: iA Writer Design Language -> aterm Design System
domain: ia-design-language
status: research
---

# iA Writer Design Language -> aterm Design System

## TL;DR

- iA Writer's design language is **radical subtraction**: no chrome, no toolbars, no popups, "no buttons, no popups, no title bar" [1]. The content (text) is the entire UI; everything else is hairlines, whitespace, and one accent. Map this directly to aterm: the timeline of blocks *is* the window; the input box and a thin status strip are the only persistent chrome.
- Typography is the brand. iA uses a **monospace-by-default** philosophy ("text is work in progress") with three width systems: **Mono** (all glyphs 1x), **Duo** (a "duospace" - m/M/w/W get ~1.5x), **Quattro** (4 widths, near-proportional) [2][3]. Our bundle maps cleanly: **iM Writing Mono NFM** for the terminal grid (constant advance, mandatory), **Duo** for agent prose / UI labels, **Quattro** for denser proportional chrome.
- The signature accent is a **single blue**, used almost exclusively for the caret and links - scarcity is the point. I recommend **`#1A93E8`** (light) / **`#4DA6F0`** (dark) as the aterm primary accent; I could NOT pin the exact iA app hex to a primary source, so treat these as derived-and-WCAG-checked, not "the iA blue" (see Risks).
- Two themes are non-negotiable: a warm-neutral **"paper" light** and a **dark**. I derive both from the open-source "Pencil" scheme (the de-facto iA-Writer-inspired terminal palette, MIT) [4][5] plus the iA-Writer Sublime dark scheme [6], so the ANSI 16-color sets and the UI tokens come from the *same* hue family - critical because aterm renders both raw ANSI terminal output and its own UI in one surface.
- Concrete deliverable below: a **semantic token set** (light + dark hex), **two ANSI-16 palettes** tuned per theme, a **spacing/type scale**, motion + caret rules, and component specs for the command block, prompt, agent card, status chip, and risk-gate badge.
- iA's measure is **64 / 72 / 80 characters** (their three line-length options) [7]. For aterm's agent-prose column this is the target measure; the terminal grid itself is unconstrained (PTY-driven columns).

## Findings

### 1. Philosophy: content-first, chrome-less, "omit needless words"

iA Writer's stated design ethos is Strunk & White applied to UI: *"Omit needless words"* expressed as visual restraint - "No buttons, no popups, no title bar" [1]. The product surfaces almost nothing but the text and a blinking caret. Two signature behaviors:

- **Focus Mode** - dims everything except the active sentence/paragraph, fading the rest to a muted gray (typewriter-style concentration) [1].
- **Syntax Highlight** - an *editorial* mode that tints parts of speech (nouns/verbs/adjectives/adverbs) to expose sentence structure; off by default. Not relevant to a terminal grid directly, but the *idea* - semantic color applied sparingly and only on demand - is the model for how aterm should color command output and agent reasoning.

Design takeaways for aterm (a terminal, not an editor):
- The block timeline is the document. No tab bar chrome, no sidebars by default, no window title bar (use a borderless/transparent-titlebar NSWindow). Hairline separators between blocks, not boxes/cards with heavy borders.
- Generous vertical rhythm between blocks; left gutter reserved for a thin status marker (exit code / running / risk badge) rather than heavy iconography.
- A "focus" affordance is natural here: dim completed blocks, keep the active/running block and the input at full contrast (a direct analog of Focus Mode).

### 2. Typography: Mono / Duo / Quattro and the duospace idea

iA's font reasoning [2][3][8]:
- **Monospace = honesty.** Monospace signals "work in progress"; proportional signals "almost finished." A writing tool should look like a draft. "Every letter, every number, every punctuation mark and every space takes the same visual space." Larger word spacing aids typo-spotting and glyph discernibility [2].
- **Duospace ("Duo")** keeps the monospace benefits but relaxes the worst constraint: the cramped **m/M/w/W** get **50% extra width (1.5x)**, an idea borrowed from the single/double-width model of CJK typography [2][8]. Net effect: still grid-like and "draft," but reads more comfortably.
- **Quattro** uses **four character widths** - giving extra width to m/w while *reducing* f/i/l/r/s - to approach a proportional feel while keeping "wider gaps between words" and the typewriter virtues; "saves space on small screens" [3][9].
- All three are **variable fonts**, derived from **IBM Plex Mono** (same designer lineage as Bold Monday's Nitti), with iA's modifications: square dots instead of round, reworked swirls/curves on `a j f l t y Q`, and a retained **single-storey lowercase g** ("makes the text image more homogenous, calmer") [2][8][10]. iA abandoned an in-house duospace ("iA 735", 735 variations) when they found Plex [2].
- Weights in the Quattro family: **Regular 400, Medium 500, SemiBold 600, Bold 700**, each with italics [9].
- The iA-Fonts repo states the fonts are a **modification of IBM Plex** and asks downstream users to "review licensing" and "Don't recreate iA Writer or iA Writer Themes. Use them creatively." [10] (This matters for our reserved-name handling - see Risks.)

**Mapping to the aterm `iM Writing Nerd Font` bundle** (decided already):

| Role | Font | Rationale |
|---|---|---|
| Terminal grid (command input echo, PTY output, code/diffs) | **iM Writing Mono NFM** (constant advance) | A terminal grid REQUIRES constant cell advance for column alignment; Duo/Quattro's variable widths would break box-drawing, tables, TUI apps. NFM = "Nerd Font Mono" = single-width icon glyphs, safe in the grid. |
| Agent prose / explanations / chat-like transcript text | **iM Writing Duo** | Duospace reads as comfortable prose while staying visually kin to the grid - the "almost monospaced" register iA uses for body text. Proportional-ish but still draft-honest. |
| Dense UI chrome: status strip, chip labels, command-palette rows, settings | **iM Writing Quattro** | Most space-efficient of the three; near-proportional; good for small fixed labels where every pixel of width counts. |

Type scale (in pt; assume macOS @2x; tune to the chosen renderer's px):

| Token | Size | Line height | Use |
|---|---|---|---|
| `type.grid` | 13 | 1.30 (≈17px) | terminal cells (line-height is critical for scroll rhythm; iA-ish leading) |
| `type.body` | 14 | 1.50 | agent prose (Duo); 1.5 matches iA template line-height [11] |
| `type.label` | 11 | 1.30 | chips, status, gutter (Quattro) |
| `type.heading` | 16 | 1.35 | block headers / agent step titles (Duo medium 500) |
| `type.caption` | 10.5 | 1.30 | timestamps, exit codes, secondary meta |

Measure: agent prose column capped at **~72ch** (iA's middle option of 64/72/80 [7]); never cap the terminal grid (it follows PTY columns).

### 3. Color: light "paper" + dark, derived from one hue family

Two source palettes anchor the derivation so UI and ANSI share hues:
- **Pencil** (reedes/vim-colors-pencil, the canonical iA-Writer-inspired scheme; ports exist for terminals, MIT) [4][5][12].
- **iA-Writer Sublime dark** (acheronfail) for the dark accent/syntax family [6].

Raw source values used (verbatim from sources):

Pencil **light**: bg `#F1F1F1`, fg `#424242`, fg-subtle `#545454`, blue `#005F87`, cyan `#20A5BA`, green `#10A778`, yellow `#A89C14`, red `#C30771`, purple `#523C79`, pink `#FB007A`, selection `#B6D6FD`, subtle-bg `#D9D9D9`, very-subtle-bg `#E5E6E6` [4].

Pencil **dark**: bg `#212121`, fg `#E5E6E6`, fg-subtle `#D9D9D9`, blue `#20BBFC`, cyan `#4FB8CC`, green `#5FD7A7`, yellow `#F3E430`, red `#E32791`, purple `#6855DE`, pink `#FB007A`, selection `#545454`, subtle-bg `#424242`, very-subtle-bg `#262626` [4].

iA-Writer Sublime **dark** (alt syntax family): bg `#1D1F20`, fg `#C5C9C6`, muted `#707070`, comment `#525252`, accent-cyan `#15BDEC`, red `#F2777A`, green `#B1BE5A`, yellow `#F2B160`, blue `#7AA4C2`, purple `#B893BE`, orange `#EA9052` [6].

#### Derived aterm semantic tokens

I warmed the light background slightly toward "paper" (off-white, not the cool `#F1F1F1`) and chose a true blue accent rather than Pencil's teal-leaning `#005F87`/`#20BBFC`, to match iA Writer's actual link/caret blue register. All accent-on-bg pairs below were contrast-checked (see the table after).

| Token | Light value | Dark value | Notes |
|---|---|---|---|
| `bg.canvas` | `#FAF9F6` | `#1C1C1C` | the "paper" / the void. Light warmed off `#F1F1F1`; dark between Pencil `#212121` and Sublime `#1D1F20`. |
| `bg.surface` | `#F1F0EC` | `#262626` | raised block / card fill (very subtle) |
| `bg.surface.alt` | `#E9E7E1` | `#303030` | code/output block fill, hover rows |
| `fg.primary` | `#2A2A28` | `#E6E5E1` | body text / grid default fg |
| `fg.secondary` | `#5C5B57` | `#B8B7B2` | secondary meta |
| `fg.muted` | `#8A8984` | `#7A7A75` | dimmed (Focus-Mode faded blocks, placeholders) |
| `fg.faint` | `#B5B4AE` | `#4A4A46` | hairline-adjacent text, disabled |
| `accent.primary` | `#1A93E8` | `#4DA6F0` | THE blue: caret, links, active target indicator, focus ring (derived, not source-verified) |
| `accent.primary.weak` | `#D6EAFB` | `#1E3A52` | accent fill at low emphasis (selection-of-target, badges) |
| `hairline` | `#E0DED8` | `#343433` | the 1px separators between blocks (iA's signature) |
| `hairline.strong` | `#C9C7C0` | `#454544` | section dividers |
| `selection.bg` | `#CFE3F7` | `#34465A` | text selection (light from Pencil `#B6D6FD` family, warmed) |
| `success` | `#1E8E5A` | `#5FD7A7` | exit 0, allowed gate (Pencil green family) |
| `caution` | `#B0820E` | `#E0B341` | needs-approval gate, warnings (Pencil yellow family) |
| `danger` | `#C2185B` | `#E85A95` | exit≠0, blocked/destructive gate (Pencil red/pink family) |
| `info` | `#1A93E8` | `#4DA6F0` | = accent.primary (one blue, reused) |

Note: `success/caution/danger` deliberately echo iA Writer's syntax-highlight hue family (green/yellow/magenta-red) rather than generic web traffic-light colors, so the risk gate reads as part of the same design system, not a bolted-on alert UI.

#### ANSI 16-color palettes (tuned per theme)

Terminal output must look correct AND belong to the theme. Both sets keep ANSI hues recognizable while pulling saturation/value toward the Pencil family. Bright = lighter/more saturated sibling.

**Light "paper" ANSI:**

| Index | Name | Hex | | Index | Name | Hex |
|---|---|---|---|---|---|---|
| 0 | black | `#2A2A28` | | 8 | br.black | `#5C5B57` |
| 1 | red | `#C30771` | | 9 | br.red | `#E0306F` |
| 2 | green | `#10A778` | | 10 | br.green | `#1EB886` |
| 3 | yellow | `#A8800E` | | 11 | br.yellow | `#C39A14` |
| 4 | blue | `#1A6FB0` | | 12 | br.blue | `#1A93E8` |
| 5 | magenta | `#7C3F9E` | | 13 | br.magenta | `#9B5BC0` |
| 6 | cyan | `#138D9E` | | 14 | br.cyan | `#20A5BA` |
| 7 | white | `#5C5B57` | | 15 | br.white | `#2A2A28` |

(On a light bg, ANSI "white" is the dark text and "black" is the darkest - standard for light terminal themes; foreground default = `fg.primary`.)

**Dark ANSI:**

| Index | Name | Hex | | Index | Name | Hex |
|---|---|---|---|---|---|---|
| 0 | black | `#1C1C1C` | | 8 | br.black | `#5A5A55` |
| 1 | red | `#E85A95` | | 9 | br.red | `#F277A8` |
| 2 | green | `#5FD7A7` | | 10 | br.green | `#7DE6BC` |
| 3 | yellow | `#E0B341` | | 11 | br.yellow | `#F3E430` |
| 4 | blue | `#4DA6F0` | | 12 | br.blue | `#74BFF7` |
| 5 | magenta | `#B893BE` | | 13 | br.magenta | `#CBAAD0` |
| 6 | cyan | `#4FB8CC` | | 14 | br.cyan | `#6FCFE0` |
| 7 | white | `#E6E5E1` | | 15 | br.white | `#FFFFFF` |

#### Contrast (WCAG AA target = 4.5:1 body, 3:1 large/UI)

Approximate computed ratios (sRGB, against the relevant bg):
- Light `fg.primary #2A2A28` on `bg.canvas #FAF9F6` ≈ **13.8:1** (pass AAA).
- Light `accent.primary #1A93E8` on `#FAF9F6` ≈ **3.3:1** - passes AA for large/UI/links, **borderline for small body text**; use accent for caret/links/large elements, not 11px body. Darken to `#1577C2` (≈ 4.7:1) when accent must carry small text.
- Dark `fg.primary #E6E5E1` on `bg.canvas #1C1C1C` ≈ **13.5:1** (AAA).
- Dark `accent.primary #4DA6F0` on `#1C1C1C` ≈ **6.4:1** (AA body, comfortable).
These are derived estimates (not from a source); validate with a real contrast lib before locking tokens (Risks).

### 4. Spacing, rhythm, motion, caret

**Spacing scale** (4px base; iA leans on whitespace, so default to the larger steps for block separation):

| Token | px | Use |
|---|---|---|
| `space.0` | 0 | |
| `space.1` | 4 | inline gaps (chip padding) |
| `space.2` | 8 | label-to-value, icon-to-text |
| `space.3` | 12 | intra-block padding |
| `space.4` | 16 | block content padding (horizontal gutter) |
| `space.6` | 24 | between blocks (vertical rhythm) |
| `space.8` | 32 | section breaks, agent-card outer margin |
| `space.12` | 48 | top/bottom canvas breathing room |

Radii: keep tiny - `radius.sm = 4px` (chips/badges), `radius.md = 6px` (agent card). iA uses essentially flat rectangles; avoid pill/rounded-heavy shapes. Borders are hairlines (`1px` `hairline`), never 2px+.

**Motion** (minimal, purposeful - this is a 60fps-floor product, so motion must never threaten frame budget):
- Durations: `motion.fast = 90ms`, `motion.base = 140ms`, `motion.slow = 220ms`. Easing: `cubic-bezier(0.2, 0, 0, 1)` (standard decelerate).
- Only animate: block insert (fade + 4px rise), gate badge state change (cross-fade), focus dim (opacity of non-active blocks). NO spinners-as-decoration; running state = a single, subtle pulsing dot at `accent.primary`.
- Caret blink should be a smooth opacity ramp, not a hard toggle (iA's caret reads "soft"). Disable blink entirely while typing (resume after idle), standard terminal behavior.

**Caret styles:**
- Default: **thin vertical bar**, 2px wide, color `accent.primary` (the iA "blue cursor blinking" [1]). This is the brand caret.
- Provide a **block caret** option for vi-normal-mode / terminal-app raw modes (when the PTY requests `DECSCUSR` block/underline, honor it). Block caret uses `accent.primary` at ~70% with `bg.canvas` glyph (inverse).
- Caret in the unified input box also signals routing target indirectly; the explicit target indicator is a separate chip (see components).

### 5. Component guidance

**Command block** (one human-entered command + its output):
- Left gutter (~`space.4` wide): a status marker - running = pulsing `accent.primary` dot; exit 0 = `success` tick (thin); exit≠0 = `danger` dot + exit code in `type.caption`. No heavy box; the block is delimited by `hairline` top/bottom only.
- Command line: `iM Writing Mono NFM`, `fg.primary`. Re-rendered (not raw) so it can carry the gutter + timestamp.
- Output: Mono NFM, ANSI palette per theme, full width (no measure cap), `bg.canvas` (or `bg.surface.alt` if a "raised output" treatment is wanted - prefer canvas for flatness).
- Collapsed state: long output collapses to N lines with a `fg.muted` "… +123 lines" affordance.

**Prompt (the unified input box)** - shell-first, one box:
- A single full-width input, `iM Writing Mono NFM`, `fg.primary`, caret = thin blue bar.
- **Routing-target indicator**: a small `status chip` at the input's left edge: `SHELL` (neutral - `bg.surface` fill, `fg.secondary` text) vs `AGENT` (accent - `accent.primary.weak` fill, `accent.primary` text). The chip is the *visible indicator* the spec requires; toggling the hotkey cross-fades the chip (motion.fast) and the caret tint shifts subtly. Text is preserved across toggle (state, not UI, concern - flagged for the input-handling domain).
- Hairline above the input separating it from the timeline; the input sits in a persistent bottom zone with `space.4` padding.

**Agent card** (a step in the agentic transcript):
- `bg.surface`, `radius.md`, `1px hairline` border, `space.4` padding, `space.6` vertical gap from neighbors.
- Header row: step title in `type.heading` (Duo medium 500) + a `status chip` (planning / running / done / error). Prose body in `iM Writing Duo`, `type.body`, capped at ~72ch measure.
- Tool calls / commands the agent proposes render as nested mini command blocks (Mono NFM) with a `risk-gate badge` inline.
- Reasoning/plan text can use the muted `fg.secondary` to de-emphasize vs. user-facing conclusions (echoes Focus Mode's contrast hierarchy).

**Status chip** (generic small pill):
- `radius.sm`, `type.label` (Quattro), `space.1`/`space.2` padding. Variants: neutral (`bg.surface`/`fg.secondary`), info (`accent.primary.weak`/`accent.primary`), success/caution/danger (weak tint of each semantic color + the saturated color as text). Hairline border only on neutral.

**Risk-gate badge** (the code-side safety gate verdict - a KEEP from the prototype):
- Three states mapped to semantic colors so the verdict is legible at a glance:
  - **Allowed** -> `success` (filled dot + "auto" or no badge if policy is silent-allow).
  - **Needs approval** -> `caution` filled chip "APPROVE?" with the parsed reason in `type.caption` on hover/expand; this is the interactive gate.
  - **Blocked / destructive** -> `danger` chip "BLOCKED" with reason; requires explicit override.
- Badge sits inline at the head of the proposed command's mini-block, in the left gutter alignment, so a scanning eye reads gutter color = safety state. Color is the *only* fast signal; always pair with a text label (color-blind safety - never color alone).

## Recommendations for aterm

1. **Two themes from one hue family, shipped day one: "paper" light + dark.** Derive UI tokens and ANSI palettes together (as above) so terminal output and app UI never clash. (High)
2. **Mono NFM for the grid, Duo for prose, Quattro for chrome** - exactly the three-register split iA uses. Never put a variable-width font in the terminal grid. (High)
3. **One accent blue, used scarcely** (caret, links, active routing target, focus ring). Scarcity is the iA signature; resist the urge to color UI. (High)
4. **Risk-gate colors = iA syntax-highlight hue family** (green/yellow/magenta-red), not generic alert colors, and always color+label (never color alone). (Med - color-blind + brand-cohesion rationale)
5. **Measure-cap agent prose at ~72ch; leave the grid uncapped.** (Med - 72 is iA's middle option [7]; the grid must follow PTY columns.)
6. **Caret: thin 2px blue bar by default, honor DECSCUSR for block/underline in raw-mode apps; soft opacity blink, suppressed while typing.** (High)
7. **Motion budget: only 3 animations (block insert, gate state, focus dim), all <=220ms, decelerate easing; running = single pulsing dot, no decorative spinners** - protects the 60fps floor. (High)
8. **Pin the accent blue and re-run WCAG before locking tokens.** Use `#1577C2` for accent-bearing small text on light bg (AA), keep `#1A93E8` for large/UI. (Med)
9. **Borderless window, hairline separators, flat rectangles, generous whitespace** - the chrome-less iA shell. Avoid cards-with-shadows except the agent card's single hairline. (High)
10. **Implement a Focus-Mode analog**: dim non-active blocks to `fg.muted`/lowered opacity, keep the running block + input at full contrast. (Low - nice-to-have, but cheap and on-brand)

## Risks & unknowns

- **The exact iA Writer app accent-blue hex is NOT source-verified.** iA's templates use HTML/CSS but the docs don't publish the link/caret hex, and the iA-Fonts/template repos I reached didn't expose it in the fetched views [10][13]. `#1A93E8`/`#4DA6F0` are my derived, contrast-aware choices - defensible and iA-adjacent, but do not claim they are "the iA blue." The Pencil scheme's blues are teal-leaning (`#005F87`/`#20BBFC`) [4]; the Sublime dark accent is cyan `#15BDEC` [6] - both diverge from a true link-blue.
- **All contrast ratios above are computed estimates, not from a source.** Validate with a real WCAG library against final hexes before locking design tokens.
- **Font naming / reserved names.** iA asks downstream users to review licensing and not "recreate iA Writer or iA Writer Themes" [10]; the project decision already renames to "iMWriting" to avoid the reserved "iA Writer"/"Plex" names under OFL. Confirm the bundled `iM Writing Nerd Font` patched set is itself OFL-clean (Nerd Fonts patching + iA's Plex modification = two upstream licenses to honor); this is a legal/licensing-domain check, not mine to settle.
- **Duo/Quattro precise metrics** (exact 1.5x advance, x-height, cap-height numbers) are described qualitatively in iA's essays but I found no published numeric table [2][3][8]; measure them from the actual font files before building any layout that depends on Duo advance widths.
- **ANSI tuning is taste, not spec.** My 16-color sets are hand-derived from Pencil; they should be eyeballed against real output (ls --color, vim, htop, git diff) on both themes before shipping. Some TUI apps assume high-contrast pure ANSI and may look washed out on a warm-paper bg.
- **Light "paper" + heavy ANSI output** is the riskiest combination: many CLI tools' default colors (bright cyan/yellow) are near-invisible on light bg. May need an output-time remap or a "boost saturation on light theme" toggle.

## Open questions for the product owner

1. Default theme on first launch - follow macOS system appearance, or default to "paper" light (iA's most iconic look)?
2. Is a Focus-Mode analog (dimming inactive blocks) in scope for v1, or a later polish item?
3. How loud should the risk gate be visually? "Needs approval" as a quiet caution chip vs. a full-width interrupting banner changes the whole rhythm.
4. Do we honor terminal-app color requests verbatim (DECSCUSR caret, OSC palette overrides) or enforce the aterm theme for visual consistency? (Trade-off: app compatibility vs. brand cohesion.)
5. Should the routing-target indicator (SHELL/AGENT) also recolor the caret, or keep the caret always-blue and signal target only via the chip? (Affects how strong the toggle feedback is.)
6. Confirm the exact accent blue - accept the derived `#1A93E8`/`#4DA6F0`, or do you want it sampled from the live iA Writer app for fidelity?

## Sources

1. iA Writer (product page) - https://ia.net/writer
2. In Search of the Perfect Writing Font (iA) - https://ia.net/topics/in-search-of-the-perfect-writing-font
3. A Typographic Christmas (iA, Mono/Duo/Quattro) - https://ia.net/topics/a-typographic-christmas
4. vim-colors-pencil source (reedes) - https://raw.githubusercontent.com/reedes/vim-colors-pencil/master/colors/pencil.vim
5. term-colors-pencil (gummesson) - https://github.com/gummesson/term-colors-pencil
6. iA Writer dark Sublime color scheme (acheronfail) - https://github.com/acheronfail/ia-writer-sublime/blob/master/ia-writer-dark.sublime-color-scheme
7. iA Writer Settings (characters per line 64/72/80) - https://ia.net/writer/support/basics/settings
8. iA Writer Duospace (referenced via iA fonts repo) - https://github.com/iaolo/iA-Fonts
9. Complete Guide to iA Writer Quattro (Beautiful Web Type) - https://www.beautifulwebtype.com/ia-writer-quattro/
10. iA-Fonts repository (iaolo) - https://github.com/iaolo/iA-Fonts
11. iA Writer HTML template CSS (line-height 1.5em, max-width 45em) - https://gist.github.com/alexcabrera/1046827
12. Pencil for iTerm (mattly) - https://github.com/mattly/iterm-colors-pencil
13. iA Writer Templates - https://ia.net/writer/support/preview/templates
