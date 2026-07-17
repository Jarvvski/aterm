//! Proportional prose layout - the SECOND layout front-end over the shared
//! [`crate::atlas::GlyphAtlas`] (ticket T-4.3).
//!
//! Where the terminal grid ([`crate::grid_render`]) places one constant-advance cell
//! per column with a direct cmap lookup, agent prose is real proportional text: the
//! iA "duospace" **Duo** register for body prose (`type.body`) and the near-
//! proportional **Quattro** register for dense chrome labels (`type.label` /
//! `type.caption`). This module runs the FULL swash shaper (`08-text-glyph-rendering.md`
//! §2: one shaping engine, two layout front-ends) - clusters, per-glyph advances,
//! kerning, and ligatures/contextual alternates where the face carries them - then
//! greedily word-wraps the shaped clusters at the prose measure. The grid stays Mono
//! and uncapped; only prose is measured.
//!
//! ## What is pure vs font-coupled
//! - [`wrap_lines`] (greedy word-wrap over cluster advances) and [`measure_px`] (the
//!   `ch`-based column width) are PURE and unit-tested headlessly.
//! - [`ProseShaper::layout`] runs swash over the bundled face bytes - no GPU, no window,
//!   so its tests also run on every platform (the crate's "pure logic, no window" rule).
//! - [`ProseRenderer`] is the only GPU-coupled piece (it acquires shaped glyphs from the
//!   shared atlas and draws them); its tests are macOS-gated like the grid's.
//!
//! ## Measured metrics (the AC3 documentation, measured from the bundled `*-Regular.ttf`)
//! `units_per_em = 1000`; vertical metrics are SHARED across all three registers
//! (`ascent 1025, descent 275, leading 0, cap_height 698, x_height 516`), so prose and
//! grid share one baseline geometry. Advance widths in em-fractions (advance / upem):
//!
//! | glyph        | Mono  | Duo   | Quattro |
//! |--------------|-------|-------|---------|
//! | space        | 0.667 | 0.600 | 0.450   |
//! | i, l         | 0.667 | 0.600 | 0.300   |
//! | r, f         | 0.667 | 0.600 | 0.450   |
//! | s, a, 0, n   | 0.667 | 0.600 | 0.600   |
//! | m, w, M, W   | 0.667 | 0.900 | 0.900   |
//! | average      | 0.667 | 0.874 | 0.873   |
//!
//! So **Mono** is a constant 0.667em (the grid invariant), **Duo** is duospace (0.6em
//! with m/w/M/W at 0.9em = 1.5x), and **Quattro** spans four widths (0.3 / 0.45 / 0.6 /
//! 0.9em). The [`prose_metrics`] tests assert these directly against the live faces, so
//! the table cannot silently drift from the bundle.

use std::mem::size_of;
use std::ops::Range;

use swash::shape::ShapeContext;
use swash::text::Script;
use swash::FontRef;

use crate::atlas::{GlyphAtlas, GlyphInstance, InstanceBuffer};
use crate::text::{FaceStyle, FontFamily, GlyphKey};

/// Default agent-prose measure in characters - iA's middle line-length option, mirrored
/// by [`aterm_tokens::type_scale::MEASURE_CH`]. A `ch` is the advance of '0'.
pub use aterm_tokens::type_scale::MEASURE_CH;

/// A single positioned prose glyph: its font glyph id plus the PEN origin (the
/// baseline-left point) in physical px, relative to the layout's top-left. The render
/// path turns this into a quad by offsetting with the rasterized glyph's `(left, top)`,
/// so the family/face/px (shared by the whole layout) plus the pen position is all the
/// layout needs to carry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PositionedGlyph {
    /// Font glyph id from the shaper (NOT a codepoint).
    pub glyph_id: u16,
    /// Pen x: the glyph's left origin, physical px, relative to the layout origin.
    pub pen_x: f32,
    /// Baseline y: physical px, relative to the layout origin (top = 0).
    pub baseline: f32,
}

/// A laid-out prose paragraph: the positioned glyphs plus the block extent. One layout
/// is a single `(family, face)` at one size - mixed bold/italic/size runs (markdown
/// emphasis) are a richer composition layer (T-4.6), not T-4.3.
#[derive(Debug, Clone)]
pub struct ProseLayout {
    pub family: FontFamily,
    pub face: FaceStyle,
    /// The pixel size the glyphs were shaped + must be rasterized at.
    pub px: f32,
    pub glyphs: Vec<PositionedGlyph>,
    /// Number of visual (soft + hard) lines the text occupied.
    pub line_count: usize,
    /// The widest line's content width in px (trailing whitespace excluded). NOTE: this
    /// can EXCEED `measure_px` when a single unbreakable token (e.g. a long URL or path)
    /// is wider than the measure - greedy wrap overflows such a token rather than
    /// splitting mid-word (no hyphenation). A caller sizing a container from this must
    /// clamp/scroll rather than assume `width <= measure`.
    pub width: f32,
    /// Total block height in px (`line_count * line_height_px`).
    pub height: f32,
}

