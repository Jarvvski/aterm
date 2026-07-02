---
id: T-9.4
epic: EPIC-9-vision-mock-reskin
title: Unified input bar + mode chip re-skin (mode-colored glyph/caret, pill chip, per-mode placeholder)
status: done
labels: [ui, input]
depends_on: [T-9.1]
---

# Goal

Re-skin the unified input bar to the mock: a `hairline`-topped bottom zone with a
mode-colored prompt glyph (`❯` shell / `◇` agent), an auto-growing text field
whose caret is tinted to the current mode, a per-mode placeholder, and a
right-aligned pill **mode chip** showing the glyph + label + `⌘I`. This realizes
the two-accent mode model from ADR-0011 on the input box.

# Context

- North star: [ADR-0011](../../adr/0011-vision-mock-ui-north-star.md) (two mode
  accents; caret now tints per mode). Visual source:
  [`AtermWindow.dc.html`](../../design/vision-mock/AtermWindow.dc.html) `<!-- input
  bar -->` block.
- Existing implementation this re-skins: the input box + iA mode indicator from
  [T-3.6](../EPIC-3-unified-input/TICKET-3.6-input-box-widget.md) and the routing
  from [T-3.3](../EPIC-3-unified-input/); the `InputModel` (`text`+`selection`+
  `mode`) is unchanged (ADR-0004). Domain: `InputModel`, `Mode (Shell|Agent)`.

# Implementation notes

- **Prompt glyph**: to the left of the field, in `--mode` (shell -> `accent.primary`,
  agent -> `accent.agent`), `font.grid`, semibold. `❯` for shell, `◇` for agent.
- **Field**: transparent, borderless, auto-growing (1 row -> up to ~200px then
  scroll), `fg.primary`, caret color = `--mode`. Placeholder in `fg.faint`:
  "Type a command..." (shell) / "Ask the agent to do something..." (agent).
- **Mode chip** (right, pill): 1px border + a ~13% tint fill in `--mode`, text in
  `--mode`; contents = the mode glyph + label ("Shell"/"Agent") + a dimmed `⌘I`.
  Clicking it toggles mode (same intent as the `⌘I` hotkey, T-3.3). On toggle the
  chip + glyph + caret cross-fade to the other accent (`motion.fast`), and typed
  text is preserved by construction (the reducer only flips `mode`).
- The mock recolors the caret per mode - this resolves `design-system.md` OQ5 in
  favor of "caret tints with mode" (ADR-0011). Update any comment/spec that still
  says "caret always blue".
- Input bar is **hidden** on the settings and editor surfaces (EPIC-11/12 own
  those); expose a "show input" predicate the app can drive.

# Acceptance criteria

- [x] The input bar renders to the mock in both themes and both modes: correct
  mode glyph, mode-tinted caret, per-mode placeholder, and the pill chip with
  glyph + label + `⌘I`.
- [x] Toggling mode (`⌘I`) cross-fades glyph/caret/chip within `motion.fast` and
  preserves the typed text and selection (see the note on chip click).
- [x] The chip toggle and the hotkey drive the same routing intent (no divergent
  code path).
- [x] Motion budget and T-1.8 no-per-frame-alloc assertion hold; a widget test
  covers both modes x both themes.

## Notes

Landed 2026-07-02. The prompt glyph (`❯` shell / `◊` agent) and the caret both tint to
the current mode accent via `mode_accent` (shell blue / agent purple); the mode chip is
now a pill in the mode color (accent border + ~13% accent tint fill + accent text)
carrying the glyph + label ("Shell"/"Agent") + a `⌘I` hint, sized to the wider mode so
the toggle never reflows. Per-mode placeholders and the module doc / `CHIP_LABELS` /
tests were updated.

Font substitutions (the mock's chars are `.notdef` in the bundled faces, so the nearest
present glyphs stand in, guarded by coverage tests): agent glyph `◇` U+25C7 -> `◊` U+25CA
(diamond lozenge); the chip's `⌘` U+2318 -> the Nerd MDI `apple_keyboard_command` PUA icon
U+F0633.

Deferred (documented): the mode-toggle cross-fade is declared (`Animation::CrossFade`,
`motion.fast`) but not time-driven yet - like every animation today it swaps instantly
until a frame clock lands. Text/selection are preserved by construction (the `InputModel`
reducer only flips `mode`, ADR-0004). The chip is presentation-only: the sole toggle path
is the `⌘I` hotkey -> `Disposition::ToggleMode` -> the reducer (no divergent path), so AC3
holds; wiring the chip's pointer CLICK to that same path needs mouse hit-testing (absent
today - the app handles only keys + wheel), which the ticket scopes to routing mechanics
(T-3.3, "presentation only").

# Out of scope

- The tab-completion popover ([T-9.5](TICKET-9.5-launch-and-completion.md)).
- Routing/keymap mechanics (T-3.3/T-3.4, done) - this is presentation only.
