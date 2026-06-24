//! aterm-ui — the renderer SEAM.
//!
//! Opens a `winit` 0.30 window, brings up a `wgpu` device/surface, and runs the
//! event loop, clearing every frame to the active `aterm-tokens` theme's canvas
//! color. The [`Renderer`] trait is the swappable seam; [`gpu::GpuRenderer`] is
//! the wgpu implementation, which draws the terminal grid snapshot through the
//! instanced glyph-atlas pipeline ([`grid_render::GridRenderer`], swash-rasterized).
//!
//! Depends on `aterm-core` (the grid snapshot) and `aterm-tokens` (colors/fonts).

// Test-only heap-allocation counter for the steady-state no-alloc assertions
// (ticket T-1.8 AC2 / T-1.5 AC5). Compiled out of every non-test build.
#[cfg(test)]
mod alloc_probe;

pub mod app;
pub mod fonts;
pub mod glyph;
pub mod gpu;
pub mod grid_render;
pub mod indicator;
pub mod present;
pub mod profiling;
pub mod renderer;
pub mod text;
pub mod timeline;
pub mod widgets;
pub mod window;

pub use app::{run, run_with, AtermApp, HeadlessCallbacks, RenderConfig, UiCallbacks};
pub use glyph::{CellMetrics, GlyphRasterizer, RasterGlyph};
pub use gpu::GpuRenderer;
pub use grid_render::{FrameSize, GridRenderer};
pub use indicator::IntegrationIndicator;
pub use present::{DisplayLink, FrameDecision, PresentScheduler, DEFAULT_KEEP_WARM};
pub use renderer::{Frame, RenderError, Renderer};
pub use text::{
    build_grid_cells, classify_run, is_ascii_fast, resolve_color, AtlasRect, FaceStyle, GlyphCache,
    GlyphKey, GridCell, RunLayout, ShelfAllocator,
};
pub use timeline::{
    layout as timeline_layout, visible_block_count, GutterMarker, Scroll, TimelineLayout,
    TimelineMode, TimelineRow, VisibleBlock,
};

// Re-export the winit key types the host app needs for input routing.
pub use winit::keyboard::NamedKey;
pub use winit::window::Window;

// Re-export the theme selector so the host app picks a theme without a direct
// tokens dependency.
pub use aterm_tokens::ThemeKind;
