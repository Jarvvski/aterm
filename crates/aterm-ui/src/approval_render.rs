//! The risk-gate approval card (ticket T-9.7): the modal affordance drawn over the input
//! while an agent turn is PARKED on a `RequireConfirm` verdict, styled to the vision mock
//! (ADR-0011) `gate` state.
//!
//! Another front-end over the shared [`GlyphAtlas`], anchored just above the input bar like
//! the completion popover ([`crate::completion_render`]). It reads a pure, agent-domain-free
//! [`ApprovalView`] the host projects from the turn's pending approval (the crate arrow: the
//! command is already SANITIZED in `aterm-app`, so no raw secret reaches here) and draws:
//!
//! - the proposed command row: the tool name in `accent.primary` + the argv in `fg.primary`;
//! - a `caution`-bordered card on a `caution_weak` fill with a `△` glyph, a `fg.primary`
//!   title, and a `fg.secondary` reason (the gate's plain-language gloss);
//! - a split **Approve** button (accent fill, white text) with a `▾` that opens a
//!   dropdown ("Approve once" / "Always approve `<pattern>`"), a **Reject** button (hairline
//!   border, `fg.secondary`), and a `fg.faint` keyboard hint.
//!
//! Color is ALWAYS paired with a text label (color-blind safety): the caution card leads
//! with its title, the buttons with their labels, the resolved states (drawn as timeline
//! `Approval` blocks by [`crate::timeline_render`], not here) with `Approved`/`Rejected`.
//!
//! ## Damage gating
//! [`Self::prepare`] keys a rebuild on a cheap FNV signature over everything drawn (the view
//! fields, the dropdown state, the anchor, px, the colors) and early-outs alloc-free when
//! unchanged - so a parked frame holds the 60fps floor (T-1.8). The whole card is ONE rect
//! draw + ONE glyph draw, like every other front-end.

use std::mem::size_of;

use aterm_tokens::{legible_against, space, type_scale, Rgba, Theme};

use crate::atlas::{GlyphAtlas, GlyphInstance, InstanceBuffer, RectInstance};
use crate::cell_render::{emit_cell, CellCtx};
use crate::components::{RiskState, CHIP_MIN_CONTRAST};
use crate::grid_render::{FrameSize, INSET_LOGICAL};
use crate::text::{FontFamily, GridCell};
use crate::window::cell_px;

/// The caution triangle in the card header (the mock's `△`, U+25B3): a Nerd-Font PUA icon
/// (`nf-fa-exclamation-triangle`), present in the bundled Mono face - the BMP `△` is
/// `.notdef` there (like `◇`). Guarded by `card_glyphs_exist_in_the_bundled_grid_font`.
const CAUTION_GLYPH: char = '\u{f071}';
/// The split-Approve dropdown caret (the mock's `▾`): `nf-fa-caret-down`, the sibling of the
/// gutter's already-tested `nf-fa-caret-right` (U+F0DA).
const MENU_CARET: char = '\u{f0d7}';
/// The middle dot separating the hint's key labels (U+00B7, present in the Mono face).
const SEP: char = '\u{00B7}';

/// The proposed-command tool + argv separator gap, and the button/label paddings, all in
/// CELLS (whole cells so everything lands on the Mono grid).
const GAP_CELLS: f32 = 1.0;
/// Horizontal cell padding inside a button (each side).
const BTN_PAD_CELLS: f32 = 1.0;
/// The reason wraps at this many cells so a long gloss stays a tidy column (not the full
/// window width). A tuning default, not a protocol constant.
const REASON_MEASURE: usize = 56;

/// The two dropdown rows (kept in step with the host's `GATE_MENU_LEN`).
const MENU_ITEMS: [&str; 2] = ["Approve once", "Always approve"];

