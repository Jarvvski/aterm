# ADR-0004: Unified input model - one shell-first box, hotkey toggles Enter routing

## Status

Accepted

## Context

aterm needs one input surface that serves both the live shell and the AI agent. The prior
prototype's worst sin was a hard mode split whose toggle **cleared the in-progress text**,
and which degraded silently to zsh-only. Warp's central architectural move - and the right
one - is that the *terminal application* owns the in-progress command line as a structured
editor, not the shell's line editor (ZLE/readline); only the finished command is committed
to the PTY ([05-unified-input-ux.md](../research/05-unified-input-ux.md),
[01-warp-internals.md](../research/01-warp-internals.md)). For aterm the editor is small (a
single, occasionally multi-line command line); the dominant complexity is macOS IME
composition, the shell-vs-agent routing brain, and async highlight/ghost-text overlays that
must never stall the frame loop.

## Decision

- **ONE shell-first input box.** A hotkey toggles where Enter routes (live shell vs the AI
  agent); typed text is **preserved verbatim** across the toggle. This is NOT a sigil scheme.
- The model is a single pure-Rust `InputModel` reducer holding `text` + `selection` + a
  `mode: Shell | Agent` field. **The hotkey mutates only `mode`.** Because `mode` lives
  inside the model, mode and text are atomically consistent and the toggle provably cannot
  touch `rope`/`selection`/`undo` - this is the structural fix for the context-clearing
  toggle.
- Buffer storage: `ropey 0.6` (or a plain `String` for v1); the buffer is not the bottleneck.
- Properties ported from the prototype's `CommandBuffer`: the buffer stores characters only
  and never interprets them (a paste is one inert `insert` as one undo unit, preventing the
  paste-auto-executes class of bug); submit is the caller reading `text` then resetting.
- **Mode is shown by caret tint + prompt glyph** (Shell = ink/blue caret + `❯`; Agent =
  amber caret + `✦`). No banner. The accent is the only moving color.
- The routing brain is a priority-ordered gate whose **top priority is `preedit-active`**:
  while an IME composition is active, Enter/Tab/Esc are owned by the IME (confirm/cancel
  candidate) and never submit or route (fixes the Zed #23003 Enter-eats-candidate bug).
  `Opt-Enter` remains a mode-independent one-shot send-to-agent.
- IME via winit 0.30 `Ime` events first, with a hand-rolled `NSTextInputClient` as the
  documented escape hatch if winit's known gaps (#3617 nested preedit, Pinyin crash) bite.
- All highlight/parse/ghost/completion work runs async and debounced (long idle debounce,
  short-circuit on space/paste/selection), applied as non-inheritable style spans; the
  render loop reads the last-good overlay and never blocks.

## Consequences

- Text preservation across the toggle is guaranteed by construction, not by discipline: you
  can type `git rebase -i HEAD~3`, hit the toggle, and the agent receives your exact text.
- Mode indication adds zero chrome and is pre-attentive (color + glyph shape), matching the
  iA restraint of the design language ([07-ia-design-language.md](../research/07-ia-design-language.md)).
- The async overlay discipline protects the 60fps floor: the render loop never blocks on
  highlight or ghost-text computation.
- The `preedit-active` gate is mandatory and load-bearing for CJK users; the
  `NSTextInputClient` escape hatch is the named fallback if winit's IME gaps surface.
- The mode-toggle hotkey default and rebindability remain a product call (proposal `Cmd-/`,
  rejecting `Ctrl-Space` (macOS IME switch) and `Cmd-.` (SIGINT muscle memory)); the model
  is indifferent to which key fires it.
- Degradation when shell integration is absent is shown visibly via the mode/state
  indicator, never silently ([ADR-0008](0008-shell-integration.md)).

## Alternatives considered

- **Two separate input boxes (shell and agent).** Rejected: it is the hard mode split that
  the prototype proved hostile; it duplicates state and cannot preserve text across a switch.
- **A sigil/prefix scheme** (e.g. a leading `/` or `!` to route to the agent). Explicitly
  rejected by the locked decision: it overloads characters the shell needs and is less
  discoverable than an explicit mode with a visible indicator.
- **Letting the shell's line editor (ZLE/readline) own the line.** Rejected: it is the
  character-oriented PTY interface that makes mouse editing, real multi-line, and IDE
  features impossible - the exact inversion Warp and aterm exist to undo.
- **A heavier text stack (`cosmic-text::Editor`) for the input box.** Deferred: reasonable
  only if the renderer already pulls it in; for a single command line the pure `InputModel`
  reducer wins on testability and control.
