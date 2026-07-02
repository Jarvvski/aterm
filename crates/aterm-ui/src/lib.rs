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
pub mod approval_render;
// The shared glyph atlas + glyph pipeline seam (T-4.3). The module is private. The
// `GlyphAtlas` TYPE is re-exported below ONLY so the pub `ProseRenderer` can name it in
// its signatures (like the pub `GridRenderer`); its methods and constructor are all
// crate-internal, so externally it is an un-constructable opaque shell - the prose
// render path is driven only from within the crate until the atlas hoists up to the
// live `GpuRenderer` (T-4.6).
mod atlas;
mod cell_render;
pub mod completion_render;
pub mod components;
pub mod constraint;
pub mod fonts;
pub mod glyph;
pub mod gpu;
pub mod grid_render;
pub mod hit;
pub mod ime;
pub mod indicator;
pub mod input_widget;
pub mod overlay;
pub mod present;
pub mod profiling;
pub mod prose;
pub mod recorder;
pub mod renderer;
pub mod screens;
pub mod sprite;
pub mod text;
pub mod timeline;
pub mod timeline_render;
pub mod title_bar;
pub mod widgets;
pub mod window;
pub mod window_frame;

pub use app::{
    run, run_with, run_with_recorder, AtermApp, HeadlessCallbacks, KeyPress, Mods, RenderConfig,
    ScrollCommand, UiCallbacks,
};
pub use approval_render::{ApprovalRenderer, ApprovalView};
pub use atlas::GlyphAtlas;
pub use completion_render::CompletionRenderer;
pub use components::{
    AgentCardStyle, Animation, AutonomyChip, AutonomyMode, ChipStyle, ChipVariant,
    CommandBlockStyle, GutterShape, GutterStyle, MotionSpec, PromptChip, PromptMode, RiskBadge,
    RiskState,
};
pub use constraint::{Align, Constraint, Placed, Sizing};
pub use glyph::{CellMetrics, GlyphRasterizer, RasterGlyph};
pub use gpu::GpuRenderer;
pub use grid_render::{FrameSize, GridRenderer};
pub use hit::{HitMap, HitRect, HitTarget, WindowControl};
pub use ime::{ImeEvent, NativeTextInput};
pub use indicator::IntegrationIndicator;
pub use input_widget::InputWidgetRenderer;
pub use overlay::{OverlayRequest, OverlayResult, OverlayWorker, DEFAULT_DEBOUNCE};
pub use present::{DisplayLink, FrameDecision, PresentScheduler, DEFAULT_KEEP_WARM};
pub use prose::{measure_px, PositionedGlyph, ProseLayout, ProseRenderer, ProseShaper, MEASURE_CH};
pub use recorder::{
    FrameRecorder, FrameSample, FrameStats, FrameTiming, Refresh, DEFAULT_CAPACITY,
    DEFAULT_TOLERANCE_MS,
};
pub use renderer::{Frame, RenderError, Renderer};
pub use screens::{ScreenKind, ScreensRenderer};
pub use text::{
    build_grid_cells, classify_run, is_ascii_fast, resolve_color, AtlasRect, FaceStyle, FontFamily,
    GlyphCache, GlyphKey, GridCell, RunLayout, ShelfAllocator,
};
pub use timeline::{
    layout as timeline_layout, visible_block_count, GutterMarker, Scroll, ScrollState,
    TimelineLayout, TimelineMode, TimelineRow, VisibleBlock,
};
pub use timeline_render::TimelineRenderer;
pub use title_bar::{TitleBarRenderer, TitleBarView, TITLE_BAR_LOGICAL};
pub use window_frame::{WindowFrameRenderer, WINDOW_RADIUS_LOGICAL};

// Re-export the winit key types the host app needs for input routing.
pub use winit::keyboard::NamedKey;
pub use winit::window::Window;

// Re-export the theme selector so the host app picks a theme without a direct
// tokens dependency.
pub use aterm_tokens::ThemeKind;
