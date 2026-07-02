//! The borderless window frame (ticket T-9.9): the rounded `bg.canvas` fill + 1px
//! `hairline` border that turns the transparent, native-chrome-less window into the
//! vision mock's (ADR-0011) rounded `.aw` container.
//!
//! Another front-end over the shared [`GlyphAtlas`], but it draws through the dedicated
//! rounded-rect frame pipeline (a rounded-rect SDF in the fragment shader) rather than the
//! flat rect pipeline: ONE [`crate::atlas::FrameInstance`] covering the whole surface, with
//! the corners falling away to transparent so - on the transparent surface configured by
//! [`crate::gpu`] - the desktop shows through the rounded corners and macOS draws its soft
//! window shadow hugging the opaque rounded region (the mock's `box-shadow`). It is drawn
//! FIRST, beneath every other layer, so its `bg.canvas` fill is the base every timeline /
//! input / chrome layer composits onto (replacing the old opaque canvas clear).
//!
//! ## Scope (T-9.9)
//! - Rounding + the 1px hairline border are drawn HERE (the rect-pipeline element T-9.2
//!   specced but could not draw into a native-decorated opaque surface).
//! - The soft drop shadow is the OS window shadow (`with_has_shadow`), which follows the
//!   drawn opaque alpha - higher quality than compositing a blurred rect ourselves, and it
//!   sits OUTSIDE the surface where we cannot draw. See the ticket Notes.
//!
//! ## Damage gating
//! [`Self::prepare`] keys a rebuild on a cheap signature over everything drawn (size,
//! radius, border, the two colors) and early-outs (reusing the prior instance, ZERO
//! allocation) when nothing changed - the T-1.8 60fps floor.

use std::mem::size_of;

use aterm_tokens::{space, Rgba, Theme};

use crate::atlas::{FrameInstance, GlyphAtlas, InstanceBuffer};
use crate::grid_render::FrameSize;

/// The window corner radius in LOGICAL px (the mock's `.aw { border-radius: 12px }`);
/// scaled to physical at draw.
pub const WINDOW_RADIUS_LOGICAL: f32 = 12.0;

/// The rounded-window-frame front-end. Owns one reused [`FrameInstance`] buffer + a rebuild
/// gate; draws through the shared atlas's frame pipeline. Constructed once from the device.
pub struct WindowFrameRenderer {
    instances: Vec<FrameInstance>,
    buf: InstanceBuffer,
    built: Option<u64>,
}

impl WindowFrameRenderer {
    /// Build the frame front-end: its single-instance buffer.
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            instances: Vec::new(),
            buf: InstanceBuffer::new(device, "aterm-window-frame", size_of::<FrameInstance>(), 1),
            built: None,
        }
    }

    /// Build the frame instance for `size` through the shared `atlas` (its viewport uniform),
    /// reusing the prior build when unchanged (the damage gate). Returns `true` (the frame
    /// always draws when called). The frame spans the whole surface; the SDF alphas the
    /// corners. The unchanged path allocates nothing.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &GlyphAtlas,
        theme: &Theme,
        size: FrameSize,
    ) -> bool {
        let FrameSize {
            width,
            height,
            scale,
        } = size;
        let radius = (WINDOW_RADIUS_LOGICAL * scale).round();
        let border = (f32::from(space::HAIRLINE_WIDTH) * scale).round().max(1.0);
        let canvas = theme.colors.bg_canvas;
        let hairline = theme.colors.hairline;

        let sig = signature(width, height, radius, border, canvas, hairline);
        if self.built == Some(sig) {
            return !self.instances.is_empty();
        }

        self.instances.clear();
        self.instances.push(FrameInstance {
            rect: [0.0, 0.0, width as f32, height as f32],
            fill: canvas.to_linear_f32(),
            border: hairline.to_linear_f32(),
            params: [radius, border, 0.0, 0.0],
        });

        self.buf.ensure(
            device,
            "aterm-window-frame",
            size_of::<FrameInstance>(),
            self.instances.len(),
        );
        queue.write_buffer(self.buf.buf(), 0, bytemuck::cast_slice(&self.instances));
        atlas.set_viewport(queue, width, height);

        self.built = Some(sig);
        true
    }

    /// Record the frame draw (one rounded-rect instance) into `pass` through the shared
    /// `atlas`'s frame pipeline. Drawn first, beneath every other layer.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, atlas: &GlyphAtlas) {
        if !self.instances.is_empty() {
            atlas.draw_frame(pass, &self.buf, self.instances.len());
        }
    }
}

