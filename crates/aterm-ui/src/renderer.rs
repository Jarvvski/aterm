//! The renderer SEAM. `Renderer` is the swappable interface the app calls once
//! per frame; the wgpu implementation lives in [`crate::gpu`]. Keeping this a
//! trait means a future software/test renderer (or a different GPU backend) can
//! drop in without touching the app's frame loop, and keeps the 60fps fast-path
//! behind a stable surface.

use aterm_core::{BlockList, InputModel, Integration, Snapshot};
use aterm_tokens::Theme;

/// Errors a renderer can surface during a frame.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("surface lost or outdated; reconfigure needed")]
    SurfaceLost,
    #[error("surface out of memory")]
    OutOfMemory,
    #[error("render backend error: {0}")]
    Backend(String),
}

/// One frame's worth of input to the renderer.
///
/// For the scaffold the only required content is the active theme (used for the
/// clear color). The terminal grid `snapshot` is optional: when present and the
/// renderer has a text fast-path, it is drawn; otherwise it is ignored (the
/// window still clears to the paper background).
pub struct Frame<'a> {
    pub theme: &'a Theme,
    pub snapshot: Option<&'a Snapshot>,
    /// The published block list for the virtualized timeline (ticket T-2.7), borrowed
    /// from the host's `Arc<BlockList>` for the duration of the frame. `None` for a
    /// host with no engine (e.g. the headless UI). The renderer virtualizes it via the
    /// SumTree ([`crate::timeline`]); the host supplies it through
    /// [`crate::app::UiCallbacks::blocks`].
    pub blocks: Option<&'a BlockList>,
    /// The shell-integration indicator state for this frame (ticket T-2.6). The
    /// renderer maps it to a glyph + tooltip via [`crate::indicator::
    /// IntegrationIndicator`]; the host supplies it through
    /// [`crate::app::UiCallbacks::integration_status`]. `Copy`, so it rides along by
    /// value with no per-frame allocation.
    pub integration: Integration,
    /// The unified-input state for this frame (ticket T-3.6), borrowed from the host's
    /// `Session`-owned [`InputModel`]. `None` for a host with no input (e.g. the headless
    /// UI), in which case the renderer draws no input box and the timeline/grid uses the
    /// full window. The renderer reads only the model's accessors (text/caret/selection/
    /// mode/ghost/preedit/highlight) - it never mutates it. The host supplies it through
    /// [`crate::app::UiCallbacks::input`].
    pub input: Option<&'a InputModel>,
}

/// The swappable renderer seam.
pub trait Renderer {
    /// React to a window resize (reconfigure the surface / viewport).
    fn resize(&mut self, width: u32, height: u32);

    /// Render exactly one frame. Must clear to `frame.theme`'s canvas color even
    /// when there is nothing else to draw.
    fn render(&mut self, frame: Frame<'_>) -> Result<(), RenderError>;
}
