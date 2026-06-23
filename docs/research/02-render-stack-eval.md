---
title: Rust GPU UI Render-Stack Evaluation (GATING DECISION)
domain: render-stack-eval
status: research
---

# Rust GPU UI Render-Stack Evaluation (GATING DECISION)

## TL;DR

- **Recommended stack: GPUI (Zed's framework) as the UI layer, on its native Metal backend, with `parley` (or GPUI's built-in CoreText path) for text, and `winit` only if we ever need to abandon GPUI's windowing.** GPUI is the only Rust UI framework that has *already shipped* a 120fps-class, GPU-accelerated, block/scene-graph UI mixing a monospace editor grid with proportional prose - i.e. it has solved exactly aterm's hardest problem in production (Zed) [1][2][3]. It is now an Apache-2.0 crate on crates.io (`gpui 0.2.2`, published 2025-10-22) [4], which is GPLv3-compatible.
- **The headline risk is accessibility.** As of 2025-2026 neither GPUI nor Zed has working VoiceOver/screen-reader support for the editing surface; AccessKit-into-GPUI is an open, "far beyond 1.0" effort [5][6]. If aterm must ship a11y on day one, GPUI is disqualified and the call flips to `wgpu` + `parley` (parley already has full AccessKit text properties [7]). Otherwise a11y is a known, deferrable gap.
- **Warp's own path (custom `wgpu`/Metal renderer + platform text shaping) is proven for this exact product** - Warp sustains >144fps with a 1.9ms average redraw using only rect/image/glyph primitives in ~200 lines of shader [8][9]. It gives total control but is the highest-effort option (you rebuild layout, input, IME, hit-testing, selection from scratch). Choose it only if GPUI's API instability or coupling becomes intolerable.
- **egui and iced are not recommended as the primary surface.** Both have real IME and accessibility gaps documented as of April 2025 (iced: IME "won't activate", Narrator can't see the window; egui: IME steals Tab, blocks provisional/preedit states) [10] - both are showstoppers for a CJK-capable terminal input box and the rich agentic transcript.
- **Vello is not ready as the load-bearing renderer** (alpha, no blur/filter yet) but `vello_cpu`/`vello_hybrid` are worth watching; do not bet the 60fps floor on it now [11][12]. Skia bindings (`skia-safe`) and `femtovg` are viable 2D backends but bring C++/maintenance or feature-coverage costs and no UI layer - they don't change the GPUI-vs-custom decision.
- **Net call for aterm: build on GPUI now, behind a thin internal UI-abstraction seam so a later swap to a custom `wgpu`+`parley` renderer is possible if GPUI's pre-1.0 churn or a11y blocker forces it.** This maximizes time-to-first-pixel and hits the 60fps floor with near-certainty while keeping the escape hatch open.

## Findings

### The problem shape, restated against the render stack

aterm's UI is, structurally, *Zed's problem minus the code intelligence*: one scrolling, retained timeline that interleaves (a) a constant-advance monospace terminal grid (PTY output, OSC-133 command blocks) with (b) proportional prose, cards, and status chips (the agentic transcript), plus (c) a single live-editable input box that must support IME, selection, and a target-routing indicator. The 60fps floor (120 on ProMotion) is the gating non-functional requirement. The two existence proofs in the wild - Zed (GPUI) and Warp (custom Metal) - are the two serious answers, and both are Rust. egui/iced/vello are the "could we avoid building the hard part" candidates, and the evidence says they each fail on at least one of {IME, accessibility, maturity} for this product.

### (1) GPUI (Zed's framework)

**Architecture.** GPUI is "a hybrid immediate and retained mode, GPU accelerated, UI framework for Rust" with three layers: an entity-based state system, a high-level declarative `Render` trait (views), and a low-level imperative element API [1]. Rendering is organized as a platform-neutral `Scene` of layered primitives - in Zed's own description, a `Layer { shadows, rectangles, glyphs, icons, image }` drawn in painter's-algorithm order [2]. This scene-graph + instanced-primitive model is *exactly* what a block timeline wants: each command block, transcript card, and grid row is a composition of rects + glyphs + images, not a general vector scene.

**How it hits 120fps (directly relevant to our floor).** Zed published two primary-source deep dives:
- Rectangles are rendered via signed-distance-field shaders on the GPU; drop shadows use Evan Wallace's closed-form Gaussian (`erf`) technique; glyphs are rasterized once to a GPU texture atlas with **up to 16 sub-pixel variants per glyph**, storing only the alpha channel and applying color via shader multiply [2]. CPU does shaping (CoreText on macOS), rasterization, atlas bin-packing, layout; GPU does all primitive draws via instanced calls. The 8.33ms (120fps) frame budget drove the design [2].
- The "Optimizing the Metal pipeline" post documents the *delivery* problem and fixes: render times were consistently <4ms, but frames weren't *presented* on time. Fixes: switch `wait_until_completed()` -> `wait_until_scheduled()`; triple-buffer instance buffers released via Metal completion handlers; render repeated frames for ~1s after the last input via `CADisplayLink` (`on_request_frame`) to defeat ProMotion downclocking; disable `presentsWithTransaction` for steady-state and use `present_drawable` before `commit()` [3]. **These are the exact macOS-specific lessons aterm would otherwise have to rediscover from scratch.** Using GPUI means inheriting them for free.

**Text + IME + selection.** Glyph path is mature and shipping (CoreText shaping, sub-pixel atlas) [2]. GPUI carries IME and text-input plumbing because Zed's editor needs it; the April-2025 survey explicitly notes GPUI "IME works fine" (in contrast to iced/egui) [10]. Selection in a real code editor is proven by Zed itself.

**Accessibility - the load-bearing weakness.** As of 2025-2026: Zed menus are accessible but there is *no* accessibility for editing functions under macOS VoiceOver, and Windows screen-reader support is effectively zero [6]. The community/maintainer position: integrate AccessKit into GPUI (referencing egui's AccessKit integration), but "a11y in Zed will be a long project, likely lasting far beyond 1.0" [5][6]. For aterm this means: shipping GPUI = shipping with no screen-reader support for the timeline/input initially.

**Standalone usability outside Zed.** This is the second real risk. Maintainers have said they "don't have the resources to extract and maintain GPUI as a standalone library outside of Zed's purposes" [13]. The April-2025 survey was blunt: docs "spotty", install "janky", no basic text-input widget out of the box, ~700-line example to get going, and "not sure you're actually supposed to use GPUI at this stage" [10]. **However**, two things materially de-risk this since that survey: (a) GPUI is now a real crate, `gpui 0.2.2`, Apache-2.0, published 2025-10-22 [4]; (b) Longbridge's `gpui-component` provides 60+ production components (virtualized Table/List, a Tree-sitter+Rope code editor, dock layout, theming, markdown/HTML rendering) and ships in Longbridge Pro - a real third-party desktop app built standalone on GPUI [14]. So standalone GPUI is now *demonstrably* viable, just rough.

**API stability.** Pre-1.0, "breaking changes often occur between versions" [1]. This is the cost we pay for the head start.

**License.** `gpui` crate is **Apache-2.0** [4]. Apache-2.0 is one-way compatible *into* GPLv3, so a GPLv3 aterm can depend on GPUI cleanly [15] (note: not the reverse, irrelevant here). No conflict with our GPLv3 decision.

**Cross-platform.** GPUI runs macOS (Metal), Linux (Vulkan via Blade; Wayland/X11 windowing), Windows (DX12; Win32 + DirectWrite text) [1][16]. Matches our "macOS-first, others not precluded" requirement.

### (2) wgpu + custom retained UI layer + text stack (parley / cosmic-text / glyphon / swash) - "Warp's path"

**What Warp actually did.** Warp built a custom Metal renderer over three primitives - rectangles, images, glyphs (texture atlas) - with "shaders for each of these primitives in around 200 lines of code", explicitly so that porting to another GPU API is "<250 lines of shader code" while elements above stay untouched [9][8]. Result: ">144 FPS" with "average time to redraw the screen ... only 1.9 ms" [8]. Text: platform shaping (CoreText/DirectWrite/Pango+FreeType) into a glyph atlas keyed by (font id, glyph id, size), with a clever 3-subpixel-offset scheme (round x to 0.0/0.33/0.66) to preserve kerning with only 3 variants per glyph - landed in "<200 lines, mostly Rust, with a couple minor edits to Metal shader code" [17]. Warp also wrote a candid post on *why* this is hard in Rust (no inheritance, mutable-tree ownership pain; they used an ECS-style `HashMap<EntityId, Box<dyn AnyView>>`) [18].

**The build for aterm.** `wgpu 29.0.3` (MIT/Apache-2.0) [19] gives the cross-platform GPU abstraction (Metal/Vulkan/DX12/GL/WebGPU). `winit 0.30.13` (Apache-2.0) [20] gives windowing + the event/IME loop. For text you have a strong menu:
- `parley 0.10.0` (Apache-2.0/MIT, 2025-06-01) [21][7]: rich text *layout* + `PlainEditor` with selection, IME hooks (added for Android IME), HarfRust shaping, and - critically - "all possible AccessKit text properties" + AccessKit 0.24 integration [7]. This is the most production-credible high-level text choice and is the Linebender/Xilem stack's text engine.
- `cosmic-text 0.19.0` (MIT/Apache-2.0) [22]: multi-line shaping/layout/editing in safe Rust, bidi, swash rasterization, ligatures, color emoji; used by COSMIC desktop.
- `glyphon 0.11.0` (MIT/Apache/Zlib) [23]: thin wgpu text renderer = cosmic-text + etagere atlas. Fastest path to "text on a wgpu surface" but it's a renderer, not an editor.
- `swash 0.2.9` (Apache/MIT) [24]: low-level shaping+rasterization primitive under cosmic-text.

**Trade-off.** Total control, no pre-1.0 framework churn, proven to exceed our fps floor (Warp). But you build the entire retained UI layer - layout, hit-testing, focus, event routing, the input box, selection model, the block/transcript widgets - yourself. Time-to-first-pixel is weeks-to-months longer than GPUI. This is the right answer only if GPUI's coupling/instability/a11y becomes a hard blocker.

### (3) egui (immediate mode)

`egui 0.34.3` (MIT/Apache) [25]. Immediate-mode: simple, fast to start, good for tools/overlays. AccessKit support exists (Windows + macOS) and is on by default in eframe [26]. But for aterm's surface the April-2025 hands-on found: IME "steals Tab", and provisional/preedit composition states are blocked - i.e. CJK input is broken in practice [10]; rich text (bold/underline/mixed fonts/proportional+mono in one flow) is not a first-class feature, only plain `TextEdit` [26][27]. Immediate-mode also means re-running the whole UI per frame, which is fine for fps but awkward for a long retained transcript with virtualization. **Verdict: not the primary surface; acceptable only for incidental debug overlays.**

### (4) iced (retained, Elm-like)

`iced 0.14.0` (MIT, 2025-12-07) [28]. Clean Elm architecture, retained, good for structured apps; 0.14 improved input handling and rendering [29]. But the April-2025 survey found IME "won't activate" and "Windows Narrator can't see into this window" [10] - both fatal for a terminal that must accept CJK input and (eventually) be accessible. iced's text-input is extensible but there is no evidence of a battle-tested monospace-grid-plus-proportional-transcript at 120fps. **Verdict: not recommended for aterm's surface given the IME gap.**

### Render backends assessed (not UI layers)

- **Vello** `0.9.0` (Apache/MIT, 2026-05-15) [30]: GPU compute 2D renderer; powerful but **alpha**, blur/filter effects still unimplemented [11]. `vello_cpu`/`vello_hybrid` are maturing (a 30% overdraw-handling win reported Dec 2025) [11][12]. It's the Xilem rendering backend. Do not put the 60fps floor on it today; revisit in ~12 months.
- **skia-safe** (Rust bindings to Google Skia): extremely mature rasterizer, but pulls in a large C++ dependency, slower/heavier builds, and gives you *no* UI/text-layout/input layer - you'd still need everything above it. Not GPLv3-blocking (Skia is BSD-3) but high integration cost.
- **femtovg**: NanoVG-style GPU canvas, lighter than Skia, but again a canvas not a UI framework, and less coverage than vello/skia for complex text. Niche.
- **winit** `0.30.13` (Apache-2.0) [20]: the de-facto windowing/event crate; GPLv3-compatible; the right choice *if* we go the custom-renderer route. GPUI brings its own platform windowing, so we only need winit on the custom path.
- **AccessKit** `0.24.1` (MIT/Apache, 2026-06-12) [31]: the cross-platform a11y answer regardless of stack - maps to NSAccessibility (macOS), UIA (Windows), AT-SPI (Linux) [32]. `parley` already integrates it [7]; GPUI does not yet [5].

### Scored matrix (1-5, 5 = best for aterm; weights reflect the gating fps floor and product needs)

| Criterion (weight) | GPUI | wgpu+parley (custom) | egui | iced | vello/Xilem |
|---|---|---|---|---|---|
| Perf headroom / 60fps floor (x3) | 5 - Zed ships 120fps; lessons baked in [2][3] | 5 - Warp >144fps, 1.9ms [8] | 4 | 3 | 3 - alpha [11] |
| Text + IME + selection (x3) | 4 - mature glyph+IME; selection proven [2][10] | 4 - parley editor+IME+shaping [7][21] | 2 - IME preedit broken [10] | 2 - IME won't activate [10] | 3 - via parley, immature stack |
| Accessibility (x2) | 1 - no screen reader yet [5][6] | 4 - parley AccessKit text props [7] | 3 - AccessKit on [26] | 1 - Narrator blind [10] | 3 |
| Dev effort / time-to-first-pixel (x3) | 4 - framework exists; gpui-component helps [14] | 1 - build whole UI layer | 4 | 3 | 2 |
| Ecosystem + docs (x1) | 2 - "spotty/janky" but improving [10][14] | 4 - winit/wgpu/parley well-doc'd | 4 | 4 | 2 |
| License vs GPLv3 (x1) | 5 - Apache-2.0 [4][15] | 5 - all permissive [19][20][21] | 5 | 5 | 5 |
| Cross-platform path (x1) | 4 - mac/linux/win [16] | 5 - wgpu everywhere [19] | 5 | 5 | 4 |
| Agentic-UI fit (mono grid + prose + cards) (x2) | 5 - scene graph + Zed precedent [2][14] | 4 - full control, you build it | 2 - immediate mode, plain text | 3 | 3 |
| **Weighted total (max 80)** | **~62** | **~57** | **~46** | **~40** | **~43** |

Scoring is judgment, not arithmetic truth; the gap between GPUI and the custom path is small and turns almost entirely on accessibility (custom wins) vs time-to-first-pixel (GPUI wins).

## Recommendations for aterm

1. **Build on GPUI (`gpui`, Apache-2.0) as the primary UI/render layer on its native Metal backend.** Rationale: it is the only Rust framework that has *already shipped* aterm's exact hard problem (mono grid + proportional prose at 120fps) and hands us Zed's macOS frame-pacing lessons for free. **Confidence: High** (on the perf/fit axes), **Med** overall (pinned to GPUI's pre-1.0 churn).
2. **Pin `gpui` to an exact version (vendor or git-pin) and budget for periodic breaking-change migrations.** Pre-1.0 churn is real [1]. Treat GPUI upgrades as scheduled work, not incidental. **Confidence: High.**
3. **Introduce a thin internal `aterm-ui` seam** (our own traits for "render a block", "render the input", "render a transcript card") so GPUI lives behind it and a future swap to `wgpu`+`parley` is a contained refactor, not a rewrite. This is the single most important hedge against GPUI risk. **Confidence: High.**
4. **Pull in `gpui-component` selectively** for the virtualized List/Table (the transcript timeline is a virtualized list) and theming, rather than reinventing them. **Confidence: Med** (evaluate its API stability first).
5. **Use GPUI's CoreText glyph path for the terminal grid;** only introduce `parley` if/when we need richer proportional-prose layout in the transcript than GPUI gives us. Don't run two text stacks unless forced. **Confidence: Med.**
6. **Treat accessibility as a tracked, deferred milestone**, and contribute to / track the AccessKit-into-GPUI effort [5]. If a11y becomes a launch requirement, that triggers the fallback. **Confidence: High** (that it's deferrable for an early prototype; Low that GPUI gets a11y soon).
7. **Fallback stack, fully specified for the escape hatch:** `winit 0.30` + `wgpu 29` + `parley 0.10` (layout/editor/IME/AccessKit) + custom rect/glyph/image instanced renderer modeled on Warp's ~200-line shader approach [8][9]. **Confidence: High** that this works (Warp proves it); **Med** on the multi-month cost.
8. **Do not adopt egui or iced for the main surface; do not bet on vello yet.** egui/iced fail IME/a11y today [10]; vello is alpha [11]. **Confidence: High.**

## Risks & unknowns

- **GPUI accessibility is absent today** [5][6]. If aterm needs screen-reader support at launch, GPUI is disqualified - flip to the wgpu+parley fallback. This is the single decision-changing fact.
- **GPUI standalone is officially under-supported** ("no resources to maintain outside Zed") [13] and pre-1.0 [1]. Breaking changes and thin docs are guaranteed friction. `gpui-component` mitigates but is itself third-party.
- **I could not independently verify GPUI's IME quality for CJK on the latest crate** - the "IME works fine" claim is from the April-2025 survey [10] and Zed's editor behavior, not a fresh hands-on test on `gpui 0.2.2`. Needs a spike. Likewise I did not benchmark GPUI's standalone fps on aterm's specific workload - the 120fps numbers are Zed's, in Zed [2][3].
- **`gpui-component`'s exact current version and API stability** were not pinned in this pass (release page exists [14] but I did not extract a version). Verify before depending on it.
- **Warp is closed-source**; their numbers (>144fps, 1.9ms, ~200-line shaders) come from their own engineering blog [8][9][17] and are not independently reproducible by us.
- **Vello timeline is speculative** - "revisit in ~12 months" is a guess, not a roadmap commitment.
- **Bundled iMWriting Nerd Font interaction with GPUI's font loading** (font-kit/CoreText) is unverified - confirm a Nerd-Font-patched mono with private-use-area glyphs renders correctly in GPUI's atlas. Likely fine (it's CoreText underneath) but untested here.

## Open questions for the product owner

1. **Is screen-reader / VoiceOver accessibility a launch requirement, or a deferred milestone?** This single answer decides GPUI vs the custom wgpu+parley path. (GPUI: defer; custom: ship.)
2. **How much pre-1.0 churn / periodic forced migration is acceptable** in exchange for GPUI's months of saved build time? Are you comfortable pinning and treating upgrades as scheduled work?
3. **Is depending on Zed's framework (a competitor-adjacent project) acceptable strategically**, or do you want aterm's renderer fully owned (custom path) for independence?
4. **Cross-platform horizon:** is Linux/Windows a real near-term goal (favors wgpu's uniformity) or genuinely "not precluded, much later" (GPUI's mac-first maturity is fine)?
5. **Do you want to fund a 1-2 week GPUI spike** (mono grid + input box + virtualized transcript + CJK IME test on `gpui 0.2.2` + bundled Nerd Font) before committing? Strongly recommended given the open verification items above.

## Sources

1. GPUI README, zed-industries/zed: https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md
2. "Leveraging Rust and the GPU to render user interfaces at 120 FPS", Zed blog: https://zed.dev/blog/videogame
3. "Optimizing the Metal pipeline to maintain 120 FPS in GPUI", Zed blog: https://zed.dev/blog/120fps
4. `gpui` crate, crates.io (v0.2.2, Apache-2.0, 2025-10-22): https://crates.io/crates/gpui
5. "Accessibility (a11y) in Zed" discussion #6576: https://github.com/zed-industries/zed/discussions/6576
6. "Is screen reader compatibility on macOS in the works" discussion #8146: https://github.com/zed-industries/zed/discussions/8146
7. Parley releases / AccessKit text properties (v0.8.0, AccessKit 0.24): https://github.com/linebender/parley/releases
8. "How Warp Works", Warp blog (>144fps, 1.9ms, ~200-line shaders): https://www.warp.dev/blog/how-warp-works
9. "Why is building a UI in Rust so hard?", Warp blog: https://www.warp.dev/blog/why-is-building-a-ui-in-rust-so-hard
10. "A 2025 Survey of Rust GUI Libraries", boringcactus (gpui/iced/egui IME+a11y hands-on, Apr 2025): https://www.boringcactus.com/2025/04/13/2025-survey-of-rust-gui-libraries.html
11. "Linebender in December 2025" (Vello status): https://linebender.org/blog/tmil-24/
12. `vello` on lib.rs: https://lib.rs/crates/vello
13. "Please extract GPUI" discussion #30515 (maintainer stance, gpui-ce fork): https://github.com/zed-industries/zed/discussions/30515
14. `longbridge/gpui-component` (60+ standalone GPUI components, Longbridge Pro): https://github.com/longbridge/gpui-component
15. Apache License v2.0 and GPL Compatibility, ASF: https://www.apache.org/licenses/GPL-compatibility.html
16. GPUI crate dir (platform backends): https://github.com/zed-industries/zed/tree/main/crates/gpui
17. "Adventures in Text Rendering: Kerning and Glyph Atlases", Warp blog: https://www.warp.dev/blog/adventures-text-rendering-kerning-glyph-atlases
18. (same as 9) Warp on Rust UI difficulty / ECS approach: https://www.warp.dev/blog/why-is-building-a-ui-in-rust-so-hard
19. `wgpu` crate, crates.io (v29.0.3, MIT/Apache-2.0): https://crates.io/crates/wgpu
20. `winit` crate, crates.io (v0.30.13, Apache-2.0): https://crates.io/crates/winit
21. `parley` crate, crates.io (v0.10.0, Apache-2.0/MIT, 2025-06-01): https://crates.io/crates/parley
22. `cosmic-text` crate, crates.io (v0.19.0, MIT/Apache-2.0): https://crates.io/crates/cosmic-text
23. `glyphon` crate, docs.rs/crates.io (v0.11.0): https://docs.rs/glyphon
24. `swash` crate, crates.io (v0.2.9, Apache-2.0/MIT): https://crates.io/crates/swash
25. `egui` crate, crates.io (v0.34.3, MIT/Apache-2.0): https://crates.io/crates/egui
26. egui README / AccessKit support: https://github.com/emilk/egui
27. egui Text Input and Editing (DeepWiki): https://deepwiki.com/emilk/egui/4.5-text-input-and-editing
28. `iced` crate, crates.io (v0.14.0, MIT, 2025-12-07): https://crates.io/crates/iced
29. iced-rs/iced repo: https://github.com/iced-rs/iced
30. `vello` crate, crates.io (v0.9.0, Apache-2.0/MIT, 2026-05-15): https://crates.io/crates/vello
31. `accesskit` crate, crates.io (v0.24.1, MIT/Apache-2.0, 2026-06-12): https://crates.io/crates/accesskit
32. AccessKit project (NSAccessibility/UIA/AT-SPI adapters): https://accesskit.dev/
