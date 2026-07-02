//! The shared glyph atlas + glyph GPU pipeline (ticket T-4.3, extracted from the
//! T-1.8 grid renderer).
//!
//! This is the "one shaping engine / shared atlas, two layout front-ends" seam
//! (`08-text-glyph-rendering.md` §2, Rec 5): a single R8 alpha-coverage atlas texture,
//! a single `(family, glyph, face, px, sprite)`-keyed cache + rasterizer, and the one
//! textured glyph pipeline that BOTH the terminal grid ([`crate::grid_render`]) and the
//! proportional prose path ([`crate::prose`]) draw through. Each front-end owns its own
//! instance buffers (so the grid's rebuild-gate buffer persists across frames and prose
//! can never clobber it); the atlas owns NO per-frame instance state.
//!
//! What lives here vs the front-end:
//! - **Atlas**: the atlas texture + sampler, the [`GlyphCache`]/[`GlyphRasterizer`]/
//!   placements/skip-memo (the rasterize-once guarantee), the SHARED group(0) viewport
//!   uniform, and BOTH shared 2D pipelines that bind it - the textured glyph pipeline
//!   ([`GlyphAtlas::draw_glyphs`]) and the solid-quad rect pipeline
//!   ([`GlyphAtlas::draw_rects`], the grid's cell backgrounds + the timeline's flat
//!   rectangles). Owning both here keeps the one viewport layout in one place.
//!   Acquisition ([`GlyphAtlas::acquire_font`] / [`GlyphAtlas::acquire_sprite`]) returns
//!   only the raw raster facts: an [`AtlasRect`] + the pen `(left, top)`.
//! - **Front-end**: all quad GEOMETRY (the grid's sprite-fill / constraint / baseline
//!   placement; prose's shaped pen positions; the timeline's block/chip/gutter layout),
//!   the per-front-end instance `Vec`s + GPU buffers, and the per-front-end rebuild gate
//!   + draw-call counter.
//!
//! Colors are linearized in the token layer; the atlas stores only 8-bit coverage and
//! the per-instance color multiplies it in the shader (grayscale AA, no color in the
//! atlas) - the same composite-by-multiply the grid has always used.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::mem::size_of;

use bytemuck::{Pod, Zeroable};

use crate::glyph::{GlyphRasterizer, RasterGlyph};
use crate::text::{AtlasRect, FaceStyle, FontFamily, GlyphCache, GlyphKey};

/// Atlas dimensions (px). 1024² of R8 = 1 MiB, enough for the full ASCII set across
/// all faces at Retina sizes many times over. NOTE: with the prose path sharing this
/// one atlas (T-4.3), the budget is now cross-family - prose at large sizes and grid
/// share the same give-up memo; eviction/growth when it fills remains a follow-up
/// (today a full atlas logs once and drops further new glyphs).
pub(crate) const ATLAS_DIM: u32 = 1024;

/// A textured glyph-quad instance. Both front-ends build these; the one glyph
/// pipeline consumes them. Fields are physical px + normalized atlas coords + the
/// linear foreground color multiplied by the atlas coverage.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct GlyphInstance {
    /// `[x, y, w, h]` in physical px (the glyph bitmap's placed box).
    pub rect: [f32; 4],
    /// `[u0, v0, u1, v1]` normalized atlas coordinates.
    pub uv: [f32; 4],
    /// Linear RGBA foreground (multiplied by the atlas coverage).
    pub color: [f32; 4],
}

/// A solid-color quad instance: every flat rectangle the UI draws - the grid's cell
/// backgrounds + underlines, and the timeline's block/card fills, hairline separators,
/// gutter markers, and chip/badge fills. Both front-ends build these; the one shared
/// rect pipeline ([`GlyphAtlas::draw_rects`]) consumes them. Physical-px rect + linear
/// RGBA; alpha is honored (so a focus-dim / running-pulse overlay can be
/// semi-transparent through the same pipeline), and an opaque color blends identically
/// to a plain `REPLACE`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct RectInstance {
    /// `[x, y, w, h]` in physical px (top-left origin).
    pub rect: [f32; 4],
    /// Linear RGBA.
    pub color: [f32; 4],
}

/// Viewport uniform: the surface size in physical px (padded to 16 bytes). Shared by
/// the rect pipeline and the glyph pipeline via the one [`GlyphAtlas`] viewport bind
/// group.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Viewport {
    size: [f32; 2],
    _pad: [f32; 2],
}

