//! The custom window title bar (ticket T-9.2): the chrome strip every screen sits under
//! (including alt-screen, T-9.9 - the native buttons floating over it are permanent
//! chrome), drawn to the vision mock (ADR-0011) `<!-- title bar -->` block.
//!
//! It is another front-end over the shared [`GlyphAtlas`] (like the grid, prose, timeline,
//! and input box): a bar with a bottom `hairline` rule carrying a sidebar-toggle glyph in
//! `fg.muted` and an absolutely-centered active title (`fg.primary`) + `  -  <cwd>`
//! (`fg.muted`). The host reserves the top [`title_bar_px`] so the timeline lays out below
//! it.
//!
//! ## The native transparent titlebar (ticket T-9.9)
//! The window keeps its NATIVE `.titled` chrome with a TRANSPARENT titlebar (see
//! [`crate::window`]): macOS paints no titlebar background, but the REAL traffic-light
//! buttons float over this bar's top-left. So this bar draws NO window controls of its
//! own - the left [`TRAFFIC_LIGHT_INSET_LOGICAL`] is left empty for the native buttons,
//! and the bar's height matches the native 28pt titlebar band so the buttons (AppKit
//! centers them at y=14pt; immovable without private API) share the bar's vertical
//! center with the toggle glyph + title.
//!
//! ## Scope
//! - The sidebar-toggle glyph is drawn + hover/clickable (T-9.8); the sidebar PANEL it
//!   will open/close is EPIC-10. The toggle-sidebar INTENT path lives in `aterm-app`.
//!
//! ## Damage gating
//! Like the input box, [`Self::prepare`] keys a full rebuild on a cheap FNV signature over
//! everything drawn (title, cwd, viewport, px, the colors read) and early-outs (reusing the
//! prior buffers, ZERO allocation) when nothing changed - so an idle present allocates
//! nothing (the T-1.8 60fps floor).

use std::mem::size_of;

use aterm_tokens::{space, type_scale, Rgba, Theme};

use crate::atlas::{GlyphAtlas, GlyphInstance, InstanceBuffer, RectInstance};
use crate::cell_render::{emit_cell, CellCtx};
use crate::grid_render::FrameSize;
use crate::hit::{HitRect, HitTarget};
use crate::prose::ProseShaper;
use crate::text::{FaceStyle, FontFamily, GlyphKey, GridCell};
use crate::window::cell_px;

/// The title bar's height in LOGICAL px; scaled to physical at draw. Matches the native
/// macOS titlebar band (28pt), where AppKit vertically centers the real traffic-light
/// buttons (at y=14pt, fixed) - so the bar's own centered content shares their center
/// (ticket T-9.9). Exposed so the host ([`crate::gpu`]) reserves this top band and lays
/// the timeline out below it via [`title_bar_px`].
pub const TITLE_BAR_LOGICAL: f32 = 28.0;

/// The left inset in LOGICAL px reserved for the NATIVE traffic-light buttons (ticket
/// T-9.9): the button cluster ends at x=61pt on macOS 15 (frames at x=7/27/47, 14pt
/// wide), and ~71pt is the de-facto convention for the first custom control after it
/// (Zed's `TRAFFIC_LIGHT_PADDING`). The bar draws NOTHING to the left of this.
pub const TRAFFIC_LIGHT_INSET_LOGICAL: f32 = 71.0;

/// The sidebar-toggle glyph: `nf-fa-columns` (U+F0DB), a two-panel icon that stands in for
/// the mock's `◧` (U+25E7 SQUARE WITH LEFT HALF BLACK, `.notdef` in the bundled Mono Nerd
/// Font). Present + coverage-tested (`toggle_glyphs_exist_in_the_bundled_grid_font`).
const SIDEBAR_TOGGLE_GLYPH: char = '\u{f0db}';

/// The title/cwd separator: `  -  ` (a plain hyphen, NOT the mock's em dash) between the
/// active title and the cwd, per the ticket's `  -  <cwd>`.
const TITLE_CWD_SEP: &str = "  -  ";

/// The title bar's physical-px height at `scale`.
#[must_use]
pub fn title_bar_px(scale: f32) -> f32 {
    (TITLE_BAR_LOGICAL * scale).round().max(1.0)
}

