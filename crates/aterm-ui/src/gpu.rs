//! The wgpu implementation of the [`Renderer`] seam.
//!
//! `GpuRenderer` owns the wgpu device/queue/surface and the shared [`GlyphAtlas`] (the
//! T-4.6 hoist), and clears every frame to the active theme's canvas color (the hard
//! requirement). It then draws ONE primary view through the atlas:
//!
//! - **The block timeline** ([`crate::timeline_render::TimelineRenderer`]) in normal
//!   mode - finished + running command blocks from the published block model, styled to
//!   the iA component spec (ticket T-4.6). The running block carries its live output
//!   (the engine's incremental capture), so a streaming command shows in the timeline.
//! - **The raw terminal grid** ([`crate::grid_render::GridRenderer`]) when a full-screen
//!   app owns the screen (alt-screen, ADR-0007) or there is no engine (the headless
//!   stand-in) - the per-cell instanced glyph fast-path (ticket T-1.8 / T-1.6).
//!
//! Both share one atlas + one rect/glyph pipeline pair; the timeline path is gated so an
//! idle present allocates nothing (the 60fps floor, T-1.8).
//!
//! On top of the primary view, in normal mode, the **unified input box**
//! ([`crate::input_widget::InputWidgetRenderer`], ticket T-3.6) draws over a reserved
//! bottom zone: the live pre-submit command line, the mode-carrying prompt glyph + chip,
//! the caret, ghost text, and preedit. It reads the host's `InputModel` ([`Frame::input`])
//! and is the single on-screen home of the in-progress line (the raw grid, with the
//! shell's own echo, is not drawn in normal mode, so there is no double echo). The
//! timeline viewport is shrunk by [`crate::input_widget::zone_px`] so the two never
//! overlap; in alt-screen the box is hidden (the full-screen app owns input).

use std::sync::Arc;

use winit::window::Window;

use aterm_tokens::Rgba;

use crate::atlas::GlyphAtlas;
use crate::grid_render::GridRenderer;
use crate::hit::{HitMap, HitTarget};
use crate::renderer::{Frame, RenderError, Renderer};

/// The timeline idle-gate signature (ticket T-2.7, extended by T-9.8): `(snapshot version,
/// scroll offset, surface width, effective height, theme code, alt placeholder, hovered
/// block)`. A tuple (not a struct) so equality is derived; aliased to keep the field type
/// legible.
type TimelineSig = (u64, u64, u32, u32, u8, bool, Option<usize>);

