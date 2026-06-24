//! The terminal grid text pipeline (ticket T-1.6) - CPU half.
//!
//! This module owns the *deterministic, GPU-free* logic the grid renderer is built
//! on, kept separate from wgpu so it is unit-testable with no window or device:
//!
//! - [`resolve_color`]: a VT [`CellColor`] resolved against the active
//!   [`Theme`] to a concrete [`Rgba`] (ANSI-16, the 256-color cube + grayscale
//!   ramp, true color, and the semantic default-fg/bg slots).
//! - [`build_grid_cells`]: a [`Snapshot`] flattened into per-cell
//!   [`GridCell`] instances (resolved colors, inverse applied, wide glyphs flagged,
//!   their trailing spacer dropped) - the input the GPU draws as one instanced
//!   quad per cell.
//! - [`is_ascii_fast`] / [`classify_run`]: the ASCII fast-path decision - a plain
//!   single-width printable-ASCII run is placed at constant advance WITHOUT the
//!   shaper (ticket AC).
//! - [`ShelfAllocator`] + [`GlyphCache`]: the glyph-atlas rectangle packer and the
//!   (glyph, face, px)-keyed cache that guarantees a repeated glyph is rasterized
//!   only once (ticket AC).
//!
//! What is NOT here (it needs a GPU/device and on-screen verification, so it is the
//! owner-watched render pass, like T-1.5's present loop): swash/CoreText glyph
//! rasterization into the atlas texture, the wgpu instanced pipeline + grayscale
//! composite-by-multiply shader, and the single-draw-call submission. Those consume
//! the types here through the `aterm-ui` renderer seam.

use std::collections::HashMap;

use aterm_core::{CellColor, Snapshot};
use aterm_tokens::{Rgba, Theme};

// ---------------------------------------------------------------------------
// Color resolution
// ---------------------------------------------------------------------------

/// `NamedColor::Foreground` / `Background` discriminants (see [`CellColor::Named`]).
const NAMED_FOREGROUND: u16 = 256;
const NAMED_BACKGROUND: u16 = 257;
/// `NamedColor` dim-ANSI range: `DimBlack..=DimWhite` map onto ANSI 0..=7, dimmed.
/// Verified against `vte` 0.15 `NamedColor` (Cursor=258, DimBlack=259 .. DimWhite=266,
/// BrightForeground=267, DimForeground=268).
const NAMED_DIM_FIRST: u16 = 259;
const NAMED_DIM_LAST: u16 = 266;
/// `NamedColor::BrightForeground` / `DimForeground`.
const NAMED_BRIGHT_FOREGROUND: u16 = 267;
const NAMED_DIM_FOREGROUND: u16 = 268;

/// Resolve a VT [`CellColor`] against `theme` to a concrete [`Rgba`].
///
/// `is_fg` selects the default slot for the semantic foreground/background named
/// colors and for any unrecognized high `Named` value, so a default cell themes to
/// `fg_primary` on `bg_canvas`.
#[must_use]
pub fn resolve_color(color: CellColor, theme: &Theme, is_fg: bool) -> Rgba {
    match color {
        CellColor::Rgb(r, g, b) => Rgba { r, g, b, a: 255 },
        CellColor::Indexed(i) => resolve_indexed(i, theme),
        CellColor::Named(n) => resolve_named(n, theme, is_fg),
    }
}

/// Resolve an xterm 256-color palette index: 0..=15 themed ANSI, 16..=231 the
/// 6x6x6 color cube, 232..=255 the 24-step grayscale ramp (the standard xterm
/// formulas).
fn resolve_indexed(i: u8, theme: &Theme) -> Rgba {
    match i {
        0..=15 => theme.ansi.by_index(i),
        16..=231 => {
            let v = i - 16;
            let r = v / 36;
            let g = (v / 6) % 6;
            let b = v % 6;
            // Channel level: 0 stays 0, otherwise 55 + 40*step (xterm cube).
            let level = |c: u8| if c == 0 { 0 } else { 55 + c * 40 };
            Rgba {
                r: level(r),
                g: level(g),
                b: level(b),
                a: 255,
            }
        }
        232..=255 => {
            // Grayscale ramp: 8, 18, 28, ... 238.
            let level = 8 + (i - 232) * 10;
            Rgba {
                r: level,
                g: level,
                b: level,
                a: 255,
            }
        }
    }
}

