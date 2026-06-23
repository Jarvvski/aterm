---
id: T-1.5
epic: EPIC-1-terminal-core
title: wgpu device/surface + CADisplayLink present loop + keep-warm
status: ready-for-agent
labels: [ui, perf, render, macos]
depends_on: [T-1.3]
---

# Goal

Stand up the `aterm-ui` render core: a winit window, a `wgpu` device/surface, and a render loop driven by a self-bridged `CADisplayLink` that presents every vsync during interaction and idles to zero frames when nothing changes. This is the load-bearing macOS frame-pacing work the 60fps floor depends on.

# Context

- Research: [09-performance-60fps.md](../../research/09-performance-60fps.md) sections 2.1-2.4 (CADisplayLink NOT CVDisplayLink; present-early; keep-warm ~1s; maximumDrawableCount 2-3; presentsWithTransaction disabled in steady state, enabled on resize; waitUntilScheduled). Risk: winit's macOS redraw timing is awkward; bridging CADisplayLink may need unsafe AppKit interop.
- ADR: render-stack commitment (custom wgpu, no GPUI, no blocking spike gate). This ticket carries the perf risk Epic 0's spike would have - pair with T-1.8.

# Implementation notes

- Crate: `aterm-ui`. Modules `window`, `gpu`, `present`.
- Dependencies: `winit = "0.30"`, `wgpu = "29"` (pin exactly). For the display link, bridge a `CADisplayLink` via `objc2`/AppKit interop on a high-priority thread; do NOT rely on winit's `request_redraw` for pacing (document winit issues #2640/#2954).
- Window: borderless/transparent-titlebar attributes (full work in T-8.1; here just get a window + surface).
- Present path (Metal): `maximumDrawableCount = 2` or 3; per-frame uniform/instance buffer pool (one set per in-flight frame, recycled in the completion handler); `presentsWithTransaction` disabled steady-state, enabled during resize; switch `waitUntilCompleted` -> `waitUntilScheduled` to avoid over-long main-thread blocks.
- Keep-warm: present every vsync for ~1s after the last input/PTY activity, then go idle (zero frames). A clean dirty/keep-warm flag, fed by the model snapshot version (T-1.3) and input events.
- Render thread reads the latest immutable snapshot (T-1.3 contract); never blocks on the model or PTY.
- Keep the renderer behind the `aterm-ui` seam (a `Renderer` trait) so a `metal`-crate backend could replace wgpu on the hot path later. Implement only the wgpu backend now.

# Acceptance criteria

- A window opens and clears to the canvas color at the display refresh rate; on ProMotion hardware it holds 120Hz while interacting (verified by present-interval logging; gated formally in T-7.2).
- After ~1s of no activity, drawn-frame count drops to ~0 (idle); a new input resumes presenting.
- A test/inspection confirms CADisplayLink (not CVDisplayLink) drives the loop and present-interval is stable (no 8-16ms oscillation) in a manual run.
- Resize re-enables `presentsWithTransaction` for the transaction and disables it afterward.
- No per-frame heap allocation in the steady-state present path (debug allocation assertion, shared with T-1.8).

# Out of scope

- Glyph atlas + grid drawing (T-1.6); this ticket can draw a clear color / a test triangle.
- Damage tracking refinement + perf validation (T-1.8).
- Hidden-titlebar bundle work (T-8.1).
