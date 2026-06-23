//! aterm-ui — the renderer SEAM.
//!
//! Opens a `winit` 0.30 window, brings up a `wgpu` device/surface, and runs the
//! event loop, clearing every frame to the active `aterm-tokens` theme's canvas
//! color. The [`Renderer`] trait is the swappable seam; [`gpu::GpuRenderer`] is
//! the wgpu implementation, which includes a `glyphon` text fast-path (stretch)
//! for drawing the terminal grid snapshot.
//!
//! Depends on `aterm-core` (the grid snapshot) and `aterm-tokens` (colors/fonts).

pub mod app;
pub mod fonts;
pub mod gpu;
pub mod present;
pub mod renderer;
pub mod widgets;
pub mod window;

pub use app::{run, AtermApp, HeadlessCallbacks, UiCallbacks};
pub use gpu::GpuRenderer;
pub use present::{FrameDecision, PresentScheduler, DEFAULT_KEEP_WARM};
pub use renderer::{Frame, RenderError, Renderer};

// Re-export the winit key types the host app needs for input routing.
pub use winit::keyboard::NamedKey;
pub use winit::window::Window;

// Re-export the theme selector so the host app picks a theme without a direct
// tokens dependency.
pub use aterm_tokens::ThemeKind;
