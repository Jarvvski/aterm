use std::mem::size_of;

use aterm_core::{Document, InputMode};
use aterm_tokens::{space, type_scale, Mode, Rgba, Theme};

use crate::atlas::{GlyphAtlas, GlyphInstance, InstanceBuffer, RectInstance};
use crate::grid_render::FrameSize;
use crate::prose::{ProseLayout, ProseShaper};
use crate::text::{FaceStyle, FontFamily, GlyphKey};

const MAX_BODY_WIDTH_LOGICAL: f32 = 620.0;
const EDGE_PADDING_LOGICAL: f32 = 22.0;
const BODY_GAP_LOGICAL: f32 = 22.0;
const CMD_KEY_GLYPH: char = '\u{F0633}';

/// Borrowed editor facts carried across the existing renderer seam without cloning the
/// document or leaking file transport into `aterm-ui`.
#[derive(Debug, Clone, Copy)]
pub struct EditorView<'a> {
    pub document: &'a Document,
    pub filename: &'a str,
    pub mode: InputMode,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct EditorLayout {
    header_x: f32,
    header_y: f32,
    header_width: f32,
    header_rule_y: f32,
    body_x: f32,
    body_y: f32,
    body_width: f32,
    caret_color: Rgba,
}

fn layout(
    view: &EditorView<'_>,
    theme: &Theme,
    size: FrameSize,
    title_height: f32,
) -> EditorLayout {
    let scale = size.scale;
    let content_width = (size.width as f32 - size.content_left).max(0.0);
    let edge = EDGE_PADDING_LOGICAL * scale;
    let header_x = size.content_left + edge;
    let header_width = (content_width - 2.0 * edge).max(0.0);
    let header_y = title_height + edge;
    let header_height = type_scale::CAPTION.size_pt * type_scale::CAPTION.line_height * scale;
    let header_rule_y = header_y + header_height + f32::from(space::S4) * scale;
    let body_width = (MAX_BODY_WIDTH_LOGICAL * scale).min(header_width);
    let body_x = size.content_left + (content_width - body_width) * 0.5;
    let body_y = header_rule_y + (f32::from(space::HAIRLINE_WIDTH) + BODY_GAP_LOGICAL) * scale;
    let mode = match view.mode {
        InputMode::Shell => Mode::Shell,
        InputMode::Agent => Mode::Agent,
    };
    EditorLayout {
        header_x,
        header_y,
        header_width,
        header_rule_y,
        body_x,
        body_y,
        body_width,
        caret_color: theme.colors.mode_accent(mode),
    }
}

#[derive(Debug, Clone)]
struct CachedLine {
    text: String,
    layout: ProseLayout,
}

struct LineCache {
    shaper: ProseShaper,
    lines: Vec<CachedLine>,
    metrics: Option<(u32, u32, u32)>,
}

impl LineCache {
    fn new() -> Self {
        Self {
            shaper: ProseShaper::new(),
            lines: Vec::new(),
            metrics: None,
        }
    }

    /// Synchronize hard-line layouts and return how many lines required shaping.
    fn sync(&mut self, text: &str, px: f32, measure: f32, line_height: f32) -> usize {
        let metrics = (px.to_bits(), measure.to_bits(), line_height.to_bits());
        if self.metrics != Some(metrics) {
            self.lines.clear();
            self.metrics = Some(metrics);
        }

        let incoming_count = text.split('\n').count();
        if incoming_count == self.lines.len()
            && text
                .split('\n')
                .zip(&self.lines)
                .all(|(line, cached)| line == cached.text)
        {
            return 0;
        }

        let mut previous = std::mem::take(&mut self.lines);
        let mut next = Vec::with_capacity(incoming_count);
        let mut reshaped = 0;
        for line in text.split('\n') {
            if let Some(position) = previous.iter().position(|cached| cached.text == line) {
                next.push(previous.remove(position));
            } else {
                next.push(CachedLine {
                    text: line.to_string(),
                    layout: self.shaper.layout(
                        line,
                        FontFamily::Prose,
                        FaceStyle::Regular,
                        px,
                        measure,
                        line_height,
                    ),
                });
                reshaped += 1;
            }
        }
        self.lines = next;
        reshaped
    }
}

#[derive(Debug, Clone, Copy)]
struct EditorPoint {
    x: f32,
    y: f32,
}

/// Damage-gated writing-surface front-end over the shared glyph atlas. The retained hard-line
/// cache gives edits locality: a keystroke reshapes its affected line while unchanged lines keep
/// their layouts, and an unchanged frame returns before allocating or shaping anything.
pub struct EditorRenderer {
    bg_instances: Vec<RectInstance>,
    glyph_instances: Vec<GlyphInstance>,
    bg_buf: InstanceBuffer,
    glyph_buf: InstanceBuffer,
    line_cache: LineCache,
    shaper: ProseShaper,
    built: Option<u64>,
    caret_area: Option<[f32; 4]>,
    last_glyph_draw_calls: u32,
}

