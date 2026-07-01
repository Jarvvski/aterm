//! The unified-input box (ticket T-3.6): the FOURTH front-end over the shared
//! [`GlyphAtlas`], after the grid ([`crate::grid_render`]), prose ([`crate::prose`]),
//! and the block timeline ([`crate::timeline_render`]).
//!
//! aterm has ONE shell-first input field pinned to the bottom of the window. This draws
//! it to the iA spec ([`05-unified-input-ux.md`] §3, [`07-ia-design-language.md`] §5): a
//! hairline separating it from the timeline, a mode-carrying prompt glyph (`❯` Shell /
//! a Nerd-Font "sparkles" icon Agent), the Mono command line with a 2px accent caret, a
//! small right-aligned
//! SHELL/AGENT chip, a fish-style muted ghost-text tail, an inline IME preedit underline,
//! and the async syntax-highlight overlay - all reading the pure [`aterm_core::InputModel`]
//! the host (`aterm-app` `Session`) owns and drives.
//!
//! ## What carries the mode (locked decision)
//! The caret stays the ONE accent blue ([`aterm_tokens`] `accent_primary`) in BOTH modes
//! (an amber agent caret would collide with the `caution` risk color - ADR / ticket
//! note). The mode reads from the prompt-glyph SHAPE + the chip ([`crate::components`]
//! `PromptChip`: neutral SHELL / accent AGENT) + the empty-buffer placeholder text. The
//! chip occupies a FIXED-WIDTH slot (the max of the two labels), so toggling swaps only
//! the fill + label and never reflows - and, critically, the input TEXT origin is fixed,
//! so the text never moves when the mode flips (AC2; the text itself is preserved by the
//! `InputModel` reducer, ADR-0004). The `motion.fast` cross-fade ([`crate::components`]
//! `Animation::CrossFade`) is the spec; like the timeline's running-pulse / block-insert /
//! focus-dim it is not yet TIME-driven (no frame clock is plumbed into a `Frame` yet), so
//! the swap is instant today (trivially within the 90ms budget) and reflow-free.
//!
//! ## One shaping engine, identical cells
//! The command line, prompt glyph, ghost tail, and preedit all go through the SAME per-cell
//! emitter the grid + timeline use ([`crate::cell_render::emit_cell`], Mono/[`FontFamily::
//! Grid`]) - a selection paints via the cell `bg`, the preedit via the cell `underline` -
//! so the input text is pixel-identical to the timeline's command lines. The Quattro chip
//! LABEL is proportional, so it is shaped through the shared [`crate::prose::ProseShaper`]
//! and its glyphs are placed into the SAME glyph buffer. The solid layer (hairline, caret,
//! chip fill/border, selection) draws through the shared rect pipeline; the whole widget
//! is one rect draw + one glyph draw.
//!
//! ## Damage gating
//! Like the other front-ends, [`Self::prepare`] keys a full rebuild on a cheap FNV
//! signature over everything drawn (mode, text, caret, selection, ghost, preedit,
//! highlight, viewport, px, the colors read) and early-outs (reusing the prior buffers,
//! ZERO allocation) when nothing changed - so an idle present allocates nothing (the
//! T-1.8 60fps floor). The host ([`crate::gpu`]) reserves the bottom zone via the
//! standalone [`zone_px`] BEFORE laying out the timeline, so the timeline draws above the
//! box and neither overlaps.
//!
//! ## Scope (T-3.6)
//! This renders WHATEVER the model carries. The async highlight/ghost worker (T-3.5) and
//! IME preedit feed (T-3.2) populate those fields - until they land the overlay/ghost/
//! preedit are simply empty and the box shows the plain command line. The mode-toggle
//! hotkey + routing (T-3.3) and history (T-3.7) are the host's; this only reflects the
//! `mode` field.

use std::mem::size_of;

use aterm_tokens::{space, type_scale, Rgba, Theme};

use aterm_core::{Highlight, InputMode, InputModel, Preedit, Selection, SpanKind};

use crate::atlas::{GlyphAtlas, GlyphInstance, InstanceBuffer, RectInstance};
use crate::cell_render::{emit_cell, CellCtx};
use crate::components::{AutonomyChip, AutonomyMode, PromptChip, PromptMode};
use crate::grid_render::{FrameSize, INSET_LOGICAL};
use crate::prose::ProseShaper;
use crate::text::{FaceStyle, FontFamily, GlyphKey, GridCell};
use crate::window::cell_px;

/// The most input rows the box shows before it stops growing; a taller multi-line paste
/// scrolls vertically to keep the caret's line in view. Bounds the zone height so a huge
/// paste cannot eat the whole window (and bounds the per-frame layout cost).
const MAX_INPUT_ROWS: u16 = 6;

/// Caret width in logical px (the iA "thin 2px caret"); scaled to physical px at draw.
const CARET_WIDTH_LOGICAL: f32 = 2.0;

/// The Shell prompt glyph (`❯`, U+276F): the shell-first default. Verified present in the
/// bundled Mono Nerd Font (`prompt_glyphs_exist_in_the_bundled_grid_font`).
const SHELL_GLYPH: char = '\u{276F}';
/// The Agent prompt glyph: the Nerd Font "sparkles" icon (`nf-md-creation`, U+F0674) - the
/// research's "small spark" for the agent ([`05-unified-input-ux.md`] §3). It lives in the
/// supplementary PUA, so it is auto-scaled/centered into the cell by the T-4.4 constraint
/// table (`0xF0000..` -> FIT_CENTER) and is verified present in the bundled face. NOTE the
/// obvious geometric "spark"/"star" BMP glyphs (U+2726 `✦`, U+2605 `★`, …) are NOT in this
/// Mono Nerd Font - they resolve to `.notdef` - so the mode glyph MUST be a present PUA
/// icon (this is what the `prompt_glyphs_exist_in_the_bundled_grid_font` test guards).
const AGENT_GLYPH: char = '\u{F0674}';
/// Empty-buffer placeholder per mode (a strong, zero-chrome onboarding + color-blind
/// reinforcement signal; disappears as soon as the user types - [`05-unified-input-ux.md`]
/// §3 option 4).
const SHELL_PLACEHOLDER: &str = "Type a command";
const AGENT_PLACEHOLDER: &str = "Ask the agent";

/// The prompt glyph for `mode` (shape carries the mode; color stays the one accent).
fn prompt_glyph(mode: InputMode) -> char {
    match mode {
        InputMode::Shell => SHELL_GLYPH,
        InputMode::Agent => AGENT_GLYPH,
    }
}

/// The empty-buffer placeholder for `mode`.
fn placeholder(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Shell => SHELL_PLACEHOLDER,
        InputMode::Agent => AGENT_PLACEHOLDER,
    }
}

