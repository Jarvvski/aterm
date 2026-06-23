---
title: Guaranteeing & Proving 60fps
domain: performance-60fps
status: research
---

# Guaranteeing & Proving 60fps

## TL;DR

- **A 60fps floor is achievable in Rust but is NOT free; it is an architectural property, not a tuning afterthought.** The decisive wins are: (1) a render loop driven by the display's own vsync callback, (2) damage tracking so we redraw only changed cells, (3) decoupling PTY-read from render so a flooding subprocess cannot stall the UI, and (4) zero per-frame heap allocation in the hot path. Every fast Rust/native terminal (Alacritty, Ghostty, Rio) and Zed's GPUI all converge on these same four pillars [1][3][4][6].
- **On macOS, drive the render loop from `CADisplayLink` (via the Metal-recommended pattern), NOT `CVDisplayLink`.** Zed shipped `CVDisplayLink`, measured frame times oscillating 8-16ms (i.e. dropping from 120 to 60fps), and reverted to `CADisplayLink`; ProMotion down-clocks the panel the moment you skip a present, so you must present every vsync during interaction and keep the display "awake" for ~1s after the last input [3].
- **Decouple PTY read from render with bounded backpressure.** The single most common 60fps killer is a flood (`cat hugefile`, `yes`): a naive "parse-then-redraw on every read" pegs the main thread. The fix (Ghostty, Alacritty) is an I/O thread that parses into the grid continuously, while the renderer samples a *snapshot* of grid state at most once per vsync. Output throughput is decoupled from frame rate; the screen only ever shows the latest coherent state [4][6].
- **Prove it with a two-tier, CI-gated benchmark harness.** Tier 1 (micro, deterministic): `iai-callgrind` instruction-count benchmarks of the VT parser + grid mutation + damage computation - zero variance, runs on noisy GitHub Actions runners, fails the PR on regression [8]. Tier 2 (frame-level, on a real Mac): an in-process frame-time recorder that captures CPU frame time, GPU frame time, and dropped-frame count under scripted stress scenarios, asserting **p99 frame time <= 8.0ms (120Hz) / <= 16.0ms (60Hz)** and **zero dropped frames** in steady state.
- **Recommended stack:** `winit` 0.30.x for windowing/input, render driven by a `CADisplayLink` we bridge ourselves, GPU via **`wgpu` 29.x** for portability *unless* profiling shows its 5-10% validation/state-tracking overhead threatens the budget - in which case drop to the `metal` crate on the macOS hot path [2][5]. Profiling via **Tracy** (`tracing-tracy`) for live frame inspection plus Xcode **Metal System Trace** for GPU-side truth [7].
- **The honest risk:** wgpu's portability tax and winit's historically awkward macOS redraw timing [10] both sit directly on the critical path. Budget a spike (1-2 weeks) to validate the `CADisplayLink` + present path end-to-end before committing the render stack. This is flagged for the owner under Open Questions.

## Findings

### 1. The frame-budget model

The headline numbers:

- **60fps => 16.67ms** per frame, end to end (input sampled -> grid updated -> frame drawn -> presented at vsync).
- **120fps (ProMotion) => 8.33ms** per frame.
- These budgets cover **everything on the critical path**: input handling, VT parsing of any new PTY bytes that must appear this frame, grid mutation, damage computation, vertex/instance buffer build, GPU command encode + submit, and present. They do **not** include work we successfully pushed off the critical path (async PTY reads, glyph rasterization on a worker, etc.).
- A useful internal sub-budget for 120Hz (8.33ms): input + state update <= 2ms, frame build (CPU, vertex/instance data) <= 2ms, GPU encode + draw <= 3ms, with ~1.3ms slack. These are targets to instrument against, not guarantees.

The critical insight from Zed's 120fps work: **frame budget is a tail-latency problem, not an average problem.** Their frame times "oscillating between 8ms and 16ms" still average well under 16ms but *feel* and *measure* as stutter because individual frames miss the 8.33ms ProMotion deadline and the panel down-clocks [3]. Therefore every pass/fail criterion below is stated in **p99 / max**, never mean.

