//! Sessions sidebar renderer (ticket T-10.2).
//!
//! The module's interface is a borrowed [`SidebarView`] plus one retained
//! [`SidebarRenderer`]. Layout, token selection, text shaping, GPU buffers, damage
//! gating, and hit geometry stay behind that seam so the app only projects its live
//! session list and the renderer does the rest.

use std::mem::size_of;

use aterm_tokens::{space, type_scale, Rgba, Theme};

use crate::atlas::{GlyphAtlas, GlyphInstance, InstanceBuffer, RectInstance};
use crate::grid_render::FrameSize;
use crate::hit::{HitRect, HitTarget};
use crate::prose::{ProseLayout, ProseShaper};
use crate::text::{FaceStyle, FontFamily, GlyphKey};
use crate::title_bar::title_bar_px;

/// Sidebar width from the vision mock, in logical px.
pub const SIDEBAR_LOGICAL_WIDTH: f32 = 210.0;

const PANEL_PADDING_X: f32 = 22.0;
const PANEL_PADDING_TOP: f32 = 20.0;
const HEADER_ROW_HEIGHT: f32 = 20.0;
const HEADER_BOTTOM_GAP: f32 = 8.0;
const ROW_HEIGHT: f32 = 28.0;
const STATUS_DOT_SIZE: f32 = 6.0;
const STATUS_NAME_GAP: f32 = 11.0;
const ACTIVE_BAR_WIDTH: f32 = 2.0;
const FOOTER_LINE_HEIGHT: f32 = 17.0;
const FOOTER_TOP_PADDING: f32 = 16.0;
const FOOTER_SHORTCUT_GAP: f32 = 7.0;

const HEADER_X: f32 = PANEL_PADDING_X;
#[cfg(test)]
const ACTIVE_BAR_X: f32 = 1.0;
#[cfg(test)]
const STATUS_DOT_X: f32 = PANEL_PADDING_X + STATUS_DOT_SIZE * 0.5;

const HEADER: &str = "SESSIONS";
const FOOTER_ROWS: [(&str, &str); 3] = [
    ("\u{f0633}T", "new session"),
    ("\u{f0633}I", "switch mode"),
    ("\u{f0633}L", "switch theme"),
];

fn rows_top(scale: f32) -> f32 {
    title_bar_px(scale) + (PANEL_PADDING_TOP + HEADER_ROW_HEIGHT + HEADER_BOTTOM_GAP) * scale
}

#[cfg(test)]
fn first_row_y() -> f32 {
    rows_top(1.0) + ROW_HEIGHT * 0.5
}

#[cfg(test)]
fn second_row_y() -> f32 {
    first_row_y() + ROW_HEIGHT
}

fn footer_y(height: u32, scale: f32) -> f32 {
    height as f32 - (FOOTER_TOP_PADDING + FOOTER_LINE_HEIGHT * FOOTER_ROWS.len() as f32) * scale
}

/// One retained, presentation-ready session row. The app updates this projection when
/// session identity, name, or activity changes; a frame only borrows the resulting slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarItem {
    pub name: String,
    pub running: bool,
}

impl SidebarItem {
    #[must_use]
    pub fn new(name: impl Into<String>, running: bool) -> Self {
        Self {
            name: name.into(),
            running,
        }
    }
}

/// Borrowed frame input for the sidebar. The active session is an index into `items`,
/// matching the stable display order owned by `SessionList`.
#[derive(Debug, Clone, Copy)]
pub struct SidebarView<'a> {
    pub items: &'a [SidebarItem],
    pub active: usize,
}

/// Retained GPU front-end for the sidebar.
pub struct SidebarRenderer {
    bg_instances: Vec<RectInstance>,
    glyph_instances: Vec<GlyphInstance>,
    bg_buf: InstanceBuffer,
    glyph_buf: InstanceBuffer,
    shaper: ProseShaper,
    built: Option<u64>,
    last_glyph_draw_calls: u32,
    hit_regions: Vec<(HitRect, HitTarget)>,
}