/// wgpu-backed renderer with the instanced grid fast-path.
pub struct GpuRenderer {
    // Surface must be declared before `window` so it drops first.
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// The shared glyph engine (atlas texture + cache + rasterizer + the rect & glyph
    /// pipelines + the group(0) viewport uniform). Owned here (the T-4.6 hoist) and lent
    /// to every front-end - the grid, and the timeline/prose paths that join it - so all
    /// draw through one atlas and one pair of pipelines.
    atlas: GlyphAtlas,
    /// The instanced terminal-grid front-end (bg/glyph instance build + version-gated
    /// rebuild, ticket T-1.8); draws through the shared `atlas`. The PRIMARY view in
    /// `alt_screen` mode (a full-screen app owns the screen); the block timeline is the
    /// primary view otherwise (T-4.6).
    grid: GridRenderer,
    /// The block-timeline front-end (ticket T-4.6): the primary on-screen view in normal
    /// (non-alt-screen) mode, drawing finished + running blocks from the block model
    /// through the shared `atlas`.
    timeline: crate::timeline_render::TimelineRenderer,
    /// The unified-input box front-end (ticket T-3.6): drawn over the bottom zone in
    /// normal (non-alt-screen) mode when the host supplies an `InputModel`. It is
    /// self-gated (its own damage signature), so it allocates nothing on an idle present;
    /// the timeline viewport is shrunk by its [`crate::input_widget::zone_px`] so the two
    /// never overlap.
    input: crate::input_widget::InputWidgetRenderer,
    /// The custom title-bar front-end (ticket T-9.2): drawn over a reserved TOP band in
    /// normal (non-alt-screen) mode when the host supplies title-bar content. Self-gated
    /// (its own damage signature), so it allocates nothing on an idle present; the timeline
    /// is laid out below it via a top inset of [`crate::title_bar::title_bar_px`].
    title: crate::title_bar::TitleBarRenderer,
    /// The informational-screens front-end (ticket T-9.5): the `launch` empty state (drawn
    /// when the block timeline is empty) and the `modes` explainer (drawn on `show_help`),
    /// centered in the content band between the title bar and the input box.
    screens: crate::screens::ScreensRenderer,
    /// The tab-completion popover front-end (ticket T-9.5): the fuzzy finder drawn above the
    /// input's left edge when the host's completion state is open.
    completion: crate::completion_render::CompletionRenderer,
    /// The risk-gate approval card front-end (ticket T-9.7): the caution card + split
    /// Approve/Reject drawn over the input while an agent turn is parked on a
    /// `RequireConfirm` verdict. Self-gated (its own damage signature), so a parked frame
    /// allocates nothing.
    approval: crate::approval_render::ApprovalRenderer,
    /// Idle gate for the timeline path: the `(snapshot version, scroll, viewport, theme,
    /// alt, hovered-block)` signature last laid out + prepared. When unchanged, the per-frame
    /// `timeline::layout` (which allocates) + prepare are skipped and the prior timeline
    /// instances are redrawn - so an idle present allocates nothing (the T-1.8 floor). The
    /// hovered block (T-9.8) is folded in so a pointer hover that toggles a block-meta reveal
    /// forces exactly one relayout.
    timeline_sig: Option<TimelineSig>,
    /// Virtualized-timeline scroll controller (ticket T-2.7). Follows the bottom (the
    /// live-terminal default) until the user scrolls; the wheel / PageUp / PageDown
    /// bindings that drive it are wired in [`crate::app`] (ticket T-7.2).
    scroll: crate::timeline::ScrollState,
    /// Blocks that built timeline geometry on the last drawn frame - the AC1
    /// virtualization counter (ticket T-2.7), exposed via
    /// [`GpuRenderer::visible_block_count`] for tests / a future status line. `0`
    /// until a block list is published.
    last_visible_blocks: usize,
    /// Which front-end drew the last frame - so [`Self::last_glyph_draw_calls`] reports
    /// the active PRIMARY path's counter (the timeline in normal mode, the grid in
    /// alt-screen). The input box, when shown, is an ADDITIONAL one-glyph-draw layer on
    /// top; its counter lives on the input front-end.
    drew_timeline: bool,
    /// Whether the input box drew on the last frame (it is drawn over the bottom zone in
    /// normal mode when the host supplies an `InputModel`).
    drew_input: bool,
    /// Whether the title bar drew on the last frame (drawn over the top band in normal mode
    /// when the host supplies title-bar content).
    drew_title: bool,
    /// Whether the raw grid drew as the PRIMARY view on the last frame (alt-screen or the
    /// no-engine headless stand-in). Tracked separately from `drew_timeline` so the glyph
    /// draw-call counter attributes the primary view correctly across all modes.
    drew_grid: bool,
    /// Whether an informational screen (launch / modes) drew on the last frame (T-9.5).
    drew_screens: bool,
    /// Whether the completion popover drew on the last frame (T-9.5).
    drew_completion: bool,
    /// Whether the risk-gate approval card drew on the last frame (T-9.7).
    drew_approval: bool,
    /// The frame's clickable regions (ticket T-9.8), rebuilt each present from every drawn
    /// front-end's cached geometry (in draw order, topmost last) and queried by the host's
    /// pointer path via [`Self::hit_test`]. Reused (clear + push), so a warm present's
    /// rebuild allocates nothing; it persists untouched across idle frames.
    hit_map: HitMap,
    /// The target the pointer currently hovers (ticket T-9.8), set by the host via
    /// [`Self::set_hover`] before a redraw and read here to drive each front-end's hover
    /// treatment. `None` when the pointer is over nothing clickable / off-window.
    hovered: Option<HitTarget>,
    // Keep the window alive for the static-lifetime surface.
    _window: Arc<Window>,
    scale_factor: f32,
}

