---
id: T-3.7
epic: EPIC-3-unified-input
title: Shared history ring + per-mode query lens
status: done
labels: [core, input]
depends_on: [T-3.1]
---

# Goal

One shared, wall-clock-ordered history ring storing both shell commands and agent prompts with a `mode` tag, exposed through two query lenses so Up-arrow / Ctrl-R in Shell mode searches shell entries and in Agent mode searches agent prompts.

# Context

- Research: [05-unified-input-ux.md](../../research/05-unified-input-ux.md) section 4 (shared vs separate history) + Recommendation 8. Owner open-question #3 (history scope default; do agent prompts leak into the real shell history file - default: no).

# Implementation notes

- Crate: `aterm-core` (pure data) consumed by `aterm-ui`/`aterm-app`.
- A single ring: entries `{ text, mode: Shell|Agent, timestamp }`. Two query lenses (Shell-only, Agent-only) with a user setting to widen either to "all".
- Up-arrow / Ctrl-R use the lens matching the current `InputModel.mode`. Shell-mode ghost text (T-3.5) draws from the Shell lens.
- Do NOT write agent prompts into the user's real shell history file; aterm's history is separate (persist to aterm's own config/data dir).

# Acceptance criteria

- Submitting a shell command and an agent prompt stores both with correct mode tags + timestamps.
- Up-arrow in Shell mode cycles shell entries only; in Agent mode, agent entries only.
- Ctrl-R fuzzy/prefix search respects the lens.
- The "widen to all" setting surfaces both in either mode.
- Agent prompts are absent from the user's shell history file (assert it is untouched).

# Out of scope

- Persistence format/migration polish (config work in T-8.3).
- Completion menus (later).

# Resolution

**done 2026-06-24** (jj, not pushed). Implemented as the pure module
`aterm-core::history`, the ticket's prescribed home ("pure data, consumed by
`aterm-ui`/`aterm-app`"). No I/O, no window, no clock of its own - runs on the Linux
unit-test runners. Landed in one focused commit (the module + `lib.rs` re-exports),
then an adversarial-review remediation to the tests.

**The ring.** `HistoryRing` is one shared, bounded (`DEFAULT_HISTORY_CAP = 10_000`),
wall-clock-ordered ring of `HistoryEntry { text, mode, at }`. Entries are kept in
submission (insertion) order - the newest at the back - and that *is* the wall-clock
order, so the ring never sorts by the `at` field, which sidesteps any non-monotonic
system-clock surprise; `at` is carried purely for display. When full it evicts the
single oldest entry (mode-blind global FIFO). Blank submissions (empty/whitespace-only)
are dropped, as shells do; otherwise text is stored verbatim (meaningful leading or
trailing whitespace is preserved). The caller supplies the timestamp, which keeps the
ring clock-free and its tests deterministic.

**The two lenses (the headline).** `HistoryScope { Mode(InputMode), All }` with
`for_mode(mode, widen)` is the per-mode lens widenable to "all". `scoped(scope)` is the
single newest-first primitive every reader is built on, so the Shell lens sees only
shell commands and the Agent lens only agent prompts (the "widen to all" *setting*
itself lives in the consumer, not the ring).

**The three read paths.**
- *Recall (Up/Down)* - a stateful `Recall` cursor walking one lens: `older`/`newer`
  with an explicit `Some(0) -> draft` transition, `at_draft`/`position`/`reset`. `older`
  clamps at the oldest (no-op `None`, cursor unmoved); `newer` past the newest returns
  to the draft. Pure index math over `scoped`.
- *Ctrl-R (search)* - `search(scope, query)`: case-insensitive **substring**
  (reverse-i-search) within the lens, newest-first; empty query returns everything in
  scope. This faithfully implements the locked "fish/zsh-autosuggestions semantics" of
  `05-unified-input-ux.md` §4 (zsh's native Ctrl-R *is* substring); the "fuzzy" Tab
  menu of §1 is a separate, explicitly out-of-scope subsystem (completion menus).
- *Ghost text (T-3.5 support)* - `suggest(scope, prefix)`: the most-recent lens entry
  whose text begins with `prefix` and has a non-empty tail (the fish-style
  `zsh-autosuggestions` prefix match). Empty prefix or no match -> `None`. The
  `text.len() > prefix.len()` tail guard is byte-length-correct: with `starts_with`,
  equal lengths can only mean exact equality (no tail), longer means a real tail.

**ACs (all met; 13 unit tests; pure, Linux-runnable).**
- *AC1 store both with mode + timestamp:* `push_stores_both_with_mode_and_timestamp`.
- *AC2 Up-arrow cycles within the lens:* `scoped_lens_filters_by_mode_newest_first`
  (data) + `recall_cycles_within_lens_and_returns_to_draft` (behavior, incl. the
  agent-entry-skipped, oldest-clamp, and back-to-draft transitions).
- *AC3 Ctrl-R respects the lens:* `search_substring_respects_lens` (case-insensitive,
  shell-only, newest-first; an agent entry containing the needle is excluded).
- *AC4 widen to all:* `widen_to_all_surfaces_both_modes` (search + recall + `for_mode`).
- *AC5 absent from the shell history:* the *file* half is true by construction - the
  module imports no `std::fs`, takes no path, writes nowhere - and the end-to-end "an
  agent prompt never reaches the shell" guarantee is the routing brain's (T-3.3:
  agent-mode submit bypasses the PTY). What this layer owns and can assert red-capably
  is partitioning: `agent_prompts_are_absent_from_the_shell_lens` proves agent prompts
  never surface through the Shell lens's recall/search/ghost.

**Scoping decisions (deliberate).**
- The recall/search/ghost *UI wiring* (Up-arrow vs. in-buffer vertical motion, the
  Ctrl-R chord, the "widen" toggle, capture-on-submit) is the routing brain's (T-3.3)
  and the input widget's (T-3.6) job - explicitly out of scope here. A session capture
  hook was prototyped and reverted: its getter would be dead code under CI's
  `-D warnings`, and T-3.3 is the coherent place to wire capture *and* recall together
  (the same precedent as T-3.1 leaving routing to T-3.3). All five ACs are proven at
  the ring API regardless.
- Plain `VecDeque<HistoryEntry>` storage (no new dependency); the ring is one small
  struct per command line, never a hot-path or 60fps concern, and Ctrl-R/ghost run
  off the render thread (T-3.5). No version bump / CHANGELOG: internal engine API, no
  user-visible behaviour change yet (matching the T-1.1/T-1.9/T-3.1 precedent).

**Adversarial review** (5 lenses x skeptic-verify, ultracode; 7 findings, 1 confirmed,
6 dismissed, 2 dismissed-but-folded-in). The one real defect was a test-quality
tautology: the original AC5 test wrote a sentinel to a temp path `push` can never
reach, so its assertion held no matter what `push` did (a reviewer reproduced a
file-append regression that still passed it green). Replaced with the red-capable
shell-lens-partitioning test above, plus a by-construction note for the file invariant.
Two dismissed-but-cheap test-hygiene notes were folded in:
`ordering_is_insertion_order_not_the_timestamp` (a later push with an earlier stamp
still sorts newest - would catch a sort-by-`at` regression) and
`eviction_is_mode_blind_fifo` (interleaved-mode eviction pins the global-FIFO doc).
The substring-vs-"fuzzy"/`#[must_use]`/coverage findings were dismissed with reasons
recorded (substring matches the locked research semantics; `#[must_use]` is a
pedantic-only lint that `block.rs`/`input.rs` also omit).