/// Resolve a semantic/named color. ANSI 0..=15 theme directly; the dim-ANSI range
/// maps onto dimmed ANSI 0..=7; the default fg/bg slots take the theme's primary
/// text / canvas; anything else falls back to the default slot for `is_fg`.
fn resolve_named(n: u16, theme: &Theme, is_fg: bool) -> Rgba {
    match n {
        0..=15 => theme.ansi.by_index(n as u8),
        NAMED_FOREGROUND => theme.colors.fg_primary,
        NAMED_BACKGROUND => theme.colors.bg_canvas,
        NAMED_DIM_FIRST..=NAMED_DIM_LAST => dim(theme.ansi.by_index((n - NAMED_DIM_FIRST) as u8)),
        NAMED_BRIGHT_FOREGROUND => theme.colors.fg_primary,
        NAMED_DIM_FOREGROUND => theme.colors.fg_muted,
        _ => {
            // Cursor (258) or a future slot: theme to the default text or canvas
            // color rather than guessing a dedicated token (cursor cells are drawn
            // by a separate cursor pass, not via this color path).
            if is_fg {
                theme.colors.fg_primary
            } else {
                theme.colors.bg_canvas
            }
        }
    }
}

/// Dim a color toward black (~2/3 brightness) for the dim-ANSI slots. Approximate
/// but stable; a tuned dim palette is a token-layer refinement.
fn dim(c: Rgba) -> Rgba {
    Rgba {
        r: ((c.r as u16 * 2) / 3) as u8,
        g: ((c.g as u16 * 2) / 3) as u8,
        b: ((c.b as u16 * 2) / 3) as u8,
        a: c.a,
    }
}

// ---------------------------------------------------------------------------
// Grid instance generation
// ---------------------------------------------------------------------------

/// One renderable grid cell: the GPU draws one instanced quad per [`GridCell`]
/// (the cell background, then its glyph composited in `fg`). Colors are already
/// resolved against the theme and inverse is already applied, so the renderer is
/// pure geometry + atlas lookups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridCell {
    pub col: u16,
    pub row: u16,
    pub ch: char,
    pub fg: Rgba,
    pub bg: Rgba,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// This glyph occupies two columns (a wide/CJK cell); the GPU advances the
    /// pen by two cells. The trailing spacer cell is not emitted.
    pub wide: bool,
}