/// A stable u64 over everything the frame draws: size, radius, border, and the two colors.
/// Allocation-free (folds small numbers only).
fn signature(w: u32, h: u32, radius: f32, border: f32, canvas: Rgba, hairline: Rgba) -> u64 {
    fn fold(h: u64, v: u64) -> u64 {
        (h ^ v).wrapping_mul(0x0000_0100_0000_01b3)
    }
    let mut s: u64 = 0xcbf2_9ce4_8422_2325;
    s = fold(s, u64::from(w));
    s = fold(s, u64::from(h));
    s = fold(s, u64::from(radius.to_bits()));
    s = fold(s, u64::from(border.to_bits()));
    s = fold(s, u64::from(canvas.to_u32()));
    s = fold(s, u64::from(hairline.to_u32()));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_tokens::ThemeKind;

    #[test]
    fn signature_changes_on_every_drawn_axis() {
        let t = *Theme::for_kind(ThemeKind::Dark);
        let c = t.colors.bg_canvas;
        let h = t.colors.hairline;
        let base = signature(960, 600, 24.0, 2.0, c, h);
        assert_eq!(base, signature(960, 600, 24.0, 2.0, c, h), "deterministic");
        assert_ne!(base, signature(961, 600, 24.0, 2.0, c, h), "width");
        assert_ne!(base, signature(960, 601, 24.0, 2.0, c, h), "height");
        assert_ne!(base, signature(960, 600, 12.0, 2.0, c, h), "radius");
        assert_ne!(base, signature(960, 600, 24.0, 1.0, c, h), "border");
        let light = *Theme::for_kind(ThemeKind::Light);
        assert_ne!(
            base,
            signature(
                960,
                600,
                24.0,
                2.0,
                light.colors.bg_canvas,
                light.colors.hairline
            ),
            "theme colors"
        );
    }
}

// The frame draws to a real GPU through the shared atlas's SDF frame pipeline, so it is
// verified offscreen and read back - macOS-only, skipping when no adapter is present (the
// same harness as the other front-end GPU tests). These prove: the rounded fill inks in the
// interior in both themes, the corners are NOT filled (rounded away), and the rebuild gate
// early-outs alloc-free.
#[cfg(all(test, target_os = "macos"))]
mod gpu_tests {
    use super::*;
    use aterm_tokens::ThemeKind;

    const SCALE: f32 = 1.0;

    fn device() -> Option<(wgpu::Device, wgpu::Queue, wgpu::TextureFormat)> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("aterm-frame-test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .ok()?;
        Some((device, queue, wgpu::TextureFormat::Rgba8UnormSrgb))
    }

    struct Readback {
        data: Vec<u8>,
        stride: usize,
    }
    impl Readback {
        fn lum(&self, x: u32, y: u32) -> u8 {
            let o = y as usize * self.stride + x as usize * 4;
            self.data[o].max(self.data[o + 1]).max(self.data[o + 2])
        }
    }

    fn render(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &GlyphAtlas,
        wf: &mut WindowFrameRenderer,
        theme: &Theme,
        w: u32,
        h: u32,
    ) -> Readback {
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wf-target"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let vw = target.create_view(&wgpu::TextureViewDescriptor::default());
        let stride = ((w * 4).div_ceil(256) * 256) as usize;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wf-readback"),
            size: (stride as u32 * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        wf.prepare(
            device,
            queue,
            atlas,
            theme,
            FrameSize {
                width: w,
                height: h,
                scale: SCALE,
            },
        );
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            // Clear to a TRANSPARENT black so an unfilled (rounded-away) corner reads 0 and
            // the interior fill reads the canvas color.
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("wf-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &vw,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            wf.draw(&mut pass, atlas);
        }
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride as u32),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(enc.finish()));
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
        let data = slice.get_mapped_range().to_vec();
        Readback { data, stride }
    }

    #[test]
    fn frame_fills_the_interior_and_rounds_the_corners_in_both_themes() {
        let Some((device, queue, format)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let (w, h) = (200u32, 160u32);
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let theme = *Theme::for_kind(kind);
            let atlas = GlyphAtlas::new(&device, format);
            let mut wf = WindowFrameRenderer::new(&device);
            let rb = render(&device, &queue, &atlas, &mut wf, &theme, w, h);

            // The interior (window center) is filled with the canvas color -> alpha 1 -> the
            // canvas inks (both themes have a non-black canvas over the transparent clear).
            assert!(
                rb.lum(w / 2, h / 2) > 8,
                "{kind:?}: the window interior is filled with the canvas"
            );
            // The extreme corner pixel is OUTSIDE the 12px rounding -> alpha 0 -> stays the
            // transparent-black clear. (Radius 12 at scale 1: the (1,1) pixel is well inside
            // the cut corner.)
            assert_eq!(
                rb.lum(0, 0),
                0,
                "{kind:?}: the top-left corner is rounded away (transparent)"
            );
            assert_eq!(
                rb.lum(w - 1, h - 1),
                0,
                "{kind:?}: the bottom-right corner is rounded away (transparent)"
            );
        }
    }

    #[test]
    fn unchanged_frame_skips_rebuild_alloc_free() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let atlas = GlyphAtlas::new(&device, format);
        let mut wf = WindowFrameRenderer::new(&device);
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let size = FrameSize {
            width: 200,
            height: 160,
            scale: SCALE,
        };
        wf.prepare(&device, &queue, &atlas, &theme, size);
        let allocs = crate::alloc_probe::count_allocs(|| {
            let drew = wf.prepare(&device, &queue, &atlas, &theme, size);
            std::hint::black_box(drew);
        });
        assert_eq!(
            allocs, 0,
            "an unchanged window-frame prepare early-out allocates nothing (got {allocs})"
        );
    }
}
