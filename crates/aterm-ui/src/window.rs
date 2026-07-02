//! Window setup: winit window attributes and the pixel↔grid geometry math.
//!
//! Kept separate from [`crate::app`] (the event loop) and [`crate::gpu`] (the
//! surface) so window construction and the cell-size arithmetic are in one place.
//! The geometry helpers are pure and unit-tested.
//!
//! ## Native transparent titlebar (ticket T-9.9)
//! On macOS the window is the standard "transparent titlebar" style every native-feeling
//! app uses (kitty, Slack, Linear; alacritty's `decorations = "Transparent"` sets the
//! identical trio): the window stays `.titled` - native rounded corners, the native drop
//! shadow, and the REAL traffic-light buttons - while `titlebar_transparent` +
//! `title_hidden` + `fullsize_content_view` make macOS paint NO titlebar background, no
//! title text, and no separator, so aterm's custom bar (T-9.2) is the only visible bar and
//! the native buttons float over it. Measured on macOS 15 (see the T-9.9 ticket notes):
//! pointer events in the titlebar band reach the CONTENT view (only the button widgets
//! themselves intercept), so the bar's sidebar glyph stays hover/clickable; nothing drags
//! automatically, so [`crate::app`] starts an explicit `Window::drag_window` on a
//! title-bar-band press that hits no target (the Zed pattern). We deliberately do NOT set
//! `movable_by_window_background`: on macOS that starts an AppKit background-drag loop on
//! any press-with-drift and swallows the terminating `mouseUp`, losing a drifted click.
//! The buttons sit at AppKit's fixed geometry - centered in the standard 28pt band, the
//! cluster ending at x=61pt - which [`crate::title_bar`] aligns to (bar height 28,
//! content inset 71).

use winit::dpi::LogicalSize;
use winit::window::{Window, WindowAttributes};

#[cfg(target_os = "macos")]
use winit::platform::macos::WindowAttributesExtMacOS;

/// Default window size in logical points (matches the old inline scaffold value).
pub const DEFAULT_LOGICAL_SIZE: (f64, f64) = (960.0, 600.0);

/// Build the winit attributes for the main window: title + size + (on macOS) the native
/// transparent-titlebar chrome (ticket T-9.9). The title still names the window in the
/// app switcher / Mission Control even though the titlebar draws no text. Non-macOS keeps
/// the plain titled window (v1 is macOS-first).
#[must_use]
pub fn window_attributes(title: &str) -> WindowAttributes {
    let attrs = Window::default_attributes()
        .with_title(title)
        .with_inner_size(LogicalSize::new(
            DEFAULT_LOGICAL_SIZE.0,
            DEFAULT_LOGICAL_SIZE.1,
        ));
    #[cfg(target_os = "macos")]
    let attrs = attrs
        .with_titlebar_transparent(true)
        .with_title_hidden(true)
        .with_fullsize_content_view(true);
    attrs
}

/// One grid cell's size in **physical** pixels for the active grid type style.
///
/// iM Writing Mono's advance is ~0.6em; the line box is `size * line_height`. The
/// result feeds the pixel→(cols, rows) translation on resize. Returned components
/// are clamped to >= 1.0 so the division below can never divide by zero.
#[must_use]
pub fn cell_px(scale: f32) -> (f32, f32) {
    let g = aterm_tokens::type_scale::GRID;
    let w = g.size_pt * 0.6 * scale;
    let h = g.size_pt * g.line_height * scale;
    (w.max(1.0), h.max(1.0))
}

/// Translate a physical-pixel surface size into a `(cols, rows)` terminal grid,
/// flooring to whole cells and clamping to at least 1×1 so a PTY resize is always
/// valid even for a degenerate (zero/tiny) window.
#[must_use]
pub fn grid_dims(width_px: u32, height_px: u32, scale: f32) -> (u16, u16) {
    let (cw, ch) = cell_px(scale);
    let cols = ((width_px as f32) / cw).floor().max(1.0);
    let rows = ((height_px as f32) / ch).floor().max(1.0);
    // The floor()+max(1.0) above keep this in a sane range; the `as u16` is a
    // saturating-ish cast guarded by min() so an enormous surface can't wrap.
    let cols = cols.min(u16::MAX as f32) as u16;
    let rows = rows.min(u16::MAX as f32) as u16;
    (cols, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_px_is_positive_and_scales() {
        let (w1, h1) = cell_px(1.0);
        assert!(w1 >= 1.0 && h1 >= 1.0);
        let (w2, h2) = cell_px(2.0);
        // Retina (2x) cells are wider/taller than 1x.
        assert!(w2 > w1 && h2 > h1);
    }

    #[test]
    fn grid_dims_floor_to_whole_cells() {
        let (cw, ch) = cell_px(1.0);
        // A surface exactly 10 cells wide / 5 tall (plus a sub-cell sliver that
        // must be floored away) yields exactly 10x5.
        let w = (cw * 10.0 + cw * 0.4).round() as u32;
        let h = (ch * 5.0 + ch * 0.4).round() as u32;
        let (cols, rows) = grid_dims(w, h, 1.0);
        assert_eq!(cols, 10);
        assert_eq!(rows, 5);
    }

    #[test]
    fn grid_dims_clamps_degenerate_window_to_one_cell() {
        // A zero-size surface (mid-minimize) must still produce a valid 1x1 PTY
        // size, never 0 (which a PTY would reject) and never a divide-by-zero.
        assert_eq!(grid_dims(0, 0, 1.0), (1, 1));
        assert_eq!(grid_dims(1, 1, 1.0), (1, 1));
    }

    #[test]
    fn grid_dims_does_not_wrap_on_huge_surface() {
        // An absurd surface must saturate at u16::MAX, not wrap to a small number.
        let (cols, rows) = grid_dims(u32::MAX, u32::MAX, 1.0);
        assert_eq!(cols, u16::MAX);
        assert_eq!(rows, u16::MAX);
    }

    #[test]
    fn window_attributes_set_title() {
        let attrs = window_attributes("aterm-test");
        assert_eq!(attrs.title, "aterm-test");
    }
}
