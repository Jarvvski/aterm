---
title: GPU Text + Glyph Rendering Pipeline
domain: text-glyph-rendering
status: research
---

# GPU Text + Glyph Rendering Pipeline

## TL;DR

- **Recommend the GPU-glyph-atlas + alpha-mask architecture used by Zed/GPUI, Ghostty, and Alacritty: rasterize each glyph once (via OS CoreText on macOS) into a cached atlas, store only the 8-bit alpha (coverage) channel, then composite with per-glyph color via a single instanced draw call.** This is the only approach with multiple shipping proofs of 120fps text on Apple Silicon [1][2][6].
- **Two viable Rust stacks: (A) GPUI's own text system (proven in Zed at 120fps, CoreText rasterization, etagere atlas, 16 subpixel variants per glyph) or (B) the DIY `wgpu` + `cosmic-text` (0.19) + `swash` (0.2.x) + `glyphon` (0.9) stack.** GPUI buys you a battle-tested pipeline but couples you to a large opinionated UI framework; the DIY stack is more work but gives a terminal-shaped grid fast-path you fully control. **Lean toward GPUI if you adopt it as the whole UI layer; lean toward the DIY wgpu stack if you want a custom block/timeline renderer.** (See Recommendations - this is the load-bearing call for the product owner.)
- **Run ONE shaping/layout engine but TWO render paths.** cosmic-text and Parley both now shape with HarfRust (Rust HarfBuzz port) [3]. Use a constant-advance "grid fast path" for the monospace terminal (skip shaping for plain ASCII runs; cache shaped runs for the rest) and a normal proportional layout path for agent prose in the Duo/Quattro variants. Same atlas, same shader, same FontSystem.
- **macOS is grayscale-AA-only in practice since Mojave (10.14, 2018) disabled subpixel AA** [4][5]. On Retina/ProMotion this is fine and is what Zed, Ghostty, and modern Terminal.app do. **Do NOT build subpixel (LCD) AA as a v1 requirement** - it conflicts with translucency, per-glyph color, and the iA aesthetic. Grayscale alpha masks + gamma-correct blend is the call.
- **Ligatures: shape them, but only on the non-fast-path.** iM Writing carries Plex-derived ligatures; shaping a line costs ~microseconds and is cached per-line (WezTerm caches shaping+BiDi per line [7]). The cost is reshaping on edit, not steady-state. Plain ASCII grid cells can bypass shaping entirely.
- **Nerd Font specifics need a Ghostty-style per-codepoint Constraint table.** PUA/symbol glyphs (Powerline E0A0-E0D7, thousands of icons across BMP PUA E000-F8FF and SMP PUA U+F0000+) do not match the base font's cell metrics and must be scaled/centered/stretched per codepoint to align in the grid [8][9][2]. Plan to vendor or regenerate this table.

## Findings

### 1. The core architecture every fast terminal converges on

All three reference terminals/editors (Zed/GPUI, Ghostty, Alacritty) and the cosmic-text/glyphon stack use the same shape:

1. **Shape + lay out** a run of text (codepoints -> positioned glyph IDs).
2. **Rasterize each unique (glyph, size, subpixel-offset) once** into a small bitmap.
3. **Pack that bitmap into a GPU texture atlas** (bin-packing), recording its UV rect on the CPU.
4. **Per frame, emit one instance per visible glyph** (atlas UV + screen position + color) and draw the whole screen in a **single instanced draw call** sampling the atlas.

GPUI's blog states the composite step "approximates the bandwidth of the GPU, as we are literally copying bytes from one texture to the other" [1]. Ghostty: "the atlas is uploaded to the GPU once. Subsequent frames reference the atlas without re-rasterizing" [2]. This is why steady-state typing/scrolling is cheap - the expensive work (rasterization, shaping) is amortized into caches that almost never miss for terminal text.

#### Alpha-only mask, color by multiplication

GPUI rasterizes **"only the alpha component (the opacity) of the glyph"** so any color can be applied "using a simple multiplication and avoid storing one copy of the glyph in the atlas for each color used" [1]. This is essential for a terminal: 16/256/truecolor ANSI colors and theme changes must not multiply atlas storage. Adopt alpha-only (grayscale coverage) atlas as the default; reserve a separate BGRA atlas only for color emoji / color glyphs (Ghostty uses exactly this split: `.grayscale` 1 byte, `.bgra` 4 bytes, plus a `.bgr` 3-byte format for subpixel that we will not use) [2].

