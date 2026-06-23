# ADR-0002: Render stack - custom wgpu + parley behind the aterm-ui seam

## Status

Accepted

## Context

aterm's UI is structurally Zed's problem minus the code intelligence: one scrolling,
retained timeline interleaving a constant-advance monospace terminal grid with proportional
prose, cards, and status chips, plus a single live-editable input box with IME. The 60fps
floor (120 on ProMotion) is the gating non-functional requirement. The two serious,
shipping answers are both Rust: Zed's **GPUI** framework, and Warp's **custom Metal
renderer** over rect/image/glyph primitives.

The render-stack research doc ([02-render-stack-eval.md](../research/02-render-stack-eval.md))
originally led with GPUI on a weighted matrix (GPUI ~62 vs custom wgpu+parley ~57 of 80) -
a 4-point gap that turns almost entirely on time-to-first-pixel (GPUI wins) vs accessibility
(custom wins). Both score 5/5 on raw perf headroom (Zed ships 120fps; Warp ships
>144fps / ~1.9ms redraw - both vendor figures). The
[overview](../research/00-overview.md) reversed that lead after weighing three factors the
matrix under-weighted: licensing permanence, strategic independence, and the accessibility
gap. The licensing detail is decisive and is corroborated by
[12-licensing.md](../research/12-licensing.md).

## Decision

**Build aterm's UI/render layer as a custom `wgpu` + `parley`/`cosmic-text` + `swash`
renderer (the "Warp path"), behind a thin internal `aterm-ui` seam, driven by a
self-bridged `CADisplayLink` loop. Do NOT adopt GPUI as the UI framework.** This is
committed - there is NO spike gate; we build directly on wgpu.

The `aterm-ui` crate is a seam: its public API is a set of aterm-owned traits ("render a
block", "render the input", "render a transcript card"). The custom wgpu renderer is the
only implementation in v1; GPUI remains a theoretical fallback behind the seam, never used.

Rationale (why custom wgpu over GPUI):

1. **Licensing is the decisive fact.** GPUI declares Apache-2.0 but a default release build
   statically links GPL-3.0-or-later object code through
   `gpui -> sum_tree -> ztracing -> zlog/ztracing_macro` (Zed issue #55470, open, no fix -
   verified). For a GPLv3 app this is not a violation, but it permanently forecloses any
   future relicense/dual-license/permissive-fork and makes aterm track someone else's
   accidental copyleft on every bump. The custom stack (wgpu, winit, parley/cosmic-text,
   swash - all MIT and/or Apache-2.0) is unambiguously clean.
2. **Accessibility.** GPUI has no working VoiceOver/screen-reader support for the editing
   surface, and AccessKit-into-GPUI is an open effort "far beyond Zed 1.0". `parley` already
   integrates AccessKit text properties. The custom path ships a11y; GPUI defers it
   indefinitely.
3. **Strategic independence and product fit.** aterm's wedge post-Warp-open-source is
   feel + openness + agent integration ([11-competitive-landscape.md](../research/11-competitive-landscape.md)).
   Owning the renderer - the terminal grid fast-path, the block/timeline layout, the input
   box - *is* the product, and it removes a dependence on a fast-moving, pre-1.0 framework
   officially under-supported outside Zed and built by an adjacent competitor.
4. **The perf floor is an architectural property we own anyway.** Per
   [09-performance-60fps.md](../research/09-performance-60fps.md), 60fps is won by
   vsync-driven rendering, damage tracking, PTY/render decoupling, and zero per-frame
   allocation - all of which we implement ourselves regardless of framework. Zed's
   macOS frame-pacing lessons are *documented* (corrected: Zed kept CADisplayLink and fixed
   pacing by presenting drawables early + a ~1s keep-warm window, rather than reverting to
   CADisplayLink as one source claimed) and reproducible. We adopt `CADisplayLink` as the
   default loop driver and inherit present-early + keep-warm by reading the writeup, not by
   importing the framework.

The stack: `winit 0.30` (windowing/IME/events) + `wgpu 29.x` (GPU, behind a renderer trait
that allows a `metal`-crate hot-path fallback) + `parley`/`cosmic-text 0.19` + `swash`
(+ `glyphon`) for text. See [ADR-0009](0009-text-and-glyph-pipeline.md).

## Consequences

- We rebuild the entire retained UI layer ourselves: layout, hit-testing, focus, event
  routing, the input box, selection, IME plumbing, and the block/transcript widgets. This
  lengthens time-to-first-pixel by weeks-to-months versus adopting GPUI - the single
  strongest counter-argument, and the reason the dossier rates this medium-high confidence,
  not high.
- Mitigation for that cost: lean on `parley`'s `PlainEditor` + IME + AccessKit and
  `glyphon`'s atlas rather than building text from zero; ship a vertical slice early.
- The renderer is fully owned and openly (GPLv3-cleanly) licensed; no tracking of a
  competitor's accidental copyleft; a11y is achievable.
- The `aterm-ui` seam keeps the GPUI option reversible in principle (it is a contained
  refactor, not a rewrite), satisfying the "renderer stays swappable" requirement.
- The 60fps floor is not de-risked by a framework freebie; it is de-risked by the
  `aterm-bench` standing proof ([09-performance-60fps.md](../research/09-performance-60fps.md))
  and the architecture we own.

## Alternatives considered

- **GPUI (Zed's framework).** The only Rust stack that has shipped aterm's exact hard
  problem at 120fps. Rejected for the three liabilities above (GPL-3.0 contamination via
  sum_tree->ztracing, zero accessibility, pre-1.0 competitor dependence) which collectively
  outweigh its real "months of saved build time" advantage for a project whose identity is a
  custom, owned, minimal, openly-licensed terminal. Kept as a theoretical fallback behind the
  seam only.
- **egui / iced.** Disqualified: broken IME (egui blocks preedit/steals Tab; iced IME "won't
  activate") and absent/blind accessibility make them unfit for a CJK-capable input box and
  rich transcript ([02-render-stack-eval.md](../research/02-render-stack-eval.md)).
- **Vello / Xilem.** Rejected: alpha, no blur/filter yet; do not bet the 60fps floor on it.
- **A render spike gate before committing.** Explicitly rejected by the locked decision: we
  are committed to wgpu and build directly on it. The `aterm-bench` harness is the standing
  proof rather than a one-time gate.
