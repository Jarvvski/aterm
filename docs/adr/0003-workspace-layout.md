# ADR-0003: Cargo workspace layout - six crates

## Status

Accepted

## Context

aterm must keep a strict separation between the terminal engine, the LLM/agent logic, the
renderer, design tokens, and the binary, so that: the engine and agent stay
unit-testable without a window; the renderer can be a swappable seam
([ADR-0002](0002-render-stack.md)); design tokens never create a dependency cycle; and the
perf harness can depend on the engine and renderer without pulling in the LLM. The dossier
([00-overview.md](../research/00-overview.md), [10-packaging-scaffold.md](../research/10-packaging-scaffold.md))
specifies this split, which mirrors the proven prior prototype and is enforceable in CI.

## Decision

A six-crate Cargo workspace with these exact crate names and dependency arrows:

- **aterm-core** - engine. PTY spawn/resize/signals (portable-pty), VT/ANSI parsing + grid
  (alacritty_terminal 0.26, the published crate - NOT Zed's fork), the block model,
  OSC-133/OSC-7 mark interception + nonce gating, the shell-integration shim extraction. No
  UI, no LLM. *Internal deps: none (leaf).*
- **aterm-tokens** - design tokens (colors, spacing, type scale, font names) as typed Rust.
  *Internal deps: none (leaf).*
- **aterm-agent** - `LlmProvider` trait + `AnthropicProvider` + `OpenAiProvider`, the
  provider-neutral event mapper, the agentic turn loop, the deterministic risk gate
  (zsh-aware argv parse), the single Secrets source, the `OutputSanitizer`,
  command-execution sinks, the `Sandbox` trait. *Internal deps: aterm-core.*
- **aterm-ui** - the renderer seam. winit windowing, the wgpu device/surface, the
  cosmic-text/swash glyph atlas + grid fast-path, layout/hit-testing/focus/IME, the
  timeline/block/input widgets, damage tracking, the CADisplayLink-driven present loop.
  *Internal deps: aterm-core, aterm-tokens.*
- **aterm-app** - the binary `aterm`. Wires ui+agent+core, owns the window + the 3-thread
  model, config load, the unified-input routing. *Internal deps: aterm-ui, aterm-agent (and
  transitively core/tokens).*
- **aterm-bench** - criterion + iai-callgrind harnesses; the scripted 60fps stress
  scenarios. *Internal deps: aterm-core, aterm-ui.*

Dependency direction (no cycles):

```
  app    -> { ui, agent }
  ui     -> { core, tokens }
  agent  -> core
  bench  -> { core, ui }
  tokens -> (leaf)
  core   -> (leaf)
```

`aterm-app` is the only crate that touches both the OS window/GPU and the LLM, and the
only crate packaged into a shippable `.app`.

## Consequences

- The engine (`aterm-core`) and the agent (`aterm-agent`) are heavily unit-testable with no
  window and no GPU - the risk gate, Secrets source, sanitizer, block model, and mark filter
  are all pure logic tested in isolation.
- The renderer lives entirely behind `aterm-ui`, satisfying the swappable-seam requirement
  of [ADR-0002](0002-render-stack.md).
- `aterm-tokens` as a leaf means UI and any future consumer can read tokens without ever
  creating a cycle back through the renderer.
- `aterm-bench` can stress the engine and renderer directly without compiling or linking the
  agent/LLM client, keeping the perf harness fast and focused.
- The no-cycle rule and the MIT/Apache-only-plus-GPLv3 dependency policy are enforced in CI
  (`cargo deny check licenses` with a GPL/AGPL denylist for *incoming* deps; a no-internal-
  cycle check). See [12-licensing.md](../research/12-licensing.md).

## Alternatives considered

- **A single crate / fewer crates.** Rejected: it would couple the renderer to the engine
  and the agent, defeating the swappable seam, slowing the perf harness, and making the
  engine/agent logic harder to test headlessly.
- **Splitting the agent's provider clients into separate crates per provider.** Rejected for
  v1 as premature: both providers sit behind one `LlmProvider` trait inside `aterm-agent`
  with a shared turn loop ([ADR-0005](0005-agent-loop-and-providers.md)); a per-provider
  crate split adds boundaries without current benefit.
- **Folding `aterm-tokens` into `aterm-ui`.** Rejected: tokens are consumed beyond the
  renderer and must stay a leaf to avoid cycles.