impl EditorRenderer {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            bg_instances: Vec::new(),
            glyph_instances: Vec::new(),
            bg_buf: InstanceBuffer::new(device, "aterm-editor-bg", size_of::<RectInstance>(), 32),
            glyph_buf: InstanceBuffer::new(
                device,
                "aterm-editor-glyph",
                size_of::<GlyphInstance>(),
                512,
            ),
            line_cache: LineCache::new(),
            shaper: ProseShaper::new(),
            built: None,
            caret_area: None,
            last_glyph_draw_calls: 0,
        }
    }

    #[must_use]
    pub fn caret_area_px(&self) -> Option<[f32; 4]> {
        self.caret_area
    }

    #[must_use]
    pub fn last_glyph_draw_calls(&self) -> u32 {
        self.last_glyph_draw_calls
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        view: &EditorView<'_>,
        title_height: f32,
        theme: &Theme,
        size: FrameSize,
    ) -> bool {
        let sig = signature(view, title_height, theme, size);
        if self.built == Some(sig) {
            return !self.glyph_instances.is_empty() || !self.bg_instances.is_empty();
        }

        let geometry = layout(view, theme, size, title_height);
        let body_px = (type_scale::HEADING.size_pt * size.scale).round().max(1.0);
        let body_line_height = body_px * 1.9;
        self.line_cache.sync(
            view.document.text(),
            body_px,
            geometry.body_width,
            body_line_height,
        );
        self.bg_instances.clear();
        self.glyph_instances.clear();

        let hairline = (f32::from(space::HAIRLINE_WIDTH) * size.scale)
            .round()
            .max(1.0);
        self.bg_instances.push(RectInstance {
            rect: [
                geometry.header_x,
                geometry.header_rule_y.round(),
                geometry.header_width,
                hairline,
            ],
            color: theme.colors.hairline.to_linear_f32(),
        });

        let caption_px = (type_scale::CAPTION.size_pt * size.scale).round().max(1.0);
        let caption_line_height = caption_px * type_scale::CAPTION.line_height;
        let filename = self.shaper.layout(
            view.filename,
            FontFamily::Ui,
            FaceStyle::Regular,
            caption_px,
            f32::MAX,
            caption_line_height,
        );
        place_layout(
            &mut self.glyph_instances,
            queue,
            atlas,
            &filename,
            geometry.header_x,
            geometry.header_y,
            theme.colors.fg_primary,
        );
        if view.document.is_dirty() {
            let edited = self.shaper.layout(
                " · edited",
                FontFamily::Ui,
                FaceStyle::Regular,
                caption_px,
                f32::MAX,
                caption_line_height,
            );
            place_layout(
                &mut self.glyph_instances,
                queue,
                atlas,
                &edited,
                geometry.header_x + filename.width,
                geometry.header_y,
                theme.colors.fg_muted,
            );
        }
        let header_right = format!(
            "markdown · {} words · {CMD_KEY_GLYPH}S save · esc to shell",
            view.document.word_count(),
        );
        let right = self.shaper.layout(
            &header_right,
            FontFamily::Ui,
            FaceStyle::Regular,
            caption_px,
            f32::MAX,
            caption_line_height,
        );
        place_layout(
            &mut self.glyph_instances,
            queue,
            atlas,
            &right,
            (geometry.header_x + geometry.header_width - right.width).max(geometry.header_x),
            geometry.header_y,
            theme.colors.fg_faint,
        );

        let selection = view.document.selection();
        let start = editor_point(
            &mut self.shaper,
            &self.line_cache,
            view.document.text(),
            selection.start(),
            body_px,
            geometry.body_width,
            body_line_height,
        );
        let end = editor_point(
            &mut self.shaper,
            &self.line_cache,
            view.document.text(),
            selection.end(),
            body_px,
            geometry.body_width,
            body_line_height,
        );
        if !selection.is_empty() {
            push_selection(
                &mut self.bg_instances,
                start,
                end,
                geometry.body_x,
                geometry.body_y,
                geometry.body_width,
                body_line_height,
                theme.colors.selection_bg,
            );
        }

        let mut line_y = geometry.body_y;
        for cached in &self.line_cache.lines {
            place_layout(
                &mut self.glyph_instances,
                queue,
                atlas,
                &cached.layout,
                geometry.body_x,
                line_y,
                theme.colors.fg_primary,
            );
            line_y += cached.layout.height;
        }

        let mut caret = editor_point(
            &mut self.shaper,
            &self.line_cache,
            view.document.text(),
            selection.caret,
            body_px,
            geometry.body_width,
            body_line_height,
        );
        if let Some(preedit) = view.document.preedit() {
            let preedit_layout = self.shaper.layout(
                &preedit.text,
                FontFamily::Prose,
                FaceStyle::Regular,
                body_px,
                (geometry.body_width - caret.x).max(1.0),
                body_line_height,
            );
            place_layout(
                &mut self.glyph_instances,
                queue,
                atlas,
                &preedit_layout,
                geometry.body_x + caret.x,
                geometry.body_y + caret.y,
                theme.colors.fg_secondary,
            );
            self.bg_instances.push(RectInstance {
                rect: [
                    geometry.body_x + caret.x,
                    geometry.body_y + caret.y + body_line_height - hairline,
                    preedit_layout.width.max(hairline),
                    hairline,
                ],
                color: geometry.caret_color.to_linear_f32(),
            });
            caret.x += preedit_layout.width;
        }

        let caret_width = (2.0 * size.scale).round().max(1.0);
        let caret_height = (body_line_height * 0.82).round().max(1.0);
        let caret_x = (geometry.body_x + caret.x).round();
        let caret_y = (geometry.body_y + caret.y + (body_line_height - caret_height) * 0.5).round();
        self.caret_area = Some([caret_x, caret_y, caret_width, caret_height]);
        self.bg_instances.push(RectInstance {
            rect: [caret_x, caret_y, caret_width, caret_height],
            color: geometry.caret_color.to_linear_f32(),
        });

        if !self.bg_instances.is_empty() {
            self.bg_buf.ensure(
                device,
                "aterm-editor-bg",
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
                "aterm-editor-glyph",
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
            color: color.to_linear_f32(),
        });
    }
}

