//! Per-codepoint Nerd Font constraint table (ticket T-4.4).
//!
//! Nerd Font icon glyphs live in the Private Use Area and are patched in at sizes
//! that do not match an arbitrary base font's cell box, so without correction they
//! render "small, squished, or not full width" (see `08-text-glyph-rendering.md`
//! §3). The fix is a per-codepoint *constraint* describing how to fit each glyph
//! into the grid cell: scale it (fit / cover / stretch), then align it. This module
//! owns that table ([`lookup`]) plus the pure geometry that applies a constraint to
//! a rasterized glyph's box ([`Constraint::place`]). It is GPU-free and exhaustively
//! unit-tested (the crate's "pure logic, no window" rule); the grid renderer
//! ([`crate::grid_render`]) consumes it when placing a font glyph quad.
//!
//! ## Provenance + license (the ticket's documentation AC)
//!
//! - **Codepoint ranges** were enumerated from the *actual bundled* face's covered
//!   charset (`fc-query --format='%{charset}' iMWritingMonoNerdFontMono-Regular.ttf`)
//!   and grouped per the public Nerd Fonts v3 patcher category boundaries: Pomicons
//!   `E000`, Powerline `E0A0..E0D7`, Font-Awesome-Extension `E200`, Weather `E300`,
//!   Seti-UI/Custom `E5FA`, Devicons `E700`, Codicons `EA60`, Font Awesome /
//!   Font-Logos `ED00..F381`, Octicons `F400`, and Material Design Icons in the SMP
//!   PUA at `U+F0001..F1AF0` (the beyond-BMP range the ticket calls out). IEC power
//!   (`23FB..23FE`, `2B58`) and the Powerline trigram (`2630`) round out the set the
//!   dossier names.
//! - **The constraint model** (`fit` / `cover` / `stretch` sizing + alignment) is an
//!   independent reimplementation of the *approach* Ghostty documents for its
//!   generated `getConstraint(cp)` table (`08-text-glyph-rendering.md` §3). No
//!   third-party code or generated data is vendored - the directives below are
//!   authored for aterm against the bundled font - so this carries no upstream
//!   license obligation beyond aterm's own GPLv3 and the font's OFL (see
//!   `assets/fonts/OFL-LICENSE.md`).
//! - **Not exhaustive by design.** Ghostty's codegen emits thousands of per-glyph
//!   entries; the patcher groups them by category with shared scale rules, which is
//!   the granularity reproduced here (one directive per category range). It covers
//!   the bundled face's PUA; a future regeneration can refine individual glyphs.
//!
//! Box-drawing, block elements, braille, and the Powerline *triangle* separators
//! (`E0B0..E0B3`) are drawn by the procedural sprite face ([`crate::sprite`], ticket
//! T-4.5): the grid renderer intercepts them upstream so they bypass the font, and
//! [`lookup`] returns `None` for them so the table is provably disjoint from the
//! sprite ranges (the `sprite_codepoints_are_never_constrained` invariant).
//!
//! ## Rasterization-quality note (a deliberate follow-up)
//!
//! The renderer rasterizes an icon at the font's native grid pixel size and this
//! constraint then scales the resulting *quad*; under the grid's Nearest sampler a
//! small PUA icon enlarged to the cell therefore samples blocky (and a shrunk one
//! aliases). Re-rasterizing constrained PUA glyphs at their placed pixel size, so the
//! sampler maps 1:1, is a clean future refinement - out of scope for this table,
//! which is about correct sizing/alignment, not sub-pixel sharpness.

/// How a glyph is scaled along one axis to meet its cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sizing {
    /// Leave the glyph at its natural rasterized size (no scaling).
    None,
    /// Scale (up or down) so the whole glyph fits inside the cell, preserving
    /// aspect ratio. This is what enlarges a patched-small icon to fill the cell.
    Fit,
    /// Scale so the glyph covers the cell, preserving aspect ratio (may overflow
    /// the non-binding axis).
    Cover,
    /// Scale each axis independently to fill the cell exactly (distorts aspect).
    /// Used for Powerline separators, which must butt edge-to-edge with no gap.
    Stretch,
}

/// Where a glyph sits within its cell along one axis once scaled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// Left / top edge of the available box.
    Start,
    /// Centered in the available box.
    Center,
    /// Right / bottom edge of the available box.
    End,
}