#### Subpixel positioning variants

GPUI generates **"up to 16 different variants of each individual glyph to account for sub-pixel positioning, since CoreText subtly adjusts antialiasing"** [1]. This is about *fractional pen position within a pixel* (horizontal AA quality), NOT LCD subpixel color AA. swash's `Render` exposes `offset` for "fractional positioning" and the `ScaleContext` LRU-caches scaled outlines [10][11]. For a monospace grid most glyphs land on integer cell origins, so the variant count needed is far smaller than for proportional text; for the proportional agent-prose path you want the full set.

### 2. One shaping engine, two render paths (monospace grid vs proportional UI)

- **Shaping convergence on HarfRust.** Both cosmic-text (0.19.0) and Parley now shape with HarfRust, Google Fonts' Rust port of HarfBuzz [3]. cosmic-text wraps shaping, fontdb font discovery, font fallback, swash rasterization, layout, and editing behind `FontSystem` / `Buffer` / `SwashCache` [12]. Parley switched from swash to HarfRust for shaping and icu4x for text analysis, giving "production-quality shaping for all scripts" [3].
- **Grid fast path.** A monospace terminal does NOT need to shape every cell. Plain runs of single-width ASCII can be mapped codepoint->glyph directly at constant advance, bypassing the shaper entirely - this is the hot path that must hit 60-120fps. cosmic-text exposes a `Monospace` width hint and a `ShapeRunCache` "for caching shape runs ... a critical optimization for terminal-like environments with frequent character redraws" [12].
- **Proportional path.** Agent prose rendered in iMWriting Duo/Quattro is normal rich-text layout (wrapping, mixed widths, possibly ligatures and fallback). This goes through full shaping + layout. It is low-frequency (prose streams in at human-reading speed), so its cost is irrelevant to the 60fps floor as long as it is incremental.
- **Single pipeline, two front-ends.** The atlas, shader, and `FontSystem` are shared. Only the layout front-end differs (constant-advance grid layout vs `Buffer` layout). This is the recommended structure: do not build two GPU pipelines.

### 3. Nerd Font / iM Writing specifics

The project bundles **iMWriting Mono Nerd Font Mono (NFM)** - confirmed in the prior prototype: `ui-desktop/src/main/resources/fonts/iMWritingMonoNerdFontMono-{Regular,Bold,Italic,BoldItalic}.ttf` plus `LICENSE-iAWriterNerdFont.md`. **Note: the prior prototype bundled only the Mono variant; Duo/Quattro were NOT vendored.** The agent-prose proportional requirement means Duo/Quattro must be added (or fallback to a system proportional face).

- **Codepoint ranges.** Nerd Font glyphs live in the Unicode Private Use Area or pre-assigned ranges. Powerline: `E0A0-E0A2, E0B0-E0B3`; Powerline Extra: `E0A3, E0B4-E0C8, E0CA, E0CC-E0D7, 2630`; broader icon sets span BMP PUA (E000-F8FF) and Material Design Icons in the SMP PUA at **U+F0000+** [8][9]. The grid renderer must handle codepoints beyond the BMP.
- **Constraint/alignment table (the important one).** Nerd Font glyphs are patched in at sizes that do not match an arbitrary base font's cell box, so they look "small, squished, or not full width" without correction [9]. Ghostty solves this with a generated per-codepoint constraint table: `src/font/nerd_font_codegen.py` extracts scaling/positioning directives from the official Nerd Fonts patcher into `src/font/nerd_font_attributes.zig`, exposing `getConstraint(cp)` returning constraints (e.g. `.center1` = center within the first cell of a multi-cell glyph) "for thousands of codepoints" [2]. **aterm needs an equivalent table** (regenerate from the patcher, or vendor Ghostty's mapping under a compatible license). This is non-trivial and easy to under-scope.
- **Double-width cells.** Icons and CJK occupy 2 grid cells. Cell width must be computed via Unicode East Asian Width + the Nerd Font width rules; the constraint table also drives center-in-cell-1 vs span-both-cells behavior [2][9].
- **Box-drawing / Powerline as a sprite font.** Ghostty does NOT rely on the font for box-drawing, block elements, Powerline separators, braille, or "Symbols for Legacy Computing" - it draws them procedurally in `src/font/sprite/Face.zig` via a `z2d` canvas (`box()`, `line()`, `fill()`, then `trim()`) so they are always pixel-perfect and seamless regardless of font [2]. **Strongly consider a built-in sprite face for these ranges**; it removes a whole class of misalignment bugs and is render-cheap (drawn once into the atlas).
- **Color emoji / fallback.** Color glyphs go in a BGRA atlas (cosmic-text/swash support color emoji [12]). Font fallback for missing glyphs (CJK, emoji, symbols not in iMWriting) is handled by cosmic-text's fontdb-backed fallback chain [12]; for a tighter grid you may want an explicit, ordered fallback list (base -> Nerd symbols -> system CJK -> system emoji) rather than fontdb's automatic resolution.

