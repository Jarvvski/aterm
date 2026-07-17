//! The renderer SEAM. `Renderer` is the swappable interface the app calls once
//! per frame; the wgpu implementation lives in [`crate::gpu`]. Keeping this a
//! trait means a future software/test renderer (or a different GPU backend) can
//! drop in without touching the app's frame loop, and keeps the 60fps fast-path
//! behind a stable surface.

use aterm_core::{BlockList, Completion, InputModel, Integration, Snapshot};
use aterm_tokens::Theme;

use crate::approval_render::ApprovalView;
use crate::components::AutonomyMode;
use crate::editor::EditorView;
use crate::sidebar::SidebarView;
use crate::title_bar::TitleBarView;

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
    /// The first-class editor surface for this frame. When present it replaces the terminal
    /// timeline, raw grid, informational screens, and unified input while retaining window
    /// chrome. The borrowed view carries no file transport and allocates nothing.
    pub editor: Option<EditorView<'a>>,
    /// The autonomy posture to show in the always-visible indicator this frame
    /// (ticket T-5.11), or `None` for a host with no agent (e.g. the headless UI), in
    /// which case no autonomy chip is drawn. `Copy`, so it rides along by value with
    /// no per-frame allocation. The host supplies it through
    /// [`crate::app::UiCallbacks::autonomy_mode`]; `aterm-app` maps its
    /// `aterm_agent::AutonomyMode` onto this UI-local one.
    pub autonomy: Option<AutonomyMode>,
    /// The custom title-bar content for this frame (ticket T-9.2): the active title + cwd,
    /// borrowed from the host. `None` for a host with no chrome (e.g. the headless UI), in
    /// which case no title bar is drawn and the timeline uses the full height. The renderer
    /// draws it over a reserved top band in normal (non-alt-screen) mode; the host supplies
    /// it through [`crate::app::UiCallbacks::title_bar`].
    pub title_bar: Option<TitleBarView<'a>>,
    /// The sessions sidebar for this frame, or `None` while closed. The borrowed rows
    /// are a retained host projection, so carrying the panel through the renderer seam
    /// allocates nothing per frame (ticket T-10.2).
    pub sidebar: Option<SidebarView<'a>>,
    /// The tab-completion popover state for this frame (ticket T-9.5), borrowed from the
    /// host's [`Completion`]. `None` for a host with no completion (e.g. the headless UI);
    /// when `Some` and open, the renderer draws the fuzzy-finder popover above the input's
    /// left edge. Supplied through [`crate::app::UiCallbacks::completion`].
    pub completion: Option<&'a Completion>,
    /// Whether to draw the `modes` explainer screen this frame (ticket T-9.5), in place of
    /// the timeline. `false` normally; the host toggles it on demand (a help affordance).
    /// The `launch` empty state is derived by the renderer from an empty block list, so it
    /// needs no flag. Supplied through [`crate::app::UiCallbacks::show_help`].
    pub show_help: bool,
    /// The risk-gate approval card to draw this frame (ticket T-9.7), or `None` when no agent
    /// turn is parked on a `RequireConfirm` verdict. When `Some`, the renderer draws the
    /// caution card + split Approve/Reject over the input (like the completion popover). The
    /// host projects it from the pending approval (SANITIZED in `aterm-app`, so no raw secret
    /// crosses the arrow) via [`crate::app::UiCallbacks::approval`]. `Copy` (borrowed strs),
    /// so it rides along with no per-frame allocation.
    pub approval: Option<ApprovalView<'a>>,
}

/// The swappable renderer seam.
pub trait Renderer {
    /// React to a window resize (reconfigure the surface / viewport).
    fn resize(&mut self, width: u32, height: u32);

    /// Render exactly one frame. Must clear to `frame.theme`'s canvas color even
    /// when there is nothing else to draw.
    fn render(&mut self, frame: Frame<'_>) -> Result<(), RenderError>;
}