/// A growable GPU instance buffer + its CPU capacity in instances. Owned by each
/// front-end (the grid's two; prose's one); the atlas only draws from one on request.
pub(crate) struct InstanceBuffer {
    buf: wgpu::Buffer,
    cap: usize,
}

impl InstanceBuffer {
    pub(crate) fn new(device: &wgpu::Device, label: &str, stride: usize, cap: usize) -> Self {
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: (stride * cap.max(1)) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            buf,
            cap: cap.max(1),
        }
    }

    /// Ensure room for `count` instances, recreating (growing) the buffer if needed.
    /// Returns `true` if the buffer was reallocated (the steady state returns
    /// `false` - no allocation).
    pub(crate) fn ensure(
        &mut self,
        device: &wgpu::Device,
        label: &str,
        stride: usize,
        count: usize,
    ) -> bool {
        if count <= self.cap {
            return false;
        }
        let cap = count.next_power_of_two();
        self.buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: (stride * cap) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.cap = cap;
        true
    }

    /// The underlying GPU buffer, for `queue.write_buffer` / `set_vertex_buffer`.
    pub(crate) fn buf(&self) -> &wgpu::Buffer {
        &self.buf
    }
}

/// The shared glyph engine: one atlas texture, one cache+rasterizer+placements+skip
/// memo, one glyph pipeline, and the shared group(0) viewport uniform. Owns NO
/// per-frame instance state. Constructed once from the device; the grid and prose
/// front-ends are its only clients.
///
/// The TYPE is `pub` (so the `pub` [`crate::prose::ProseRenderer`] can name it in its
/// signatures, mirroring the `pub` `GridRenderer`), but every method is `pub(crate)`:
/// externally it is an opaque, un-constructable handle. This is the internal
/// "one shaping engine, two layout front-ends" seam, not a general-purpose API.
pub struct GlyphAtlas {
    // Atlas + caches.
    atlas: wgpu::Texture,
    cache: GlyphCache,
    rasterizer: GlyphRasterizer,
    /// Glyph placement `(left, top)` per key, paralleling `cache`. NOTE: this, the
    /// `GlyphCache`, and the atlas texture are never cleared today - they grow with
    /// the set of distinct `(family, glyph, face, px)` seen, so a DPI/font-size change
    /// leaves the prior size's glyphs resident. Bounded by the atlas (a full atlas then
    /// drops new glyphs); eviction + growth is a follow-up (see [`ATLAS_DIM`]).
    placements: HashMap<GlyphKey, (i32, i32)>,
    /// Keys that emit no glyph instance and must not be re-rasterized on every rebuild:
    /// inkless glyphs (space / `.notdef` with no outline) AND glyphs that could not be
    /// placed because the atlas is full (the give-up memo).
    skip_glyphs: HashSet<GlyphKey>,
    atlas_full_logged: bool,

    // Pipeline + bindings.
    glyph_pipeline: wgpu::RenderPipeline,
    /// The shared solid-quad pipeline (group(0) viewport only, alpha-blended). Both the
    /// grid's cell backgrounds and the timeline's flat rectangles draw through it.
    rect_pipeline: wgpu::RenderPipeline,
    viewport_buf: wgpu::Buffer,
    viewport_bind: wgpu::BindGroup,
    atlas_bind: wgpu::BindGroup,
    /// Dedup guard for [`Self::set_viewport`]: the `(w, h)` last written, so a frame
    /// whose surface size is unchanged skips the 16-byte upload. `Cell` so the call
    /// takes `&self` and composes when both front-ends write it per frame.
    last_viewport: Cell<(u32, u32)>,
    /// Test-only: count of ACTUAL viewport uploads (skipped dedups don't count), so a
    /// test can prove the dedup elides redundant writes.
    #[cfg(test)]
    viewport_writes: Cell<u32>,
}

