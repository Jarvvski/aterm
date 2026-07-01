# aterm - System Architecture

Status: Phase-2 design. This document is the authoritative system architecture for
building aterm. It is built on the research dossier in `docs/research/` (start at
[00-overview.md](research/00-overview.md)) and on the locked decisions recorded as
ADRs in `docs/adr/`. Where a research doc and its adversarial verification disagreed,
the corrected fact is used here, matching the overview.

aterm is a native, GPU-rendered macOS terminal in Rust. It clones the *behavior* of
Warp - a controlled native UI wrapping a hidden background login shell over a PTY,
rendering its own block-based timeline rather than a raw VT grid - with the radically
minimal visual language of iA Writer. The headline non-functional requirement, against
which every architectural choice is judged, is a guaranteed 60fps floor for normal use
(typing, scrolling, streaming output), and 120fps on ProMotion. See
[09-performance-60fps.md](research/09-performance-60fps.md).

---

## 1. The crate spine

aterm is a six-crate Cargo workspace. The split mirrors the proven prior prototype and
is enforced in CI (`cargo deny`, no-cycle check). The full rationale is
[ADR-0003](adr/0003-workspace-layout.md).

```
aterm-core    engine. PTY spawn/resize/signals (portable-pty), VT/ANSI parsing +
              grid (alacritty_terminal 0.26, published crate - NOT Zed's fork), the
              block model, OSC-133/OSC-7 mark interception + nonce gating, the
              shell-integration shim extraction. No UI, no LLM.
              internal deps: none (leaf)

aterm-tokens  design tokens (colors, spacing, type scale, font names) as typed Rust.
              internal deps: none (leaf)

aterm-agent   LlmProvider trait + AnthropicProvider + OpenAiProvider, the
              provider-neutral event mapper, the agentic turn loop, the deterministic
              risk gate (zsh-aware argv parse), the single Secrets source, the
              OutputSanitizer, command-execution sinks, the Sandbox trait.
              internal deps: aterm-core

aterm-ui      the renderer SEAM. winit windowing, the wgpu device/surface, the
              cosmic-text/swash glyph atlas + grid fast-path, layout / hit-testing /
              focus / IME, the timeline / block / input widgets, damage tracking, the
              CADisplayLink-driven present loop.
              internal deps: aterm-core, aterm-tokens

aterm-app     the binary `aterm`. Wires ui + agent + core, owns the window + the
              3-thread model, config load, the unified-input routing.
              internal deps: aterm-ui, aterm-agent (transitively core/tokens)

aterm-bench   criterion + iai-callgrind harnesses; the scripted 60fps stress scenarios.
              internal deps: aterm-core, aterm-ui
```

### 1.1 Crate-dependency diagram

The arrows point from dependent to dependency. There are no cycles. `aterm-app` is the
only crate that touches the OS window/GPU *and* the LLM; it is the only crate packaged
into a shippable `.app`.

```
                          +-------------+
                          |  aterm-app  |  (binary: the only packaged crate)
                          +------+------+
                          /             \
                         v               v
                  +-----------+    +-------------+
                  | aterm-ui  |    | aterm-agent |
                  +-----+-----+    +------+------+
                   /        \              |
                  v          v             v
          +-------------+   +------------------+
          | aterm-tokens|   |    aterm-core    |
          +-------------+   +------------------+
                                    ^
                                    |
                  +-------------+   |
                  | aterm-bench |---+----> aterm-ui
                  +-------------+

  Dependency direction (textual):
    app    -> { ui, agent }
    ui     -> { core, tokens }
    agent  -> core
    bench  -> { core, ui }
    tokens -> (leaf)
    core   -> (leaf, internal)
```

