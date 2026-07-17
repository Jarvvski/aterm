use std::mem::size_of;

use aterm_tokens::{space, type_scale, Rgba, Theme, ThemeKind};

use crate::atlas::{GlyphAtlas, GlyphInstance, InstanceBuffer, RectInstance};
use crate::components::AutonomyMode;
use crate::grid_render::FrameSize;
use crate::hit::{HitRect, HitTarget};
use crate::prose::{ProseLayout, ProseShaper};
use crate::text::{FaceStyle, FontFamily, GlyphKey};

/// Provider choice displayed by the Preferences surface. T-12.1 owns only the
/// presentation value; T-12.2 binds it to persisted application configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsProvider {
    Anthropic,
    OpenAi,
    Local,
}

/// One frame's borrowed-free Preferences state. The host supplies this compact value and
/// the settings module hides all layout, typography, controls, and damage tracking behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsView {
    pub provider: SettingsProvider,
    pub autonomy: AutonomyMode,
    pub font_size_px: u16,
}

const EDGE_LOGICAL: f32 = 22.0;
const HEADING_GAP_LOGICAL: f32 = 22.0;
const ROW_HEIGHT_LOGICAL: f32 = 64.0;
const ROW_LABEL_TOP_LOGICAL: f32 = 11.0;
const ROW_DESCRIPTION_TOP_LOGICAL: f32 = 34.0;
const SEGMENT_GAP_LOGICAL: f32 = 18.0;
const CONTROL_HIT_PAD_LOGICAL: f32 = 5.0;
const STEPPER_BUTTON_LOGICAL: f32 = 28.0;
const STEPPER_GAP_LOGICAL: f32 = 16.0;
const FOOTER_GAP_LOGICAL: f32 = 26.0;

const HEADING: &str = "PREFERENCES";
const FOOTER: &str = concat!(
    "aterm ",
    env!("CARGO_PKG_VERSION"),
    " - themes stay calm, config stays typographic"
);

#[derive(Debug, Clone, Copy)]
struct SegmentSpec {
    label: &'static str,
    selected: bool,
    target: HitTarget,
}

/// Damage-gated Preferences front-end. Callers supply one compact [`SettingsView`]; this
/// module owns the full row layout, shared control treatments, typography, hit geometry,
/// GPU buffers, and idle-frame cache behind that small interface.
pub struct SettingsRenderer {
    bg_instances: Vec<RectInstance>,
    glyph_instances: Vec<GlyphInstance>,
    bg_buf: InstanceBuffer,
    glyph_buf: InstanceBuffer,
    shaper: ProseShaper,
    hit_regions: Vec<(HitRect, HitTarget)>,
    built: Option<u64>,
    row_rule_count: usize,
    last_glyph_draw_calls: u32,
}

