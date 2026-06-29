//! Procedurally-drawn "sprite face" for box-drawing, block elements, braille, and
//! Powerline separators (ticket T-4.5).
//!
//! These glyph ranges are drawn DIRECTLY into an 8-bit alpha coverage bitmap sized
//! to the grid cell, rather than pulled from the font outline, so they are
//! pixel-perfect and seamless regardless of which font is active - removing a whole
//! class of misalignment bugs (small/squished/off-cell box and Powerline glyphs).
//! The output is a [`RasterGlyph`] (the SAME type swash produces in
//! [`crate::glyph`]), so the existing atlas + cache + instanced pipeline consume a
//! sprite exactly like any font glyph: drawn once, cached, never re-rasterized.
//!
//! Pure + deterministic + GPU-free: a sprite is a function of `(codepoint, cell_w,
//! cell_h)` only, so the whole module is unit-tested headlessly (edge coverage for
//! seamless tiling, dot/fill/triangle placement) with no window or device.
//!
//! ## Coverage
//!
//! - **Box-drawing** `U+2500..=257F`: straight lines, corners, T/cross junctions,
//!   and half-lines, in LIGHT and HEAVY weight, via a uniform 4-arm model. The
//!   mixed light/heavy junctions, the double-line set (`U+2550..=256C`), arcs
//!   (`U+256D..=2570`), diagonals (`U+2571..=2573`), and dashes are intentionally
//!   left to the FONT (see [`classify`]); they fall through the normal glyph path
//!   with no regression. The straight-line set is what tables / `tmux` borders use
//!   and is the seamless-tiling case the ticket targets.
//! - **Block elements** `U+2580..=259F`: half/eighth blocks, quadrants, and the
//!   three shades (full coverage).
//! - **Braille** `U+2800..=28FF`: the full 256-pattern 2x4 dot matrix (algorithmic).
//! - **Powerline** `U+E0B0..=E0B3`: the filled/outline left/right triangles.

use crate::glyph::RasterGlyph;

/// Weight of one box-drawing arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Weight {
    None,
    Light,
    Heavy,
}

/// What a sprite codepoint resolves to - enough to draw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sprite {
    /// Box-drawing arms in order `[up, down, left, right]`.
    Box([Weight; 4]),
    /// A block-element / quadrant / shade codepoint (drawn by [`draw_block`]).
    Block(char),
    /// A braille pattern: the 8-bit dot mask (`codepoint - 0x2800`).
    Braille(u8),
    /// A Powerline triangle codepoint (`U+E0B0..=E0B3`).
    Powerline(char),
}

/// Whether `ch` is drawn by the sprite face (and so should bypass the font).
#[must_use]
pub fn is_sprite(ch: char) -> bool {
    classify(ch).is_some()
}

/// Draw `ch` into a `w` x `h` 8-bit alpha coverage bitmap, or `None` if `ch` is not
/// a sprite codepoint (the caller falls back to the font) or the cell is degenerate.
///
/// The returned [`RasterGlyph`] has `left = top = 0`: a sprite fills the cell box and
/// the renderer positions it at the cell origin (not baseline-relative like a font
/// glyph). An ink-free sprite (only blank braille `U+2800` can be one) returns a
/// zero-size [`RasterGlyph::is_empty`] glyph, so the pipeline skips it outright - no
/// atlas slot, no degenerate invisible quad (the same treatment a font space gets).
#[must_use]
pub fn render(ch: char, w: u32, h: u32) -> Option<RasterGlyph> {
    if w == 0 || h == 0 {
        return None;
    }
    let sprite = classify(ch)?;
    let mut c = Canvas::new(w, h);
    match sprite {
        Sprite::Box(arms) => draw_box(&mut c, arms),
        Sprite::Block(cp) => draw_block(&mut c, cp),
        Sprite::Braille(mask) => draw_braille(&mut c, mask),
        Sprite::Powerline(cp) => draw_powerline(&mut c, cp),
    }
    let g = c.into_raster();
    if g.coverage.iter().all(|&v| v == 0) {
        // Inked nothing: hand back a truly-empty glyph so `place_glyph` skips it
        // (else it would cache + emit a fully-transparent quad).
        return Some(RasterGlyph {
            left: 0,
            top: 0,
            width: 0,
            height: 0,
            coverage: Vec::new(),
        });
    }
    Some(g)
}