The `aterm-ui` crate is a deliberate **seam**, not just a layer. Its public API is a set
of aterm-owned traits ("render this block", "render the input", "render a transcript
card") with the custom wgpu+parley renderer as the only implementation in v1. GPUI
remains a theoretical fallback behind that seam, never used. See
[ADR-0002](adr/0002-render-stack.md).

---

## 2. The three-thread model

The 60fps floor is won or lost in the thread topology. aterm uses three long-lived
threads communicating over **bounded** channels, the convergent design of every fast
native terminal (Alacritty, Ghostty, Rio, Zed/GPUI), validated by
[03-pty-vt-rust.md](research/03-pty-vt-rust.md) and
[09-performance-60fps.md](research/09-performance-60fps.md). Backpressure is implicit:
bounded channels mean a flooding producer eventually blocks on `send`, which blocks the
PTY `read`, which lets the kernel PTY buffer apply OS-level flow control. No
application-level unbounded queue, no unbounded memory growth.

```
   kernel PTY        +------------------+   bytes     +-----------------------+
   (login shell) --> |   PTY reader     | ----------> |   model / VT / block  |
                     |   thread         |  (bounded   |   thread              |
   stdin <---------- |                  |   channel)  |                       |
        ^            +------------------+             +-----------+-----------+
        |                                                         | publishes
        | command bytes                                           | immutable
        | (on submit)                                             v RenderSnapshot
        |            +------------------+   vsync     +-----------------------+
        +----------- |   render thread  | <---------- |  (arc-swap / triple   |
                     |  (CADisplayLink) |  callback   |   buffer of snapshot) |
                     +------------------+             +-----------------------+
                              ^
                              | reads latest snapshot only; never blocks
                              | on model or PTY

   main / app thread (winit event loop): window + raw input events. Records input
   intent into a lock-light input state and requests a redraw; never parses or lays
   out inside a key handler. On macOS (macOS-first) the render thread may call
   drawFrame() directly (Metal does not force draw-from-app-thread).
```

### 2.1 PTY reader thread

Owns `master.try_clone_reader()` from `portable-pty`. Loops blocking `read()` into a
single reusable ~64 KiB buffer (Zellij's proven buffer size) and sends the bytes over a
bounded channel to the model thread. It never touches the GPU, the grid, or the block
model. PTY spawn/resize/signals and process-group tracking live in `aterm-core`; see
[ADR-0007](adr/0007-terminal-engine.md).

### 2.2 Model / VT / block thread

Owns the `alacritty_terminal::Term`, the `Grid<Cell>`, and the `BlockList`. It drains
the channel, runs bytes through the OSC-133/7 pre-parser mark filter, then through the
VT parser, mutates the grid and blocks, and publishes an immutable `RenderSnapshot` plus
a damage set to the renderer.

**Burst coalescing is mandatory.** The thread does not wake the renderer per chunk; it
merges everything available within a ~4-8ms tick (comfortably under the 8.33ms/120fps
and 16.67ms/60fps budgets) so a megabyte burst becomes one parse pass and one frame, not
thousands. This directly mitigates the documented GPUI-terminal `cat`-flood freeze that
was fixed by exactly such a batching interval. Parsing is decoupled from rendering: the
grid may be mutated hundreds of times between two vsyncs; the renderer only ever sees the
latest coherent snapshot. See [03-pty-vt-rust.md](research/03-pty-vt-rust.md) sec. E and
[09-performance-60fps.md](research/09-performance-60fps.md) sec. 5.

The model thread also handles `alacritty_terminal` `Event`s, including wiring
`Event::PtyWrite` (DA/DSR/cursor-position query replies) back to the PTY master writer -
dropping these breaks programs that probe the terminal.

### 2.3 Render thread

Driven by a self-bridged `CADisplayLink` callback (NOT `CVDisplayLink`, which Zed
measured oscillating 8-16ms and reverted). Each callback: read the latest snapshot via a
lock-light handoff (arc-swap or a triple-buffered `Mutex<Snapshot>`), and if dirty (or
inside the keep-warm window) build the frame and present at vsync. It never blocks on the
model thread or the PTY.

Frame-pacing discipline inherited from Zed's writeup (read, not imported): present every
vsync during interaction; keep the display "warm" for ~1s after the last input or PTY
activity to defeat ProMotion down-clocking; triple-buffer per-frame instance/uniform
buffers recycled in the Metal completion handler; `present_drawable` before `commit()`;
`presentsWithTransaction` disabled in steady state and re-enabled only during
startup/resize. Zero per-frame heap allocation in the hot path, CI-asserted. See
[ADR-0002](adr/0002-render-stack.md) and
[09-performance-60fps.md](research/09-performance-60fps.md) sec. 2.

---

## 3. Data-flow pipeline

This is the canonical path a byte takes from the shell to the screen. Each arrow is a
pure transformation owned by a named module in `aterm-core` (except the final hop into
`aterm-ui`).

```
  PTY bytes
     |
     v
  OSC mark filter            strips our OSC-133 (A/B/C/D) + OSC-7 marks to zero-width,
  (pre-parser)               tags each with an offset into the clean passthrough text,
                             enforces the per-session nonce, decides alt-screen
                             suppression at FIRE TIME (not parse time)
     |  (clean bytes + offset-tagged mark events)
     v
  VT parser                  alacritty_terminal's vte::ansi::Processor::advance drives
  (alacritty_terminal)       the Williams state machine into Term's Handler impl
     |
     v
  grid                       Grid<Cell>: live VT surface, reflow, alt-screen, line-level
  (Grid<Cell>)               damage. The renderer walks RenderableContent.display_iter.
     |
     v
  block segmentation         OSC-133 A->B->C->D drives the BlockList lifecycle:
  (BlockList)                A opens a prompt region, C opens a RunningBlock over the
                             live grid, D finalizes the block with exit code
     |
     v
  immutable per-block        on command finish, output rows are snapshotted into an
  snapshots                  immutable CommandBlock { output: Vec<RowRun>, exit_code,
                             cwd, cmdline, ts }. History is immune to later grid
                             reflow/eviction (dodges alacritty reflow sharp edges).
                             A SumTree height index gives O(log n) viewport queries.
     |
     v
  RenderSnapshot             immutable view handed to the render thread: visible blocks
  (-> aterm-ui)              (by SumTree viewport intersection), the live grid or
                             alt-screen surface, cursor, selection, damage set, the
                             current InputModel view, and the agent transcript entries
```

Key invariants:

- **Marks are intercepted by us, before the emulator.** `alacritty_terminal` does not
  parse OSC 133 (its issue #5850 is open). The mark filter is load-bearing and must
  handle split sequences across reads and both `BEL` and `ST` terminators exactly. See
  [04-shell-integration.md](research/04-shell-integration.md) and
  [ADR-0008](adr/0008-shell-integration.md).
- **Alt-screen suppression is decided at fire time.** When a TUI emits a stray OSC-133
  mark, the decision to suppress it reads the *current* alt-screen flag against the
  drained emulator state, because the toggling CSI may still be unparsed passthrough when
  the mark is first seen.
- **Finished blocks are immutable.** Only the live grid goes through alacritty reflow on
  resize; history is re-wrapped from our own stored snapshots. See
  [ADR-0007](adr/0007-terminal-engine.md).
- **Full-screen apps live outside the block list.** On `?1049h` (detected via
  `TermMode::ALT_SCREEN`) the UI switches to render the alt grid as one full-window
  surface with input passed straight through to the PTY; on exit the command becomes a
  compact `Interactive` block ("ran vim - 12s") with no captured output.

---

## 4. The unified-input data model

aterm has ONE shell-first input box. A hotkey toggles where Enter routes (live shell vs
the AI agent); typed text is preserved verbatim across the toggle. This is a locked
decision, not a sigil scheme. The full rationale and the prototype sins it fixes are in
[ADR-0004](adr/0004-unified-input-model.md) and
[05-unified-input-ux.md](research/05-unified-input-ux.md).

The model is a single pure-Rust reducer. `mode` lives *inside* the model so that mode and
text are atomically consistent and the toggle is a one-field mutation that provably
cannot touch the text:

```rust
struct InputModel {
    rope: Rope,                 // ropey 0.6 (or String for v1); buffer is not the bottleneck
    selection: Selection,       // primary caret + optional anchor; multi-caret later
    mode: InputMode,            // Shell | Agent  -- the hotkey flips ONLY this field
    undo: Vec<Snapshot>,
    preedit: Option<Preedit>,   // active IME composition (string + byte-range cursor)
    overlay: Highlight,         // non-inheritable style spans, computed ASYNC, off the render loop
    ghost: Option<GhostText>,   // async suggestion tail
}
enum InputMode { Shell, Agent }
struct Preedit { text: String, cursor: Option<(usize, usize)> } // winit byte indices
```

Properties ported from the prototype's `CommandBuffer`:

- The buffer **stores characters only and never interprets them**. A paste is one
  `insert` of the whole string as one undo unit; embedded newlines/control chars are
  literal and inert. This structurally prevents the "paste auto-executes" class of bug.
- Submit is the *caller* reading `text` then resetting; the buffer does not decide
  whether Enter submits.

The routing brain is a priority-ordered gate. The top-priority gate is **preedit-active**:
while an IME composition is in progress, Enter/Tab/Esc are owned by the IME (confirm /
cancel candidate) and never trigger submit-to-shell, submit-to-agent, or
completion-accept (fixes the Zed #23003 Enter-eats-candidate bug). Subsequent gates:
agent-holds-shell, `Opt-Enter` one-shot send-to-agent, degraded/heuristic, alt-screen
pass-through, in-flight stdin, then plain Enter routed by `mode`.

Mode is shown by **mode-accent caret tint + prompt glyph** (Shell = blue caret + `❯`;
Agent = purple caret + `◇`; the two mode accents per ADR-0011), no banner. All
highlight/parse/ghost/completion work runs async and debounced (long idle debounce,
short-circuit on space/paste/selection), applied as non-inheritable style spans; the
render loop reads the last-good overlay and never blocks - protecting the frame floor.

IME is implemented via winit 0.30 `Ime` events first, with a hand-rolled
`NSTextInputClient` as the documented escape hatch if winit's known gaps bite.

---

## 5. The agent loop and safety architecture

aterm is full-agentic from day one: a client-side manual loop calling the LLM Messages
API directly over HTTP from a thin Rust client. There is no Agent SDK and no Managed
Agents (their tools run in Anthropic's container; aterm must run commands on the user's
machine). The full rationale is in [ADR-0005](adr/0005-agent-loop-and-providers.md),
[ADR-0006](adr/0006-safety-gate-and-sandbox.md), and
[06-agent-architecture.md](research/06-agent-architecture.md).

### 5.1 The turn loop

The loop runs on a tokio runtime **off** the render thread. SSE deltas land on the UI by
channel and mutate the current timeline entry incrementally - never relaying out the
whole timeline per delta.

```
  plan  ->  act  ->  observe  ->  repeat
   ^                                  |
   +---- loop while stop_reason == "tool_use" ----+

  POST /v1/messages (stream: true)
     |
     v
  SSE: message_start -> content_block_start/delta/stop -> message_delta -> message_stop
     |                                    |
     | accumulate AssistantText/Thinking  | tool_use block opens a ToolCall
     v                                    v
  provider-neutral event mapper  ----> turn loop
     |
     v
  for each tool_use:  risk gate (deterministic, code-side)
     |                    |
     |  Safe + no         |  Caution / Dangerous, or any shell-active reason
     |  shell-active      v
     |  reason       confirmation UI (timeline proposal w/ risk reasons)
     v                    |
  execute in Sandbox <----+ (on approve)
  (Seatbelt + setrlimit + timeout-kill)
     |
     v
  OutputSanitizer (redact secret values BEFORE truncation)
     |
     v
  tool_result block  ->  back into the next /v1/messages request
```

### 5.2 Providers

Multi-provider in v1. One `LlmProvider` trait with two implementations - `AnthropicProvider`
(default; Claude `claude-opus-4-8`, adaptive thinking + the `effort` param, Messages API)
and `OpenAiProvider` (Responses API) - behind a provider-neutral event mapper and one
shared turn loop. This mirrors the prior prototype. The mapper normalizes each provider's
streaming events into a single internal event type so the turn loop is provider-agnostic.

### 5.3 Safety architecture (four layers, none trusted alone)

1. **Deterministic code-side risk gate** (ported from the prototype's `CommandLineRisk`/
   `Risk`). Parses each proposed command's argv (zsh-aware), over-approximates toward
   `RequireConfirm`/`Dangerous`, never trusts the model's self-reported risk, splits
   multi-line buffers and takes the MAX risk. Tools are typed (`run_command` takes an
   argv `string[]`, no shell), so the gate sees structured args, not an opaque string.
2. **Single Secrets source** feeding BOTH the gate (sensitive-path deny-set) and the
   `OutputSanitizer` (redacts secret values before truncation), so the two defenses
   cannot drift. This is the single most important structural invariant.
3. **Mandatory macOS Seatbelt sandbox** via `sandbox-exec`, behind a `Sandbox` trait,
   plus `setrlimit` (CPU/address-space/open-files) and process-group timeout-kill. The
   gate is a classifier, not a boundary; the sandbox is the boundary. Because the default
   trust surface is larger (auto-safe), the sandbox is mandatory, not optional.
4. **Approval UX + autonomy controls in the timeline.** Default autonomy is **AUTO-SAFE
   ON**: commands the gate proves `Safe` (and that carry no shell-active reason) auto-run;
   `Caution`/`Dangerous` always require explicit confirmation. Auto-run never clears
   shell-active strings.

Prompt-injection defense is layered: the agent reads untrusted command output, file
contents, and tool results, so the deterministic gate (which classifies the *parsed
command*, not the model's rationale) is the primary anti-injection control, backed by
output sanitization, structural separation of tool results from instructions, the
auto-safe-but-confirm-everything-else default, and the sandbox as backstop.

---

## 6. The single wall-clock timeline

The structural reason human and agent activity interleave with no special-casing is that
there is **one block primitive and one timeline**, sorted by wall-clock timestamp. This
is Warp's actual model, confirmed in [01-warp-internals.md](research/01-warp-internals.md)
and [06-agent-architecture.md](research/06-agent-architecture.md).

- Human command blocks are OSC-133-delimited shell commands (sec. 3).
- Agent activity is modeled as `AgentTurn`s composed of timestamped `AgentStep`s
  (`UserPrompt`, `Thinking`, `AssistantText`, `ToolCall`, `ToolResult`, `Approval`).
- **Agent-run commands are ordinary terminal blocks.** A `run_command` tool call that the
  agent executes produces a command block in the same `BlockList` as a human-typed
  command. Agent conversation steps are additional block *variants* in that same list.
- Every `AgentStep` carries its own timestamp, so a long-running `ToolCall` interleaves
  correctly with a human typing in another block - the timeline is sorted by `ts`, not by
  turn.
- The **conversation history sent to the API** is derived from the `AgentTurn` (assistant
  `content` blocks + `tool_result` user messages) and is kept *separate* from the richer
  rendered timeline (which carries glossed risk reasons, approval state, and sanitized
  output). `tool_use_id` is the join key across `ToolCall`, `Approval`, and `ToolResult`.

```
   timeline (sorted by wall-clock ts)
   +--------------------------------------------------+
   | HumanBlock   $ git status                 12:01  |   <- OSC-133 shell command
   | HumanBlock   $ cargo build                12:02  |
   | AgentTurn    "fix the failing test"       12:03  |   <- UserPrompt
   |   Thinking   (summarized)                 12:03  |
   |   AssistantText  streamed deltas...       12:03  |
   |   ToolCall   run_command [cargo test]     12:03  |   <- renders as a command block,
   |   ToolResult (sanitized output)           12:04  |      auto-run because gate=Safe
   |   AssistantText  "the test now passes"    12:04  |
   | HumanBlock   $ git diff                    12:04  |   <- human interleaves freely
   +--------------------------------------------------+
                       ^ one BlockList, one SumTree height index, virtualized twice
                         (blocks intersecting viewport, then rows within each)
```

Streaming maps to incremental entry mutation: a `content_block_delta` appends to the
current `AssistantText` or `Thinking` step; the render loop watches a dirty-flag/version
on the current entry and never re-lays-out the whole timeline per delta. This is the
60fps requirement reaching into the agent layer.

---

## 7. Where the floor is enforced

The 60fps floor is an architectural property aterm owns, not a tuning afterthought. It is
enforced by: the vsync-driven `CADisplayLink` render loop (sec. 2.3); damage/dirty-region
tracking; PTY/model/render thread decoupling with burst coalescing (sec. 2.2); zero
per-frame allocation; present-early + ~1s keep-warm to keep ProMotion clocked high. The
`aterm-bench` crate is the standing proof: `iai-callgrind` instruction-count micro-benches
gate every PR on noisy shared runners, and an in-process frame recorder runs the scripted
stress scenarios (`fast_scroll`, `output_flood`, `large_scrollback`,
`agent_stream_while_typing`, `window_resize`, `fullscreen_tui_redraw`, `idle`) on real
Apple Silicon ProMotion hardware against p50/p99/max/dropped-frame and input-latency
gates. The hard gate is the 60fps floor (16ms); 120fps is tracked. See
[09-performance-60fps.md](research/09-performance-60fps.md).
