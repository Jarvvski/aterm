//! The tab-completion popover (ticket T-9.5): the fuzzy finder that hugs the prompt, drawn
//! to the vision mock (ADR-0011) `completeOpen` block in the input bar.
//!
//! Another front-end over the shared [`GlyphAtlas`]. It reads the pure
//! [`aterm_core::Completion`] state the host owns (open flag, ranked items, active row) and
//! draws a floating panel anchored just above the input's left edge: an elevated-surface
//! fill + hairline border, a faint header (count + key hints), and one row per candidate -
//! a `>` pointer on the active row (in the accent), the candidate with fuzzy-matched letters
//! in `accent.primary` and the rest in `fg.secondary`, then a faint description; the active
//! row sits on a weak accent tint. All cells go through the Mono grid emitter so the
//! candidates are pixel-identical to the command line.
//!
//! ## Scope (T-9.5)
//! This is the VISUAL + interaction render. Candidate SOURCES + the keyboard nav wiring
//! (Tab/up/down/Enter/Esc -> the core [`aterm_core::Completion`]) are the host's (T-8.5 owns
//! the richer sources: `$PATH`, Fig specs; T-9.5 seeds from shell history).
//!
//! ## Damage gating
//! [`Self::prepare`] keys a rebuild on a cheap FNV signature over everything drawn (each
//! item's text/desc/hits, the active index, the anchor, px, the colors) and early-outs
//! alloc-free when unchanged - the T-1.8 floor.

use std::mem::size_of;

use aterm_tokens::{space, type_scale, Rgba, Theme};

use aterm_core::Completion;

use crate::atlas::{GlyphAtlas, GlyphInstance, InstanceBuffer, RectInstance};
use crate::cell_render::{emit_cell, CellCtx};
use crate::grid_render::{FrameSize, INSET_LOGICAL};
use crate::text::{FontFamily, GridCell};
use crate::window::cell_px;

/// The active-row pointer (the mock's `›`); an ASCII `>` (always present in Mono, unlike the
/// U+203A single guillemet, whose Mono coverage is not guaranteed).
const POINTER: char = '>';
/// The gap in header hint separators (the mock's middle dot, U+00B7 - present in Mono).
const SEP: char = '\u{00B7}';

/// The completion popover front-end over the shared [`GlyphAtlas`].
pub struct CompletionRenderer {
    bg_instances: Vec<RectInstance>,
    glyph_instances: Vec<GlyphInstance>,
    bg_buf: InstanceBuffer,
    glyph_buf: InstanceBuffer,
    built: Option<u64>,
    last_glyph_draw_calls: u32,
}

