---
id: T-3.1
epic: EPIC-3-unified-input
title: Pure InputModel reducer (text + selection + mode)
status: done
labels: [core, input]
depends_on: []
---

# Goal

Build the pure, offline-testable `InputModel` reducer that owns the in-progress command line - text, selection/caret, and a `mode: Shell|Agent` field - where the hotkey mutates ONLY `mode`, so text is preserved across the toggle by construction. This is the structural fix for the prototype's worst sin (the toggle that cleared context).

# Context

- Research: [05-unified-input-ux.md](../../research/05-unified-input-ux.md) sections 1, 2 (editing model) and Recommendations 1-3. The buffer is small (one occasionally-multiline command line); the data structure is not the bottleneck.
- ADR: unified-input model (one shell-first box; mode is a field, not a sigil; text preserved).

# Implementation notes

- Crate: `aterm-core` (or a dedicated `aterm-input` module within core - keep it pure, no UI/LLM). Confirm placement against the locked crate layout; it must be reusable by `aterm-ui` and `aterm-app`.
- Storage: `ropey = "0.6"` (char-indexed column math) or a plain `String` for v1. Do not over-engineer.
- Properties to port verbatim from the prototype's `CommandBuffer`:
  - Buffer stores characters only, never interprets them. A paste is ONE `insert` of the whole string as one undo unit; embedded newlines/control chars are literal and inert (structurally prevents paste-auto-execute).
  - Caret as an offset; motions pure (left/right/word/home/end/up/down with column memory); edits push undo units.
  - Submit is the caller reading `text` then resetting - the buffer does not decide whether Enter submits.
- `mode: InputMode { Shell, Agent }` lives IN the model; a `toggle_mode()` mutates only `mode`, provably leaving `rope`/`selection`/`undo` untouched.
- Reserve fields for `preedit: Option<Preedit>` (T-3.2), `overlay: Highlight` and `ghost: Option<GhostText>` (T-3.5) - but those are populated by other tickets.

# Acceptance criteria

- Property test: after any sequence of edits followed by `toggle_mode()`, `text` and `selection` are byte-for-byte identical to before the toggle.
- A paste containing `\n; rm -rf /` inserts as inert literal text (no execution, one undo unit); undo removes it whole.
- Word/home/end/up/down motions with column memory pass a table of cases.
- `submit()`/reset semantics leave the model empty and return the prior text.
- 100% pure: no I/O, runs on any platform; `cargo test` green with no window.

# Out of scope

- IME preedit (T-3.2), routing/hotkey wiring (T-3.3), highlight/ghost (T-3.5), the widget (T-3.6), history (T-3.7).

# Resolution

**done 2026-06-24** (jj, not pushed). `aterm-core::input` is now the pure editor
ADR-0004 prescribes, replacing the scaffold stand-in that conflated routing into the
reducer (an `InputOutcome::ToPty`/`Submitted` enum and a `Submit` event - all
removed). Landed in one focused commit (the reducer + `lib.rs` exports + the
`aterm-app::session` consumer), then an adversarial-review remediation.

**The reducer.** `InputModel` holds `text` + `Selection { anchor, caret }` (char
offsets) + `mode` + an undo/redo stack + a vertical `goal_col`. `reduce(InputEvent)`
is a pure `&mut self -> ()` mutator over `Insert | Backspace | Delete | Move(Motion,
extend) | Undo | Redo | ToggleMode`; it performs no I/O and never interprets the
buffer. Storage is a plain `String` (the ticket's sanctioned v1 choice - the buffer
is one command line, not a bottleneck; no `ropey` dependency added). Char-index vs
byte-offset math goes through `byte_of`, so multibyte text is correct.

**ACs (all met, unit + property tested, 12 tests; pure, run on Linux runners with no
window).**
- *Toggle preserves text + selection:* `toggle_mode` reassigns only `mode`. The
  property test drives 64 seeds x 40 randomized edit ops (insert incl. multibyte +
  newline, backspace/delete, extend-select motions, undo) via a deterministic LCG,
  then asserts `text` and `selection` are byte-identical across the toggle and the
  round-trip. This is the structural fix for the prototype's context-clearing toggle.
- *Paste is inert + one undo unit:* `Insert("\n; rm -rf /")` stores the newline as a
  literal inert char (the model never executes anything) and a single `Undo` removes
  the whole paste. A paste over a selection is also one undo unit (delete+insert
  wrapped in one snapshot).
- *Motions with column memory:* word (whitespace-delimited tokens), home/end
  (line-relative), buffer-start/end, and up/down preserving a goal column across
  shorter lines - covered by an explicit motion table.
- *Submit/reset:* `take() -> String` returns the line and resets text/selection/undo/
  goal/preedit while **preserving mode**; the caller (not the buffer) decides when to
  call it. This is ADR-0004's caller-owns-submit rule verbatim.

**Scoping decisions (deliberate, ticket-sanctioned).**
- `preedit: Option<Preedit>` (T-3.2), `overlay: Highlight` and `ghost: Option<GhostText>`
  (T-3.5) are reserved exactly as the architecture sketch prescribes, with read
  accessors and empty/`None` defaults; no T-3.1 logic populates them. `Highlight` is an
  empty marker struct so T-3.5 owns its span payload without a reshape.
- `aterm-app::session::on_key` is a clearly-marked **T-3.3 stopgap**: the real routing
  brain (disposition gates, IME, the toggle chord) and the input widget that renders
  the buffer (T-3.6) are not built, so Shell-mode keystrokes are still mirrored raw to
  the PTY (the shell's own line editor echoes them) and the model is kept live in
  parallel. The bytes reaching the PTY are byte-for-byte what the prior scaffold sent,
  so the running app is unchanged - hence no version bump / CHANGELOG entry (internal
  engine API; no user-visible behaviour change, matching the T-1.1/T-1.9 precedent).

**Adversarial review** (4 lenses x skeptic-verify, ultracode; 12 findings, 1
confirmed, 9 dismissed as conformance confirmations, 2 folded in). The one real
defect: the stopgap emitted `0x7f` (DEL) to the PTY *unconditionally* in Shell mode,
where the prior scaffold gated it on `cursor > 0`, so a Backspace at an empty prompt
sent a stray DEL and falsified the "byte-for-byte identical" comment. Fixed by gating
the DEL on the pre-reduce buffer actually having something to erase, restoring exact
parity. The stopgap routing has no pure unit-test seam (it requires a live `Engine`);
that seam arrives with the T-3.3 routing brain, which is the correct place to lock it
down. The reducer itself is exhaustively unit-tested.