/// A borrowed, allocation-free view of the title-bar content the host supplies: the active
/// title and the current working directory (from OSC-7 if the host has it, else the process
/// cwd). EPIC-10 replaces `title` with the active session name and makes the toggle glyph
/// open/close the sidebar panel.
#[derive(Debug, Clone, Copy)]
pub struct TitleBarView<'a> {
    pub title: &'a str,
    pub cwd: &'a str,
}

/// The title-bar front-end over the shared [`GlyphAtlas`]. Owns its own instance buffers +
/// rebuild gate + a [`ProseShaper`] for the centered title/cwd; draws through the shared
/// rect + glyph pipelines. Constructed once from the device.
pub struct TitleBarRenderer {
    bg_instances: Vec<RectInstance>,
    glyph_instances: Vec<GlyphInstance>,
    bg_buf: InstanceBuffer,
    glyph_buf: InstanceBuffer,
    /// Shapes the proportional Quattro title + cwd; its glyphs join `glyph_instances` so the
    /// whole bar is one glyph draw.
    shaper: ProseShaper,
    built: Option<u64>,
    last_glyph_draw_calls: u32,
    /// The sidebar-toggle glyph's clickable region in physical px (ticket T-9.8), cached on
    /// the rebuild path so the host's pointer path can hit-test it even on an idle frame that
    /// early-outs. `None` until the bar has drawn once.
    sidebar_toggle_rect: Option<HitRect>,
}