/// One shaped cluster's wrap-relevant facts: its total advance and whether it is a
/// break opportunity (whitespace). The pure [`wrap_lines`] consumes a slice of these.
#[derive(Debug, Clone, Copy)]
struct ClusterAdvance {
    advance: f32,
    breakable: bool,
}

/// One shaped glyph within a cluster (kept so a multi-glyph cluster - marks, ligature
/// components - positions correctly).
#[derive(Debug, Clone, Copy)]
struct ShapedGlyph {
    glyph_id: u16,
    x: f32,
    advance: f32,
}

/// A shaped cluster: its glyphs + advance + break flag.
#[derive(Debug, Clone)]
struct ShapedCluster {
    glyphs: Vec<ShapedGlyph>,
    advance: f32,
    breakable: bool,
}

/// Greedy first-fit word-wrap: assign each cluster to a soft line so each line's running
/// advance stays within `measure`, breaking only at whitespace clusters (break
/// opportunities). A single word wider than the measure overflows its line rather than
/// being split mid-word (no hyphenation in v1). Returns one `[start, end)` cluster range
/// per soft line. Pure - the unit tests drive it with synthetic advances.
fn wrap_lines(clusters: &[ClusterAdvance], measure: f32) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    if clusters.is_empty() {
        return lines;
    }
    let mut start = 0usize;
    let mut x = 0.0f32;
    // The last whitespace cluster index we may break AFTER (only valid if >= start).
    let mut last_break: Option<usize> = None;
    let mut i = 0usize;
    while i < clusters.len() {
        let c = clusters[i];
        let next_x = x + c.advance;
        // Only a WORD glyph that won't fit forces a break; a trailing whitespace that
        // tips the running width past the measure does not (it just ends the line and
        // is dropped visually, being inkless). This is the standard greedy rule and is
        // why "AAAA BBBB " can fill a line even though the closing space spills over.
        if !c.breakable && next_x > measure && last_break.is_some_and(|b| b >= start) {
            let b = last_break.unwrap();
            lines.push(start..b + 1);
            start = b + 1;
            // Re-accumulate the pending (non-broken) run that has spilled onto the new
            // line, then re-examine cluster `i` without advancing.
            x = clusters[start..i].iter().map(|c| c.advance).sum();
            last_break = None;
            continue;
        }
        x = next_x;
        if c.breakable {
            last_break = Some(i);
        }
        i += 1;
    }
    lines.push(start..clusters.len());
    lines
}

/// The prose measure in physical px: `chars * advance('0')`, the CSS `ch` unit, where
/// the reference advance is '0' in `family`'s Regular face at `px`. Pure (no shaper -
/// a direct cmap + glyph-metrics lookup). The default agent-prose column is
/// `measure_px(family, MEASURE_CH, px)`.
#[must_use]
pub fn measure_px(family: FontFamily, chars: u16, px: f32) -> f32 {
    let bytes = crate::fonts::face_bytes(family, FaceStyle::Regular);
    let Some(font) = FontRef::from_index(bytes, 0) else {
        return f32::from(chars) * px * 0.6;
    };
    let upem = f32::from(font.metrics(&[]).units_per_em).max(1.0);
    let gid = font.charmap().map('0');
    let adv = font.glyph_metrics(&[]).advance_width(gid) / upem * px;
    f32::from(chars) * adv
}

/// Shapes + wraps proportional prose into a [`ProseLayout`]. Holds the swash
/// [`ShapeContext`] (LRU caches + scratch) so it amortizes across paragraphs; lives on
/// the render thread, which the grid never shares with shaping (the grid is cmap-only).
pub struct ProseShaper {
    ctx: ShapeContext,
}

impl Default for ProseShaper {
    fn default() -> Self {
        Self::new()
    }
}

