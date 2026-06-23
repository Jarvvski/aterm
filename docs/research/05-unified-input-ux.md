---
title: The Unified Shell+Agent Input Editor
domain: unified-input-ux
status: research
---

# The Unified Shell+Agent Input Editor

## TL;DR

- **The hard model is right and matches the state of the art.** Warp's central architectural move is exactly ours: the *terminal application* owns the in-progress command line as a structured editor, not the shell's line editor (ZLE/readline); only the finished command is committed to the PTY [1][2]. The prior Kotlin prototype already implemented this inversion (`CommandBuffer` owns the bytes, commits on submit) and it should be ported as a *concept*, rebuilt in Rust.
- **For aterm the editor is small** - a single command line that is occasionally multi-line, not a megabyte source file. A full rope (ropey/crop) is overkill for correctness but cheap to adopt; the dominant complexity is not the buffer data structure, it is (a) macOS IME composition, (b) the shell-vs-agent routing brain, and (c) ghost-text/highlight/completion overlays that must never stall the 60fps render.
- **Recommendation: a single owned `InputModel` (small rope = `ropey` 0.6.2 or a plain `String`+gap for v1) holding text + multi-caret + selection, plus a thin `mode` field (Shell | Agent).** The hotkey flips `mode`; the buffer text is untouched (this directly fixes the prototype's worst sin - the toggle that CLEARED context). One shared history ring, two query lenses.
- **Mode indication, iA-restrained: a single accent-colored prompt glyph + caret tint.** Shell = the iA "blue"/ink caret and a `❯`-class glyph; Agent = a warm accent (amber) caret + a distinct glyph (e.g. a small spark/`✦`). No chrome, no banner, no second box. The accent is the *only* moving color. A tiny right-aligned status word ("shell"/"agent") is optional and low-contrast.
- **IME is the single biggest correctness risk.** Because we self-draw (no `NSTextView`), we must implement `NSTextInputClient` ourselves exactly as Zed/GPUI do [6][7], or ride winit 0.30's `Ime` events (`Enabled`/`Preedit(String, Option<(usize,usize)>)`/`Commit(String)`/`Disabled`, byte-indexed) [4][5]. The known failure mode (Zed issue #23003: Enter during composition is eaten by the terminal instead of confirming the candidate) is exactly the kind of bug our routing brain must guard against: **while preedit is active, Enter confirms the IME candidate and never submits/routes** [7].
- **Ghost-text / highlight / completion must be async + debounced** (Warp uses long debounce + short-circuit on space/paste/selection [1]) and decoupled from the render loop, or they will blow the frame budget. Highlight is computed off the main thread and applied as a *non-inheritable* style overlay [1].

## Findings

### 1. Prior art: how the leaders build the command editor

#### Warp (the closest model; Rust, Metal, closed source)

Warp's public engineering posts are the most directly relevant prior art because they made the same hard decisions we did.

- **The shell does not own the line.** Traditional terminals run "a completely character oriented interface" through the PTY where "the shell manages all changes to the input buffer," which is why mouse editing, real multi-line, and IDE features are impossible. Warp "relocated the text editing layer entirely into the terminal application," populating "the input buffer entirely at the terminal layer," and sends "the complete command to the shell" only on submit [2]. This is the exact inversion the prototype's `CommandBuffer` header describes ("aterm - not zsh's ZLE - owns the in-progress command line").
- **Custom parser, loosely based on Nushell.** One parser drives the Command Inspector, autosuggestions, completions, error underlining, and syntax highlighting [1]. Highlighting reuses the parser to know "whether a command is invalid (error underlining) and the different parts of a command" [1].
- **Self-drawn styling, not ANSI.** ANSI SGR can't style an editor buffer, so Warp "defines its own primitives for styling text using Apple's Metal graphics library" and draws underline rectangles itself [1]. This is mandatory for us too: the input editor is *our* UI, not a VT cell grid.
- **Debounce + short-circuit.** Validation/red-underlining waits for "longer debouncing intervals" so incomplete tokens aren't flagged, with "short-circuit" triggers on **space, paste, and cursor selection** for instant feedback when a token is clearly complete [1]. All highlight/parse work is **async** to avoid typing regressions [1]. (No exact ms figures published - see Risks.)
- **Data structure: a SumTree (rope-like).** Warp stores text in a "SumTree custom data structure similar to a Rope" giving O(log N) indexing across chars/bytes/lines with aggregate stats and rebalancing [1]. Styles are classified **inheritable** (user formatting that propagates to following typed text) vs **non-inheritable** (auto annotations like error underlines that do not) [1]. This inheritable/non-inheritable split is a clean model we should copy for our highlight overlay.
- **Universal Input / input routing.** Warp historically shipped a "Universal Input" concept (now legacy) where the one input box could target different things; it works over SSH and with zero config [2][3]. They do not publish the routing internals for interactive programs (vim/ssh/password) - see Risks.

#### Fig / Amazon Q Developer CLI (open source; Rust)

Now `aws/amazon-q-developer-cli-autocomplete`; the `withfig/autocomplete` spec repo is still the data source [8][10].

- **Two independent surfaces.** (1) An **autocomplete dropdown** to the right of the cursor (IDE-style, arrow-key selectable, themeable), and (2) **inline ghost text** suggestions on the command line. They are independent and separately configurable [8].
- **Architecture (the cautionary tale).** Fig did *not* own the editor. It used `figterm` - a headless PTY that **intercepts the shell's edit buffer** - plus `fig_desktop` (a Rust `tao`/`wry` webview app), `fig_input_method` (a macOS input method just to read the cursor position), and the macOS **Accessibility API** to position a floating window over the real terminal [8]. This bolt-on-over-the-real-terminal approach is fragile (it is reading someone else's buffer) and is exactly what owning the editor avoids. **Lesson: don't bolt onto the shell's buffer; own it.**
- **Completion specs are reusable data.** A spec is "a declarative schema that specifies the `subcommands`, `options` and `args` for a CLI tool," written in TypeScript, with **Generators** for dynamic argument suggestions; "400+ contributors" maintain specs for git/npm/docker/aws etc. [8][10]. These specs are MIT-licensed data we could *consume* (parse the schema, not run the TS) to get completions for free - a high-leverage option.