impl GlyphAtlas {
    /// Build the atlas + glyph pipeline against `format` (the surface's sRGB format).
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aterm-glyph-atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_DIM,
                height: ATLAS_DIM,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_view = atlas.create_view(&wgpu::TextureViewDescriptor::default());
        // Nearest, NOT Linear: glyph quads are snapped to integer pixel origins by each
        // front-end and packed edge-to-edge in the atlas with no gutter, so a 1:1 texel
        // mapping is exact. Linear would interpolate the boundary texels of one glyph
        // against its atlas NEIGHBOR (a different glyph) and soften the hinted bitmap.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("aterm-atlas-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let viewport_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aterm-viewport"),
            size: size_of::<Viewport>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // group(0): viewport uniform (shared by the glyph pipeline AND the grid's bg
        // pipeline, which builds its own pipeline layout against `viewport_layout`).
        let viewport_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aterm-viewport-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let viewport_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aterm-viewport-bind"),
            layout: &viewport_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: viewport_buf.as_entire_binding(),
            }],
        });

        // group(1): atlas texture + sampler (glyph pipeline only).
        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aterm-atlas-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let atlas_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aterm-atlas-bind"),
            layout: &atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aterm-glyph-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        // Glyph pipeline: group(0) + group(1), alpha-blended.
        let glyph_pl_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aterm-glyph-pl"),
            bind_group_layouts: &[Some(&viewport_layout), Some(&atlas_layout)],
            immediate_size: 0,
        });
        let glyph_attrs = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4];
        let glyph_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("aterm-glyph-pipeline"),
            layout: Some(&glyph_pl_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_glyph"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: size_of::<GlyphInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &glyph_attrs,
                }],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_glyph"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        // Rect pipeline: the solid-quad layer shared by the grid (cell backgrounds +
        // underlines) and the timeline (block/card fills, hairlines, gutter markers,
        // chip/badge fills). group(0) viewport ONLY (no atlas), and the SAME WGSL +
        // `vs_bg`/`fs_solid` entry points as before. Alpha-blended rather than REPLACE:
        // an opaque fill (alpha == 1) blends identically to REPLACE, but this lets a
        // focus-dim / running-pulse overlay be semi-transparent through one pipeline.
        let rect_pl_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aterm-rect-pl"),
            bind_group_layouts: &[Some(&viewport_layout)],
            immediate_size: 0,
        });
        let rect_attrs = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4];
        let rect_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("aterm-rect-pipeline"),
            layout: Some(&rect_pl_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_bg"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: size_of::<RectInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &rect_attrs,
                }],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_solid"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        Self {
            atlas,
            cache: GlyphCache::new(ATLAS_DIM, ATLAS_DIM),
            rasterizer: GlyphRasterizer::new(),
            placements: HashMap::new(),
            skip_glyphs: HashSet::new(),
            atlas_full_logged: false,
            glyph_pipeline,
            rect_pipeline,
            viewport_buf,
            viewport_bind,
            atlas_bind,
            last_viewport: Cell::new((u32::MAX, u32::MAX)),
            #[cfg(test)]
            viewport_writes: Cell::new(0),
        }
    }

    /// Write the shared viewport uniform, skipping the 16-byte upload when `(w, h)` is
    /// unchanged since the last call. Cheap and idempotent so both front-ends can call
    /// it per frame; it is on the CHANGED path of each front-end's rebuild (never on a
    /// steady-state early-out), so it does not threaten the zero-alloc present.
    pub(crate) fn set_viewport(&self, queue: &wgpu::Queue, w: u32, h: u32) {
        if self.last_viewport.get() == (w, h) {
            return;
        }
        queue.write_buffer(
            &self.viewport_buf,
            0,
            bytemuck::bytes_of(&Viewport {
                size: [w as f32, h as f32],
                _pad: [0.0, 0.0],
            }),
        );
        self.last_viewport.set((w, h));
        #[cfg(test)]
        self.viewport_writes.set(self.viewport_writes.get() + 1);
    }

    /// The atlas dimension (px); front-ends normalize an [`AtlasRect`] to UV with
    /// `1.0 / atlas_dim() as f32`.
    pub(crate) fn atlas_dim(&self) -> u32 {
        ATLAS_DIM
    }

    /// Cell/line metrics for a family's Regular face at `px` (delegates to the
    /// rasterizer).
    pub(crate) fn cell_metrics(&self, family: FontFamily, px: f32) -> crate::glyph::CellMetrics {
        self.rasterizer.cell_metrics(family, px)
    }

    /// Map a char to its cmap glyph id in `(family, face)` (no shaping; the grid fast
    /// path).
    pub(crate) fn glyph_id(&self, family: FontFamily, face: FaceStyle, ch: char) -> u16 {
        self.rasterizer.glyph_id(family, face, ch)
    }

    /// Get-or-rasterize a FONT glyph for `key` in `(family, face)`. Returns the cached
    /// `(rect, pen (left, top))` on a hit (zero rasterization); on a miss rasterizes
    /// once + uploads once. `None` => inkless or atlas-full (memoized so it is never
    /// re-rasterized).
    pub(crate) fn acquire_font(
        &mut self,
        queue: &wgpu::Queue,
        key: GlyphKey,
        family: FontFamily,
        face: FaceStyle,
        gid: u16,
        px: f32,
    ) -> Option<(AtlasRect, (i32, i32))> {
        self.acquire(queue, key, |r| r.rasterize(family, face, gid, px))
    }

    /// Get-or-rasterize a procedural SPRITE glyph (box / block / braille / Powerline)
    /// for `key`, drawn into a `cw` x `ch_px` cell box. Same caching + skip-memo
    /// contract as [`Self::acquire_font`].
    pub(crate) fn acquire_sprite(
        &mut self,
        queue: &wgpu::Queue,
        key: GlyphKey,
        ch: char,
        cw: u32,
        ch_px: u32,
    ) -> Option<(AtlasRect, (i32, i32))> {
        self.acquire(queue, key, |_| crate::sprite::render(ch, cw, ch_px))
    }

    /// The shared acquisition core both wrappers funnel through. Encapsulates the
    /// skip-memo, the cache hit, and the once-only rasterize-on-miss guarantee, so no
    /// front-end can re-rasterize a cached glyph or bypass the give-up memo. `produce`
    /// is called at most once (on a miss) with `&mut` the rasterizer.
    fn acquire(
        &mut self,
        queue: &wgpu::Queue,
        key: GlyphKey,
        produce: impl FnOnce(&mut GlyphRasterizer) -> Option<RasterGlyph>,
    ) -> Option<(AtlasRect, (i32, i32))> {
        if self.skip_glyphs.contains(&key) {
            return None;
        }
        if let Some(rect) = self.cache.get(&key) {
            return Some((rect, self.placements[&key]));
        }
        let g = produce(&mut self.rasterizer)?;
        self.place_glyph(queue, key, &g)
    }

    /// Upload an already-rasterized glyph - font OR procedural sprite - into the atlas
    /// and record its placement. Inkless glyphs AND glyphs the (full) atlas cannot
    /// place are memoized in `skip_glyphs` so they are never re-rasterized on a later
    /// rebuild. Returns the atlas rect + placement, or `None` if it emits no instance.
    fn place_glyph(
        &mut self,
        queue: &wgpu::Queue,
        key: GlyphKey,
        g: &RasterGlyph,
    ) -> Option<(AtlasRect, (i32, i32))> {
        if g.is_empty() {
            self.skip_glyphs.insert(key);
            return None;
        }
        let atlas = &self.atlas;
        let rect = self.cache.get_or_insert(
            key,
            |rect| upload_glyph(queue, atlas, rect, &g.coverage),
            g.width,
            g.height,
        );
        let Some(rect) = rect else {
            // Atlas full: memoize the give-up so this glyph is not re-rasterized every
            // rebuild (it cannot be placed until eviction/growth lands).
            self.skip_glyphs.insert(key);
            if !self.atlas_full_logged {
                log::warn!("glyph atlas full; new glyphs will not render (growth is a follow-up)");
                self.atlas_full_logged = true;
            }
            return None;
        };
        self.placements.insert(key, (g.left, g.top));
        Some((rect, (g.left, g.top)))
    }

    /// Record a solid-quad draw into a caller-owned `pass`: the shared rect pipeline +
    /// the group(0) viewport bind + the caller's instance buffer, then ONE instanced
    /// draw of `count` quads. The grid's cell backgrounds/underlines and the timeline's
    /// flat rectangles (block/card fills, hairlines, gutter markers, chips) all funnel
    /// through here, so the whole solid layer of a front-end is one draw call.
    pub(crate) fn draw_rects(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        buf: &InstanceBuffer,
        count: usize,
    ) {
        pass.set_pipeline(&self.rect_pipeline);
        pass.set_bind_group(0, &self.viewport_bind, &[]);
        pass.set_vertex_buffer(0, buf.buf().slice(..));
        pass.draw(0..6, 0..count as u32);
    }

    /// Record the shared glyph draw into a caller-owned `pass`: set the glyph pipeline,
    /// group(0) viewport, group(1) atlas, and the caller's instance buffer, then issue
    /// EXACTLY ONE instanced draw of `count` quads. The caller (front-end) owns the
    /// draw-call counter; the atlas cannot know who called it.
    pub(crate) fn draw_glyphs(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        buf: &InstanceBuffer,
        count: usize,
    ) {
        pass.set_pipeline(&self.glyph_pipeline);
        pass.set_bind_group(0, &self.viewport_bind, &[]);
        pass.set_bind_group(1, &self.atlas_bind, &[]);
        pass.set_vertex_buffer(0, buf.buf().slice(..));
        pass.draw(0..6, 0..count as u32);
    }

    /// Distinct glyphs rasterized into the atlas so far (the no-re-raster counter;
    /// stable across frames once warm). Test-only today (the front-ends' GPU tests
    /// assert the rasterize-once invariant through it); ungate when a non-test caller
    /// (a diagnostic / status line) needs it.
    #[cfg(test)]
    pub(crate) fn rasterizations(&self) -> u64 {
        self.cache.rasterizations()
    }

    /// Test-only: the `(w, h)` last written by [`Self::set_viewport`] (the dedup state).
    #[cfg(test)]
    pub(crate) fn last_viewport(&self) -> (u32, u32) {
        self.last_viewport.get()
    }

    /// Test-only: count of actual viewport uploads (skipped dedups excluded).
    #[cfg(test)]
    pub(crate) fn viewport_writes(&self) -> u32 {
        self.viewport_writes.get()
    }
}