impl TitleBarRenderer {
    /// Build the title-bar front-end: its reused instance buffers + the title/cwd shaper.
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            bg_instances: Vec::new(),
            glyph_instances: Vec::new(),
            bg_buf: InstanceBuffer::new(device, "aterm-titlebar-bg", size_of::<RectInstance>(), 16),
            glyph_buf: InstanceBuffer::new(
                device,
                "aterm-titlebar-glyph",
                size_of::<GlyphInstance>(),
                64,
            ),
            shaper: ProseShaper::new(),
            built: None,
            last_glyph_draw_calls: 0,
            sidebar_toggle_rect: None,
        }
    }

    /// Glyph-layer draw calls from the last [`Self::draw`] (1 when there is text).
    #[must_use]
    pub fn last_glyph_draw_calls(&self) -> u32 {
        self.last_glyph_draw_calls
    }

    /// The sidebar-toggle glyph's clickable region in physical px (ticket T-9.8), or `None`
    /// before the first draw. The host pushes this into the frame's [`crate::hit::HitMap`] as
    /// a [`HitTarget::SidebarToggle`] so a pointer click drives the same intent as `Cmd-B`.
    #[must_use]
    pub fn sidebar_toggle_rect(&self) -> Option<HitRect> {
        self.sidebar_toggle_rect
    }

    /// Build the frame's instances for `view` through the shared `atlas`, reusing the prior
    /// build when the signature is unchanged (the damage gate). Returns `true` if there is
    /// anything to draw. The bar occupies the TOP [`title_bar_px`] of the surface; the host
    /// reserves that band so the timeline lays out below it.
    ///
    /// The unchanged path allocates nothing (the steady-state early-out); the changed path
    /// reuses its warm `Vec`s + the glyph cache (`queue.write_buffer` is wgpu staging).
    #[allow(clippy::too_many_arguments)] // by-value frame-path args, like the other front-ends
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        view: &TitleBarView,
        hovered: Option<HitTarget>,
        theme: &Theme,
        size: FrameSize,
    ) -> bool {
        let FrameSize {
            width,
            height: _,
            scale,
        } = size;
        let px = (type_scale::GRID.size_pt * scale).round().max(1.0);
        let px_key = px as u32;

        // The sidebar-toggle glyph brightens on pointer hover (ticket T-9.8), the mock's
        // `.navitem:hover { color: var(--ink) }`. Folded into the signature so a hover
        // change forces exactly one rebuild.
        let toggle_hovered = hovered == Some(HitTarget::SidebarToggle);
        let sig = fold_bool(signature(view, width, px_key, theme), toggle_hovered);
        if self.built == Some(sig) {
            return !self.glyph_instances.is_empty() || !self.bg_instances.is_empty();
        }

        self.bg_instances.clear();
        self.glyph_instances.clear();

        let (cw, ch) = cell_px(scale);
        let bar_h = title_bar_px(scale);
        let canvas = theme.colors.bg_canvas;
        let metrics = atlas.cell_metrics(FontFamily::Grid, px);
        let baseline_off = (ch - metrics.line) * 0.5 + metrics.ascent;
        let ctx = CellCtx {
            cw,
            ch,
            cw_i: cw.round().max(1.0) as u32,
            ch_i: ch.round().max(1.0) as u32,
            baseline_off,
            descent: metrics.descent,
            px,
            px_key,
            atlas_dim: atlas.atlas_dim(),
            canvas,
        };

        let hairline_h = (f32::from(space::HAIRLINE_WIDTH) * scale).round().max(1.0);
        // Bottom hairline rule spanning the full bar width (the mock's `border-bottom`).
        self.bg_instances.push(RectInstance {
            rect: [0.0, (bar_h - hairline_h).round(), width as f32, hairline_h],
            color: theme.colors.hairline.to_linear_f32(),
        });

        // A cell row vertically centered in the bar, for the toggle glyph.
        let row_y = ((bar_h - ch) * 0.5).max(0.0);
        let c = &theme.colors;

        // The sidebar-toggle glyph, just right of the NATIVE traffic-light buttons (ticket
        // T-9.9): everything left of the inset is left empty for the real buttons floating
        // over this bar. `fg.muted` at rest, brightening to `fg.primary` on pointer hover
        // (ticket T-9.8).
        let toggle_x = TRAFFIC_LIGHT_INSET_LOGICAL * scale;
        let toggle_fg = if toggle_hovered {
            c.fg_primary
        } else {
            c.fg_muted
        };
        let toggle = grid_glyph(SIDEBAR_TOGGLE_GLYPH, toggle_fg, canvas);
        emit_cell(
            atlas,
            queue,
            &toggle,
            (toggle_x, row_y),
            &ctx,
            &mut self.bg_instances,
            &mut self.glyph_instances,
        );
        // Cache its clickable region (ticket T-9.8): the glyph cell padded to a comfortable
        // pointer target, clamped to the bar. Physical px, so the host hit-tests it directly.
        let hit_pad = 6.0 * scale;
        self.sidebar_toggle_rect = Some([
            (toggle_x - hit_pad).max(0.0),
            (row_y - hit_pad).max(0.0),
            cw + 2.0 * hit_pad,
            (ch + 2.0 * hit_pad).min(bar_h),
        ]);

        // The absolutely-centered title (`fg.primary`) + `  -  <cwd>` (`fg.muted`), shaped
        // in the Quattro UI face. Two runs so the two colors differ; centered as one group.
        let px_label = (type_scale::LABEL.size_pt * scale).round().max(1.0);
        let line_h_label = px_label * type_scale::LABEL.line_height;
        let title_layout = self.shaper.layout(
            view.title,
            FontFamily::Ui,
            FaceStyle::Regular,
            px_label,
            f32::MAX,
            line_h_label,
        );
        let cwd_run = format!("{TITLE_CWD_SEP}{}", view.cwd);
        let cwd_layout = self.shaper.layout(
            &cwd_run,
            FontFamily::Ui,
            FaceStyle::Regular,
            px_label,
            f32::MAX,
            line_h_label,
        );
        let total_w = title_layout.width + cwd_layout.width;
        // Center in the bar, but never let it collide with the left chrome (clamp its start
        // to the right of the toggle glyph + one cell of breathing room).
        let group_x0 = ((width as f32 - total_w) * 0.5).max(toggle_x + 2.0 * cw);
        let group_y = ((bar_h - title_layout.height) * 0.5).max(0.0);
        self.place_run(queue, atlas, &title_layout, group_x0, group_y, c.fg_primary);
        self.place_run(
            queue,
            atlas,
            &cwd_layout,
            group_x0 + title_layout.width,
            group_y,
            c.fg_muted,
        );

        if !self.bg_instances.is_empty() {
            self.bg_buf.ensure(
                device,
                "aterm-titlebar-bg",
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
                "aterm-titlebar-glyph",
                size_of::<GlyphInstance>(),
                self.glyph_instances.len(),
            );
            queue.write_buffer(
                self.glyph_buf.buf(),
                0,
                bytemuck::cast_slice(&self.glyph_instances),
            );
        }
        atlas.set_viewport(queue, width, size.height);

        self.built = Some(sig);
        !self.glyph_instances.is_empty() || !self.bg_instances.is_empty()
    }

    /// Place one shaped Quattro run's glyphs into the shared glyph buffer at `(x0, y0)` in
    /// `color`. Mirrors the input box's chip-label placement.
    fn place_run(
        &mut self,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        layout: &crate::prose::ProseLayout,
        x0: f32,
        y0: f32,
        color: Rgba,
    ) {
        // The glyphs were shaped + rounded to `layout.px`; rasterize + key at that same px
        // so the hinted bitmap maps 1:1 under the atlas's Nearest sampler.
        let px_label = layout.px;
        let inv = 1.0 / atlas.atlas_dim() as f32;
        let color = color.to_linear_f32();
        for pg in &layout.glyphs {
            let key = GlyphKey {
                family: FontFamily::Ui,
                glyph_id: pg.glyph_id,
                face: FaceStyle::Regular,
                px: px_label as u32,
                sprite: false,
            };
            let Some((rect, (left, top))) = atlas.acquire_font(
                queue,
                key,
                FontFamily::Ui,
                FaceStyle::Regular,
                pg.glyph_id,
                px_label,
            ) else {
                continue;
            };
            self.glyph_instances.push(GlyphInstance {
                rect: [
                    (x0 + pg.pen_x + left as f32).round(),
                    (y0 + pg.baseline - top as f32).round(),
                    rect.w as f32,
                    rect.h as f32,
                ],
                uv: [
                    rect.x as f32 * inv,
                    rect.y as f32 * inv,
                    (rect.x + rect.w) as f32 * inv,
                    (rect.y + rect.h) as f32 * inv,
                ],
                color,
            });
        }
    }

    /// Record the title-bar draws into `pass` through the shared `atlas`: the solid layer
    /// (bottom hairline) first, then the single glyph instanced draw (toggle + text).
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

