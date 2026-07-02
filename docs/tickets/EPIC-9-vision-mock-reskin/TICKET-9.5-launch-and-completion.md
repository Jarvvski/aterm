---
id: T-9.5
epic: EPIC-9-vision-mock-reskin
title: Launch + modes empty states and the tab-completion popover
status: done
labels: [ui, input]
depends_on: [T-9.1, T-9.4]
---

# Goal

Render the mock's two quiet informational states - `launch` (a fresh, historyless
window) and `modes` (the one-input-two-destinations explainer) - and the
`complete` tab-completion popover: a fuzzy finder that hugs the prompt, matched
letters in accent, a count + hint header, and a `>` pointer on the active row.

# Context

- North star: [ADR-0011](../../adr/0011-vision-mock-ui-north-star.md). Visual
  source: [`AtermWindow.dc.html`](../../design/vision-mock/AtermWindow.dc.html) -
  the `<!-- launch -->`, `<!-- modes -->`, and `<!-- tab completion -->` states
  plus the `completeOpen` popover in the input-bar block.
- The completion popover is the *visual* half of the completions menu tracked by
  [T-8.5](../EPIC-8-packaging/TICKET-8.5-focus-mode-completions.md); its data
  sources (history/path/Fig spec) stay in T-8.5. Ghost text is T-3.5 (done).

# Implementation notes

