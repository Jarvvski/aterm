//! swash glyph rasterization for the terminal grid (ticket T-1.8, the GPU half of
//! T-1.6).
//!
//! This is the rasterization front-end the instanced grid pipeline ([`crate::gpu`])
//! consumes: it turns a `(face, glyph, px)` request into an 8-bit **alpha coverage**
//! bitmap plus its placement relative to the pen, which the pipeline uploads once
//! into the shared atlas (keyed + deduplicated by [`crate::text::GlyphCache`]) and
//! composites with the cell's foreground color by multiplication. Grayscale AA only
//! (macOS dropped LCD subpixel AA in 2018 and the iA aesthetic wants grayscale; see
//! `08-text-glyph-rendering.md` Rec 3). No subpixel-offset variants: the grid
//! renderer SNAPS each glyph quad to an integer pixel origin (the cell advance is
//! fractional, so it rounds), and samples Nearest, so one rasterization per
//! `(glyph, face, px)` is exact (the [`crate::text::GlyphKey`] note).
//!
//! Why swash and not glyphon's whole-buffer reshape: the interim glyphon path
//! re-shaped the ENTIRE grid through cosmic-text on every keystroke (`Shaping::
//! Advanced` PUA fallback measured in *seconds* per keystroke with icon glyphs on
//! screen - the typing-lag diagnosis). swash rasterizes each unique glyph exactly
//! once; the steady-state hot path is an atlas-rect lookup, never a reshape.

use swash::scale::{Render, ScaleContext, Source};
use swash::zeno::Format;
use swash::FontRef;

use crate::fonts::{GRID_BOLD, GRID_BOLD_ITALIC, GRID_ITALIC, GRID_REGULAR};
use crate::text::FaceStyle;

/// A rasterized glyph: its 8-bit alpha coverage and placement relative to the pen
/// origin (the cell's baseline-left point). `left`/`top` follow swash/zeno
/// conventions: `left` is the x offset from the pen, `top` is the y offset of the
/// bitmap's TOP edge **above** the baseline (positive = up).
#[derive(Debug, Clone)]
pub struct RasterGlyph {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    /// `width * height` coverage bytes, row-major, 1 byte per texel.
    pub coverage: Vec<u8>,
}

impl RasterGlyph {
    /// Whether this glyph has any drawable pixels (a space / `.notdef` with no
    /// outline rasterizes to an empty image - the pipeline skips it, emitting no
    /// glyph instance for that cell).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0 || self.coverage.is_empty()
    }
}

/// Pixel cell metrics derived from the Regular face at a given pixel size: the
/// horizontal advance and the vertical baseline placement within the line box. The
/// renderer uses these to position glyphs; the cell BOX itself comes from
/// [`crate::window::cell_px`] (the same metric that sizes the PTY grid), so the two
/// never drift.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellMetrics {
    /// Advance width of one cell in px (the font's design advance at this size).
    pub advance: f32,
    /// Distance from the baseline up to the ascent top, in px.
    pub ascent: f32,
    /// Distance from the baseline down to the descent bottom, in px (positive).
    pub descent: f32,
    /// The font's natural line height (ascent + descent + leading) in px.
    pub line: f32,
}

/// Holds the swash scaler context and the bundled face data, and rasterizes glyphs
/// on demand. Lives on the render thread (the `ScaleContext` is single-threaded);
/// the `GlyphCache` in front of it guarantees each glyph is rasterized at most once.
pub struct GlyphRasterizer {
    ctx: ScaleContext,
}

impl Default for GlyphRasterizer {
    fn default() -> Self {
        Self::new()
    }
}

