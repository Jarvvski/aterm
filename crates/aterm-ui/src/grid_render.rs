//! The instanced terminal-grid GPU pipeline (ticket T-1.8, the GPU half of T-1.6).
//!
//! This is the renderer that replaces the interim glyphon whole-buffer reshape on
//! the hot path. It is the convergent fast-terminal architecture (see
//! `08-text-glyph-rendering.md` §1): rasterize each unique glyph ONCE into a shared
//! 8-bit **alpha** atlas, then per frame emit one instanced quad per cell and draw
//! the whole grid in a handful of draw calls (one for the glyph layer, AC c).
//!
//! ## Layers (drawn in this order into the already-cleared surface)
//!
//! 1. **Background** - one solid quad per cell whose background differs from the
//!    canvas (canvas-colored cells are left to the clear, cutting overdraw), plus a
//!    thin quad per underlined cell. Opaque (`REPLACE`).
//! 2. **Glyph** - one alpha-blended quad per inked cell, sampling the atlas coverage
//!    and multiplying by the cell's foreground color. **One instanced draw call**
//!    (T-1.6 AC c). Grayscale AA only (T-1.6 AC d) - the atlas is single-channel
//!    coverage; color comes from the instance, never the atlas.
//!
//! ## Rebuild gating (the damage story, T-1.8)
//!
//! `09-performance-60fps.md` §3 is explicit that for a GPU terminal the payoff of
//! damage tracking is "deciding whether to draw at all" and "bounding CPU-side
//! frame-build work", NOT partial GPU redraw ("most GPU terminals rebuild the full
//! instance buffer each frame because per-cell instanced rendering of a full grid is
//! already cheap"). So [`GridRenderer::prepare`] keys the full instance rebuild on a
//! cheap `(snapshot version, viewport, px, theme)` signature and returns early -
//! reusing the prior instance buffers with ZERO work and ZERO allocation - when
//! nothing changed. That is the empirical "measure and pick" call the ticket invites:
//! full rebuild on change, skip on no-change; no partial-row GPU redraw.
//!
//! Colors are linearized ([`Rgba::to_linear_f32`]) because the surface is an sRGB
//! format and the shader output is encoded to sRGB on store, matching the clear.

use std::collections::HashSet;
use std::mem::size_of;

use bytemuck::{Pod, Zeroable};

use aterm_core::Snapshot;
use aterm_tokens::{type_scale, Theme};

use crate::glyph::{GlyphRasterizer, RasterGlyph};
use crate::text::{build_grid_cells, FaceStyle, GlyphCache, GlyphKey, GridCell};
use crate::window::cell_px;

/// Atlas dimensions (px). 1024² of R8 = 1 MiB, enough for the full ASCII set across
/// all four faces at Retina sizes many times over. Growth/eviction when it fills is
/// a follow-up (today a full atlas logs once and drops further new glyphs).
const ATLAS_DIM: u32 = 1024;

/// Left/top inset of the grid from the surface origin, in LOGICAL px (scaled by the
/// DPI factor at use). Matches the interim glyphon path's `(8, 8)` offset.
const INSET_LOGICAL: f32 = 8.0;

/// A solid-color quad instance (background cells + underlines).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BgInstance {
    /// `[x, y, w, h]` in physical px (top-left origin).
    rect: [f32; 4],
    /// Linear RGBA.
    color: [f32; 4],
}

/// A textured glyph-quad instance.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GlyphInstance {
    /// `[x, y, w, h]` in physical px (the glyph bitmap's placed box).
    rect: [f32; 4],
    /// `[u0, v0, u1, v1]` normalized atlas coordinates.
    uv: [f32; 4],
    /// Linear RGBA foreground (multiplied by the atlas coverage).
    color: [f32; 4],
}

/// Viewport uniform: the surface size in physical px (padded to 16 bytes).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Viewport {
    size: [f32; 2],
    _pad: [f32; 2],
}

/// The surface geometry for a frame: physical-pixel size + the DPI scale factor.
/// Bundled so [`GridRenderer::prepare`] stays a tidy call.
#[derive(Debug, Clone, Copy)]
pub struct FrameSize {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
}

/// A growable GPU instance buffer + its CPU capacity in instances.
struct InstanceBuffer {
    buf: wgpu::Buffer,
    cap: usize,
}