impl SettingsRenderer {
    #[must_use]
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            bg_instances: Vec::new(),
            glyph_instances: Vec::new(),
            bg_buf: InstanceBuffer::new(device, "aterm-settings-bg", size_of::<RectInstance>(), 32),
            glyph_buf: InstanceBuffer::new(
                device,
                "aterm-settings-glyph",
                size_of::<GlyphInstance>(),
                512,
            ),
            shaper: ProseShaper::new(),
            hit_regions: Vec::new(),
            built: None,
            row_rule_count: 0,
            last_glyph_draw_calls: 0,
        }
    }

    #[must_use]
    pub fn last_glyph_draw_calls(&self) -> u32 {
        self.last_glyph_draw_calls
    }

    pub(crate) fn hit_regions(&self) -> &[(HitRect, HitTarget)] {
        &self.hit_regions
    }

    #[cfg(test)]
    fn row_rule_count(&self) -> usize {
        self.row_rule_count
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        view: SettingsView,
        hovered: Option<HitTarget>,
        title_height: f32,
        theme: &Theme,
        size: FrameSize,
    ) -> bool {
        let sig = signature(view, hovered, title_height, theme, size);
        if self.built == Some(sig) {
            return !self.glyph_instances.is_empty() || !self.bg_instances.is_empty();
        }

        self.bg_instances.clear();
        self.glyph_instances.clear();
        self.hit_regions.clear();
        self.row_rule_count = 0;

        let scale = size.scale;
        let edge = EDGE_LOGICAL * scale;
        let x = size.content_left + edge;
        let width = (size.width as f32 - size.content_left - edge * 2.0).max(0.0);
        let mut y = title_height + edge;

        let heading = self.shape(HEADING, FontFamily::Ui, type_scale::LABEL.size_pt * scale);
        self.place(queue, atlas, &heading, x, y, theme.colors.fg_muted);
        y += heading.height + HEADING_GAP_LOGICAL * scale;

        self.push_rule(x, y, width, scale, theme.colors.hairline);
        self.build_row_text(
            queue,
            atlas,
            "Theme",
            "Warm paper, or warm near-black.",
            x,
            y,
            scale,
            theme,
        );
        let theme_segments = [
            SegmentSpec {
                label: "Dark",
                selected: theme.kind == ThemeKind::Dark,
                target: HitTarget::SettingsSegment { group: 0, index: 0 },
            },
            SegmentSpec {
                label: "Light",
                selected: theme.kind == ThemeKind::Light,
                target: HitTarget::SettingsSegment { group: 0, index: 1 },
            },
        ];
        self.build_segmented(
            queue,
            atlas,
            &theme_segments,
            x + width,
            y,
            scale,
            hovered,
            theme,
        );
        y += ROW_HEIGHT_LOGICAL * scale;

        self.push_rule(x, y, width, scale, theme.colors.hairline);
        self.build_row_text(
            queue,
            atlas,
            "Font size",
            "Applies across every block.",
            x,
            y,
            scale,
            theme,
        );
        self.build_stepper(
            queue,
            atlas,
            view.font_size_px,
            x + width,
            y,
            scale,
            hovered,
            theme,
        );
        y += ROW_HEIGHT_LOGICAL * scale;

        self.push_rule(x, y, width, scale, theme.colors.hairline);
        self.build_row_text(
            queue,
            atlas,
            "Default provider",
            "Model backing the agent loop.",
            x,
            y,
            scale,
            theme,
        );
        let provider_segments = [
            SegmentSpec {
                label: "Anthropic",
                selected: view.provider == SettingsProvider::Anthropic,
                target: HitTarget::SettingsSegment { group: 1, index: 0 },
            },
            SegmentSpec {
                label: "OpenAI",
                selected: view.provider == SettingsProvider::OpenAi,
                target: HitTarget::SettingsSegment { group: 1, index: 1 },
            },
            SegmentSpec {
                label: "Local",
                selected: view.provider == SettingsProvider::Local,
                target: HitTarget::SettingsSegment { group: 1, index: 2 },
            },
        ];
        self.build_segmented(
            queue,
            atlas,
            &provider_segments,
            x + width,
            y,
            scale,
            hovered,
            theme,
        );
        y += ROW_HEIGHT_LOGICAL * scale;

        self.push_rule(x, y, width, scale, theme.colors.hairline);
        self.build_row_text(
            queue,
            atlas,
            "Autonomy",
            "When the agent may act without asking.",
            x,
            y,
            scale,
            theme,
        );
        let autonomy_segments = [
            SegmentSpec {
                label: "Ask each time",
                selected: view.autonomy == AutonomyMode::AskAlways,
                target: HitTarget::SettingsSegment { group: 2, index: 0 },
            },
            SegmentSpec {
                label: "Auto-run safe",
                selected: view.autonomy == AutonomyMode::AutoSafe,
                target: HitTarget::SettingsSegment { group: 2, index: 1 },
            },
            SegmentSpec {
                label: "Full auto",
                selected: view.autonomy == AutonomyMode::AutoRunInSession,
                target: HitTarget::SettingsSegment { group: 2, index: 2 },
            },
        ];
        self.build_segmented(
            queue,
            atlas,
            &autonomy_segments,
            x + width,
            y,
            scale,
            hovered,
            theme,
        );
        y += ROW_HEIGHT_LOGICAL * scale;

        self.push_rule(x, y, width, scale, theme.colors.hairline);
        let footer = self.shape(FOOTER, FontFamily::Ui, type_scale::CAPTION.size_pt * scale);
        self.place(
            queue,
            atlas,
            &footer,
            x,
            y + FOOTER_GAP_LOGICAL * scale,
            theme.colors.fg_muted,
        );

        if !self.bg_instances.is_empty() {
            self.bg_buf.ensure(
                device,
                "aterm-settings-bg",
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
                "aterm-settings-glyph",
                size_of::<GlyphInstance>(),
                self.glyph_instances.len(),
            );
            queue.write_buffer(
                self.glyph_buf.buf(),
                0,
                bytemuck::cast_slice(&self.glyph_instances),
            );
        }
        atlas.set_viewport(queue, size.width, size.height);
        self.built = Some(sig);
        !self.glyph_instances.is_empty() || !self.bg_instances.is_empty()
    }

    #[allow(clippy::too_many_arguments)]
    fn build_row_text(
        &mut self,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        label: &str,
        description: &str,
        x: f32,
        row_y: f32,
        scale: f32,
        theme: &Theme,
    ) {
        let label = self.shape(label, FontFamily::Prose, type_scale::BODY.size_pt * scale);
        let description = self.shape(
            description,
            FontFamily::Ui,
            type_scale::LABEL.size_pt * scale,
        );
        self.place(
            queue,
            atlas,
            &label,
            x,
            row_y + ROW_LABEL_TOP_LOGICAL * scale,
            theme.colors.fg_primary,
        );
        self.place(
            queue,
            atlas,
            &description,
            x,
            row_y + ROW_DESCRIPTION_TOP_LOGICAL * scale,
            theme.colors.fg_muted,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn build_segmented(
        &mut self,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        segments: &[SegmentSpec],
        right: f32,
        row_y: f32,
        scale: f32,
        hovered: Option<HitTarget>,
        theme: &Theme,
    ) {
        let px = type_scale::GRID.size_pt * scale;
        let layouts: Vec<_> = segments
            .iter()
            .map(|segment| self.shape(segment.label, FontFamily::Ui, px))
            .collect();
        let gap = SEGMENT_GAP_LOGICAL * scale;
        let total_width = layouts.iter().map(|layout| layout.width).sum::<f32>()
            + gap * segments.len().saturating_sub(1) as f32;
        let mut x = right - total_width;
        for (segment, layout) in segments.iter().zip(layouts.iter()) {
            let y = row_y + (ROW_HEIGHT_LOGICAL * scale - layout.height) * 0.5;
            let color = if segment.selected {
                theme.colors.accent_primary
            } else if hovered == Some(segment.target) {
                theme.colors.fg_primary
            } else {
                theme.colors.fg_secondary
            };
            self.place(queue, atlas, layout, x, y, color);
            let pad = CONTROL_HIT_PAD_LOGICAL * scale;
            self.hit_regions.push((
                [
                    x - pad,
                    y - pad,
                    layout.width + pad * 2.0,
                    layout.height + pad * 2.0,
                ],
                segment.target,
            ));
            x += layout.width + gap;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_stepper(
        &mut self,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        value: u16,
        right: f32,
        row_y: f32,
        scale: f32,
        hovered: Option<HitTarget>,
        theme: &Theme,
    ) {
        let button = STEPPER_BUTTON_LOGICAL * scale;
        let gap = STEPPER_GAP_LOGICAL * scale;
        let value_text = format!("{value} px");
        let value_layout = self.shape(
            &value_text,
            FontFamily::Ui,
            type_scale::GRID.size_pt * scale,
        );
        let value_width = (44.0 * scale).max(value_layout.width);
        let total_width = button * 2.0 + gap * 2.0 + value_width;
        let decrement = HitTarget::SettingsStepper { increment: false };
        let increment = HitTarget::SettingsStepper { increment: true };
        let y = row_y + (ROW_HEIGHT_LOGICAL * scale - button) * 0.5;
        let left_x = right - total_width;
        let value_x = left_x + button + gap + (value_width - value_layout.width) * 0.5;
        let right_x = left_x + button + gap + value_width + gap;

        self.push_border(left_x, y, button, theme.colors.hairline, scale);
        self.push_border(right_x, y, button, theme.colors.hairline, scale);
        self.hit_regions
            .push(([left_x, y, button, button], decrement));
        self.hit_regions
            .push(([right_x, y, button, button], increment));

        let minus = self.shape("-", FontFamily::Ui, type_scale::GRID.size_pt * scale);
        let plus = self.shape("+", FontFamily::Ui, type_scale::GRID.size_pt * scale);
        let button_color = |target| {
            if hovered == Some(target) {
                theme.colors.fg_primary
            } else {
                theme.colors.fg_secondary
            }
        };
        self.place(
            queue,
            atlas,
            &minus,
            left_x + (button - minus.width) * 0.5,
            y + (button - minus.height) * 0.5,
            button_color(decrement),
        );
        self.place(
            queue,
            atlas,
            &value_layout,
            value_x,
            row_y + (ROW_HEIGHT_LOGICAL * scale - value_layout.height) * 0.5,
            theme.colors.fg_primary,
        );
        self.place(
            queue,
            atlas,
            &plus,
            right_x + (button - plus.width) * 0.5,
            y + (button - plus.height) * 0.5,
            button_color(increment),
        );
    }

    fn push_rule(&mut self, x: f32, y: f32, width: f32, scale: f32, color: Rgba) {
        let hairline = (f32::from(space::HAIRLINE_WIDTH) * scale).round().max(1.0);
        self.bg_instances.push(RectInstance {
            rect: [x.round(), y.round(), width.round(), hairline],
            color: color.to_linear_f32(),
        });
        self.row_rule_count += 1;
    }

    fn push_border(&mut self, x: f32, y: f32, size: f32, color: Rgba, scale: f32) {
        let line = (f32::from(space::HAIRLINE_WIDTH) * scale).round().max(1.0);
        let color = color.to_linear_f32();
        for rect in [
            [x, y, size, line],
            [x, y + size - line, size, line],
            [x, y, line, size],
            [x + size - line, y, line, size],
        ] {
            self.bg_instances.push(RectInstance { rect, color });
        }
    }

    fn shape(&mut self, text: &str, family: FontFamily, px: f32) -> ProseLayout {
        let line_height = match family {
            FontFamily::Prose => type_scale::BODY.line_height,
            FontFamily::Ui => type_scale::LABEL.line_height,
            FontFamily::Grid => type_scale::GRID.line_height,
        };
        self.shaper.layout(
            text,
            family,
            FaceStyle::Regular,
            px,
            f32::MAX,
            px * line_height,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn place(
        &mut self,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        layout: &ProseLayout,
        x: f32,
        y: f32,
        color: Rgba,
    ) {
        let inv = 1.0 / atlas.atlas_dim() as f32;
        let color = color.to_linear_f32();
        for glyph in &layout.glyphs {
            let key = GlyphKey {
                family: layout.family,
                glyph_id: glyph.glyph_id,
                face: layout.face,
                px: layout.px as u32,
                sprite: false,
            };
            let Some((rect, (left, top))) = atlas.acquire_font(
                queue,
                key,
                layout.family,
                layout.face,
                glyph.glyph_id,
                layout.px,
            ) else {
                continue;
            };
            self.glyph_instances.push(GlyphInstance {
                rect: [
                    (x + glyph.pen_x + left as f32).round(),
                    (y + glyph.baseline - top as f32).round(),
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

fn signature(
    view: SettingsView,
    hovered: Option<HitTarget>,
    title_height: f32,
    theme: &Theme,
    size: FrameSize,
) -> u64 {
    fn fold(hash: u64, value: u64) -> u64 {
        (hash ^ value).wrapping_mul(0x0000_0100_0000_01b3)
    }
    let provider = match view.provider {
        SettingsProvider::Anthropic => 0,
        SettingsProvider::OpenAi => 1,
        SettingsProvider::Local => 2,
    };
    let autonomy = match view.autonomy {
        AutonomyMode::AskAlways => 0,
        AutonomyMode::AutoSafe => 1,
        AutonomyMode::AutoRunInSession => 2,
    };
    let hover = match hovered {
        Some(HitTarget::SettingsSegment { group, index }) => {
            1 + u64::from(group) * 8 + u64::from(index)
        }
        Some(HitTarget::SettingsStepper { increment }) => 32 + u64::from(increment),
        _ => 0,
    };
    let mut hash = 0xcbf2_9ce4_8422_2325;
    for value in [
        provider,
        autonomy,
        u64::from(view.font_size_px),
        hover,
        u64::from(size.width),
        u64::from(size.height),
        u64::from(size.scale.to_bits()),
        u64::from(size.content_left.to_bits()),
        u64::from(title_height.to_bits()),
    ] {
        hash = fold(hash, value);
    }
    for color in [
        theme.colors.bg_canvas,
        theme.colors.fg_primary,
        theme.colors.fg_secondary,
        theme.colors.fg_muted,
        theme.colors.accent_primary,
        theme.colors.hairline,
    ] {
        hash = fold(hash, u64::from(color.to_u32()));
    }
    hash
}

#[cfg(test)]
mod pure_tests {
    use super::FOOTER;

    #[test]
    fn preferences_footer_uses_the_package_version() {
        assert_eq!(
            FOOTER,
            concat!(
                "aterm ",
                env!("CARGO_PKG_VERSION"),
                " - themes stay calm, config stays typographic"
            )
        );
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use aterm_tokens::{Theme, ThemeKind};

    use super::*;
    use crate::{FrameSize, GlyphAtlas};

    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("aterm-settings-test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .ok()
    }

    #[test]
    fn preferences_surface_prepares_four_ruled_rows_footer_and_all_controls() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let mut renderer = SettingsRenderer::new(&device);
        let size = FrameSize {
            width: 960,
            height: 620,
            scale: 1.0,
            content_left: 0.0,
        };

        assert!(renderer.prepare(
            &device,
            &queue,
            &mut atlas,
            SettingsView {
                provider: SettingsProvider::Anthropic,
                autonomy: AutonomyMode::AutoSafe,
                font_size_px: 14,
            },
            None,
            28.0,
            Theme::for_kind(ThemeKind::Dark),
            size,
        ));

        assert_eq!(renderer.hit_regions().len(), 10);
        assert_eq!(renderer.row_rule_count(), 5);
        assert!(!renderer.glyph_instances.is_empty());
    }

    #[test]
    fn unchanged_preferences_prepare_is_allocation_free() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let mut renderer = SettingsRenderer::new(&device);
        let view = SettingsView {
            provider: SettingsProvider::Anthropic,
            autonomy: AutonomyMode::AutoSafe,
            font_size_px: 14,
        };
        let size = FrameSize {
            width: 960,
            height: 620,
            scale: 1.0,
            content_left: 0.0,
        };
        let theme = Theme::for_kind(ThemeKind::Dark);
        renderer.prepare(&device, &queue, &mut atlas, view, None, 28.0, theme, size);

        let allocations = crate::alloc_probe::count_allocs(|| {
            std::hint::black_box(
                renderer.prepare(&device, &queue, &mut atlas, view, None, 28.0, theme, size),
            );
        });

        assert_eq!(allocations, 0);
    }

    #[test]
    fn active_segments_use_the_accent_and_inactive_hover_lifts_to_primary_in_both_themes() {
        let Some((device, queue)) = device() else {
            return;
        };
        let size = FrameSize {
            width: 960,
            height: 620,
            scale: 1.0,
            content_left: 0.0,
        };
        let view = SettingsView {
            provider: SettingsProvider::Anthropic,
            autonomy: AutonomyMode::AutoSafe,
            font_size_px: 14,
        };
        let hovered = HitTarget::SettingsSegment { group: 1, index: 1 };

        for kind in [ThemeKind::Light, ThemeKind::Dark] {
            let theme = Theme::for_kind(kind);
            let mut atlas = GlyphAtlas::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
            let mut renderer = SettingsRenderer::new(&device);
            renderer.prepare(&device, &queue, &mut atlas, view, None, 28.0, theme, size);
            let accent = theme.colors.accent_primary.to_linear_f32();
            let primary = theme.colors.fg_primary.to_linear_f32();
            let accent_before = renderer
                .glyph_instances
                .iter()
                .filter(|glyph| glyph.color == accent)
                .count();
            let primary_before = renderer
                .glyph_instances
                .iter()
                .filter(|glyph| glyph.color == primary)
                .count();
            assert!(accent_before > 0, "{kind:?} has accented active segments");

            renderer.prepare(
                &device,
                &queue,
                &mut atlas,
                view,
                Some(hovered),
                28.0,
                theme,
                size,
            );
            let accent_after = renderer
                .glyph_instances
                .iter()
                .filter(|glyph| glyph.color == accent)
                .count();
            let primary_after = renderer
                .glyph_instances
                .iter()
                .filter(|glyph| glyph.color == primary)
                .count();
            assert_eq!(accent_after, accent_before, "hover keeps active accents");
            assert!(
                primary_after > primary_before,
                "{kind:?} inactive hover lifts from dim to primary"
            );
        }
    }
}