impl GlyphRasterizer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ctx: ScaleContext::new(),
        }
    }

    /// The bundled TTF bytes for a face style.
    #[must_use]
    fn face_bytes(face: FaceStyle) -> &'static [u8] {
        match face {
            FaceStyle::Regular => GRID_REGULAR,
            FaceStyle::Bold => GRID_BOLD,
            FaceStyle::Italic => GRID_ITALIC,
            FaceStyle::BoldItalic => GRID_BOLD_ITALIC,
        }
    }

    /// Parse a face's font directory into a `FontRef` (cheap - it only reads the
    /// table offsets; the heavy outline scaling is cached downstream). Returns
    /// `None` only if the bundled bytes fail to parse, which the build-time
    /// `bundled_faces_are_nonempty` test guards against.
    fn font(face: FaceStyle) -> Option<FontRef<'static>> {
        FontRef::from_index(Self::face_bytes(face), 0)
    }

    /// Map a character to its glyph id in `face`, WITHOUT shaping (the constant-
    /// advance grid fast-path: a direct cmap lookup, no HarfBuzz run). Returns `0`
    /// (`.notdef`) for a codepoint the face lacks - the renderer draws whatever the
    /// face provides (tofu for, e.g., CJK in a Latin face; real font fallback is a
    /// later text-polish pass).
    #[must_use]
    pub fn glyph_id(&self, face: FaceStyle, ch: char) -> u16 {
        Self::font(face).map_or(0, |f| f.charmap().map(ch))
    }

    /// Cell metrics from the Regular face at `px`. Regular is authoritative so bold/
    /// italic cells (which can have a different advance) still align to the grid.
    #[must_use]
    pub fn cell_metrics(&self, px: f32) -> CellMetrics {
        let Some(font) = Self::font(FaceStyle::Regular) else {
            return CellMetrics {
                advance: px * 0.6,
                ascent: px * 0.8,
                descent: px * 0.2,
                line: px,
            };
        };
        let m = font.metrics(&[]);
        let upem = f32::from(m.units_per_em).max(1.0);
        let s = px / upem;
        CellMetrics {
            advance: m.average_width * s,
            ascent: m.ascent * s,
            descent: m.descent * s,
            line: (m.ascent + m.descent + m.leading) * s,
        }
    }

    /// Rasterize `(face, glyph_id)` at `px` into an 8-bit alpha coverage bitmap.
    /// Returns `None` if the face fails to parse or swash produces no image; an
    /// outline-less glyph (space) returns `Some` with a zero-size image
    /// ([`RasterGlyph::is_empty`]).
    #[must_use]
    pub fn rasterize(&mut self, face: FaceStyle, glyph_id: u16, px: f32) -> Option<RasterGlyph> {
        let font = Self::font(face)?;
        let mut scaler = self.ctx.builder(font).size(px).hint(true).build();
        let image = Render::new(&[Source::Outline])
            .format(Format::Alpha)
            .render(&mut scaler, glyph_id)?;
        Some(RasterGlyph {
            left: image.placement.left,
            top: image.placement.top,
            width: image.placement.width,
            height: image.placement.height,
            coverage: image.data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grid pixel size at 1x scale (GRID type style is 13pt).
    const PX: f32 = 13.0;

    #[test]
    fn ascii_glyph_ids_are_present_and_distinct() {
        let r = GlyphRasterizer::new();
        let a = r.glyph_id(FaceStyle::Regular, 'A');
        let b = r.glyph_id(FaceStyle::Regular, 'B');
        assert_ne!(a, 0, "the Latin face must have 'A'");
        assert_ne!(b, 0, "the Latin face must have 'B'");
        assert_ne!(a, b, "distinct chars map to distinct glyphs");
    }

    #[test]
    fn missing_codepoint_maps_to_notdef() {
        let r = GlyphRasterizer::new();
        // A Latin mono face has no CJK glyph -> .notdef (0). It still LAYS OUT (the
        // wide flag drives the 2-column advance); real fallback rendering is later.
        assert_eq!(r.glyph_id(FaceStyle::Regular, '\u{4e2d}'), 0);
    }

    #[test]
    fn rasterize_letter_has_coverage() {
        let mut r = GlyphRasterizer::new();
        let gid = r.glyph_id(FaceStyle::Regular, 'M');
        let g = r
            .rasterize(FaceStyle::Regular, gid, PX)
            .expect("M rasterizes");
        assert!(!g.is_empty(), "'M' has drawable pixels");
        assert_eq!(
            g.coverage.len(),
            (g.width * g.height) as usize,
            "alpha image is width*height bytes (1 byte/texel)"
        );
        assert!(
            g.coverage.iter().any(|&a| a > 0),
            "at least one texel is inked"
        );
    }

    #[test]
    fn space_rasterizes_empty() {
        let mut r = GlyphRasterizer::new();
        let gid = r.glyph_id(FaceStyle::Regular, ' ');
        let g = r
            .rasterize(FaceStyle::Regular, gid, PX)
            .expect("space scales");
        assert!(
            g.is_empty(),
            "a space has no ink -> no glyph instance emitted"
        );
    }

    #[test]
    fn cell_metrics_are_positive_and_scale() {
        let r = GlyphRasterizer::new();
        let m = r.cell_metrics(PX);
        assert!(m.advance > 0.0 && m.ascent > 0.0 && m.descent > 0.0 && m.line > 0.0);
        // Doubling px doubles the metrics (linear scale).
        let m2 = r.cell_metrics(PX * 2.0);
        assert!((m2.ascent - m.ascent * 2.0).abs() < 0.01);
    }

    #[test]
    fn bold_and_regular_share_glyph_coverage_shape() {
        // Bold 'M' should still rasterize to a non-empty image (a distinct face).
        let mut r = GlyphRasterizer::new();
        let gid = r.glyph_id(FaceStyle::Bold, 'M');
        let g = r
            .rasterize(FaceStyle::Bold, gid, PX)
            .expect("bold M rasterizes");
        assert!(!g.is_empty());
    }
}
