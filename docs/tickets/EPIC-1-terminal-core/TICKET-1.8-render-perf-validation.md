---
id: T-1.8
epic: EPIC-1-terminal-core
title: Render-path perf validation (folded-in spike) + damage tracking
status: ready-for-agent
labels: [ui, perf, render]
depends_on: [T-1.5, T-1.6]
---

# Goal

Validate the wgpu + CADisplayLink + atlas path against the frame-time budget end-to-end, and implement damage tracking so the renderer decides *whether to draw at all* and bounds CPU frame-build work. This is the perf-validation that replaces Epic 0's blocking render spike - it runs as early proof, not a gate.

# Context

- Research: [09-performance-60fps.md](../../research/09-performance-60fps.md) sections 1 (tail-latency, p99 not mean), 3 (damage tracking), 4 (zero per-frame allocation), Risk list. [08-text-glyph-rendering.md](../../research/08-text-glyph-rendering.md) section 5. The dossier folds the 1-2 week spike here because the owner committed to building directly on wgpu.

# Implementation notes

- Crate: `aterm-ui`.
- Damage: read `TermDamage` from `aterm-core` (T-1.2) via the snapshot; use it to (1) skip the frame entirely when nothing is dirty and we're outside the keep-warm window, and (2) bound the instance-buffer rebuild. Note the dossier caveat: full-instance rebuild may be cheap enough vs partial GPU redraw - measure and pick empirically.
- Zero per-frame allocation: a debug `GlobalAlloc` wrapper asserting 0 allocations during a steady-state frame; wire it into a test build.
- Instrument the present path: capture present-interval (CADisplayLink timestamp deltas), CPU frame-build time. Use Tracy (`tracing-tracy`) zones for input/parse/build/encode; validate the present path in Xcode Metal System Trace manually (document findings).
- Write a short perf-validation note (in the ticket's PR description or a scratch doc, NOT a committed .md per repo conventions) recording measured p50/p99 frame time on real Apple Silicon for: idle, plaintext scroll, output flood. If any breaches the 16ms floor, that is a finding to escalate (the seam allows a `metal` backend fallback).

# Acceptance criteria

- Damage tracking demonstrably skips drawing when idle (drawn-frame count ~0; ties to T-1.5 keep-warm).
- The debug allocation assertion passes: 0 allocations in a steady-state frame.
- A manual run on Apple Silicon ProMotion records p50/p99 present-interval for idle / scroll / flood and the numbers are captured. No 8-16ms oscillation in steady state.
- Tracy zones are present and produce a readable frame breakdown.

# Out of scope

- The full Tier-2 recorder + 7 scenarios + CI gate (Epic 7); this is early manual validation.
- Resize/reflow perf (T-7.4).

# Notes

**Inherited 2026-06-24 (from T-1.6, T-1.5).** Beyond its own damage-tracking + perf
scope, this ticket now also carries: (a) the GPU half of T-1.6 - swash/CoreText
rasterization + the wgpu instanced pipeline that replaces the interim glyphon text path
on screen, consuming the `GridCell`/`GlyphCache`/`ShelfAllocator` CPU half already
landed; and (b) the resize `presentsWithTransaction` polish from T-1.5 (AC4). The
instanced fast-path is also the real cure for the typing-lag stand-in (per the render
diagnosis: glyphon `Shaping::Advanced` per-keystroke full-grid reshape). On-hardware
120Hz/ProMotion validation itself is EPIC-7 (T-7.x).