impl CompletionRenderer {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            bg_instances: Vec::new(),
            glyph_instances: Vec::new(),
            bg_buf: InstanceBuffer::new(
                device,
                "aterm-completion-bg",
                size_of::<RectInstance>(),
                32,
            ),
            glyph_buf: InstanceBuffer::new(
                device,
                "aterm-completion-glyph",
                size_of::<GlyphInstance>(),
                256,
            ),
            built: None,
            last_glyph_draw_calls: 0,
        }
    }

    #[must_use]
    pub fn last_glyph_draw_calls(&self) -> u32 {
        self.last_glyph_draw_calls
    }

    /// Build the popover for `completion` through the shared `atlas`. `input_zone_top` is the
    /// top edge (physical px) of the input bar, so the panel's bottom sits just above it and
    /// grows upward. Returns `true` if anything drew. Damage-gated + alloc-free unchanged.
    #[allow(clippy::too_many_arguments)] // by-value frame-path args, like the other front-ends
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        completion: &Completion,
        input_zone_top: f32,
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

        let sig = signature(completion, width, input_zone_top, px_key, theme);
        if self.built == Some(sig) {
            return !self.glyph_instances.is_empty() || !self.bg_instances.is_empty();
        }

        self.bg_instances.clear();
        self.glyph_instances.clear();

        // Nothing to draw when closed / empty.
        let items = completion.items();
        if !completion.is_open() || items.is_empty() {
            self.built = Some(sig);
            return false;
        }

        let (cw, ch) = cell_px(scale);
        let inset = INSET_LOGICAL * scale;
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
        let c = &theme.colors;

        // The faint header: "N <sep> tab/enter accept <sep> up/down move <sep> esc".
        let header = format!(
            "{} {SEP} tab/enter accept {SEP} up/down move {SEP} esc",
            items.len()
        );

        // Width in CELLS: the widest of the header and every "> candidate  desc" row.
        let header_cols = header.chars().count();
        let mut max_cols = header_cols;
        for it in items {
            // pointer(1) + gap(1) + candidate + gap(2) + desc
            let cols = 2 + it.text.chars().count() + 2 + it.desc.chars().count();
            max_cols = max_cols.max(cols);
        }

        let pad = f32::from(space::S2) * scale;
        let hairline_h = (f32::from(space::HAIRLINE_WIDTH) * scale).round().max(1.0);
        let content_w = max_cols as f32 * cw;
        let box_w = content_w + 2.0 * pad;
        let rows = items.len();
        let box_h = (1 + rows) as f32 * ch + 2.0 * pad; // header + rows
        let box_left = inset;
        let gap_above_input = f32::from(space::S1) * scale + 2.0 * scale; // ~6px (the mock)
        let box_bottom = (input_zone_top - gap_above_input).max(box_h);
        let box_top = (box_bottom - box_h).max(0.0);

        // The elevated-surface panel: an outer hairline border rect + an inset bg.elev fill.
        self.bg_instances.push(RectInstance {
            rect: [box_left, box_top, box_w, box_h],
            color: c.hairline.to_linear_f32(),
        });
        self.bg_instances.push(RectInstance {
            rect: [
                box_left + hairline_h,
                box_top + hairline_h,
                (box_w - 2.0 * hairline_h).max(0.0),
                (box_h - 2.0 * hairline_h).max(0.0),
            ],
            color: c.bg_elev.to_linear_f32(),
        });

        let content_x = box_left + pad;
        // The header row (faint), cells drawn with bg=canvas so emit_cell skips per-cell
        // quads and the glyphs sit on the panel fill.
        let header_y = box_top + pad;
        self.emit_text(atlas, queue, &ctx, &header, content_x, header_y, c.fg_muted);

        // Candidate rows.
        for (i, it) in items.iter().enumerate() {
            let row_y = box_top + pad + (1 + i) as f32 * ch;
            let active = i == completion.index();
            if active {
                // Weak accent tint behind the active row (inside the panel content width).
                self.bg_instances.push(RectInstance {
                    rect: [box_left + hairline_h, row_y, box_w - 2.0 * hairline_h, ch],
                    color: c.accent_primary_weak.to_linear_f32(),
                });
            }
            // Pointer (accent on the active row, blank otherwise).
            if active {
                self.emit_cell_at(
                    atlas,
                    queue,
                    &ctx,
                    POINTER,
                    c.accent_primary,
                    content_x,
                    row_y,
                );
            }
            // Candidate: matched chars in the accent; the rest in fg.secondary, brightened to
            // fg.primary on the active row (the mock's active-row `color: var(--ink)`).
            let cand_x = content_x + 2.0 * cw; // after the pointer + a gap
            let plain = if active { c.fg_primary } else { c.fg_secondary };
            for (k, cch) in it.text.chars().enumerate() {
                let hit = it.hits.get(k).copied().unwrap_or(false);
                let fg = if hit { c.accent_primary } else { plain };
                self.emit_cell_at(atlas, queue, &ctx, cch, fg, cand_x + k as f32 * cw, row_y);
            }
            // Description (faint), two cells after the candidate.
            if !it.desc.is_empty() {
                let desc_x = cand_x + (it.text.chars().count() as f32 + 2.0) * cw;
                self.emit_text(atlas, queue, &ctx, &it.desc, desc_x, row_y, c.fg_muted);
            }
        }

        if !self.bg_instances.is_empty() {
            self.bg_buf.ensure(
                device,
                "aterm-completion-bg",
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
                "aterm-completion-glyph",
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

    /// Emit a run of Mono cells for `text` in `fg` starting at `(x, y)`.
    #[allow(clippy::too_many_arguments)]
    fn emit_text(
        &mut self,
        atlas: &mut GlyphAtlas,
        queue: &wgpu::Queue,
        ctx: &CellCtx,
        text: &str,
        x: f32,
        y: f32,
        fg: Rgba,
    ) {
        for (k, ch) in text.chars().enumerate() {
            self.emit_cell_at(atlas, queue, ctx, ch, fg, x + k as f32 * ctx.cw, y);
        }
    }

    /// Emit one Mono cell (`ch` in `fg`) at `(x, y)`, with bg=canvas so emit_cell skips the
    /// per-cell background quad (the panel fill already provides the surface).
    #[allow(clippy::too_many_arguments)]
    fn emit_cell_at(
        &mut self,
        atlas: &mut GlyphAtlas,
        queue: &wgpu::Queue,
        ctx: &CellCtx,
        ch: char,
        fg: Rgba,
        x: f32,
        y: f32,
    ) {
        let cell = GridCell {
            col: 0,
            row: 0,
            ch,
            fg,
            bg: ctx.canvas,
            bold: false,
            italic: false,
            underline: false,
            wide: false,
        };
        emit_cell(
            atlas,
            queue,
            &cell,
            (x, y),
            ctx,
            &mut self.bg_instances,
            &mut self.glyph_instances,
        );
    }

    /// Record the popover draws into `pass`: the solid layer (panel fill/border + active-row
    /// tint) then the one glyph instanced draw.
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

/// A stable u64 over everything the popover draws: open flag, each item's text/desc/hits, the
/// active index, the anchor, px, and the colors. Allocation-free (folds borrowed strs +
/// bounded item counts).
fn signature(
    completion: &Completion,
    w: u32,
    input_zone_top: f32,
    px_key: u32,
    theme: &Theme,
) -> u64 {
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

    let mut s: u64 = 0xcbf2_9ce4_8422_2325;
    s = fold_u64(s, completion.is_open() as u64);
    s = fold_u64(s, completion.index() as u64);
    s = fold_u64(s, completion.items().len() as u64);
    for it in completion.items() {
        s = fold_str(s, &it.text);
        s = fold_str(s, &it.desc);
        s = fold_u64(s, it.hits.len() as u64);
        for &h in &it.hits {
            s = fold_u64(s, h as u64);
        }
    }
    s = fold_u64(s, u64::from(w));
    s = fold_u64(s, input_zone_top.to_bits() as u64);
    s = fold_u64(s, u64::from(px_key));
    let c = &theme.colors;
    for color in [
        c.bg_canvas,
        c.bg_elev,
        c.fg_primary,
        c.fg_secondary,
        c.fg_muted,
        c.accent_primary,
        c.accent_primary_weak,
        c.hairline,
    ] {
        s = fold_color(s, color);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_core::{rank, Completion};
    use aterm_tokens::ThemeKind;

    fn open_completion() -> Completion {
        let cands = [
            ("git status", "working tree"),
            ("git commit -m", "record"),
            ("cargo build --release", "build"),
        ];
        let mut c = Completion::new();
        c.open_with(rank("g", &cands, 6));
        c
    }

    #[test]
    fn pointer_and_sep_glyphs_exist_in_the_bundled_grid_font() {
        // The popover draws the `>` pointer + the U+00B7 separator through the Mono GRID
        // face; a `.notdef` would be a box. Pure font parse (every platform).
        use crate::text::FaceStyle;
        let r = crate::glyph::GlyphRasterizer::new();
        for glyph in [POINTER, SEP] {
            let gid = r.glyph_id(FontFamily::Grid, FaceStyle::Regular, glyph);
            assert_ne!(
                gid, 0,
                "popover glyph U+{:04X} is .notdef in the bundled Mono Nerd Font",
                glyph as u32
            );
        }
    }

    #[test]
    fn signature_changes_on_open_index_and_items() {
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let mut c = open_completion();
        let base = signature(&c, 960, 500.0, 13, &theme);
        assert_eq!(base, signature(&c, 960, 500.0, 13, &theme), "deterministic");
        // Moving the active row changes the drawn state (pointer + tint move).
        c.move_down();
        assert_ne!(base, signature(&c, 960, 500.0, 13, &theme), "index");
        // Closing changes it (draws nothing).
        let mut closed = open_completion();
        closed.close();
        assert_ne!(
            base,
            signature(&closed, 960, 500.0, 13, &theme),
            "open/close"
        );
        // The anchor (input zone top) moves the panel.
        assert_ne!(
            base,
            signature(&open_completion(), 960, 480.0, 13, &theme),
            "anchor"
        );
        // A theme switch.
        assert_ne!(
            base,
            signature(
                &open_completion(),
                960,
                500.0,
                13,
                Theme::for_kind(ThemeKind::Light)
            ),
            "theme"
        );
    }
}

// The popover draws to a real GPU through the shared atlas, so it is verified offscreen and
// read back - macOS-only, skipping when no adapter is present (the shared front-end harness).
#[cfg(all(test, target_os = "macos"))]
mod gpu_tests {
    use super::*;
    use aterm_core::{rank, Completion};
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
            label: Some("aterm-completion-test"),
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

    fn cmpl() -> Completion {
        let cands = [
            ("git status", "working tree"),
            ("git commit -m", "record"),
            ("cargo test", "test"),
        ];
        let mut c = Completion::new();
        c.open_with(rank("g", &cands, 6));
        c
    }

    #[allow(clippy::too_many_arguments)]
    fn render(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        cr: &mut CompletionRenderer,
        completion: &Completion,
        theme: &Theme,
        w: u32,
        h: u32,
    ) -> Readback {
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cr-target"),
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
            label: Some("cr-readback"),
            size: (stride as u32 * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        // Input zone top near the bottom, so the panel grows upward into the surface.
        cr.prepare(
            device,
            queue,
            atlas,
            completion,
            h as f32 - 40.0,
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
                label: Some("cr-pass"),
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
            cr.draw(&mut pass, atlas);
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
    fn popover_inks_in_both_themes() {
        let Some((device, queue, format)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let (w, h) = (420u32, 260u32);
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let theme = *Theme::for_kind(kind);
            let mut atlas = GlyphAtlas::new(&device, format);
            let mut cr = CompletionRenderer::new(&device);
            let c = cmpl();
            let rb = render(&device, &queue, &mut atlas, &mut cr, &c, &theme, w, h);
            // The panel + text ink in the band above the input zone (which starts at h-40).
            assert!(
                rb.any_ink(8, h - 160, 200, h - 40, 20),
                "{kind:?}: the completion popover inks above the input"
            );
        }
    }

    #[test]
    fn closed_completion_draws_nothing() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut cr = CompletionRenderer::new(&device);
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let mut c = cmpl();
        c.close();
        let drew = cr.prepare(
            &device,
            &queue,
            &mut atlas,
            &c,
            220.0,
            &theme,
            FrameSize {
                width: 420,
                height: 260,
                scale: SCALE,
            },
        );
        assert!(!drew, "a closed completion draws nothing");
    }

    #[test]
    fn popover_glyph_layer_is_a_single_draw_call() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut cr = CompletionRenderer::new(&device);
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let c = cmpl();
        render(&device, &queue, &mut atlas, &mut cr, &c, &theme, 420, 260);
        assert_eq!(
            cr.last_glyph_draw_calls(),
            1,
            "the popover is ONE glyph draw"
        );
    }

    #[test]
    fn unchanged_popover_skips_rebuild_alloc_free() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut cr = CompletionRenderer::new(&device);
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let c = cmpl();
        let size = FrameSize {
            width: 420,
            height: 260,
            scale: SCALE,
        };
        cr.prepare(&device, &queue, &mut atlas, &c, 220.0, &theme, size);
        let allocs = crate::alloc_probe::count_allocs(|| {
            let drew = cr.prepare(&device, &queue, &mut atlas, &c, 220.0, &theme, size);
            std::hint::black_box(drew);
        });
        assert_eq!(
            allocs, 0,
            "an unchanged popover frame's prepare early-out allocates nothing (got {allocs})"
        );
    }
}