- **Launch**: centered "aterm", a two-line `fg.secondary` tagline ("A quiet place
  to run things." / "Type a command below, or press ⌘I to ask the agent
  instead."), and a `fg.faint` "no history yet" line. Shown when the active
  timeline is empty.
- **Modes**: an uppercase `fg.faint` eyebrow, a `fg.secondary` paragraph with the
  shell/agent split (a hairline divider between the `❯` Shell and `◇` Agent
  columns), and a "Currently routing to <mode>" line. This is an explainer surface
  reachable on demand, not a persistent state; wire it behind whatever affordance
  the app already uses for help/onboarding, or leave it as a documented state the
  app can select.
- **Completion popover**: anchored just above the input's left edge, on `bg.elev`
  with a `hairline` border + soft shadow. Header row in `fg.faint`: "N/12 · tab
  ⏎ accept · up/down move · esc". Each row: the `>` pointer (accent) on the active
  row else blank, the candidate with fuzzy-matched letters in `accent.primary`
  semibold and the rest in `fg.secondary`, then a `fg.faint` description. Active
  row on an `accent.primary` weak-tint fill. Reuse the fuzzy match highlight model;
  keyboard nav (Tab/up/down/Enter/Esc) matches the mock.

# Acceptance criteria

- [x] Launch renders centered when the timeline is empty, in both themes.
- [x] The modes explainer renders to spec in both themes with the two-column
  shell/agent split.
- [x] The completion popover renders on `bg.elev` above the prompt with the header,
  accent-highlighted match letters, `>` pointer, and active-row tint; keyboard nav
  (tab/up/down/enter/esc) works and accepting fills the input. *(The soft drop shadow
  is deferred - flat on `bg.elev` with a hairline border - see Notes.)*
- [x] Motion budget + T-1.8 no-per-frame-alloc assertion hold; a render/widget test
  covers the popover and both empty states in both themes.

## Notes

Landed 2026-07-02. Three pieces:

- **Completion model** (`aterm-core/src/completion.rs`, pure + exported): a case-insensitive
  fuzzy SUBSEQUENCE matcher (`fuzzy_match` / `rank`, mirroring the mock's `fuzzyParts` -
  per-char hit flags + a `first + (span - qlen)` score, best-first, capped) plus the
  `Completion` navigation state (open / items / active row, with `open_with` / `refresh` /
  `move_up` / `move_down` / `active`). Unit-tested on every platform.
- **Screens front-end** (`aterm-ui/src/screens.rs`): the `launch` splash (centered "aterm" +
  tagline + "no history yet") and the `modes` explainer (eyebrow, paragraph, the two-column
  `❯` Shell / `◊` Agent split with a hairline divider, and a "Currently routing to <mode>"
  line). The host (`gpu.rs`) centers them in the content band between the title bar and the
  input box; `launch` shows when the block timeline is empty, `modes` on the `Cmd-?` hotkey.
- **Completion popover** (`aterm-ui/src/completion_render.rs`): a `bg.elev` panel with a
  hairline border above the input's left edge - a faint header (count + `tab/enter accept /
  up/down move / esc`), one row per candidate with the `>` pointer (accent) on the active row,
  the candidate with fuzzy-matched letters in `accent.primary` and the rest in `fg.secondary`,
  a faint description, and a weak-accent tint behind the active row. The app (`session.rs`)
  wires the nav: `Tab` opens (ranking shell history against the line) / accepts, `up`/`down`
  move, `Enter`/`Tab` accept (replacing the line as one undo unit), `Esc` closes; typing
  re-ranks. All three front-ends are damage-gated (one rect + one glyph draw each, alloc-free
  on an unchanged frame) and GPU-tested in both themes.

Font substitutions (coverage-tested): the modes Agent glyph `◇` (U+25C7) -> `◊` (U+25CA); the
popover pointer `›` -> `>`; ASCII "Cmd-I" / "up/down" / "tab/enter" stand in for the mock's
`⌘I` / `↑↓` / `⏎` (those symbols are `.notdef` in the bundled faces).

Deferred (documented, not silently dropped):
- The popover's **soft drop shadow** is deferred - it renders flat on `bg.elev` with a hairline
  border - landing with the transparent-surface / window-shadow work in T-8.1 (the same
  deferral as the title bar's shadow, T-9.2).
- **Completion candidate SOURCES** beyond this session's command history ($PATH binaries, Fig
  argument specs, `@file` mentions, persisted history) are
  [T-8.5](../EPIC-8-packaging/TICKET-8.5-focus-mode-completions.md); this seeds from the
  in-session history ring so the nav + accept are exercisable now. Descriptions are empty
  (history has none); richer sources add them. **`Tab` is captured by aterm's finder**, which
  supersedes the integrated shell's own Tab-completion while the finder HAS candidates - but
  when the finder has nothing to offer (no history match, e.g. a fresh session), `Tab` falls
  through to normal routing so the shell still receives `\t` and its own completer runs. The
  finder header shows the dynamic shown-count `N` (the mock's fixed `/12` denominator is
  dropped - there is no fixed candidate pool here).
- **Accepting a completion in Shell mode desyncs the hidden shell's line buffer** - the same
  pre-existing limitation as the T-3.5 ghost-accept: editing keys are mirrored raw to the PTY
  (the shell echoes them), but a programmatic line replacement (accept) updates only the
  `InputModel`, so on submit the shell would run its own buffer, not the accepted line. The
  real fix is the T-3.6 "input box is the source of truth" refactor (submit the whole line
  instead of mirroring keystrokes); until then accept-in-Shell shares ghost-accept's behavior.
- The `modes` explainer is reachable via the `Cmd-?` hotkey (rebindable; `ATERM_HELP_KEY`).
  The mock's "tap the chip" affordance needs mouse hit-testing (absent today - the same
  cross-cutting prerequisite as the T-9.4 chip click / T-9.2 sidebar-glyph click).
- The completion NAV is unit-tested at the model layer (`completion::tests`) + the render is
  GPU-tested; the `session.rs` on_key glue is thin wiring over those tested pieces (a live
  Session spawn - PTY + agent runtime - is deliberately avoided in the app's unit tests).

# Out of scope

- Completion data sources and the Fig-spec ingest ([T-8.5](../EPIC-8-packaging/TICKET-8.5-focus-mode-completions.md)).
- Agent-mode `@file`/`@command` mention completion (also T-8.5).