impl InstanceBuffer {
    fn new(device: &wgpu::Device, label: &str, stride: usize, cap: usize) -> Self {
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
    fn ensure(&mut self, device: &wgpu::Device, label: &str, stride: usize, count: usize) -> bool {
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
}

/// The instanced grid renderer: owns the glyph atlas, the two pipelines, the reused
/// instance buffers, and the glyph cache + rasterizer. Constructed once from the
/// device; `prepare` (per frame, but early-outs when unchanged) builds instances and
/// `draw` records the two instanced draws into a render pass the caller owns.
pub struct GridRenderer {
    // Atlas.
    atlas: wgpu::Texture,
    cache: GlyphCache,
    rasterizer: GlyphRasterizer,
    /// Glyph placement `(left, top)` per key, paralleling `cache` (the cache stores
    /// only the atlas rect; placement positions the quad). NOTE: this, the
    /// `GlyphCache`, and the atlas texture are never cleared today - they grow with
    /// the set of distinct `(glyph, face, px)` seen, so a DPI/font-size change leaves
    /// the prior size's glyphs resident. Bounded by the atlas (a full atlas then
    /// drops new glyphs); eviction + atlas growth is a follow-up (see the `ATLAS_DIM`
    /// note).
    placements: std::collections::HashMap<GlyphKey, (i32, i32)>,
    /// Keys that emit no glyph instance and must not be re-rasterized on every
    /// rebuild: inkless glyphs (space / `.notdef` with no outline) AND glyphs that
    /// could not be placed because the atlas is full (the give-up memo).
    skip_glyphs: HashSet<GlyphKey>,

    // Pipelines + bindings.
    bg_pipeline: wgpu::RenderPipeline,
    glyph_pipeline: wgpu::RenderPipeline,
    viewport_buf: wgpu::Buffer,
    viewport_bind: wgpu::BindGroup,
    atlas_bind: wgpu::BindGroup,

    // Reused CPU + GPU instance storage.
    grid_cells: Vec<GridCell>,
    bg_instances: Vec<BgInstance>,
    glyph_instances: Vec<GlyphInstance>,
    bg_buf: InstanceBuffer,
    glyph_buf: InstanceBuffer,

    /// Rebuild gate: `(version, vw, vh, px, theme_sig)` currently built, or `None`.
    /// `theme_sig` is a hash over every theme color the build reads (see
    /// [`theme_signature`]), so a theme change always invalidates the build.
    built: Option<(u64, u32, u32, u32, u64)>,
    /// Glyph-layer draw calls issued by the last [`Self::draw`] (the T-1.6 AC c
    /// counter: exactly 1 when the grid has any inked cell, else 0).
    last_glyph_draw_calls: u32,
    atlas_full_logged: bool,
}

impl GridRenderer {
    /// Build the pipeline against `format` (the surface's sRGB format).
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
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
        // Nearest, NOT Linear: glyph quads are snapped to integer pixel origins (see
        // `prepare`) and packed edge-to-edge in the atlas with no gutter, so a 1:1
        // texel mapping is exact. Linear would interpolate the boundary texels of one
        // glyph against its atlas NEIGHBOR (a different glyph) and soften the hinted
        // bitmap; Nearest at integer positions is the conventional crisp choice for a
        // constant-advance grid (08-text-glyph-rendering.md).
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

        // group(0): viewport uniform (shared by both pipelines).
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
            label: Some("aterm-grid-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        // Background pipeline: group(0) only, opaque.
        let bg_pl_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aterm-bg-pl"),
            bind_group_layouts: &[Some(&viewport_layout)],
            immediate_size: 0,
        });
        let bg_attrs = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4];
        let bg_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("aterm-bg-pipeline"),
            layout: Some(&bg_pl_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_bg"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: size_of::<BgInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &bg_attrs,
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
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
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

        Self {
            atlas,
            cache: GlyphCache::new(ATLAS_DIM, ATLAS_DIM),
            rasterizer: GlyphRasterizer::new(),
            placements: std::collections::HashMap::new(),
            skip_glyphs: HashSet::new(),
            bg_pipeline,
            glyph_pipeline,
            viewport_buf,
            viewport_bind,
            atlas_bind,
            grid_cells: Vec::new(),
            bg_instances: Vec::new(),
            glyph_instances: Vec::new(),
            bg_buf: InstanceBuffer::new(device, "aterm-bg-instances", size_of::<BgInstance>(), 256),
            glyph_buf: InstanceBuffer::new(
                device,
                "aterm-glyph-instances",
                size_of::<GlyphInstance>(),
                256,
            ),
            built: None,
            last_glyph_draw_calls: 0,
            atlas_full_logged: false,
        }
    }

    /// Glyph-layer draw calls from the last [`Self::draw`] (T-1.6 AC c: 1 when there
    /// is text).
    #[must_use]
    pub fn last_glyph_draw_calls(&self) -> u32 {
        self.last_glyph_draw_calls
    }