#### Kitty (raw terminal, but instructive on input encoding)

Kitty is a classic GPU terminal (it does *not* own the line editor) but it authored the **Kitty keyboard protocol**, a disambiguating key-encoding scheme the prototype already implemented (`KittyKeyboard.kt`, `KittyKeyboardTest.kt`). Relevance to us: when we *forward* keys raw to the PTY (alt-screen TUIs, in-flight stdin), we still need a correct key-to-bytes encoder. The prototype's `KeyStroke`/`NamedKey` neutral model + a mode-aware encoder (DECCKM application-cursor mode honored) is the right shape and should be re-implemented in Rust.

### 2. Rust text-editor building blocks

#### Rope / buffer crates (current versions verified)

| Crate | Version | Index unit | Backing | O(1) clone | Notes |
|---|---|---|---|---|---|
| `ropey` | 0.6.2 [11] | Unicode scalar (char) | B-tree | yes | Never splits grapheme clusters; knows all 8 Unicode line breaks; mature, widely used (Helix, Xi-derived). Char-indexed APIs are intuitive and can't create invalid UTF-8 [11]. |
| `crop` | 0.4.3 (2025-04-25) [12] | UTF-8 byte offset (like `String`) | B-tree | yes | Faster than ropey on real edit traces (e.g. automerge-paper 12.39ms vs 44.14ms; sveltecomponent 0.95ms vs 3.65ms) [12]; graphemes optional via `unicode-segmentation`; only LF/CRLF line breaks. |
| `xi-rope` | (unmaintained) | - | B-tree | - | Historical; its `Metric` trait inspired crop [12]. Do not adopt. |

**Caveat on the benchmark numbers:** those figures are *whole-document* editing traces (thousands of edits on real source files), i.e. the worst case crop is tuned for. Our buffer is one command line. At that scale **both crates are effectively instant** and the choice is about ergonomics, not speed. crop's byte-offset indexing aligns with winit's byte-indexed IME ranges [4] and Rust `String` slicing; ropey's char indexing is friendlier for column math. Either is fine; the buffer is not the bottleneck.

#### Higher-level: `cosmic-text`