impl GpuRenderer {
    /// Initialize the GPU stack for `window`. Blocks on adapter/device requests
    /// via `pollster` (one-time setup, not on the frame path).
    pub fn new(window: Arc<Window>) -> Result<Self, RenderError> {
        let size = window.inner_size();
        let width = size.width.max(1);
        let height = size.height.max(1);
        let scale_factor = window.scale_factor() as f32;

        // `InstanceDescriptor` has no `Default` in wgpu 29; `Instance::default()`
        // picks sensible backends (Metal on macOS) for us.
        let instance = wgpu::Instance::default();

        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| RenderError::Backend(format!("create_surface: {e}")))?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
        }))
        .map_err(|e| RenderError::Backend(format!("request_adapter: {e}")))?;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("aterm-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .map_err(|e| RenderError::Backend(format!("request_device: {e}")))?;

        let caps = surface.get_capabilities(&adapter);
        // Prefer an sRGB format so our linear clear + linear instance colors are
        // presented correctly (the shader output is encoded to sRGB on store).
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            present_mode: wgpu::PresentMode::Fifo, // vsync — the 60fps floor anchor
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let atlas = GlyphAtlas::new(&device, format);
        let grid = GridRenderer::new(&device);
        let timeline = crate::timeline_render::TimelineRenderer::new(&device);
        let input = crate::input_widget::InputWidgetRenderer::new(&device);
        let title = crate::title_bar::TitleBarRenderer::new(&device);
        let screens = crate::screens::ScreensRenderer::new(&device);
        let completion = crate::completion_render::CompletionRenderer::new(&device);
        let approval = crate::approval_render::ApprovalRenderer::new(&device);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            atlas,
            grid,
            timeline,
            input,
            title,
            screens,
            completion,
            approval,
            timeline_sig: None,
            scroll: crate::timeline::ScrollState::default(),
            last_visible_blocks: 0,
            drew_timeline: false,
            drew_input: false,
            drew_title: false,
            drew_grid: false,
            drew_screens: false,
            drew_completion: false,
            drew_approval: false,
            hit_map: HitMap::new(),
            hovered: None,
            _window: window,
            scale_factor,
        })
    }

    /// The number of blocks that built timeline geometry on the last drawn frame -
    /// the AC1 virtualization counter (ticket T-2.7).
    #[must_use]
    pub fn visible_block_count(&self) -> usize {
        self.last_visible_blocks
    }

    /// Scroll the block timeline by a signed number of display rows (NEGATIVE = up,
    /// toward older output; POSITIVE = down, toward the newest). Breaks the
    /// follow-bottom lock; the offset is clamped and finalized on the next frame's
    /// `render`. The wheel binding in [`crate::app`] drives this (ticket T-7.2).
    pub fn scroll_by_rows(&mut self, delta: i64) {
        self.scroll.scroll_by_rows(delta);
    }

    /// Page the timeline by `dir` viewports (`-1` = PageUp/older, `+1` =
    /// PageDown/newer). Driven by the PageUp / PageDown key bindings (ticket T-7.2).
    pub fn scroll_page(&mut self, dir: i64) {
        self.scroll.page(dir);
    }

    /// Jump the timeline to the oldest block (top); breaks the follow-bottom lock.
    pub fn scroll_to_top(&mut self) {
        self.scroll.to_top();
    }

    /// Jump the timeline to the newest output and re-engage the follow-bottom lock.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll.to_bottom();
    }

    /// Whether the timeline is currently following (pinned to) the newest output.
    #[must_use]
    pub fn is_following_bottom(&self) -> bool {
        self.scroll.is_following_bottom()
    }

    /// The clickable target at `(x, y)` in physical px, or `None` (ticket T-9.8). Queries the
    /// last-built hit map, so it reflects the most recent present's geometry - valid to call
    /// between frames (the map persists across idle presents). The host's pointer path uses
    /// this for hover tracking and click dispatch.
    #[must_use]
    pub fn hit_test(&self, x: f32, y: f32) -> Option<HitTarget> {
        self.hit_map.hit(x, y)
    }

    /// The target the pointer currently hovers (ticket T-9.8).
    #[must_use]
    pub fn hovered(&self) -> Option<HitTarget> {
        self.hovered
    }

    /// Set the hovered target (ticket T-9.8) and report whether it CHANGED. The host calls
    /// this from its `CursorMoved`/`CursorLeft` handling; on a `true` return it re-arms
    /// keep-warm and requests a redraw so the new hover treatment paints. A `false` return
    /// (steady hover) must NOT force a redraw - the unchanged-frame early-out then holds and
    /// the frame allocates nothing (the T-1.8 invariant).
    pub fn set_hover(&mut self, hovered: Option<HitTarget>) -> bool {
        if self.hovered == hovered {
            return false;
        }
        self.hovered = hovered;
        true
    }

    /// The input caret's rect in PHYSICAL px `[x, y, w, h]` when the input box drew on
    /// the last frame, else `None` (alt-screen, or a host with no input). [`crate::app`]
    /// feeds this to `Window::set_ime_cursor_area` so the IME candidate window sits under
    /// the caret (ticket T-3.2).
    #[must_use]
    pub fn ime_cursor_area(&self) -> Option<[f32; 4]> {
        if self.drew_input {
            self.input.caret_area_px()
        } else {
            None
        }
    }

    /// Glyph-layer draw calls from the last frame: the sum of each layer that drew, one per
    /// layer that has text (T-1.6 AC c). The PRIMARY view (timeline in normal mode, grid in
    /// alt-screen / headless, or an informational screen - launch / modes, T-9.5), plus the
    /// input box (T-3.6), the completion popover (T-9.5), and the title bar (T-9.2) when each
    /// is shown. So a normal frame with input + chrome is 3 (timeline + box + title bar), 4
    /// with the completion popover open; an alt-screen frame is 1 (the grid, no box/chrome).
    /// Exposed for tests / instrumentation.
    #[must_use]
    pub fn last_glyph_draw_calls(&self) -> u32 {
        let mut n = 0;
        if self.drew_timeline {
            n += self.timeline.last_glyph_draw_calls();
        }
        if self.drew_grid {
            n += self.grid.last_glyph_draw_calls();
        }
        if self.drew_screens {
            n += self.screens.last_glyph_draw_calls();
        }
        if self.drew_input {
            n += self.input.last_glyph_draw_calls();
        }
        if self.drew_completion {
            n += self.completion.last_glyph_draw_calls();
        }
        if self.drew_approval {
            n += self.approval.last_glyph_draw_calls();
        }
        if self.drew_title {
            n += self.title.last_glyph_draw_calls();
        }
        n
    }

    /// Render the snapshot grid (and always clear). Split out so `render` reads
    /// cleanly.
    fn render_inner(&mut self, frame: Frame<'_>) -> Result<(), RenderError> {
        // Tracy frame zone (ticket T-1.8 AC4); zero-cost with no subscriber.
        let _frame_zone = tracing::trace_span!("frame").entered();
        let clear = linear_to_wgpu(frame.theme.colors.bg_canvas);

        // The pointer's current hover (ticket T-9.8), read once into a local (Copy) so the
        // disjoint front-end borrows in the build block below don't conflict with `self`.
        // Front-ends fold their relevant slice of it into their own damage signatures.
        // While the risk-gate approval card is up it is a MODAL over the pointer: the hit map
        // is emptied below so nothing behind it is clickable, so the hover TREATMENTS clear
        // too (a stale brightened glyph / lifted chip would imply a clickable target that is
        // not) - hover implies clickable. Forcing `None` flips each front-end's folded hover
        // bit, so they rebuild once to the resting look while the modal owns pointer input.
        let hovered = if frame.approval.is_some() {
            None
        } else {
            self.hovered
        };
        let hovered_block = match hovered {
            Some(HitTarget::BlockMeta(i)) => Some(i),
            _ => None,
        };

        // Resolve the shell-integration indicator (ticket T-2.6) so the state reaches
        // the renderer and the presentation seam is exercised every frame. Drawing it
        // (a glyph + tooltip in the gutter/status strip) is EPIC-4 visual polish.
        let _indicator =
            crate::indicator::IntegrationIndicator::resolve(frame.integration, frame.theme);

        // Choose the primary view (ticket T-4.6). Normally the BLOCK TIMELINE is drawn
        // (finished + running blocks from the block model - the running block carries
        // its live output via the engine's incremental capture). The RAW GRID is drawn
        // only when a full-screen app owns the screen (alt-screen, ADR-0007) or there is
        // no engine (the headless / no-blocks stand-in).
        let alt_screen = frame.snapshot.is_some_and(|s| s.alt_screen);
        // The `modes` explainer (T-9.5) replaces the timeline on demand; the `launch` empty
        // state (T-9.5) shows when the block timeline is empty. Both are drawn by the
        // screens front-end, centered in the content band. Neither shows in alt-screen.
        let show_modes = !alt_screen && frame.show_help;
        let blocks_empty = frame.blocks.is_some_and(aterm_core::BlockList::is_empty);
        let show_launch = !alt_screen && !show_modes && blocks_empty;
        let draw_screens = show_modes || show_launch;
        let screen_kind = if show_modes {
            crate::screens::ScreenKind::Modes
        } else {
            crate::screens::ScreenKind::Launch
        };
        // The block timeline is the primary view except in alt-screen, when a screen replaces
        // it (modes), or when there is no engine (headless -> the grid). Under `launch` the
        // timeline is still "drawn" but is empty, so the screens splash sits over a blank
        // canvas.
        let draw_timeline = !alt_screen && !show_modes && frame.blocks.is_some();
        // The raw grid is the PRIMARY view only in alt-screen (a full-screen app owns the
        // screen, ADR-0007) or the no-engine headless stand-in - never while a screen shows.
        let draw_grid = !draw_timeline && !show_modes;
        // The input box draws over the bottom zone in normal mode (a full-screen app owns
        // input in alt-screen, so it is hidden there). It is the single on-screen home of
        // the live command line - the raw grid (with the shell's own echo) is not drawn in
        // normal mode (T-4.6), so there is no double echo.
        let draw_input = !alt_screen && frame.input.is_some();
        // The completion popover (T-9.5) floats above the input's left edge when open.
        let draw_completion = !alt_screen
            && frame
                .completion
                .is_some_and(aterm_core::Completion::is_open);
        // The risk-gate approval card (T-9.7) is a MODAL SAFETY overlay: it draws whenever a
        // turn is parked on a `RequireConfirm` verdict, INCLUDING over an alt-screen app (the
        // agent's `run_command` runs as a separate sandboxed subprocess, independent of the
        // hidden shell's fullscreen TUI). The keyboard resolves the decision regardless of
        // alt-screen (see `Session::handle_gate_key`), so the card must be visible regardless
        // too - otherwise the user could approve/reject a command they cannot see. So it is
        // NOT `!alt_screen`-gated like the chrome layers above.
        let draw_approval = frame.approval.is_some();
        // The custom title bar draws over a reserved TOP band in normal mode (a full-screen
        // app owns the whole surface in alt-screen, so it is hidden there, like the input
        // box). It reserves `title_h` off the top so the timeline lays out below it.
        let draw_title = !alt_screen && frame.title_bar.is_some();
        self.drew_timeline = draw_timeline;
        self.drew_grid = draw_grid;
        self.drew_screens = draw_screens;
        self.drew_input = draw_input;
        self.drew_completion = draw_completion;
        self.drew_approval = draw_approval;
        self.drew_title = draw_title;
        let size = crate::grid_render::FrameSize {
            width: self.config.width,
            height: self.config.height,
            scale: self.scale_factor,
        };

        // Reserve the bottom input zone (ticket T-3.6) so the timeline lays out ABOVE it.
        // `zone_px` is the single source of the zone height shared with the input front-end.
        // The viewport uniform stays the FULL surface size; only the layout row budget
        // shrinks, which keeps the timeline's last row above the box.
        let (_, ch) = crate::window::cell_px(self.scale_factor);
        let input_zone = if draw_input {
            crate::input_widget::zone_px(
                frame.input.expect("draw_input implies input"),
                self.scale_factor,
            )
        } else {
            0.0
        };
        // Reserve the top title-bar band (ticket T-9.2) so the timeline lays out BELOW it,
        // mirroring the bottom input zone. The timeline honors this as a top inset (added to
        // its own top breathing band); the grid fast-path is only drawn in alt-screen, where
        // the title bar is hidden, so it never sees this inset.
        let title_h = if draw_title {
            crate::title_bar::title_bar_px(self.scale_factor)
        } else {
            0.0
        };
        let effective_h = (self.config.height as f32 - title_h - input_zone).max(0.0);
        // The block timeline reserves top + bottom canvas breathing room (T-4.7,
        // `space::S12` each), so fewer rows fit than the raw surface height. The grid
        // fast-path keeps its own tight inset and does not consume this row budget.
        // The split is asymmetric across two files BY DESIGN: this reserves BOTH bands
        // (2x) in the row budget, while `timeline_render` applies only the TOP margin
        // (`top_margin = S12`) as an explicit y-offset - the matching BOTTOM band is the
        // unused tail of this shrunken budget (the last row's bottom lands at
        // `top_margin + viewport_rows*ch <= effective_h - S12`). Keep the `2.0 *` here in
        // step with that single top offset there, or the symmetric rhythm drifts.
        let timeline_breathing = 2.0 * f32::from(aterm_tokens::space::S12) * self.scale_factor;
        let viewport_rows = ((effective_h - timeline_breathing).max(0.0) / ch)
            .floor()
            .max(0.0) as u64;

        // Build instances BEFORE acquiring the surface texture. Each front-end's rebuild
        // is damage-gated and reuses its buffers with zero allocation when nothing
        // changed (the steady-state present floor, T-1.8).
        {
            let _build = tracing::trace_span!("build").entered();
            if draw_timeline {
                let blocks = frame.blocks.expect("draw_timeline implies blocks");
                // Finalize the scroll offset for this frame against the live extents:
                // while following the bottom (the live-terminal default) this pins to
                // the latest content so the running command's tail stays on screen;
                // once the user scrolls (wheel / PageUp / PageDown, wired in `app`) it
                // holds their offset, clamped into range (ticket T-7.2).
                let scroll = self
                    .scroll
                    .resolve(crate::timeline::total_display_rows(blocks), viewport_rows);
                self.last_visible_blocks =
                    crate::timeline::visible_block_count(blocks, false, scroll, viewport_rows);
                // Idle gate: `timeline::layout` allocates, so skip it (and the rebuild)
                // when nothing drawn changed - an idle present then allocates nothing and
                // simply redraws the prior timeline instances.
                // Fold the EFFECTIVE timeline height (surface minus the input zone), so a
                // growing multi-line input box reflows the timeline above it.
                let sig = (
                    frame.snapshot.map_or(0, |s| s.version),
                    scroll.offset_rows,
                    self.config.width,
                    effective_h.round() as u32,
                    theme_kind_code(frame.theme),
                    false,
                    hovered_block,
                );
                if self.timeline_sig != Some(sig) {
                    let layout = crate::timeline::layout(blocks, false, scroll, viewport_rows);
                    self.timeline.prepare(
                        &self.device,
                        &self.queue,
                        &mut self.atlas,
                        &layout,
                        title_h,
                        hovered_block,
                        frame.theme,
                        size,
                    );
                    self.timeline_sig = Some(sig);
                }
            } else if draw_grid {
                // Alt-screen surface or no-engine stand-in: the grid is the view.
                self.last_visible_blocks = 0;
                if let Some(snap) = frame.snapshot {
                    self.grid.prepare(
                        &self.device,
                        &self.queue,
                        &mut self.atlas,
                        snap,
                        frame.theme,
                        size,
                    );
                }
                // Force a timeline rebuild the next time we re-enter timeline mode.
                self.timeline_sig = None;
            } else {
                // A screen (the modes explainer) replaces the primary view; neither the
                // timeline nor the grid draws. Force a timeline rebuild on re-entry.
                self.last_visible_blocks = 0;
                self.timeline_sig = None;
            }

            // The informational screens (T-9.5): launch (empty timeline) or modes (on
            // demand), centered in the content band between the title bar and the input box.
            // Self-gated. The "Currently routing to <mode>" line reads the live input mode.
            if draw_screens {
                let screen_mode =
                    frame
                        .input
                        .map_or(aterm_tokens::Mode::Shell, |m| match m.mode() {
                            aterm_core::InputMode::Agent => aterm_tokens::Mode::Agent,
                            aterm_core::InputMode::Shell => aterm_tokens::Mode::Shell,
                        });
                self.screens.prepare(
                    &self.device,
                    &self.queue,
                    &mut self.atlas,
                    screen_kind,
                    screen_mode,
                    title_h,
                    effective_h,
                    frame.theme,
                    size,
                );
            }

            // The input box (self-gated: its own damage signature early-outs alloc-free).
            if draw_input {
                self.input.prepare(
                    &self.device,
                    &self.queue,
                    &mut self.atlas,
                    frame.input.expect("draw_input implies input"),
                    frame.autonomy,
                    hovered,
                    frame.theme,
                    size,
                );
            }

            // The completion popover (T-9.5): floats above the input's top edge. Self-gated.
            if draw_completion {
                let input_zone_top = (self.config.height as f32 - input_zone).max(0.0);
                self.completion.prepare(
                    &self.device,
                    &self.queue,
                    &mut self.atlas,
                    frame
                        .completion
                        .expect("draw_completion implies completion"),
                    hovered,
                    input_zone_top,
                    frame.theme,
                    size,
                );
            }

            // The risk-gate approval card (T-9.7): floats above the input while parked.
            // Self-gated (its own damage signature early-outs alloc-free).
            if draw_approval {
                let input_zone_top = (self.config.height as f32 - input_zone).max(0.0);
                self.approval.prepare(
                    &self.device,
                    &self.queue,
                    &mut self.atlas,
                    &frame.approval.expect("draw_approval implies approval"),
                    input_zone_top,
                    frame.theme,
                    size,
                );
            }

            // The title bar (self-gated, like the input box).
            if draw_title {
                self.title.prepare(
                    &self.device,
                    &self.queue,
                    &mut self.atlas,
                    &frame.title_bar.expect("draw_title implies title_bar"),
                    hovered,
                    frame.theme,
                    size,
                );
            }

            // Rebuild the frame's hit map (ticket T-9.8) from each drawn front-end's cached
            // geometry, in DRAW order (bottom to top) so `HitMap::hit`'s last-wins scan
            // returns the topmost target. Reuses the map's capacity (clear + push), so a warm
            // present allocates nothing. While the risk-gate approval card is up it is a MODAL
            // safety overlay (T-9.7): suppress ALL pointer targets so a click can never slip
            // through to a control behind the pending decision - the map stays empty until it
            // resolves.
            self.hit_map.clear();
            if !draw_approval {
                if draw_timeline {
                    for &(index, rect) in self.timeline.block_meta_rects() {
                        self.hit_map.push(rect, HitTarget::BlockMeta(index));
                    }
                }
                if draw_input {
                    if let Some(rect) = self.input.chip_rect() {
                        self.hit_map.push(rect, HitTarget::ModeChip);
                    }
                }
                if draw_completion {
                    for (i, &rect) in self.completion.row_rects().iter().enumerate() {
                        self.hit_map.push(rect, HitTarget::CompletionRow(i));
                    }
                }
                if draw_title {
                    if let Some(rect) = self.title.sidebar_toggle_rect() {
                        self.hit_map.push(rect, HitTarget::SidebarToggle);
                    }
                }
            }
        }

        // wgpu 29: `get_current_texture` returns a `CurrentSurfaceTexture` enum.
        let surface_tex = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                return Err(RenderError::SurfaceLost);
            }
            // Occluded / Timeout: skip this frame cleanly.
            wgpu::CurrentSurfaceTexture::Occluded | wgpu::CurrentSurfaceTexture::Timeout => {
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(RenderError::Backend("surface validation error".into()));
            }
        };
        let view = surface_tex
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        {
            let _encode = tracing::trace_span!("encode").entered();
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("aterm-frame"),
                });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("aterm-clear+grid"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(clear),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });

                // Draw the chosen PRIMARY front-end through the shared atlas. Each draw
                // no-ops (and zeroes its own glyph-draw-call counter) when it has no
                // instances, so the counter stays honest across mode switches and blank
                // frames.
                if draw_timeline {
                    self.timeline.draw(&mut pass, &self.atlas);
                } else if draw_grid {
                    self.grid.draw(&mut pass, &self.atlas);
                }
                // An informational screen (launch over the empty timeline, or modes as the
                // whole content) draws in the content band.
                if draw_screens {
                    self.screens.draw(&mut pass, &self.atlas);
                }
                // The input box draws over the reserved bottom zone (its hairline +
                // text + chip + caret sit on top of the cleared canvas).
                if draw_input {
                    self.input.draw(&mut pass, &self.atlas);
                }
                // The completion popover floats above the input, on top of the content.
                if draw_completion {
                    self.completion.draw(&mut pass, &self.atlas);
                }
                // The risk-gate approval card floats over the input while a turn is parked,
                // above the content (a modal decision, on top like the popover).
                if draw_approval {
                    self.approval.draw(&mut pass, &self.atlas);
                }
                // The title bar draws last, over the reserved top band (its bottom hairline
                // + dots + toggle + centered title sit on top of the cleared canvas).
                if draw_title {
                    self.title.draw(&mut pass, &self.atlas);
                }
            }
            self.queue.submit(std::iter::once(encoder.finish()));
        }

        {
            let _present = tracing::trace_span!("present").entered();
            surface_tex.present();
        }
        Ok(())
    }
}

impl Renderer for GpuRenderer {
    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    fn render(&mut self, frame: Frame<'_>) -> Result<(), RenderError> {
        match self.render_inner(frame) {
            Err(RenderError::SurfaceLost) => {
                // Reconfigure and let the next frame retry.
                self.surface.configure(&self.device, &self.config);
                Ok(())
            }
            other => other,
        }
    }
}

/// A 1-byte discriminant for the active theme, folded into the timeline idle-gate
/// signature so a light<->dark switch forces a timeline rebuild (the two themes are the
/// only palettes, and the rendered/effective palette is a pure function of the kind).
fn theme_kind_code(theme: &aterm_tokens::Theme) -> u8 {
    match theme.kind {
        aterm_tokens::ThemeKind::Light => 0,
        aterm_tokens::ThemeKind::Dark => 1,
    }
}

/// Convert an `aterm-tokens::Rgba` to a wgpu clear color (linearized).
fn linear_to_wgpu(c: Rgba) -> wgpu::Color {
    let [r, g, b, a] = c.to_linear_f32();
    wgpu::Color {
        r: r as f64,
        g: g as f64,
        b: b as f64,
        a: a as f64,
    }
}