/// Map the core [`InputMode`] onto the UI-local [`PromptMode`] the chip resolver speaks
/// (the two enums are intentionally distinct: the chip lives in `aterm-ui`, the mode in
/// `aterm-core`, and `aterm-ui -> aterm-core` is the only allowed edge).
fn prompt_mode(mode: InputMode) -> PromptMode {
    match mode {
        InputMode::Shell => PromptMode::Shell,
        InputMode::Agent => PromptMode::Agent,
    }
}

/// The label this mode's fixed-width chip slot is sized to fit both of. Sizing the slot
/// to the WIDER of the two ("SHELL" / "AGENT") is what makes the toggle reflow-free.
const CHIP_LABELS: [&str; 2] = ["SHELL", "AGENT"];

fn chip_label(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Shell => CHIP_LABELS[0],
        InputMode::Agent => CHIP_LABELS[1],
    }
}

/// The autonomy-indicator labels (ticket T-5.11); the slot is sized to the WIDEST so
/// a tier switch is reflow-free, mirroring the routing chip.
const AUTONOMY_LABELS: [&str; 3] = ["ASK", "AUTO-SAFE", "AUTO-RUN"];

/// The (fg, underline) a highlighted span contributes - a deliberately RESTRAINED,
/// near-monochrome, token-only mapping (iA discipline; no syntax rainbow). Command/argument
/// stay the primary ink; options/operators/strings de-emphasize to `fg.secondary`; an error
/// span draws in `danger` and underlines (the one decoration the model names). The async
/// highlighter (T-3.5) can retune this palette; the rendering plumbing is what T-3.6 owns.
fn span_style(kind: SpanKind, theme: &Theme) -> (Rgba, bool) {
    let c = &theme.colors;
    match kind {
        SpanKind::Command | SpanKind::Argument => (c.fg_primary, false),
        SpanKind::Flag | SpanKind::Operator | SpanKind::QuotedString => (c.fg_secondary, false),
        SpanKind::ErrorUnderline => (c.danger, true),
    }
}

/// Number of input rows to show: the buffer's logical line count, clamped to
/// `[1, MAX_INPUT_ROWS]` (empty text still reserves one row for the caret/placeholder).
fn visible_input_rows(text: &str) -> u16 {
    let lines = text.chars().filter(|c| *c == '\n').count() as u16 + 1;
    lines.clamp(1, MAX_INPUT_ROWS)
}

/// The physical-px height of the bottom input zone for `input` at `scale`: a top hairline +
/// `space.4` padding + N text rows + `space.4` padding. Exposed so the host
/// ([`crate::gpu`]) can reserve the zone and shrink the timeline viewport BEFORE it lays
/// the timeline out (the two must agree, so the geometry lives in one function).
#[must_use]
pub fn zone_px(input: &InputModel, scale: f32) -> f32 {
    zone_px_for(input.text(), scale)
}

fn zone_px_for(text: &str, scale: f32) -> f32 {
    let (_, ch) = cell_px(scale);
    let rows = visible_input_rows(text);
    let hairline = (f32::from(space::HAIRLINE_WIDTH) * scale).round().max(1.0);
    let pad = f32::from(space::S4) * scale;
    hairline + pad + f32::from(rows) * ch + pad
}

// ---------------------------------------------------------------------------
// Pure layout (no GPU / no window) - testable on every platform
// ---------------------------------------------------------------------------

/// A borrowed, allocation-free view of the input state the widget draws. Taken as
/// EXPLICIT pieces (not just an `&InputModel`) so the pure layout is testable independent
/// of the model's private fields - notably a [`Preedit`], which the model exposes only by
/// accessor and has no public setter (it is fed by T-3.2).
pub(crate) struct InputView<'a> {
    pub text: &'a str,
    pub caret: usize,
    pub selection: Selection,
    pub mode: InputMode,
    pub ghost_tail: Option<&'a str>,
    pub preedit: Option<&'a Preedit>,
    pub highlight: &'a Highlight,
}

impl<'a> InputView<'a> {
    fn from_model(m: &'a InputModel) -> Self {
        Self {
            text: m.text(),
            caret: m.caret(),
            selection: m.selection(),
            mode: m.mode(),
            ghost_tail: m.ghost_tail(),
            preedit: m.preedit(),
            highlight: m.highlight(),
        }
    }
}

/// One placed display cell: its display grid position (post horizontal-scroll + vertical
/// window), glyph, resolved fg, background (canvas unless selected), and underline (a
/// preedit char or an error span).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Placed {
    col: u16,
    row: u16,
    ch: char,
    fg: Rgba,
    bg: Rgba,
    underline: bool,
}

/// One pre-clip visual cell while a line is being assembled (before the horizontal scroll
/// and the column budget turn it into a [`Placed`]): its glyph, its resolved fg, whether
/// it underlines (a preedit char or an error span), and whether it falls in the selection.
#[derive(Clone, Copy)]
struct VisCell {
    ch: char,
    fg: Rgba,
    underline: bool,
    selected: bool,
}

/// The pure result of laying out the editable region: the display cells, the caret's
/// display position (already scrolled into view), the number of text rows shown, and
/// whether this is the empty-buffer placeholder.
struct InputCells {
    cells: Vec<Placed>,
    caret_col: u16,
    caret_row: u16,
    rows: u16,
    placeholder: bool,
}

/// Count the chars of `s` whose byte offset is `< byte` (winit preedit cursor ranges are
/// byte-indexed; the visual caret is char-indexed). Robust to a non-char-boundary `byte`.
fn byte_to_char_count(s: &str, byte: usize) -> usize {
    s.char_indices().take_while(|(b, _)| *b < byte).count()
}