impl ProseShaper {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ctx: ShapeContext::new(),
        }
    }

    /// Shape one hard-line of `text` in `(family, face)` at `px` into shaped clusters
    /// (full shaping: ligatures + contextual alternates + kerning where the face has
    /// them). The shaper yields advances in px because the builder size is set.
    fn shape_clusters(
        &mut self,
        text: &str,
        family: FontFamily,
        face: FaceStyle,
        px: f32,
    ) -> Vec<ShapedCluster> {
        let bytes = crate::fonts::face_bytes(family, face);
        let Some(font) = FontRef::from_index(bytes, 0) else {
            return Vec::new();
        };
        let mut shaper = self
            .ctx
            .builder(font)
            .script(Script::Latin)
            .size(px)
            .features(&[("liga", 1), ("calt", 1), ("kern", 1)])
            .build();
        shaper.add_str(text);
        let mut out = Vec::new();
        shaper.shape_with(|cluster| {
            let glyphs = cluster
                .glyphs
                .iter()
                .map(|g| ShapedGlyph {
                    glyph_id: g.id,
                    x: g.x,
                    advance: g.advance,
                })
                .collect();
            out.push(ShapedCluster {
                glyphs,
                advance: cluster.advance(),
                breakable: cluster.info.is_whitespace(),
            });
        });
        out
    }

    /// Lay out `text` in `(family, face)` at `px`, wrapping each hard-line (split on
    /// `\n`) at `measure_px` and stacking soft lines `line_height_px` apart. Whitespace
    /// glyphs advance the pen but are not emitted (they are inkless anyway). Pen
    /// positions are left fractional here; the render path snaps the final quad to
    /// integer pixels (the crispness discipline, mirroring the grid).
    #[must_use]
    pub fn layout(
        &mut self,
        text: &str,
        family: FontFamily,
        face: FaceStyle,
        px: f32,
        measure_px: f32,
        line_height_px: f32,
    ) -> ProseLayout {
        self.layout_with_last_line_width(text, family, face, px, measure_px, line_height_px)
            .0
    }

    /// Editor-facing variant that also returns the final visual line's advance. Keeping
    /// this crate-private avoids widening the general prose interface merely for caret
    /// placement while still using the exact same shaping and wrapping implementation.
    pub(crate) fn layout_with_last_line_width(
        &mut self,
        text: &str,
        family: FontFamily,
        face: FaceStyle,
        px: f32,
        measure_px: f32,
        line_height_px: f32,
    ) -> (ProseLayout, f32) {
        // Round the size ONCE here (the source) so we shape, place pens, AND later
        // rasterize at the same integer px - hinted glyphs map 1:1 under the atlas's
        // Nearest sampler, exactly as the grid rounds its px once (grid_render.rs).
        // `layout.px` is therefore always integer-valued downstream.
        let px = px.round().max(1.0);
        let ascent = self.ctx_ascent(family, px);
        let mut glyphs = Vec::new();
        let mut line_index = 0usize;
        let mut max_width = 0.0f32;
        let mut last_line_width = 0.0f32;

        for hard_line in text.split('\n') {
            let clusters = self.shape_clusters(hard_line, family, face, px);
            if clusters.is_empty() {
                // A blank hard-line still occupies one line of vertical rhythm.
                line_index += 1;
                last_line_width = 0.0;
                continue;
            }
            let advances: Vec<ClusterAdvance> = clusters
                .iter()
                .map(|c| ClusterAdvance {
                    advance: c.advance,
                    breakable: c.breakable,
                })
                .collect();
            for range in wrap_lines(&advances, measure_px) {
                let baseline = line_index as f32 * line_height_px + ascent;
                let mut pen_x = 0.0f32;
                let mut content_w = 0.0f32;
                for ci in range {
                    let c = &clusters[ci];
                    for g in &c.glyphs {
                        if !c.breakable {
                            glyphs.push(PositionedGlyph {
                                glyph_id: g.glyph_id,
                                pen_x: pen_x + g.x,
                                baseline,
                            });
                        }
                        pen_x += g.advance;
                    }
                    if !c.breakable {
                        content_w = pen_x; // width up to the last non-whitespace cluster
                    }
                }
                max_width = max_width.max(content_w);
                last_line_width = content_w;
                line_index += 1;
            }
        }

        (
            ProseLayout {
                family,
                face,
                px,
                glyphs,
                line_count: line_index,
                width: max_width,
                height: line_index as f32 * line_height_px,
            },
            last_line_width,
        )
    }

    /// The Regular-face ascent (px) for `family` - the first baseline offset. Uses the
    /// shaper's font metrics so it matches the rasterized glyphs.
    fn ctx_ascent(&self, family: FontFamily, px: f32) -> f32 {
        let bytes = crate::fonts::face_bytes(family, FaceStyle::Regular);
        FontRef::from_index(bytes, 0).map_or(px * 0.8, |font| {
            let m = font.metrics(&[]);
            let upem = f32::from(m.units_per_em).max(1.0);
            m.ascent / upem * px
        })
    }
}

/// The PROSE front-end over the shared [`GlyphAtlas`] - the analogue of
/// [`crate::grid_render::GridRenderer`] for proportional text. It owns its OWN glyph
/// instance buffer (so the grid's rebuild-gate buffer is never touched) and draws every
/// run through the one shared glyph pipeline. For T-4.3 this is the tested render path;
/// composing it into the live timeline / agent cards is T-4.6.
///
/// NOTE for T-4.6: unlike [`crate::grid_render::GridRenderer`], [`Self::prepare`] has no
/// rebuild/damage gate - it re-walks the layout and re-uploads its instance buffer every
/// call (warm, that is alloc-free but not zero-work). Before driving prose from the live
/// per-frame loop, add the same `(layout-version, origin, color, viewport)` early-out the
/// grid has, so an idle agent card costs nothing per vsync.
///
/// `pub` (like `GridRenderer`) because it is a renderer building block T-4.6 wires up;
/// the atlas it draws into is supplied per call, so multiple prose blocks share one.
pub struct ProseRenderer {
    glyph_instances: Vec<GlyphInstance>,
    glyph_buf: InstanceBuffer,
}

