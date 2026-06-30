//! The shared per-cell emitter (ticket T-4.6): turn ONE resolved [`GridCell`] into its
//! background/underline rect + its glyph instance, placed at a caller-given pixel
//! origin, through the shared [`GlyphAtlas`].
//!
//! Both grid-cell front-ends funnel through here so they render IDENTICALLY: the live
//! terminal grid ([`crate::grid_render`]) and the captured-output rows + command lines of
//! the block timeline ([`crate::timeline_render`]). The sprite/Nerd-Font-constraint/
//! baseline placement and the integer-snap-for-crispness discipline live in ONE place,
//! so a box-drawing char or a PUA icon in a finished block's `git diff` output looks
//! exactly like it does in the live grid.
//!
//! Pure geometry + atlas lookups: the caller owns the instance `Vec`s (so each
//! front-end's rebuild gate keeps its own buffers) and decides WHERE each cell sits;
//! this only decides how one cell becomes instances.

use aterm_tokens::Rgba;

use crate::atlas::{GlyphAtlas, GlyphInstance, RectInstance};
use crate::text::{FaceStyle, FontFamily, GlyphKey, GridCell};

/// The per-frame cell geometry shared by every cell in a draw: cell box size (`cw` x
/// `ch`, plus the integer `cw_i`/`ch_i` for procedural sprites), the baseline offset +
/// descent for glyph/underline placement, the rounded pixel size the atlas is keyed on,
/// the atlas dimension for UV normalization, and the canvas color (a cell whose
/// background equals it is left to the clear, cutting overdraw). Computed once by the
/// caller, then handed to every [`emit_cell`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct CellCtx {
    pub cw: f32,
    pub ch: f32,
    pub cw_i: u32,
    pub ch_i: u32,
    /// Baseline below the cell-box top (the font line box centered in the cell).
    pub baseline_off: f32,
    /// Font descent (px) - positions the underline just under the baseline.
    pub descent: f32,
    pub px: f32,
    pub px_key: u32,
    pub atlas_dim: u32,
    pub canvas: Rgba,
}

/// Emit one cell's background/underline rects and its glyph instance, placed with its
/// top-left at `origin` (physical px), appending to the caller's `bg_out`/`glyph_out`.
/// Always [`FontFamily::Grid`] (Mono): both callers draw constant-advance cell content.
///
/// Mirrors the grid's long-standing per-cell path exactly: skip a canvas-colored
/// background, draw a thin underline under the baseline, then the glyph - a procedural
/// sprite filling the cell box, a Nerd-Font PUA icon scaled/centered by its constraint
/// (T-4.4), or an ordinary baseline-relative font glyph - each SNAPPED to integer pixels
/// so the hinted bitmap maps 1:1 under the atlas's Nearest sampler (no inter-glyph bleed).
pub(crate) fn emit_cell(
    atlas: &mut GlyphAtlas,
    queue: &wgpu::Queue,
    cell: &GridCell,
    origin: (f32, f32),
    ctx: &CellCtx,
    bg_out: &mut Vec<RectInstance>,
    glyph_out: &mut Vec<GlyphInstance>,
) {
    let (cell_x, cell_y) = origin;
    let cw_cell = if cell.wide { ctx.cw * 2.0 } else { ctx.cw };

    // Background quad (skip canvas-colored cells; the clear covers them).
    if cell.bg != ctx.canvas {
        bg_out.push(RectInstance {
            rect: [cell_x, cell_y, cw_cell, ctx.ch],
            color: cell.bg.to_linear_f32(),
        });
    }
    // Underline: a thin quad just under the baseline.
    if cell.underline {
        let uy = cell_y + ctx.baseline_off + (ctx.descent * 0.3).max(1.0);
        bg_out.push(RectInstance {
            rect: [cell_x, uy, cw_cell, (ctx.ch * 0.06).max(1.0)],
            color: cell.fg.to_linear_f32(),
        });
    }

    // Glyph quad. A sprite codepoint (box-drawing / blocks / braille / Powerline) is
    // drawn procedurally into the cell box and bypasses the font; everything else is a
    // font glyph keyed by its cmap glyph id.
    let sprite = crate::sprite::is_sprite(cell.ch);
    let face = if sprite {
        FaceStyle::Regular
    } else {
        FaceStyle::from_flags(cell.bold, cell.italic)
    };
    let gkey = GlyphKey {
        family: FontFamily::Grid,
        glyph_id: if sprite {
            cell.ch as u32 as u16 // sprite codepoints are all in the BMP
        } else {
            atlas.glyph_id(FontFamily::Grid, face, cell.ch)
        },
        face,
        px: ctx.px_key,
        sprite,
    };
    let slot = if sprite {
        atlas.acquire_sprite(queue, gkey, cell.ch, ctx.cw_i, ctx.ch_i)
    } else {
        atlas.acquire_font(queue, gkey, FontFamily::Grid, face, gkey.glyph_id, ctx.px)
    };
    let Some((rect, (left, top))) = slot else {
        return;
    };
    // Snap the glyph quad to integer pixels (the cell origin is fractional). Three
    // placements: a sprite fills the cell box; a constrained PUA icon is scaled/centered
    // into the cell; an ordinary glyph is baseline-relative at its natural size.
    let (gx, gy, gw, gh) = if sprite {
        (cell_x.round(), cell_y.round(), rect.w as f32, rect.h as f32)
    } else if let Some(con) = crate::constraint::lookup(cell.ch) {
        let p = con.place(rect.w as f32, rect.h as f32, cw_cell, ctx.ch);
        (
            (cell_x + p.x).round(),
            (cell_y + p.y).round(),
            p.w.round().max(1.0),
            p.h.round().max(1.0),
        )
    } else {
        (
            (cell_x + left as f32).round(),
            (cell_y + ctx.baseline_off - top as f32).round(),
            rect.w as f32,
            rect.h as f32,
        )
    };
    let inv = 1.0 / ctx.atlas_dim as f32;
    glyph_out.push(GlyphInstance {
        rect: [gx, gy, gw, gh],
        uv: [
            rect.x as f32 * inv,
            rect.y as f32 * inv,
            (rect.x + rect.w) as f32 * inv,
            (rect.y + rect.h) as f32 * inv,
        ],
        color: cell.fg.to_linear_f32(),
    });
}