### 2. Render-loop architecture

#### 2.1 Vsync-driven loop, NOT a spin loop

There are three families of loop:

1. **Free-running / "as fast as possible"** - Ghostty does this by default (no vsync) [4]. Maximizes responsiveness but wastes power, can tear, and provides no inherent pacing. Viable but undesirable for a battery-sensitive macOS app.
2. **Vsync-blocked present (FIFO)** - block on the GPU present queue. Simple, but `wgpu`'s changelog notes "extremely poor frame pacing" can occur in FIFO present modes on some drivers (a Windows/DXGI issue specifically called out in wgpu 26.0.x, less relevant on Metal) [5].
3. **Display-link callback driven** - the OS calls you back once per vsync on a dedicated high-priority thread; you build and present a frame in response. This is Apple's recommended pattern and what Zed uses [3].

**Recommendation is family 3.** On macOS you register a `CADisplayLink` (modern, ProMotion-aware) whose callback fires on a high-priority thread synchronized to the panel refresh. Each callback: check the dirty flag; if dirty, snapshot grid state, build the frame, encode, and present; if clean, optionally skip (but see ProMotion caveat).

#### 2.2 CADisplayLink vs CVDisplayLink vs CAMetalDisplayLink (macOS specifics)

This is the single most load-bearing macOS decision and the evidence is direct:

- Zed initially used `CADisplayLink`. They tried replacing it with **`CVDisplayLink`** following Apple's docs precisely and saw **frame times oscillate between 8ms and 16ms** - unstable, never holding 120fps [3]. They reverted.
- **`CADisplayLink`** is ProMotion-aware and was historically iOS-only but is available on macOS 14+. It is the recommended path. Apple's forums also point to **`CAMetalDisplayLink`** as the newest, Metal-integrated frame-pacing primitive [1].
- **ProMotion down-clocking:** "If you consistently present a drawable on every frame, the display will continue to run at a constant refresh rate, but as soon as you neglect to draw a frame, its refresh rate drops" [1]. Consequence: to *hold* 120Hz you cannot skip presents during interaction. Zed's mitigation: **render repeated frames for ~1 second after the last input event**, then go idle [3]. aterm must do the same - a "keep-warm" window after any input or PTY activity.

#### 2.3 Present-path details (Metal)

From Zed's investigation and Apple forums:

- **`maximumDrawableCount = 2` or 3 (triple buffering).** With 2 drawables a frame is "pretty much guaranteed to display on the next vsync if rendering completes quickly enough" [1]. Triple buffering (3) decouples CPU frame N+1 build from GPU frame N read; Ghostty's Metal backend uses **3 swap-chain buffers** [4]. Use a pool of per-frame instance/uniform buffers (one per in-flight frame) to avoid the CPU writing memory the GPU is still reading [3].
- **`presentsWithTransaction`:** Zed found enabling it for steady-state rendering hurt; they **disabled it for steady state** and only re-enabled during **startup and window resize** to avoid visual artifacts [3]. Resize is the one scenario where you want present synchronized with the CoreAnimation transaction so the layer and window size change atomically.
- **`waitUntilScheduled` vs `waitUntilCompleted`:** Zed switched from `waitUntilCompleted` to `waitUntilScheduled` to avoid an over-long main-thread block, though they later evolved this further [3]. The principle: don't block the main thread waiting for GPU *completion*; let the completion handler recycle the frame's buffers asynchronously.

#### 2.4 Thread architecture

Converged design across Ghostty [4][6], Alacritty, and adaptable to aterm:

