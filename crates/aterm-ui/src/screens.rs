//! The quiet informational screens (ticket T-9.5): the `launch` empty state and the `modes`
//! one-input-two-destinations explainer, drawn to the vision mock (ADR-0011)
//! `<!-- launch -->` and `<!-- modes -->` states.
//!
//! Another front-end over the shared [`GlyphAtlas`], like the title bar. The host
//! ([`crate::gpu`]) knows the content region between the title bar and the input box, so it
//! passes that band in and this centers its content within it. Which screen (if any) to draw
//! is the host's decision: `launch` when the timeline is empty, `modes` on demand
//! (`Frame::show_help`). Prose is shaped through the shared [`ProseShaper`]; the `❯`/`◊` mode
//! glyphs go through the Mono grid emitter (where they are coverage-tested), so the whole
//! screen is one rect draw + one glyph draw.
//!
//! ## Damage gating
//! Like every other front-end, [`Self::prepare`] keys a rebuild on a cheap FNV signature over
//! everything drawn (the screen kind, the routing mode, the content geometry, px, the colors)
//! and early-outs alloc-free when unchanged - the T-1.8 60fps floor.

use std::mem::size_of;

use aterm_tokens::{type_scale, Mode, Rgba, Theme};

use crate::atlas::{GlyphAtlas, GlyphInstance, InstanceBuffer, RectInstance};
use crate::cell_render::{emit_cell, CellCtx};
use crate::grid_render::FrameSize;
use crate::prose::{ProseLayout, ProseShaper};
use crate::text::{FaceStyle, FontFamily, GlyphKey, GridCell};
use crate::window::cell_px;

/// The shell prompt glyph (`❯`, U+276F) drawn in the modes explainer's Shell column - the
/// same coverage-tested Mono glyph the timeline gutter + input box use.
const SHELL_GLYPH: char = '\u{276F}';
/// The agent prompt glyph (`◊`, U+25CA) drawn in the modes explainer's Agent column - the
/// mock's `◇` (U+25C7) is `.notdef` in the bundled Mono face, so the present lozenge stands
/// in (identical substitution to the input box's `AGENT_GLYPH`, coverage-tested here too).
const AGENT_GLYPH: char = '\u{25CA}';

/// Which informational screen to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenKind {
    /// A fresh, historyless window: a centered "aterm" + tagline + "no history yet".
    Launch,
    /// The one-input-two-destinations explainer (the shell/agent split).
    Modes,
}

// The launch splash strings (ASCII only, so every glyph resolves in the Prose/Ui faces; the
// mock's `⌘I` is written "Cmd-I" here - the `⌘` PUA icon is not guaranteed in the Duo face).
const LAUNCH_TITLE: &str = "aterm";
const LAUNCH_TAGLINE_1: &str = "A quiet place to run things.";
const LAUNCH_TAGLINE_2: &str = "Type a command below, or press Cmd-I to ask the agent instead.";
const LAUNCH_FOOTER: &str = "no history yet";

// The modes explainer strings.
const MODES_EYEBROW: &str = "ONE INPUT, TWO DESTINATIONS";
const MODES_PARAGRAPH: &str = "The box at the bottom drives the shell or the agent. The mode chip decides where Enter goes - press Cmd-I (or tap the chip) to flip it. Whatever you've typed stays exactly where it is.";
const MODES_SHELL_LABEL: &str = "Shell";
const MODES_SHELL_DESC: &str =
    "Enter hands the line to the background shell and appends a command block to the timeline.";
const MODES_AGENT_LABEL: &str = "Agent";
const MODES_AGENT_DESC: &str =
    "Enter starts a client-side agentic loop that plans, calls tools, and reports back as blocks.";

/// The informational-screens front-end over the shared [`GlyphAtlas`].
pub struct ScreensRenderer {
    bg_instances: Vec<RectInstance>,
    glyph_instances: Vec<GlyphInstance>,
    bg_buf: InstanceBuffer,
    glyph_buf: InstanceBuffer,
    shaper: ProseShaper,
    built: Option<u64>,
    last_glyph_draw_calls: u32,
}