`cosmic-text` 0.14.2 [13] is a full text-shaping/layout/edit stack: HarfRust shaping, custom safe-Rust bidi layout, swash rendering (ligatures + color emoji), and a built-in `Editor` with cursor/selection/optional syntect highlight and vi-style commands via `modit` [13]. It is attractive because it *also solves shaping/layout/IME-adjacent cursor math* that we need anyway for the agent prose. **But** it couples buffer + shaping + a particular editor model; for a single command line with our own highlight overlay it may be more than we want at the input layer (it's a better fit for the agent transcript / prose rendering - cross-ref the rendering-stack research). Flag for the renderer-stack decision: if aterm adopts `cosmic-text` (or `glyphon`, its wgpu glue) for text rendering generally, reusing its `Editor` for the input box is the path of least resistance.

#### The editing model (recommended internal data model)

The prototype's `CommandBuffer` is a clean, pure, offline-testable reducer and is the right *shape*. Port these properties to Rust:

- Buffer stores **characters only, never interprets them**. A paste is one `insert` of the whole string as one undo unit; embedded newlines/control chars are literal and inert. This structurally prevents the Warp `#7419`-class "paste auto-executes" bug (the prototype calls this out explicitly).
- Caret as an offset; motions are pure (left/right/word/home/end/up/down with column memory); edits push undo units.
- **Submit is the caller reading `text` then resetting** - the buffer doesn't decide whether Enter submits.

Rust shape (illustrative, not prescriptive of the final API - confirm before coding per the user's interface-design rule):

```
struct InputModel {
    rope: Rope,                 // ropey::Rope (or String for v1)
    selection: Selection,       // primary caret + optional anchor; multi-caret later
    mode: InputMode,            // Shell | Agent  -- flipped by hotkey, text untouched
    undo: Vec<Snapshot>,
    preedit: Option<Preedit>,   // active IME composition (string + cursor byte-range)
    overlay: Highlight,         // non-inheritable style spans, computed async
    ghost: Option<GhostText>,   // async suggestion tail
}
enum InputMode { Shell, Agent }
struct Preedit { text: String, cursor: Option<(usize, usize)> } // byte indices, per winit
```

`mode` lives *in the model* (not in some external app state) so that mode and text are atomically consistent and the toggle is a one-field mutation that provably preserves `rope`. This is the direct structural fix for the prototype's context-clearing toggle.

#### macOS IME (the load-bearing platform detail)

We self-draw, so there is no `NSTextView`; the OS text-input system must talk to *us*. Two routes, both verified:

1. **winit 0.30.13** surfaces IME as `WindowEvent::Ime(Ime)` with `Ime::Enabled`, `Ime::Preedit(String, Option<(usize,usize)>)` (**byte-indexed** cursor range), `Ime::Commit(String)`, `Ime::Disabled` [4][5]. You call `Window::set_ime_allowed(true)` to opt in and `Window::set_ime_cursor_area(...)` so the candidate window sits under the caret [5]. winit 0.30 added `request_ime_update`, `Ime::DeleteSurrounding`, more `ImePurpose`/`ImeHints` [5]. **Known winit gaps:** `_selected_range`/`_replacement_range` are ignored in its `NSTextInputClient` impl (breaks IMEs with nested preedits, issue #3617) and there have been out-of-bounds `set_marked_text` crashes with Pinyin [5]. So winit's IME is usable but not bulletproof.
2. **Roll our own `NSTextInputClient`** on a single raw `NSView`, exactly as Zed/GPUI do - "a 1:1 exposure of the NSTextInputClient API" against "a single NSView that captures raw input and dispatches it to GPUI's own keyboard handling" [6][7]. This gives full control over marked text, replacement ranges, and candidate placement, at the cost of writing `objc2` bindings.

**The Enter-during-composition trap (must-fix):** Zed terminal issue #23003 - pressing Enter while an IME candidate is being composed inserts a terminal newline instead of confirming the candidate [7]. Our routing brain must check `preedit.is_some()` *first*: while a composition is active, Enter/Tab/Esc are owned by the IME (confirm/cancel candidate) and **never** trigger submit-to-shell, submit-to-agent, or completion-accept. The prototype's `inputDisposition` already orders gates carefully (agent-holds-shell, then alt-Enter, then degraded, then alt-screen, then in-flight, then Enter); **add `preedit-active` as the new top-priority gate.**

### 3. Mode indication consistent with iA restraint

iA Writer's visual language: near-monochrome, one disciplined accent, generous whitespace, no chrome. The mode indicator must read instantly but add no furniture. Ranked options:

1. **Caret tint + prompt glyph (recommended).** The caret and the prompt glyph share one accent that is the *only* color that changes between modes. Shell = ink/blue caret with a shell glyph (`❯`); Agent = amber caret with a distinct glyph (`✦`/spark). This is pre-attentive (color + shape), occupies zero extra space, and is dead-on-iA. The accent is reused from the design-system palette (cross-ref design-system research).
2. **Caret shape.** Shell = block/bar; Agent = underline or a subtly thicker bar. Secondary reinforcement to color; good for color-blind users. Cheap to add.
3. **A low-contrast right-aligned status word** ("shell" / "agent") in the proportional Duo/Quattro UI face, dimmed. Optional, for discoverability during onboarding; can be a setting that fades after N uses.
4. **Placeholder text changes** when empty: `Type a command` (Shell) vs `Ask the agent` (Agent). Strong onboarding signal at zero chrome cost; disappears as soon as you type.

**Avoid:** banners, a second input box, background-color fills behind the whole input (too loud for iA), or animated transitions longer than ~120ms (cross-ref the 60fps floor - any mode-flip animation must be a single cheap interpolation, not a layout reflow).

### 4. Interaction details

#### The hotkey

- **Recommendation: a single dedicated toggle that does not collide with shell editing.** Strong candidate: a tap of a modifier-less but reserved key is impossible (shell needs them all), so use a chord. `⌘.` or `⌃Space` or `⌘/` are candidates; **`⌃Space`** is clean but on macOS it's the system "Select previous input source" shortcut (IME) - avoid. **Recommend `⌘.` is risky** (often SIGINT-adjacent in muscle memory). **Leaning `⌘/`** (mnemonic: "/" = ask) or a configurable binding defaulting to a function-ish chord. This is a product decision - see Open Questions.
- **`⌥⏎` already means "submit to agent" in one shot** in the prototype (`InputDisposition.SubmitToAgent`), independent of the persistent mode. Keep both: the *mode toggle* sets where plain Enter routes; `⌥⏎` is a one-shot "send this to the agent right now regardless of mode." This dual scheme is good - mode for sustained agent conversation, `⌥⏎` for a quick aside.

#### What happens to in-flight text on toggle

- **Nothing. The text is preserved verbatim.** This is the headline fix. The toggle mutates only `mode`; `rope`, `selection`, and `undo` are untouched. You can type `git rebase -i HEAD~3`, hit the toggle, and the agent receives "how do I do `git rebase -i HEAD~3`" with your exact text intact.
- Highlight overlay and ghost text are *mode-dependent* and should be recomputed on toggle (shell highlight is command syntax; agent "highlight" is essentially none/prose). The recompute is async; the text never flickers.

#### How the agent's streaming reply relates to the input box

- **The input box stays put; the reply streams into the timeline above it**, not into the box. The single wall-clock-ordered timeline (a prototype "keep") receives the agent transcript as blocks interleaved with command blocks. The input box is a persistent footer.
- While the agent "holds the shell" (deciding/running), the prototype's `InputDisposition.Swallow` consumes keystrokes. **Reconsider this:** swallowing all input during an agent turn is hostile. Better: keep the input box live so the user can **queue the next message** or **type an interrupt** (Esc to cancel the agent turn). At minimum, Esc must always be able to interrupt. This is a refinement of the prototype's invariant - see Open Questions.

#### Shared vs separate history

- **One shared, wall-clock-ordered history ring; two query lenses.** Both shell commands and agent prompts are stored with a `mode` tag and timestamp. Up-arrow / `⌃R` in Shell mode searches *shell* entries (fish/zsh-autosuggestions semantics); in Agent mode searches *agent prompt* entries. A user setting can widen either lens to "all." Rationale: a single ring keeps the timeline and history coherent (matching the unified-timeline keep) without polluting shell history with prose or vice versa.
- **Ghost text (autosuggest), per mode.** Shell mode: fish-style ghost text from history (most-recent prefix match), accepted with `→`/`End` at end-of-line - exactly `zsh-autosuggestions` semantics: muted gray tail, `forward-char`/`end-of-line` accepts [14]. Completions strategy array (history, then completion spec) mirrors `ZSH_AUTOSUGGEST_STRATEGY` [14]. Agent mode: ghost text from prior agent prompts (or off by default - prose autosuggest is noisy).

#### Completion behavior per mode

- **Shell mode: IDE-style completions.** Tab opens a fuzzy menu to the right of the caret (Fig/Warp model [1][8]). Source: parse the MIT Fig completion-spec data [8][10] (consume the declarative schema; do not execute the TS) plus filesystem/path completion plus history. This is a large, separable subsystem - v1 can ship history-only ghost text + path completion and add spec-driven menus later.
- **Agent mode: no shell completions.** Tab can insert a literal tab (for prose) or be repurposed for `@file`/`@command` reference autocomplete (mention a file path or a recent command to the agent). Recommend `@`-mention completion in Agent mode rather than command completion.

### 5. Prior-prototype pitfalls and how the model fixes them

| Prototype sin | Fix in this design |
|---|---|
| Mode toggle **CLEARED context** | `mode` is one field in `InputModel`; toggling provably can't touch `rope`. Text preserved by construction. |
| Hard MODE split (two worlds) | One box, shell-first; `⌥⏎` one-shot agent + persistent mode toggle. Shared history ring. |
| zsh-only with **silent** degradation | The routing brain already has an `integrationLive` gate (no OSC-133 -> raw passthrough, classic ZLE); make the degradation **visible** (mode chip shows "raw"/degraded) instead of silent. |
| (new risk) IME Enter eaten | New top-priority `preedit-active` gate in the routing brain; Enter confirms candidate, never submits [7]. |
| (new risk) highlight/ghost stalls render | All parse/highlight/ghost/completion work async + debounced (Warp model [1]); overlay applied as non-inheritable spans; render reads last-good overlay, never blocks. |

## Recommendations for aterm

1. **Own the line; rebuild `CommandBuffer` as a pure Rust `InputModel` reducer.** Port the prototype's character-only/paste-is-inert/submit-is-caller properties verbatim. *(High)* - it is the proven core and the safety story depends on it.
2. **Put `mode: InputMode` inside the model; toggle mutates only `mode`.** Structurally guarantees text preservation across the toggle. *(High)*
3. **Buffer storage: `ropey` 0.6.2 for v1** (mature, char-indexed column math is convenient), or a plain `String` if we want zero deps initially; revisit `crop`/`cosmic-text::Editor` only if the renderer-stack pick (cross-ref) already pulls one in. *(Med)* - the buffer is not the bottleneck; don't over-engineer.
4. **Implement IME via winit 0.30.13 `Ime` events first**, with a fallback plan to hand-roll `NSTextInputClient` (Zed/GPUI model) if winit's known gaps (#3617 nested preedit, Pinyin crash) bite. *(Med)* - winit is fast to integrate; the hand-roll is the escape hatch.
5. **Add `preedit-active` as the highest-priority gate in the routing brain**; Enter/Tab/Esc are owned by the IME during composition. *(High)* - prevents the Zed #23003 class of bug.
6. **Mode indication = accent caret tint + prompt glyph (+ caret shape as secondary).** No banner, no second box, one moving color. *(High)* - matches iA restraint and is pre-attentive.
7. **All highlight/parse/ghost/completion runs async + debounced** (long debounce; short-circuit on space/paste/selection), applied as non-inheritable style spans; the render loop reads the last-good overlay and never blocks. *(High)* - directly protects the 60fps floor.
8. **One shared history ring with a `mode` tag; per-mode query lens; fish-style ghost text in Shell mode** (`→`/`End` accepts, gray tail) [14]. *(Med)*
9. **Completions: ship history + path completion in v1; consume MIT Fig completion-spec data for menus later.** *(Med)* - large separable subsystem; don't block v1 on it.
10. **Keep the input box live during agent turns; Esc always interrupts; allow queueing the next message** rather than swallowing all input. *(Med)* - revises the prototype's `Swallow` invariant toward a less hostile UX.
11. **`⌥⏎` = one-shot send-to-agent; a configurable mode-toggle hotkey** (default proposal `⌘/`, avoid `⌃Space`/`⌘.`). *(Low)* - the exact key is a product call.

## Risks & unknowns

- **Exact Warp debounce timings are not published** - "longer debounce" + "short-circuit on space/paste/selection" is qualitative [1]. We will need to tune (start ~80-150ms idle debounce; measure against the frame budget).
- **Warp's interactive-program input routing is undocumented** [2][3]. Our story rests on the prototype's OSC-133-anchored `inFlight`/`altScreen` raw-passthrough; if shell integration isn't live we degrade to classic ZLE. The robustness of "is a foreground program reading stdin" detection across shells/programs is a real unknown (cross-ref the shell-integration research).
- **winit IME gaps are real:** `_selected_range`/`_replacement_range` ignored (#3617), historical Pinyin `set_marked_text` OOB crash [5]. If these block CJK users we must hand-roll `NSTextInputClient`. Unverified whether the latest 0.30.x patch closed the Pinyin crash.
- **`cosmic-text::Editor` vs a custom reducer** is entangled with the renderer-stack decision; I did not benchmark `cosmic-text`'s editing path for a single-line buffer. If the renderer is `glyphon`/`cosmic-text`, reusing its `Editor` may beat a bespoke reducer; if the renderer is custom-on-wgpu/Metal, the bespoke reducer wins. Flagged for that decision.
- **Fig completion-spec consumption is unproven by us.** The schema is TypeScript; consuming it means parsing a JS/TS object graph (not executing it) - the parse path and generator (dynamic) specs may be awkward to use without a JS runtime. Needs a spike.
- **Mention-completion (`@file`/`@command`) in Agent mode** is a proposal, not validated against the agent's actual context-ingestion API (cross-ref the agent-loop research).

## Open questions for the product owner

1. **Mode-toggle hotkey:** confirm the default. Proposal `⌘/`; explicitly rejecting `⌃Space` (macOS IME switch) and `⌘.` (SIGINT muscle memory). Should it be user-rebindable from day one?
2. **Agent-turn input policy:** keep the input box live (queue next message; Esc interrupts) vs the prototype's full `Swallow`? This changes the interaction feel significantly.
3. **History scope default:** per-mode lens (recommended) vs one unified searchable history vs strict separation? And do agent prompts ever leak into the user's real shell history file?
4. **Ghost text in Agent mode:** on (from prior prompts) or off by default?
5. **Completions ambition for v1:** history+path only, or invest in spec-driven menus immediately?
6. **Degraded-mode visibility:** how loud should the "raw/classic ZLE" indicator be when shell integration isn't live (the prototype degraded silently - a known sin)?
7. **`⌥⏎` semantics:** confirm it remains a persistent-mode-independent one-shot "send to agent."

## Sources

1. Warp - How We Built Syntax Highlighting for the Terminal Input Editor: https://www.warp.dev/blog/how-we-built-syntax-highlighting-for-the-terminal-input-editor
2. Warp - Why is the terminal input so weird?: https://www.warp.dev/blog/why-is-the-terminal-input-so-weird
3. Warp docs - Universal Input (Legacy): https://docs.warp.dev/terminal/input/universal-input/
4. winit `Ime` enum (event variants, byte-indexed preedit): https://docs.rs/winit/latest/winit/event/enum.Ime.html
5. winit IME issues/PRs (NSTextInputClient, #3617 selected/replacement range, Pinyin crash, 0.30 additions): https://github.com/rust-windowing/winit/issues/3617 and https://github.com/rust-windowing/winit/releases
6. Zed/GPUI input handling & NSTextInputClient (DeepWiki): https://deepwiki.com/zed-industries/zed/2.3-input-handling-and-key-dispatch
7. Zed terminal IME Enter bug (issue #23003): https://github.com/zed-industries/zed/issues/23003
8. Fig / Amazon Q autocomplete repo (figterm, fig_desktop tao/wry, fig_input_method, Accessibility API, specs schema, generators): https://github.com/withfig/autocomplete
9. Amazon Q Developer command line assistance (inline ghost text vs dropdown): https://docs.aws.amazon.com/amazonq/latest/qdeveloper-ug/command-line-assistance.html
10. Amazon Q Developer CLI (open-source autocomplete): https://github.com/aws/amazon-q-developer-cli-autocomplete
11. ropey 0.6.2 (char-indexed rope, grapheme/line-break handling): https://crates.io/crates/ropey/0.6.2
12. crop 0.4.3 (byte-offset rope, B-tree, benchmarks vs ropey): https://github.com/noib3/crop and https://lib.rs/crates/crop
13. cosmic-text 0.14.2 (shaping/layout/editor, HarfRust/swash, bidi): https://crates.io/crates/cosmic-text
14. zsh-autosuggestions (fish-style ghost text, strategies, accept-on-forward-char/end): https://github.com/zsh-users/zsh-autosuggestions