/// Flatten `snap` into per-cell [`GridCell`] instances, resolving colors against
/// `theme`, applying inverse (swap fg/bg), flagging wide glyphs, and DROPPING the
/// trailing spacer of a wide glyph (it is drawn from its lead cell).
///
/// Reuses `out` (clear + refill) so a steady-state frame allocates nothing once
/// the buffer is warm - the same zero-alloc-present discipline as T-1.5. Emits one
/// instance per non-spacer cell (the whole visible grid), which the renderer draws
/// in a single instanced call.
pub fn build_grid_cells(snap: &Snapshot, theme: &Theme, out: &mut Vec<GridCell>) {
    out.clear();
    out.reserve(snap.rows * snap.cols);
    for row in 0..snap.rows {
        for (col, cell) in snap.row(row).iter().enumerate() {
            // The trailing half of a wide glyph carries no glyph of its own.
            if cell.wide_spacer {
                continue;
            }
            let mut fg = resolve_color(cell.fg, theme, true);
            let mut bg = resolve_color(cell.bg, theme, false);
            if cell.inverse {
                std::mem::swap(&mut fg, &mut bg);
            }
            out.push(GridCell {
                col: col as u16,
                row: row as u16,
                ch: cell.c,
                fg,
                bg,
                bold: cell.bold,
                italic: cell.italic,
                underline: cell.underline,
                wide: cell.wide,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// ASCII fast path
// ---------------------------------------------------------------------------

/// Whether `ch` is a plain single-width printable-ASCII glyph the constant-advance
/// grid can place by a direct codepoint->glyph lookup, with NO shaper involvement
/// (no ligatures, combining marks, or complex/wide scripts).
#[must_use]
pub fn is_ascii_fast(ch: char) -> bool {
    ('\u{20}'..='\u{7e}').contains(&ch)
}

/// How a run of cells should be laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunLayout {
    /// Every char is plain single-width ASCII: place at constant advance, no shaper.
    AsciiFast,
    /// At least one char needs the shaper (non-ASCII, combining, ligatures, wide).
    Shape,
}

/// Classify a run of characters into the fast path or the shaping path. Used by the
/// grid layout to skip the shaper for plain ASCII lines (ticket AC: a plain ASCII
/// line provably takes the fast path).
#[must_use]
pub fn classify_run(chars: impl IntoIterator<Item = char>) -> RunLayout {
    if chars.into_iter().all(is_ascii_fast) {
        RunLayout::AsciiFast
    } else {
        RunLayout::Shape
    }
}

// ---------------------------------------------------------------------------
// Glyph atlas: shelf allocator + cache
// ---------------------------------------------------------------------------

/// A rectangle reserved in the glyph atlas texture (in texels).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtlasRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// A shelf (row) packer for the glyph atlas. Glyphs are packed left-to-right into
/// the current shelf; when a glyph does not fit the shelf width a new shelf opens
/// below at the running max height. Simple and cache-friendly for the near-uniform
/// rectangles of a monospace face; [`alloc`](Self::alloc) returns `None` when the
/// atlas is full (the caller grows the texture or evicts). A guillotine packer
/// (etagere) is the upgrade path if fragmentation ever bites.
#[derive(Debug, Clone)]
pub struct ShelfAllocator {
    width: u32,
    height: u32,
    shelf_y: u32,
    shelf_height: u32,
    pen_x: u32,
}

impl ShelfAllocator {
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            shelf_y: 0,
            shelf_height: 0,
            pen_x: 0,
        }
    }

    /// Reserve a `w` x `h` rectangle, or `None` if it cannot fit (too wide for the
    /// atlas, or no vertical room left).
    pub fn alloc(&mut self, w: u32, h: u32) -> Option<AtlasRect> {
        if w == 0 || h == 0 || w > self.width {
            return None;
        }
        // Open a new shelf if this glyph would overflow the current row.
        if self.pen_x + w > self.width {
            self.shelf_y = self.shelf_y.checked_add(self.shelf_height)?;
            self.shelf_height = 0;
            self.pen_x = 0;
        }
        if self.shelf_y + h > self.height {
            return None; // out of vertical room
        }
        let rect = AtlasRect {
            x: self.pen_x,
            y: self.shelf_y,
            w,
            h,
        };
        self.pen_x += w;
        self.shelf_height = self.shelf_height.max(h);
        Some(rect)
    }
}

/// The face variant a glyph was shaped in - part of the atlas cache key, since the
/// same codepoint rasterizes differently per weight/slant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaceStyle {
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

impl FaceStyle {
    /// The face for a cell's bold/italic flags.
    #[must_use]
    pub fn from_flags(bold: bool, italic: bool) -> Self {
        match (bold, italic) {
            (false, false) => FaceStyle::Regular,
            (true, false) => FaceStyle::Bold,
            (false, true) => FaceStyle::Italic,
            (true, true) => FaceStyle::BoldItalic,
        }
    }
}

/// Atlas cache key. NOTE: there is NO subpixel-offset field. On a constant-advance
/// grid every glyph lands on an integer cell origin, so there is exactly ONE
/// subpixel variant per (glyph, face, size) - far fewer than the ~16 a proportional
/// layout (e.g. GPUI) needs. `px` is the integer pixel size (after the scale
/// factor) so a Retina vs non-Retina run keys distinct rasterizations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub glyph_id: u16,
    pub face: FaceStyle,
    pub px: u32,
}

/// Maps a [`GlyphKey`] to its reserved [`AtlasRect`], rasterizing each unique glyph
/// at most once (ticket AC: no re-rasterization of a cached glyph). The actual
/// rasterization (swash/CoreText -> alpha bitmap -> texture upload) is supplied by
/// the GPU layer as the `rasterize` closure; this owns only the allocation +
/// caching + the once-only guarantee.
#[derive(Debug)]
pub struct GlyphCache {
    map: HashMap<GlyphKey, AtlasRect>,
    alloc: ShelfAllocator,
    rasterizations: u64,
}

impl GlyphCache {
    #[must_use]
    pub fn new(atlas_width: u32, atlas_height: u32) -> Self {
        Self {
            map: HashMap::new(),
            alloc: ShelfAllocator::new(atlas_width, atlas_height),
            rasterizations: 0,
        }
    }

    /// Look up `key`; on a miss, reserve atlas space and rasterize exactly once via
    /// `rasterize` (which returns the glyph's `(width, height)` in texels and fills
    /// the texture). Returns the cached rect, or `None` if the atlas is full.
    ///
    /// `rasterize` is invoked at most once per distinct key - that is the
    /// no-re-rasterization guarantee the ticket asserts.
    pub fn get_or_insert(
        &mut self,
        key: GlyphKey,
        rasterize: impl FnOnce(AtlasRect),
        glyph_w: u32,
        glyph_h: u32,
    ) -> Option<AtlasRect> {
        if let Some(rect) = self.map.get(&key) {
            return Some(*rect);
        }
        let rect = self.alloc.alloc(glyph_w, glyph_h)?;
        rasterize(rect);
        self.rasterizations += 1;
        self.map.insert(key, rect);
        Some(rect)
    }

    /// Whether `key` is already rasterized + cached.
    #[must_use]
    pub fn contains(&self, key: &GlyphKey) -> bool {
        self.map.contains_key(key)
    }

    /// The cached atlas rect for `key`, or `None` if not yet rasterized. Lets the
    /// GPU layer reuse a hit WITHOUT the `(glyph_w, glyph_h)` that
    /// [`Self::get_or_insert`] requires - the renderer rasterizes (which produces
    /// those dimensions) only on a miss, so on a hit it has neither but still needs
    /// the rect.
    #[must_use]
    pub fn get(&self, key: &GlyphKey) -> Option<AtlasRect> {
        self.map.get(key).copied()
    }

    /// Total distinct glyphs rasterized so far (the ticket's no-re-raster counter).
    #[must_use]
    pub fn rasterizations(&self) -> u64 {
        self.rasterizations
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_core::Snapshot;
    use aterm_tokens::ThemeKind;

    fn theme() -> Theme {
        *Theme::for_kind(ThemeKind::Dark)
    }

    #[test]
    fn rgb_is_passed_through_verbatim() {
        let t = theme();
        assert_eq!(
            resolve_color(CellColor::Rgb(10, 20, 30), &t, true),
            Rgba {
                r: 10,
                g: 20,
                b: 30,
                a: 255
            }
        );
    }

    #[test]
    fn named_default_fg_bg_use_theme_slots() {
        let t = theme();
        assert_eq!(
            resolve_color(CellColor::Named(256), &t, true),
            t.colors.fg_primary
        );
        assert_eq!(
            resolve_color(CellColor::Named(257), &t, false),
            t.colors.bg_canvas
        );
    }

    #[test]
    fn named_ansi_indices_theme_to_palette() {
        let t = theme();
        for i in 0u16..=15 {
            assert_eq!(
                resolve_color(CellColor::Named(i), &t, true),
                t.ansi.by_index(i as u8),
                "named ANSI {i} should map to the themed palette slot"
            );
        }
    }

    #[test]
    fn unknown_named_falls_back_to_default_slot() {
        let t = theme();
        // 258 (Cursor) and other high slots fall back to the default for is_fg.
        assert_eq!(
            resolve_color(CellColor::Named(258), &t, true),
            t.colors.fg_primary
        );
        assert_eq!(
            resolve_color(CellColor::Named(258), &t, false),
            t.colors.bg_canvas
        );
    }

    #[test]
    fn bright_and_dim_foreground_map_to_theme_text_slots() {
        let t = theme();
        // 267 BrightForeground -> primary text; 268 DimForeground -> muted text.
        assert_eq!(
            resolve_color(CellColor::Named(267), &t, true),
            t.colors.fg_primary
        );
        assert_eq!(
            resolve_color(CellColor::Named(268), &t, true),
            t.colors.fg_muted
        );
    }

    #[test]
    fn dim_ansi_is_darker_than_base() {
        let t = theme();
        let base = t.ansi.by_index(1); // red
        let dimmed = resolve_color(CellColor::Named(NAMED_DIM_FIRST + 1), &t, true); // dim red
        assert!(
            dimmed.r <= base.r && dimmed.g <= base.g && dimmed.b <= base.b,
            "dim red {dimmed:?} must be no brighter than base red {base:?}"
        );
        assert_ne!(
            dimmed, base,
            "dim should differ from base when base is non-zero"
        );
    }

    #[test]
    fn indexed_low_matches_ansi() {
        let t = theme();
        assert_eq!(resolve_indexed(7, &t), t.ansi.by_index(7));
    }

    #[test]
    fn indexed_cube_corners_match_xterm() {
        let t = theme();
        // 16 = (0,0,0) cube origin -> black; 231 = (5,5,5) -> white.
        assert_eq!(
            resolve_indexed(16, &t),
            Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 255
            }
        );
        assert_eq!(
            resolve_indexed(231, &t),
            Rgba {
                r: 255,
                g: 255,
                b: 255,
                a: 255
            }
        );
        // 196 = (5,0,0) -> pure cube red 255.
        assert_eq!(
            resolve_indexed(196, &t),
            Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            }
        );
    }

    #[test]
    fn indexed_grayscale_ramp_endpoints() {
        let t = theme();
        assert_eq!(
            resolve_indexed(232, &t),
            Rgba {
                r: 8,
                g: 8,
                b: 8,
                a: 255
            }
        );
        assert_eq!(
            resolve_indexed(255, &t),
            Rgba {
                r: 238,
                g: 238,
                b: 238,
                a: 255
            }
        );
    }

    #[test]
    fn build_grid_cells_skips_wide_spacer_and_flags_wide() {
        let t = theme();
        let mut snap = Snapshot::empty(1, 4);
        // Col 0: a wide (CJK) lead; col 1: its spacer; cols 2,3: plain.
        snap.cells[0].c = '\u{4e2d}';
        snap.cells[0].wide = true;
        snap.cells[1].wide_spacer = true;
        snap.cells[2].c = 'a';
        snap.cells[3].c = 'b';
        let mut out = Vec::new();
        build_grid_cells(&snap, &t, &mut out);
        // 3 cells emitted (the spacer is dropped).
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].col, 0);
        assert!(out[0].wide, "lead cell must be flagged wide");
        assert_eq!(out[0].ch, '\u{4e2d}');
        // The next emitted cell is col 2 (col 1 spacer skipped).
        assert_eq!(out[1].col, 2);
        assert_eq!(out[1].ch, 'a');
    }

    #[test]
    fn build_grid_cells_applies_inverse() {
        let t = theme();
        let mut snap = Snapshot::empty(1, 1);
        snap.cells[0].c = 'x';
        snap.cells[0].inverse = true;
        let fg = resolve_color(snap.cells[0].fg, &t, true);
        let bg = resolve_color(snap.cells[0].bg, &t, false);
        let mut out = Vec::new();
        build_grid_cells(&snap, &t, &mut out);
        assert_eq!(out[0].fg, bg, "inverse swaps fg<->bg");
        assert_eq!(out[0].bg, fg);
    }

    #[test]
    fn build_grid_cells_reuses_buffer() {
        let t = theme();
        let snap = Snapshot::empty(4, 10);
        let mut out = Vec::new();
        build_grid_cells(&snap, &t, &mut out);
        let cap = out.capacity();
        let ptr = out.as_ptr();
        build_grid_cells(&snap, &t, &mut out);
        assert_eq!(
            out.capacity(),
            cap,
            "stable dims must reuse the buffer (no realloc)"
        );
        assert_eq!(out.as_ptr(), ptr);
    }

    #[test]
    fn ascii_fast_path_predicate() {
        assert!(is_ascii_fast('a'));
        assert!(is_ascii_fast(' '));
        assert!(is_ascii_fast('~'));
        assert!(!is_ascii_fast('\u{4e2d}')); // CJK
        assert!(!is_ascii_fast('\t')); // control
        assert!(!is_ascii_fast('\u{e9}')); // accented
    }

    #[test]
    fn classify_run_fast_vs_shape() {
        assert_eq!(
            classify_run("ls -la README.md".chars()),
            RunLayout::AsciiFast
        );
        assert_eq!(classify_run("hi \u{4e2d}".chars()), RunLayout::Shape);
        // A ligature candidate is still ASCII bytes; it is classified fast here -
        // ligature SHAPING is a per-face decision the shaper makes, not this gate.
        assert_eq!(classify_run("=>".chars()), RunLayout::AsciiFast);
    }

    #[test]
    fn shelf_allocator_packs_without_overlap_and_fills() {
        let mut a = ShelfAllocator::new(10, 10);
        let r1 = a.alloc(4, 5).unwrap();
        let r2 = a.alloc(4, 5).unwrap();
        // Same shelf, side by side, no overlap.
        assert_eq!(r1.y, r2.y);
        assert!(r2.x >= r1.x + r1.w);
        // Third 4-wide does not fit the 10-wide shelf (4+4=8, +4=12) -> new shelf.
        let r3 = a.alloc(4, 5).unwrap();
        assert!(
            r3.y >= r1.y + 5,
            "overflowing the shelf opens a new row below"
        );
        // Too wide for the atlas -> None.
        assert!(a.alloc(11, 1).is_none());
    }

    #[test]
    fn shelf_allocator_reports_full() {
        let mut a = ShelfAllocator::new(4, 4);
        assert!(a.alloc(4, 4).is_some());
        // No vertical room left for another 4-tall shelf.
        assert!(a.alloc(4, 1).is_none());
    }

    #[test]
    fn glyph_cache_rasterizes_each_glyph_once() {
        let mut cache = GlyphCache::new(256, 256);
        let key = GlyphKey {
            glyph_id: 42,
            face: FaceStyle::Regular,
            px: 17,
        };
        let mut raster_calls = 0;
        let r1 = cache
            .get_or_insert(key, |_| raster_calls += 1, 8, 16)
            .unwrap();
        // Second lookup of the SAME key must NOT rasterize again (ticket AC).
        let r2 = cache
            .get_or_insert(key, |_| raster_calls += 1, 8, 16)
            .unwrap();
        assert_eq!(r1, r2, "a cached glyph returns the same atlas rect");
        assert_eq!(raster_calls, 1, "a cached glyph is rasterized exactly once");
        assert_eq!(cache.rasterizations(), 1);
        // A different face is a distinct key -> a second rasterization.
        let bold = GlyphKey {
            face: FaceStyle::Bold,
            ..key
        };
        cache
            .get_or_insert(bold, |_| raster_calls += 1, 8, 16)
            .unwrap();
        assert_eq!(cache.rasterizations(), 2);
    }

    #[test]
    fn glyph_cache_get_returns_hit_without_dimensions() {
        let mut cache = GlyphCache::new(64, 64);
        let key = GlyphKey {
            glyph_id: 7,
            face: FaceStyle::Regular,
            px: 13,
        };
        assert_eq!(cache.get(&key), None, "miss before insert");
        let rect = cache.get_or_insert(key, |_| {}, 5, 9).unwrap();
        // `get` returns the same rect a fresh `get_or_insert` would, with no (w, h).
        assert_eq!(cache.get(&key), Some(rect));
    }

    #[test]
    fn face_style_from_flags() {
        assert_eq!(FaceStyle::from_flags(false, false), FaceStyle::Regular);
        assert_eq!(FaceStyle::from_flags(true, false), FaceStyle::Bold);
        assert_eq!(FaceStyle::from_flags(false, true), FaceStyle::Italic);
        assert_eq!(FaceStyle::from_flags(true, true), FaceStyle::BoldItalic);
    }
}
