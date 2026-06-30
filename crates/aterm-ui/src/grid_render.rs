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

use std::mem::size_of;

use aterm_core::Snapshot;
use aterm_tokens::{type_scale, Theme};

use crate::atlas::{GlyphAtlas, GlyphInstance, InstanceBuffer, RectInstance};
use crate::text::{build_grid_cells, FaceStyle, FontFamily, GlyphKey, GridCell};
use crate::window::cell_px;

/// Left/top inset of the grid from the surface origin, in LOGICAL px (scaled by the
/// DPI factor at use). Matches the interim glyphon path's `(8, 8)` offset.
const INSET_LOGICAL: f32 = 8.0;

/// The surface geometry for a frame: physical-pixel size + the DPI scale factor.
/// Bundled so [`GridRenderer::prepare`] stays a tidy call.
#[derive(Debug, Clone, Copy)]
pub struct FrameSize {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
}

/// The instanced grid renderer: the GRID front-end over the shared [`GlyphAtlas`]. It
/// owns the grid layout + the reused instance buffers and the `(version, viewport, px,
/// theme)` rebuild gate; the atlas (texture, cache, rasterizer, the rect + glyph
/// pipelines, the shared viewport uniform) is the shared engine it BORROWS - now owned
/// one level up by [`crate::gpu::GpuRenderer`] (the T-4.6 hoist) so the grid, the
/// timeline, and prose all draw through one atlas. `prepare` (per frame, but early-outs
/// when unchanged) builds instances and `draw` records the rect layer + the single
/// glyph instanced draw into a caller-owned pass, both through the borrowed atlas.
pub struct GridRenderer {
    // Reused CPU + GPU instance storage (grid-owned: the rebuild gate reuses these
    // across frames, so nothing else may write them - see the early-out in `prepare`).
    grid_cells: Vec<GridCell>,
    bg_instances: Vec<RectInstance>,
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
}

