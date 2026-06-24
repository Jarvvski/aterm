//! The wgpu implementation of the [`Renderer`] seam.
//!
//! `GpuRenderer` owns the wgpu device/queue/surface and the instanced terminal-grid
//! pipeline ([`crate::grid_render::GridRenderer`]). It clears every frame to the
//! active theme's canvas color (the hard requirement) and draws the terminal grid
//! snapshot through the atlas + instanced pipeline when one is supplied - the
//! per-cell glyph fast-path that replaced the interim glyphon whole-buffer reshape
//! (ticket T-1.8, the GPU half of T-1.6; the typing-lag cure).

use std::sync::Arc;

use winit::window::Window;

use aterm_tokens::Rgba;

use crate::grid_render::GridRenderer;
use crate::renderer::{Frame, RenderError, Renderer};

/// wgpu-backed renderer with the instanced grid fast-path.
pub struct GpuRenderer {
    // Surface must be declared before `window` so it drops first.
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// The instanced terminal-grid pipeline: glyph atlas + bg/glyph instanced draws,
    /// with version-gated rebuild (ticket T-1.8).
    grid: GridRenderer,
    /// Virtualized-timeline scroll position (ticket T-2.7). Auto-follows the bottom
    /// (the live-terminal default) until scroll input lands (EPIC-3).
    scroll: crate::timeline::Scroll,
    /// Blocks that built timeline geometry on the last drawn frame - the AC1
    /// virtualization counter (ticket T-2.7), exposed via
    /// [`GpuRenderer::visible_block_count`] for tests / a future status line. `0`
    /// until a block list is published.
    last_visible_blocks: usize,
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

        let grid = GridRenderer::new(&device, format);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            grid,
            scroll: crate::timeline::Scroll::default(),
            last_visible_blocks: 0,
            _window: window,
            scale_factor,
        })
    }

    /// The number of grid rows that fit in the current surface - the timeline
    /// viewport height in display rows (ticket T-2.7). Uses the same GRID cell-height
    /// metric as the PTY grid sizing ([`crate::window::cell_px`]), so the timeline
    /// viewport matches the terminal's row count.
    fn viewport_rows(&self) -> u64 {
        let (_, ch) = crate::window::cell_px(self.scale_factor);
        (self.config.height as f32 / ch).floor().max(0.0) as u64
    }

    /// The number of blocks that built timeline geometry on the last drawn frame -
    /// the AC1 virtualization counter (ticket T-2.7).
    #[must_use]
    pub fn visible_block_count(&self) -> usize {
        self.last_visible_blocks
    }

    /// Glyph-layer draw calls from the last frame (ticket T-1.6 AC c: exactly 1 when
    /// the grid has text). Exposed for tests / instrumentation.
    #[must_use]
    pub fn last_glyph_draw_calls(&self) -> u32 {
        self.grid.last_glyph_draw_calls()
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

        // Virtualized-timeline bookkeeping (ticket T-2.7): count the blocks
        // intersecting the viewport via the SumTree (O(log n), no allocation). The
        // on-screen view stays the raw grid for now; drawing the timeline cards is
        // T-4.6 (it consumes this geometry + the captured block output).
        let alt_screen = frame.snapshot.is_some_and(|s| s.alt_screen);
        let viewport_rows = self.viewport_rows();
        self.last_visible_blocks = match frame.blocks {
            Some(blocks) => {
                if !alt_screen {
                    self.scroll
                        .to_bottom(blocks.total_height_rows(), viewport_rows);
                }
                crate::timeline::visible_block_count(blocks, alt_screen, self.scroll, viewport_rows)
            }
            None => 0,
        };

        // Build the grid instances BEFORE acquiring the surface texture - rebuilds
        // only when the snapshot version / size / theme changed (the damage gate),
        // and reuses the buffers with zero work + zero allocation otherwise.
        let has_text = {
            let _build = tracing::trace_span!("build").entered();
            match frame.snapshot {
                Some(snap) => self.grid.prepare(
                    &self.device,
                    &self.queue,
                    snap,
                    frame.theme,
                    crate::grid_render::FrameSize {
                        width: self.config.width,
                        height: self.config.height,
                        scale: self.scale_factor,
                    },
                ),
                None => false,
            }
        };

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

                if has_text {
                    self.grid.draw(&mut pass);
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