fn editor_point(
    shaper: &mut ProseShaper,
    cache: &LineCache,
    text: &str,
    char_offset: usize,
    px: f32,
    measure: f32,
    line_height: f32,
) -> EditorPoint {
    let byte = text
        .char_indices()
        .nth(char_offset)
        .map_or(text.len(), |(byte, _)| byte);
    let before = &text[..byte];
    let line_index = before.bytes().filter(|byte| *byte == b'\n').count();
    let prefix = before.rsplit('\n').next().unwrap_or("");
    let (prefix_layout, width) = shaper.layout_with_last_line_width(
        prefix,
        FontFamily::Prose,
        FaceStyle::Regular,
        px,
        measure,
        line_height,
    );
    let preceding_height = cache
        .lines
        .iter()
        .take(line_index)
        .map(|line| line.layout.height)
        .sum::<f32>();
    EditorPoint {
        x: width,
        y: preceding_height + prefix_layout.line_count.saturating_sub(1) as f32 * line_height,
    }
}

#[allow(clippy::too_many_arguments)]
fn push_selection(
    instances: &mut Vec<RectInstance>,
    start: EditorPoint,
    end: EditorPoint,
    body_x: f32,
    body_y: f32,
    body_width: f32,
    line_height: f32,
    color: Rgba,
) {
    let color = color.to_linear_f32();
    if (start.y - end.y).abs() < f32::EPSILON {
        instances.push(RectInstance {
            rect: [
                body_x + start.x,
                body_y + start.y,
                (end.x - start.x).max(1.0),
                line_height,
            ],
            color,
        });
        return;
    }
    instances.push(RectInstance {
        rect: [
            body_x + start.x,
            body_y + start.y,
            (body_width - start.x).max(1.0),
            line_height,
        ],
        color,
    });
    let mut y = start.y + line_height;
    while y < end.y {
        instances.push(RectInstance {
            rect: [body_x, body_y + y, body_width, line_height],
            color,
        });
        y += line_height;
    }
    instances.push(RectInstance {
        rect: [body_x, body_y + end.y, end.x.max(1.0), line_height],
        color,
    });
}