impl SidebarRenderer {
    #[must_use]
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            bg_instances: Vec::new(),
            glyph_instances: Vec::new(),
            bg_buf: InstanceBuffer::new(device, "aterm-sidebar-bg", size_of::<RectInstance>(), 32),
            glyph_buf: InstanceBuffer::new(
                device,
                "aterm-sidebar-glyph",
                size_of::<GlyphInstance>(),
                128,
            ),
            shaper: ProseShaper::new(),
            built: None,
            last_glyph_draw_calls: 0,
            hit_regions: Vec::new(),
        }
    }

    #[must_use]
    pub fn last_glyph_draw_calls(&self) -> u32 {
        self.last_glyph_draw_calls
    }

    /// Cached pointer regions in draw order. The containing row precedes its close
    /// control so the close intent wins where the two overlap.
    #[must_use]
    pub fn hit_regions(&self) -> &[(HitRect, HitTarget)] {
        &self.hit_regions
    }

    /// Build the panel below the native title-bar band. An unchanged view returns before
    /// touching retained vectors or GPU buffers.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        view: &SidebarView<'_>,
        hovered: Option<crate::hit::HitTarget>,
        theme: &Theme,
        size: FrameSize,
    ) -> bool {
        let sig = signature(view, hovered, theme, size);
        if self.built == Some(sig) {
            return !self.bg_instances.is_empty() || !self.glyph_instances.is_empty();
        }

        self.bg_instances.clear();
        self.glyph_instances.clear();
        self.hit_regions.clear();

        let scale = size.scale;
        let panel_top = title_bar_px(scale);
        let panel_width = (SIDEBAR_LOGICAL_WIDTH * scale).round();
        let panel_height = (size.height as f32 - panel_top).max(0.0);
        let colors = &theme.colors;
        self.bg_instances.push(RectInstance {
            rect: [0.0, panel_top, panel_width, panel_height],
            color: colors.bg_canvas.to_linear_f32(),
        });
        let hairline_width = (f32::from(space::HAIRLINE_WIDTH) * scale).round().max(1.0);
        self.bg_instances.push(RectInstance {
            rect: [
                panel_width - hairline_width,
                panel_top,
                hairline_width,
                panel_height,
            ],
            color: colors.hairline.to_linear_f32(),
        });

        let label_px = (type_scale::LABEL.size_pt * scale).round().max(1.0);
        let label_y = panel_top + PANEL_PADDING_TOP * scale;
        self.place_text(
            queue,
            atlas,
            HEADER,
            HEADER_X * scale,
            label_y,
            label_px,
            colors.fg_faint,
        );
        let hit_padding = 6.0 * scale;
        let add_x = panel_width - PANEL_PADDING_X * scale - label_px;
        self.hit_regions.push((
            [
                (add_x - hit_padding).max(0.0),
                (label_y - hit_padding).max(panel_top),
                label_px + hit_padding * 2.0,
                label_px * type_scale::LABEL.line_height + hit_padding * 2.0,
            ],
            HitTarget::SidebarAdd,
        ));
        self.place_text(
            queue,
            atlas,
            "+",
            panel_width - PANEL_PADDING_X * scale - label_px,
            label_y,
            label_px,
            if hovered == Some(HitTarget::SidebarAdd) {
                colors.fg_primary
            } else {
                colors.fg_faint
            },
        );

        let body_px = (type_scale::LABEL.size_pt * scale).round().max(1.0);
        let row_top = rows_top(scale);
        for (index, item) in view.items.iter().enumerate() {
            let y = row_top + index as f32 * ROW_HEIGHT * scale;
            self.hit_regions.push((
                [0.0, y, panel_width - hairline_width, ROW_HEIGHT * scale],
                HitTarget::SidebarSession(index),
            ));
            if index == view.active {
                self.bg_instances.push(RectInstance {
                    rect: [0.0, y, panel_width - hairline_width, ROW_HEIGHT * scale],
                    color: colors.accent_primary_weak.to_linear_f32(),
                });
                self.bg_instances.push(RectInstance {
                    rect: [
                        0.0,
                        y,
                        (ACTIVE_BAR_WIDTH * scale).round().max(1.0),
                        ROW_HEIGHT * scale,
                    ],
                    color: colors.accent_primary.to_linear_f32(),
                });
            }

            let dot_size = (STATUS_DOT_SIZE * scale).round().max(1.0);
            let dot_y = y + (ROW_HEIGHT * scale - dot_size) * 0.5;
            let dot_x = PANEL_PADDING_X * scale;
            let corner = scale.round().max(1.0).min(dot_size * 0.5);
            let dot_color = if item.running {
                colors.accent_primary
            } else {
                colors.fg_faint
            }
            .to_linear_f32();
            // Two overlapping quads form a tiny octagonal disc with empty corners. The
            // shared rect pipeline has no radius primitive, and at 6px this matches the
            // mock's circular status mark without adding another pipeline.
            self.bg_instances.push(RectInstance {
                rect: [dot_x + corner, dot_y, dot_size - 2.0 * corner, dot_size],
                color: dot_color,
            });
            self.bg_instances.push(RectInstance {
                rect: [dot_x, dot_y + corner, dot_size, dot_size - 2.0 * corner],
                color: dot_color,
            });

            let name_x = (PANEL_PADDING_X + STATUS_DOT_SIZE + STATUS_NAME_GAP) * scale;
            let text_y = y + (ROW_HEIGHT * scale - body_px * type_scale::LABEL.line_height) * 0.5;
            let close_px = body_px;
            let close_x = panel_width - PANEL_PADDING_X * scale - close_px;
            let name_right = close_x - STATUS_NAME_GAP * scale;
            let display_name = ellipsize_name(
                &mut self.shaper,
                &item.name,
                body_px,
                (name_right - name_x).max(0.0),
            );
            self.place_text(
                queue,
                atlas,
                &display_name,
                name_x,
                text_y,
                body_px,
                colors.fg_primary,
            );

            let row_hovered = matches!(
                hovered,
                Some(HitTarget::SidebarSession(hovered_index)
                    | HitTarget::SidebarClose(hovered_index)) if hovered_index == index
            );
            if row_hovered {
                self.place_text(
                    queue,
                    atlas,
                    "×",
                    close_x,
                    text_y,
                    close_px,
                    if hovered == Some(HitTarget::SidebarClose(index)) {
                        colors.fg_primary
                    } else {
                        colors.fg_faint
                    },
                );
                self.hit_regions.push((
                    [
                        close_x - hit_padding,
                        y,
                        close_px + hit_padding * 2.0,
                        ROW_HEIGHT * scale,
                    ],
                    HitTarget::SidebarClose(index),
                ));
            }
        }

        let footer_px = (type_scale::CAPTION.size_pt * scale).round().max(1.0);
        let footer_top = footer_y(size.height, scale);
        for (index, (shortcut, description)) in FOOTER_ROWS.iter().enumerate() {
            let x = PANEL_PADDING_X * scale;
            let y = footer_top + index as f32 * FOOTER_LINE_HEIGHT * scale;
            let shortcut_width = self.place_text_family(
                queue,
                atlas,
                shortcut,
                FontFamily::Grid,
                x,
                y,
                footer_px,
                colors.fg_faint,
            );
            self.place_text(
                queue,
                atlas,
                description,
                x + shortcut_width + FOOTER_SHORTCUT_GAP * scale,
                y,
                footer_px,
                colors.fg_faint,
            );
        }

        if !self.bg_instances.is_empty() {
            self.bg_buf.ensure(
                device,
                "aterm-sidebar-bg",
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
                "aterm-sidebar-glyph",
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
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn place_text(
        &mut self,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        text: &str,
        x: f32,
        y: f32,
        px: f32,
        color: Rgba,
    ) -> f32 {
        self.place_text_family(queue, atlas, text, FontFamily::Ui, x, y, px, color)
    }

    #[allow(clippy::too_many_arguments)]
    fn place_text_family(
        &mut self,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        text: &str,
        family: FontFamily,
        x: f32,
        y: f32,
        px: f32,
        color: Rgba,
    ) -> f32 {
        let layout = self.shaper.layout(
            text,
            family,
            FaceStyle::Regular,
            px,
            f32::MAX,
            px * type_scale::LABEL.line_height,
        );
        place_layout(
            &mut self.glyph_instances,
            queue,
            atlas,
            &layout,
            x,
            y,
            color,
        );
        layout.width
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

/// Shape names against the real Quattro face and replace the tail with one ellipsis when
/// it would enter the reserved close-control slot. Runs only on a changed sidebar build;
/// unchanged frames return before this work.
fn ellipsize_name(shaper: &mut ProseShaper, name: &str, px: f32, max_width: f32) -> String {
    let line_height = px * type_scale::LABEL.line_height;
    if shaper
        .layout(
            name,
            FontFamily::Ui,
            FaceStyle::Regular,
            px,
            f32::MAX,
            line_height,
        )
        .width
        <= max_width
    {
        return name.to_string();
    }

    let mut prefix = String::new();
    let mut candidate = String::new();
    for ch in name.chars() {
        prefix.push(ch);
        candidate.clear();
        candidate.push_str(&prefix);
        candidate.push('…');
        let width = shaper
            .layout(
                &candidate,
                FontFamily::Ui,
                FaceStyle::Regular,
                px,
                f32::MAX,
                line_height,
            )
            .width;
        if width > max_width {
            prefix.pop();
            break;
        }
    }
    prefix.push('…');
    prefix
}

#[allow(clippy::too_many_arguments)]
fn place_layout(
    instances: &mut Vec<GlyphInstance>,
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
        instances.push(GlyphInstance {
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

fn signature(
    view: &SidebarView<'_>,
    hovered: Option<crate::hit::HitTarget>,
    theme: &Theme,
    size: FrameSize,
) -> u64 {
    fn fold(h: u64, value: u64) -> u64 {
        (h ^ value).wrapping_mul(0x0000_0100_0000_01b3)
    }
    let mut sig = 0xcbf2_9ce4_8422_2325;
    sig = fold(sig, view.active as u64);
    sig = fold(sig, size.width.into());
    sig = fold(sig, size.height.into());
    sig = fold(sig, size.scale.to_bits().into());
    for item in view.items {
        sig = fold(sig, item.name.len() as u64);
        for ch in item.name.chars() {
            sig = fold(sig, ch as u64);
        }
        sig = fold(sig, u64::from(item.running));
    }
    let colors = &theme.colors;
    for color in [
        colors.bg_canvas,
        colors.fg_primary,
        colors.fg_faint,
        colors.accent_primary,
        colors.accent_primary_weak,
        colors.hairline,
    ] {
        sig = fold(sig, color.to_u32().into());
    }
    sig = fold(sig, hover_code(hovered));
    sig
}

fn hover_code(hovered: Option<HitTarget>) -> u64 {
    match hovered {
        None => 0,
        Some(HitTarget::SidebarAdd) => 1,
        Some(HitTarget::SidebarSession(index)) => 2 + index as u64 * 2,
        Some(HitTarget::SidebarClose(index)) => 3 + index as u64 * 2,
        Some(HitTarget::SidebarToggle) => 4,
        Some(HitTarget::ModeChip) => 5,
        Some(HitTarget::BlockMeta(index)) => 6 + index as u64,
        Some(HitTarget::CompletionRow(index)) => 7 + index as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_symbols_exist_in_the_bundled_ui_face() {
        let rasterizer = crate::glyph::GlyphRasterizer::new();
        for symbol in ['+', '…', '×'] {
            assert_ne!(
                rasterizer.glyph_id(FontFamily::Ui, FaceStyle::Regular, symbol),
                0,
                "sidebar symbol U+{:04X} is missing from the UI face",
                symbol as u32
            );
        }
        let command_key = '\u{f0633}';
        assert_ne!(
            rasterizer.glyph_id(FontFamily::Grid, FaceStyle::Regular, command_key),
            0,
            "the Nerd Font Command-key glyph is missing from the bundled grid face"
        );
    }
}

#[cfg(all(test, target_os = "macos"))]
mod gpu_tests {
    use super::*;
    use aterm_tokens::{Theme, ThemeKind};

    use crate::atlas::GlyphAtlas;
    use crate::grid_render::FrameSize;

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
            label: Some("aterm-sidebar-test"),
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
        fn rgba(&self, x: u32, y: u32) -> [u8; 4] {
            let offset = y as usize * self.stride + x as usize * 4;
            self.data[offset..offset + 4]
                .try_into()
                .expect("rgba pixel")
        }

        fn differs(&self, a: (u32, u32), b: (u32, u32), threshold: u8) -> bool {
            let a = self.rgba(a.0, a.1);
            let b = self.rgba(b.0, b.1);
            a[..3]
                .iter()
                .zip(&b[..3])
                .any(|(a, b)| a.abs_diff(*b) > threshold)
        }

        fn region_differs_from(
            &self,
            rect: (u32, u32, u32, u32),
            reference: (u32, u32),
            threshold: u8,
        ) -> bool {
            let reference = self.rgba(reference.0, reference.1);
            (rect.1..rect.3.min(self.h)).any(|y| {
                (rect.0..rect.2.min(self.w)).any(|x| {
                    self.rgba(x, y)[..3]
                        .iter()
                        .zip(&reference[..3])
                        .any(|(value, reference)| value.abs_diff(*reference) > threshold)
                })
            })
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        sidebar: &mut SidebarRenderer,
        view: &SidebarView<'_>,
        hovered: Option<crate::HitTarget>,
        theme: &Theme,
        width: u32,
        height: u32,
    ) -> Readback {
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sidebar-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let texture_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let stride = ((width * 4).div_ceil(256) * 256) as usize;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sidebar-readback"),
            size: (stride as u32 * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        sidebar.prepare(
            device,
            queue,
            atlas,
            view,
            hovered,
            theme,
            FrameSize {
                width,
                height,
                scale: SCALE,
                content_left: 0.0,
            },
        );

        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sidebar-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &texture_view,
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
            sidebar.draw(&mut pass, atlas);
        }
        encoder.copy_texture_to_buffer(
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
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(encoder.finish()));
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
        let data = slice.get_mapped_range().to_vec();
        Readback {
            data,
            stride,
            w: width,
            h: height,
        }
    }

    #[test]
    fn sidebar_renders_two_sessions_and_marks_activity_and_selection_in_both_themes() {
        let Some((device, queue, format)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let items = [
            SidebarItem::new("main", true),
            SidebarItem::new("a very long idle session name that must ellipsize", false),
        ];
        let view = SidebarView {
            items: &items,
            active: 0,
        };
        let (width, height) = (480, 360);

        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let theme = *Theme::for_kind(kind);
            let mut atlas = GlyphAtlas::new(&device, format);
            let mut sidebar = SidebarRenderer::new(&device);
            let frame = render(
                &device,
                &queue,
                &mut atlas,
                &mut sidebar,
                &view,
                None,
                &theme,
                width,
                height,
            );

            assert_eq!(frame.w, width);
            assert_eq!(frame.h, height);
            assert!(
                frame.region_differs_from((HEADER_X as u32, 45, 100, 70), (100, 35), 4,),
                "{kind:?}: the SESSIONS header inks above the rows"
            );
            assert!(
                frame.region_differs_from(
                    (
                        PANEL_PADDING_X as u32,
                        footer_y(height, SCALE) as u32,
                        150,
                        height,
                    ),
                    (100, 35),
                    4,
                ),
                "{kind:?}: shortcut hints ink at the panel footer"
            );
            assert!(
                frame.differs(
                    (ACTIVE_BAR_X as u32, first_row_y() as u32),
                    (SIDEBAR_LOGICAL_WIDTH as u32 - 8, first_row_y() as u32),
                    4,
                ),
                "{kind:?}: the active row has a 2px accent inset bar"
            );
            assert!(
                frame.differs(
                    (STATUS_DOT_X as u32, first_row_y() as u32),
                    (STATUS_DOT_X as u32, second_row_y() as u32),
                    4,
                ),
                "{kind:?}: running and idle dots use distinct token colors"
            );
            assert!(
                frame.differs(
                    (SIDEBAR_LOGICAL_WIDTH as u32 - 1, 180),
                    (SIDEBAR_LOGICAL_WIDTH as u32 - 5, 180),
                    4,
                ),
                "{kind:?}: the panel has a right hairline"
            );
        }
    }

    #[test]
    fn sidebar_hover_reveals_close_and_publishes_add_select_close_intents() {
        let Some((device, queue, format)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let items = [
            SidebarItem::new("main", true),
            SidebarItem::new("idle", false),
        ];
        let view = SidebarView {
            items: &items,
            active: 0,
        };
        let (width, height) = (480, 360);
        let mut atlas = GlyphAtlas::new(&device, format);

        let mut rest = SidebarRenderer::new(&device);
        let rest_frame = render(
            &device, &queue, &mut atlas, &mut rest, &view, None, &theme, width, height,
        );
        let mut rest_hits = crate::HitMap::new();
        for &(rect, target) in rest.hit_regions() {
            rest_hits.push(rect, target);
        }
        assert_eq!(
            rest_hits.hit(185.0, 55.0),
            Some(crate::HitTarget::SidebarAdd),
            "the + emits the new-session intent"
        );
        assert_eq!(
            rest_hits.hit(100.0, second_row_y()),
            Some(crate::HitTarget::SidebarSession(1)),
            "clicking a row emits its select intent"
        );
        assert!(
            !rest
                .hit_regions()
                .iter()
                .any(|(_, target)| matches!(target, crate::HitTarget::SidebarClose(_))),
            "a cold row has no invisible close target"
        );

        let mut hovered = SidebarRenderer::new(&device);
        hovered.prepare(
            &device,
            &queue,
            &mut atlas,
            &view,
            Some(crate::HitTarget::SidebarSession(1)),
            &theme,
            FrameSize {
                width,
                height,
                scale: SCALE,
                content_left: 0.0,
            },
        );
        let close_rect = hovered
            .hit_regions()
            .iter()
            .find_map(|(rect, target)| {
                (*target == crate::HitTarget::SidebarClose(1)).then_some(*rect)
            })
            .expect("hovering a row reveals its close target");
        let mut hovered_hits = crate::HitMap::new();
        for &(rect, target) in hovered.hit_regions() {
            hovered_hits.push(rect, target);
        }
        let close_center = (
            close_rect[0] + close_rect[2] * 0.5,
            close_rect[1] + close_rect[3] * 0.5,
        );
        assert_eq!(
            hovered_hits.hit(close_center.0, close_center.1),
            Some(crate::HitTarget::SidebarClose(1)),
            "the close target wins over its containing row"
        );

        let hovered_frame = render(
            &device,
            &queue,
            &mut atlas,
            &mut hovered,
            &view,
            Some(crate::HitTarget::SidebarSession(1)),
            &theme,
            width,
            height,
        );
        let x0 = close_rect[0] as u32;
        let y0 = close_rect[1] as u32;
        let x1 = (close_rect[0] + close_rect[2]) as u32;
        let y1 = (close_rect[1] + close_rect[3]) as u32;
        assert!(
            (y0..y1).any(|y| {
                (x0..x1).any(|x| rest_frame.rgba(x, y)[..3] != hovered_frame.rgba(x, y)[..3])
            }),
            "hovering the row reveals close-control ink"
        );
    }

    #[test]
    fn sidebar_ellipsizes_long_names_before_the_close_slot() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let items = [SidebarItem::new(
            "this session name is intentionally far wider than the sidebar row",
            false,
        )];
        let view = SidebarView {
            items: &items,
            active: 0,
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut sidebar = SidebarRenderer::new(&device);
        sidebar.prepare(
            &device,
            &queue,
            &mut atlas,
            &view,
            None,
            &theme,
            FrameSize {
                width: 480,
                height: 300,
                scale: SCALE,
                content_left: 0.0,
            },
        );
        let body_px = (type_scale::LABEL.size_pt * SCALE).round().max(1.0);
        let close_left = SIDEBAR_LOGICAL_WIDTH - PANEL_PADDING_X - body_px - 6.0;
        let name_row_top = rows_top(SCALE);
        let name_row_bottom = name_row_top + ROW_HEIGHT * SCALE;
        let rightmost_name_ink = sidebar
            .glyph_instances
            .iter()
            .filter(|glyph| {
                glyph.rect[1] < name_row_bottom
                    && glyph.rect[1] + glyph.rect[3] > name_row_top
                    && glyph.rect[0] > (PANEL_PADDING_X + STATUS_DOT_SIZE) * SCALE
            })
            .map(|glyph| glyph.rect[0] + glyph.rect[2])
            .reduce(f32::max)
            .expect("the row emitted name ink");
        assert!(
            rightmost_name_ink <= close_left,
            "ellipsized name ends before close slot: right={rightmost_name_ink}, close={close_left}"
        );
    }

    #[test]
    fn sidebar_add_affordance_brightens_on_hover() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let items = [SidebarItem::new("main", false)];
        let view = SidebarView {
            items: &items,
            active: 0,
        };
        let (width, height) = (480, 300);
        let mut atlas = GlyphAtlas::new(&device, format);

        let mut rest = SidebarRenderer::new(&device);
        let rest_frame = render(
            &device, &queue, &mut atlas, &mut rest, &view, None, &theme, width, height,
        );
        let add_rect = rest
            .hit_regions()
            .iter()
            .find_map(|(rect, target)| (*target == HitTarget::SidebarAdd).then_some(*rect))
            .expect("the add affordance has a hit rect");

        let mut hovered = SidebarRenderer::new(&device);
        let hovered_frame = render(
            &device,
            &queue,
            &mut atlas,
            &mut hovered,
            &view,
            Some(HitTarget::SidebarAdd),
            &theme,
            width,
            height,
        );
        let x0 = add_rect[0] as u32;
        let y0 = add_rect[1] as u32;
        let x1 = (add_rect[0] + add_rect[2]) as u32;
        let y1 = (add_rect[1] + add_rect[3]) as u32;
        assert!(
            (y0..y1).any(|y| {
                (x0..x1).any(|x| rest_frame.rgba(x, y)[..3] != hovered_frame.rgba(x, y)[..3])
            }),
            "the + re-inks from faint to primary on hover"
        );
    }

    #[test]
    fn unchanged_sidebar_prepare_is_allocation_free() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let items = [
            SidebarItem::new("main", true),
            SidebarItem::new("idle", false),
        ];
        let view = SidebarView {
            items: &items,
            active: 0,
        };
        let size = FrameSize {
            width: 480,
            height: 300,
            scale: SCALE,
            content_left: 0.0,
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut sidebar = SidebarRenderer::new(&device);
        sidebar.prepare(&device, &queue, &mut atlas, &view, None, &theme, size);
        let allocations = crate::alloc_probe::count_allocs(|| {
            let drew = sidebar.prepare(&device, &queue, &mut atlas, &view, None, &theme, size);
            std::hint::black_box(drew);
        });
        assert_eq!(
            allocations, 0,
            "an unchanged sidebar frame allocates nothing (got {allocations})"
        );
    }

    #[test]
    fn sidebar_status_mark_has_rounded_corners() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let items = [SidebarItem::new("main", true)];
        let view = SidebarView {
            items: &items,
            active: 0,
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut sidebar = SidebarRenderer::new(&device);
        let frame = render(
            &device,
            &queue,
            &mut atlas,
            &mut sidebar,
            &view,
            None,
            &theme,
            480,
            300,
        );
        let dot_left = PANEL_PADDING_X as u32;
        let dot_top = (rows_top(SCALE) + (ROW_HEIGHT - STATUS_DOT_SIZE) * 0.5) as u32;
        let row_fill = (dot_left + STATUS_DOT_SIZE as u32 + 3, dot_top);
        assert!(
            !frame.differs((dot_left, dot_top), row_fill, 3),
            "the status mark leaves its top-left corner uninked"
        );
        assert!(
            frame.differs(
                (
                    dot_left + STATUS_DOT_SIZE as u32 / 2,
                    dot_top + STATUS_DOT_SIZE as u32 / 2,
                ),
                row_fill,
                3,
            ),
            "the status mark inks its center"
        );
    }
}