/// A single Mono cell carrying `ch` in `fg` on the canvas background (so the background
/// quad is skipped by [`emit_cell`]); used for the toggle glyph.
fn grid_glyph(ch: char, fg: Rgba, canvas: Rgba) -> GridCell {
    GridCell {
        col: 0,
        row: 0,
        ch,
        fg,
        bg: canvas,
        bold: false,
        italic: false,
        underline: false,
        wide: false,
    }
}

/// A stable u64 over everything the title bar draws: the title, the cwd, the width, the px,
/// and the colors read. Computed every frame BEFORE the rebuild gate, so it allocates
/// nothing (folds borrowed strs + small counts only). The height is fixed
/// ([`TITLE_BAR_LOGICAL`]) modulo the px/scale already folded, so it is not folded.
/// Fold a hover bool into a base signature (ticket T-9.8) so a pointer hover change forces
/// exactly one title-bar rebuild. Kept separate from [`signature`] so its sig-tests are
/// unchanged, mirroring the timeline's `fold_top_inset`.
fn fold_bool(base: u64, b: bool) -> u64 {
    (base ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
}

fn signature(view: &TitleBarView, w: u32, px_key: u32, theme: &Theme) -> u64 {
    fn fold_u64(h: u64, v: u64) -> u64 {
        (h ^ v).wrapping_mul(0x0000_0100_0000_01b3)
    }
    fn fold_color(h: u64, c: Rgba) -> u64 {
        fold_u64(h, u64::from(c.to_u32()))
    }
    fn fold_str(mut h: u64, s: &str) -> u64 {
        h = fold_u64(h, s.len() as u64);
        for ch in s.chars() {
            h = fold_u64(h, ch as u64);
        }
        h
    }

    let mut s: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
    s = fold_str(s, view.title);
    s = fold_str(s, view.cwd);
    s = fold_u64(s, u64::from(w));
    s = fold_u64(s, u64::from(px_key));
    let c = &theme.colors;
    for color in [c.bg_canvas, c.fg_primary, c.fg_muted, c.hairline] {
        s = fold_color(s, color);
    }
    s
}

// ---------------------------------------------------------------------------
// Pure (no-GPU) tests - run on every platform
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use aterm_tokens::ThemeKind;

    #[test]
    fn title_bar_px_scales_and_is_positive() {
        assert_eq!(title_bar_px(1.0), TITLE_BAR_LOGICAL);
        assert!(title_bar_px(2.0) > title_bar_px(1.0));
        assert!(title_bar_px(0.0) >= 1.0, "clamped to at least one px");
    }

    #[test]
    fn title_bar_glyphs_exist_in_the_bundled_grid_font() {
        // The toggle renders through the Mono GRID face; a glyph missing from the
        // bundled Nerd Font would draw `.notdef` (a box) and silently break the chrome -
        // the same regression the gutter/prompt/chip glyph tests guard. A cmap lookup of 0
        // IS `.notdef`, so it must resolve non-zero. Pure font parse: every platform.
        let r = crate::glyph::GlyphRasterizer::new();
        let gid = r.glyph_id(FontFamily::Grid, FaceStyle::Regular, SIDEBAR_TOGGLE_GLYPH);
        assert_ne!(
            gid, 0,
            "title-bar glyph U+{:04X} is .notdef in the bundled Mono Nerd Font",
            SIDEBAR_TOGGLE_GLYPH as u32
        );
    }

    #[test]
    fn signature_is_stable_and_changes_on_each_drawn_axis() {
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let base = TitleBarView {
            title: "aterm",
            cwd: "~/projects/aterm",
        };
        let s = signature(&base, 960, 13, &theme);
        assert_eq!(s, signature(&base, 960, 13, &theme), "deterministic");
        assert_ne!(
            s,
            signature(
                &TitleBarView {
                    title: "dev server",
                    cwd: "~/projects/aterm"
                },
                960,
                13,
                &theme
            ),
            "title"
        );
        assert_ne!(
            s,
            signature(
                &TitleBarView {
                    title: "aterm",
                    cwd: "~/other"
                },
                960,
                13,
                &theme
            ),
            "cwd"
        );
        assert_ne!(s, signature(&base, 961, 13, &theme), "width");
        assert_ne!(s, signature(&base, 960, 26, &theme), "px");
        assert_ne!(
            s,
            signature(&base, 960, 13, Theme::for_kind(ThemeKind::Light)),
            "theme"
        );
    }
}

