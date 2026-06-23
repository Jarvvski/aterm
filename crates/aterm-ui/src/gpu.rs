//! The wgpu implementation of the [`Renderer`] seam.
//!
//! `GpuRenderer` owns the wgpu device/queue/surface and a glyphon text layer. It
//! clears every frame to the active theme's canvas color (the hard requirement)
//! and draws the terminal grid snapshot through glyphon when one is supplied.

use std::sync::Arc;

use glyphon::{
    Attrs, Buffer, Cache, Color as GColor, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use winit::window::Window;

use aterm_tokens::{font as tfont, Rgba};

use crate::fonts::font_system_with_bundled;
use crate::renderer::{Frame, RenderError, Renderer};

/// wgpu-backed renderer with a glyphon text fast-path.
pub struct GpuRenderer {
    // Surface must be declared before `window` so it drops first.
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    // Text pipeline.
    font_system: FontSystem,
    swash_cache: SwashCache,
    atlas: TextAtlas,
    viewport: Viewport,
    text_renderer: TextRenderer,
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
        // Prefer an sRGB format so our linear clear color is presented correctly.
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

        // glyphon text pipeline.
        let cache = Cache::new(&device);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let viewport = Viewport::new(&device, &cache);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let font_system = font_system_with_bundled();
        let swash_cache = SwashCache::new();

        Ok(Self {
            surface,
            device,
            queue,
            config,
            font_system,
            swash_cache,
            atlas,
            viewport,
            text_renderer,
            _window: window,
            scale_factor,
        })
    }

    /// Render the snapshot text (and always clear). Split out so `render` reads
    /// cleanly.
    fn render_inner(&mut self, frame: Frame<'_>) -> Result<(), RenderError> {
        let clear = linear_to_wgpu(frame.theme.colors.bg_canvas);
        let fg = frame.theme.colors.fg_primary;

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

        // Build the text buffer from the snapshot (if any) ahead of the pass so
        // glyphon's prepare can borrow it.
        let mut buffer = self.build_text_buffer(frame.snapshot, fg);

        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.config.width,
                height: self.config.height,
            },
        );

        if let Some(buf) = buffer.as_mut() {
            let text_areas = [TextArea {
                buffer: buf,
                left: 8.0,
                top: 8.0,
                scale: self.scale_factor,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: self.config.width as i32,
                    bottom: self.config.height as i32,
                },
                default_color: rgba_to_gcolor(fg),
                custom_glyphs: &[],
            }];
            self.text_renderer
                .prepare(
                    &self.device,
                    &self.queue,
                    &mut self.font_system,
                    &mut self.atlas,
                    &self.viewport,
                    text_areas,
                    &mut self.swash_cache,
                )
                .map_err(|e| RenderError::Backend(format!("text prepare: {e}")))?;
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aterm-frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aterm-clear+text"),
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

            if buffer.is_some() {
                self.text_renderer
                    .render(&self.atlas, &self.viewport, &mut pass)
                    .map_err(|e| RenderError::Backend(format!("text render: {e}")))?;
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        surface_tex.present();
        self.atlas.trim();
        Ok(())
    }

    /// Build a glyphon text buffer from the grid snapshot. Returns `None` when
    /// there is nothing to draw (window still clears).
    fn build_text_buffer(
        &mut self,
        snapshot: Option<&aterm_core::Snapshot>,
        _fg: Rgba,
    ) -> Option<Buffer> {
        let snap = snapshot?;
        // Render the visible grid as monospaced lines. This is the stretch text
        // fast-path; it is intentionally simple (no per-cell color/attr yet).
        // TODO(ticket EPIC-1.6): per-cell fg/bg + bold/italic runs + a real grid
        // fast-path that batches one quad per cell instead of shaping each row.
        let grid = aterm_tokens::type_scale::GRID;
        let metrics = Metrics::new(grid.size_pt, grid.size_pt * grid.line_height);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(
            &mut self.font_system,
            Some(self.config.width as f32),
            Some(self.config.height as f32),
        );

        let mut text = String::with_capacity(snap.rows * (snap.cols + 1));
        for row in 0..snap.rows {
            for cell in snap.row(row) {
                text.push(cell.c);
            }
            text.push('\n');
        }
        let attrs = Attrs::new().family(Family::Name(tfont::GRID));
        buffer.set_text(
            &mut self.font_system,
            &text,
            &attrs,
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut self.font_system, false);
        Some(buffer)
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

/// Convert to glyphon's packed RGBA color.
fn rgba_to_gcolor(c: Rgba) -> GColor {
    GColor::rgba(c.r, c.g, c.b, c.a)
}
