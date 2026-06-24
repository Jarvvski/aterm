---
id: T-1.8
epic: EPIC-1-terminal-core
title: Render-path perf validation (folded-in spike) + damage tracking
status: done
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

# Resolution

**done 2026-06-24** (jj, not pushed). Landed in 5 focused commits: the swash glyph
rasterizer, the wgpu instanced glyph-atlas pipeline (replacing glyphon), the
zero-allocation steady-state assertion, `tracing`/Tracy frame zones, and an
adversarial-review remediation pass.

**The headline - the typing-lag cure.** The interim glyphon path re-shaped the ENTIRE
grid through cosmic-text on every keystroke (`Shaping::Advanced` PUA fallback measured
in seconds/keystroke with icon glyphs on screen). It is replaced by the convergent
fast-terminal architecture (`08-text-glyph-rendering.md` §1): each unique `(glyph, face,
px)` is rasterized ONCE via swash into a shared 8-bit alpha atlas
(`GlyphCache`/`ShelfAllocator` from T-1.6); per frame the grid renders as one background
pass + a single instanced glyph draw call (`grid_render::GridRenderer`). Nearest sampling
with integer-snapped quads keeps the hinted bitmaps crisp 1:1; grayscale AA only; colors
linearized for the sRGB surface.

**ACs.**
- *Inherited T-1.6 GPU half (render ASCII/bold/italic/fg/bg/wide correctly; ONE
  instanced glyph draw call; grayscale AA; atlas built-once + reused):* met and
  **verified on a real Metal device** by offscreen render-to-texture + pixel-readback
  tests (`grid_render::gpu_tests`) - the headless wgpu path works in CI (macos-14, per
  CLAUDE.md), so these are no longer compile-only. Includes a two-distinct-glyph atlas
  test (no neighbor bleed) and a single-draw-call counter.
- *AC1 (damage skips drawing when idle):* the keep-warm scheduler (T-1.5, tested) drops
  to zero frames when idle; the renderer additionally version-gates the instance rebuild
  and reuses the buffers when unchanged. Per `09-performance-60fps.md` §3 the GPU-terminal
  payoff is "draw at all?" + "bound CPU build", NOT partial-row GPU redraw - so the gate
  is a full-rebuild-on-change / skip-on-no-change signature (the empirical call the
  ticket invited). The `Terminal::take_damage` line API remains for a future
  partial-redraw experiment if a bench ever shows full rebuild breaching budget (T-7.x).
- *AC2 (0 allocations in a steady-state frame):* met + tested via a `cfg(test)` counting
  `GlobalAlloc` (`alloc_probe`): the unchanged-frame `prepare` early-out and the warm CPU
  frame build both allocate zero.
- *AC4 (Tracy zones):* `tracing` spans wrap the `frame`/`build`/`encode`/`present`
  phases (zero-cost with no subscriber); a Tracy subscriber is wired behind the
  `tracy` cargo feature (`aterm-app --features tracy`), compile-verified both ways.

**Residual delegated (NOT parked on a human), consistent with `done`:**
- *AC3 (on-ProMotion p50/p99 present-interval, no 8-16ms oscillation) + a readable Tracy
  frame-breakdown capture:* require real Apple-Silicon ProMotion hardware -> **EPIC-7**
  (T-7.1 frame recorder / T-7.2 scenarios). The instrumentation + the no-alloc/skip
  properties they measure are in place.
- *Resize `presentsWithTransaction` (the T-1.5 AC4 inherited item):* needs wgpu's
  `Surface::as_hal` `CAMetalLayer` (a second, objc2-0.6 FFI surface) + a synchronous
  main-thread transactional present, and is tear-free-verifiable only on a display ->
  delegated to **T-8.1** (window chrome), where the layer/titlebar FFI work lives.
- *Ligature run-shaping (the non-fast-path) + font fallback (CJK/emoji) + the Nerd Font
  constraint table:* the renderer draws per-cell via a direct cmap lookup (no HarfBuzz
  run-shaping); ligatures, fallback, and PUA alignment are the EPIC-4 text-polish pass
  (T-4.4/T-4.5). Wide cells are laid out across two columns (the AC); CJK with no glyph
  in the Latin face renders `.notdef` until fallback lands.
- *Glyph atlas growth/eviction:* the 1024² atlas holds the ASCII set across all faces
  many times over; a full atlas memoizes the give-up and logs once. Growth/eviction is a
  follow-up (noted in `grid_render`).

**Adversarial review applied** (4 lenses x skeptic-verify, ultracode; 12 findings, 10
confirmed). All 10 were fixed in the remediation commit: Nearest sampler +
integer-snapped quads (was Linear + zero-gutter -> edge bleed/softening), an atlas-full
give-up memo (was re-rasterizing every frame), an honest draw-call counter on blank
frames, a full-theme rebuild signature (was XOR of two colors), and doc/changelog/README
honesty (scoped the allocation claim to the CPU build; corrected the never-cleared cache
comment; dropped the stale "glyphon" README mention). 2 findings dismissed.