/// A pure, agent-domain-free description of the turn's pending approval for the card (ticket
/// T-9.7). NOT an `aterm_agent` type (the crate arrow forbids it): `aterm-app` projects the
/// parked `ApprovalRequest` onto this, sanitizing the command first. Borrowed strings, so it
/// rides into the frame with no per-frame allocation (like [`crate::title_bar::TitleBarView`]).
#[derive(Debug, Clone, Copy)]
pub struct ApprovalView<'a> {
    /// The gated tool's name (e.g. `run_command`) - drawn in `accent.primary`.
    pub tool: &'a str,
    /// The SANITIZED proposed argument (e.g. `rm -rf ./target`) - drawn in `fg.primary`.
    pub command: &'a str,
    /// The UI risk verdict: `NeedsApproval` (a plain Caution gate) or `Blocked` (a Dangerous
    /// command - the danger-toned `△`). Never `Allowed` (a Safe command auto-runs silently).
    pub risk: RiskState,
    /// The plain-language card title (e.g. "Destructive command - needs your approval").
    pub title: &'a str,
    /// The gate's plain-language reason (the parsed gloss).
    pub reason: &'a str,
    /// The command "family" shown faint beside the "Always approve" menu item (e.g.
    /// `rm -rf ...`).
    pub pattern: &'a str,
    /// Whether the split-Approve dropdown is expanded.
    pub menu_open: bool,
    /// The highlighted dropdown row when [`Self::menu_open`] (`0` = Approve once, `1` =
    /// Always approve).
    pub menu_index: usize,
}

/// The approval-card front-end over the shared [`GlyphAtlas`].
pub struct ApprovalRenderer {
    bg_instances: Vec<RectInstance>,
    glyph_instances: Vec<GlyphInstance>,
    bg_buf: InstanceBuffer,
    glyph_buf: InstanceBuffer,
    built: Option<u64>,
    last_glyph_draw_calls: u32,
}