/// Upload one glyph's coverage bytes into the atlas at `rect`. `write_texture` has no
/// 256-byte row-alignment requirement (that constraint is only for buffer copies), so
/// the tight `bytes_per_row = rect.w` upload is valid.
fn upload_glyph(queue: &wgpu::Queue, atlas: &wgpu::Texture, rect: AtlasRect, coverage: &[u8]) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: atlas,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: rect.x,
                y: rect.y,
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        coverage,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(rect.w),
            rows_per_image: Some(rect.h),
        },
        wgpu::Extent3d {
            width: rect.w,
            height: rect.h,
            depth_or_array_layers: 1,
        },
    );
}

/// The shared WGSL. The atlas builds ONE shader module from this source and both its
/// pipelines select different entry points: `vs_bg`/`fs_solid` (the rect pipeline,
/// solid quads) and `vs_glyph`/`fs_glyph` (the glyph pipeline, textured coverage). Kept
/// `pub(crate)` for any future front-end that wants to build its own pipeline against
/// the same source + viewport layout.
pub(crate) const SHADER: &str = r#"
struct Viewport { size: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> viewport: Viewport;

fn corner(vi: u32) -> vec2<f32> {
    var c = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    return c[vi];
}

fn to_clip(px: vec2<f32>) -> vec4<f32> {
    let ndc = vec2<f32>(px.x / viewport.size.x * 2.0 - 1.0,
                        1.0 - px.y / viewport.size.y * 2.0);
    return vec4<f32>(ndc, 0.0, 1.0);
}

// --- Background / solid quads ---
struct BgIn { @location(0) rect: vec4<f32>, @location(1) color: vec4<f32> };
struct BgOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };

@vertex
fn vs_bg(@builtin(vertex_index) vi: u32, inst: BgIn) -> BgOut {
    var out: BgOut;
    let c = corner(vi);
    out.pos = to_clip(inst.rect.xy + c * inst.rect.zw);
    out.color = inst.color;
    return out;
}

@fragment
fn fs_solid(in: BgOut) -> @location(0) vec4<f32> {
    return in.color;
}

// --- Glyph quads ---
struct GIn {
    @location(0) rect: vec4<f32>,
    @location(1) uv: vec4<f32>,
    @location(2) color: vec4<f32>,
};
struct GOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_samp: sampler;

@vertex
fn vs_glyph(@builtin(vertex_index) vi: u32, inst: GIn) -> GOut {
    var out: GOut;
    let c = corner(vi);
    out.pos = to_clip(inst.rect.xy + c * inst.rect.zw);
    out.uv = mix(inst.uv.xy, inst.uv.zw, c);
    out.color = inst.color;
    return out;
}

@fragment
fn fs_glyph(in: GOut) -> @location(0) vec4<f32> {
    let a = textureSample(atlas_tex, atlas_samp, in.uv).r;
    return vec4<f32>(in.color.rgb, in.color.a * a);
}
"#;

// Atlas-level invariant tests. Acquisition needs a real device+queue (the miss path
// uploads into the texture), so they are macOS-only and skip without an adapter - the
// same harness as the grid GPU tests. They pin the seam's guarantees AT the atlas
// (rasterize-once, the skip-memo, the set_viewport dedup) so the future prose front-end
// inherits them proven, independent of any one front-end's tests.
#[cfg(all(test, target_os = "macos"))]
mod gpu_tests {
    use super::*;