/// Classify a codepoint into the sprite it draws, or `None` (use the font).
fn classify(ch: char) -> Option<Sprite> {
    match ch {
        '\u{2500}'..='\u{257F}' => box_arms(ch).map(Sprite::Box),
        '\u{2580}'..='\u{259F}' => Some(Sprite::Block(ch)),
        '\u{2800}'..='\u{28FF}' => Some(Sprite::Braille((ch as u32 - 0x2800) as u8)),
        '\u{E0B0}'..='\u{E0B3}' => Some(Sprite::Powerline(ch)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Box-drawing: the 4-arm model
// ---------------------------------------------------------------------------

/// The `[up, down, left, right]` arm weights for a box-drawing codepoint we draw, or
/// `None` to defer to the font (mixed light/heavy junctions, double lines, arcs,
/// diagonals, dashes). `N`/`L`/`H` abbreviate the weights for the table's density.
fn box_arms(ch: char) -> Option<[Weight; 4]> {
    use Weight::{Heavy as H, Light as L, None as N};
    let arms = match ch {
        // Straight lines.
        '\u{2500}' => [N, N, L, L], // ─
        '\u{2501}' => [N, N, H, H], // ━
        '\u{2502}' => [L, L, N, N], // │
        '\u{2503}' => [H, H, N, N], // ┃
        // Corners (down+right, down+left, up+right, up+left) across both weights.
        '\u{250C}' => [N, L, N, L], // ┌
        '\u{250D}' => [N, L, N, H], // ┍
        '\u{250E}' => [N, H, N, L], // ┎
        '\u{250F}' => [N, H, N, H], // ┏
        '\u{2510}' => [N, L, L, N], // ┐
        '\u{2511}' => [N, L, H, N], // ┑
        '\u{2512}' => [N, H, L, N], // ┒
        '\u{2513}' => [N, H, H, N], // ┓
        '\u{2514}' => [L, N, N, L], // └
        '\u{2515}' => [L, N, N, H], // ┕
        '\u{2516}' => [H, N, N, L], // ┖
        '\u{2517}' => [H, N, N, H], // ┗
        '\u{2518}' => [L, N, L, N], // ┘
        '\u{2519}' => [L, N, H, N], // ┙
        '\u{251A}' => [H, N, L, N], // ┚
        '\u{251B}' => [H, N, H, N], // ┛
        // Pure-weight T and cross junctions (the mixed-weight T/cross set is left to
        // the font - its per-arm weights are subtle and rarely used).
        '\u{251C}' => [L, L, N, L], // ├
        '\u{2523}' => [H, H, N, H], // ┣
        '\u{2524}' => [L, L, L, N], // ┤
        '\u{252B}' => [H, H, H, N], // ┫
        '\u{252C}' => [N, L, L, L], // ┬
        '\u{2533}' => [N, H, H, H], // ┳
        '\u{2534}' => [L, N, L, L], // ┴
        '\u{253B}' => [H, N, H, H], // ┻
        '\u{253C}' => [L, L, L, L], // ┼
        '\u{254B}' => [H, H, H, H], // ╋
        // Half-lines (a single arm), light then heavy.
        '\u{2574}' => [N, N, L, N], // ╴
        '\u{2575}' => [L, N, N, N], // ╵
        '\u{2576}' => [N, N, N, L], // ╶
        '\u{2577}' => [N, L, N, N], // ╷
        '\u{2578}' => [N, N, H, N], // ╸
        '\u{2579}' => [H, N, N, N], // ╹
        '\u{257A}' => [N, N, N, H], // ╺
        '\u{257B}' => [N, H, N, N], // ╻
        // Mixed-weight half-lines (both arms collinear, differing weight).
        '\u{257C}' => [N, N, L, H], // ╼ light left, heavy right
        '\u{257D}' => [L, H, N, N], // ╽ light up, heavy down
        '\u{257E}' => [N, N, H, L], // ╾ heavy left, light right
        '\u{257F}' => [H, L, N, N], // ╿ heavy up, light down
        _ => return None,
    };
    Some(arms)
}

/// Stroke thickness in px for a weight, derived from the smaller cell dimension so
/// lines stay proportionate at any size. Heavy is always at least one px thicker.
fn stroke(weight: Weight, w: u32, h: u32) -> u32 {
    let base = w.min(h) as f32;
    match weight {
        Weight::None => 0,
        Weight::Light => (base * 0.12).round().max(1.0) as u32,
        Weight::Heavy => (base * 0.30).round().max(2.0) as u32,
    }
}

/// Draw the present arms. Each arm runs from the cell edge to the centre, in a band
/// centred on the mid-axis; opposite arms overlap at the centre (union via `max`), so
/// a full line inks edge-to-edge (the seamless-tiling property) and a junction joins
/// cleanly.
fn draw_box(c: &mut Canvas, arms: [Weight; 4]) {
    let (w, h) = (c.w as i32, c.h as i32);
    let (cx, cy) = (w / 2, h / 2);
    let band = |s: i32, centre: i32| (centre - s / 2, centre - s / 2 + s);
    // up, down, left, right
    if arms[0] != Weight::None {
        let s = stroke(arms[0], c.w, c.h) as i32;
        let (lo, hi) = band(s, cx);
        c.fill(lo, 0, hi, cy + (s - s / 2));
    }
    if arms[1] != Weight::None {
        let s = stroke(arms[1], c.w, c.h) as i32;
        let (lo, hi) = band(s, cx);
        c.fill(lo, cy - s / 2, hi, h);
    }
    if arms[2] != Weight::None {
        let s = stroke(arms[2], c.w, c.h) as i32;
        let (lo, hi) = band(s, cy);
        c.fill(0, lo, cx + (s - s / 2), hi);
    }
    if arms[3] != Weight::None {
        let s = stroke(arms[3], c.w, c.h) as i32;
        let (lo, hi) = band(s, cy);
        c.fill(cx - s / 2, lo, w, hi);
    }
}

// ---------------------------------------------------------------------------
// Block elements + quadrants + shades (U+2580..=259F)
// ---------------------------------------------------------------------------

fn draw_block(c: &mut Canvas, cp: char) {
    let (w, h) = (c.w as i32, c.h as i32);
    // Shared 1/8 gridlines (rounded). Halves, eighths, AND quadrants divide on the
    // SAME lines, so complementary glyphs (upper/lower half, the quadrants) tile with
    // no seam even at odd cell sizes (the default 1x grid height is odd). g(0)=0,
    // g(8)=the full extent.
    let gx = |k: i32| (k * w + 4) / 8;
    let gy = |k: i32| (k * h + 4) / 8;
    let (cx, cy) = (gx(4), gy(4)); // horizontal + vertical midlines
    match cp {
        '\u{2580}' => c.fill(0, 0, w, cy), // ▀ upper half
        '\u{2581}'..='\u{2588}' => {
            // ▁..█ lower 1/8..8/8.
            let n = cp as i32 - 0x2580; // 1..=8
            c.fill(0, gy(8 - n), w, h);
        }
        '\u{2589}'..='\u{258F}' => {
            // ▉..▏ left 7/8..1/8.
            let n = 0x2590 - cp as i32; // 258F -> 1 .. 2589 -> 7
            c.fill(0, 0, gx(n), h);
        }
        '\u{2590}' => c.fill(cx, 0, w, h), // ▐ right half
        '\u{2591}' => c.fill_alpha(0, 0, w, h, 0x40), // ░ light shade
        '\u{2592}' => c.fill_alpha(0, 0, w, h, 0x80), // ▒ medium shade
        '\u{2593}' => c.fill_alpha(0, 0, w, h, 0xC0), // ▓ dark shade
        '\u{2594}' => c.fill(0, 0, w, gy(1)), // ▔ upper 1/8
        '\u{2595}' => c.fill(gx(7), 0, w, h), // ▕ right 1/8
        // Quadrants. UL,UR,LL,LR.
        '\u{2596}' => c.fill(0, cy, cx, h), // ▖ LL
        '\u{2597}' => c.fill(cx, cy, w, h), // ▗ LR
        '\u{2598}' => c.fill(0, 0, cx, cy), // ▘ UL
        '\u{2599}' => {
            c.fill(0, 0, cx, cy); // ▙ UL+LL+LR
            c.fill(0, cy, w, h);
        }
        '\u{259A}' => {
            c.fill(0, 0, cx, cy); // ▚ UL+LR
            c.fill(cx, cy, w, h);
        }
        '\u{259B}' => {
            c.fill(0, 0, w, cy); // ▛ UL+UR+LL
            c.fill(0, cy, cx, h);
        }
        '\u{259C}' => {
            c.fill(0, 0, w, cy); // ▜ UL+UR+LR
            c.fill(cx, cy, w, h);
        }
        '\u{259D}' => c.fill(cx, 0, w, cy), // ▝ UR
        '\u{259E}' => {
            c.fill(cx, 0, w, cy); // ▞ UR+LL
            c.fill(0, cy, cx, h);
        }
        '\u{259F}' => {
            c.fill(cx, 0, w, cy); // ▟ UR+LL+LR
            c.fill(0, cy, w, h);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Braille (U+2800..=28FF)
// ---------------------------------------------------------------------------

/// Bit -> (column, row) in the 2x4 dot matrix. Unicode braille bit order: dots
/// 1,2,3 are the left column rows 0,1,2; 4,5,6 the right column rows 0,1,2; 7 is
/// bottom-left, 8 bottom-right.
const BRAILLE_DOTS: [(u32, u32); 8] = [
    (0, 0),
    (0, 1),
    (0, 2),
    (1, 0),
    (1, 1),
    (1, 2),
    (0, 3),
    (1, 3),
];

fn draw_braille(c: &mut Canvas, mask: u8) {
    let (w, h) = (c.w, c.h);
    // Dot radius: a small filled disc, scaled to the cell.
    let r = ((w.min(h) as f32) * 0.12).round().max(1.0) as i32;
    for (bit, &(col, row)) in BRAILLE_DOTS.iter().enumerate() {
        if mask & (1u8 << bit) == 0 {
            continue;
        }
        // Column centres at 1/4 and 3/4 of width; row centres at 1/8,3/8,5/8,7/8.
        let dx = (w as i32 * (1 + 2 * col as i32)) / 4;
        let dy = (h as i32 * (1 + 2 * row as i32)) / 8;
        c.disc(dx, dy, r);
    }
}

// ---------------------------------------------------------------------------
// Powerline triangles (U+E0B0..=E0B3)
// ---------------------------------------------------------------------------

fn draw_powerline(c: &mut Canvas, cp: char) {
    let (w, h) = (c.w as i32, c.h as i32);
    let cy = h / 2;
    let edge = ((w.min(h) as f32) * 0.16).round().max(1.0) as i32;
    match cp {
        // E0B0 rightward filled triangle: apex at right-mid, base = left edge.
        '\u{E0B0}' | '\u{E0B1}' => {
            for y in 0..h {
                // Right boundary at row y: full width at centre, 0 at top/bottom.
                let frac = 1.0 - ((y - cy).abs() as f32 / cy.max(1) as f32);
                let right = (w as f32 * frac).round() as i32;
                if cp == '\u{E0B0}' {
                    c.fill(0, y, right, y + 1);
                } else {
                    // Outline: just the diagonal edge (a band near `right`).
                    c.fill(right - edge, y, right, y + 1);
                }
            }
        }
        // E0B2 leftward filled triangle: apex at left-mid, base = right edge.
        '\u{E0B2}' | '\u{E0B3}' => {
            for y in 0..h {
                let frac = 1.0 - ((y - cy).abs() as f32 / cy.max(1) as f32);
                let left = w - (w as f32 * frac).round() as i32;
                if cp == '\u{E0B2}' {
                    c.fill(left, y, w, y + 1);
                } else {
                    c.fill(left, y, left + edge, y + 1);
                }
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Canvas: an 8-bit alpha coverage buffer with clamped fills
// ---------------------------------------------------------------------------

struct Canvas {
    w: u32,
    h: u32,
    a: Vec<u8>,
}

impl Canvas {
    fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            a: vec![0; (w as usize) * (h as usize)],
        }
    }

    /// Set one pixel to `v` (union via `max`, so overlapping draws never darken).
    fn put(&mut self, x: u32, y: u32, v: u8) {
        if x < self.w && y < self.h {
            let i = (y * self.w + x) as usize;
            if v > self.a[i] {
                self.a[i] = v;
            }
        }
    }

    /// Fill the half-open rect `[x0,x1) x [y0,y1)` (clamped) with full coverage.
    fn fill(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        self.fill_alpha(x0, y0, x1, y1, 0xFF);
    }

    /// Fill the half-open rect (clamped) with alpha `v`.
    fn fill_alpha(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, v: u8) {
        let x0 = x0.max(0) as u32;
        let y0 = y0.max(0) as u32;
        let x1 = (x1.max(0) as u32).min(self.w);
        let y1 = (y1.max(0) as u32).min(self.h);
        for y in y0..y1 {
            for x in x0..x1 {
                self.put(x, y, v);
            }
        }
    }

    /// Fill a disc of radius `r` centred at `(cx, cy)`.
    fn disc(&mut self, cx: i32, cy: i32, r: i32) {
        let r2 = r * r;
        for y in (cy - r)..=(cy + r) {
            for x in (cx - r)..=(cx + r) {
                let (dx, dy) = (x - cx, y - cy);
                if dx * dx + dy * dy <= r2 && x >= 0 && y >= 0 {
                    self.put(x as u32, y as u32, 0xFF);
                }
            }
        }
    }

    fn into_raster(self) -> RasterGlyph {
        RasterGlyph {
            left: 0,
            top: 0,
            width: self.w,
            height: self.h,
            coverage: self.a,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative cell size (≈13pt grid at 2x: cw≈16, ch≈34).
    const W: u32 = 16;
    const H: u32 = 34;

    fn cov(g: &RasterGlyph, x: u32, y: u32) -> u8 {
        g.coverage[(y * g.width + x) as usize]
    }

    #[test]
    fn classify_covers_intended_ranges_and_defers_the_rest() {
        assert!(is_sprite('\u{2500}')); // ─ light horizontal
        assert!(is_sprite('\u{253C}')); // ┼ light cross
        assert!(is_sprite('\u{2588}')); // █ full block
        assert!(is_sprite('\u{2592}')); // ▒ medium shade
        assert!(is_sprite('\u{28FF}')); // braille all dots
        assert!(is_sprite('\u{E0B0}')); // Powerline
                                        // Deferred to the font (no regression): double-line, arc, diagonal, dash.
        assert!(!is_sprite('\u{2550}')); // ═ double horizontal
        assert!(!is_sprite('\u{256D}')); // ╭ arc
        assert!(!is_sprite('\u{2571}')); // ╱ diagonal
        assert!(!is_sprite('\u{2504}')); // ┄ dashed
        assert!(!is_sprite('A')); // ordinary glyph
    }

    #[test]
    fn render_rejects_a_degenerate_cell() {
        assert!(render('\u{2500}', 0, H).is_none());
        assert!(render('\u{2500}', W, 0).is_none());
        assert!(render('A', W, H).is_none()); // not a sprite
    }

    #[test]
    fn horizontal_line_inks_full_width_at_centre_for_seamless_tiling() {
        // ─ : the centre row band must ink BOTH edge columns so two horizontally
        // adjacent cells abut with no gap.
        let g = render('\u{2500}', W, H).unwrap();
        let mid = H / 2;
        assert!(cov(&g, 0, mid) > 0, "left edge inked");
        assert!(cov(&g, W - 1, mid) > 0, "right edge inked");
        // Every column on the centre row is inked (a continuous line).
        for x in 0..W {
            assert!(cov(&g, x, mid) > 0, "column {x} inked on the centre row");
        }
        // The top row is blank (the line is a thin band, not a fill).
        assert!((0..W).all(|x| cov(&g, x, 0) == 0), "top row blank");
    }

    #[test]
    fn vertical_line_inks_full_height_at_centre() {
        let g = render('\u{2502}', W, H).unwrap();
        let mid = W / 2;
        assert!(cov(&g, mid, 0) > 0, "top edge inked");
        assert!(cov(&g, mid, H - 1) > 0, "bottom edge inked");
        for y in 0..H {
            assert!(cov(&g, mid, y) > 0, "row {y} inked on the centre column");
        }
    }

    #[test]
    fn heavy_line_is_thicker_than_light() {
        let count = |ch: char| {
            let g = render(ch, W, H).unwrap();
            g.coverage.iter().filter(|&&v| v > 0).count()
        };
        assert!(
            count('\u{2501}') > count('\u{2500}'),
            "heavy horizontal must ink more than light"
        );
    }

    #[test]
    fn corner_inks_only_its_two_arms() {
        // ┌ (down + right): the right half of the centre row and the lower half of
        // the centre column are inked; the left and top are NOT.
        let g = render('\u{250C}', W, H).unwrap();
        let (mx, my) = (W / 2, H / 2);
        assert!(cov(&g, W - 1, my) > 0, "right arm reaches the right edge");
        assert!(cov(&g, mx, H - 1) > 0, "down arm reaches the bottom edge");
        assert_eq!(cov(&g, 0, my), 0, "no left arm");
        assert_eq!(cov(&g, mx, 0), 0, "no up arm");
    }

    #[test]
    fn cross_junction_inks_all_four_edges() {
        let g = render('\u{253C}', W, H).unwrap(); // ┼
        let (mx, my) = (W / 2, H / 2);
        assert!(
            cov(&g, 0, my) > 0 && cov(&g, W - 1, my) > 0,
            "left+right arms"
        );
        assert!(cov(&g, mx, 0) > 0 && cov(&g, mx, H - 1) > 0, "up+down arms");
    }

    #[test]
    fn full_block_fills_every_pixel() {
        let g = render('\u{2588}', W, H).unwrap();
        assert!(g.coverage.iter().all(|&v| v == 0xFF), "█ is fully opaque");
    }

    #[test]
    fn lower_eighths_fill_from_the_bottom() {
        // ▁ (lower 1/8): the bottom row is inked, the top is not.
        let g = render('\u{2581}', W, H).unwrap();
        assert!(cov(&g, 0, H - 1) > 0, "bottom inked");
        assert_eq!(cov(&g, 0, 0), 0, "top blank");
        // ▄ (lower 4/8 = lower half): roughly half the cell is inked.
        let half = render('\u{2584}', W, H).unwrap();
        let inked = half.coverage.iter().filter(|&&v| v > 0).count();
        let total = (W * H) as usize;
        assert!(
            inked > total / 3 && inked < (2 * total) / 3,
            "lower half fills ~50% (got {inked}/{total})"
        );
    }

    #[test]
    fn shades_are_partial_uniform_coverage() {
        let light = render('\u{2591}', W, H).unwrap();
        let dark = render('\u{2593}', W, H).unwrap();
        // Uniform fill (every pixel the same non-zero, non-opaque alpha).
        assert!(light.coverage.iter().all(|&v| v == 0x40));
        assert!(dark.coverage.iter().all(|&v| v == 0xC0));
    }

    #[test]
    fn quadrant_lower_left_inks_only_that_corner() {
        let g = render('\u{2596}', W, H).unwrap(); // ▖ LL
        assert!(cov(&g, 0, H - 1) > 0, "lower-left inked");
        assert_eq!(cov(&g, W - 1, 0), 0, "upper-right blank");
        assert_eq!(cov(&g, W - 1, H - 1), 0, "lower-right blank");
    }

    #[test]
    fn half_blocks_and_quadrants_share_one_midline_at_odd_height() {
        // The default 1x grid height is ODD (≈17), where a floor/ceil split would
        // leave one row covered by neither ▀ nor ▄. Upper ▀ + lower ▄ must partition
        // EVERY row exactly once (no gap, no overlap), and the lower quadrant must
        // start on the same row as ▄.
        for h in [17u32, 33] {
            let upper = render('\u{2580}', 8, h).unwrap(); // ▀
            let lower = render('\u{2584}', 8, h).unwrap(); // ▄
            let ll = render('\u{2596}', 8, h).unwrap(); // ▖ lower-left quadrant
            for y in 0..h {
                let u = cov(&upper, 0, y) > 0;
                let l = cov(&lower, 0, y) > 0;
                assert!(u ^ l, "row {y} (h={h}) must be in exactly one of ▀ / ▄");
                // The lower quadrant occupies precisely ▄'s rows in the left column.
                assert_eq!(
                    cov(&ll, 0, y) > 0,
                    l,
                    "row {y} (h={h}): lower quadrant must align with the lower half"
                );
            }
        }
    }

    #[test]
    fn braille_blank_has_no_ink_and_full_has_all_dots() {
        // U+2800 = no dots -> a truly-empty glyph the pipeline skips (no atlas slot,
        // no transparent quad), exactly like a font space.
        let blank = render('\u{2800}', W, H).unwrap();
        assert!(
            blank.is_empty(),
            "blank braille must return an empty glyph the pipeline skips"
        );
        // U+28FF = all 8 dots -> ink in both columns and all four rows.
        let full = render('\u{28FF}', W, H).unwrap();
        let any = |col_lo: u32, col_hi: u32, row_lo: u32, row_hi: u32| {
            (row_lo..row_hi).any(|y| (col_lo..col_hi).any(|x| cov(&full, x, y) > 0))
        };
        assert!(any(0, W / 2, 0, H / 4), "a dot in the upper-left region");
        assert!(
            any(W / 2, W, 3 * H / 4, H),
            "a dot in the lower-right region"
        );
    }

    #[test]
    fn braille_single_dot_is_isolated() {
        // U+2801 = dot 1 only (upper-left). The lower-right region stays blank.
        let g = render('\u{2801}', W, H).unwrap();
        let lr = (3 * H / 4..H).any(|y| (W / 2..W).any(|x| cov(&g, x, y) > 0));
        assert!(!lr, "dot-1-only must not ink the lower-right");
    }

    #[test]
    fn powerline_filled_triangle_spans_full_width_at_centre() {
        // E0B0 (right-pointing filled): the centre row reaches the right edge; the
        // top and bottom rows are (near) empty (the triangle narrows to a point).
        let g = render('\u{E0B0}', W, H).unwrap();
        let cy = H / 2;
        assert!(
            cov(&g, W - 1, cy) > 0,
            "apex reaches the right edge at centre"
        );
        assert!(cov(&g, 0, cy) > 0, "base inked at the left edge");
        assert_eq!(cov(&g, W - 1, 0), 0, "narrow at the top");
    }

    #[test]
    fn powerline_left_triangle_is_the_mirror() {
        let g = render('\u{E0B2}', W, H).unwrap();
        let cy = H / 2;
        assert!(cov(&g, 0, cy) > 0, "apex reaches the left edge at centre");
        assert!(cov(&g, W - 1, cy) > 0, "base inked at the right edge");
        assert_eq!(cov(&g, 0, 0), 0, "narrow at the top");
    }
}