/// Lay out the editable region into display cells + caret position, given the visible
/// column budget. Pure: no GPU, no atlas, no window - just the model view + theme + width.
///
/// Handles: the empty-buffer placeholder (per mode), the selection background, the async
/// highlight overlay (per-char fg + error underline), the inline IME preedit (spliced at
/// the caret, underlined, advancing the visual caret), the fish-style ghost tail (muted,
/// appended at the line end when no preedit is active), per-line horizontal scroll that
/// keeps the caret column visible, and a vertical window over `MAX_INPUT_ROWS` that keeps
/// the caret's line on screen.
fn layout_cells(view: &InputView, theme: &Theme, visible_cols: u16) -> InputCells {
    let visible_cols = visible_cols.max(1);
    let canvas = theme.colors.bg_canvas;
    let chars: Vec<char> = view.text.chars().collect();

    // Empty buffer with no active composition: show the placeholder + a caret at the start.
    if chars.is_empty() && view.preedit.is_none() {
        let text = placeholder(view.mode);
        let mut cells = Vec::new();
        for (i, ch) in text.chars().enumerate() {
            if i >= visible_cols as usize {
                break;
            }
            cells.push(Placed {
                col: i as u16,
                row: 0,
                ch,
                fg: theme.colors.fg_muted,
                bg: canvas,
                underline: false,
            });
        }
        return InputCells {
            cells,
            caret_col: 0,
            caret_row: 0,
            rows: 1,
            placeholder: true,
        };
    }

    // Logical lines as (start char index, char len), always non-empty.
    let mut lines: Vec<(usize, usize)> = Vec::new();
    let (mut start, mut count) = (0usize, 0usize);
    for (i, ch) in chars.iter().enumerate() {
        if *ch == '\n' {
            lines.push((start, count));
            start = i + 1;
            count = 0;
        } else {
            count += 1;
        }
    }
    lines.push((start, count));

    // Caret (line, col) from its char offset.
    let (mut caret_line, mut caret_col0) = (0usize, 0usize);
    for ch in chars.iter().take(view.caret) {
        if *ch == '\n' {
            caret_line += 1;
            caret_col0 = 0;
        } else {
            caret_col0 += 1;
        }
    }

    let total_lines = lines.len();
    let rows_shown = (total_lines as u16).min(MAX_INPUT_ROWS);
    // Vertical window: keep the caret line visible, preferring it near the bottom.
    let window_start = if total_lines <= MAX_INPUT_ROWS as usize {
        0
    } else {
        let want = (caret_line + 1).saturating_sub(MAX_INPUT_ROWS as usize);
        want.min(total_lines - MAX_INPUT_ROWS as usize)
    };

    let sel_active = !view.selection.is_empty();
    let (sel_start, sel_end) = (view.selection.start(), view.selection.end());
    let style_for = |g: usize| -> (Rgba, bool) {
        for span in &view.highlight.spans {
            if g >= span.start && g < span.end {
                return span_style(span.kind, theme);
            }
        }
        (theme.colors.fg_primary, false)
    };

    // Build each shown line's visual cell run (fg/underline/selected), splicing the
    // preedit on the caret line and appending the ghost tail at its end. The visual caret
    // column lives on the caret line (after any preedit).
    let mut line_vis: Vec<(u16, Vec<VisCell>)> = Vec::new();
    let mut caret_vis_col = 0usize;
    let mut caret_display_row = 0u16;
    for (li, &(lstart, llen)) in lines
        .iter()
        .enumerate()
        .skip(window_start)
        .take(rows_shown as usize)
    {
        let display_row = (li - window_start) as u16;
        let mut vis: Vec<VisCell> = Vec::with_capacity(llen);
        for k in 0..llen {
            let g = lstart + k;
            let (fg, underline) = style_for(g);
            let selected = sel_active && g >= sel_start && g < sel_end;
            vis.push(VisCell {
                ch: chars[g],
                fg,
                underline,
                selected,
            });
        }
        if li == caret_line {
            caret_display_row = display_row;
            if let Some(pe) = view.preedit {
                let pe_chars: Vec<char> = pe.text.chars().collect();
                let at = caret_col0.min(vis.len());
                let mut spliced = Vec::with_capacity(vis.len() + pe_chars.len());
                spliced.extend_from_slice(&vis[..at]);
                for &pc in &pe_chars {
                    spliced.push(VisCell {
                        ch: pc,
                        fg: theme.colors.fg_primary,
                        underline: true,
                        selected: false,
                    });
                }
                spliced.extend_from_slice(&vis[at..]);
                vis = spliced;
                let pe_caret = pe
                    .cursor
                    .map_or(pe_chars.len(), |(_, e)| byte_to_char_count(&pe.text, e));
                caret_vis_col = caret_col0 + pe_caret;
            } else {
                caret_vis_col = caret_col0;
                if let Some(g) = view.ghost_tail {
                    for gc in g.chars() {
                        vis.push(VisCell {
                            ch: gc,
                            fg: theme.colors.fg_muted,
                            underline: false,
                            selected: false,
                        });
                    }
                }
            }
        }
        line_vis.push((display_row, vis));
    }

    // Horizontal scroll: shift so the caret column is visible (the caret line drives it;
    // the same offset applies to every shown line, which is what a single-line box wants).
    let h_off = if caret_vis_col >= visible_cols as usize {
        caret_vis_col - visible_cols as usize + 1
    } else {
        0
    };

    let mut cells = Vec::new();
    for (display_row, vis) in &line_vis {
        for (vi, slot) in vis.iter().enumerate() {
            if vi < h_off {
                continue;
            }
            let dc = vi - h_off;
            if dc >= visible_cols as usize {
                break;
            }
            cells.push(Placed {
                col: dc as u16,
                row: *display_row,
                ch: slot.ch,
                fg: slot.fg,
                bg: if slot.selected {
                    theme.colors.selection_bg
                } else {
                    canvas
                },
                underline: slot.underline,
            });
        }
    }

    InputCells {
        cells,
        caret_col: (caret_vis_col - h_off) as u16,
        caret_row: caret_display_row,
        rows: rows_shown,
        placeholder: false,
    }
}

// ---------------------------------------------------------------------------
// GPU front-end
// ---------------------------------------------------------------------------

/// The unified-input box front-end over the shared [`GlyphAtlas`]. Owns its own instance
/// buffers + rebuild gate + a [`ProseShaper`] for the Quattro chip label; draws through
/// the shared rect + glyph pipelines. Constructed once from the device.
pub struct InputWidgetRenderer {
    bg_instances: Vec<RectInstance>,
    glyph_instances: Vec<GlyphInstance>,
    bg_buf: InstanceBuffer,
    glyph_buf: InstanceBuffer,
    /// Shapes the proportional SHELL/AGENT chip label (Quattro/`FontFamily::Ui`); its
    /// glyphs are placed into `glyph_instances`, so the whole widget is one glyph draw.
    shaper: ProseShaper,
    /// Rebuild gate: the signature currently built, or `None`.
    built: Option<u64>,
    /// Glyph-layer draw calls issued by the last [`Self::draw`] (1 when anything inked).
    last_glyph_draw_calls: u32,
    /// The caret's rect in PHYSICAL px `[x, y, w, h]` from the last [`Self::prepare`]
    /// that rebuilt, or `None` before the first build. Used by [`crate::app`] to place
    /// the IME candidate window under the caret via `Window::set_ime_cursor_area`
    /// (ticket T-3.2). Held across an unchanged-signature early-out because the caret is
    /// part of the signature, so a stale value only survives while the caret has not
    /// moved.
    last_caret_px: Option<[f32; 4]>,
}