    fn device() -> Option<(wgpu::Device, wgpu::Queue, wgpu::TextureFormat)> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("aterm-atlas-test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .ok()?;
        Some((device, queue, wgpu::TextureFormat::Rgba8UnormSrgb))
    }

    fn key(family: FontFamily, gid: u16) -> GlyphKey {
        GlyphKey {
            family,
            glyph_id: gid,
            face: FaceStyle::Regular,
            px: 28,
            sprite: false,
        }
    }

    #[test]
    fn acquire_font_rasterizes_each_glyph_once_across_front_ends() {
        let Some((device, queue, format)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        // A Prose 'm' (the duospace wide glyph) acquired twice: rasterized once, the
        // second call is a pure cache hit returning the identical (rect, pen).
        let gid = atlas.glyph_id(FontFamily::Prose, FaceStyle::Regular, 'm');
        let k = key(FontFamily::Prose, gid);
        let a = atlas.acquire_font(&queue, k, FontFamily::Prose, FaceStyle::Regular, gid, 28.0);
        assert!(a.is_some(), "Prose 'm' acquires a slot");
        assert_eq!(atlas.rasterizations(), 1);
        let b = atlas.acquire_font(&queue, k, FontFamily::Prose, FaceStyle::Regular, gid, 28.0);
        assert_eq!(a, b, "the second acquire returns the cached (rect, pen)");
        assert_eq!(
            atlas.rasterizations(),
            1,
            "a cached glyph is never re-rasterized (I3 at the atlas)"
        );
        // The SAME glyph id in another family is a distinct key -> a second raster.
        let kp = key(FontFamily::Ui, gid);
        atlas.acquire_font(&queue, kp, FontFamily::Ui, FaceStyle::Regular, gid, 28.0);
        assert_eq!(
            atlas.rasterizations(),
            2,
            "the family axis disjoints the cache (UI re-rasterizes the same id)"
        );
    }

    #[test]
    fn inkless_glyph_is_memoized_and_never_re_rasterized() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        // A space has no outline -> None, memoized in skip_glyphs (no atlas slot, no
        // rasterization), and a repeat acquire stays None with no new work.
        let gid = atlas.glyph_id(FontFamily::Prose, FaceStyle::Regular, ' ');
        let k = key(FontFamily::Prose, gid);
        assert!(
            atlas
                .acquire_font(&queue, k, FontFamily::Prose, FaceStyle::Regular, gid, 28.0)
                .is_none(),
            "an inkless space yields no glyph instance"
        );
        let before = atlas.rasterizations();
        assert!(atlas
            .acquire_font(&queue, k, FontFamily::Prose, FaceStyle::Regular, gid, 28.0)
            .is_none());
        assert_eq!(
            atlas.rasterizations(),
            before,
            "the inkless skip-memo prevents any re-rasterization"
        );
    }

    #[test]
    fn set_viewport_dedups_unchanged_size() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let atlas = GlyphAtlas::new(&device, format);
        atlas.set_viewport(&queue, 800, 600);
        assert_eq!(atlas.last_viewport(), (800, 600));
        assert_eq!(atlas.viewport_writes(), 1);
        // Same size again: dedup elides the upload.
        atlas.set_viewport(&queue, 800, 600);
        assert_eq!(
            atlas.viewport_writes(),
            1,
            "an unchanged size skips the write"
        );
        // A new size writes again (a resize must reach the GPU).
        atlas.set_viewport(&queue, 1024, 768);
        assert_eq!(atlas.last_viewport(), (1024, 768));
        assert_eq!(atlas.viewport_writes(), 2, "a changed size writes");
    }
}