// The title bar draws to a real GPU through the shared atlas, so it is verified offscreen
// and read back - macOS-only, skipping when no adapter is present (the same harness as the
// grid/prose/timeline/input GPU tests). These cover: the title-bar chrome inks (centered
// title + hairline) in both themes while the native traffic-light inset stays EMPTY, the
// glyph layer is one draw call, and the damage gate early-outs alloc-free.
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
            label: Some("aterm-titlebar-test"),
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
        w: u32,
        h: u32,
    }
    impl Readback {
        fn lum(&self, x: u32, y: u32) -> u8 {
            let o = y as usize * self.stride + x as usize * 4;
            self.data[o].max(self.data[o + 1]).max(self.data[o + 2])
        }
        fn any_ink(&self, x0: u32, y0: u32, x1: u32, y1: u32, thresh: u8) -> bool {
            (y0..y1.min(self.h)).any(|y| (x0..x1.min(self.w)).any(|x| self.lum(x, y) > thresh))
        }
    }

    #[allow(clippy::too_many_arguments)] // a test-only offscreen-render harness
    fn render(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        tb: &mut TitleBarRenderer,
        view: &TitleBarView,
        hovered: Option<HitTarget>,
        theme: &Theme,
        w: u32,
        h: u32,
    ) -> Readback {
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tb-target"),
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
            label: Some("tb-readback"),
            size: (stride as u32 * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        tb.prepare(
            device,
            queue,
            atlas,
            view,
            hovered,
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
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tb-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &vw,
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
            tb.draw(&mut pass, atlas);
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

    #[test]
    fn title_bar_inks_centered_title_and_keeps_the_native_button_inset_empty() {
        let Some((device, queue, format)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let (w, h) = (480u32, 120u32);
        let view = TitleBarView {
            title: "aterm",
            cwd: "~/projects/aterm",
        };
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let theme = *Theme::for_kind(kind);
            let mut atlas = GlyphAtlas::new(&device, format);
            let mut tb = TitleBarRenderer::new(&device);
            let rb = render(
                &device, &queue, &mut atlas, &mut tb, &view, None, &theme, w, h,
            );

            let bar_h = title_bar_px(SCALE) as u32;
            // The traffic-light inset draws NOTHING (ticket T-9.9): the REAL native buttons
            // float there, so any ink under them would clash. Sampled above the hairline.
            let inset = (TRAFFIC_LIGHT_INSET_LOGICAL * SCALE) as u32;
            // (Minus a small rounding slack so the toggle glyph's own left edge, pinned AT
            // the inset, can never trip this.)
            assert!(
                !rb.any_ink(0, 0, inset.saturating_sub(4), bar_h.saturating_sub(2), 10),
                "{kind:?}: the native traffic-light inset stays empty (no drawn dots)"
            );
            // The sidebar-toggle glyph inks just right of the inset - sampled ABOVE the
            // hairline (which spans the full width and would satisfy any_ink on its own).
            assert!(
                rb.any_ink(inset, 0, inset + 30, bar_h.saturating_sub(2), 25),
                "{kind:?}: the sidebar-toggle glyph inks right of the native buttons"
            );
            // The centered title inks in the middle of the bar.
            assert!(
                rb.any_ink(w / 2 - 40, 0, w / 2 + 40, bar_h, 25),
                "{kind:?}: the centered title inks in the middle of the title bar"
            );
            // The bottom hairline inks across the bar, sampled clear of the centered text.
            assert!(
                rb.any_ink(w - 40, bar_h.saturating_sub(2), w - 8, bar_h, 15),
                "{kind:?}: the bottom hairline inks across the title bar"
            );
        }
    }

    #[test]
    fn sidebar_toggle_re_inks_on_hover_in_both_themes() {
        // T-9.8 AC3/AC5: a hover over the sidebar-toggle glyph drives a redraw whose toggle
        // cell inks DIFFERENTLY (`fg.muted` -> `fg.primary`), in both themes. The offscreen
        // target clears to black, so "brighter" is theme-direction-dependent (primary is the
        // strongest ink: lighter on dark, near-black on light); the theme-agnostic proof of a
        // hover-driven redraw is that the toggle region's pixels CHANGE. Sampling the exact
        // cached hit rect keeps the check on the glyph, not incidental relayout.
        let Some((device, queue, format)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let (w, h) = (480u32, 120u32);
        let view = TitleBarView {
            title: "aterm",
            cwd: "~/p",
        };
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let theme = *Theme::for_kind(kind);
            let mut atlas = GlyphAtlas::new(&device, format);

            let mut rest = TitleBarRenderer::new(&device);
            let rb_rest = render(
                &device, &queue, &mut atlas, &mut rest, &view, None, &theme, w, h,
            );
            let rect = rest
                .sidebar_toggle_rect()
                .expect("the toggle rect is cached after a draw");

            let mut hot = TitleBarRenderer::new(&device);
            let rb_hot = render(
                &device,
                &queue,
                &mut atlas,
                &mut hot,
                &view,
                Some(HitTarget::SidebarToggle),
                &theme,
                w,
                h,
            );

            // Max per-pixel luminance delta over the toggle cell: non-trivial iff the glyph
            // re-inked in a different tone on hover.
            let (x0, y0) = (rect[0] as u32, rect[1] as u32);
            let (x1, y1) = ((rect[0] + rect[2]) as u32, (rect[1] + rect[3]) as u32);
            let mut max_delta = 0i32;
            for y in y0..y1.min(rb_rest.h) {
                for x in x0..x1.min(rb_rest.w) {
                    let d = i32::from(rb_hot.lum(x, y)) - i32::from(rb_rest.lum(x, y));
                    max_delta = max_delta.max(d.abs());
                }
            }
            assert!(
                max_delta > 10,
                "{kind:?}: the hovered sidebar toggle re-inks (max |delta| = {max_delta})"
            );
        }
    }

    #[test]
    fn title_bar_glyph_layer_is_a_single_draw_call() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut tb = TitleBarRenderer::new(&device);
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let view = TitleBarView {
            title: "aterm",
            cwd: "~/p",
        };
        render(
            &device, &queue, &mut atlas, &mut tb, &view, None, &theme, 480, 120,
        );
        assert_eq!(
            tb.last_glyph_draw_calls(),
            1,
            "the whole title-bar glyph layer (toggle + title + cwd) is ONE draw"
        );
    }

    #[test]
    fn unchanged_title_bar_skips_rebuild_alloc_free() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut tb = TitleBarRenderer::new(&device);
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let view = TitleBarView {
            title: "aterm",
            cwd: "~/projects/aterm",
        };
        let size = FrameSize {
            width: 480,
            height: 120,
            scale: SCALE,
        };
        tb.prepare(&device, &queue, &mut atlas, &view, None, &theme, size);
        let allocs = crate::alloc_probe::count_allocs(|| {
            let drew = tb.prepare(&device, &queue, &mut atlas, &view, None, &theme, size);
            std::hint::black_box(drew);
        });
        assert_eq!(
            allocs, 0,
            "an unchanged title-bar frame's prepare early-out allocates nothing (got {allocs})"
        );
    }
}