impl InputWidgetRenderer {
    /// Build the input front-end: its reused instance buffers + the chip-label shaper.
    /// The shared [`GlyphAtlas`] is owned by [`crate::gpu::GpuRenderer`] and lent per call.
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            bg_instances: Vec::new(),
            glyph_instances: Vec::new(),
            bg_buf: InstanceBuffer::new(device, "aterm-input-bg", size_of::<RectInstance>(), 64),
            glyph_buf: InstanceBuffer::new(
                device,
                "aterm-input-glyph",
                size_of::<GlyphInstance>(),
                128,
            ),
            shaper: ProseShaper::new(),
            built: None,
            last_glyph_draw_calls: 0,
            last_caret_px: None,
        }
    }

    /// Glyph-layer draw calls from the last [`Self::draw`] (1 when there is text).
    #[must_use]
    pub fn last_glyph_draw_calls(&self) -> u32 {
        self.last_glyph_draw_calls
    }

    /// The caret's rect in PHYSICAL px `[x, y, w, h]` as of the last built frame, or
    /// `None` before the first build. [`crate::app`] feeds this to
    /// `Window::set_ime_cursor_area` so the IME candidate window sits under the caret
    /// (ticket T-3.2).
    #[must_use]
    pub fn caret_area_px(&self) -> Option<[f32; 4]> {
        self.last_caret_px
    }

    /// Build the frame's instances for `input` through the shared `atlas`, reusing the
    /// prior build when the signature is unchanged (the damage gate). Returns `true` if
    /// there is anything to draw. The box is laid out at the BOTTOM of the surface, in a
    /// zone of height [`zone_px`]; the host shrinks the timeline viewport by that amount.
    ///
    /// The unchanged path allocates nothing (the steady-state early-out); the changed path
    /// reuses its warm `Vec`s + the glyph cache (`queue.write_buffer` is wgpu staging, not
    /// part of that claim).
    // The renderer fast-path threads device/queue/atlas/model/autonomy/theme/size by
    // value to stay allocation-free; bundling them into a struct would add a borrow
    // dance per frame for no clarity gain. Mirrors the grid/timeline prepare shape.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        input: &InputModel,
        autonomy: Option<AutonomyMode>,
        theme: &Theme,
        size: FrameSize,
    ) -> bool {
        let FrameSize {
            width,
            height,
            scale,
        } = size;
        let px = (type_scale::GRID.size_pt * scale).round().max(1.0);
        let px_key = px as u32;
        let view = InputView::from_model(input);

        // Fold the autonomy posture (ticket T-5.11) into the damage signature so a
        // mode switch redraws the always-visible indicator even when the input buffer
        // is unchanged (AC4). `None` (a host with no agent) is its own state.
        let autonomy_code: u64 = match autonomy {
            None => 0,
            Some(AutonomyMode::AskAlways) => 1,
            Some(AutonomyMode::AutoSafe) => 2,
            Some(AutonomyMode::AutoRunInSession) => 3,
        };
        let sig = signature(&view, width, height, px_key, theme)
            .wrapping_mul(0x0000_0100_0000_01b3)
            ^ autonomy_code.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        if self.built == Some(sig) {
            return !self.glyph_instances.is_empty() || !self.bg_instances.is_empty();
        }

        self.bg_instances.clear();
        self.glyph_instances.clear();

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

        // Zone geometry (physical px). The box sits flush against the bottom; the canvas
        // clear shows through it (iA: no fill behind the input).
        let zone_h = zone_px_for(view.text, scale);
        let zone_top = (height as f32 - zone_h).max(0.0);
        let hairline_h = (f32::from(space::HAIRLINE_WIDTH) * scale).round().max(1.0);
        let pad = f32::from(space::S4) * scale;
        let content_w = (width as f32 - 2.0 * inset).max(0.0);
        let first_row_y = zone_top + hairline_h + pad;

        // Top hairline separating the box from the timeline.
        self.bg_instances.push(RectInstance {
            rect: [inset, zone_top.round(), content_w, hairline_h],
            color: theme.colors.hairline.to_linear_f32(),
        });

        // The prompt glyph at the left edge (one Mono cell, the one accent color; the
        // SHAPE carries the mode, the color stays accent in both modes).
        let prompt = grid_glyph(prompt_glyph(view.mode), theme.colors.accent_primary, canvas);
        emit_cell(
            atlas,
            queue,
            &prompt,
            (inset, first_row_y),
            &ctx,
            &mut self.bg_instances,
            &mut self.glyph_instances,
        );

        // The fixed-width SHELL/AGENT chip, right-aligned. Shape BOTH labels so the slot
        // fits the wider, then place the active one centered - so a toggle never reflows.
        let px_label = (type_scale::LABEL.size_pt * scale).round().max(1.0);
        let line_h_label = px_label * type_scale::LABEL.line_height;
        let pad_x = f32::from(space::S2) * scale;
        let pad_y = f32::from(space::S1) * scale;
        let mut slot_w: f32 = 0.0;
        let mut label_h: f32 = line_h_label;
        for lbl in CHIP_LABELS {
            let l = self.shaper.layout(
                lbl,
                FontFamily::Ui,
                FaceStyle::Regular,
                px_label,
                f32::MAX,
                line_h_label,
            );
            slot_w = slot_w.max(l.width);
            label_h = label_h.max(l.height);
        }
        let slot_w = slot_w + 2.0 * pad_x;
        let chip_h = label_h + 2.0 * pad_y;
        let chip_right = width as f32 - inset;
        let chip_x = (chip_right - slot_w).max(inset);
        let chip_y = first_row_y + (ch - chip_h) * 0.5;

        let chip = PromptChip::resolve(prompt_mode(view.mode), theme);
        // Chip fill (+ hairline border on the neutral SHELL chip only).
        if let Some(border) = chip.chip.border {
            self.bg_instances.push(RectInstance {
                rect: [chip_x, chip_y, slot_w, chip_h],
                color: border.to_linear_f32(),
            });
            let b = hairline_h;
            self.bg_instances.push(RectInstance {
                rect: [
                    chip_x + b,
                    chip_y + b,
                    (slot_w - 2.0 * b).max(0.0),
                    (chip_h - 2.0 * b).max(0.0),
                ],
                color: chip.chip.fill.to_linear_f32(),
            });
        } else {
            self.bg_instances.push(RectInstance {
                rect: [chip_x, chip_y, slot_w, chip_h],
                color: chip.chip.fill.to_linear_f32(),
            });
        }
        // The active label, shaped + centered in the slot, into the shared glyph buffer.
        let label_layout = self.shaper.layout(
            chip_label(view.mode),
            FontFamily::Ui,
            FaceStyle::Regular,
            px_label,
            f32::MAX,
            line_h_label,
        );
        let label_x = chip_x + (slot_w - label_layout.width) * 0.5;
        let label_y = chip_y + (chip_h - label_layout.height) * 0.5;
        let label_color = chip.chip.text.to_linear_f32();
        let inv = 1.0 / atlas.atlas_dim() as f32;
        for pg in &label_layout.glyphs {
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
                    (label_x + pg.pen_x + left as f32).round(),
                    (label_y + pg.baseline - top as f32).round(),
                    rect.w as f32,
                    rect.h as f32,
                ],
                uv: [
                    rect.x as f32 * inv,
                    rect.y as f32 * inv,
                    (rect.x + rect.w) as f32 * inv,
                    (rect.y + rect.h) as f32 * inv,
                ],
                color: label_color,
            });
        }

        // The autonomy-mode indicator (ticket T-5.11): an always-visible chip to the
        // LEFT of the routing chip so the safety posture is never hidden (AC4). Sized
        // to the WIDEST tier label so a switch never reflows; ALWAYS color + label.
        // Returns the chip's left edge so the text region clears it too.
        let left_edge = if let Some(mode) = autonomy {
            let mut a_slot_w: f32 = 0.0;
            let mut a_label_h: f32 = line_h_label;
            for lbl in AUTONOMY_LABELS {
                let l = self.shaper.layout(
                    lbl,
                    FontFamily::Ui,
                    FaceStyle::Regular,
                    px_label,
                    f32::MAX,
                    line_h_label,
                );
                a_slot_w = a_slot_w.max(l.width);
                a_label_h = a_label_h.max(l.height);
            }
            let a_slot_w = a_slot_w + 2.0 * pad_x;
            let a_chip_h = a_label_h + 2.0 * pad_y;
            let gap = f32::from(space::S2) * scale;
            let a_chip_x = (chip_x - gap - a_slot_w).max(inset);
            let a_chip_y = first_row_y + (ch - a_chip_h) * 0.5;

            let ac = AutonomyChip::resolve(mode, theme);
            // Chip fill (+ hairline border on the neutral ASK chip only).
            if let Some(border) = ac.chip.border {
                self.bg_instances.push(RectInstance {
                    rect: [a_chip_x, a_chip_y, a_slot_w, a_chip_h],
                    color: border.to_linear_f32(),
                });
                let b = hairline_h;
                self.bg_instances.push(RectInstance {
                    rect: [
                        a_chip_x + b,
                        a_chip_y + b,
                        (a_slot_w - 2.0 * b).max(0.0),
                        (a_chip_h - 2.0 * b).max(0.0),
                    ],
                    color: ac.chip.fill.to_linear_f32(),
                });
            } else {
                self.bg_instances.push(RectInstance {
                    rect: [a_chip_x, a_chip_y, a_slot_w, a_chip_h],
                    color: ac.chip.fill.to_linear_f32(),
                });
            }
            // The active label, shaped + centered in the slot.
            let a_layout = self.shaper.layout(
                ac.label,
                FontFamily::Ui,
                FaceStyle::Regular,
                px_label,
                f32::MAX,
                line_h_label,
            );
            let a_label_x = a_chip_x + (a_slot_w - a_layout.width) * 0.5;
            let a_label_y = a_chip_y + (a_chip_h - a_layout.height) * 0.5;
            let a_color = ac.chip.text.to_linear_f32();
            for pg in &a_layout.glyphs {
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
                        (a_label_x + pg.pen_x + left as f32).round(),
                        (a_label_y + pg.baseline - top as f32).round(),
                        rect.w as f32,
                        rect.h as f32,
                    ],
                    uv: [
                        rect.x as f32 * inv,
                        rect.y as f32 * inv,
                        (rect.x + rect.w) as f32 * inv,
                        (rect.y + rect.h) as f32 * inv,
                    ],
                    color: a_color,
                });
            }
            a_chip_x
        } else {
            chip_x
        };

        // The editable region: from after the prompt glyph (one blank cell) to clear of
        // the chips. Clip the text to the columns that fit (clip-by-omission; no scissor).
        let text_x = inset + 2.0 * cw;
        let text_right = (left_edge - cw).max(text_x);
        let visible_cols = (((text_right - text_x) / cw).floor() as i64).max(1) as u16;
        let laid = layout_cells(&view, theme, visible_cols);

        for c in &laid.cells {
            let cell = text_cell(c.ch, c.fg, c.bg, c.underline);
            emit_cell(
                atlas,
                queue,
                &cell,
                (
                    text_x + f32::from(c.col) * cw,
                    first_row_y + f32::from(c.row) * ch,
                ),
                &ctx,
                &mut self.bg_instances,
                &mut self.glyph_instances,
            );
        }

        // The caret: a thin accent bar at the caret column (the one accent, both modes).
        let caret_w = (CARET_WIDTH_LOGICAL * scale).round().max(1.0);
        let caret_x = text_x + f32::from(laid.caret_col) * cw;
        let caret_y = first_row_y + f32::from(laid.caret_row) * ch + ch * 0.1;
        let caret_h = (ch * 0.8).round().max(1.0);
        self.bg_instances.push(RectInstance {
            rect: [caret_x.round(), caret_y.round(), caret_w, caret_h],
            color: theme.colors.accent_primary.to_linear_f32(),
        });
        // Record the caret rect (physical px) so the host can position the IME candidate
        // window under it (ticket T-3.2). Kept until the next rebuild; the caret is part
        // of the damage signature, so this is only stale while the caret has not moved.
        self.last_caret_px = Some([caret_x.round(), caret_y.round(), caret_w, caret_h]);

        if !self.bg_instances.is_empty() {
            self.bg_buf.ensure(
                device,
                "aterm-input-bg",
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
                "aterm-input-glyph",
                size_of::<GlyphInstance>(),
                self.glyph_instances.len(),
            );
            queue.write_buffer(
                self.glyph_buf.buf(),
                0,
                bytemuck::cast_slice(&self.glyph_instances),
            );
        }
        atlas.set_viewport(queue, width, height);

        self.built = Some(sig);
        let _ = laid.rows;
        let _ = laid.placeholder;
        !self.glyph_instances.is_empty() || !self.bg_instances.is_empty()
    }

    /// Record the input-box draws into `pass` through the shared `atlas`: the solid layer
    /// (hairline, selection, caret, chip fill) first, then the single glyph instanced draw.
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
/// quad is skipped by [`emit_cell`]); used for the prompt glyph.
fn grid_glyph(ch: char, fg: Rgba, canvas: Rgba) -> GridCell {
    text_cell(ch, fg, canvas, false)
}