impl ApprovalRenderer {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            bg_instances: Vec::new(),
            glyph_instances: Vec::new(),
            bg_buf: InstanceBuffer::new(device, "aterm-approval-bg", size_of::<RectInstance>(), 32),
            glyph_buf: InstanceBuffer::new(
                device,
                "aterm-approval-glyph",
                size_of::<GlyphInstance>(),
                512,
            ),
            built: None,
            last_glyph_draw_calls: 0,
        }
    }

    #[must_use]
    pub fn last_glyph_draw_calls(&self) -> u32 {
        self.last_glyph_draw_calls
    }

    /// Build the card for `view` through the shared `atlas`. `input_zone_top` is the top edge
    /// (physical px) of the input bar; the card's bottom sits just above it and grows upward.
    /// Returns `true` if anything drew. Damage-gated + alloc-free when unchanged.
    #[allow(clippy::too_many_arguments)] // by-value frame-path args, like the other front-ends
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        view: &ApprovalView,
        input_zone_top: f32,
        theme: &Theme,
        size: FrameSize,
    ) -> bool {
        let FrameSize {
            width,
            height,
            scale,
            content_left,
        } = size;
        let px = (type_scale::GRID.size_pt * scale).round().max(1.0);
        let px_key = px as u32;

        let sig = fold_content_left(
            signature(view, width, input_zone_top, px_key, theme),
            content_left,
        );
        if self.built == Some(sig) {
            return !self.glyph_instances.is_empty() || !self.bg_instances.is_empty();
        }

        self.bg_instances.clear();
        self.glyph_instances.clear();

        let (cw, ch) = cell_px(scale);
        let left_inset = content_left + INSET_LOGICAL * scale;
        let right_inset = INSET_LOGICAL * scale;
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

        // ---- measure -------------------------------------------------------
        let pad = f32::from(space::S2) * scale; // panel padding
        let ipad = f32::from(space::S2) * scale; // caution-box inner padding
        let gap = f32::from(space::S1) * scale;
        let hairline_h = (f32::from(space::HAIRLINE_WIDTH) * scale).round().max(1.0);

        // The panel content budget in CELLS, derived from the surface (NOT from the content),
        // so every wrapped/measured section is bounded and NOTHING is ever laid out past the
        // clamped panel - the whole card fits the window. `card_budget` is the narrower
        // interior of the caution box (its own `ipad` each side).
        let max_w = (width as f32 - left_inset - right_inset).max(cw);
        let ipad_cells = (ipad / cw).ceil() as usize;
        let budget_cells = (((max_w - 2.0 * pad) / cw).floor() as usize).max(8);
        let card_budget = budget_cells.saturating_sub(2 * ipad_cells).max(8);

        // The reason wraps into a tidy column, never wider than the caution box interior.
        let reason_lines = wrap(view.reason, REASON_MEASURE.min(card_budget));

        // The proposed command: one line beside the tool when it fits, else the tool on its
        // own line with the argv WRAPPED below (never truncated / never off-screen - the argv
        // is the thing the user must read before approving).
        let tool_cells = view.tool.chars().count();
        let command_cells = view.command.chars().count();
        let command_one_line = tool_cells + 1 + command_cells <= budget_cells;
        let command_lines = if command_one_line {
            Vec::new()
        } else {
            wrap(view.command, budget_cells)
        };
        let command_rows = if command_one_line {
            1
        } else {
            1 + command_lines.len()
        };
        let command_cols = if command_one_line {
            tool_cells + 1 + command_cells
        } else {
            tool_cells.max(
                command_lines
                    .iter()
                    .map(|l| l.chars().count())
                    .max()
                    .unwrap_or(0),
            )
        };

        // Button widths (in px): label + 1 cell padding each side; the caret is its own cell.
        let approve_w = (APPROVE_STR.chars().count() as f32 + 2.0 * BTN_PAD_CELLS) * cw;
        let caret_w = (1.0 + 2.0 * BTN_PAD_CELLS) * cw;
        let reject_w = (REJECT_STR.chars().count() as f32 + 2.0 * BTN_PAD_CELLS) * cw;
        let hint = hint_text(view.menu_open);
        let action_w =
            approve_w + caret_w + GAP_CELLS * cw + reject_w + GAP_CELLS * cw + col_px(&hint, cw);

        // Caution-box inner content width (px): the widest of the title / reason / actions,
        // clamped to the box interior so the border never draws past the panel.
        let title_cols = 2 + view.title.chars().count(); // △ + gap + title
        let reason_cols = reason_lines
            .iter()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(0);
        let card_inner_w = (title_cols.max(reason_cols) as f32 * cw)
            .max(action_w)
            .min(card_budget as f32 * cw);
        let card_w = card_inner_w + 2.0 * ipad;

        // Menu popover width (px), when open.
        let menu_w = if view.menu_open {
            let widest = MENU_ITEMS
                .iter()
                .enumerate()
                .map(|(i, label)| menu_row_cols(i, label, view.pattern))
                .max()
                .unwrap_or(0)
                .min(card_budget);
            (widest as f32 + 2.0 * BTN_PAD_CELLS) * cw
        } else {
            0.0
        };

        // Panel content width = widest section; the panel adds `pad` each side. Every section
        // is budget-bounded above, so this never exceeds `max_w` (the `.min` is a belt).
        let content_w = (command_cols as f32 * cw).max(card_w).max(menu_w);
        let box_w = (content_w + 2.0 * pad).min(max_w);

        // Heights (px): command row(s) + gap + caution box (+ menu below when open).
        let card_h = ipad
            + ch // title
            + reason_lines.len() as f32 * ch
            + gap
            + ch // actions
            + ipad;
        let menu_h = if view.menu_open {
            gap + (MENU_ITEMS.len() as f32) * ch + 2.0 * hairline_h
        } else {
            0.0
        };
        let box_h = 2.0 * pad + command_rows as f32 * ch + gap + card_h + menu_h;

        // Anchor: bottom just above the input, grow upward (like the completion popover).
        let box_left = left_inset;
        let gap_above_input = gap + 2.0 * scale;
        let box_bottom = (input_zone_top - gap_above_input).max(box_h);
        let box_top = (box_bottom - box_h).max(0.0);

        // ---- panel + occluding fill ---------------------------------------
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
        let mut y = box_top + pad;

        // ---- proposed command row(s) --------------------------------------
        let after_tool = self.emit_text(
            atlas,
            queue,
            &ctx,
            view.tool,
            content_x,
            y,
            c.accent_primary,
        );
        if command_one_line {
            self.emit_text(
                atlas,
                queue,
                &ctx,
                view.command,
                after_tool + GAP_CELLS * cw,
                y,
                c.fg_primary,
            );
            y += ch;
        } else {
            // The tool sits alone; the wrapped argv follows, indented to the content column.
            y += ch;
            for line in &command_lines {
                self.emit_text(atlas, queue, &ctx, line, content_x, y, c.fg_primary);
                y += ch;
            }
        }
        y += gap;

        // ---- caution card --------------------------------------------------
        let card_top = y;
        // △ tone: danger for a destructive (Blocked) command, else caution.
        let tri_color = if view.risk == RiskState::Blocked {
            c.danger
        } else {
            c.caution
        };
        // Border rect + weak fill.
        self.bg_instances.push(RectInstance {
            rect: [content_x, card_top, card_inner_w + 2.0 * ipad, card_h],
            color: c.caution.to_linear_f32(),
        });
        self.bg_instances.push(RectInstance {
            rect: [
                content_x + hairline_h,
                card_top + hairline_h,
                (card_inner_w + 2.0 * ipad - 2.0 * hairline_h).max(0.0),
                (card_h - 2.0 * hairline_h).max(0.0),
            ],
            color: c.caution_weak.to_linear_f32(),
        });

        let card_x = content_x + ipad;
        let mut cy = card_top + ipad;
        // Title: △ + gap + title text.
        self.emit_cell_at(atlas, queue, &ctx, CAUTION_GLYPH, tri_color, card_x, cy);
        self.emit_text(
            atlas,
            queue,
            &ctx,
            view.title,
            card_x + 2.0 * cw,
            cy,
            c.fg_primary,
        );
        cy += ch;
        // Reason (wrapped), de-emphasized.
        for line in &reason_lines {
            self.emit_text(atlas, queue, &ctx, line, card_x, cy, c.fg_secondary);
            cy += ch;
        }
        cy += gap;

        // ---- action row: Approve | ▾ | Reject | hint ----------------------
        // The Approve button's ink is WHITE on the accent fill (the mock's `#fff`), pulled
        // toward legibility only if it somehow failed the UI contrast floor - it does not
        // (white clears >=3:1 on the accent in both themes), so this stays white (NOT the
        // max-contrast endpoint of an unreachable ratio, which would flip to black on the
        // mid-tone accent). Guarded by `on_accent_is_white_in_both_themes`.
        let on_accent = on_accent_ink(c.accent_primary);
        let action_y = cy;
        // Approve button (accent fill).
        self.bg_instances.push(RectInstance {
            rect: [card_x, action_y, approve_w, ch],
            color: c.accent_primary.to_linear_f32(),
        });
        self.emit_text(
            atlas,
            queue,
            &ctx,
            APPROVE_STR,
            card_x + BTN_PAD_CELLS * cw,
            action_y,
            on_accent,
        );
        // Caret segment (accent fill, slightly separated by a hairline gap).
        let caret_x = card_x + approve_w;
        self.bg_instances.push(RectInstance {
            rect: [caret_x, action_y, caret_w, ch],
            color: c.accent_primary.to_linear_f32(),
        });
        self.emit_cell_at(
            atlas,
            queue,
            &ctx,
            MENU_CARET,
            on_accent,
            caret_x + BTN_PAD_CELLS * cw,
            action_y,
        );
        // Reject button (hairline border, transparent fill = the panel bg_elev).
        let reject_x = caret_x + caret_w + GAP_CELLS * cw;
        self.bg_instances.push(RectInstance {
            rect: [reject_x, action_y, reject_w, ch],
            color: c.hairline.to_linear_f32(),
        });
        self.bg_instances.push(RectInstance {
            rect: [
                reject_x + hairline_h,
                action_y + hairline_h,
                (reject_w - 2.0 * hairline_h).max(0.0),
                (ch - 2.0 * hairline_h).max(0.0),
            ],
            color: c.bg_elev.to_linear_f32(),
        });
        self.emit_text(
            atlas,
            queue,
            &ctx,
            REJECT_STR,
            reject_x + BTN_PAD_CELLS * cw,
            action_y,
            c.fg_secondary,
        );
        // Keyboard hint (faint).
        let hint_x = reject_x + reject_w + GAP_CELLS * cw;
        self.emit_text(atlas, queue, &ctx, &hint, hint_x, action_y, c.fg_faint);

        // ---- dropdown menu (when open) ------------------------------------
        if view.menu_open {
            let menu_top = card_top + card_h + gap;
            let menu_rows_h = (MENU_ITEMS.len() as f32) * ch + 2.0 * hairline_h;
            // Elevated menu panel with a hairline border, hugging the Approve button's left.
            self.bg_instances.push(RectInstance {
                rect: [card_x, menu_top, menu_w, menu_rows_h],
                color: c.hairline.to_linear_f32(),
            });
            self.bg_instances.push(RectInstance {
                rect: [
                    card_x + hairline_h,
                    menu_top + hairline_h,
                    (menu_w - 2.0 * hairline_h).max(0.0),
                    (menu_rows_h - 2.0 * hairline_h).max(0.0),
                ],
                color: c.bg_elev.to_linear_f32(),
            });
            for (i, label) in MENU_ITEMS.iter().enumerate() {
                let row_y = menu_top + hairline_h + i as f32 * ch;
                let active = i == view.menu_index;
                if active {
                    self.bg_instances.push(RectInstance {
                        rect: [
                            card_x + hairline_h,
                            row_y,
                            (menu_w - 2.0 * hairline_h).max(0.0),
                            ch,
                        ],
                        color: c.accent_primary_weak.to_linear_f32(),
                    });
                }
                let item_x = card_x + BTN_PAD_CELLS * cw;
                let fg = if active { c.fg_primary } else { c.fg_secondary };
                let after = self.emit_text(atlas, queue, &ctx, label, item_x, row_y, fg);
                // "Always approve <pattern>" trails the family faint.
                if i == 1 && !view.pattern.is_empty() {
                    self.emit_text(
                        atlas,
                        queue,
                        &ctx,
                        view.pattern,
                        after + cw,
                        row_y,
                        c.fg_faint,
                    );
                }
            }
        }

        // ---- upload --------------------------------------------------------
        if !self.bg_instances.is_empty() {
            self.bg_buf.ensure(
                device,
                "aterm-approval-bg",
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
                "aterm-approval-glyph",
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
        !self.glyph_instances.is_empty() || !self.bg_instances.is_empty()
    }

    /// Emit a run of Mono cells for `text` in `fg` from `(x, y)`, returning the x after it.
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
    ) -> f32 {
        let mut cx = x;
        for ch in text.chars() {
            self.emit_cell_at(atlas, queue, ctx, ch, fg, cx, y);
            cx += ctx.cw;
        }
        cx
    }

    /// Emit one Mono cell (`ch` in `fg`) at `(x, y)`, bg=canvas so `emit_cell` skips the
    /// per-cell background quad (the section fill already provides the surface).
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

    /// Record the card draws into `pass`: the solid layer (panel/border/fills/buttons) then
    /// the one glyph instanced draw.
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

/// The "Approve" button label.
const APPROVE_STR: &str = "Approve";
/// The "Reject" button label.
const REJECT_STR: &str = "Reject";

/// The keyboard hint under the actions: context-sensitive on the dropdown state (ticket
/// T-9.7). Plain words + the middle dot (all present in the Mono face), so the affordance
/// stays legible without a glyph the bundled font might lack (the `⏎` return symbol is
/// `.notdef` there). Color-blind safe (it is a text label).
fn hint_text(menu_open: bool) -> String {
    if menu_open {
        format!("up/down choose {SEP} enter select {SEP} esc back")
    } else {
        format!("enter approve {SEP} tab options {SEP} esc reject")
    }
}

/// The cell count of the `i`th dropdown row: the label, plus (for "Always approve") a gap +
/// the pattern.
fn menu_row_cols(i: usize, label: &str, pattern: &str) -> usize {
    let mut cols = label.chars().count();
    if i == 1 && !pattern.is_empty() {
        cols += 1 + pattern.chars().count();
    }
    cols
}

/// The pixel width of a string in Mono cells.
fn col_px(s: &str, cw: f32) -> f32 {
    s.chars().count() as f32 * cw
}

/// Opaque white - the Approve button's on-accent ink (the mock's `#fff`).
const WHITE: Rgba = Rgba {
    r: 0xFF,
    g: 0xFF,
    b: 0xFF,
    a: 0xFF,
};

/// The ink for text on the accent-filled Approve button (ticket T-9.7): white, pulled toward
/// legibility ONLY if it fails the UI contrast floor on the accent (it does not in either
/// shipped theme, so it stays white). Seeding white and correcting downward - rather than
/// asking for the max-contrast endpoint - is what keeps it white on the mid-tone accent
/// (whose highest-contrast endpoint is actually black).
fn on_accent_ink(accent: Rgba) -> Rgba {
    legible_against(WHITE, accent, CHIP_MIN_CONTRAST)
}

/// Greedy word-wrap `text` into lines of at most `measure` chars (ticket T-9.7). A single
/// word longer than the measure is hard-split so it never overflows the card. Allocates in
/// the rebuild path only (the unchanged-signature early-out never calls it).
fn wrap(text: &str, measure: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        // A single over-long word: hard-split into measure-sized chunks.
        if word.chars().count() > measure {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            let mut chunk = String::new();
            for ch in word.chars() {
                if chunk.chars().count() == measure {
                    lines.push(std::mem::take(&mut chunk));
                }
                chunk.push(ch);
            }
            cur = chunk;
            continue;
        }
        let need = if cur.is_empty() {
            word.chars().count()
        } else {
            cur.chars().count() + 1 + word.chars().count()
        };
        if need > measure {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
        } else {
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// A stable u64 over everything the card draws: the view fields, the dropdown state, the
/// anchor, px, and the colors. Allocation-free (folds borrowed strs + scalars).
fn signature(view: &ApprovalView, w: u32, input_zone_top: f32, px_key: u32, theme: &Theme) -> u64 {
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
    fn risk_code(r: RiskState) -> u64 {
        match r {
            RiskState::Allowed => 0,
            RiskState::NeedsApproval => 1,
            RiskState::Blocked => 2,
        }
    }

    let mut s: u64 = 0xcbf2_9ce4_8422_2325;
    s = fold_str(s, view.tool);
    s = fold_str(s, view.command);
    s = fold_u64(s, risk_code(view.risk));
    s = fold_str(s, view.title);
    s = fold_str(s, view.reason);
    s = fold_str(s, view.pattern);
    s = fold_u64(s, view.menu_open as u64);
    s = fold_u64(s, view.menu_index as u64);
    s = fold_u64(s, u64::from(w));
    s = fold_u64(s, input_zone_top.to_bits() as u64);
    s = fold_u64(s, u64::from(px_key));
    let c = &theme.colors;
    for color in [
        c.bg_canvas,
        c.bg_elev,
        c.fg_primary,
        c.fg_secondary,
        c.fg_faint,
        c.accent_primary,
        c.accent_primary_weak,
        c.hairline,
        c.caution,
        c.caution_weak,
        c.danger,
    ] {
        s = fold_color(s, color);
    }
    s
}

fn fold_content_left(base: u64, content_left: f32) -> u64 {
    (base ^ u64::from(content_left.to_bits()).rotate_left(17)).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_tokens::ThemeKind;

    fn view() -> ApprovalView<'static> {
        ApprovalView {
            tool: "run_command",
            command: "rm -rf ./target",
            risk: RiskState::Blocked,
            title: "Destructive command - needs your approval",
            reason: "This permanently deletes files and can't be undone.",
            pattern: "rm -rf ...",
            menu_open: false,
            menu_index: 0,
        }
    }

    #[test]
    fn card_glyphs_exist_in_the_bundled_grid_font() {
        // The card draws the △ + ▾ + the middle dot through the Mono GRID face; a `.notdef`
        // would be an indistinct box (what the BMP `△`/`▾` are in this face). A cmap lookup
        // of 0 IS `.notdef`, so every card glyph must resolve non-zero. Pure font parse
        // (runs on every platform), like the completion / gutter coverage guards.
        use crate::text::FaceStyle;
        let r = crate::glyph::GlyphRasterizer::new();
        for glyph in [CAUTION_GLYPH, MENU_CARET, SEP] {
            let gid = r.glyph_id(FontFamily::Grid, FaceStyle::Regular, glyph);
            assert_ne!(
                gid, 0,
                "card glyph U+{:04X} is .notdef in the bundled Mono Nerd Font",
                glyph as u32
            );
        }
    }

    #[test]
    fn signature_changes_on_every_draw_affecting_field() {
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let base = signature(&view(), 960, 500.0, 13, &theme);
        assert_eq!(
            base,
            signature(&view(), 960, 500.0, 13, &theme),
            "deterministic"
        );

        // Opening the dropdown, moving the selection, and every text field change re-key it.
        let mut open = view();
        open.menu_open = true;
        assert_ne!(base, signature(&open, 960, 500.0, 13, &theme), "menu open");
        let mut moved = open;
        moved.menu_index = 1;
        assert_ne!(
            signature(&open, 960, 500.0, 13, &theme),
            signature(&moved, 960, 500.0, 13, &theme),
            "menu index"
        );
        let mut cmd = view();
        cmd.command = "rm -rf ./build";
        assert_ne!(base, signature(&cmd, 960, 500.0, 13, &theme), "command");
        let mut risk = view();
        risk.risk = RiskState::NeedsApproval;
        assert_ne!(base, signature(&risk, 960, 500.0, 13, &theme), "risk");
        // Anchor + theme.
        assert_ne!(base, signature(&view(), 960, 480.0, 13, &theme), "anchor");
        assert_ne!(
            base,
            signature(&view(), 960, 500.0, 13, Theme::for_kind(ThemeKind::Light)),
            "theme"
        );
    }

    #[test]
    fn wrap_breaks_at_the_measure_and_hard_splits_a_long_word() {
        // A tidy column: no wrapped line exceeds the measure, and an over-long token is
        // hard-split rather than overflowing the card.
        let lines = wrap("the quick brown fox jumps over the lazy dog", 12);
        assert!(lines.iter().all(|l| l.chars().count() <= 12));
        assert!(lines.len() > 1, "a long reason wraps into multiple lines");
        let long = wrap("supercalifragilisticexpialidocious", 8);
        assert!(long.iter().all(|l| l.chars().count() <= 8));
        // Empty reason yields one (empty) line, never zero (the card still lays out).
        assert_eq!(wrap("", 10), vec![String::new()]);
    }

    #[test]
    fn on_accent_is_white_in_both_themes() {
        // T-9.7 AC1 / mock: the Approve button's text is WHITE on the accent fill in BOTH
        // shipped themes, and clears the UI contrast floor. (Regression guard: asking
        // legible_against for the max-contrast endpoint would flip it to BLACK on the
        // mid-tone accent - this seeds white and only corrects downward if needed.)
        use aterm_tokens::contrast_ratio;
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let accent = Theme::for_kind(kind).colors.accent_primary;
            let ink = on_accent_ink(accent);
            assert_eq!(
                ink, WHITE,
                "{kind:?}: the Approve ink stays white on the accent"
            );
            assert!(
                contrast_ratio(ink, accent) >= CHIP_MIN_CONTRAST,
                "{kind:?}: white-on-accent clears the UI contrast floor"
            );
        }
    }

    #[test]
    fn hint_is_context_sensitive_and_ascii_plus_sep_only() {
        // The hint communicates the keys in words (no `⏎` the bundled font lacks); it differs
        // for the open dropdown. Every char is ASCII or the (present) middle dot.
        let closed = hint_text(false);
        let open = hint_text(true);
        assert_ne!(closed, open);
        assert!(closed.contains("approve") && closed.contains("reject"));
        assert!(open.contains("choose") && open.contains("select"));
        for h in [closed, open] {
            for ch in h.chars() {
                assert!(
                    ch.is_ascii() || ch == SEP,
                    "hint char U+{:04X} must be ASCII or the middle dot",
                    ch as u32
                );
            }
        }
    }
}

// The card draws to a real GPU through the shared atlas, so it is verified offscreen and
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
            label: Some("aterm-approval-test"),
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

    fn pending(menu_open: bool) -> ApprovalView<'static> {
        ApprovalView {
            tool: "run_command",
            command: "rm -rf ./target",
            risk: RiskState::Blocked,
            title: "Destructive command - needs your approval",
            reason: "This permanently deletes files and can't be undone. Safe commands run \
                     on their own; this one is gated.",
            pattern: "rm -rf ...",
            menu_open,
            menu_index: if menu_open { 1 } else { 0 },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        ar: &mut ApprovalRenderer,
        view: &ApprovalView,
        theme: &Theme,
        w: u32,
        h: u32,
    ) -> Readback {
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ar-target"),
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
            label: Some("ar-readback"),
            size: (stride as u32 * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        ar.prepare(
            device,
            queue,
            atlas,
            view,
            h as f32 - 40.0,
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
                label: Some("ar-pass"),
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
            ar.draw(&mut pass, atlas);
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
    fn pending_card_inks_in_both_themes() {
        // AC1 + AC5 (the pending half): the caution card + command + actions render above the
        // input in both themes. The resolved approved/rejected states are timeline `Approval`
        // blocks (covered by the timeline_render GPU test), not this overlay.
        let Some((device, queue, format)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let (w, h) = (520u32, 340u32);
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let theme = *Theme::for_kind(kind);
            let mut atlas = GlyphAtlas::new(&device, format);
            let mut ar = ApprovalRenderer::new(&device);
            let rb = render(
                &device,
                &queue,
                &mut atlas,
                &mut ar,
                &pending(false),
                &theme,
                w,
                h,
            );
            assert!(
                rb.any_ink(8, h - 220, 320, h - 40, 20),
                "{kind:?}: the pending approval card inks above the input"
            );
        }
    }

    #[test]
    fn open_dropdown_inks_the_menu_rows() {
        // The split-Approve dropdown draws its two rows when open (in both themes).
        let Some((device, queue, format)) = device() else {
            return;
        };
        let (w, h) = (520u32, 380u32);
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let theme = *Theme::for_kind(kind);
            let mut atlas = GlyphAtlas::new(&device, format);
            let mut ar = ApprovalRenderer::new(&device);
            let rb = render(
                &device,
                &queue,
                &mut atlas,
                &mut ar,
                &pending(true),
                &theme,
                w,
                h,
            );
            assert!(
                rb.any_ink(8, h - 260, 340, h - 40, 20),
                "{kind:?}: the open dropdown inks its rows"
            );
        }
    }

    #[test]
    fn card_glyph_layer_is_a_single_draw_call() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut ar = ApprovalRenderer::new(&device);
        let theme = *Theme::for_kind(ThemeKind::Dark);
        render(
            &device,
            &queue,
            &mut atlas,
            &mut ar,
            &pending(false),
            &theme,
            520,
            340,
        );
        assert_eq!(ar.last_glyph_draw_calls(), 1, "the card is ONE glyph draw");
    }

    #[test]
    fn unchanged_card_skips_rebuild_alloc_free() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut ar = ApprovalRenderer::new(&device);
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let v = pending(false);
        let size = FrameSize {
            width: 520,
            height: 340,
            scale: SCALE,
            content_left: 0.0,
        };
        ar.prepare(&device, &queue, &mut atlas, &v, 300.0, &theme, size);
        let allocs = crate::alloc_probe::count_allocs(|| {
            let drew = ar.prepare(&device, &queue, &mut atlas, &v, 300.0, &theme, size);
            std::hint::black_box(drew);
        });
        assert_eq!(
            allocs, 0,
            "an unchanged parked frame's prepare early-out allocates nothing (got {allocs})"
        );
    }

    #[test]
    fn approval_card_respects_the_sidebar_content_inset() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut approval = ApprovalRenderer::new(&device);
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let view = pending(false);
        let content_left = 210.0;
        approval.prepare(
            &device,
            &queue,
            &mut atlas,
            &view,
            300.0,
            &theme,
            FrameSize {
                width: 720,
                height: 380,
                scale: SCALE,
                content_left,
            },
        );
        let first_rect_x = approval
            .bg_instances
            .iter()
            .map(|rect| rect.rect[0])
            .reduce(f32::min)
            .expect("the approval card emitted backgrounds");
        assert!(
            first_rect_x >= content_left,
            "approval geometry begins after the sidebar, got x={first_rect_x} for left={content_left}"
        );
    }
}