    /// Distinct glyphs rasterized into the atlas so far (T-1.6 AC5: stable across
    /// frames once warm - a repeated glyph is never re-rasterized).
    #[must_use]
    pub fn rasterizations(&self) -> u64 {
        self.cache.rasterizations()
    }

    /// Build the frame's instances from `snap`, reusing the prior build when the
    /// `(version, viewport, px, theme)` signature is unchanged (the damage gate).
    /// Returns `true` if there is anything to draw.
    ///
    /// The unchanged path allocates nothing (the steady-state present; asserted by
    /// `steady_state_prepare_is_allocation_free`). On the CHANGED path the CPU
    /// instance build reuses its warm `Vec`s and the glyph cache, so it does not
    /// allocate either once warm at a stable size and glyph set; the GPU upload
    /// (`queue.write_buffer`) is wgpu-managed staging and is not part of that claim.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        snap: &Snapshot,
        theme: &Theme,
        size: FrameSize,
    ) -> bool {
        let FrameSize {
            width: viewport_w,
            height: viewport_h,
            scale,
        } = size;
        let px = (type_scale::GRID.size_pt * scale).round().max(1.0);
        let px_key = px as u32;
        let key = (
            snap.version,
            viewport_w,
            viewport_h,
            px_key,
            theme_signature(theme),
        );
        if self.built == Some(key) {
            // Nothing changed: reuse the instance buffers verbatim (no rebuild, no
            // allocation). This is the steady-state present path (T-1.5 AC5 / AC1).
            return !self.glyph_instances.is_empty() || !self.bg_instances.is_empty();
        }

        let (cw, ch) = cell_px(scale);
        // Integer cell extent for procedural sprite glyphs (box-drawing / blocks /
        // braille / Powerline), which fill the cell box rather than a font outline.
        let cw_i = cw.round().max(1.0) as u32;
        let ch_i = ch.round().max(1.0) as u32;
        let inset = INSET_LOGICAL * scale;
        let metrics = self.rasterizer.cell_metrics(px);
        // Center the font's line box in the cell box, then baseline = ascent below
        // the box top.
        let baseline_off = (ch - metrics.line) * 0.5 + metrics.ascent;
        let canvas = theme.colors.bg_canvas;

        build_grid_cells(snap, theme, &mut self.grid_cells);
        self.bg_instances.clear();
        self.glyph_instances.clear();

        // Take the cell list out to avoid borrowing `self` twice (cells + caches).
        let cells = std::mem::take(&mut self.grid_cells);
        for cell in &cells {
            let cw_cell = if cell.wide { cw * 2.0 } else { cw };
            let cell_x = inset + f32::from(cell.col) * cw;
            let cell_y = inset + f32::from(cell.row) * ch;

            // Background quad (skip canvas-colored cells; the clear covers them).
            if cell.bg != canvas {
                self.bg_instances.push(BgInstance {
                    rect: [cell_x, cell_y, cw_cell, ch],
                    color: cell.bg.to_linear_f32(),
                });
            }
            // Underline: a thin quad just under the baseline.
            if cell.underline {
                let uy = cell_y + baseline_off + (metrics.descent * 0.3).max(1.0);
                self.bg_instances.push(BgInstance {
                    rect: [cell_x, uy, cw_cell, (ch * 0.06).max(1.0)],
                    color: cell.fg.to_linear_f32(),
                });
            }

            // Glyph quad. A sprite codepoint (box-drawing / blocks / braille /
            // Powerline) is drawn procedurally into the cell box and bypasses the
            // font; everything else is a font glyph keyed by its cmap glyph id.
            let sprite = crate::sprite::is_sprite(cell.ch);
            let face = if sprite {
                FaceStyle::Regular
            } else {
                FaceStyle::from_flags(cell.bold, cell.italic)
            };
            let gkey = GlyphKey {
                glyph_id: if sprite {
                    cell.ch as u32 as u16 // sprite codepoints are all in the BMP
                } else {
                    self.rasterizer.glyph_id(face, cell.ch)
                },
                face,
                px: px_key,
                sprite,
            };
            if self.skip_glyphs.contains(&gkey) {
                continue;
            }
            let slot = match self.cache.get(&gkey) {
                Some(rect) => Some((rect, self.placements[&gkey])),
                None if sprite => crate::sprite::render(cell.ch, cw_i, ch_i)
                    .and_then(|g| self.place_glyph(queue, gkey, &g)),
                None => self.rasterize_into_atlas(queue, gkey, face, gkey.glyph_id, px),
            };
            let Some((rect, (left, top))) = slot else {
                continue;
            };
            // Snap the glyph quad to integer pixels so the hinted bitmap maps 1:1 to
            // texels under the Nearest sampler (crisp, no inter-glyph bleed). The cell
            // origin is fractional (cw is ~7.8px), so without this the quad would
            // straddle pixel boundaries. A sprite fills the cell box, so it is placed
            // at the cell origin (its left/top are 0); a font glyph is baseline-relative.
            let (gx, gy) = if sprite {
                (cell_x.round(), cell_y.round())
            } else {
                (
                    (cell_x + left as f32).round(),
                    (cell_y + baseline_off - top as f32).round(),
                )
            };
            let inv = 1.0 / ATLAS_DIM as f32;
            self.glyph_instances.push(GlyphInstance {
                rect: [gx, gy, rect.w as f32, rect.h as f32],
                uv: [
                    rect.x as f32 * inv,
                    rect.y as f32 * inv,
                    (rect.x + rect.w) as f32 * inv,
                    (rect.y + rect.h) as f32 * inv,
                ],
                color: cell.fg.to_linear_f32(),
            });
        }
        self.grid_cells = cells; // return the buffer for reuse

        // Upload instances (grow buffers only when the counts exceed capacity).
        if !self.bg_instances.is_empty() {
            self.bg_buf.ensure(
                device,
                "aterm-bg-instances",
                size_of::<BgInstance>(),
                self.bg_instances.len(),
            );
            queue.write_buffer(
                &self.bg_buf.buf,
                0,
                bytemuck::cast_slice(&self.bg_instances),
            );
        }
        if !self.glyph_instances.is_empty() {
            self.glyph_buf.ensure(
                device,
                "aterm-glyph-instances",
                size_of::<GlyphInstance>(),
                self.glyph_instances.len(),
            );
            queue.write_buffer(
                &self.glyph_buf.buf,
                0,
                bytemuck::cast_slice(&self.glyph_instances),
            );
        }
        queue.write_buffer(
            &self.viewport_buf,
            0,
            bytemuck::bytes_of(&Viewport {
                size: [viewport_w as f32, viewport_h as f32],
                _pad: [0.0, 0.0],
            }),
        );

        self.built = Some(key);
        !self.glyph_instances.is_empty() || !self.bg_instances.is_empty()
    }

    /// Rasterize a glyph on a cache miss, upload it into the atlas, and record its
    /// placement. Inkless glyphs AND glyphs the (full) atlas cannot place are added to
    /// `skip_glyphs` so they are never re-rasterized on a later rebuild. Returns the
    /// atlas rect + placement, or `None` if it emits no glyph instance.
    fn rasterize_into_atlas(
        &mut self,
        queue: &wgpu::Queue,
        gkey: GlyphKey,
        face: FaceStyle,
        gid: u16,
        px: f32,
    ) -> Option<(crate::text::AtlasRect, (i32, i32))> {
        let g = self.rasterizer.rasterize(face, gid, px)?;
        self.place_glyph(queue, gkey, &g)
    }

    /// Upload an already-rasterized glyph - font OR procedural sprite - into the
    /// atlas and record its placement. Inkless glyphs AND glyphs the (full) atlas
    /// cannot place are memoized in `skip_glyphs` so they are never re-rasterized on
    /// a later rebuild. Returns the atlas rect + placement, or `None` if it emits no
    /// glyph instance.
    fn place_glyph(
        &mut self,
        queue: &wgpu::Queue,
        gkey: GlyphKey,
        g: &RasterGlyph,
    ) -> Option<(crate::text::AtlasRect, (i32, i32))> {
        if g.is_empty() {
            self.skip_glyphs.insert(gkey);
            return None;
        }
        let atlas = &self.atlas;
        let rect = self.cache.get_or_insert(
            gkey,
            |rect| upload_glyph(queue, atlas, rect, &g.coverage),
            g.width,
            g.height,
        );
        let Some(rect) = rect else {
            // Atlas full: memoize the give-up so this glyph is not re-rasterized every
            // rebuild (it cannot be placed until eviction/growth lands).
            self.skip_glyphs.insert(gkey);
            if !self.atlas_full_logged {
                log::warn!("glyph atlas full; new glyphs will not render (growth is a follow-up)");
                self.atlas_full_logged = true;
            }
            return None;
        };
        self.placements.insert(gkey, (g.left, g.top));
        Some((rect, (g.left, g.top)))
    }

    /// Record the grid draws into `pass` (which the caller has begun with the canvas
    /// clear). Background first (opaque), then the single glyph draw (alpha-blended).
    pub fn draw(&mut self, pass: &mut wgpu::RenderPass<'_>) {
        if !self.bg_instances.is_empty() {
            pass.set_pipeline(&self.bg_pipeline);
            pass.set_bind_group(0, &self.viewport_bind, &[]);
            pass.set_vertex_buffer(0, self.bg_buf.buf.slice(..));
            pass.draw(0..6, 0..self.bg_instances.len() as u32);
        }
        if self.glyph_instances.is_empty() {
            self.last_glyph_draw_calls = 0;
        } else {
            pass.set_pipeline(&self.glyph_pipeline);
            pass.set_bind_group(0, &self.viewport_bind, &[]);
            pass.set_bind_group(1, &self.atlas_bind, &[]);
            pass.set_vertex_buffer(0, self.glyph_buf.buf.slice(..));
            pass.draw(0..6, 0..self.glyph_instances.len() as u32);
            self.last_glyph_draw_calls = 1;
        }
    }
}