/// A Mono cell for the editable text: glyph `ch` in `fg` on `bg` (canvas or selection), with
/// an optional `underline` (a preedit char or an error span).
fn text_cell(ch: char, fg: Rgba, bg: Rgba, underline: bool) -> GridCell {
    GridCell {
        col: 0,
        row: 0,
        ch,
        fg,
        bg,
        bold: false,
        italic: false,
        underline,
        wide: false,
    }
}

/// A stable u64 over everything the input draw reads: the mode, the full text, the caret +
/// selection, the ghost tail, the preedit, the highlight spans, the viewport, the px, and
/// the colors consumed. Computed every frame BEFORE the rebuild gate, so it folds borrowed
/// data and small counts only (it allocates nothing).
fn signature(view: &InputView, w: u32, h: u32, px_key: u32, theme: &Theme) -> u64 {
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
    s = fold_u64(s, matches!(view.mode, InputMode::Agent) as u64);
    s = fold_str(s, view.text);
    s = fold_u64(s, view.caret as u64);
    s = fold_u64(s, view.selection.anchor as u64);
    s = fold_u64(s, view.selection.caret as u64);
    s = fold_u64(s, u64::from(w));
    s = fold_u64(s, u64::from(h));
    s = fold_u64(s, u64::from(px_key));

    match view.ghost_tail {
        Some(g) => s = fold_str(fold_u64(s, 1), g),
        None => s = fold_u64(s, 0),
    }
    match view.preedit {
        Some(pe) => {
            s = fold_str(fold_u64(s, 1), &pe.text);
            match pe.cursor {
                Some((a, b)) => {
                    s = fold_u64(fold_u64(fold_u64(s, 1), a as u64), b as u64);
                }
                None => s = fold_u64(s, 0),
            }
        }
        None => s = fold_u64(s, 0),
    }
    s = fold_u64(s, view.highlight.spans.len() as u64);
    for span in &view.highlight.spans {
        s = fold_u64(s, span.start as u64);
        s = fold_u64(s, span.end as u64);
        s = fold_u64(s, span.kind as u64);
    }

    // The colors the input reads (a superset is always safe - it can only force an extra,
    // correct rebuild, never keep a stale color).
    let c = &theme.colors;
    for color in [
        c.bg_canvas,
        c.fg_primary,
        c.fg_secondary,
        c.fg_muted,
        c.accent_primary,
        c.accent_agent,
        c.accent_primary_text,
        c.accent_primary_weak,
        c.bg_surface,
        c.selection_bg,
        c.hairline,
        c.danger,
        // The autonomy chip resolves Success/Caution tints, so a theme that moved
        // only those (with the folded colors unchanged) must still invalidate.
        c.success,
        c.caution,
    ] {
        s = fold_color(s, color);
    }
    s
}