### 4. Ligatures

iM Writing inherits IBM Plex / iA Writer ligatures. For a controlled-UI terminal:

- **Cost is in reshaping, not steady state.** WezTerm caches font shaping + BiDi per line: "the initial draw of a screenful ... may take a few milliseconds the first time, but then should be relatively cheap" - the worst case is *rapid scrollback* that blows the cache [7]. For aterm's block model (finite committed command blocks + bounded live region) this is very manageable.
- **Grid bypass for plain ASCII.** Lines with no ligature-triggering sequences can skip shaping. Detect with a cheap scan; only route lines containing candidate operator sequences (`==`, `=>`, `->`, `!=`, `<=`, etc.) through the shaper. Alacritty deliberately omits ligatures to "keep the terminal lean" [13]; aterm can have them because it is not a hot-loop raw terminal and its block model bounds the dirty region.
- **Recommendation: ligatures ON for committed/idle text, with a fast-path bypass; OFF or deferred during high-throughput streaming.** This keeps the 60fps floor safe while delivering the iA look at rest.

### 5. Performance: caching, damage/dirty-region redraw, cost model

- **Damage tracking is mandatory.** "Missing damage tracking and always painting everything kills performance and input latency" for realistic workloads (editing, blinking cursors, progress bars) [7]. GPUI's retained scene graph "only re-renders regions that changed (dirty rects)" - typing one character reshapes 1 line and submits 1 draw call [6]. Rio explicitly improved "damage merging to always accumulate updates" and coalesces non-synchronized updates [7].
- **Do not reshape unchanged lines.** Associate cached shaping results with each line/block; invalidate only on content change. cosmic-text's `ShapeRunCache` and per-line caches (WezTerm pattern) are the mechanism [12][7].
- **Full-scrollback redraw cost model.** Only the *visible* rows are ever turned into draw instances; scrollback is data, not geometry. Cost per frame ~ O(visible cells) for instance generation + O(1) draw call + atlas already resident. The expensive transition is *scroll velocity high enough to miss the shape cache* every frame [7] - mitigate with a generous shape cache and the ASCII bypass.
- **Frame pacing.** Cap to display refresh (WezTerm exposes `max_fps`; the lesson is do not free-run the renderer) [7]; drive redraw from damage + a vsync/`CADisplayLink`-style signal, not a busy loop, to hit the 120fps ProMotion target without burning power.

### 6. Render-stack options, concretely

| Option | Rasterizer | Shaper | Atlas | Status / risk |
|---|---|---|---|---|
| **GPUI text system** | CoreText (via `font-kit` on macOS) | platform | etagere | Proven 120fps in Zed [1][6]; couples you to GPUI as the whole UI layer; APIs "best learned from Zed source" [14] |
| **wgpu + cosmic-text + swash + glyphon** | swash (Rust) or CoreText | HarfRust (via cosmic-text 0.19) [3][12] | etagere (in glyphon) | glyphon 0.9.0 (Apr 2025), "middleware pattern" integrates into your wgpu pass [15]; you own the grid fast-path; more integration work |
| **wgpu + Parley** | swash/own | HarfRust + icu4x [3] | your own | Best-in-class proportional layout; no built-in atlas/GPU renderer - you build the atlas + shader; younger for terminal-grid use |
| Custom (swash + ab_glyph/fontdue + hand-rolled atlas) | swash / ab_glyph / fontdue | rustybuzz or HarfRust | hand-rolled | Maximum control, maximum work; only if the above genuinely block you |

Current versions verified at time of research: cosmic-text **0.19.0** [12], swash **0.2.x** (0.2.6/0.2.7 on docs.rs) [10], glyphon **0.9.0** [15], wgpu **29.0.3** [16]. (Pin exact versions at build time; these move.)

GPUI's macOS text path requires `font-kit` for glyph rasterization or it "falls back to a placeholder text system that lays text out but renders no glyphs" [14] - i.e. GPUI does the OS-native CoreText rasterization that matches other macOS apps [1].