/// Upload one glyph's coverage bytes into the atlas at `rect`. `write_texture` has no
/// 256-byte row-alignment requirement (that constraint is only for buffer copies),
/// so the tight `bytes_per_row = rect.w` upload is valid.
fn upload_glyph(
    queue: &wgpu::Queue,
    atlas: &wgpu::Texture,
    rect: crate::text::AtlasRect,
    coverage: &[u8],
) {
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

/// A stable hash over every theme color the build reads (canvas, primary + muted
/// text, and the 16-color ANSI palette), so the rebuild gate invalidates on ANY
/// theme change. An XOR of two colors (the previous gate) can collide and ignores the
/// ANSI palette / muted text entirely. Computed once per rebuild, never on the
/// steady-state present path.
fn theme_signature(theme: &Theme) -> u64 {
    fn fold(h: u64, c: aterm_tokens::Rgba) -> u64 {
        (h ^ u64::from(c.to_u32())).wrapping_mul(0x0000_0100_0000_01b3)
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
    h = fold(h, theme.colors.bg_canvas);
    h = fold(h, theme.colors.fg_primary);
    h = fold(h, theme.colors.fg_muted);
    for i in 0..16u8 {
        h = fold(h, theme.ansi.by_index(i));
    }
    h
}

const SHADER: &str = r#"
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

// The instanced pipeline draws to a real GPU, so its correctness is verified by
// rendering to an offscreen texture and reading the pixels back. These tests need a
// Metal device and so are macOS-only (CI runs them on macos-14, per CLAUDE.md); they
// skip gracefully if no adapter is available. They cover the on-screen ACs the
// CPU-only T-1.6 tests could not: a glyph renders into the right cell (AC a), the
// background fills (AC a), a wide cell spans two columns (AC a), the glyph layer is a
// single instanced draw call (AC c), the atlas is single-channel coverage with color
// from the instance (AC d), and a repeated glyph is never re-rasterized (T-1.6 AC5).
#[cfg(all(test, target_os = "macos"))]
mod gpu_tests {
    use super::*;
    use aterm_core::{CellColor, Snapshot};
    use aterm_tokens::{Theme, ThemeKind};

    const SCALE: f32 = 1.0;

    fn theme() -> Theme {
        *Theme::for_kind(ThemeKind::Dark)
    }

    /// A headless device + queue + the production sRGB format, or `None` if no GPU
    /// adapter is available (skip the test rather than fail).
    fn device() -> Option<(wgpu::Device, wgpu::Queue, wgpu::TextureFormat)> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("aterm-grid-test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .ok()?;
        Some((device, queue, wgpu::TextureFormat::Rgba8UnormSrgb))
    }

    /// A read-back framebuffer: RGBA8 rows at `stride` (256-aligned) bytes.
    struct Readback {
        data: Vec<u8>,
        stride: usize,
        w: u32,
        h: u32,
    }
    impl Readback {
        fn px(&self, x: u32, y: u32) -> [u8; 4] {
            let o = y as usize * self.stride + x as usize * 4;
            [
                self.data[o],
                self.data[o + 1],
                self.data[o + 2],
                self.data[o + 3],
            ]
        }
        /// Whether any pixel in the half-open box has channel `ch` above `thresh`.
        fn any_chan(&self, x0: u32, y0: u32, x1: u32, y1: u32, ch: usize, thresh: u8) -> bool {
            (y0..y1.min(self.h)).any(|y| (x0..x1.min(self.w)).any(|x| self.px(x, y)[ch] > thresh))
        }
    }

    /// Cell pixel box at SCALE=1 (matching `GridRenderer::prepare`'s layout math).
    fn cell_box(col: u16, row: u16, wide: bool) -> (u32, u32, u32, u32) {
        let (cw, ch) = cell_px(SCALE);
        let inset = INSET_LOGICAL * SCALE;
        let x0 = inset + f32::from(col) * cw;
        let y0 = inset + f32::from(row) * ch;
        let w = if wide { cw * 2.0 } else { cw };
        (x0 as u32, y0 as u32, (x0 + w) as u32, (y0 + ch) as u32)
    }

    fn target_size(cols: u16, rows: u16) -> (u32, u32) {
        let (cw, ch) = cell_px(SCALE);
        let inset = INSET_LOGICAL * SCALE;
        (
            (inset * 2.0 + f32::from(cols) * cw).ceil() as u32,
            (inset * 2.0 + f32::from(rows) * ch).ceil() as u32,
        )
    }

    /// Render `snap` through the grid pipeline into an offscreen `w`x`h` texture
    /// cleared to black, and read the pixels back.
    fn render(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        grid: &mut GridRenderer,
        snap: &Snapshot,
        w: u32,
        h: u32,
    ) -> Readback {
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test-target"),
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
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let stride = ((w * 4).div_ceil(256) * 256) as usize;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test-readback"),
            size: (stride as u32 * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        grid.prepare(
            device,
            queue,
            snap,
            &theme(),
            FrameSize {
                width: w,
                height: h,
                scale: SCALE,
            },
        );
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("test-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            grid.draw(&mut pass);
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
        Readback { data, stride, w, h }
    }

    /// A snapshot with one cell set; the rest are blank defaults.
    fn one_cell(cols: u16, ch: char, fg: CellColor, bg: CellColor, wide: bool) -> Snapshot {
        let mut snap = Snapshot::empty(1, cols as usize);
        snap.version = 1;
        snap.cells[0].c = ch;
        snap.cells[0].fg = fg;
        snap.cells[0].bg = bg;
        if wide {
            snap.cells[0].wide = true;
            snap.cells[1].wide_spacer = true;
        }
        snap
    }

    #[test]
    fn glyph_and_background_render_into_the_right_cell() {
        let Some((device, queue, format)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let mut grid = GridRenderer::new(&device, format);
        let (w, h) = target_size(4, 1);
        // 'M' white-on-red at col 0; the rest are blank defaults (canvas bg).
        let snap = one_cell(
            4,
            'M',
            CellColor::Rgb(255, 255, 255),
            CellColor::Rgb(255, 0, 0),
            false,
        );
        let rb = render(&device, &queue, &mut grid, &snap, w, h);

        let (x0, y0, x1, y1) = cell_box(0, 0, false);
        // Background filled red somewhere in the cell (red channel high).
        assert!(
            rb.any_chan(x0, y0, x1, y1, 0, 200),
            "cell 0 shows its red background"
        );
        // The glyph inked: white 'M' over red raises the GREEN channel (red bg has
        // g=0), so green > 0 proves coverage was composited from the atlas.
        assert!(
            rb.any_chan(x0, y0, x1, y1, 1, 40),
            "the 'M' glyph composites coverage (green raised over the red bg)"
        );

        // A blank default cell (col 2) draws nothing (canvas bg is skipped, space is
        // inkless) -> stays the black clear.
        let (bx0, by0, bx1, by1) = cell_box(2, 0, false);
        assert!(
            !rb.any_chan(bx0, by0, bx1, by1, 0, 20)
                && !rb.any_chan(bx0, by0, bx1, by1, 1, 20)
                && !rb.any_chan(bx0, by0, bx1, by1, 2, 20),
            "a blank cell stays the clear color (canvas-skip + empty-glyph-skip)"
        );
    }

    #[test]
    fn sprite_glyphs_render_through_the_atlas_pipeline() {
        // T-4.5: box-drawing / block / Powerline sprites reach the atlas and
        // composite end-to-end, just like a font glyph - verified on real GPU.
        let Some((device, queue, format)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let mut grid = GridRenderer::new(&device, format);
        let (w, h) = target_size(4, 1);

        // █ FULL BLOCK, white fg on canvas bg: the cell centre inks white (all
        // channels high) - proves a sprite is drawn, cached, and composited.
        let snap = one_cell(
            4,
            '\u{2588}',
            CellColor::Rgb(255, 255, 255),
            CellColor::Named(257),
            false,
        );
        let rb = render(&device, &queue, &mut grid, &snap, w, h);
        let (x0, y0, x1, y1) = cell_box(0, 0, false);
        let (cx, cy) = ((x0 + x1) / 2, (y0 + y1) / 2);
        assert!(
            rb.any_chan(cx, cy, cx + 1, cy + 1, 0, 150)
                && rb.any_chan(cx, cy, cx + 1, cy + 1, 1, 150)
                && rb.any_chan(cx, cy, cx + 1, cy + 1, 2, 150),
            "full-block sprite fills the cell centre white"
        );

        // ─ LIGHT HORIZONTAL: a thin band at the vertical centre, NOT a full fill -
        // distinguishes the sprite from a block and proves it is the procedural line.
        let mut grid2 = GridRenderer::new(&device, format);
        let snap2 = one_cell(
            4,
            '\u{2500}',
            CellColor::Rgb(255, 255, 255),
            CellColor::Named(257),
            false,
        );
        let rb2 = render(&device, &queue, &mut grid2, &snap2, w, h);
        // A few rows around the vertical centre catch the thin (1px at 1x) band.
        assert!(
            rb2.any_chan(x0, cy.saturating_sub(2), x1, cy + 3, 0, 150),
            "the line inks the cell's vertical centre"
        );
        assert!(
            !rb2.any_chan(x0, y0, x1, y0 + 2, 0, 80),
            "the top of the cell stays blank (a thin line, not a fill)"
        );
    }

    #[test]
    fn glyph_layer_is_a_single_draw_call() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut grid = GridRenderer::new(&device, format);
        let (w, h) = target_size(4, 1);
        let snap = one_cell(
            4,
            'A',
            CellColor::Rgb(255, 255, 255),
            CellColor::Rgb(0, 0, 0),
            false,
        );
        render(&device, &queue, &mut grid, &snap, w, h);
        assert_eq!(
            grid.last_glyph_draw_calls(),
            1,
            "the whole glyph layer is ONE instanced draw call (T-1.6 AC c)"
        );

        // An all-blank grid issues zero glyph draws.
        let mut blank = Snapshot::empty(1, 4);
        blank.version = 2;
        render(&device, &queue, &mut grid, &blank, w, h);
        assert_eq!(
            grid.last_glyph_draw_calls(),
            0,
            "a blank grid draws no glyphs"
        );
    }

    #[test]
    fn repeated_glyph_is_rasterized_once_and_unchanged_frames_reuse() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut grid = GridRenderer::new(&device, format);
        let (w, h) = target_size(4, 1);
        let white = CellColor::Rgb(255, 255, 255);
        let black = CellColor::Rgb(0, 0, 0);

        let snap1 = one_cell(4, 'W', white, black, false);
        render(&device, &queue, &mut grid, &snap1, w, h);
        let after_first = grid.rasterizations();
        assert!(after_first > 0, "the first 'W' is rasterized");

        // Same version + content: prepare must early-out (no re-raster).
        render(&device, &queue, &mut grid, &snap1, w, h);
        assert_eq!(
            grid.rasterizations(),
            after_first,
            "an unchanged frame reuses the build (no rasterization)"
        );

        // A NEW frame (new version) that still contains 'W': the atlas cache hits,
        // so still no re-rasterization (T-1.6 AC5).
        let mut snap2 = one_cell(4, 'W', white, black, false);
        snap2.version = 99;
        render(&device, &queue, &mut grid, &snap2, w, h);
        assert_eq!(
            grid.rasterizations(),
            after_first,
            "a repeated glyph in a new frame is never re-rasterized (atlas reuse)"
        );
    }

    #[test]
    fn steady_state_prepare_is_allocation_free() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut grid = GridRenderer::new(&device, format);
        let (w, h) = target_size(4, 1);
        let snap = one_cell(
            4,
            'S',
            CellColor::Rgb(255, 255, 255),
            CellColor::Rgb(0, 0, 0),
            false,
        );
        let size = FrameSize {
            width: w,
            height: h,
            scale: SCALE,
        };
        // First prepare builds + caches (allocates).
        grid.prepare(&device, &queue, &snap, &theme(), size);

        // An unchanged frame (same version/viewport/theme) must early-out with NO
        // allocation - the steady-state present path (ticket T-1.8 AC1/AC2). This is
        // the renderer-level "skip the rebuild when nothing is dirty".
        let allocs = crate::alloc_probe::count_allocs(|| {
            let drew = grid.prepare(&device, &queue, &snap, &theme(), size);
            std::hint::black_box(drew);
        });
        assert_eq!(
            allocs, 0,
            "an unchanged frame's prepare early-out allocates nothing (got {allocs})"
        );
    }

    #[test]
    fn wide_cell_background_spans_two_columns() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut grid = GridRenderer::new(&device, format);
        let (w, h) = target_size(4, 1);
        // A wide cell with a red bg at col 0 (+ spacer at col 1).
        let snap = one_cell(
            4,
            '\u{4e2d}',
            CellColor::Rgb(255, 255, 255),
            CellColor::Rgb(255, 0, 0),
            true,
        );
        let rb = render(&device, &queue, &mut grid, &snap, w, h);

        // The red background must reach into col 1's x-range (the spacer column),
        // proving the wide cell occupies two columns (AC a).
        let (cw, _) = cell_px(SCALE);
        let inset = INSET_LOGICAL * SCALE;
        let col1_x = (inset + cw * 1.5) as u32; // mid of column 1
        let (_, y0, _, y1) = cell_box(0, 0, true);
        assert!(
            rb.any_chan(col1_x, y0, col1_x + 1, y1, 0, 200),
            "the wide cell's red bg extends across the second column"
        );
    }

    #[test]
    fn two_distinct_glyphs_share_the_atlas_without_bleeding() {
        // Every other GPU test renders ONE glyph into a fresh atlas, so neighbor-glyph
        // bleed (Linear sampling across adjacent atlas rects) is invisible to them.
        // This renders two DISTINCT glyphs into one atlas with a blank cell between
        // them and asserts both ink while the blank cell stays clear - the Nearest
        // sampler + integer-snapped quads must not spill one glyph into the other.
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut grid = GridRenderer::new(&device, format);
        let (w, h) = target_size(4, 1);
        let mut snap = Snapshot::empty(1, 4);
        snap.version = 1;
        let white = CellColor::Rgb(255, 255, 255);
        let black = CellColor::Rgb(0, 0, 0);
        // 'A' at col 0, 'B' at col 1, blank at col 2.
        for (i, ch) in [(0usize, 'A'), (1, 'B')] {
            snap.cells[i].c = ch;
            snap.cells[i].fg = white;
            snap.cells[i].bg = black;
        }
        let rb = render(&device, &queue, &mut grid, &snap, w, h);
        assert!(
            grid.rasterizations() >= 2,
            "two distinct glyphs are rasterized"
        );

        let (a0, ay0, a1, ay1) = cell_box(0, 0, false);
        let (b0, by0, b1, by1) = cell_box(1, 0, false);
        assert!(
            rb.any_chan(a0, ay0, a1, ay1, 0, 60),
            "'A' inked (white on black)"
        );
        assert!(
            rb.any_chan(b0, by0, b1, by1, 0, 60),
            "'B' inked (white on black)"
        );
        // Sample the CENTER of the blank cell (col 2), clear of any neighbor glyph's
        // own ink extent at the cell boundary - any coverage here would be true
        // atlas-neighbor bleed. With Nearest sampling + integer-snapped quads there is
        // none.
        let (cw, _) = cell_px(SCALE);
        let inset = INSET_LOGICAL * SCALE;
        let cx = (inset + cw * 2.5) as u32; // center of column 2
        let (_, cy0, _, cy1) = cell_box(2, 0, false);
        assert!(
            !rb.any_chan(cx - 1, cy0, cx + 2, cy1, 0, 20),
            "the blank cell's center stays clear (no atlas-neighbor bleed)"
        );
    }
}

#[cfg(test)]
mod sig_tests {
    use super::theme_signature;
    use crate::app::effective_theme;
    use aterm_tokens::{Theme, ThemeKind};

    #[test]
    fn theme_signature_distinguishes_themes_and_is_stable() {
        let dark = *Theme::for_kind(ThemeKind::Dark);
        let light = *Theme::for_kind(ThemeKind::Light);
        assert_eq!(
            theme_signature(&dark),
            theme_signature(&dark),
            "the signature is deterministic for a theme"
        );
        assert_ne!(
            theme_signature(&dark),
            theme_signature(&light),
            "a theme change must change the rebuild-gate signature (else the renderer keeps stale colors)"
        );
    }

    #[test]
    fn theme_signature_pins_the_effective_palette_the_renderer_draws() {
        // The theme that actually reaches prepare() is `effective_theme(kind)` (the
        // LIGHT palette AFTER the legibility remap), not the raw `Theme::for_kind`.
        // Pin the runtime-relevant pairs so a future palette edit cannot silently
        // collide the rebuild-gate signatures and keep stale colors on screen.
        let eff_light = effective_theme(ThemeKind::Light);
        let eff_dark = effective_theme(ThemeKind::Dark);
        assert_ne!(
            theme_signature(&eff_light),
            theme_signature(&eff_dark),
            "a light↔dark switch (effective palettes) must invalidate the rebuild gate"
        );
        // The remap must be visible in the signature: the effective light palette
        // differs from the raw light palette, so applying it forces a rebuild.
        assert_ne!(
            theme_signature(&eff_light),
            theme_signature(Theme::for_kind(ThemeKind::Light)),
            "the light legibility remap must change the signature (else its effect is invisible to the gate)"
        );
    }
}