// ---------------------------------------------------------------------------
// Pure (no-GPU) tests - run on every platform
// ---------------------------------------------------------------------------
#[cfg(test)]
mod layout_tests {
    use super::*;
    use aterm_core::{Preedit, StyleSpan};
    use aterm_tokens::ThemeKind;

    fn dark() -> Theme {
        *Theme::for_kind(ThemeKind::Dark)
    }

    fn view<'a>(
        text: &'a str,
        caret: usize,
        sel: Selection,
        mode: InputMode,
        ghost: Option<&'a str>,
        preedit: Option<&'a Preedit>,
        hl: &'a Highlight,
    ) -> InputView<'a> {
        InputView {
            text,
            caret,
            selection: sel,
            mode,
            ghost_tail: ghost,
            preedit,
            highlight: hl,
        }
    }

    const EMPTY_HL: Highlight = Highlight { spans: Vec::new() };

    #[test]
    fn prompt_glyphs_exist_in_the_bundled_grid_font() {
        // The mode indicator is broken if a prompt glyph is missing from the bundled Mono
        // Nerd Font: `emit_cell` would draw `.notdef` (a box) or nothing. A cmap lookup of
        // `0` IS the `.notdef` glyph id, so BOTH prompt glyphs must resolve non-zero. This
        // runs on every platform (no GPU - just the font parse), catching the regression
        // the macOS-only GPU test cannot guarantee. (Review finding: U+2726 was `.notdef`.)
        let r = crate::glyph::GlyphRasterizer::new();
        for (mode, glyph) in [
            (InputMode::Shell, SHELL_GLYPH),
            (InputMode::Agent, AGENT_GLYPH),
        ] {
            let gid = r.glyph_id(FontFamily::Grid, FaceStyle::Regular, glyph);
            assert_ne!(
                gid, 0,
                "{mode:?} prompt glyph U+{:04X} is .notdef in the bundled Mono Nerd Font",
                glyph as u32
            );
        }
    }

    #[test]
    fn empty_buffer_shows_mode_placeholder_and_caret_at_start() {
        let theme = dark();
        let hl = Highlight::default();
        for (mode, want) in [
            (InputMode::Shell, SHELL_PLACEHOLDER),
            (InputMode::Agent, AGENT_PLACEHOLDER),
        ] {
            let v = view("", 0, Selection::at(0), mode, None, None, &hl);
            let laid = layout_cells(&v, &theme, 80);
            assert!(
                laid.placeholder,
                "{mode:?}: empty buffer is the placeholder"
            );
            let s: String = laid.cells.iter().map(|c| c.ch).collect();
            assert_eq!(s, want, "{mode:?}: placeholder text");
            assert!(
                laid.cells.iter().all(|c| c.fg == theme.colors.fg_muted),
                "placeholder is muted"
            );
            assert_eq!((laid.caret_col, laid.caret_row), (0, 0));
        }
    }

    #[test]
    fn text_places_cells_with_caret_at_end() {
        let theme = dark();
        let hl = Highlight::default();
        let v = view(
            "git push",
            8,
            Selection::at(8),
            InputMode::Shell,
            None,
            None,
            &hl,
        );
        let laid = layout_cells(&v, &theme, 80);
        assert!(!laid.placeholder);
        let s: String = laid.cells.iter().map(|c| c.ch).collect();
        assert_eq!(s, "git push");
        assert!(laid.cells.iter().all(|c| c.fg == theme.colors.fg_primary));
        assert_eq!((laid.caret_col, laid.caret_row), (8, 0));
    }

    #[test]
    fn mode_does_not_change_text_cells_or_caret() {
        // AC2 at the layout layer: the text + caret are identical across modes; only the
        // chip + prompt glyph (resolved elsewhere) differ. The toggle never reflows text.
        let theme = dark();
        let hl = Highlight::default();
        let shell = layout_cells(
            &view(
                "ls -la",
                6,
                Selection::at(6),
                InputMode::Shell,
                None,
                None,
                &hl,
            ),
            &theme,
            80,
        );
        let agent = layout_cells(
            &view(
                "ls -la",
                6,
                Selection::at(6),
                InputMode::Agent,
                None,
                None,
                &hl,
            ),
            &theme,
            80,
        );
        assert_eq!(shell.cells, agent.cells, "text cells are mode-independent");
        assert_eq!(
            (shell.caret_col, shell.caret_row),
            (agent.caret_col, agent.caret_row)
        );
        // The prompt glyph + chip label DO differ by mode.
        assert_ne!(
            prompt_glyph(InputMode::Shell),
            prompt_glyph(InputMode::Agent)
        );
        assert_ne!(chip_label(InputMode::Shell), chip_label(InputMode::Agent));
    }

    #[test]
    fn selection_paints_selected_cells_with_selection_bg() {
        let theme = dark();
        let hl = Highlight::default();
        // Select chars [0,3) of "hello".
        let v = view(
            "hello",
            3,
            Selection {
                anchor: 0,
                caret: 3,
            },
            InputMode::Shell,
            None,
            None,
            &hl,
        );
        let laid = layout_cells(&v, &theme, 80);
        for c in &laid.cells {
            let want = if c.col < 3 {
                theme.colors.selection_bg
            } else {
                theme.colors.bg_canvas
            };
            assert_eq!(c.bg, want, "col {} selection bg", c.col);
        }
    }

    #[test]
    fn ghost_tail_appends_muted_cells_after_the_text() {
        let theme = dark();
        let hl = Highlight::default();
        let v = view(
            "git st",
            6,
            Selection::at(6),
            InputMode::Shell,
            Some("atus"),
            None,
            &hl,
        );
        let laid = layout_cells(&v, &theme, 80);
        let s: String = laid.cells.iter().map(|c| c.ch).collect();
        assert_eq!(s, "git status", "the ghost tail extends the line");
        // The first 6 are primary, the trailing 4 (the ghost) are muted.
        for (i, c) in laid.cells.iter().enumerate() {
            let want = if i < 6 {
                theme.colors.fg_primary
            } else {
                theme.colors.fg_muted
            };
            assert_eq!(c.fg, want, "cell {i} fg");
        }
        // The caret stays at the typed end, before the ghost.
        assert_eq!(laid.caret_col, 6);
    }

    #[test]
    fn preedit_renders_inline_underlined_and_advances_the_caret() {
        let theme = dark();
        let hl = Highlight::default();
        // Composing "ni" at the caret (after "ko") -> e.g. en route to a CJK candidate.
        let pe = Preedit {
            text: "ni".to_string(),
            cursor: None,
        };
        let v = view(
            "ko",
            2,
            Selection::at(2),
            InputMode::Agent,
            None,
            Some(&pe),
            &hl,
        );
        let laid = layout_cells(&v, &theme, 80);
        let s: String = laid.cells.iter().map(|c| c.ch).collect();
        assert_eq!(s, "koni", "preedit is spliced at the caret");
        // The two preedit cells are underlined; the committed text is not.
        assert!(!laid.cells[0].underline && !laid.cells[1].underline);
        assert!(
            laid.cells[2].underline && laid.cells[3].underline,
            "preedit underlined"
        );
        // The visual caret sits after the preedit.
        assert_eq!(laid.caret_col, 4);
    }

    #[test]
    fn highlight_overlay_applies_span_fg_and_error_underline() {
        let theme = dark();
        // "rm x" -> command span [0,2), an error underline over the arg [3,4).
        let hl = Highlight {
            spans: vec![
                StyleSpan {
                    start: 0,
                    end: 2,
                    kind: SpanKind::Command,
                },
                StyleSpan {
                    start: 3,
                    end: 4,
                    kind: SpanKind::ErrorUnderline,
                },
            ],
        };
        let v = view(
            "rm x",
            4,
            Selection::at(4),
            InputMode::Shell,
            None,
            None,
            &hl,
        );
        let laid = layout_cells(&v, &theme, 80);
        // Command chars are primary, no underline.
        assert_eq!(laid.cells[0].fg, theme.colors.fg_primary);
        assert!(!laid.cells[0].underline);
        // The errored 'x' is danger + underlined.
        let x = laid.cells.iter().find(|c| c.ch == 'x').unwrap();
        assert_eq!(x.fg, theme.colors.danger);
        assert!(x.underline, "error span underlines");
    }

    #[test]
    fn a_long_line_scrolls_horizontally_to_keep_the_caret_visible() {
        let theme = dark();
        let hl = Highlight::default();
        // 40 distinct-ish chars so the visible window can be identified by content.
        let text: String = (0..40u8).map(|i| char::from(b'a' + i % 26)).collect();
        let last = text.chars().last().unwrap();
        // Caret at the END (col 40); only 10 columns fit. The caret claims its own column,
        // so the tail shows with the caret just after it - the caret is never off screen.
        let v = view(
            &text,
            40,
            Selection::at(40),
            InputMode::Shell,
            None,
            None,
            &hl,
        );
        let laid = layout_cells(&v, &theme, 10);
        assert!(
            laid.caret_col < 10,
            "the caret is scrolled into the visible window (col {})",
            laid.caret_col
        );
        assert!(
            laid.cells.iter().all(|c| c.col < 10),
            "clipped to the budget"
        );
        let rightmost = laid.cells.iter().max_by_key(|c| c.col).unwrap();
        assert_eq!(
            rightmost.ch, last,
            "the visible window shows the line's tail, not its head"
        );
        // A mid-line caret stays visible too (clipped on BOTH sides, full budget used).
        let mid = view(
            &text,
            20,
            Selection::at(20),
            InputMode::Shell,
            None,
            None,
            &hl,
        );
        let lm = layout_cells(&mid, &theme, 10);
        assert!(lm.caret_col < 10 && lm.cells.iter().all(|c| c.col < 10));
        assert_eq!(
            lm.cells.len(),
            10,
            "a mid-line caret fills the visible budget"
        );
    }

    #[test]
    fn multiline_clamps_rows_and_keeps_the_caret_line_visible() {
        let theme = dark();
        let hl = Highlight::default();
        // 9 lines "0".."8", caret on the last line.
        let text = "0\n1\n2\n3\n4\n5\n6\n7\n8";
        let caret = text.chars().count();
        let v = view(
            text,
            caret,
            Selection::at(caret),
            InputMode::Shell,
            None,
            None,
            &hl,
        );
        let laid = layout_cells(&v, &theme, 80);
        assert_eq!(laid.rows, MAX_INPUT_ROWS, "rows clamp to the max");
        // The caret line is the bottom shown row.
        assert_eq!(laid.caret_row, MAX_INPUT_ROWS - 1);
        // The bottom shown row carries the last logical line's glyph ('8').
        assert!(laid
            .cells
            .iter()
            .any(|c| c.row == MAX_INPUT_ROWS - 1 && c.ch == '8'));
    }

    #[test]
    fn zone_px_grows_with_lines_and_clamps_at_the_max() {
        let one = zone_px_for("echo hi", 1.0);
        let three = zone_px_for("a\nb\nc", 1.0);
        let many = zone_px_for("a\nb\nc\nd\ne\nf\ng\nh", 1.0);
        assert!(three > one, "more lines is a taller zone");
        let capped = zone_px_for(
            &(0..MAX_INPUT_ROWS + 4).map(|_| "x\n").collect::<String>(),
            1.0,
        );
        assert_eq!(
            capped, many,
            "the zone height clamps at MAX_INPUT_ROWS rows"
        );
        assert_eq!(visible_input_rows("a\nb"), 2);
        assert_eq!(visible_input_rows(""), 1, "empty still reserves one row");
        assert_eq!(visible_input_rows(&"x\n".repeat(20)), MAX_INPUT_ROWS);
    }

    #[test]
    fn signature_is_stable_and_changes_on_each_drawn_axis() {
        let theme = dark();
        let hl = EMPTY_HL;
        let base_v = view("ls", 2, Selection::at(2), InputMode::Shell, None, None, &hl);
        let base = signature(&base_v, 800, 600, 13, &theme);
        assert_eq!(
            base,
            signature(&base_v, 800, 600, 13, &theme),
            "deterministic"
        );
        // Mode.
        assert_ne!(
            base,
            signature(
                &view("ls", 2, Selection::at(2), InputMode::Agent, None, None, &hl),
                800,
                600,
                13,
                &theme
            ),
            "mode"
        );
        // Text.
        assert_ne!(
            base,
            signature(
                &view(
                    "lsx",
                    3,
                    Selection::at(3),
                    InputMode::Shell,
                    None,
                    None,
                    &hl
                ),
                800,
                600,
                13,
                &theme
            ),
            "text"
        );
        // Caret / selection.
        assert_ne!(
            base,
            signature(
                &view("ls", 1, Selection::at(1), InputMode::Shell, None, None, &hl),
                800,
                600,
                13,
                &theme
            ),
            "caret"
        );
        // Ghost.
        assert_ne!(
            base,
            signature(
                &view(
                    "ls",
                    2,
                    Selection::at(2),
                    InputMode::Shell,
                    Some(" -la"),
                    None,
                    &hl
                ),
                800,
                600,
                13,
                &theme
            ),
            "ghost"
        );
        // Viewport + px + theme.
        assert_ne!(base, signature(&base_v, 801, 600, 13, &theme), "width");
        assert_ne!(base, signature(&base_v, 800, 601, 13, &theme), "height");
        assert_ne!(base, signature(&base_v, 800, 600, 26, &theme), "px");
        assert_ne!(
            base,
            signature(&base_v, 800, 600, 13, Theme::for_kind(ThemeKind::Light)),
            "theme"
        );
    }
}