## Recommendations for aterm

1. **Architecture: GPU glyph atlas + alpha-only mask + single instanced draw call per frame.** Rationale: the only approach with multiple shipping 120fps proofs on Apple Silicon. **Confidence: High.**
2. **Rasterize via CoreText on macOS (native look, matches other apps), with swash as the portable fallback for the Linux/Windows-later goal.** Rationale: GPUI/Ghostty both use CoreText for native-matching AA; swash keeps the door open cross-platform. **Confidence: High.**
3. **Grayscale AA only; gamma-correct blend in linear space; NO LCD subpixel AA in v1.** Rationale: macOS disabled subpixel AA in 2018 [4][5]; subpixel fights translucency, per-glyph color, and the iA aesthetic. **Confidence: High.**
4. **Stack choice (the pivotal call):** If aterm's whole UI is built in **GPUI**, use GPUI's text system - do not reinvent it. If aterm builds a **custom wgpu renderer** for the block/timeline UI, use **wgpu + cosmic-text(0.19) + swash + glyphon(0.9)** and own the monospace fast-path. **Recommend the wgpu + cosmic-text/glyphon stack** unless the broader architecture research picks GPUI as the UI framework, because the terminal grid fast-path and custom block UI are easier to control outside GPUI's opinions. Rationale: control over the hot path vs proven turnkey path; this depends on the UI-framework decision in the adjacent render-stack research. **Confidence: Med** (contingent on the framework decision).
5. **One shaping engine (HarfRust via cosmic-text), two layout front-ends: constant-advance grid + proportional `Buffer`. Shared atlas/shader/FontSystem.** Rationale: convergent industry pattern; avoids a second GPU pipeline. **Confidence: High.**
6. **Implement an ASCII grid bypass that skips shaping for plain single-width runs; cache shaped runs per line/block via `ShapeRunCache`.** Rationale: keeps the 60fps floor safe; matches WezTerm/cosmic-text caching [7][12]. **Confidence: High.**
7. **Ship a built-in sprite face for box-drawing, block elements, Powerline separators, and braille (Ghostty pattern).** Rationale: removes a whole class of font-misalignment bugs; cheap. **Confidence: Med-High.**
8. **Build/vendor a Nerd Font per-codepoint Constraint table (regenerate from the Nerd Fonts patcher, à la Ghostty's `nerd_font_codegen.py`).** Rationale: PUA icons will look squished/misaligned without it; do not under-scope this. **Confidence: High.**
9. **Ligatures ON at rest with ASCII bypass; throttle/disable during high-throughput streaming.** Rationale: delivers the iA look without risking the frame floor [7][13]. **Confidence: Med.**
10. **Damage/dirty-region redraw driven by display-refresh signal (vsync/CADisplayLink), capped to refresh rate; never free-run.** Rationale: mandatory for input latency and power; proven by GPUI/Rio/WezTerm [6][7]. **Confidence: High.**
11. **Add the iMWriting Duo/Quattro variants to the bundle (only Mono is currently vendored) for agent prose, or define an explicit proportional fallback.** Rationale: the proportional UI/prose requirement has no font shipped yet. **Confidence: High.**

## Risks & unknowns

- **GPUI as a dependency.** GPUI's text APIs are documented as "best learned from Zed source" [14] and it is a large, fast-moving, Zed-internal framework. Adopting it for text but not UI is awkward; adopting it for everything is a big commitment. Not independently verified that GPUI's text system is cleanly usable standalone.
- **Nerd Font constraint table licensing/effort.** Regenerating from the patcher is real work; vendoring Ghostty's `.zig`-generated data needs a license check (Ghostty is MIT; aterm is GPLv3, which is compatible to consume, but verify). Not yet verified.
- **swash vs HarfRust shaping nuance.** cosmic-text 0.19 shapes with HarfRust but rasterizes with swash; swash also has its own shaper. Exact integration seams and which path handles iMWriting ligatures/contextual alternates correctly need a spike. Not verified end-to-end.
- **Subpixel-positioning variant count for the grid.** GPUI's "up to 16 variants" [1] is for proportional text; the right number for a constant-advance grid (likely far fewer, mostly integer origins) is unverified and should be measured.
- **Color-emoji + alpha-atlas interaction.** Mixing a grayscale alpha atlas with a BGRA color atlas in one draw call needs either a second pass or a format flag per instance; cost not measured.
- **Benchmark numbers are claims, not measurements.** The 120fps figures are from vendor blogs/wikis [1][6][7], not reproduced here. aterm must benchmark its own pipeline against the 60fps floor early.
- **iMWriting glyph coverage.** Whether the bundled iMWriting Nerd patch already includes the full Powerline/symbol set vs needing system fallback is unverified - inspect the actual TTFs.

## Open questions for the product owner

1. **Is the overall UI being built in GPUI, or a custom wgpu renderer?** This single decision determines the text stack (Recommendation 4) and should be settled by the render-stack/architecture research before text work starts.
2. **Ligatures: on by default at rest?** Confirm the product wants iA-style ligatures in the terminal grid (not just prose), accepting the bypass/throttle complexity.
3. **Subpixel-AA stance for non-Retina external displays.** Confirm grayscale-only is acceptable (it is the modern macOS default and the recommendation), forgoing any LCD-AA escape hatch.
4. **Bundle the Duo/Quattro variants now?** Confirm shipping the proportional iMWriting faces for agent prose, or specify a system-font fallback for proportional UI text.
5. **Color emoji in the terminal grid - required for v1?** Affects whether the BGRA color atlas path ships in v1 or is deferred.

## Sources

1. Zed Blog, "Leveraging Rust and the GPU to render user interfaces at 120 FPS" - https://zed.dev/blog/videogame
2. DeepWiki, Ghostty "Glyph Rendering and Atlases" (constraint table, sprite face, atlas formats) - https://deepwiki.com/ghostty-org/ghostty/5.5.3-glyph-rendering-and-atlases
3. Linebender blog, "Linebender in October 2025" (Parley -> HarfRust + icu4x; cosmic-text on HarfRust) - https://linebender.org/blog/tmil-22/
4. Michael Tsai, "macOS 10.14 Mojave Removes Subpixel Anti-aliasing" - https://mjtsai.com/blog/2018/07/13/macos-10-14-mojave-removes-subpixel-anti-aliasing/
5. How-To Geek, "How to Fix Blurry Fonts on macOS Mojave (With Subpixel Antialiasing)" - https://www.howtogeek.com/358596/how-to-fix-blurry-fonts-on-macos-mojave-with-subpixel-antialiasing/
6. johal.in, "Zed 0.13 / GPUI 0.3 Internals: Fast Rust Rendering" (dirty rects, 1 line reshape, 1 draw call, etagere/hashbrown) - https://johal.in/internals-zed-013-uses-gpui-03-fast-rendering/
7. Hacker News discussion on terminal damage tracking + WezTerm per-line shaping/BiDi cache - https://news.ycombinator.com/item?id=42518110 ; Rio changelog (damage merging) - https://rioterm.com/changelog ; WezTerm `max_fps` - https://wezterm.org/config/lua/config/max_fps.html
8. Nerd Fonts Wiki, "Glyph Sets and Code Points" (Powerline ranges, PUA placement) - https://github.com/ryanoasis/nerd-fonts/wiki/Glyph-Sets-and-Code-Points
9. Nerd Fonts FAQ, "Why do the glyphs look small, squished, or not full width" - https://github.com/ryanoasis/nerd-fonts/wiki/FAQ-and-Troubleshooting
10. swash docs.rs (ScaleContext LRU caches, Render offset/subpixel) - https://docs.rs/swash/latest/swash/scale/index.html
11. swash `Render` struct (subpixel format, fractional positioning) - https://docs.rs/swash/latest/swash/scale/struct.Render.html
12. cosmic-text docs.rs 0.19.0 (FontSystem/Buffer/SwashCache, harfrust, monospace, fallback, color emoji, ShapeRunCache) - https://docs.rs/cosmic-text/latest/cosmic_text/
13. brev.al, "Enabling Ligatures (and Why Alacritty Might Break Your Heart)" + Alacritty scope-no-ligatures rationale - https://brev.al/blog/articles/coding-ligatures-alacritty-nerd-fonts
14. GPUI README (macOS rasterization needs font-kit; DirectWrite on Windows; APIs learned from Zed source) - https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md
15. glyphon GitHub / docs.rs 0.9.0 (cosmic-text + etagere + wgpu, middleware pattern) - https://github.com/grovesNL/glyphon ; https://docs.rs/glyphon/latest/glyphon/index.html
16. wgpu releases / crates.io (29.0.3, Metal default on macOS) - https://github.com/gfx-rs/wgpu/releases ; https://crates.io/crates/wgpu