- **Render thread** - owns the GPU device/queue, runs off the `CADisplayLink` callback. Takes an immutable *snapshot* of renderable state (`RenderState` in Ghostty: grid contents + dirty regions) so it never contends with the I/O thread on the live grid [6].
- **I/O / PTY thread** - reads from the PTY fd, feeds bytes into the VT parser, mutates the grid, sets dirty flags. Continuous; not paced to vsync.
- **Main/app thread** - winit event loop: window + input events. On macOS some platforms force "must draw from app thread"; Ghostty sets that `false` for Metal so the render thread can call `drawFrame()` directly, but `true` for GTK [6]. aterm is macOS-first, so render-from-render-thread is available.

Communication is via **mailboxes / channels**, not shared mutable state: main->render (`resize`, `focus`, `config change`, `font change`), render->main (`redraw`) [6]. Cross-thread messages are small and bounded.

#### 2.5 Decoupling input

- macOS coalesces mouse-move/drag events by default [11]; keep that (we don't need every intermediate mouse position).
- Event monitor handlers and the winit event loop run on the main thread [11]. Input should mutate a small, lock-light input/edit state; the render thread reads a snapshot. Never do parsing or layout *inside* the keyDown handler - just record the intent and request a redraw.
- Target **keystroke -> visible glyph** latency, measured separately from frame rate (see benchmark Tier 2). Ghostty's maintainer candidly admits input latency "has never been reliably measured or optimized" and one measurement showed 24ms avg / 41ms max vs 7ms for minimalist zutty [4] - a cautionary data point: GPU acceleration alone does not guarantee low input latency.

### 3. Damage tracking / dirty regions

This is what makes a terminal cheap to render: in normal use only a handful of cells change per frame (the cursor, the line being typed). Redrawing the whole grid every frame is wasteful and, on large windows, can blow the budget.

- Ghostty tracks dirty regions via a `dirty` field on its `RenderState` snapshot and "avoids redrawing unchanged regions" [6]. Damage is computed per-cell / per-row during grid mutation on the I/O thread.
- The renderer distinguishes an **Update phase** (rebuild renderable content from changed terminal state) from a **Draw phase** (submit GPU commands) [6]. If nothing is dirty and we are outside the post-input keep-warm window, the frame can be skipped entirely.
- **Caveat for GPU renderers:** with a persistent framebuffer you can in principle re-draw only damaged regions, but most GPU terminals rebuild the full instance buffer each frame because per-cell instanced rendering of a full grid is already cheap (tens of thousands of quads is trivial for a Metal-class GPU). Damage tracking's biggest payoff is therefore **deciding whether to draw at all** and **bounding CPU-side frame-build work**, more than partial GPU redraw. Confirm empirically (Risk).

### 4. Avoiding per-frame allocation

- Pre-allocate and reuse: the instance/vertex buffer staging `Vec`, the uniform struct, the per-frame GPU buffers (one set per in-flight frame, recycled in the completion handler) [3].
- The grid itself is a flat `Vec<Cell>` with a fixed `Cell` struct (char/glyph id + fg/bg + flags). No per-cell heap. Scrollback is a ring buffer.
- The VT parser (`vte` crate, see 5) is a table-driven state machine that does not allocate per byte.
- Glyph atlas: rasterize each glyph once into a GPU texture atlas and cache by (glyph, size, weight) key; the hot path only looks up atlas coordinates. Ghostty's font system does exactly this glyph caching + atlas management [6].
- Lint this in CI: a debug-build allocation counter (e.g. a custom `GlobalAlloc` wrapper) asserting **0 allocations during a steady-state frame** in a test build.

### 5. PTY backpressure under output floods

The defining stress: `cat 500MB.log`, `yes`, a build emitting megabytes of output. The failure mode of a naive design is redraw-per-read saturating the main thread.

Correct design (Alacritty/Ghostty):

- The I/O thread reads PTY in large chunks into a reusable buffer and feeds the **`vte`** parser, mutating the grid as fast as it can. **Parsing is decoupled from rendering**: the grid may be updated hundreds of times between two vsyncs.
- The renderer samples grid state **at most once per vsync**. Intermediate states are simply never drawn - the user sees a coherent stream at 60/120fps, not every transient line.
- **Backpressure:** if the application produces output faster than we can parse, the PTY's OS-level buffer fills and `write()` in the child blocks - natural flow control, no unbounded memory growth. We do not need an application-level unbounded queue; we read greedily but the grid is bounded (viewport + capped scrollback ring).
- vtebench explicitly measures *this* - "the speed at which a terminal reads from the PTY" - but its README warns it "lacks support for critical factors like frame rate or latency" [12], so it is necessary but not sufficient; pair it with frame-time measurement.
- Ghostty's maintainer notes a real-world optimization target: low numbers of **unique styles** (~64 max) and plaintext scrolling, and that synthetic worst-cases (every cell a unique style) are deliberately not the optimization target [4]. aterm should benchmark both real-world and adversarial, but gate primarily on real-world.

### 6. The VT parser and grid

- **`vte` 0.13.0** (released 2025-11-17) - Alacritty's table-driven escape-sequence parser, the de-facto Rust choice; recent releases added DECRPM/DECRQM mode reporting [9]. This is the parser to build the grid mutation layer on.
- Terminal speed is "two things mainly: its VT parser, and its rendering engine" [9] - we are gating both, separately, in Tier 1 and Tier 2.

### 7. The GPU stack: wgpu vs direct Metal

- **`wgpu` 29.x** is the current release line (26.0.0 released 2025-07-10; 29.0.x current as of early 2026) [2][5]. Cross-platform (Metal/Vulkan/D3D12/GL/WebGPU), pure-Rust, safe. Directly supports the "Linux/Windows not precluded later" constraint.
- **Overhead:** wgpu's own maintainers estimate **5-10% overhead** from validation, state tracking, and lifetime tracking in a real app [5]. For a terminal (low draw-call count) this is almost certainly within budget, but it is non-zero and sits on the critical path.
- **`metal` crate** - unsafe direct Metal bindings; what Ghostty and Zed effectively use. Lowest overhead, macOS-only, more code.
- Rio's "Sugarloaf" renderer is built on wgpu/WebGPU and ships a real GPU terminal, proving wgpu is *viable* for this workload [1].

**Call:** start on `wgpu` 29.x for portability and developer velocity; keep the renderer behind a trait (like Ghostty's `GenericRenderer(GraphicsAPI)` [6]) so a `metal`-crate backend can replace it on the hot path if Tier-2 benchmarks show wgpu eating too much of the 8.33ms budget. Validate with a spike before committing.

### 8. Profiling tooling

- **Tracy** (`wolfpld/tracy`) via the **`tracing-tracy`** crate (or the `profiling` abstraction crate over puffin/tracy/optick) - nanosecond-precision frame profiler with a GUI; supports CPU zones, GPU zones (including **Metal** and WebGPU), locks, and memory allocations; remote profiling [7]. The convention: place the main frame marker after the present call, with sub-zones for input/parse/build/encode [7]. This is the primary live dev tool.
- **`puffin`** - lighter, Rust-native, egui-based flamegraph; good for an in-app overlay.
- **Xcode Instruments - Metal System Trace / GPU frame capture** - ground truth for GPU-side frame time, drawable acquisition stalls, and present timing. Use this to validate the `CADisplayLink`/present path that pure-Rust tools can't see into.
- **`cargo-flamegraph`** - sampling profiler for CPU hot spots (parser, grid).
- **`iai-callgrind`** - instruction-count benchmarking (see harness below).

### 9. CI-gated benchmark harness design

The owner wants "a solid benchmark of 60fps always at a minimum" as a first-class, CI-gated artifact. Two tiers, because frame-time benchmarks are too noisy on shared CI runners and instruction-count benchmarks can't measure the GPU/present path.

#### Tier 1 - Micro (deterministic, every PR, on GitHub Actions)

- Tool: **`iai-callgrind`** (Valgrind/Callgrind instruction counts). "Each benchmark is only run once... can take accurate measurements even in virtualized CI environments... completely negating the noise of the environment" [8]. This is the only credible way to gate performance on shared runners.
- Benchmarks: (a) `vte` parse of fixed payloads (plaintext scroll, dense unique-style cells, unicode, SGR-heavy, alt-screen TUI redraw); (b) grid mutation + scroll; (c) damage computation; (d) frame-build CPU work (instance buffer generation for a fixed grid state) - the GPU-free portion.
- Gate: **fail the PR if instruction count regresses > N%** (start at 5%; tighten later). Track over time with **Bencher** (`bencherdev/bencher`), which "fails the PR when there's a performance regression" and stores history [8].

#### Tier 2 - Frame-level (real macOS hardware, nightly + pre-release)

This is the actual "60fps always" proof. It needs real Metal + real display link, so it runs on a dedicated/self-hosted Apple Silicon runner (or developer-triggered), not shared CI.

- **In-process frame recorder:** a ring buffer capturing, per frame: `cpu_frame_ms` (snapshot->encode), `gpu_frame_ms` (from Metal GPU timestamps / `MTLCommandBuffer` GPU start-end), `present_interval_ms` (delta between successive `CADisplayLink` timestamps), `dirty_cells`, `allocations` (debug build), and a `frame_dropped` flag (present_interval exceeded the deadline + tolerance).
- **Driver:** a scripted scenario runner that feeds deterministic PTY input and synthetic input events, then dumps the recorder to JSON for offline analysis (percentiles, histograms).
- **Input latency** measured separately: software path via a Typometer-style screen-capture-on-keypress measure for CI, with an optional hardware (light-sensor / Teensy) rig for ground-truth keyboard-to-photon as in the GNOME 46 methodology (120 iterations, report median + p25/p75 + outliers) [13][14].

#### Tier 2 stress scenarios (each a named, scripted benchmark)

| Scenario | What it stresses | Driver |
|---|---|---|
| `fast_scroll` | scroll throughput + damage | hold page-down through 100k-line buffer |
| `output_flood` | PTY backpressure, render decoupling | `cat` a 200MB file; `yes` for 10s |
| `large_scrollback` | memory + scroll into deep history | 1M-line ring, jump to top |
| `agent_stream_while_typing` | concurrent agent token stream + live input edit | inject streamed tokens while replaying keystrokes |
| `window_resize` | reflow + `presentsWithTransaction` path | animate window 800->1600px over 1s |
| `fullscreen_tui_redraw` | full-grid invalidation | run a TUI (vim/htop sim) doing full repaints |
| `idle` | down-clock + zero CPU when nothing changes | sit idle 5s; assert ~0 frames drawn |

#### Pass/fail criteria (the gate)

Steady-state, per scenario, on Apple Silicon ProMotion:

- **p50 frame time:** <= 8.33ms (120Hz) / <= 16.67ms (60Hz).
- **p99 frame time:** <= 8.0ms (120Hz target) / **hard floor: <= 16.0ms (60Hz, never exceed)**.
- **max frame time:** <= 16.0ms in any non-resize steady-state scenario (resize allowed a one-frame transaction spike).
- **Dropped frames:** **0** in steady state per scenario; flood/resize allowed a bounded, declared budget (e.g. <= 2 frames during a resize animation), regression-gated.
- **Input latency (keystroke->glyph):** median <= 1.5 frames, p99 <= 3 frames at the active refresh rate.
- **Allocations per steady-state frame:** **0** (debug-instrumented build).
- **Idle:** CPU ~0, frames drawn ~0 after the keep-warm window expires.

Any breach fails the nightly and blocks release. Tier 1 blocks every PR.

## Recommendations for aterm

1. **Drive rendering from `CADisplayLink` (Metal pattern), never `CVDisplayLink`.** *Rationale: direct, documented evidence that CVDisplayLink oscillates 8-16ms and fails to hold 120fps [3].* **Confidence: High.**
2. **Three-thread model: render (display-link), I/O/PTY+parse, main/input - communicating via bounded mailboxes; renderer works off an immutable grid snapshot with dirty regions.** *Rationale: the convergent design of every fast native terminal [4][6].* **Confidence: High.**
3. **Decouple PTY read from render; sample grid at most once per vsync; rely on OS PTY buffer for backpressure.** *Rationale: only correct way to survive `cat`/`yes` floods at 60fps [4][6].* **Confidence: High.**
4. **Implement a post-input/post-activity "keep-warm" window (~1s) presenting every vsync, then idle to zero frames.** *Rationale: prevents ProMotion down-clock mid-interaction; matches Zed [1][3].* **Confidence: High.**
5. **Build on `vte` 0.13.0 for parsing; flat `Vec<Cell>` grid + scrollback ring; glyph atlas keyed by (glyph,size,weight); zero per-frame allocation enforced by a CI allocation assertion.** *Rationale: standard, proven, allocation-free hot path [9][6].* **Confidence: High.**
6. **Start the GPU backend on `wgpu` 29.x behind a renderer trait; keep a `metal`-crate backend as a fallback escape hatch if Tier-2 shows wgpu's 5-10% overhead threatens the 8.33ms budget.** *Rationale: portability + velocity now, no architectural lock-in later [2][5][6].* **Confidence: Med (the fallback may prove necessary).**
7. **Two-tier benchmark harness: `iai-callgrind` (instruction counts) gating every PR via Bencher; an in-process frame recorder running the 7 scripted stress scenarios nightly on a real Apple Silicon ProMotion runner with the p50/p99/max/dropped-frame/latency gates above.** *Rationale: the only honest way to make "60fps always" a CI artifact - micro on noisy runners, frame-level on real hardware [8][12][13].* **Confidence: High.**
8. **Profile with Tracy (`tracing-tracy`) live, validate the present path in Xcode Metal System Trace, sample CPU with `cargo-flamegraph`.** *Rationale: pure-Rust tools can't see drawable/present stalls; need GPU-side ground truth [7].* **Confidence: High.**
9. **Gate on real-world workloads (plaintext scroll, low unique-style counts) primarily; track adversarial (every-cell-unique-style) as a secondary, non-blocking signal.** *Rationale: Ghostty's explicit, hard-won stance - optimizing for synthetic worst-cases distorts the design [4].* **Confidence: Med.**

## Risks & unknowns

- **winit macOS redraw timing is historically awkward.** winit has had long-standing issues drawing inside `drawRect:` / `kCFRunLoopBeforeWaiting` and `request_redraw()` having platform-dependent effects [10]. Bridging our own `CADisplayLink` into winit 0.30's `ApplicationHandler` may require unsafe AppKit interop or bypassing winit's redraw scheduling. **Spike this before committing to winit for the render trigger.**
- **wgpu's 5-10% overhead [5] is an estimate for "a real app," not a terminal measurement.** It could be negligible (low draw-call workload) or could matter at 8.33ms. Unverified for our case - the renderer-trait + fallback mitigates, but adds cost.
- **`CAMetalDisplayLink` vs `CADisplayLink`:** Apple forums recommend the former as newest [1] but Zed's proven path is `CADisplayLink` [3]. We have not verified `CAMetalDisplayLink` behavior under ProMotion ourselves. Treat `CADisplayLink` as the safe default.
- **Damage-driven partial GPU redraw vs full-instance-rebuild:** I asserted full rebuild is "cheap enough" but did not find a hard cell-count/GPU-time number for our target hardware. Verify with a microbench early.
- **Input latency is not guaranteed by GPU rendering.** Ghostty's 24ms/41ms data point [4] shows a fast renderer can still feel laggy. Latency must be its own gated metric, not assumed.
- **Self-hosted Apple Silicon CI runner** is an infra dependency for Tier 2; shared macOS GitHub runners are not ProMotion and are too noisy for frame-time gating. Cost/ownership unresolved.
- **Exact `wgpu` Metal GPU-timestamp support** for the frame recorder needs confirmation; may require dropping to the `metal` crate or Instruments for `gpu_frame_ms`.

## Open questions for the product owner

1. **Render-stack commitment:** Approve a 1-2 week spike to validate `winit` 0.30 + self-bridged `CADisplayLink` + `wgpu` present path against the frame-time gate *before* the architecture is locked? (Strongly recommended.)
2. **CI infrastructure:** Are we willing to own a dedicated/self-hosted Apple Silicon ProMotion runner for nightly Tier-2 frame benchmarks? Without it, "60fps always" cannot be CI-proven, only spot-checked.
3. **Hardware latency rig:** Do we want a light-sensor/Teensy keyboard-to-photon rig [13][14] for ground-truth input latency, or is software (Typometer-style) measurement acceptable for v1?
4. **Floor vs target enforcement:** Is the *hard* CI gate the 60fps floor (16ms) with 120fps as an aspirational/tracked-but-non-blocking target, or do we block release on the 120fps target too? (Recommendation: hard-gate 60fps floor, track 120fps.)
5. **Adversarial benchmark policy:** Accept Ghostty's stance (optimize real-world, don't chase synthetic worst-cases [4]), or require passing adversarial every-cell-unique-style scenarios too?

## Sources

1. Apple Developer Forums - Metal/CAMetalLayer frame pacing, ProMotion drawable presentation, `maximumDrawableCount`, `presentsWithTransaction`: https://developer.apple.com/forums/thread/763426 and https://developer.apple.com/forums/thread/711033
2. wgpu crate (crates.io): https://crates.io/crates/wgpu
3. Zed Blog - "Optimizing the Metal pipeline to maintain 120 FPS in GPUI": https://zed.dev/blog/120fps
4. Ghostty - "Let's talk about performance" (Discussion #4837): https://github.com/ghostty-org/ghostty/discussions/4837
5. wgpu releases / CHANGELOG (overhead estimate, FIFO frame pacing, version history): https://github.com/gfx-rs/wgpu/releases and https://github.com/gfx-rs/wgpu/blob/trunk/CHANGELOG.md
6. Ghostty Rendering System (DeepWiki) - thread model, `RenderState`, dirty regions, `GenericRenderer`, swap-chain buffers: https://deepwiki.com/ghostty-org/ghostty/5-rendering-system
7. Tracy frame profiler (CPU/GPU incl. Metal/WebGPU zones) and the `profiling` abstraction crate: https://github.com/wolfpld/tracy and https://crates.io/crates/profiling
8. Bencher - Iai-Callgrind in CI, instruction-count gating, PR regression failure: https://bencher.dev/learn/benchmarking/rust/iai/ and https://github.com/bencherdev/bencher and https://github.com/iai-callgrind/iai-callgrind
9. alacritty/vte parser (v0.13.0, 2025-11-17) and release history: https://github.com/alacritty/vte/releases
10. winit macOS redraw timing issues (RedrawRequested/`drawRect:`, request_redraw platform behavior): https://github.com/rust-windowing/winit/issues/2640 and https://github.com/rust-windowing/winit/issues/2954
11. Apple - Cocoa event handling, mouse-event coalescing, main-thread event dispatch: https://developer.apple.com/documentation/appkit/nsevent and https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/EventOverview/HandlingMouseEvents/HandlingMouseEvents.html
12. alacritty/vtebench - PTY read benchmark + its own "lacks frame rate or latency" disclaimer: https://github.com/alacritty/vtebench
13. Ivan Molodetskikh - "Just How Much Faster Are the GNOME 46 Terminals?" (hardware light-sensor latency methodology, vtebench): https://bxt.rs/blog/just-how-much-faster-are-the-gnome-46-terminals/
14. Tristan Hume - "Measuring keyboard-to-photon latency with a light sensor": https://thume.ca/2020/05/20/making-a-latency-tester/