fn signature(view: &EditorView<'_>, title_height: f32, theme: &Theme, size: FrameSize) -> u64 {
    fn fold(hash: u64, value: u64) -> u64 {
        (hash ^ value).wrapping_mul(0x0000_0100_0000_01b3)
    }
    fn fold_str(mut hash: u64, value: &str) -> u64 {
        hash = fold(hash, value.len() as u64);
        for byte in value.bytes() {
            hash = fold(hash, u64::from(byte));
        }
        hash
    }
    let mut hash = 0xcbf2_9ce4_8422_2325;
    hash = fold(hash, view.document.version());
    hash = fold_str(hash, view.filename);
    hash = fold(hash, matches!(view.mode, InputMode::Agent) as u64);
    hash = fold(hash, u64::from(size.width));
    hash = fold(hash, u64::from(size.height));
    hash = fold(hash, u64::from(size.scale.to_bits()));
    hash = fold(hash, u64::from(size.content_left.to_bits()));
    hash = fold(hash, u64::from(title_height.to_bits()));
    for color in [
        theme.colors.bg_canvas,
        theme.colors.fg_primary,
        theme.colors.fg_secondary,
        theme.colors.fg_muted,
        theme.colors.fg_faint,
        theme.colors.hairline,
        theme.colors.selection_bg,
        theme.colors.accent_primary,
        theme.colors.accent_agent,
    ] {
        hash = fold(hash, u64::from(color.to_u32()));
    }
    hash
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use aterm_tokens::{Theme, ThemeKind};

    use super::*;
    use crate::grid_render::FrameSize;

    #[test]
    fn writing_surface_is_centered_and_capped_at_620_logical_pixels() {
        let document = Document::from_text(PathBuf::from("notes.md"), "draft".to_string());
        let view = EditorView {
            document: &document,
            filename: "notes.md",
            mode: InputMode::Shell,
        };

        let layout = layout(
            &view,
            Theme::for_kind(ThemeKind::Dark),
            FrameSize {
                width: 1_200,
                height: 800,
                scale: 1.0,
                content_left: 200.0,
            },
            28.0,
        );

        assert_eq!(layout.body_width, 620.0);
        assert_eq!(layout.body_x, 390.0);
        assert!(layout.body_y > layout.header_rule_y);
    }

    #[test]
    fn changing_one_hard_line_reshapes_only_that_line() {
        let mut cache = LineCache::new();

        assert_eq!(cache.sync("first\nsecond", 16.0, 620.0, 30.4), 2);
        assert_eq!(cache.sync("first draft\nsecond", 16.0, 620.0, 30.4), 1);
        assert_eq!(cache.sync("first draft\nsecond", 16.0, 620.0, 30.4), 0);
        assert_eq!(
            cache.sync("new\nfirst draft\nsecond", 16.0, 620.0, 30.4),
            1,
            "inserting a hard line reuses the unchanged following layouts"
        );
    }

    #[test]
    fn caret_uses_the_active_mode_accent_in_both_themes() {
        let document = Document::from_text(PathBuf::from("notes.md"), "draft".to_string());
        let size = FrameSize {
            width: 900,
            height: 700,
            scale: 1.0,
            content_left: 0.0,
        };
        for kind in [ThemeKind::Light, ThemeKind::Dark] {
            let theme = Theme::for_kind(kind);
            let shell = layout(
                &EditorView {
                    document: &document,
                    filename: "notes.md",
                    mode: InputMode::Shell,
                },
                theme,
                size,
                28.0,
            );
            let agent = layout(
                &EditorView {
                    document: &document,
                    filename: "notes.md",
                    mode: InputMode::Agent,
                },
                theme,
                size,
                28.0,
            );
            assert_eq!(shell.caret_color, theme.colors.accent_primary);
            assert_eq!(agent.caret_color, theme.colors.accent_agent);
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod gpu_tests {
    use std::path::PathBuf;

    use aterm_tokens::{Theme, ThemeKind};

    use super::*;

    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("aterm-editor-test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .ok()
    }

    #[test]
    fn editor_prepares_header_body_and_caret_in_both_themes() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let document = Document::from_text(
            PathBuf::from("notes.md"),
            "A quiet first line.\n\nA second paragraph.".to_string(),
        );
        let size = FrameSize {
            width: 1_000,
            height: 700,
            scale: 1.0,
            content_left: 0.0,
        };

        for kind in [ThemeKind::Light, ThemeKind::Dark] {
            let mut renderer = EditorRenderer::new(&device);
            assert!(renderer.prepare(
                &device,
                &queue,
                &mut atlas,
                &EditorView {
                    document: &document,
                    filename: "notes.md",
                    mode: InputMode::Shell,
                },
                28.0,
                Theme::for_kind(kind),
                size,
            ));
            assert!(renderer.caret_area_px().is_some());
            assert!(!renderer.glyph_instances.is_empty());
            assert!(!renderer.bg_instances.is_empty());
        }
    }

    #[test]
    fn unchanged_editor_prepare_is_allocation_free() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let document = Document::from_text(PathBuf::from("notes.md"), "draft".to_string());
        let view = EditorView {
            document: &document,
            filename: "notes.md",
            mode: InputMode::Agent,
        };
        let size = FrameSize {
            width: 900,
            height: 700,
            scale: 1.0,
            content_left: 0.0,
        };
        let mut renderer = EditorRenderer::new(&device);
        renderer.prepare(
            &device,
            &queue,
            &mut atlas,
            &view,
            28.0,
            Theme::for_kind(ThemeKind::Dark),
            size,
        );

        let allocations = crate::alloc_probe::count_allocs(|| {
            renderer.prepare(
                &device,
                &queue,
                &mut atlas,
                &view,
                28.0,
                Theme::for_kind(ThemeKind::Dark),
                size,
            );
        });
        assert_eq!(allocations, 0);
    }
}