/// A glyph-placement box relative to the cell origin, in physical px: the renderer
/// draws the glyph quad at `(x, y)` with size `(w, h)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// How one Nerd Font glyph is fitted into the grid cell: a sizing mode, alignment
/// per axis, and a fractional padding inset (of the cell) on each axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Constraint {
    /// How the glyph is scaled to the cell. A single mode, not per-axis: `Fit` and
    /// `Cover` are inherently aspect-preserving (one uniform scale) and `Stretch`
    /// inherently fills both axes, so a per-axis sizing field would advertise control
    /// the geometry cannot honor. Alignment, by contrast, IS meaningfully per-axis.
    pub sizing: Sizing,
    pub align_x: Align,
    pub align_y: Align,
    /// Padding inset per side as a fraction of the cell width (`0.0..0.5`).
    pub pad_x: f32,
    /// Padding inset per side as a fraction of the cell height (`0.0..0.5`).
    pub pad_y: f32,
}

impl Constraint {
    /// Place a glyph whose natural rasterized box is `glyph_w` x `glyph_h` (px) into
    /// a `cell_w` x `cell_h` (px) cell, returning the quad to draw (relative to the
    /// cell origin). Pure geometry - the heart of the constraint application.
    ///
    /// `Fit`/`Cover` preserve aspect by deriving one uniform scale from both axes;
    /// `Stretch` scales the two axes independently to fill; `None` keeps the natural
    /// size. The scaled box is then aligned inside the padded cell.
    #[must_use]
    pub fn place(&self, glyph_w: f32, glyph_h: f32, cell_w: f32, cell_h: f32) -> Placed {
        let gw = glyph_w.max(1.0);
        let gh = glyph_h.max(1.0);
        // The available box inside the per-side padding.
        let pad_x = self.pad_x.clamp(0.0, 0.49);
        let pad_y = self.pad_y.clamp(0.0, 0.49);
        let avail_w = (cell_w * (1.0 - 2.0 * pad_x)).max(1.0);
        let avail_h = (cell_h * (1.0 - 2.0 * pad_y)).max(1.0);
        let off_x = cell_w * pad_x;
        let off_y = cell_h * pad_y;

        let (w, h) = match self.sizing {
            // Fill both axes independently (distorts aspect) - the Powerline tiling
            // case: the glyph must butt edge-to-edge against its neighbors.
            Sizing::Stretch => (avail_w, avail_h),
            // Aspect-preserving scale: one uniform factor from the binding axis. Fit
            // scales (up or down) so the whole glyph fits; Cover so it fills.
            Sizing::Fit => {
                let s = (avail_w / gw).min(avail_h / gh);
                (gw * s, gh * s)
            }
            Sizing::Cover => {
                let s = (avail_w / gw).max(avail_h / gh);
                (gw * s, gh * s)
            }
            // Natural rasterized size.
            Sizing::None => (gw, gh),
        };

        let x = off_x + align_offset(avail_w, w, self.align_x);
        let y = off_y + align_offset(avail_h, h, self.align_y);
        Placed { x, y, w, h }
    }
}

/// Offset of a `size`-long span inside an `avail`-long box for an alignment.
fn align_offset(avail: f32, size: f32, align: Align) -> f32 {
    match align {
        Align::Start => 0.0,
        Align::Center => (avail - size) * 0.5,
        Align::End => avail - size,
    }
}

/// Scale-to-fit, centered: the directive for the great majority of icon glyphs
/// (Devicons, Font Awesome, Material Design, Seti, Weather, Octicons, Codicons,
/// Pomicons, the Powerline non-separator symbols). A small icon is enlarged to the
/// cell, a large one shrunk, always centered.
const FIT_CENTER: Constraint = Constraint {
    sizing: Sizing::Fit,
    align_x: Align::Center,
    align_y: Align::Center,
    pad_x: 0.0,
    pad_y: 0.0,
};

/// Stretch to fill the whole cell: the directive for the Powerline-Extra seam glyphs,
/// which must tile edge-to-edge against their neighbors with no gap.
const STRETCH_FILL: Constraint = Constraint {
    sizing: Sizing::Stretch,
    align_x: Align::Center,
    align_y: Align::Center,
    pad_x: 0.0,
    pad_y: 0.0,
};