impl ProseRenderer {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            glyph_instances: Vec::new(),
            glyph_buf: InstanceBuffer::new(
                device,
                "aterm-prose-instances",
                size_of::<GlyphInstance>(),
                256,
            ),
        }
    }

    /// Build (and upload) the glyph instances for `layout` placed at logical `origin`
    /// (the layout's top-left, physical px) in linear `color`, acquiring each shaped
    /// glyph from the shared `atlas`. The per-glyph quad is the rasterized bitmap offset
    /// from the pen by the glyph's `(left, top)` and SNAPPED to integer pixels (so it
    /// maps 1:1 under the atlas's Nearest sampler - the same crispness discipline the
    /// grid uses, and what keeps prose glyphs free of atlas-neighbor bleed). Returns the
    /// instance count (== glyphs that produced ink; inkless glyphs are skipped).
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        layout: &ProseLayout,
        origin: (f32, f32),
        color: [f32; 4],
    ) -> usize {
        self.glyph_instances.clear();
        // Round the size ONCE and feed it to BOTH the cache key and the rasterizer, so a
        // glyph is keyed at the exact integer px it is rasterized at (the GlyphKey
        // one-raster-per-(family,glyph,face,px) invariant). `layout.px` is already
        // rounded by ProseShaper::layout; this is the defensive single source at the
        // render boundary, mirroring the grid which rounds px once.
        let px = layout.px.round().max(1.0);
        let px_key = px as u32;
        let inv = 1.0 / atlas.atlas_dim() as f32;
        for pg in &layout.glyphs {
            let key = GlyphKey {
                family: layout.family,
                glyph_id: pg.glyph_id,
                face: layout.face,
                px: px_key,
                sprite: false,
            };
            let Some((rect, (left, top))) =
                atlas.acquire_font(queue, key, layout.family, layout.face, pg.glyph_id, px)
            else {
                continue;
            };
            let gx = (origin.0 + pg.pen_x + left as f32).round();
            let gy = (origin.1 + pg.baseline - top as f32).round();
            self.glyph_instances.push(GlyphInstance {
                rect: [gx, gy, rect.w as f32, rect.h as f32],
                uv: [
                    rect.x as f32 * inv,
                    rect.y as f32 * inv,
                    (rect.x + rect.w) as f32 * inv,
                    (rect.y + rect.h) as f32 * inv,
                ],
                color,
            });
        }
        if !self.glyph_instances.is_empty() {
            self.glyph_buf.ensure(
                device,
                "aterm-prose-instances",
                size_of::<GlyphInstance>(),
                self.glyph_instances.len(),
            );
            queue.write_buffer(
                self.glyph_buf.buf(),
                0,
                bytemuck::cast_slice(&self.glyph_instances),
            );
        }
        self.glyph_instances.len()
    }

    /// Record this prose run's single instanced glyph draw into `pass` through the
    /// shared `atlas` (the caller has set the atlas viewport for this surface size).
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, atlas: &GlyphAtlas) {
        if !self.glyph_instances.is_empty() {
            atlas.draw_glyphs(pass, &self.glyph_buf, self.glyph_instances.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ca(advance: f32, breakable: bool) -> ClusterAdvance {
        ClusterAdvance { advance, breakable }
    }

    // ----- wrap_lines (pure) ----------------------------------------------

    #[test]
    fn wrap_keeps_a_fitting_line_whole() {
        // Three 10-wide words + 2 spaces well under a 100 measure: one line.
        let cs = [
            ca(10.0, false),
            ca(5.0, true),
            ca(10.0, false),
            ca(5.0, true),
            ca(10.0, false),
        ];
        assert_eq!(wrap_lines(&cs, 100.0), vec![0..5]);
    }

    #[test]
    fn wrap_breaks_at_the_last_whitespace_before_overflow() {
        // "AAAA BBBB CCCC": each word 40 wide, space 10. measure 95 fits "AAAA BBBB "
        // (40+10+40 = 90 <= 95) but "CCCC" (->140) overflows -> break after the 2nd
        // space (index 3). Line 1 = clusters 0..4, line 2 = 4..5.
        let cs = [
            ca(40.0, false),
            ca(10.0, true),
            ca(40.0, false),
            ca(10.0, true),
            ca(40.0, false),
        ];
        assert_eq!(wrap_lines(&cs, 95.0), vec![0..4, 4..5]);
    }

    #[test]
    fn wrap_overflows_a_single_word_longer_than_the_measure() {
        // One unbreakable 200-wide word with a 50 measure: no break opportunity, so it
        // overflows onto one line rather than splitting mid-word.
        let cs = [ca(200.0, false)];
        assert_eq!(wrap_lines(&cs, 50.0), vec![0..1]);
    }

    #[test]
    fn wrap_produces_multiple_lines_for_a_long_run() {
        // Ten 20-wide words separated by 5-wide spaces, measure 50: each line holds
        // ~two words. Expect several lines, none trivially empty.
        let mut cs = Vec::new();
        for i in 0..10 {
            if i > 0 {
                cs.push(ca(5.0, true));
            }
            cs.push(ca(20.0, false));
        }
        let lines = wrap_lines(&cs, 50.0);
        assert!(
            lines.len() >= 4,
            "a long run wraps to several lines (got {lines:?})"
        );
        // Lines partition the clusters with no gap or overlap.
        assert_eq!(lines[0].start, 0);
        assert_eq!(lines.last().unwrap().end, cs.len());
        for w in lines.windows(2) {
            assert_eq!(w[0].end, w[1].start, "ranges are contiguous");
        }
    }

    #[test]
    fn wrap_of_empty_is_empty() {
        assert!(wrap_lines(&[], 100.0).is_empty());
    }

    // ----- measure_px (pure, real fonts) ----------------------------------

    #[test]
    fn measure_is_chars_times_zero_advance() {
        // Duo '0' is 0.6em, so a 72ch measure at 14px is ~ 72 * 0.6 * 14.
        let m = measure_px(FontFamily::Prose, MEASURE_CH, 14.0);
        let expected = f32::from(MEASURE_CH) * 0.6 * 14.0;
        assert!(
            (m - expected).abs() < 1.0,
            "Duo measure {m:.2} ~= {expected:.2} (72ch * 0.6em * 14px)"
        );
    }

    // ----- layout-level shaping (real fonts, no GPU) ----------------------

    fn shaper() -> ProseShaper {
        ProseShaper::new()
    }

    #[test]
    fn duo_is_duospace_wide_glyphs_take_more_room() {
        // The defining Duo property: m/w/M/W are 1.5x the others. A run of 'm' must lay
        // out wider than the same count of 'i', proving genuine variable advance (not a
        // constant grid). One line, no wrap (huge measure).
        let mut s = shaper();
        let mm = s.layout(
            "mmmm",
            FontFamily::Prose,
            FaceStyle::Regular,
            28.0,
            1e6,
            42.0,
        );
        let ii = s.layout(
            "iiii",
            FontFamily::Prose,
            FaceStyle::Regular,
            28.0,
            1e6,
            42.0,
        );
        assert_eq!(mm.line_count, 1);
        assert_eq!(ii.line_count, 1);
        assert!(
            mm.width > ii.width * 1.3,
            "Duo 'mmmm' ({:.1}) must be markedly wider than 'iiii' ({:.1})",
            mm.width,
            ii.width
        );
    }

    #[test]
    fn quattro_spans_four_widths() {
        // Quattro: i (0.3em) < a (0.6em) < m (0.9em). Compare equal-count runs.
        let mut s = shaper();
        let w = |s: &mut ProseShaper, t: &str| {
            s.layout(t, FontFamily::Ui, FaceStyle::Regular, 28.0, 1e6, 42.0)
                .width
        };
        let wi = w(&mut s, "iiii");
        let wa = w(&mut s, "aaaa");
        let wm = w(&mut s, "mmmm");
        assert!(wi < wa, "Quattro 'i' (0.3em) narrower than 'a' (0.6em)");
        assert!(wa < wm, "Quattro 'a' (0.6em) narrower than 'm' (0.9em)");
    }

    #[test]
    fn prose_wraps_at_the_measure_and_grid_is_uncapped() {
        // A paragraph far longer than a 72ch Duo measure wraps to several lines, and no
        // line's content width exceeds the measure (every word here is short). The grid
        // has no measure at all - this front-end is the only one that wraps.
        let mut s = shaper();
        let px = 14.0;
        let measure = measure_px(FontFamily::Prose, MEASURE_CH, px);
        let para = "the quick brown fox jumps over the lazy dog and then keeps on \
                    running across the wide open field until the sun goes down again";
        // Repeat to comfortably exceed one 72ch line.
        let text = format!("{para} {para} {para}");
        let layout = s.layout(
            &text,
            FontFamily::Prose,
            FaceStyle::Regular,
            px,
            measure,
            px * 1.5,
        );
        assert!(
            layout.line_count > 1,
            "a >72ch paragraph wraps (got {} lines)",
            layout.line_count
        );
        assert!(
            layout.width <= measure + 1.0,
            "no wrapped line exceeds the measure ({:.1} <= {:.1})",
            layout.width,
            measure
        );
        // ...and the filled lines actually approach the measure (a regression that
        // wrapped at, say, half the measure would still pass the bound above).
        assert!(
            layout.width > measure * 0.7,
            "wrapped lines fill toward the ~72ch measure ({:.1} > {:.1})",
            layout.width,
            measure * 0.7
        );

        // The other half of AC2, pinned behaviorally: the terminal GRID has no measure.
        // A 200-column row emits all 200 cells (build_grid_cells never caps), unlike
        // prose which wrapped above. The two front-ends differ exactly here.
        let theme = aterm_tokens::Theme::for_kind(aterm_tokens::ThemeKind::Dark);
        let snap = aterm_core::Snapshot::empty(1, 200);
        let mut cells = Vec::new();
        crate::text::build_grid_cells(&snap, theme, &mut cells);
        assert_eq!(
            cells.len(),
            200,
            "the grid is uncapped: a 200-column row emits all 200 cells, no 72ch cap"
        );
    }

    #[test]
    fn layout_rounds_px_so_shaping_and_rasterization_share_one_integer_size() {
        // The render path keys the atlas on an integer px and rasterizes at that px; if
        // layout left px fractional the two could disagree. layout rounds once at the
        // source, so layout.px is always integer-valued (here 13.6 -> 14).
        let mut s = shaper();
        let layout = s.layout("ab", FontFamily::Prose, FaceStyle::Regular, 13.6, 1e6, 21.0);
        assert_eq!(layout.px, 14.0, "layout rounds the size to an integer px");
    }

    #[test]
    fn an_overlong_token_overflows_the_measure() {
        // Documented greedy-no-hyphenation behavior: a single unbreakable token wider
        // than the measure overflows onto one line, so layout.width EXCEEDS the measure
        // (the ProseLayout.width contract a sizing caller must respect).
        let mut s = shaper();
        let px = 14.0;
        let long = "supercalifragilisticexpialidocious";
        let measure = measure_px(FontFamily::Prose, 8, px); // ~8ch, far narrower than the word
        let layout = s.layout(
            long,
            FontFamily::Prose,
            FaceStyle::Regular,
            px,
            measure,
            px * 1.5,
        );
        assert_eq!(
            layout.line_count, 1,
            "an unbreakable token is not split mid-word"
        );
        assert!(
            layout.width > measure,
            "the overlong token overflows the measure ({:.1} > {:.1})",
            layout.width,
            measure
        );
    }

    #[test]
    fn hard_newlines_force_line_breaks_and_blank_lines_take_space() {
        let mut s = shaper();
        let layout = s.layout(
            "alpha\n\nbeta",
            FontFamily::Prose,
            FaceStyle::Regular,
            20.0,
            1e6,
            30.0,
        );
        // "alpha", blank, "beta" = 3 lines; the middle blank still advances the pen down.
        assert_eq!(layout.line_count, 3, "two words + a blank line = 3 lines");
        // The two text lines' baselines differ by 2 * line_height (the blank between).
        let ys: Vec<f32> = {
            let mut v: Vec<f32> = layout.glyphs.iter().map(|g| g.baseline).collect();
            v.dedup();
            v
        };
        assert_eq!(ys.len(), 2, "glyphs sit on two distinct baselines");
        assert!(
            (ys[1] - ys[0] - 60.0).abs() < 0.01,
            "the blank line pushes 'beta' two line-heights down (got {:.1})",
            ys[1] - ys[0]
        );
    }

    // ----- measured metrics (AC3: pin the table to the live faces) --------

    #[test]
    fn prose_metrics_match_the_documented_table() {
        // Re-derive the advance table from the bundled faces so the module docs cannot
        // drift. em-fraction = advance_width(gid) / units_per_em.
        let adv_em = |family: FontFamily, ch: char| -> f32 {
            let bytes = crate::fonts::face_bytes(family, FaceStyle::Regular);
            let font = FontRef::from_index(bytes, 0).unwrap();
            let upem = f32::from(font.metrics(&[]).units_per_em);
            font.glyph_metrics(&[])
                .advance_width(font.charmap().map(ch))
                / upem
        };
        let avg_em = |family: FontFamily| -> f32 {
            let bytes = crate::fonts::face_bytes(family, FaceStyle::Regular);
            let font = FontRef::from_index(bytes, 0).unwrap();
            let m = font.metrics(&[]);
            m.average_width / f32::from(m.units_per_em)
        };
        let close = |a: f32, b: f32| (a - b).abs() < 0.01;

        // Every cell of the documented table is pinned here, so the docs cannot drift.
        // Mono: constant 0.667em across the board.
        for ch in [
            ' ', 'i', 'l', 'r', 'f', 's', 'a', '0', 'n', 'm', 'w', 'M', 'W',
        ] {
            assert!(
                close(adv_em(FontFamily::Grid, ch), 0.667),
                "Mono {ch:?} must be the constant 0.667em advance"
            );
        }
        // Duo: 0.6em for everything except m/w/M/W (0.9em = 1.5x).
        for ch in [' ', 'i', 'l', 'r', 'f', 's', 'a', '0', 'n'] {
            assert!(
                close(adv_em(FontFamily::Prose, ch), 0.6),
                "Duo {ch:?} is 0.6em"
            );
        }
        for ch in ['m', 'w', 'M', 'W'] {
            assert!(
                close(adv_em(FontFamily::Prose, ch), 0.9),
                "Duo {ch:?} is 0.9em (1.5x)"
            );
        }
        // Quattro: the four widths 0.3 / 0.45 / 0.6 / 0.9, every documented cell.
        for ch in ['i', 'l'] {
            assert!(
                close(adv_em(FontFamily::Ui, ch), 0.3),
                "Quattro {ch:?} is 0.3em"
            );
        }
        for ch in [' ', 'r', 'f'] {
            assert!(
                close(adv_em(FontFamily::Ui, ch), 0.45),
                "Quattro {ch:?} is 0.45em"
            );
        }
        for ch in ['s', 'a', '0', 'n'] {
            assert!(
                close(adv_em(FontFamily::Ui, ch), 0.6),
                "Quattro {ch:?} is 0.6em"
            );
        }
        for ch in ['m', 'w', 'M', 'W'] {
            assert!(
                close(adv_em(FontFamily::Ui, ch), 0.9),
                "Quattro {ch:?} is 0.9em"
            );
        }
        // The documented average-width row (Mono 0.667 / Duo 0.874 / Quattro 0.873).
        assert!(
            close(avg_em(FontFamily::Grid), 0.667),
            "Mono average is 0.667em"
        );
        assert!(
            close(avg_em(FontFamily::Prose), 0.874),
            "Duo average is 0.874em"
        );
        assert!(
            close(avg_em(FontFamily::Ui), 0.873),
            "Quattro average is 0.873em"
        );

        // Shared vertical metrics across all three registers.
        for family in [FontFamily::Grid, FontFamily::Prose, FontFamily::Ui] {
            let bytes = crate::fonts::face_bytes(family, FaceStyle::Regular);
            let m = FontRef::from_index(bytes, 0).unwrap().metrics(&[]);
            assert_eq!(m.units_per_em, 1000, "{family:?} upem");
            assert!((m.ascent - 1025.0).abs() < 0.5, "{family:?} ascent");
            assert!((m.descent - 275.0).abs() < 0.5, "{family:?} descent");
            assert!((m.cap_height - 698.0).abs() < 0.5, "{family:?} cap_height");
            assert!((m.x_height - 516.0).abs() < 0.5, "{family:?} x_height");
        }
    }
}

// The prose render path draws to a real GPU through the shared atlas, so it is verified
// by rendering offscreen and reading pixels back - macOS-only (a Metal device), skipping
// when no adapter is present, exactly like the grid GPU tests. These cover AC1 (Duo +
// Quattro load and render), AC2's wrap-on-screen, and the shared-atlas property.
#[cfg(all(test, target_os = "macos"))]
mod gpu_tests {
    use super::*;
    use crate::atlas::GlyphAtlas;

    const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

    fn device() -> Option<(wgpu::Device, wgpu::Queue, wgpu::TextureFormat)> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("aterm-prose-test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .ok()?;
        Some((device, queue, wgpu::TextureFormat::Rgba8UnormSrgb))
    }

    struct Readback {
        data: Vec<u8>,
        stride: usize,
        w: u32,
        h: u32,
    }
    impl Readback {
        fn lum(&self, x: u32, y: u32) -> u8 {
            let o = y as usize * self.stride + x as usize * 4;
            self.data[o].max(self.data[o + 1]).max(self.data[o + 2])
        }
        /// Whether any pixel in the half-open box exceeds `thresh` on any RGB channel.
        fn any_ink(&self, x0: u32, y0: u32, x1: u32, y1: u32, thresh: u8) -> bool {
            (y0..y1.min(self.h)).any(|y| (x0..x1.min(self.w)).any(|x| self.lum(x, y) > thresh))
        }
    }

    /// Render `prose` (already prepared against `atlas`) into a `w` x `h` black target
    /// and read it back.
    fn render(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &GlyphAtlas,
        prose: &ProseRenderer,
        w: u32,
        h: u32,
    ) -> Readback {
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("prose-test-target"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let stride = ((w * 4).div_ceil(256) * 256) as usize;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("prose-test-readback"),
            size: (stride as u32 * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        atlas.set_viewport(queue, w, h);
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("prose-test-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            prose.draw(&mut pass, atlas);
        }
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride as u32),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(enc.finish()));
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
        let data = slice.get_mapped_range().to_vec();
        Readback { data, stride, w, h }
    }

    #[test]
    fn duo_prose_run_inks_through_the_shared_atlas() {
        let Some((device, queue, format)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut prose = ProseRenderer::new(&device);
        let mut shaper = ProseShaper::new();
        let px = 28.0;
        let layout = shaper.layout(
            "hello",
            FontFamily::Prose,
            FaceStyle::Regular,
            px,
            1e6,
            px * 1.5,
        );
        let origin = (4.0, 4.0);
        let n = prose.prepare(&device, &queue, &mut atlas, &layout, origin, WHITE);
        assert!(n >= 4, "'hello' produces several inked glyphs (got {n})");
        let (w, h) = ((layout.width as u32) + 16, (px * 2.0) as u32);
        let rb = render(&device, &queue, &atlas, &prose, w, h);
        assert!(
            rb.any_ink(0, 0, w, h, 60),
            "the Duo prose run inks the target"
        );
    }

    #[test]
    fn quattro_label_inks_through_the_shared_atlas() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut prose = ProseRenderer::new(&device);
        let mut shaper = ProseShaper::new();
        let px = 22.0;
        // A chrome label in Quattro (the UI register).
        let layout = shaper.layout("RUN", FontFamily::Ui, FaceStyle::Bold, px, 1e6, px * 1.3);
        let n = prose.prepare(&device, &queue, &mut atlas, &layout, (4.0, 4.0), WHITE);
        assert!(n >= 3, "'RUN' produces three inked glyphs (got {n})");
        let (w, h) = ((layout.width as u32) + 16, (px * 2.0) as u32);
        let rb = render(&device, &queue, &atlas, &prose, w, h);
        assert!(
            rb.any_ink(0, 0, w, h, 60),
            "the Quattro label inks the target"
        );
    }

    #[test]
    fn prose_wraps_to_a_second_inked_line_on_screen() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut prose = ProseRenderer::new(&device);
        let mut shaper = ProseShaper::new();
        let px = 20.0;
        let lh = px * 1.5;
        // A small measure forces several short words to wrap onto a second line.
        let measure = 90.0;
        let layout = shaper.layout(
            "alpha beta gamma delta epsilon",
            FontFamily::Prose,
            FaceStyle::Regular,
            px,
            measure,
            lh,
        );
        assert!(
            layout.line_count >= 2,
            "the run wraps (got {} lines)",
            layout.line_count
        );
        let origin = (4.0, 4.0);
        prose.prepare(&device, &queue, &mut atlas, &layout, origin, WHITE);
        let (w, h) = (measure as u32 + 16, (origin.1 + layout.height + lh) as u32);
        let rb = render(&device, &queue, &atlas, &prose, w, h);
        // Line 1 inks in the first line band; line 2 inks BELOW one line height - proof
        // the wrap put glyphs on a second baseline on screen.
        let l1_top = origin.1 as u32;
        let l1_bot = (origin.1 + lh) as u32;
        let l2_bot = (origin.1 + 2.0 * lh) as u32;
        assert!(rb.any_ink(0, l1_top, w, l1_bot, 60), "line 1 inks");
        assert!(
            rb.any_ink(0, l1_bot, w, l2_bot, 60),
            "the wrapped line 2 inks below the first line"
        );
    }

    #[test]
    fn grid_and_prose_families_coexist_in_one_shared_atlas() {
        // The headline of the extraction: ONE atlas serves both registers. Acquire a
        // GRID-family glyph directly, then render a PROSE-family run through the SAME
        // atlas; both families resolve, the prose inks, and the rasterization count
        // reflects glyphs from both registers (distinct cache keys via `family`).
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let px = 24.0;
        // A grid 'M' (Mono) into the shared atlas.
        let grid_gid = atlas.glyph_id(FontFamily::Grid, FaceStyle::Regular, 'M');
        let grid_key = GlyphKey {
            family: FontFamily::Grid,
            glyph_id: grid_gid,
            face: FaceStyle::Regular,
            px: px as u32,
            sprite: false,
        };
        assert!(
            atlas
                .acquire_font(
                    &queue,
                    grid_key,
                    FontFamily::Grid,
                    FaceStyle::Regular,
                    grid_gid,
                    px
                )
                .is_some(),
            "a Mono grid glyph lives in the shared atlas"
        );
        let grid_only = atlas.rasterizations();

        // Now a Duo prose run through the same atlas.
        let mut prose = ProseRenderer::new(&device);
        let mut shaper = ProseShaper::new();
        let layout = shaper.layout(
            "Mm",
            FontFamily::Prose,
            FaceStyle::Regular,
            px,
            1e6,
            px * 1.5,
        );
        let n = prose.prepare(&device, &queue, &mut atlas, &layout, (4.0, 4.0), WHITE);
        assert!(
            n >= 2,
            "the Duo run inks its glyphs through the shared atlas"
        );
        assert!(
            atlas.rasterizations() > grid_only,
            "prose glyphs add NEW rasterizations alongside the grid glyph (one shared atlas)"
        );
        let (w, h) = ((layout.width as u32) + 16, (px * 2.0) as u32);
        let rb = render(&device, &queue, &atlas, &prose, w, h);
        assert!(
            rb.any_ink(0, 0, w, h, 60),
            "the shared-atlas prose run inks"
        );
    }
}
