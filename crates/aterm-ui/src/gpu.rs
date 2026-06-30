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
use crate::renderer::{Frame, RenderError, Renderer};

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
    /// Idle gate for the timeline path: the `(snapshot version, scroll, viewport, theme,
    /// alt)` signature last laid out + prepared. When unchanged, the per-frame
    /// `timeline::layout` (which allocates) + prepare are skipped and the prior timeline
    /// instances are redrawn - so an idle present allocates nothing (the T-1.8 floor).
    timeline_sig: Option<(u64, u64, u32, u32, u8, bool)>,
    /// Virtualized-timeline scroll position (ticket T-2.7). Auto-follows the bottom
    /// (the live-terminal default) until scroll input lands (EPIC-3).
    scroll: crate::timeline::Scroll,
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

        Ok(Self {
            surface,
            device,
            queue,
            config,
            atlas,
            grid,
            timeline,
            input,
            timeline_sig: None,
            scroll: crate::timeline::Scroll::default(),
            last_visible_blocks: 0,
            drew_timeline: false,
            drew_input: false,
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

    /// Glyph-layer draw calls from the last frame: the PRIMARY front-end's (the timeline
    /// in normal mode, the grid in alt-screen - each exactly 1 when it has text, T-1.6
    /// AC c) plus the input box's one draw when it is shown (T-3.6). So a normal frame
    /// with input is 2 (timeline + box); an alt-screen frame is 1 (the grid, no box).
    /// Exposed for tests / instrumentation.
    #[must_use]
    pub fn last_glyph_draw_calls(&self) -> u32 {
        let primary = if self.drew_timeline {
            self.timeline.last_glyph_draw_calls()
        } else {
            self.grid.last_glyph_draw_calls()
        };
        let input = if self.drew_input {
            self.input.last_glyph_draw_calls()
        } else {
            0
        };
        primary + input
    }

    /// Render the snapshot grid (and always clear). Split out so `render` reads
    /// cleanly.
    fn render_inner(&mut self, frame: Frame<'_>) -> Result<(), RenderError> {
        // Tracy frame zone (ticket T-1.8 AC4); zero-cost with no subscriber.
        let _frame_zone = tracing::trace_span!("frame").entered();
        let clear = linear_to_wgpu(frame.theme.colors.bg_canvas);

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
        let draw_timeline = !alt_screen && frame.blocks.is_some();
        // The input box draws over the bottom zone in normal mode (a full-screen app owns
        // input in alt-screen, so it is hidden there). It is the single on-screen home of
        // the live command line - the raw grid (with the shell's own echo) is not drawn in
        // normal mode (T-4.6), so there is no double echo.
        let draw_input = !alt_screen && frame.input.is_some();
        self.drew_timeline = draw_timeline;
        self.drew_input = draw_input;
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
        let effective_h = (self.config.height as f32 - input_zone).max(0.0);
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
                // Pin to the bottom (the live-terminal default) until scroll input lands
                // (EPIC-3); the latest blocks + the running command's tail stay on screen.
                self.scroll
                    .to_bottom(crate::timeline::total_display_rows(blocks), viewport_rows);
                self.last_visible_blocks =
                    crate::timeline::visible_block_count(blocks, false, self.scroll, viewport_rows);
                // Idle gate: `timeline::layout` allocates, so skip it (and the rebuild)
                // when nothing drawn changed - an idle present then allocates nothing and
                // simply redraws the prior timeline instances.
                // Fold the EFFECTIVE timeline height (surface minus the input zone), so a
                // growing multi-line input box reflows the timeline above it.
                let sig = (
                    frame.snapshot.map_or(0, |s| s.version),
                    self.scroll.offset_rows,
                    self.config.width,
                    effective_h.round() as u32,
                    theme_kind_code(frame.theme),
                    false,
                );
                if self.timeline_sig != Some(sig) {
                    let layout = crate::timeline::layout(blocks, false, self.scroll, viewport_rows);
                    self.timeline.prepare(
                        &self.device,
                        &self.queue,
                        &mut self.atlas,
                        &layout,
                        frame.theme,
                        size,
                    );
                    self.timeline_sig = Some(sig);
                }
            } else {
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
            }

            // The input box (self-gated: its own damage signature early-outs alloc-free).
            if draw_input {
                self.input.prepare(
                    &self.device,
                    &self.queue,
                    &mut self.atlas,
                    frame.input.expect("draw_input implies input"),
                    frame.autonomy,
                    frame.theme,
                    size,
                );
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

                // Draw the chosen front-end through the shared atlas. Each draw no-ops
                // (and zeroes its own glyph-draw-call counter) when it has no instances,
                // so the counter stays honest across mode switches and blank frames.
                if draw_timeline {
                    self.timeline.draw(&mut pass, &self.atlas);
                } else {
                    self.grid.draw(&mut pass, &self.atlas);
                }
                // The input box draws last, over the reserved bottom zone (its hairline +
                // text + chip + caret sit on top of the cleared canvas).
                if draw_input {
                    self.input.draw(&mut pass, &self.atlas);
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