/// The constraint for `ch`, or `None` for an ordinary text codepoint that should use
/// the font's native baseline placement (the overwhelmingly common case, rejected in
/// a single comparison so the ASCII/text hot path pays almost nothing).
///
/// See the module docs for the range provenance. Specific Powerline arms are listed
/// before the broad BMP-PUA arm so they take priority within `E000..F8FF`.
#[must_use]
pub fn lookup(ch: char) -> Option<Constraint> {
    let cp = ch as u32;
    // Below the lowest constrained codepoint: ASCII, Latin, CJK, box-drawing,
    // ordinary symbols - all use native placement. One compare for the hot path.
    if cp < 0x23FB {
        return None;
    }
    Some(match cp {
        // IEC power symbols + the Powerline trigram (named in the dossier).
        0x23FB..=0x23FE | 0x2B58 | 0x2630 => FIT_CENTER,
        // Powerline non-separator symbols (branch, line/column number, padlock).
        0xE0A0..=0xE0A3 => FIT_CENTER,
        // Powerline TRIANGLE separators are the procedural sprite face (T-4.5) and are
        // intercepted upstream; the table leaves them unconstrained so it stays
        // provably disjoint from the sprite ranges (see `sprite_codepoints_are_never_
        // constrained`) and correctness does not rest on caller branch order.
        0xE0B0..=0xE0B3 => return None,
        // Powerline-Extra seam glyphs (half-circles, flames, ice, lego, honeycomb):
        // stretch-fill so they tile edge-to-edge.
        0xE0B4..=0xE0D7 => STRETCH_FILL,
        // Every other BMP Private Use Area glyph (all the icon categories).
        0xE000..=0xF8FF => FIT_CENTER,
        // Supplementary / Supplementary-B PUA: Material Design Icons (U+F0000+) and
        // any other beyond-BMP patched glyphs. Resolving these without panic is an
        // explicit ticket AC.
        0xF0000..=0xFFFFD | 0x10_0000..=0x10_FFFD => FIT_CENTER,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- lookup: range classification -----------------------------------

    #[test]
    fn ordinary_text_is_unconstrained() {
        // ASCII, Latin, CJK, ordinary symbols, and the box-drawing / block / braille
        // ranges (drawn by the sprite face or the font natively) take native
        // placement - no constraint.
        for ch in ['a', ' ', '~', 'Z', '0', '\u{00e9}', '\u{4e2d}'] {
            assert_eq!(lookup(ch), None, "{ch:?} must be unconstrained");
        }
        assert_eq!(lookup('\u{2500}'), None, "box-drawing is not constrained"); // ─
        assert_eq!(lookup('\u{2588}'), None, "block element is not constrained"); // █
        assert_eq!(lookup('\u{2665}'), None, "♥ dingbat is ordinary text");
        assert_eq!(lookup('\u{28FF}'), None, "braille is not constrained");
    }

    #[test]
    fn icon_categories_fit_and_center() {
        // A representative glyph from each BMP icon category resolves to fit+center.
        for cp in [
            0xE000u32, // Pomicons
            0xE0A0,    // Powerline branch symbol
            0xE200,    // Font Awesome Extension
            0xE300,    // Weather
            0xE5FA,    // Seti-UI / Custom
            0xE700,    // Devicons
            0xEA60,    // Codicons
            0xED00,    // Font Awesome (FA6 block)
            0xF000,    // Font Awesome (classic)
            0xF300,    // Font Logos
            0xF400,    // Octicons
            0x23FB,    // IEC power
            0x2630,    // Powerline trigram
        ] {
            let ch = char::from_u32(cp).unwrap();
            assert_eq!(
                lookup(ch),
                Some(FIT_CENTER),
                "U+{cp:04X} should be a fit-centered icon"
            );
        }
    }

    #[test]
    fn powerline_extra_seam_glyphs_stretch_to_fill() {
        // The Powerline-Extra seam glyphs (beyond the B0..B3 triangle sprites)
        // stretch-fill so they tile seamlessly. A few representative points.
        for cp in [0xE0B4u32, 0xE0C0, 0xE0C8, 0xE0D7] {
            let ch = char::from_u32(cp).unwrap();
            assert_eq!(
                lookup(ch),
                Some(STRETCH_FILL),
                "U+{cp:04X} Powerline-Extra seam glyph must stretch-fill"
            );
        }
        // The triangle separators E0B0..=E0B3 belong to the sprite face, so the table
        // leaves them unconstrained (disjointness, not stretch).
        for cp in 0xE0B0u32..=0xE0B3 {
            let ch = char::from_u32(cp).unwrap();
            assert_eq!(
                lookup(ch),
                None,
                "U+{cp:04X} is a sprite, not a table entry"
            );
        }
        // The non-separator symbols just below are fit, not stretch.
        assert_eq!(lookup('\u{E0A2}'), Some(FIT_CENTER));
    }

    #[test]
    fn sprite_codepoints_are_never_constrained() {
        // Invariant: anything the procedural sprite face (T-4.5) draws bypasses the
        // font, so the constraint table must NOT also claim it - otherwise which
        // placement a glyph gets would depend on caller branch order. Cross-check the
        // table against `sprite::is_sprite` across every range the sprite face owns.
        let ranges = (0x2500u32..=0x259F)
            .chain(0x2800..=0x28FF)
            .chain(0xE0B0..=0xE0B3);
        for cp in ranges {
            let ch = char::from_u32(cp).unwrap();
            if crate::sprite::is_sprite(ch) {
                assert_eq!(
                    lookup(ch),
                    None,
                    "sprite U+{cp:04X} must be unconstrained by the table"
                );
            }
        }
    }

    #[test]
    fn material_design_icons_beyond_the_bmp_resolve_without_panic() {
        // The ticket's headline edge case: SMP-PUA codepoints (U+F0000+) must resolve
        // to a constraint, not panic. Material Design Icons span U+F0001..F1AF0 in the
        // bundled face.
        for cp in [0xF0001u32, 0xF0100, 0xF1AF0] {
            let ch = char::from_u32(cp).unwrap();
            assert_eq!(
                lookup(ch),
                Some(FIT_CENTER),
                "MDI U+{cp:05X} resolves to a constraint"
            );
        }
        // The very top of the Supplementary-B PUA still resolves; a non-PUA high
        // codepoint above the table does not.
        assert_eq!(lookup('\u{10FFFD}'), Some(FIT_CENTER));
        assert_eq!(
            lookup('\u{2FFFF}'),
            None,
            "an unassigned high plane is text"
        );
    }

    // ----- place: the constraint geometry ---------------------------------

    #[test]
    fn fit_enlarges_a_small_icon_and_centers_it() {
        // A small square glyph (8x8) in a tall cell (16x34): Fit binds on width
        // (16/8 = 2 < 34/8), so the glyph scales 2x to 16x16, full cell width, and
        // is centered vertically.
        let p = FIT_CENTER.place(8.0, 8.0, 16.0, 34.0);
        assert!(
            (p.w - 16.0).abs() < 1e-3,
            "fit fills the cell width (got {})",
            p.w
        );
        assert!((p.h - 16.0).abs() < 1e-3, "aspect preserved (got {})", p.h);
        assert!((p.x - 0.0).abs() < 1e-3, "centered horizontally at x=0");
        assert!(
            (p.y - 9.0).abs() < 1e-3,
            "centered vertically (got {})",
            p.y
        );
    }

    #[test]
    fn fit_shrinks_an_oversized_icon() {
        // A glyph larger than the cell is scaled DOWN to fit (aspect preserved).
        let p = FIT_CENTER.place(40.0, 40.0, 16.0, 34.0);
        assert!(
            p.w <= 16.0 + 1e-3 && p.h <= 34.0 + 1e-3,
            "fits within the cell"
        );
        assert!(
            (p.w - 16.0).abs() < 1e-3,
            "width-bound: scaled to cell width"
        );
        assert!((p.w - p.h).abs() < 1e-3, "square stays square");
    }

    #[test]
    fn stretch_fills_the_whole_cell() {
        // Stretch ignores aspect and fills the cell exactly (the Powerline tiling
        // property): edge-to-edge, origin at the cell corner.
        let p = STRETCH_FILL.place(8.0, 8.0, 16.0, 34.0);
        assert!((p.w - 16.0).abs() < 1e-3, "fills width");
        assert!((p.h - 34.0).abs() < 1e-3, "fills height");
        assert!(
            (p.x - 0.0).abs() < 1e-3 && (p.y - 0.0).abs() < 1e-3,
            "no offset"
        );
    }

    #[test]
    fn padding_insets_the_available_box() {
        // A constraint with vertical padding shrinks the fit box and offsets it.
        let c = Constraint {
            pad_y: 0.1,
            ..FIT_CENTER
        };
        // 10% padding each side on a 30px-tall cell = 3px inset top and bottom.
        let p = c.place(8.0, 8.0, 16.0, 30.0);
        // avail_h = 24, avail_w = 16; fit binds on width (16/8=2 < 24/8=3) -> 16x16,
        // centered in the padded box: y = off_y(3) + (24-16)/2 = 3 + 4 = 7.
        assert!((p.y - 7.0).abs() < 1e-3, "padded + centered (got {})", p.y);
    }

    #[test]
    fn align_start_and_end_anchor_the_glyph() {
        let start = Constraint {
            align_y: Align::Start,
            ..FIT_CENTER
        };
        let end = Constraint {
            align_y: Align::End,
            ..FIT_CENTER
        };
        let s = start.place(8.0, 8.0, 16.0, 34.0);
        let e = end.place(8.0, 8.0, 16.0, 34.0);
        assert!((s.y - 0.0).abs() < 1e-3, "start anchors to the top");
        assert!(
            (e.y - (34.0 - 16.0)).abs() < 1e-3,
            "end anchors to the bottom"
        );
    }

    #[test]
    fn place_is_robust_to_a_degenerate_glyph_box() {
        // A zero-size glyph box must not divide by zero (callers skip inkless glyphs,
        // but place stays total).
        let p = FIT_CENTER.place(0.0, 0.0, 16.0, 34.0);
        assert!(p.w.is_finite() && p.h.is_finite());
    }
}