// The widget draws to a real GPU through the shared atlas, so it is verified offscreen and
// read back - macOS-only, skipping when no adapter is present (the same harness as the
// grid/prose/timeline GPU tests). These cover: the prompt glyph + command text + caret ink
// (both themes), the glyph layer is one draw call, and the damage gate early-outs alloc-free.
#[cfg(all(test, target_os = "macos"))]
mod gpu_tests {
    use super::*;
    use aterm_core::{InputEvent, InputModel};
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
            label: Some("aterm-input-test"),
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
        iw: &mut InputWidgetRenderer,
        input: &InputModel,
        theme: &Theme,
        w: u32,
        h: u32,
    ) -> Readback {
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iw-target"),
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
            label: Some("iw-readback"),
            size: (stride as u32 * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        iw.prepare(
            device,
            queue,
            atlas,
            input,
            None,
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
                label: Some("iw-pass"),
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
            iw.draw(&mut pass, atlas);
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

    fn model(text: &str, mode: InputMode) -> InputModel {
        let mut m = InputModel::new();
        if mode == InputMode::Agent {
            m.reduce(InputEvent::ToggleMode);
        }
        m.reduce(InputEvent::Insert(text.to_string()));
        m
    }

    #[test]
    fn input_box_inks_prompt_text_and_caret_in_both_themes() {
        let Some((device, queue, format)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let (cw, ch) = cell_px(SCALE);
        let inset = INSET_LOGICAL * SCALE;
        let (w, h) = (320u32, 160u32);
        // Both themes AND both modes - so the AGENT prompt glyph (the PUA "sparkles" icon)
        // is proven to ink, not just Shell's `❯` (the review's `.notdef` regression was an
        // Agent-only failure the prior Shell-only test missed).
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            for mode in [InputMode::Shell, InputMode::Agent] {
                let theme = *Theme::for_kind(kind);
                let mut atlas = GlyphAtlas::new(&device, format);
                let mut iw = InputWidgetRenderer::new(&device);
                let m = model("echo hi", mode);
                let rb = render(&device, &queue, &mut atlas, &mut iw, &m, &theme, w, h);

                let zone_top = (h as f32 - zone_px_for(m.text(), SCALE)) as u32;
                let hl = (f32::from(space::HAIRLINE_WIDTH) * SCALE).round().max(1.0);
                let pad = f32::from(space::S4) * SCALE;
                let row_y = zone_top + hl as u32 + pad as u32;

                // The prompt glyph inks in the left cell of the text row.
                assert!(
                    rb.any_ink(
                        inset as u32,
                        row_y,
                        (inset + cw) as u32,
                        row_y + ch as u32,
                        40
                    ),
                    "{kind:?}/{mode:?}: the prompt glyph inks"
                );
                // The command text inks in the content region. Threshold 30 (not 40):
                // the input box draws no fill behind itself, so on the black-cleared
                // target the light `fg.primary` ink (#26231B, max channel 38 - the warm
                // ADR-0011 palette) must still register; 30 clears it with margin while
                // staying well above the black clear.
                let tx = (inset + 2.0 * cw) as u32;
                assert!(
                    rb.any_ink(tx, row_y, tx + (7.0 * cw) as u32, row_y + ch as u32, 30),
                    "{kind:?}/{mode:?}: the command text inks"
                );
                // The hairline inks at the zone top, far right (clear of any glyph).
                assert!(
                    rb.any_ink(w - 24, zone_top.saturating_sub(1), w - 6, zone_top + 2, 20),
                    "{kind:?}/{mode:?}: the top hairline inks across the box"
                );
            }
        }
    }

    #[test]
    fn input_glyph_layer_is_a_single_draw_call() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut iw = InputWidgetRenderer::new(&device);
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let m = model("ls -la", InputMode::Agent);
        render(&device, &queue, &mut atlas, &mut iw, &m, &theme, 320, 160);
        assert_eq!(
            iw.last_glyph_draw_calls(),
            1,
            "the whole input glyph layer (text + chip label) is ONE instanced draw"
        );
    }

    #[test]
    fn unchanged_input_skips_rebuild_alloc_free() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut iw = InputWidgetRenderer::new(&device);
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let m = model("git status", InputMode::Shell);
        let size = FrameSize {
            width: 320,
            height: 160,
            scale: SCALE,
        };
        iw.prepare(&device, &queue, &mut atlas, &m, None, &theme, size);
        let allocs = crate::alloc_probe::count_allocs(|| {
            let drew = iw.prepare(&device, &queue, &mut atlas, &m, None, &theme, size);
            std::hint::black_box(drew);
        });
        assert_eq!(
            allocs, 0,
            "an unchanged input frame's prepare early-out allocates nothing (got {allocs})"
        );
    }
}
