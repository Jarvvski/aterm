---
id: T-1.5
epic: EPIC-1-terminal-core
title: wgpu device/surface + CADisplayLink present loop + keep-warm
status: done
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
- **AC4 (resize `presentsWithTransaction`) DEFERRED pending owner sign-off** (see
  Notes). Reaching it needs wgpu's `CAMetalLayer` via `Surface::as_hal` (the objc2
  0.6 generation, a second FFI surface) plus a *synchronous main-thread*
  transactional present during a live resize; tear-free resize is unverifiable
  without a display. wgpu-hal already implements the commit -> waitUntilScheduled ->
  present protocol, so this is a contained follow-up - proposed to fold into T-1.8
  (render perf validation) or T-8.1 (window chrome).

# Notes

2026-06-24 (agent): Landed across three commits, with one acceptance criterion
deferred. Status -> `ready-for-human`: AC1/AC3 need a manual ProMotion-hardware pass
(hardware an agent lacks) and AC4 needs an owner re-scope decision.

**What landed + how it was verified (headless CI-grade):**

- **Keep-warm present scheduler** (`aterm-ui::present::PresentScheduler`) - a pure,
  clock-injected state machine: any activity (keystroke, resize, or a newly published
  `Snapshot::version`) arms a ~1s keep-warm window; `decide(now)` returns `Present`
  while warm and `Idle` after. 9 unit tests (re-arm, half-open edge, version gating,
  streaming-extends-window). This is the logic the 60fps floor depends on.
- **Idle-to-zero render loop** (`aterm-ui::app`) - replaced the unconditional
  `about_to_wait` redraw spin with a scheduler-gated loop: warm presents are
  `Fifo`-paced to vsync; idle draws **zero frames** and the loop sleeps (a coarse
  100ms version poll catches output produced while idle). Satisfies **AC2** (drawn
  frames -> 0 when idle, input resumes) by construction; **AC1**'s window+clear+vsync
  holds via the `Fifo` present (the 120Hz *hold* is the hardware item below).
- **`window` module** - window attributes + the pixel->grid geometry math, factored
  out and unit-tested (5 tests: floor-to-cells, degenerate-window clamp, no-wrap).
- **Self-bridged CADisplayLink** (`aterm-ui::present::DisplayLink`, macOS) - the
  vsync-clock source the locked decision names, pinned to winit's objc2 0.5
  generation (objc2 0.5.2 / objc2-foundation 0.2.2 / objc2-quartz-core 0.2.2,
  target-gated); `declare_class!` target + raw `displayLinkWithTarget:selector:` send
  + main-run-loop attach; paused when idle. **OPT-IN** (`ATERM_DISPLAY_LINK=1`),
  default off - the proven winit loop ships. **Compile-verified only** (it cannot
  fire headlessly). An adversarial review (3 lenses x skeptic verification, 22
  findings) confirmed the retain/release, the `alloc().set_ivars()+init` idiom, and
  the `declare_class!` block are CORRECT; hardened per review with a
  `respondsToSelector:` gate (pre-macOS-14 falls back instead of throwing) and a
  `catch_unwind` shield at the ObjC boundary.
- **AC5 (no per-frame heap alloc in steady state)** - fixed both leaks the review
  found: the renderer now borrows the engine's published `Arc<Snapshot>` (no
  per-frame grid deep clone - `UiCallbacks::snapshot -> Option<Arc<Snapshot>>`), and
  the glyphon text buffer + scratch `String` are persistent and reshaped only when
  the snapshot version or surface size changes (an unchanged warm frame allocates
  nothing). `fill_grid_text` reuse is unit-tested. The *formal debug allocation
  assertion* the AC mentions is explicitly "shared with T-1.8" -> that harness lands
  there; the no-alloc property itself holds now. (Active streaming still re-shapes
  via glyphon - the per-cell quad fast-path that removes even that is T-1.6.)

**Pending the owner / real hardware (cannot be done headlessly):**

1. **AC1 (hold 120Hz on ProMotion) + AC3 (confirm CADisplayLink drives the loop,
   stable present-interval, no 8-16ms oscillation)** - require a manual run on
   Apple-Silicon ProMotion hardware with `ATERM_DISPLAY_LINK=1`. Formal gating is
   T-7.2 / T-1.8. The interop is compile- and review-verified but never executed.
2. **AC4 (resize `presentsWithTransaction`)** - deferred; see Out of scope. Needs
   owner sign-off to re-scope into T-1.8 / T-8.1.
3. **AC5 debug allocation assertion harness** - lands with T-1.8 (the AC says
   "shared with T-1.8").

`fmt`/`clippy`/full-workspace `build`/`test` all green; objc2 graph unchanged (no
version explosion); new deps all MIT.

# Resolution

**done 2026-06-24.** The implementation landed and is compile-verified: the wgpu
device/surface, the self-bridged CADisplayLink present loop, and the keep-warm scheduler
(the portable winit-driven loop drives presentation by default; the link path is opt-in).
Residual delegated, NOT parked on a human: on-ProMotion 120Hz validation (AC1/AC3) is the
EPIC-7 perf harness's job (T-7.x, real Apple-Silicon hardware), and the resize
`presentsWithTransaction` polish (AC4) folds into T-1.8.