impl ScreensRenderer {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            bg_instances: Vec::new(),
            glyph_instances: Vec::new(),
            bg_buf: InstanceBuffer::new(device, "aterm-screens-bg", size_of::<RectInstance>(), 16),
            glyph_buf: InstanceBuffer::new(
                device,
                "aterm-screens-glyph",
                size_of::<GlyphInstance>(),
                256,
            ),
            shaper: ProseShaper::new(),
            built: None,
            last_glyph_draw_calls: 0,
        }
    }

    #[must_use]
    pub fn last_glyph_draw_calls(&self) -> u32 {
        self.last_glyph_draw_calls
    }

    /// Build the frame's instances for `kind`, centered in the content band that runs from
    /// `content_top` for `content_h` physical px. `mode` is the current routing mode (for the
    /// modes screen's "Currently routing to <mode>" line). Damage-gated + alloc-free on an
    /// unchanged frame.
    #[allow(clippy::too_many_arguments)] // by-value frame-path args, like the other front-ends
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        kind: ScreenKind,
        mode: Mode,
        content_top: f32,
        content_h: f32,
        theme: &Theme,
        size: FrameSize,
    ) -> bool {
        let FrameSize {
            width,
            height: _,
            scale,
            content_left,
        } = size;
        let px = (type_scale::GRID.size_pt * scale).round().max(1.0);
        let px_key = px as u32;

        let sig = signature(
            kind,
            mode,
            width,
            content_left,
            content_top,
            content_h,
            px_key,
            theme,
        );
        if self.built == Some(sig) {
            return !self.glyph_instances.is_empty() || !self.bg_instances.is_empty();
        }

        self.bg_instances.clear();
        self.glyph_instances.clear();

        let (cw, ch) = cell_px(scale);
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

        match kind {
            ScreenKind::Launch => self.build_launch(
                queue,
                atlas,
                content_top,
                content_h,
                width,
                content_left,
                scale,
                theme,
            ),
            ScreenKind::Modes => self.build_modes(
                queue,
                atlas,
                &ctx,
                mode,
                content_top,
                content_h,
                width,
                content_left,
                scale,
                theme,
            ),
        }

        if !self.bg_instances.is_empty() {
            self.bg_buf.ensure(
                device,
                "aterm-screens-bg",
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
                "aterm-screens-glyph",
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

    /// The centered launch splash: "aterm" + a two-line tagline + "no history yet".
    #[allow(clippy::too_many_arguments)]
    fn build_launch(
        &mut self,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        content_top: f32,
        content_h: f32,
        width: u32,
        content_left: f32,
        scale: f32,
        theme: &Theme,
    ) {
        let c = &theme.colors;
        let title_px = (type_scale::HEADING.size_pt * 1.4 * scale).round().max(1.0);
        let body_px = (type_scale::BODY.size_pt * scale).round().max(1.0);
        let cap_px = (type_scale::CAPTION.size_pt * scale).round().max(1.0);
        let gap = 18.0 * scale;

        // Shape each run (all centered horizontally); Prose/Duo for the title + taglines,
        // Ui/Quattro for the faint footer.
        let title = self.shape(LAUNCH_TITLE, FontFamily::Prose, title_px);
        let t1 = self.shape(LAUNCH_TAGLINE_1, FontFamily::Prose, body_px);
        let t2 = self.shape(LAUNCH_TAGLINE_2, FontFamily::Prose, body_px);
        let footer = self.shape(LAUNCH_FOOTER, FontFamily::Ui, cap_px);

        let total_h = title.height + gap + t1.height + t2.height + gap + footer.height;
        let mut y = content_top + ((content_h - total_h) * 0.5).max(0.0);
        let content_width = (width as f32 - content_left).max(0.0);
        let center_x = |w: f32| content_left + (content_width - w) * 0.5;

        self.place(queue, atlas, &title, center_x(title.width), y, c.fg_primary);
        y += title.height + gap;
        self.place(queue, atlas, &t1, center_x(t1.width), y, c.fg_secondary);
        y += t1.height;
        self.place(queue, atlas, &t2, center_x(t2.width), y, c.fg_secondary);
        y += t2.height + gap;
        self.place(queue, atlas, &footer, center_x(footer.width), y, c.fg_muted);
    }

    /// The modes explainer: an eyebrow, a paragraph, the two-column shell/agent split with a
    /// vertical hairline divider, and a "Currently routing to <mode>" line.
    #[allow(clippy::too_many_arguments)]
    fn build_modes(
        &mut self,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        ctx: &CellCtx,
        mode: Mode,
        content_top: f32,
        content_h: f32,
        width: u32,
        content_left: f32,
        scale: f32,
        theme: &Theme,
    ) {
        let c = &theme.colors;
        let cap_px = (type_scale::CAPTION.size_pt * scale).round().max(1.0);
        let body_px = (type_scale::BODY.size_pt * scale).round().max(1.0);
        let margin = f32::from(aterm_tokens::space::S8) * scale;
        let edge = content_left + margin;
        let gap = 22.0 * scale;
        let content_w = (width as f32 - content_left - 2.0 * margin).max(1.0);
        let measure = content_w.min(520.0 * scale); // the mock's ~520px paragraph measure

        let eyebrow = self.shape(MODES_EYEBROW, FontFamily::Ui, cap_px);
        let para = self.shape_wrapped(MODES_PARAGRAPH, FontFamily::Prose, body_px, measure);

        // Two columns, each: a `❯`/`◊` glyph + label line, then a wrapped description.
        let col_gap = f32::from(aterm_tokens::space::S8) * scale;
        let col_w = ((content_w - col_gap) * 0.5).max(1.0);
        let shell_label = self.shape(MODES_SHELL_LABEL, FontFamily::Ui, body_px);
        let agent_label = self.shape(MODES_AGENT_LABEL, FontFamily::Ui, body_px);
        let shell_desc = self.shape_wrapped(MODES_SHELL_DESC, FontFamily::Prose, cap_px, col_w);
        let agent_desc = self.shape_wrapped(MODES_AGENT_DESC, FontFamily::Prose, cap_px, col_w);
        let label_row_h = ctx.ch.max(shell_label.height).max(agent_label.height);
        let col_h = label_row_h + 10.0 * scale + shell_desc.height.max(agent_desc.height);

        let routing = self.shape("Currently routing to", FontFamily::Ui, cap_px);
        let mode_word = self.shape(
            match mode {
                Mode::Shell => "Shell",
                Mode::Agent => "Agent",
            },
            FontFamily::Ui,
            cap_px,
        );

        let total_h = eyebrow.height
            + gap
            + para.height
            + gap
            + col_h
            + gap
            + routing.height.max(mode_word.height);
        let mut y = content_top + ((content_h - total_h) * 0.5).max(0.0);
        let x0 = edge;

        self.place(queue, atlas, &eyebrow, x0, y, c.fg_muted);
        y += eyebrow.height + gap;
        self.place(queue, atlas, &para, x0, y, c.fg_secondary);
        y += para.height + gap;

        // Column row. Left = Shell (accent_primary glyph), right = Agent (accent_agent glyph).
        let left_x = x0;
        let right_x = x0 + col_w + col_gap;
        // The `❯`/`◊` glyphs (one Mono cell each), then the label just after.
        let shell_glyph = grid_glyph(SHELL_GLYPH, c.accent_primary, theme.colors.bg_canvas);
        emit_cell(
            atlas,
            queue,
            &shell_glyph,
            (left_x, y),
            ctx,
            &mut self.bg_instances,
            &mut self.glyph_instances,
        );
        let agent_glyph = grid_glyph(AGENT_GLYPH, c.accent_agent, theme.colors.bg_canvas);
        emit_cell(
            atlas,
            queue,
            &agent_glyph,
            (right_x, y),
            ctx,
            &mut self.bg_instances,
            &mut self.glyph_instances,
        );
        let label_x_off = ctx.cw * 2.0; // one glyph cell + a gap
        let label_y = y + (ctx.ch - shell_label.height) * 0.5;
        self.place(
            queue,
            atlas,
            &shell_label,
            left_x + label_x_off,
            label_y,
            c.fg_secondary,
        );
        self.place(
            queue,
            atlas,
            &agent_label,
            right_x + label_x_off,
            label_y,
            c.fg_secondary,
        );
        let desc_y = y + label_row_h + 10.0 * scale;
        self.place(queue, atlas, &shell_desc, left_x, desc_y, c.fg_muted);
        self.place(queue, atlas, &agent_desc, right_x, desc_y, c.fg_muted);
        // The vertical hairline divider between the two columns.
        let hairline_h = (f32::from(aterm_tokens::space::HAIRLINE_WIDTH) * scale)
            .round()
            .max(1.0);
        self.bg_instances.push(RectInstance {
            rect: [
                (left_x + col_w + col_gap * 0.5).round(),
                y,
                hairline_h,
                col_h,
            ],
            color: c.hairline.to_linear_f32(),
        });
        y += col_h + gap;

        // "Currently routing to <mode>" (the mode word in its accent).
        self.place(queue, atlas, &routing, x0, y, c.fg_muted);
        let accent = c.mode_accent(mode);
        self.place(
            queue,
            atlas,
            &mode_word,
            x0 + routing.width + ctx.cw,
            y,
            accent,
        );
    }

    /// Shape one unwrapped run at `px` in `family` (Regular). Convenience over
    /// [`ProseShaper::layout`] with an effectively-infinite measure.
    fn shape(&mut self, text: &str, family: FontFamily, px: f32) -> ProseLayout {
        let line_h = px * line_height_for(family);
        self.shaper
            .layout(text, family, FaceStyle::Regular, px, f32::MAX, line_h)
    }

    /// Shape one run at `px` in `family`, wrapping at `measure` px.
    fn shape_wrapped(
        &mut self,
        text: &str,
        family: FontFamily,
        px: f32,
        measure: f32,
    ) -> ProseLayout {
        let line_h = px * line_height_for(family);
        self.shaper
            .layout(text, family, FaceStyle::Regular, px, measure, line_h)
    }

    /// Place a shaped run's glyphs into the shared glyph buffer at `(x0, y0)` in `color`.
    fn place(
        &mut self,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        layout: &ProseLayout,
        x0: f32,
        y0: f32,
        color: Rgba,
    ) {
        let inv = 1.0 / atlas.atlas_dim() as f32;
        let color = color.to_linear_f32();
        let px_key = layout.px as u32;
        for pg in &layout.glyphs {
            let key = GlyphKey {
                family: layout.family,
                glyph_id: pg.glyph_id,
                face: layout.face,
                px: px_key,
                sprite: false,
            };
            let Some((rect, (left, top))) = atlas.acquire_font(
                queue,
                key,
                layout.family,
                layout.face,
                pg.glyph_id,
                layout.px,
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

    /// Record the screen draws into `pass`: the solid layer (the modes divider) then the one
    /// glyph instanced draw.
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

/// The line-height multiplier for a family's body register (Prose = body, Ui = label). Used
/// to space soft-wrapped lines.
fn line_height_for(family: FontFamily) -> f32 {
    match family {
        FontFamily::Prose => type_scale::BODY.line_height,
        FontFamily::Ui => type_scale::LABEL.line_height,
        FontFamily::Grid => type_scale::GRID.line_height,
    }
}

/// A single Mono cell carrying `ch` in `fg` on the canvas background.
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

/// A stable u64 over everything a screen draws. Allocation-free (folds the fixed strings only
/// by their identity via the kind + mode; the strings themselves are constants, so folding the
/// kind/mode/geometry/px/colors is sufficient to catch every draw-affecting change).
#[allow(clippy::too_many_arguments)]
fn signature(
    kind: ScreenKind,
    mode: Mode,
    w: u32,
    content_left: f32,
    content_top: f32,
    content_h: f32,
    px_key: u32,
    theme: &Theme,
) -> u64 {
    fn fold_u64(h: u64, v: u64) -> u64 {
        (h ^ v).wrapping_mul(0x0000_0100_0000_01b3)
    }
    fn fold_color(h: u64, c: Rgba) -> u64 {
        fold_u64(h, u64::from(c.to_u32()))
    }

    let mut s: u64 = 0xcbf2_9ce4_8422_2325;
    s = fold_u64(s, matches!(kind, ScreenKind::Modes) as u64);
    s = fold_u64(s, matches!(mode, Mode::Agent) as u64);
    s = fold_u64(s, u64::from(w));
    s = fold_u64(s, u64::from(content_left.to_bits()));
    s = fold_u64(s, content_top.to_bits() as u64);
    s = fold_u64(s, content_h.to_bits() as u64);
    s = fold_u64(s, u64::from(px_key));
    let c = &theme.colors;
    for color in [
        c.bg_canvas,
        c.fg_primary,
        c.fg_secondary,
        c.fg_muted,
        c.accent_primary,
        c.accent_agent,
        c.hairline,
    ] {
        s = fold_color(s, color);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_tokens::ThemeKind;

    #[test]
    fn screen_glyphs_exist_in_the_bundled_grid_font() {
        // The modes columns draw `❯`/`◊` through the Mono GRID face; a `.notdef` would be a
        // box. Pure font parse (every platform).
        let r = crate::glyph::GlyphRasterizer::new();
        for glyph in [SHELL_GLYPH, AGENT_GLYPH] {
            let gid = r.glyph_id(FontFamily::Grid, FaceStyle::Regular, glyph);
            assert_ne!(
                gid, 0,
                "modes glyph U+{:04X} is .notdef in the bundled Mono Nerd Font",
                glyph as u32
            );
        }
    }

    #[test]
    fn signature_is_stable_and_changes_on_each_drawn_axis() {
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let base = signature(
            ScreenKind::Launch,
            Mode::Shell,
            960,
            0.0,
            44.0,
            500.0,
            13,
            &theme,
        );
        assert_eq!(
            base,
            signature(
                ScreenKind::Launch,
                Mode::Shell,
                960,
                0.0,
                44.0,
                500.0,
                13,
                &theme
            ),
            "deterministic"
        );
        assert_ne!(
            base,
            signature(
                ScreenKind::Modes,
                Mode::Shell,
                960,
                0.0,
                44.0,
                500.0,
                13,
                &theme,
            ),
            "kind"
        );
        // The routing mode only affects the modes screen, but folding it always is safe.
        assert_ne!(
            base,
            signature(
                ScreenKind::Launch,
                Mode::Agent,
                960,
                0.0,
                44.0,
                500.0,
                13,
                &theme
            ),
            "mode"
        );
        assert_ne!(
            base,
            signature(
                ScreenKind::Launch,
                Mode::Shell,
                961,
                0.0,
                44.0,
                500.0,
                13,
                &theme
            ),
            "width"
        );
        assert_ne!(
            base,
            signature(
                ScreenKind::Launch,
                Mode::Shell,
                960,
                0.0,
                48.0,
                500.0,
                13,
                &theme
            ),
            "content_top"
        );
        assert_ne!(
            base,
            signature(
                ScreenKind::Launch,
                Mode::Shell,
                960,
                0.0,
                44.0,
                480.0,
                13,
                &theme
            ),
            "content_h"
        );
        assert_ne!(
            base,
            signature(
                ScreenKind::Launch,
                Mode::Shell,
                960,
                0.0,
                44.0,
                500.0,
                26,
                &theme
            ),
            "px"
        );
        assert_ne!(
            base,
            signature(
                ScreenKind::Launch,
                Mode::Shell,
                960,
                0.0,
                44.0,
                500.0,
                13,
                Theme::for_kind(ThemeKind::Light)
            ),
            "theme"
        );
    }
}

// The screens draw to a real GPU through the shared atlas, so they are verified offscreen and
// read back - macOS-only, skipping when no adapter is present (the shared front-end harness).
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
            label: Some("aterm-screens-test"),
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

    #[allow(clippy::too_many_arguments)]
    fn render(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        sc: &mut ScreensRenderer,
        kind: ScreenKind,
        mode: Mode,
        theme: &Theme,
        w: u32,
        h: u32,
    ) -> Readback {
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sc-target"),
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
            label: Some("sc-readback"),
            size: (stride as u32 * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        // Content band = the whole surface minus a nominal title bar; enough to center in.
        sc.prepare(
            device,
            queue,
            atlas,
            kind,
            mode,
            0.0,
            h as f32,
            theme,
            FrameSize {
                width: w,
                height: h,
                scale: SCALE,
                content_left: 0.0,
            },
        );
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sc-pass"),
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
            sc.draw(&mut pass, atlas);
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
    fn launch_and_modes_ink_in_both_themes() {
        let Some((device, queue, format)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let (w, h) = (620u32, 460u32);
        for kind in [ScreenKind::Launch, ScreenKind::Modes] {
            for theme_kind in [ThemeKind::Dark, ThemeKind::Light] {
                let theme = *Theme::for_kind(theme_kind);
                let mut atlas = GlyphAtlas::new(&device, format);
                let mut sc = ScreensRenderer::new(&device);
                let rb = render(
                    &device,
                    &queue,
                    &mut atlas,
                    &mut sc,
                    kind,
                    Mode::Shell,
                    &theme,
                    w,
                    h,
                );
                // Something inks in the middle band of the content region.
                assert!(
                    rb.any_ink(0, h / 3, w, 2 * h / 3, 25),
                    "{kind:?}/{theme_kind:?}: the screen inks in the content region"
                );
                if kind == ScreenKind::Modes {
                    // The two-column split: ink in BOTH the left half and the right half.
                    assert!(
                        rb.any_ink(0, h / 3, w / 2, 2 * h / 3, 25),
                        "{theme_kind:?}: the Shell column inks (left half)"
                    );
                    assert!(
                        rb.any_ink(w / 2, h / 3, w, 2 * h / 3, 25),
                        "{theme_kind:?}: the Agent column inks (right half)"
                    );
                }
            }
        }
    }

    #[test]
    fn screens_glyph_layer_is_a_single_draw_call() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut sc = ScreensRenderer::new(&device);
        let theme = *Theme::for_kind(ThemeKind::Dark);
        render(
            &device,
            &queue,
            &mut atlas,
            &mut sc,
            ScreenKind::Modes,
            Mode::Agent,
            &theme,
            620,
            460,
        );
        assert_eq!(
            sc.last_glyph_draw_calls(),
            1,
            "the screen is ONE glyph draw"
        );
    }

    #[test]
    fn unchanged_screen_skips_rebuild_alloc_free() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut sc = ScreensRenderer::new(&device);
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let size = FrameSize {
            width: 620,
            height: 460,
            scale: SCALE,
            content_left: 0.0,
        };
        sc.prepare(
            &device,
            &queue,
            &mut atlas,
            ScreenKind::Launch,
            Mode::Shell,
            0.0,
            460.0,
            &theme,
            size,
        );
        let allocs = crate::alloc_probe::count_allocs(|| {
            let drew = sc.prepare(
                &device,
                &queue,
                &mut atlas,
                ScreenKind::Launch,
                Mode::Shell,
                0.0,
                460.0,
                &theme,
                size,
            );
            std::hint::black_box(drew);
        });
        assert_eq!(
            allocs, 0,
            "an unchanged screen frame's prepare early-out allocates nothing (got {allocs})"
        );
    }

    #[test]
    fn modes_screen_respects_the_sidebar_content_inset() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut screens = ScreensRenderer::new(&device);
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let content_left = 210.0;
        screens.prepare(
            &device,
            &queue,
            &mut atlas,
            ScreenKind::Modes,
            Mode::Shell,
            28.0,
            360.0,
            &theme,
            FrameSize {
                width: 720,
                height: 420,
                scale: SCALE,
                content_left,
            },
        );
        let first_glyph_x = screens
            .glyph_instances
            .iter()
            .map(|glyph| glyph.rect[0])
            .reduce(f32::min)
            .expect("the modes screen emitted glyphs");
        assert!(
            first_glyph_x >= content_left,
            "screen geometry begins after the sidebar, got x={first_glyph_x} for left={content_left}"
        );
    }
}
