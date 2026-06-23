# ADR-0001: Language and target platform

## Status

Accepted

## Context

aterm is a new, native, GPU-rendered terminal whose headline non-functional requirement
is a guaranteed 60fps floor (120fps on ProMotion) for typing, scrolling, and streaming
output. The prior prototype was a JVM/Kotlin Multiplatform application; the dossier
([00-overview.md](../research/00-overview.md), [09-performance-60fps.md](../research/09-performance-60fps.md))
concluded that a managed runtime with GC pauses on the render critical path is the wrong
foundation for a hard frame floor. The two existence proofs for "a 120fps GPU UI mixing a
monospace grid with proportional prose" in the wild - Zed and Warp - are both written in
Rust ([02-render-stack-eval.md](../research/02-render-stack-eval.md)). The terminal-engine,
PTY, text, and GPU ecosystems aterm depends on (`alacritty_terminal`, `portable-pty`,
`wgpu`, `winit`, `cosmic-text`/`swash`) are all Rust crates
([03-pty-vt-rust.md](../research/03-pty-vt-rust.md)).

The product is macOS-first by design intent: the iA aesthetic, the ProMotion frame-pacing
work, the Seatbelt sandbox, and the IME path are all macOS-specific in v1. But the owner
also requires that Linux and Windows are not architecturally precluded.

## Decision

- **Language: Rust.** No managed runtime on the critical path; zero per-frame allocation
  is achievable and CI-enforceable; the entire dependency stack is native Rust.
- **Primary target: macOS-first** (Apple Silicon, Metal). All v1 engineering, benchmarking,
  and the 60fps gate target Apple Silicon ProMotion hardware.
- **Linux/Windows are NOT precluded, but there is NO v1 work on them.** The architecture
  keeps the door open via two specific seams: `portable-pty` (which already carries a
  Windows ConPTY backend) for process/PTY, and a renderer trait inside the `aterm-ui` seam
  ([ADR-0002](0002-render-stack.md)) so a non-Metal backend can be added later without a
  rewrite. We pay no v1 cost for portability beyond choosing portable crates.

## Consequences

- We own a hand-rolled native stack with no GC, giving us full control over the frame
  budget - which the dossier says is the only honest way to guarantee the floor.
- No managed-runtime ecosystem (e.g. no JVM libraries); some capabilities are rebuilt in
  Rust. This is accepted: the prototype's most valuable assets (the risk gate, the input
  reducer, the block model) are pure logic that ports cleanly.
- macOS-first means VoiceOver/IME/Seatbelt/CADisplayLink work is Apple-specific and not
  abstracted in v1; the seams make later abstraction a contained effort, not a rewrite.
- Choosing portable crates (`portable-pty`, `wgpu`) over Unix-only or Metal-only ones
  (`pty-process`, the `metal` crate) is a small, deliberate insurance premium paid up
  front; the `metal` crate remains available as a macOS hot-path fallback behind the
  renderer trait if `wgpu` overhead ever threatens the budget.

## Alternatives considered

- **Keep the JVM/Kotlin prototype.** Rejected: GC pauses on the render critical path are
  incompatible with a hard frame floor, and the prototype was the thing aterm is rebuilding
  to escape ([06-agent-architecture.md](../research/06-agent-architecture.md)).
- **Zig (Ghostty's language) or C/C++.** Rejected: Rust gives equivalent performance with
  memory safety, and the reusable terminal/PTY/text/GPU ecosystem aterm needs is
  predominantly Rust ([03-pty-vt-rust.md](../research/03-pty-vt-rust.md)).
- **A cross-platform-first design (full Linux/Windows parity in v1).** Rejected as scope:
  the dossier front-loads the two existential risks (the perf floor and the engine) on one
  platform; full parity now would dilute that focus. "Not precluded" via portable crates is
  the agreed middle.