impl GridRenderer {
    /// Build the grid front-end: just its reused CPU/GPU instance buffers. The shared
    /// [`GlyphAtlas`] (which owns both pipelines + the viewport uniform) is constructed
    /// and owned by [`crate::gpu::GpuRenderer`] and passed into `prepare`/`draw`.
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            grid_cells: Vec::new(),
            bg_instances: Vec::new(),
            glyph_instances: Vec::new(),
            bg_buf: InstanceBuffer::new(
                device,
                "aterm-bg-instances",
                size_of::<RectInstance>(),
                256,
            ),
            glyph_buf: InstanceBuffer::new(
                device,
                "aterm-glyph-instances",
                size_of::<GlyphInstance>(),
                256,
            ),
            built: None,
            last_glyph_draw_calls: 0,
        }
    }

    /// Glyph-layer draw calls from the last [`Self::draw`] (T-1.6 AC c: 1 when there
    /// is text).
    #[must_use]
    pub fn last_glyph_draw_calls(&self) -> u32 {
        self.last_glyph_draw_calls
    }

    /// Build the frame's instances from `snap` through the shared `atlas`, reusing the
    /// prior build when the `(version, viewport, px, theme)` signature is unchanged (the
    /// damage gate). Returns `true` if there is anything to draw.
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
        atlas: &mut GlyphAtlas,
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
        let metrics = atlas.cell_metrics(FontFamily::Grid, px);
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
                self.bg_instances.push(RectInstance {
                    rect: [cell_x, cell_y, cw_cell, ch],
                    color: cell.bg.to_linear_f32(),
                });
            }
            // Underline: a thin quad just under the baseline.
            if cell.underline {
                let uy = cell_y + baseline_off + (metrics.descent * 0.3).max(1.0);
                self.bg_instances.push(RectInstance {
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
                family: FontFamily::Grid,
                glyph_id: if sprite {
                    cell.ch as u32 as u16 // sprite codepoints are all in the BMP
                } else {
                    atlas.glyph_id(FontFamily::Grid, face, cell.ch)
                },
                face,
                px: px_key,
                sprite,
            };
            // Acquire the glyph from the shared atlas (a cache hit, or rasterize +
            // upload once on a miss). The atlas owns the skip-memo and the once-only
            // guarantee; a sprite codepoint is drawn procedurally into the cell box,
            // everything else from the font's cmap glyph.
            let slot = if sprite {
                atlas.acquire_sprite(queue, gkey, cell.ch, cw_i, ch_i)
            } else {
                atlas.acquire_font(queue, gkey, FontFamily::Grid, face, gkey.glyph_id, px)
            };
            let Some((rect, (left, top))) = slot else {
                continue;
            };
            // Snap the glyph quad to integer pixels so the hinted bitmap maps 1:1 to
            // texels under the Nearest sampler (crisp, no inter-glyph bleed). The cell
            // origin is fractional (cw is ~7.8px), so without this the quad would
            // straddle pixel boundaries.
            //
            // Three placements:
            //  - a sprite fills the cell box, placed at the cell origin (left/top 0);
            //  - a Nerd Font icon (PUA) is scaled/centered/stretched into the cell per
            //    its constraint (T-4.4), replacing the font's often small/off-cell
            //    native placement;
            //  - an ordinary font glyph is baseline-relative at its natural size.
            let (gx, gy, gw, gh) = if sprite {
                (cell_x.round(), cell_y.round(), rect.w as f32, rect.h as f32)
            } else if let Some(con) = crate::constraint::lookup(cell.ch) {
                let p = con.place(rect.w as f32, rect.h as f32, cw_cell, ch);
                (
                    (cell_x + p.x).round(),
                    (cell_y + p.y).round(),
                    p.w.round().max(1.0),
                    p.h.round().max(1.0),
                )
            } else {
                (
                    (cell_x + left as f32).round(),
                    (cell_y + baseline_off - top as f32).round(),
                    rect.w as f32,
                    rect.h as f32,
                )
            };
            let inv = 1.0 / atlas.atlas_dim() as f32;
            self.glyph_instances.push(GlyphInstance {
                rect: [gx, gy, gw, gh],
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
                size_of::<RectInstance>(),
                self.bg_instances.len(),
            );
            queue.write_buffer(
                self.bg_buf.buf(),
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
                self.glyph_buf.buf(),
                0,
                bytemuck::cast_slice(&self.glyph_instances),
            );
        }
        // Write the shared viewport uniform (idempotent; a no-op when the surface size
        // is unchanged). On the CHANGED path only - the early-out above returns before
        // this, so the zero-alloc steady-state present is untouched.
        atlas.set_viewport(queue, viewport_w, viewport_h);

        self.built = Some(key);
        !self.glyph_instances.is_empty() || !self.bg_instances.is_empty()
    }

    /// Record the grid draws into `pass` (which the caller has begun with the canvas
    /// clear) through the shared `atlas`. Background first, then the single glyph draw
    /// (alpha-blended). The glyph layer is EXACTLY ONE instanced draw call (T-1.6 AC c);
    /// the counter stays here because the atlas cannot know its caller.
    pub fn draw(&mut self, pass: &mut wgpu::RenderPass<'_>, atlas: &GlyphAtlas) {
        if !self.bg_instances.is_empty() {
            atlas.draw_rects(pass, &self.bg_buf, self.bg_instances.len());
        }
        if self.glyph_instances.is_empty() {
            self.last_glyph_draw_calls = 0;
        } else {
            atlas.draw_glyphs(pass, &self.glyph_buf, self.glyph_instances.len());
            self.last_glyph_draw_calls = 1;
        }
    }
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
        atlas: &mut GlyphAtlas,
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
            atlas,
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
            grid.draw(&mut pass, atlas);
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
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut grid = GridRenderer::new(&device);
        let (w, h) = target_size(4, 1);
        // 'M' white-on-red at col 0; the rest are blank defaults (canvas bg).
        let snap = one_cell(
            4,
            'M',
            CellColor::Rgb(255, 255, 255),
            CellColor::Rgb(255, 0, 0),
            false,
        );
        let rb = render(&device, &queue, &mut atlas, &mut grid, &snap, w, h);

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
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut grid = GridRenderer::new(&device);
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
        let rb = render(&device, &queue, &mut atlas, &mut grid, &snap, w, h);
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
        let mut atlas2 = GlyphAtlas::new(&device, format);
        let mut grid2 = GridRenderer::new(&device);
        let snap2 = one_cell(
            4,
            '\u{2500}',
            CellColor::Rgb(255, 255, 255),
            CellColor::Named(257),
            false,
        );
        let rb2 = render(&device, &queue, &mut atlas2, &mut grid2, &snap2, w, h);
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
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut grid = GridRenderer::new(&device);
        let (w, h) = target_size(4, 1);
        let snap = one_cell(
            4,
            'A',
            CellColor::Rgb(255, 255, 255),
            CellColor::Rgb(0, 0, 0),
            false,
        );
        render(&device, &queue, &mut atlas, &mut grid, &snap, w, h);
        assert_eq!(
            grid.last_glyph_draw_calls(),
            1,
            "the whole glyph layer is ONE instanced draw call (T-1.6 AC c)"
        );

        // An all-blank grid issues zero glyph draws.
        let mut blank = Snapshot::empty(1, 4);
        blank.version = 2;
        render(&device, &queue, &mut atlas, &mut grid, &blank, w, h);
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
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut grid = GridRenderer::new(&device);
        let (w, h) = target_size(4, 1);
        let white = CellColor::Rgb(255, 255, 255);
        let black = CellColor::Rgb(0, 0, 0);

        let snap1 = one_cell(4, 'W', white, black, false);
        render(&device, &queue, &mut atlas, &mut grid, &snap1, w, h);
        let after_first = atlas.rasterizations();
        assert!(after_first > 0, "the first 'W' is rasterized");

        // Same version + content: prepare must early-out (no re-raster).
        render(&device, &queue, &mut atlas, &mut grid, &snap1, w, h);
        assert_eq!(
            atlas.rasterizations(),
            after_first,
            "an unchanged frame reuses the build (no rasterization)"
        );

        // A NEW frame (new version) that still contains 'W': the atlas cache hits,
        // so still no re-rasterization (T-1.6 AC5).
        let mut snap2 = one_cell(4, 'W', white, black, false);
        snap2.version = 99;
        render(&device, &queue, &mut atlas, &mut grid, &snap2, w, h);
        assert_eq!(
            atlas.rasterizations(),
            after_first,
            "a repeated glyph in a new frame is never re-rasterized (atlas reuse)"
        );
    }

    #[test]
    fn steady_state_prepare_is_allocation_free() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut grid = GridRenderer::new(&device);
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
        grid.prepare(&device, &queue, &mut atlas, &snap, &theme(), size);

        // An unchanged frame (same version/viewport/theme) must early-out with NO
        // allocation - the steady-state present path (ticket T-1.8 AC1/AC2). This is
        // the renderer-level "skip the rebuild when nothing is dirty".
        let allocs = crate::alloc_probe::count_allocs(|| {
            let drew = grid.prepare(&device, &queue, &mut atlas, &snap, &theme(), size);
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
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut grid = GridRenderer::new(&device);
        let (w, h) = target_size(4, 1);
        // A wide cell with a red bg at col 0 (+ spacer at col 1).
        let snap = one_cell(
            4,
            '\u{4e2d}',
            CellColor::Rgb(255, 255, 255),
            CellColor::Rgb(255, 0, 0),
            true,
        );
        let rb = render(&device, &queue, &mut atlas, &mut grid, &snap, w, h);

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
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut grid = GridRenderer::new(&device);
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
        let rb = render(&device, &queue, &mut atlas, &mut grid, &snap, w, h);
        assert!(
            atlas.rasterizations() >= 2,
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

    #[test]
    fn nerd_font_icons_are_constrained_into_the_cell() {
        // T-4.4: a Nerd Font PUA icon is scaled + centered into the cell by the
        // constraint table (not left at the font's small/off-cell native placement),
        // and a Material Design icon in the SMP PUA (beyond the BMP) resolves and
        // rasterizes WITHOUT panic - the ticket's headline edge case. This test
        // presupposes the bundled face covers U+F015 and U+F0001 (it does); the
        // font-free `constraint::tests` cover the no-panic AC independently of the font.
        let Some((device, queue, format)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let (w, h) = target_size(4, 1);
        let white = CellColor::Rgb(255, 255, 255);
        let canvas_bg = CellColor::Named(257); // theme canvas -> skipped, stays black clear
        let (x0, y0, x1, y1) = cell_box(0, 0, false);

        // BMP icon (Font Awesome "home", U+F015): inks the cell's central box,
        // proving the constraint scaled + centered it.
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut grid = GridRenderer::new(&device);
        let snap = one_cell(4, '\u{F015}', white, canvas_bg, false);
        let rb = render(&device, &queue, &mut atlas, &mut grid, &snap, w, h);
        let (bx0, bx1) = (x0 + (x1 - x0) / 4, x1 - (x1 - x0) / 4);
        let (by0, by1) = (y0 + (y1 - y0) / 4, y1 - (y1 - y0) / 4);
        assert!(
            rb.any_chan(bx0, by0, bx1, by1, 0, 40),
            "a PUA icon is scaled + centered into the cell (inks the centre box)"
        );

        // SMP icon (Material Design, U+F0001, beyond the BMP): must render without
        // panic and reach the atlas (some ink in the cell).
        let mut atlas2 = GlyphAtlas::new(&device, format);
        let mut grid2 = GridRenderer::new(&device);
        let snap2 = one_cell(4, '\u{F0001}', white, canvas_bg, false);
        let rb2 = render(&device, &queue, &mut atlas2, &mut grid2, &snap2, w, h);
        assert!(
            rb2.any_chan(x0, y0, x1, y1, 0, 30),
            "a beyond-BMP MDI icon resolves, rasterizes, and inks the cell (no panic)"
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
