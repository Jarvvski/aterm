//! The live block-timeline compositor (ticket T-4.6): the THIRD front-end over the
//! shared [`GlyphAtlas`], after the grid ([`crate::grid_render`]) and prose
//! ([`crate::prose`]).
//!
//! Where the grid draws the raw VT viewport, this draws the Warp-style block timeline:
//! the virtualized [`TimelineLayout`] (ticket T-2.7) turned into on-screen command blocks
//! styled to the vision mock ([`crate::components`], T-9.3) - a command block leads with
//! the accent `❯` prompt glyph in the gutter, the re-rendered command line, a
//! right-aligned block-meta (a [`BlockMetaStyle`] status dot + duration / "exit N"), the
//! captured output rows (dimmed to `fg.secondary`), a single top hairline separator (none
//! above the first block), and the "... +N lines" collapse affordance. (Agent-step rows
//! still draw a left-gutter status marker; the agent transcript re-skin is T-9.6.)
//! Finished blocks draw from their immutable captured `output`
//! ([`aterm_core::RowSnapshot`], byte-replayed at finish), so they are immune to the live
//! grid's scrollback eviction.
//!
//! ## One shaping engine, identical cells
//! Output rows and the command line go through the SAME per-cell emitter the grid uses
//! ([`crate::cell_render::emit_cell`], Mono/`FontFamily::Grid`), so a box-drawing char or
//! a Nerd-Font icon in a finished `git diff` looks pixel-identical to the live grid. The
//! solid layer (block separators, gutter markers, backgrounds) draws through the shared
//! rect pipeline; the whole front-end is one rect draw + one glyph draw.
//!
//! ## Scope (T-4.6)
//! This draws the COMMAND-block timeline (Mono) for BOTH finished and running blocks:
//! the running block carries its LIVE output in the block model (the engine's
//! incremental capture streams it in - `aterm-core` `live_capture`), so a streaming
//! command renders here, not from the grid `Snapshot`. The agent-card Duo prose body and
//! Quattro chrome chips also compose through this same atlas (proven by `crate::prose`
//! and `crate::components`) but are driven by the agent-step data model (T-5.10), so they
//! wire in once that lands.
//!
//! ## Damage gating
//! Like the grid, [`Self::prepare`] keys a full instance rebuild on a cheap signature -
//! over the visible blocks + their drawn CELL CONTENT + scroll + viewport + px + theme -
//! and early-outs (reusing the prior instance buffers, ZERO allocation) when nothing
//! drawn changed. Folding the visible cells (not just `output.len()`) is what catches a
//! running command's in-place `\r` redraw (a progress bar / spinner) where the row count
//! is unchanged but the content is not. The per-frame [`crate::timeline::layout`] call
//! itself allocates, so the live caller ([`crate::gpu`]) gates THAT on a snapshot-version
//! signal to keep the steady-state (idle) present allocation-free.

use std::mem::size_of;

use aterm_tokens::{type_scale, Mode, Theme};

use crate::atlas::{GlyphAtlas, GlyphInstance, InstanceBuffer, RectInstance};
use crate::cell_render::{emit_cell, CellCtx};
use crate::components::{BlockMetaStyle, CommandBlockStyle, RiskBadge, RiskState};
use crate::grid_render::FrameSize;
use crate::text::{resolve_color, resolve_output_color, FontFamily, GridCell};
use crate::timeline::{
    GutterMarker, TimelineLayout, TimelineMode, TimelineRow, VisibleBlock, GAP_ROWS,
};
use crate::window::cell_px;
use aterm_core::{AgentBadge, AgentBlockKind, AgentTextRole, Block};
use aterm_tokens::space;

/// The shell prompt glyph drawn in the timeline gutter for a command block (the mock's
/// accent `❯`, U+276F). Verified present in the bundled Mono Nerd Font by the input
/// widget's `prompt_glyphs_exist_in_the_bundled_grid_font` test (same face).
const SHELL_PROMPT_GLYPH: char = '\u{276F}';

/// The agent turn's header glyph drawn in the gutter (ticket T-9.6): the diamond `◊`
/// (U+25CA LOZENGE) in the agent accent - the SAME substitute the input box uses for
/// the mock's `◇` (U+25C7 WHITE DIAMOND, which is `.notdef` in the bundled faces).
/// Guarded by `agent_glyphs_exist_in_the_bundled_grid_font`.
const AGENT_PROMPT_GLYPH: char = '\u{25CA}';

/// The uppercase "plan" eyebrow above a turn's opening plan prose (ticket T-9.6). ASCII,
/// always present; the mock renders it via `text-transform: uppercase`.
const PLAN_EYEBROW: &str = "PLAN";

/// The resolved-gate status glyphs (ticket T-9.7): a success tick / a danger cross for
/// an approved / rejected `Approval` step. Nerd-Font PUA icons (`nf-fa-check` /
/// `nf-fa-times`), present in the bundled Mono face - the mock's `✓` (U+2713) / `✕`
/// (U+2715) are BMP geometrics absent from it. `nf-fa-check` is already the `Ok` gutter.
const APPROVAL_APPROVED_GLYPH: char = '\u{f00c}';
const APPROVAL_REJECTED_GLYPH: char = '\u{f00d}';

/// The middle-dot separating "exit N" from the duration in the block-meta caption
/// (U+00B7, present in the bundled Mono face - unlike U+2026, so this is safe).
const META_SEP: char = '\u{00B7}';

/// The block-meta caption for a command block: "exit N \u{00b7} 1.23s" on failure,
/// "running" while running, the terse state label ("approx" / "tui"), or just the
/// duration for a plain exit-0 (the mock's `.block-meta` text). Allocates in the
/// rebuild path only (never in the unchanged-signature early-out).
fn meta_caption(meta: &BlockMetaStyle, duration_secs: Option<f64>) -> String {
    let dur = duration_secs.map(|d| format!("{d:.2}s"));
    if let Some(code) = meta.exit_code {
        match &dur {
            Some(d) => format!("exit {code} {META_SEP} {d}"),
            None => format!("exit {code}"),
        }
    } else if let Some(label) = meta.label {
        label.to_string()
    } else if meta.pulsing {
        "running".to_string()
    } else {
        dur.unwrap_or_default()
    }
}

/// Map the agent-domain-free [`AgentBadge`] a tool-call block carries onto the
/// UI-local [`RiskState`] the badge styler speaks (ticket T-5.11). This is the
/// renderer's side of the one-way crate arrow: `aterm-ui` reads the projected
/// `AgentBadge` from the block, never an `aterm-agent` type.
fn risk_state_for(badge: AgentBadge) -> RiskState {
    match badge {
        AgentBadge::Auto => RiskState::Allowed,
        AgentBadge::NeedsApproval => RiskState::NeedsApproval,
        AgentBadge::Blocked => RiskState::Blocked,
    }
}

/// The block-timeline front-end over the shared [`GlyphAtlas`]. Owns its own instance
/// buffers + rebuild gate (so the grid's and prose's buffers are never touched) and
/// draws through the shared rect + glyph pipelines. Constructed once from the device;
/// `prepare` builds instances from a [`TimelineLayout`] and `draw` records the rect +
/// single glyph instanced draws into a caller-owned pass.
pub struct TimelineRenderer {
    bg_instances: Vec<RectInstance>,
    glyph_instances: Vec<GlyphInstance>,
    bg_buf: InstanceBuffer,
    glyph_buf: InstanceBuffer,
    /// Rebuild gate: the signature currently built, or `None`. Covers the visible
    /// blocks, scroll, viewport, px, and the whole theme palette.
    built: Option<u64>,
    /// Glyph-layer draw calls issued by the last [`Self::draw`] (1 when the timeline has
    /// any inked glyph, else 0) - the timeline analogue of the grid's AC-c counter.
    last_glyph_draw_calls: u32,
}

impl TimelineRenderer {
    /// Build the timeline front-end: just its reused CPU/GPU instance buffers. The
    /// shared [`GlyphAtlas`] is owned by [`crate::gpu::GpuRenderer`] and lent per call.
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            bg_instances: Vec::new(),
            glyph_instances: Vec::new(),
            bg_buf: InstanceBuffer::new(
                device,
                "aterm-timeline-bg",
                size_of::<RectInstance>(),
                256,
            ),
            glyph_buf: InstanceBuffer::new(
                device,
                "aterm-timeline-glyph",
                size_of::<GlyphInstance>(),
                256,
            ),
            built: None,
            last_glyph_draw_calls: 0,
        }
    }

    /// Glyph-layer draw calls from the last [`Self::draw`] (1 when there is text).
    #[must_use]
    pub fn last_glyph_draw_calls(&self) -> u32 {
        self.last_glyph_draw_calls
    }

    /// Build the frame's instances from `layout` through the shared `atlas`, reusing the
    /// prior build when the signature is unchanged (the damage gate). Returns `true` if
    /// there is anything to draw. In [`TimelineMode::AltScreen`] (or an empty timeline)
    /// it produces nothing - the grid draws the alt surface full-window.
    ///
    /// The unchanged path allocates nothing (the steady-state early-out). The CHANGED
    /// path reuses its warm `Vec`s + the glyph cache; `queue.write_buffer` is wgpu
    /// staging, not part of that claim.
    // Threads device/queue/atlas/layout/inset/theme/size by value to stay allocation-free
    // on the frame path; bundling them adds a per-frame borrow dance for no clarity gain.
    // Mirrors the grid/input prepare shape (T-9.2 added the `top_inset`, tipping it to 8).
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        layout: &TimelineLayout,
        top_inset: f32,
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

        // Fold the top inset (the reserved title-bar band, T-9.2) into the built signature
        // so a title-bar toggle / DPI change that moves the timeline down forces a rebuild.
        // `signature()` itself stays inset-agnostic (its sig_tests are unchanged); the fold
        // is a separate, unit-tested step (`top_inset_is_a_folded_draw_affecting_axis`).
        let sig = fold_top_inset(signature(layout, width, height, px_key, theme), top_inset);
        if self.built == Some(sig) {
            // Nothing changed: reuse the buffers verbatim (no rebuild, no allocation).
            return !self.glyph_instances.is_empty() || !self.bg_instances.is_empty();
        }

        self.bg_instances.clear();
        self.glyph_instances.clear();

        // Alt-screen / empty timeline: nothing to draw (the grid owns the screen).
        if layout.mode != TimelineMode::Timeline || layout.visible.is_empty() {
            self.built = Some(sig);
            return false;
        }

        let (cw, ch) = cell_px(scale);
        // iA whitespace rhythm (T-4.7): generous canvas margins + intra-block padding,
        // every value from the shared `space` token scale - NOT the grid's tight 8px
        // inset, which stays on `grid_render` for the raw-VT fast path. The timeline
        // lays out below a top breathing band and inside a horizontal gutter on both
        // edges; the inter-block gap (one [`GAP_ROWS`] row of whitespace) is already in
        // the layout coordinate, so here it just renders as an empty band.
        // Top breathing band, offset below any reserved title-bar band (`top_inset`, T-9.2).
        // The matching BOTTOM `space::S12` band is NOT a second offset here - the caller
        // (`gpu::prepare`) already shrank `viewport_rows` by 2*S12 AND subtracted `top_inset`
        // from the effective height, so the last row's bottom lands one S12 above the surface
        // foot. Both bands are one constant; edit them together (see gpu.rs viewport_rows).
        let top_margin = top_inset + f32::from(space::S12) * scale; // title-bar inset + breathing
        let edge = f32::from(space::S8) * scale; // horizontal canvas gutter (both sides)
        let pad = f32::from(space::S4) * scale; // intra-block content padding
        let metrics = atlas.cell_metrics(FontFamily::Grid, px);
        let baseline_off = (ch - metrics.line) * 0.5 + metrics.ascent;
        let cmd = CommandBlockStyle::resolve(theme);
        // The gutter prompt glyph is the shell accent (a command block is always a
        // shell prompt); the mode-tinted AGENT prompt lives in the input box (T-9.4).
        let prompt_color = theme.colors.mode_accent(Mode::Shell);
        let canvas = theme.colors.bg_canvas;
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

        // Geometry shared across blocks (logical tokens scaled to physical px). The
        // inner canvas spans [edge, width - edge]; the boundary hairline spans that full
        // inner width, while command/output text starts after the status-marker band +
        // one `space::S4` of intra-block padding so it never sits flush to the rule.
        let gutter_w = f32::from(cmd.gutter_px) * scale; // status-marker band
        let content_x = edge + gutter_w + pad;
        let inner_w = (width as f32 - 2.0 * edge).max(0.0);
        let hairline_h = (f32::from(space::HAIRLINE_WIDTH) * scale).round().max(1.0);
        let hairline = cmd.hairline.to_linear_f32();
        // The gutter marker is one Mono cell, centered horizontally in the marker band.
        let marker_x = edge + ((gutter_w - cw) * 0.5).max(0.0);

        // Agent-transcript palette (ticket T-9.6): the per-kind token mapping, resolved
        // once from the theme through the pure [`AgentPalette`] (so AC2 - "colors resolve
        // through tokens, no literals" - is asserted by a unit test, not just an any_ink
        // pixel check).
        let pal = AgentPalette::resolve(theme);
        // A tool result's captured output sits in a hairline LEFT-bordered block: the
        // rule at the content column, the text indented one intra-block step past it.
        let result_text_x = content_x + f32::from(space::S3) * scale;

        for (vi, vb) in layout.visible.iter().enumerate() {
            // Exactly ONE muted hairline per interior boundary: centered in the
            // leading-gap whitespace above every block except the first in the list
            // (index 0 has no boundary above it). No top/bottom edge line and no doubled
            // line - the inter-block whitespace is the primary separation, the hairline a
            // faint accent (T-4.7). Drawn only when that gap band is on screen.
            //
            // The hairline is bound to the block BELOW the boundary, so a boundary whose
            // gap is the LAST on-screen row while its lower block is one row off-screen
            // would render no rule. Unreachable today: scroll is pinned-to-bottom
            // (vp_bottom == total_display_rows, so the last row is always content, never a
            // lone trailing gap; the topmost partial block is always in `visible`). When
            // EPIC-3 free scroll lands, drive emission from the boundary above the first
            // off-screen block too, and add a test at a scroll where gapped_top == vp_bottom.
            // Grouping (ticket T-9.6): the boundary hairline separates TURNS, not the
            // steps within one - an agent turn reads as one card. So it draws above
            // command blocks and above the two turn-framing agent steps (the `◊` header
            // and the final summary), but NOT above the plan / tool / result / body
            // steps that sit inside a turn.
            if vb.index > 0
                && vb.top_in_viewport >= GAP_ROWS as i64
                && block_draws_top_hairline(&layout.visible, vi)
            {
                let center_rel = vb.top_in_viewport as f32 - GAP_ROWS as f32 * 0.5;
                let hy = (top_margin + center_rel * ch).round();
                self.bg_instances.push(RectInstance {
                    rect: [edge, hy, inner_w, hairline_h],
                    color: hairline,
                });
            }

            // The variant payloads: a command block's rows draw command/output, an
            // agent step's rows draw its text (ticket T-5.10).
            let command_block = vb.block.as_command();
            let agent_block = vb.block.as_agent();

            // A tool RESULT's output block carries a hairline LEFT border spanning its
            // visible rows (the mock's `border-left`, ticket T-9.6), drawn once here.
            if let Some(ab) = agent_block {
                if ab.kind == AgentBlockKind::ToolResult && !vb.rows.is_empty() {
                    let y0 = top_margin + vb.first_row_in_viewport as f32 * ch;
                    let h = vb.rows.len() as f32 * ch;
                    self.bg_instances.push(RectInstance {
                        rect: [content_x, y0, hairline_h, h],
                        color: hairline,
                    });
                }
            }

            for (k, row) in vb.rows.iter().enumerate() {
                let y = top_margin + (vb.first_row_in_viewport + k as i64) as f32 * ch;
                match row {
                    TimelineRow::Command => {
                        let Some(cb) = command_block else { continue };
                        // The accent `❯` prompt glyph in the gutter (the mock's shell
                        // block; ADR-0011). The status dot + duration move to the
                        // right-aligned block-meta below, so the gutter reads as a
                        // prompt, not a status icon.
                        let prompt = grid_glyph(SHELL_PROMPT_GLYPH, prompt_color, canvas);
                        emit_cell(
                            atlas,
                            queue,
                            &prompt,
                            (marker_x, y),
                            &ctx,
                            &mut self.bg_instances,
                            &mut self.glyph_instances,
                        );
                        // The re-rendered command line (Mono fg.primary).
                        let cmd_cols = cb.command.chars().count();
                        let mut x = content_x;
                        for c in cb.command.chars() {
                            let cell = grid_glyph(c, cmd.command_fg, canvas);
                            emit_cell(
                                atlas,
                                queue,
                                &cell,
                                (x, y),
                                &ctx,
                                &mut self.bg_instances,
                                &mut self.glyph_instances,
                            );
                            x += cw;
                        }
                        // Right-aligned block-meta: a status dot + duration / "exit N"
                        // caption, in the faint meta tone (the mock's `.block-meta`; the
                        // dot color/shape + the caption label carry the state, color is
                        // never the only signal). Drawn only when it clears the command
                        // text - a very narrow block hides it, matching the meta's
                        // hover-revealed intent (hover-gating itself is a follow-up:
                        // the reveal reuses the FocusDim slot, `BlockMetaStyle`).
                        let meta = BlockMetaStyle::resolve(vb.gutter, cb.duration_secs(), theme);
                        let caption = meta_caption(&meta, cb.duration_secs());
                        let meta_cols = 2 + caption.chars().count(); // dot + gap + caption
                        let meta_start = edge + inner_w - meta_cols as f32 * cw;
                        let command_end = content_x + cmd_cols as f32 * cw;
                        if meta_start >= command_end + cw {
                            let dot = grid_glyph(meta.dot_glyph, meta.dot_color, canvas);
                            emit_cell(
                                atlas,
                                queue,
                                &dot,
                                (meta_start, y),
                                &ctx,
                                &mut self.bg_instances,
                                &mut self.glyph_instances,
                            );
                            let mut mx = meta_start + 2.0 * cw; // dot + one-cell gap
                            for c in caption.chars() {
                                let cell = grid_glyph(c, meta.text_color, canvas);
                                emit_cell(
                                    atlas,
                                    queue,
                                    &cell,
                                    (mx, y),
                                    &ctx,
                                    &mut self.bg_instances,
                                    &mut self.glyph_instances,
                                );
                                mx += cw;
                            }
                        }
                    }
                    TimelineRow::Output(i) => {
                        if let Some(snap_row) = command_block.and_then(|c| c.output.get(*i)) {
                            for (col, sc) in snap_row.cells.iter().enumerate() {
                                if sc.wide_spacer {
                                    continue;
                                }
                                // Default (uncolored) output dims to fg.secondary (the
                                // mock's ink-dim body); explicit ANSI/RGB is preserved.
                                let mut fg = resolve_output_color(sc.fg, theme);
                                let mut bg = resolve_color(sc.bg, theme, false);
                                if sc.inverse {
                                    std::mem::swap(&mut fg, &mut bg);
                                }
                                let cell = GridCell {
                                    col: col as u16,
                                    row: 0,
                                    ch: sc.c,
                                    fg,
                                    bg,
                                    bold: sc.bold,
                                    italic: sc.italic,
                                    underline: sc.underline,
                                    wide: sc.wide,
                                };
                                emit_cell(
                                    atlas,
                                    queue,
                                    &cell,
                                    (content_x + col as f32 * cw, y),
                                    &ctx,
                                    &mut self.bg_instances,
                                    &mut self.glyph_instances,
                                );
                            }
                        }
                    }
                    TimelineRow::CollapseAffordance { hidden } => {
                        // "... +N lines" in fg.muted (ASCII ellipsis - always present in
                        // Mono, unlike U+2026).
                        let text = format!("... +{hidden} lines");
                        let mut x = content_x;
                        for c in text.chars() {
                            let cell = grid_glyph(c, cmd.caption_fg, canvas);
                            emit_cell(
                                atlas,
                                queue,
                                &cell,
                                (x, y),
                                &ctx,
                                &mut self.bg_instances,
                                &mut self.glyph_instances,
                            );
                            x += cw;
                        }
                    }
                    TimelineRow::Agent(line) => {
                        // The agent-transcript re-skin (ticket T-9.6): each step is
                        // styled by KIND to the vision mock's `agent` state via the token
                        // `pal`ette. `emit_run` draws one Mono run and returns the x after
                        // it, so a row can sequence name -> arg -> badge on one baseline.
                        // Turn grouping (no inter-step rule) is `block_draws_top_hairline`.
                        let Some(ab) = agent_block else { continue };
                        let line = *line;
                        let text_line = ab.text.split('\n').nth(line).unwrap_or("");
                        let bg = &mut self.bg_instances;
                        let gl = &mut self.glyph_instances;
                        match ab.kind {
                            AgentBlockKind::UserPrompt => {
                                // Turn header: agent-accent `◊` gutter glyph, the request
                                // in fg.primary, and a right-aligned "agent - N steps" meta.
                                if line == 0 {
                                    let glyph = grid_glyph(AGENT_PROMPT_GLYPH, pal.header, canvas);
                                    emit_cell(atlas, queue, &glyph, (marker_x, y), &ctx, bg, gl);
                                }
                                emit_run(
                                    atlas,
                                    queue,
                                    &ctx,
                                    text_line,
                                    pal.emphasis,
                                    canvas,
                                    content_x,
                                    y,
                                    bg,
                                    gl,
                                );
                                if line == 0 {
                                    let steps = turn_step_count(&layout.visible, vi);
                                    let meta = if steps > 0 {
                                        format!("agent {META_SEP} {steps} steps")
                                    } else {
                                        "agent".to_string()
                                    };
                                    let mx = edge + inner_w - meta.chars().count() as f32 * cw;
                                    if mx > content_x + text_line.chars().count() as f32 * cw {
                                        emit_run(
                                            atlas, queue, &ctx, &meta, pal.faint, canvas, mx, y,
                                            bg, gl,
                                        );
                                    }
                                }
                            }
                            AgentBlockKind::Thinking => {
                                // Model thinking: quiet faint prose (the mock keeps it low).
                                emit_run(
                                    atlas, queue, &ctx, text_line, pal.faint, canvas, content_x, y,
                                    bg, gl,
                                );
                            }
                            AgentBlockKind::AssistantText => {
                                // Only the turn's FINAL assistant prose is the summary
                                // (emphasized + framed by a rule); the projector marks
                                // every post-tool paragraph `Summary` since it cannot know
                                // at stream time which is last, so the renderer picks the
                                // true final one by lookahead (review fix). A plan carries
                                // an uppercase eyebrow in the grouped gap above it.
                                let final_summary = is_final_summary(&layout.visible, vi);
                                if line == 0 && ab.text_role == AgentTextRole::Plan {
                                    let ey = y - ch;
                                    if ey >= top_margin - 0.5 {
                                        emit_run(
                                            atlas,
                                            queue,
                                            &ctx,
                                            PLAN_EYEBROW,
                                            pal.faint,
                                            canvas,
                                            content_x,
                                            ey,
                                            bg,
                                            gl,
                                        );
                                    }
                                }
                                let color = if final_summary {
                                    pal.emphasis
                                } else {
                                    pal.prose
                                };
                                emit_run(
                                    atlas, queue, &ctx, text_line, color, canvas, content_x, y, bg,
                                    gl,
                                );
                            }
                            AgentBlockKind::ToolCall => {
                                // "name (accent)  arg (faint)" + a right-aligned "+A -M"
                                // for an edit. Tool calls are one line.
                                if line == 0 {
                                    let mut x = content_x;
                                    if let Some(name) = &ab.tool_name {
                                        x = emit_run(
                                            atlas,
                                            queue,
                                            &ctx,
                                            name,
                                            pal.tool_name,
                                            canvas,
                                            x,
                                            y,
                                            bg,
                                            gl,
                                        );
                                        x += cw; // gap
                                        if let Some(arg) = &ab.tool_arg {
                                            x = emit_run(
                                                atlas, queue, &ctx, arg, pal.faint, canvas, x, y,
                                                bg, gl,
                                            );
                                        }
                                    } else {
                                        // Unparseable call: fall back to the gloss text.
                                        x = emit_run(
                                            atlas, queue, &ctx, text_line, pal.prose, canvas, x, y,
                                            bg, gl,
                                        );
                                    }
                                    // A gated (non-auto) call keeps a small inline verdict
                                    // badge - the color-blind-safe safety signal; auto
                                    // calls stay clean (matching the mock). Advance `x`
                                    // PAST the badge so the right-aligned edit stats below
                                    // reserve room for it too (review fix).
                                    if let Some(badge) = ab.badge {
                                        if badge != AgentBadge::Auto {
                                            let rb =
                                                RiskBadge::resolve(risk_state_for(badge), theme);
                                            x = emit_run(
                                                atlas,
                                                queue,
                                                &ctx,
                                                rb.label,
                                                rb.gutter_color,
                                                canvas,
                                                x + cw,
                                                y,
                                                bg,
                                                gl,
                                            );
                                        }
                                    }
                                    // edit_file "+A -M", right-aligned (success / danger),
                                    // only when it clears the name/arg/badge run.
                                    if let Some((added, removed)) = ab.edit_stats {
                                        let plus = format!("+{added}");
                                        let minus = format!("-{removed}");
                                        let total =
                                            plus.chars().count() + 1 + minus.chars().count();
                                        let sx = edge + inner_w - total as f32 * cw;
                                        if sx > x + cw {
                                            let after = emit_run(
                                                atlas, queue, &ctx, &plus, pal.add, canvas, sx, y,
                                                bg, gl,
                                            );
                                            emit_run(
                                                atlas,
                                                queue,
                                                &ctx,
                                                &minus,
                                                pal.remove,
                                                canvas,
                                                after + cw,
                                                y,
                                                bg,
                                                gl,
                                            );
                                        }
                                    }
                                }
                            }
                            AgentBlockKind::ToolResult => {
                                // Captured output in the left-bordered block: faint text,
                                // `+`/`-` diff lines success/danger, FAILED/ok colored.
                                let color =
                                    diff_line_color(text_line, pal.faint, pal.add, pal.remove);
                                emit_run(
                                    atlas,
                                    queue,
                                    &ctx,
                                    text_line,
                                    color,
                                    canvas,
                                    result_text_x,
                                    y,
                                    bg,
                                    gl,
                                );
                            }
                            AgentBlockKind::Approval => {
                                // Resolved gate line (ticket T-9.7): a success ✓ / danger ✕
                                // status glyph + the resolution text, color always paired
                                // with the leading text label (color-blind safety).
                                let mut x = content_x;
                                if line == 0 {
                                    let (glyph, gcolor) = if ab.is_error {
                                        (APPROVAL_REJECTED_GLYPH, pal.remove)
                                    } else {
                                        (APPROVAL_APPROVED_GLYPH, pal.add)
                                    };
                                    let g = grid_glyph(glyph, gcolor, canvas);
                                    emit_cell(atlas, queue, &g, (x, y), &ctx, bg, gl);
                                    x += 2.0 * cw;
                                }
                                emit_run(
                                    atlas, queue, &ctx, text_line, pal.faint, canvas, x, y, bg, gl,
                                );
                            }
                        }
                    }
                }
            }
        }

        // Upload instances (grow only when the counts exceed capacity).
        if !self.bg_instances.is_empty() {
            self.bg_buf.ensure(
                device,
                "aterm-timeline-bg",
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
                "aterm-timeline-glyph",
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

    /// Record the timeline draws into `pass` (begun with the canvas clear) through the
    /// shared `atlas`: the solid layer (separators, gutter markers, cell backgrounds)
    /// first, then the single glyph instanced draw. EXACTLY ONE glyph draw call.
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

/// A single Mono cell carrying glyph `ch` in `fg` on the canvas background (so the
/// background quad is skipped by [`emit_cell`]). The command line, gutter marker,
/// exit-code tag, and collapse affordance are all built from these.
fn grid_glyph(ch: char, fg: aterm_tokens::Rgba, canvas: aterm_tokens::Rgba) -> GridCell {
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

/// Draw one Mono run of `text` in `fg` starting at `(x0, y)` through the shared cell
/// emitter, returning the x AFTER the last cell (ticket T-9.6) - so an agent row can
/// sequence name -> arg -> badge on one baseline without recomputing widths. One cell per
/// `char`, exactly like the command/output paths (no per-frame allocation).
#[allow(clippy::too_many_arguments)]
fn emit_run(
    atlas: &mut GlyphAtlas,
    queue: &wgpu::Queue,
    ctx: &CellCtx,
    text: &str,
    fg: aterm_tokens::Rgba,
    canvas: aterm_tokens::Rgba,
    x0: f32,
    y: f32,
    bg: &mut Vec<RectInstance>,
    glyph: &mut Vec<GlyphInstance>,
) -> f32 {
    let mut x = x0;
    for c in text.chars() {
        let cell = grid_glyph(c, fg, canvas);
        emit_cell(atlas, queue, &cell, (x, y), ctx, bg, glyph);
        x += ctx.cw;
    }
    x
}

/// The per-kind token palette for the agent-transcript re-skin (ticket T-9.6), resolved
/// once per frame from the theme. Pulling the color choices out of the render arm makes
/// AC2 ("colors resolve through tokens - agent = `accent.agent`, tool name =
/// `accent.primary`; no literals") a pure, noise-immune unit test instead of a weak
/// any_ink pixel check. Every field is a token, never a literal.
#[derive(Debug, Clone, Copy)]
struct AgentPalette {
    /// The `◊` turn-header glyph - the agent mode accent (purple).
    header: aterm_tokens::Rgba,
    /// A tool call's name - the primary accent (blue).
    tool_name: aterm_tokens::Rgba,
    /// The header request + the turn's final summary - `fg.primary` emphasis.
    emphasis: aterm_tokens::Rgba,
    /// Plan / mid-turn body prose + the unparseable-call fallback - `fg.secondary`.
    prose: aterm_tokens::Rgba,
    /// Args, the step meta, thinking, the plan eyebrow, output body - `fg.muted`.
    faint: aterm_tokens::Rgba,
    /// A `+` diff line / "+A" edit count / the approved `✓` - `success`.
    add: aterm_tokens::Rgba,
    /// A `-` diff line / "-M" edit count / the rejected `✕` - `danger`.
    remove: aterm_tokens::Rgba,
}

impl AgentPalette {
    #[must_use]
    fn resolve(theme: &Theme) -> Self {
        let c = &theme.colors;
        Self {
            header: c.mode_accent(Mode::Agent),
            tool_name: c.accent_primary,
            emphasis: c.fg_primary,
            prose: c.fg_secondary,
            faint: c.fg_muted,
            add: c.success,
            remove: c.danger,
        }
    }
}

/// Whether a top boundary hairline draws above the visible block at `vi` (ticket T-9.6).
/// It separates TURNS, not the steps inside one: a command block and the two turn-framing
/// agent steps (the `◊` [`UserPrompt`](AgentBlockKind::UserPrompt) header and the turn's
/// FINAL summary) get a rule; the plan / tool / result / mid-turn-body steps within a turn
/// are grouped under the header with no rule between them. The "final summary" test needs
/// turn context (a mid-turn reflection must NOT be framed), so it takes the visible slice.
fn block_draws_top_hairline(visible: &[VisibleBlock], vi: usize) -> bool {
    match visible[vi].block {
        Block::Command(_) => true,
        Block::Agent(a) => a.kind == AgentBlockKind::UserPrompt || is_final_summary(visible, vi),
    }
}

/// Whether the visible block at `vi` is its turn's FINAL summary (ticket T-9.6, review
/// fix): an [`AssistantText`](AgentBlockKind::AssistantText) whose projector role is
/// [`Summary`](AgentTextRole::Summary) AND which is the last agent step before the next
/// turn / command / end. The projector marks EVERY post-tool paragraph `Summary` (it
/// cannot know at stream time which is last), so the renderer disambiguates here: a
/// mid-turn reflection stays quiet body prose with no rule, only the true final summary
/// gets the hairline + `fg.primary` emphasis. Steps are contiguous in append order, so
/// the block immediately after `vi` decides it - O(1). Known limit (shared with
/// [`turn_step_count`]): if the summary is the last VISIBLE block but more steps are
/// scrolled below, it reads as final - acceptable, since scroll pins to the newest.
fn is_final_summary(visible: &[VisibleBlock], vi: usize) -> bool {
    let Block::Agent(a) = visible[vi].block else {
        return false;
    };
    if a.kind != AgentBlockKind::AssistantText || a.text_role != AgentTextRole::Summary {
        return false;
    }
    match visible.get(vi + 1) {
        None => true, // nothing after -> the turn's last step
        Some(next) => match next.block {
            Block::Command(_) => true, // a human command ends the turn
            // Another agent step in this turn means this is not the last; the next turn's
            // header (UserPrompt) means it was.
            Block::Agent(n) => n.kind == AgentBlockKind::UserPrompt,
        },
    }
}

/// The tool-call step count for a turn whose `◊` header is the visible block at index
/// `header_vi` (ticket T-9.6): the number of [`ToolCall`](AgentBlockKind::ToolCall) steps
/// among the VISIBLE blocks between it and the next turn / command block. O(steps in this
/// turn), so the whole pass stays O(visible-rows). Known limit: a tool call scrolled BELOW
/// the viewport is not counted (the header is the turn's first block, so steps above it
/// never exist; only the tail can be clipped) - a slight undercount on a very long turn,
/// documented rather than paid for with an O(n) full-list scan.
fn turn_step_count(visible: &[VisibleBlock], header_vi: usize) -> u32 {
    let mut n = 0u32;
    for vb in &visible[header_vi + 1..] {
        match vb.block {
            Block::Agent(a) => match a.kind {
                AgentBlockKind::ToolCall => n += 1,
                AgentBlockKind::UserPrompt => break, // the next turn begins
                _ => {}
            },
            Block::Command(_) => break, // a human command ends the turn
        }
    }
    n
}

/// The color for one tool-result output line (ticket T-9.6): a unified-diff `+` line is
/// `success`, a `-` line is `danger`; a failing test line ("FAILED") is `danger` and a
/// passing one ("test result: ok") is `success`; everything else stays `base` (faint).
/// A whole-line classifier (not per-token), so it is cheap and never mis-highlights a
/// substring; the leading whitespace is ignored so an indented diff still colors.
fn diff_line_color(
    line: &str,
    base: aterm_tokens::Rgba,
    success: aterm_tokens::Rgba,
    danger: aterm_tokens::Rgba,
) -> aterm_tokens::Rgba {
    let t = line.trim_start();
    if t.starts_with('+') || t.contains("test result: ok") {
        success
    } else if t.starts_with('-') || t.contains("FAILED") {
        danger
    } else {
        base
    }
}

/// Fold the reserved top-band inset (ticket T-9.2) into a base signature, so a title-bar
/// toggle / DPI change that shifts the timeline down forces a rebuild. Kept separate from
/// [`signature`] (which stays inset-agnostic) and unit-tested directly. `0.0` (the hidden /
/// alt-screen case) folds nothing; distinct nonzero insets yield distinct results because
/// `wrapping_mul` by an odd constant is a bijection on `u64`.
fn fold_top_inset(base: u64, top_inset: f32) -> u64 {
    base ^ (top_inset.to_bits() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

/// A stable u64 over everything the timeline draw reads: the mode + scroll geometry,
/// the viewport, the px, the whole theme palette, and each visible block's draw-affecting
/// facts (placement, gutter state, command length, captured output length, lifecycle).
/// Computed every frame BEFORE the rebuild gate, so it must not allocate - it folds over
/// the already-built `layout.visible` and reads lengths only.
fn signature(layout: &TimelineLayout, w: u32, h: u32, px_key: u32, theme: &Theme) -> u64 {
    fn fold_u64(h: u64, v: u64) -> u64 {
        (h ^ v).wrapping_mul(0x0000_0100_0000_01b3)
    }
    fn fold_color(h: u64, c: aterm_tokens::Rgba) -> u64 {
        fold_u64(h, u64::from(c.to_u32()))
    }
    fn gutter_code(g: GutterMarker) -> u64 {
        match g {
            GutterMarker::Running => 1,
            GutterMarker::Ok => 2,
            GutterMarker::Failed(c) => 3 ^ ((c as i64 as u64) << 8),
            GutterMarker::Unknown => 4,
            GutterMarker::Interactive => 5,
            GutterMarker::Approximate => 6,
            GutterMarker::Agent => 7,
        }
    }

    let mut s: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
    s = fold_u64(s, matches!(layout.mode, TimelineMode::Timeline) as u64);
    s = fold_u64(s, layout.total_rows);
    s = fold_u64(s, layout.scroll.offset_rows);
    s = fold_u64(s, u64::from(w));
    s = fold_u64(s, u64::from(h));
    s = fold_u64(s, u64::from(px_key));

    // The whole semantic palette + the 16 ANSI colors, so ANY theme change (chrome or
    // output color) invalidates the build - a superset of what is actually read, which
    // is always safe (it can only force an extra, correct rebuild, never keep stale
    // colors).
    let c = &theme.colors;
    for color in [
        c.bg_canvas,
        c.bg_surface,
        c.bg_surface_alt,
        c.bg_elev,
        c.fg_primary,
        c.fg_secondary,
        c.fg_muted,
        c.fg_faint,
        c.accent_primary,
        c.accent_agent,
        c.accent_primary_text,
        c.accent_primary_weak,
        c.hairline,
        c.hairline_strong,
        c.selection_bg,
        c.success,
        c.caution,
        c.caution_weak,
        c.danger,
        c.info,
    ] {
        s = fold_color(s, color);
    }
    for i in 0..16u8 {
        s = fold_color(s, theme.ansi.by_index(i));
    }

    /// Fold one snapshot cell's drawn facts (glyph + colors + attribute flags).
    fn fold_cell(h: u64, c: &aterm_core::SnapshotCell) -> u64 {
        let color = |x: aterm_core::CellColor| -> u64 {
            match x {
                aterm_core::CellColor::Named(n) => (1 << 33) | u64::from(n),
                aterm_core::CellColor::Indexed(i) => (2 << 33) | u64::from(i),
                aterm_core::CellColor::Rgb(r, g, b) => {
                    (3 << 33) | (u64::from(r) << 16) | (u64::from(g) << 8) | u64::from(b)
                }
            }
        };
        let flags = u64::from(c.bold)
            | u64::from(c.italic) << 1
            | u64::from(c.underline) << 2
            | u64::from(c.inverse) << 3
            | u64::from(c.wide) << 4
            | u64::from(c.wide_spacer) << 5;
        let mut h = fold_u64(h, c.c as u64);
        h = fold_u64(h, color(c.fg));
        h = fold_u64(h, color(c.bg));
        fold_u64(h, flags)
    }

    s = fold_u64(s, layout.visible.len() as u64);
    for vb in &layout.visible {
        s = fold_u64(s, vb.index as u64);
        s = fold_u64(s, vb.top_in_viewport as u64);
        s = fold_u64(s, vb.first_row_in_viewport as u64);
        s = fold_u64(s, vb.display_height);
        s = fold_u64(s, vb.rows.len() as u64);
        s = fold_u64(s, gutter_code(vb.gutter));
        let command_block = vb.block.as_command();
        let agent_block = vb.block.as_agent();
        s = fold_u64(s, command_block.map_or(0, |c| c.output.len() as u64));
        s = fold_u64(
            s,
            command_block
                .and_then(|c| c.exit_code)
                .map_or(u64::MAX, |c| c as i64 as u64),
        );
        // The finished duration drives the block-meta caption + the faint/success dot
        // split (ticket T-9.3); fold it so a block gaining a duration (or crossing the
        // success threshold) redraws. Fixed once finished, so it stays frame-stable.
        s = fold_u64(
            s,
            command_block
                .and_then(|c| c.duration_secs())
                .map_or(0, |d| (d * 1000.0) as u64),
        );
        s = fold_u64(s, vb.block.is_running() as u64);
        // An agent step's version (ticket T-5.10): a streamed text delta bumps ONLY
        // this entry's version, so folding it invalidates the gate for exactly this
        // entry and nothing else - no full-timeline relayout per delta (the 60fps
        // floor, ties to T-2.7 / T-1.8).
        s = fold_u64(s, agent_block.map_or(0, |a| a.version));
        // The risk-gate badge verdict (ticket T-5.11): a transition (e.g. an
        // approval flipping NeedsApproval -> Auto, or a re-projection to Blocked)
        // changes the drawn chip without necessarily bumping `version`, so fold it
        // so exactly this entry redraws on a verdict change.
        s = fold_u64(
            s,
            match agent_block.and_then(|a| a.badge) {
                None => 0,
                Some(AgentBadge::Auto) => 1,
                Some(AgentBadge::NeedsApproval) => 2,
                Some(AgentBadge::Blocked) => 3,
            },
        );
        // The T-9.6 display fields drive the re-skin but do NOT live in `text` (the
        // tool row draws name+arg, not the gloss) and are set once at push (version
        // stays 0), so fold them explicitly or a change would not redraw: the tool
        // name + sanitized arg, the edit "+A -M" counts, and the assistant-prose role
        // (plan eyebrow / summary emphasis).
        if let Some(a) = agent_block {
            // The kind selects the ENTIRE agent draw (header vs tool row vs result vs
            // approval, the left-border gate, the grouping/step-count lookahead), so it is
            // the primary draw-affecting axis - fold it explicitly (the shared gutter code
            // collapses every agent kind to one value, so it would not otherwise redraw).
            s = fold_u64(
                s,
                match a.kind {
                    AgentBlockKind::UserPrompt => 1,
                    AgentBlockKind::Thinking => 2,
                    AgentBlockKind::AssistantText => 3,
                    AgentBlockKind::ToolCall => 4,
                    AgentBlockKind::ToolResult => 5,
                    AgentBlockKind::Approval => 6,
                },
            );
            if let Some(name) = &a.tool_name {
                s = fold_u64(s, name.len() as u64);
                for ch in name.chars() {
                    s = fold_u64(s, ch as u64);
                }
            }
            if let Some(arg) = &a.tool_arg {
                s = fold_u64(s, arg.len() as u64);
                for ch in arg.chars() {
                    s = fold_u64(s, ch as u64);
                }
            }
            s = fold_u64(
                s,
                match a.edit_stats {
                    None => 0,
                    Some((added, removed)) => {
                        1 ^ (u64::from(added) << 8) ^ (u64::from(removed) << 40)
                    }
                },
            );
            s = fold_u64(
                s,
                match a.text_role {
                    aterm_core::AgentTextRole::Body => 0,
                    aterm_core::AgentTextRole::Plan => 1,
                    aterm_core::AgentTextRole::Summary => 2,
                },
            );
            s = fold_u64(s, a.is_error as u64);
        }
        // Fold the DRAWN CONTENT of each visible row - bounded by the visible rows
        // (~viewport), so it stays cheap - so an in-place redraw (a running command's
        // `\r` progress bar / spinner: row count unchanged, content changed) and a
        // tail-shift both invalidate the gate. Without this the running block would
        // freeze at its first captured value (the review's MAJOR-1 bug).
        for row in &vb.rows {
            match row {
                TimelineRow::Command => {
                    let cmd = command_block.map(|c| c.command.as_str()).unwrap_or("");
                    s = fold_u64(s, cmd.len() as u64);
                    for ch in cmd.chars() {
                        s = fold_u64(s, ch as u64);
                    }
                }
                TimelineRow::Output(i) => match command_block.and_then(|c| c.output.get(*i)) {
                    Some(r) => {
                        s = fold_u64(s, r.cells.len() as u64);
                        for cell in &r.cells {
                            s = fold_cell(s, cell);
                        }
                    }
                    None => s = fold_u64(s, u64::MAX),
                },
                TimelineRow::CollapseAffordance { hidden } => {
                    s = fold_u64(s, *hidden);
                }
                TimelineRow::Agent(line) => {
                    // Fold this text line's content (the version above already moves on
                    // any delta, but folding the drawn text keeps the gate exact even if
                    // a future mutation does not bump the version).
                    let text = agent_block
                        .and_then(|a| a.text.split('\n').nth(*line))
                        .unwrap_or("");
                    s = fold_u64(s, text.len() as u64);
                    for ch in text.chars() {
                        s = fold_u64(s, ch as u64);
                    }
                }
            }
        }
    }
    s
}

/// Build a `BlockList` of one finished block (exit `exit`) whose captured output is
/// `out_rows` rows, each a single visible 'X' cell - the public segmenter path, then
/// `set_block_output` (exactly how the model thread populates a finished block).
#[cfg(test)]
fn block_with_output(out_rows: usize, exit: Option<i32>) -> aterm_core::BlockList {
    block_with_output_char(out_rows, exit, 'X')
}

/// As [`block_with_output`] but with a chosen output glyph, so a test can vary the
/// drawn CONTENT while holding the structure (row count / state) fixed.
#[cfg(test)]
fn block_with_output_char(out_rows: usize, exit: Option<i32>, ch: char) -> aterm_core::BlockList {
    use aterm_core::{BlockSegmenter, CellColor, Mark, PromptKind, RowSnapshot, SnapshotCell};
    let mut list = aterm_core::BlockList::new();
    let mut seg = BlockSegmenter::new();
    seg.apply(&Mark::Prompt(PromptKind::PromptStart), 0, &mut list);
    seg.apply(&Mark::Prompt(PromptKind::OutputStart), 1, &mut list);
    seg.apply(
        &Mark::Prompt(PromptKind::CommandDone { exit_code: exit }),
        3,
        &mut list,
    );
    let rows: Vec<RowSnapshot> = (0..out_rows)
        .map(|_| {
            RowSnapshot::new(vec![SnapshotCell {
                c: ch,
                fg: CellColor::Rgb(255, 255, 255),
                bg: CellColor::Named(257), // canvas -> bg quad skipped
                ..Default::default()
            }])
        })
        .collect();
    list.set_block_output(0, rows);
    list
}

// Pure (no-GPU) tests of the damage-gate signature - run on every platform.
#[cfg(test)]
mod sig_tests {
    use super::*;
    use crate::timeline::{layout, Scroll};
    use aterm_tokens::ThemeKind;

    fn dark() -> Theme {
        *Theme::for_kind(ThemeKind::Dark)
    }

    #[test]
    fn meta_glyphs_exist_in_the_bundled_grid_font() {
        // T-9.3: the block-meta draws its status dot + the "\u{00b7}" separator through
        // the Mono GRID face; a glyph missing from the bundled face renders `.notdef` (a
        // box) and silently breaks the meta - the exact class of regression the prompt /
        // gutter / chip glyph tests guard. This ties coverage to BlockMetaStyle's OWN dot
        // glyphs (which already diverge from GutterStyle - the success dot is a filled dot,
        // not the gutter's check tick) plus META_SEP, not the gutter test's glyph set.
        // Pure font parse: runs on every platform (unlike the macOS-only GPU tests).
        use crate::glyph::GlyphRasterizer;
        use crate::text::FaceStyle;
        let r = GlyphRasterizer::new();
        let theme = dark();
        let markers = [
            GutterMarker::Running,
            GutterMarker::Ok,
            GutterMarker::Failed(1),
            GutterMarker::Unknown,
            GutterMarker::Interactive,
            GutterMarker::Approximate,
        ];
        // Both the quick and the long Ok split (different dot color, same glyph) + none.
        for marker in markers {
            for dur in [Some(0.01_f64), Some(9.9), None] {
                let meta = BlockMetaStyle::resolve(marker, dur, &theme);
                let gid = r.glyph_id(FontFamily::Grid, FaceStyle::Regular, meta.dot_glyph);
                assert_ne!(
                    gid, 0,
                    "{marker:?} meta dot glyph U+{:04X} is .notdef in the bundled Mono Nerd Font",
                    meta.dot_glyph as u32
                );
            }
        }
        assert_ne!(
            r.glyph_id(FontFamily::Grid, FaceStyle::Regular, META_SEP),
            0,
            "meta separator U+{:04X} is .notdef in the bundled Mono Nerd Font",
            META_SEP as u32
        );
    }

    #[test]
    fn agent_glyphs_exist_in_the_bundled_grid_font() {
        // T-9.6/T-9.7: the agent header `◊`, the resolved-gate `✓`/`✕` (as Nerd-Font
        // PUA icons), and the plan eyebrow + step-meta separator all draw through the
        // Mono GRID face. A `.notdef` here would draw a box and silently break the
        // re-skin - the same regression the meta/gutter glyph tests guard. Pure parse.
        use crate::glyph::GlyphRasterizer;
        use crate::text::FaceStyle;
        let r = GlyphRasterizer::new();
        for glyph in [
            AGENT_PROMPT_GLYPH,
            APPROVAL_APPROVED_GLYPH,
            APPROVAL_REJECTED_GLYPH,
        ] {
            assert_ne!(
                r.glyph_id(FontFamily::Grid, FaceStyle::Regular, glyph),
                0,
                "agent glyph U+{:04X} is .notdef in the bundled Mono Nerd Font",
                glyph as u32
            );
        }
        // The PLAN eyebrow is ASCII; every char must resolve (guards a bad edit).
        for ch in PLAN_EYEBROW.chars() {
            assert_ne!(
                r.glyph_id(FontFamily::Grid, FaceStyle::Regular, ch),
                0,
                "plan-eyebrow char {ch:?} is .notdef"
            );
        }
    }

    #[test]
    fn top_hairline_groups_a_turn_and_frames_only_the_final_summary() {
        // T-9.6 (+ review fix): the boundary rule frames a TURN, not its steps. It draws
        // above the `◊` header and the turn's FINAL summary, but NOT above the plan / tool
        // / result / MID-TURN reflection steps grouped under it. The projector marks EVERY
        // post-tool paragraph `Summary`, so this exercises the renderer's discrimination:
        // a mid-turn Summary (another step follows) must NOT be framed; only the last one.
        use aterm_core::{
            AgentBlock, AgentBlockKind, AgentTextRole, BlockSegmenter, Mark, PromptKind,
        };
        use std::time::Instant;
        let now = Instant::now();
        let ai = |kind, role, text: &str| AgentBlock::new(kind, text, now).with_text_role(role);
        let mut list = aterm_core::BlockList::new();
        // header, plan, tool, result, MID-summary, tool, result, FINAL-summary.
        list.push_agent(ai(AgentBlockKind::UserPrompt, AgentTextRole::Body, "req"));
        list.push_agent(ai(
            AgentBlockKind::AssistantText,
            AgentTextRole::Plan,
            "plan",
        ));
        list.push_agent(ai(
            AgentBlockKind::ToolCall,
            AgentTextRole::Body,
            "read_file",
        ));
        list.push_agent(ai(AgentBlockKind::ToolResult, AgentTextRole::Body, "out"));
        list.push_agent(ai(
            AgentBlockKind::AssistantText,
            AgentTextRole::Summary,
            "mid",
        ));
        list.push_agent(ai(
            AgentBlockKind::ToolCall,
            AgentTextRole::Body,
            "edit_file",
        ));
        list.push_agent(ai(AgentBlockKind::ToolResult, AgentTextRole::Body, "diff"));
        list.push_agent(ai(
            AgentBlockKind::AssistantText,
            AgentTextRole::Summary,
            "final",
        ));
        // A trailing human command (a real turn boundary below the final summary).
        let mut seg = BlockSegmenter::new();
        seg.apply(&Mark::Prompt(PromptKind::PromptStart), 0, &mut list);
        seg.apply(&Mark::Prompt(PromptKind::OutputStart), 1, &mut list);
        seg.apply(
            &Mark::Prompt(PromptKind::CommandDone { exit_code: Some(0) }),
            3,
            &mut list,
        );

        let vis = layout(&list, false, Scroll::default(), 60).visible;
        assert_eq!(vis.len(), 9, "all 8 agent steps + 1 command visible");
        // Framed (a rule above): the header (0), the FINAL summary (7), the command (8).
        for i in [0usize, 7, 8] {
            assert!(
                block_draws_top_hairline(&vis, i),
                "block {i} must be framed"
            );
        }
        // Grouped (no rule): plan (1), tool (2), result (3), MID-summary (4), tool (5),
        // result (6).
        for i in [1usize, 2, 3, 4, 5, 6] {
            assert!(
                !block_draws_top_hairline(&vis, i),
                "block {i} is intra-turn and must NOT be framed"
            );
        }
        // The discrimination the fix turns on: block 4 is a mid-turn Summary (not final),
        // block 7 is the final Summary.
        assert!(
            !is_final_summary(&vis, 4),
            "a mid-turn Summary is not the final one"
        );
        assert!(
            is_final_summary(&vis, 7),
            "the last post-tool prose IS the summary"
        );
    }

    #[test]
    fn agent_palette_maps_each_role_to_its_token_in_both_themes() {
        // T-9.6 AC2: the per-kind colors resolve through tokens (no literals). Asserting
        // the palette's token identities in both themes is the noise-immune replacement
        // for the offscreen test's any_ink (which cannot tell purple from grey).
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let theme = *Theme::for_kind(kind);
            let c = &theme.colors;
            let p = AgentPalette::resolve(&theme);
            assert_eq!(
                p.header,
                c.mode_accent(Mode::Agent),
                "{kind:?} header = agent accent"
            );
            assert_eq!(
                p.tool_name, c.accent_primary,
                "{kind:?} tool name = accent primary"
            );
            assert_eq!(p.emphasis, c.fg_primary, "{kind:?} emphasis = fg.primary");
            assert_eq!(p.prose, c.fg_secondary, "{kind:?} prose = fg.secondary");
            assert_eq!(p.faint, c.fg_muted, "{kind:?} faint = fg.muted");
            assert_eq!(p.add, c.success, "{kind:?} add = success");
            assert_eq!(p.remove, c.danger, "{kind:?} remove = danger");
        }
    }

    #[test]
    fn turn_step_count_counts_tool_calls_until_the_turn_ends() {
        // T-9.6: the header meta counts the turn's tool calls among the visible blocks,
        // stopping at the next turn (a UserPrompt) or a human command block.
        use aterm_core::{AgentBlock, AgentBlockKind, Block};
        use std::time::Instant;
        let now = Instant::now();
        let ab = |kind| Block::Agent(AgentBlock::new(kind, "x", now));
        // header, plan, 3 tool calls (with results), summary, THEN a new turn.
        let mut blocks = vec![
            ab(AgentBlockKind::UserPrompt),
            ab(AgentBlockKind::AssistantText),
            ab(AgentBlockKind::ToolCall),
            ab(AgentBlockKind::ToolResult),
            ab(AgentBlockKind::ToolCall),
            ab(AgentBlockKind::ToolResult),
            ab(AgentBlockKind::ToolCall),
            ab(AgentBlockKind::AssistantText),
            ab(AgentBlockKind::UserPrompt), // next turn: must NOT be counted
            ab(AgentBlockKind::ToolCall),
        ];
        let list = {
            let mut l = aterm_core::BlockList::new();
            for b in blocks.drain(..) {
                match b {
                    Block::Agent(a) => {
                        l.push_agent(a);
                    }
                    Block::Command(_) => unreachable!(),
                }
            }
            l
        };
        let lay = layout(&list, false, Scroll::default(), 40);
        // The first turn's header is visible block 0; it has 3 tool calls.
        assert_eq!(turn_step_count(&lay.visible, 0), 3);
        // The second turn's header (visible index 8) has 1 tool call after it.
        assert_eq!(turn_step_count(&lay.visible, 8), 1);
    }

    #[test]
    fn diff_line_color_maps_diff_and_test_lines() {
        let base = aterm_tokens::Rgba {
            r: 1,
            g: 1,
            b: 1,
            a: 255,
        };
        let ok = aterm_tokens::Rgba {
            r: 2,
            g: 2,
            b: 2,
            a: 255,
        };
        let bad = aterm_tokens::Rgba {
            r: 3,
            g: 3,
            b: 3,
            a: 255,
        };
        assert_eq!(diff_line_color("+ added line", base, ok, bad), ok);
        assert_eq!(diff_line_color("-  removed", base, ok, bad), bad);
        assert_eq!(diff_line_color("  + indented add", base, ok, bad), ok);
        assert_eq!(
            diff_line_color("test result: ok. 3 passed", base, ok, bad),
            ok
        );
        assert_eq!(
            diff_line_color("test result: FAILED. 1", base, ok, bad),
            bad
        );
        assert_eq!(diff_line_color("plain output", base, ok, bad), base);
    }

    #[test]
    fn in_place_content_change_invalidates_the_gate() {
        // MAJOR-1 regression guard: a running command's in-place `\r` redraw changes the
        // drawn CELL CONTENT while the row count / block state stay the same. The damage
        // gate must fold the visible content, else the timeline freezes at the first
        // value. Two blocks identical in structure but differing only in their output
        // glyph must produce DIFFERENT signatures.
        let a = block_with_output_char(2, Some(0), 'A');
        let b = block_with_output_char(2, Some(0), 'B');
        let la = layout(&a, false, Scroll::default(), 20);
        let lb = layout(&b, false, Scroll::default(), 20);
        assert_ne!(
            signature(&la, 800, 600, 13, &dark()),
            signature(&lb, 800, 600, 13, &dark()),
            "same structure, different drawn content must invalidate the damage gate"
        );
    }

    #[test]
    fn identical_layout_yields_a_stable_signature() {
        let blocks = block_with_output(3, Some(0));
        let l = layout(&blocks, false, Scroll::default(), 20);
        let a = signature(&l, 800, 600, 13, &dark());
        let b = signature(&l, 800, 600, 13, &dark());
        assert_eq!(a, b, "the signature is deterministic for one layout");
    }

    #[test]
    fn signature_changes_on_every_draw_affecting_axis() {
        let blocks = block_with_output(3, Some(0));
        let l = layout(&blocks, false, Scroll::default(), 20);
        let base = signature(&l, 800, 600, 13, &dark());

        // Theme switch.
        assert_ne!(
            base,
            signature(&l, 800, 600, 13, Theme::for_kind(ThemeKind::Light)),
            "a theme change must invalidate the gate"
        );
        // Viewport + px.
        assert_ne!(base, signature(&l, 801, 600, 13, &dark()), "width");
        assert_ne!(base, signature(&l, 800, 601, 13, &dark()), "height");
        assert_ne!(base, signature(&l, 800, 600, 26, &dark()), "px");

        // A different block state (exit code) is a different layout -> different sig.
        let failed = block_with_output(3, Some(1));
        let lf = layout(&failed, false, Scroll::default(), 20);
        assert_ne!(
            base,
            signature(&lf, 800, 600, 13, &dark()),
            "a changed exit code must invalidate the gate"
        );

        // More captured output rows.
        let longer = block_with_output(5, Some(0));
        let ll = layout(&longer, false, Scroll::default(), 20);
        assert_ne!(
            base,
            signature(&ll, 800, 600, 13, &dark()),
            "a changed output length must invalidate the gate"
        );

        // Scroll position (needs content taller than the viewport to actually move;
        // a layout clamps scroll to the top when everything fits).
        let tall = block_with_output(10, Some(0)); // 11 display rows
        let top = layout(&tall, false, Scroll::default(), 4);
        let down = layout(&tall, false, Scroll { offset_rows: 3 }, 4);
        assert_ne!(
            signature(&top, 800, 600, 13, &dark()),
            signature(&down, 800, 600, 13, &dark()),
            "a scroll change must invalidate the gate"
        );
    }

    #[test]
    fn top_inset_is_a_folded_draw_affecting_axis() {
        // T-9.2: the reserved title-bar band shifts the timeline down (top_margin += inset)
        // and is folded into the built signature, so toggling the title bar on/off forces a
        // rebuild. Exercises the real `fold_top_inset` the prepare gate uses.
        let blocks = block_with_output(3, Some(0));
        let l = layout(&blocks, false, Scroll::default(), 20);
        let base = signature(&l, 800, 600, 13, &dark());
        // No inset (title bar hidden / alt-screen) folds nothing.
        assert_eq!(fold_top_inset(base, 0.0), base, "a zero inset is a no-op");
        // A title-bar inset must invalidate the gate (the timeline moved down), and two
        // distinct insets (e.g. 1x vs 2x DPI) must differ.
        let bar = fold_top_inset(base, 44.0);
        assert_ne!(base, bar, "a nonzero top inset must invalidate the gate");
        assert_ne!(
            bar,
            fold_top_inset(base, 88.0),
            "distinct insets yield distinct signatures"
        );
    }

    #[test]
    fn alt_screen_and_timeline_modes_differ() {
        let blocks = block_with_output(2, Some(0));
        let tl = layout(&blocks, false, Scroll::default(), 20);
        let alt = layout(&blocks, true, Scroll::default(), 20);
        assert_ne!(
            signature(&tl, 800, 600, 13, &dark()),
            signature(&alt, 800, 600, 13, &dark()),
            "alt-screen vs timeline mode must differ (one draws nothing)"
        );
    }

    /// A finished command block (exit 0) followed by one streaming agent text step -
    /// the interleaved single timeline of T-5.10.
    fn command_then_agent(agent_text: &str) -> aterm_core::BlockList {
        use aterm_core::{AgentBlock, AgentBlockKind, BlockSegmenter, Mark, PromptKind};
        use std::time::Instant;
        let mut list = aterm_core::BlockList::new();
        let mut seg = BlockSegmenter::new();
        seg.apply(&Mark::Prompt(PromptKind::PromptStart), 0, &mut list);
        seg.apply(&Mark::Prompt(PromptKind::OutputStart), 1, &mut list);
        seg.apply(
            &Mark::Prompt(PromptKind::CommandDone { exit_code: Some(0) }),
            3,
            &mut list,
        );
        list.push_agent(AgentBlock::new(
            AgentBlockKind::AssistantText,
            agent_text,
            Instant::now(),
        ));
        list
    }

    #[test]
    fn agent_text_delta_invalidates_the_damage_gate() {
        // T-5.10 AC2: streaming a delta into the tail agent step changes its drawn
        // content (and version), so the damage gate must redraw - the agent card must
        // never freeze at its first value (the running-block MAJOR-1 analogue).
        let short = command_then_agent("Hel");
        let long_list = command_then_agent("Hello, world");
        let s1 = signature(
            &layout(&short, false, Scroll::default(), 20),
            800,
            600,
            13,
            &dark(),
        );
        let s2 = signature(
            &layout(&long_list, false, Scroll::default(), 20),
            800,
            600,
            13,
            &dark(),
        );
        assert_ne!(s1, s2, "an agent text delta must invalidate the gate");

        // The in-place streaming path (BlockList::append_agent_text) also moves it.
        let mut streamed = command_then_agent("Hel");
        let before = signature(
            &layout(&streamed, false, Scroll::default(), 20),
            800,
            600,
            13,
            &dark(),
        );
        let tail = streamed.len() - 1;
        assert!(streamed.append_agent_text(tail, "lo"));
        let after = signature(
            &layout(&streamed, false, Scroll::default(), 20),
            800,
            600,
            13,
            &dark(),
        );
        assert_ne!(before, after, "append_agent_text must invalidate the gate");
    }

    #[test]
    fn an_agent_delta_does_not_relayout_the_earlier_command_block() {
        // T-5.10 AC2: "mutates only the current entry". A single-line delta into the
        // tail agent step leaves the EARLIER command block's on-screen geometry
        // byte-identical - only the tail entry's content/version changed (no
        // full-timeline relayout per delta; the 60fps floor).
        let mut list = command_then_agent("Hel");
        // Capture the head block's owned geometry, then drop the layout borrow so the
        // list can be mutated (a VisibleBlock borrows its block, so it cannot be held
        // across the append).
        let (idx, top, first, dh, gutter, rows) = {
            let before = layout(&list, false, Scroll::default(), 20);
            let h = &before.visible[0];
            (
                h.index,
                h.top_in_viewport,
                h.first_row_in_viewport,
                h.display_height,
                h.gutter,
                h.rows.clone(),
            )
        };

        let tail = list.len() - 1;
        list.append_agent_text(tail, "lo"); // same line count, just more text
        let after = layout(&list, false, Scroll::default(), 20);
        let h = &after.visible[0];

        assert_eq!(idx, h.index);
        assert_eq!(top, h.top_in_viewport);
        assert_eq!(first, h.first_row_in_viewport);
        assert_eq!(dh, h.display_height);
        assert_eq!(rows, h.rows);
        assert_eq!(gutter, h.gutter);
    }

    #[test]
    fn agent_badge_maps_each_verdict_to_its_risk_state() {
        // T-5.11: the agent-domain-free badge maps onto the three UI risk states the
        // chip styler speaks - a total, order-preserving mapping.
        assert_eq!(risk_state_for(AgentBadge::Auto), RiskState::Allowed);
        assert_eq!(
            risk_state_for(AgentBadge::NeedsApproval),
            RiskState::NeedsApproval
        );
        assert_eq!(risk_state_for(AgentBadge::Blocked), RiskState::Blocked);
    }

    #[test]
    fn a_badge_verdict_change_invalidates_the_damage_gate() {
        // T-5.11 AC1/AC2: a gated tool-call step draws its badge; an approval flipping
        // the verdict (NeedsApproval -> Auto) - or a re-projection to Blocked - changes
        // the drawn chip, so the damage gate must redraw exactly this entry. Same text,
        // same version; only the badge differs.
        use aterm_core::{AgentBlock, AgentBlockKind};
        use std::time::Instant;

        let with_badge = |badge: Option<AgentBadge>| {
            let mut list = aterm_core::BlockList::new();
            let mut b = AgentBlock::new(AgentBlockKind::ToolCall, "run_command", Instant::now())
                .with_tool_use_id("toolu_1");
            if let Some(badge) = badge {
                b = b.with_badge(badge);
            }
            list.push_agent(b);
            list
        };
        let sig = |list: &aterm_core::BlockList| {
            signature(
                &layout(list, false, Scroll::default(), 20),
                800,
                600,
                13,
                &dark(),
            )
        };

        let needs = sig(&with_badge(Some(AgentBadge::NeedsApproval)));
        let auto = sig(&with_badge(Some(AgentBadge::Auto)));
        let blocked = sig(&with_badge(Some(AgentBadge::Blocked)));
        let none = sig(&with_badge(None));

        // Every distinct verdict (and the no-badge case) is a distinct drawn state.
        for (a, b, what) in [
            (needs, auto, "NeedsApproval vs Auto"),
            (auto, blocked, "Auto vs Blocked"),
            (needs, blocked, "NeedsApproval vs Blocked"),
            (none, auto, "no badge vs Auto"),
        ] {
            assert_ne!(a, b, "a badge change must invalidate the gate: {what}");
        }
    }

    #[test]
    fn agent_kind_invalidates_the_damage_gate() {
        // T-9.6 (review fix): the kind selects the entire Agent draw, so it must be folded
        // - two blocks identical in every other folded field but differing in kind must
        // produce different signatures (else a re-typed slot would render stale).
        use aterm_core::{AgentBlock, AgentBlockKind};
        use std::time::Instant;
        let now = Instant::now();
        let sig = |kind| {
            let mut list = aterm_core::BlockList::new();
            list.push_agent(AgentBlock::new(kind, "x", now).with_tool_use_id("t1"));
            signature(
                &layout(&list, false, Scroll::default(), 20),
                800,
                600,
                13,
                &dark(),
            )
        };
        for (a, b) in [
            (AgentBlockKind::UserPrompt, AgentBlockKind::Thinking),
            (AgentBlockKind::AssistantText, AgentBlockKind::ToolCall),
            (AgentBlockKind::ToolResult, AgentBlockKind::Approval),
        ] {
            assert_ne!(
                sig(a),
                sig(b),
                "a kind change ({a:?} vs {b:?}) must invalidate the gate"
            );
        }
    }

    #[test]
    fn t9_6_display_fields_invalidate_the_damage_gate() {
        // T-9.6 (review fix): tool_name / tool_arg / edit_stats / text_role are drawn but
        // do NOT live in `text` and are set once at push (version 0), so they can only be
        // caught by their explicit fold. Two blocks differing in exactly one must differ.
        use aterm_core::{AgentBlock, AgentBlockKind, AgentTextRole};
        use std::time::Instant;
        let now = Instant::now();
        let sig = |b: AgentBlock| {
            let mut list = aterm_core::BlockList::new();
            list.push_agent(b);
            signature(
                &layout(&list, false, Scroll::default(), 20),
                800,
                600,
                13,
                &dark(),
            )
        };
        let call = || AgentBlock::new(AgentBlockKind::ToolCall, "x", now).with_tool_use_id("t1");
        assert_ne!(
            sig(call().with_tool_display("read_file", None)),
            sig(call().with_tool_display("edit_file", None)),
            "a tool_name change must invalidate the gate"
        );
        assert_ne!(
            sig(call().with_tool_display("read_file", Some("a.rs".into()))),
            sig(call().with_tool_display("read_file", Some("b.rs".into()))),
            "a tool_arg change must invalidate the gate"
        );
        assert_ne!(
            sig(call().with_edit_stats(1, 1)),
            sig(call().with_edit_stats(2, 1)),
            "an edit_stats change must invalidate the gate"
        );
        let text =
            |role| AgentBlock::new(AgentBlockKind::AssistantText, "x", now).with_text_role(role);
        assert_ne!(
            sig(text(AgentTextRole::Plan)),
            sig(text(AgentTextRole::Summary)),
            "a text_role change must invalidate the gate"
        );
    }
}

// The timeline draws to a real GPU through the shared atlas, so it is verified by
// rendering offscreen and reading pixels back - macOS-only, skipping when no adapter is
// present, exactly like the grid/prose GPU tests. These cover: a finished block's gutter
// marker + captured output ink on screen (both themes), the glyph layer is one draw call,
// the damage gate early-outs alloc-free, and alt-screen/empty draws nothing.
#[cfg(all(test, target_os = "macos"))]
mod gpu_tests {
    use super::*;
    use crate::timeline::{layout, Scroll, TimelineLayout};
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
            label: Some("aterm-timeline-test"),
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
        tl: &mut TimelineRenderer,
        layout: &TimelineLayout,
        theme: &Theme,
        w: u32,
        h: u32,
    ) -> Readback {
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tl-target"),
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
            label: Some("tl-readback"),
            size: (stride as u32 * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        tl.prepare(
            device,
            queue,
            atlas,
            layout,
            0.0,
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
                label: Some("tl-pass"),
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
            tl.draw(&mut pass, atlas);
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

    /// Two finished failed blocks, each with `out_rows` captured 'X' output rows, via
    /// the real segmenter + `set_block_output` - so the timeline has an interior
    /// boundary to draw a hairline at (T-4.7).
    fn two_finished_blocks(out_rows: usize) -> aterm_core::BlockList {
        use aterm_core::{BlockSegmenter, CellColor, Mark, PromptKind, RowSnapshot, SnapshotCell};
        let mut list = aterm_core::BlockList::new();
        let mut seg = BlockSegmenter::new();
        for b in 0..2usize {
            let base = b * 4;
            seg.apply(&Mark::Prompt(PromptKind::PromptStart), base, &mut list);
            seg.apply(&Mark::Prompt(PromptKind::OutputStart), base + 1, &mut list);
            seg.apply(
                &Mark::Prompt(PromptKind::CommandDone { exit_code: Some(1) }),
                base + 3,
                &mut list,
            );
        }
        let rows: Vec<RowSnapshot> = (0..out_rows)
            .map(|_| {
                RowSnapshot::new(vec![SnapshotCell {
                    c: 'X',
                    fg: CellColor::Rgb(255, 255, 255),
                    bg: CellColor::Named(257), // canvas -> bg quad skipped
                    ..Default::default()
                }])
            })
            .collect();
        list.set_block_output(0, rows.clone());
        list.set_block_output(1, rows);
        list
    }

    #[test]
    fn timeline_inks_marker_output_and_one_muted_boundary_hairline_in_both_themes() {
        let Some((device, queue, format)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let (cw, ch) = cell_px(SCALE);
        // The iA token geometry (T-4.7): top breathing S12, edge gutter S8, marker band
        // S4, intra-block padding S4.
        let top = f32::from(space::S12) * SCALE; // 48
        let edge = f32::from(space::S8) * SCALE; // 32
        let gutter_w = f32::from(space::S4) * SCALE; // CommandBlockStyle.gutter_px
        let content_x = edge + gutter_w + f32::from(space::S4) * SCALE; // + intra pad = 64
        let (w, h) = (240u32, (top + 9.0 * ch) as u32);

        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let theme = *Theme::for_kind(kind);
            let mut atlas = GlyphAtlas::new(&device, format);
            let mut tl = TimelineRenderer::new(&device);
            // Two failed blocks (gutter = a danger marker, a robust BMP glyph), each 1
            // command + 2 output rows. Gapped layout: block 0 rows [0,3), gap row 3,
            // block 1 rows [4,7). Viewport 10 shows all of it from the top.
            let blocks = two_finished_blocks(2);
            let l = layout(&blocks, false, Scroll::default(), 10);
            let rb = render(&device, &queue, &mut atlas, &mut tl, &l, &theme, w, h);

            // Block 0's gutter marker inks on its command row (gapped row 0), in the
            // marker band [edge, edge + gutter_w).
            assert!(
                rb.any_ink(
                    edge as u32,
                    top as u32,
                    (edge + gutter_w) as u32,
                    (top + ch) as u32,
                    50
                ),
                "{kind:?}: the gutter status marker inks in the marker band on the command row"
            );
            // Block 0's captured output 'X' inks on its first output row (gapped row 1)
            // at the content column (which now starts after the S4 intra-block padding).
            let oy0 = (top + ch) as u32;
            let oy1 = (top + 2.0 * ch) as u32;
            assert!(
                rb.any_ink(content_x as u32, oy0, (content_x + cw) as u32, oy1, 50),
                "{kind:?}: the captured output cell inks in the padded content column"
            );
            // EXACTLY ONE muted hairline at the interior boundary: centered in the gap
            // (gapped row 3), i.e. at top + 3.5*ch. Sample a mid-canvas x inside the
            // inner width [edge, w-edge) and clear of the content glyphs.
            let (hx0, hx1) = (w / 2, w / 2 + 18);
            let hy = (top + 3.5 * ch) as u32;
            assert!(
                rb.any_ink(hx0, hy.saturating_sub(2), hx1, hy + 2, 18),
                "{kind:?}: the boundary hairline inks across the inner canvas width"
            );
            // ...and NO edge line above the first block: the breathing band + block 0's
            // top edge are clear of any rule (the old top+bottom double-draw is gone).
            // Same x window, so a top edge line (which would span the full inner width)
            // would be caught here.
            assert!(
                !rb.any_ink(hx0, 1, hx1, top as u32, 18),
                "{kind:?}: no hairline renders above the first block (no top edge line)"
            );
        }
    }

    /// A mixed finished timeline (ticket T-9.3 AC4): block 0 = exit 0 with output (ok),
    /// block 1 = exit 1 with output (failed), block 2 = exit 0 with NO output (a thin,
    /// instant command). Built through the real segmenter + `set_block_output`.
    fn mixed_finished_blocks() -> aterm_core::BlockList {
        use aterm_core::{BlockSegmenter, CellColor, Mark, PromptKind, RowSnapshot, SnapshotCell};
        let mut list = aterm_core::BlockList::new();
        let mut seg = BlockSegmenter::new();
        for (b, exit) in [(0usize, 0i32), (1, 1), (2, 0)] {
            let base = b * 4;
            seg.apply(&Mark::Prompt(PromptKind::PromptStart), base, &mut list);
            seg.apply(&Mark::Prompt(PromptKind::OutputStart), base + 1, &mut list);
            seg.apply(
                &Mark::Prompt(PromptKind::CommandDone {
                    exit_code: Some(exit),
                }),
                base + 3,
                &mut list,
            );
        }
        let rows: Vec<RowSnapshot> = (0..2usize)
            .map(|_| {
                RowSnapshot::new(vec![SnapshotCell {
                    c: 'X',
                    fg: CellColor::Rgb(255, 255, 255),
                    bg: CellColor::Named(257),
                    ..Default::default()
                }])
            })
            .collect();
        list.set_block_output(0, rows.clone()); // ok: has output
        list.set_block_output(1, rows); // failed: has output
                                        // block 2 keeps no output -> thin/instant.
        list
    }

    #[test]
    fn timeline_block_meta_inks_for_ok_failed_and_instant_in_both_themes() {
        // T-9.3 AC4: a mixed timeline (ok / failed / instant) renders its right-aligned
        // block-meta on EACH command row, in both themes. The exact per-state colors are
        // asserted in the pure `block_meta_maps_state_to_token_color_shape_and_label`
        // test; here we prove the meta actually inks on screen for all three states.
        let Some((device, queue, format)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let (_cw, ch) = cell_px(SCALE);
        let top = f32::from(space::S12) * SCALE;
        let edge = f32::from(space::S8) * SCALE;
        // Wide enough that the meta ("exit 1 \u{00b7} 0.00s") fits to the right of the
        // (empty) command text; 12 viewport rows show all 9 gapped display rows.
        let (w, h) = (360u32, (top + 12.0 * ch) as u32);
        // Gapped layout: block0 cmd row 0, block1 cmd row 4, block2 cmd row 8.
        let meta_rows = [0.0f32, 4.0, 8.0];

        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let theme = *Theme::for_kind(kind);
            let mut atlas = GlyphAtlas::new(&device, format);
            let mut tl = TimelineRenderer::new(&device);
            let blocks = mixed_finished_blocks();
            let l = layout(&blocks, false, Scroll::default(), 12);
            let rb = render(&device, &queue, &mut atlas, &mut tl, &l, &theme, w, h);

            // The meta is right-aligned inside the inner canvas [edge, w-edge); sample a
            // right-side band on each command row (clear of the left-gutter prompt glyph
            // and the empty command text).
            let mx0 = w / 2;
            let mx1 = (w as f32 - edge) as u32;
            for (i, row) in meta_rows.iter().enumerate() {
                let ry = (top + row * ch) as u32;
                assert!(
                    rb.any_ink(mx0, ry, mx1, ry + ch as u32, 30),
                    "{kind:?}: block {i}'s block-meta (dot + caption) inks on its command row"
                );
            }
        }
    }

    #[test]
    fn timeline_glyph_layer_is_a_single_draw_call() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut tl = TimelineRenderer::new(&device);
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let blocks = block_with_output(2, Some(0));
        let l = layout(&blocks, false, Scroll::default(), 8);
        render(&device, &queue, &mut atlas, &mut tl, &l, &theme, 240, 120);
        assert_eq!(
            tl.last_glyph_draw_calls(),
            1,
            "the whole timeline glyph layer is ONE instanced draw call"
        );
    }

    #[test]
    fn unchanged_layout_skips_rebuild_alloc_free() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut tl = TimelineRenderer::new(&device);
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let blocks = block_with_output(3, Some(0));
        let l = layout(&blocks, false, Scroll::default(), 12);
        let size = FrameSize {
            width: 240,
            height: 200,
            scale: SCALE,
        };
        // First prepare builds + caches (allocates).
        tl.prepare(&device, &queue, &mut atlas, &l, 0.0, &theme, size);
        // An unchanged layout must early-out with NO allocation (the steady-state
        // present path; the same zero-alloc discipline as the grid).
        let allocs = crate::alloc_probe::count_allocs(|| {
            let drew = tl.prepare(&device, &queue, &mut atlas, &l, 0.0, &theme, size);
            std::hint::black_box(drew);
        });
        assert_eq!(
            allocs, 0,
            "an unchanged timeline frame's prepare early-out allocates nothing (got {allocs})"
        );
    }

    #[test]
    fn alt_screen_and_empty_timeline_draw_nothing() {
        let Some((device, queue, format)) = device() else {
            return;
        };
        let mut atlas = GlyphAtlas::new(&device, format);
        let mut tl = TimelineRenderer::new(&device);
        let theme = *Theme::for_kind(ThemeKind::Dark);
        let size = FrameSize {
            width: 240,
            height: 200,
            scale: SCALE,
        };

        // Alt-screen: the grid owns the screen, the timeline draws nothing.
        let blocks = block_with_output(3, Some(0));
        let alt = layout(&blocks, true, Scroll::default(), 12);
        assert!(
            !tl.prepare(&device, &queue, &mut atlas, &alt, 0.0, &theme, size),
            "alt-screen mode draws no timeline"
        );
        assert_eq!(tl.bg_instances.len(), 0);
        assert_eq!(tl.glyph_instances.len(), 0);

        // Empty timeline: nothing to draw.
        let empty = aterm_core::BlockList::new();
        let el = layout(&empty, false, Scroll::default(), 12);
        assert!(
            !tl.prepare(&device, &queue, &mut atlas, &el, 0.0, &theme, size),
            "an empty timeline draws nothing"
        );
    }

    /// A multi-step agent turn (ticket T-9.6 AC4): a `◊` header, a PLAN, a read + an
    /// edit (with "+A -M") + a run, each with a result, and a final SUMMARY - the mock's
    /// `agent` state, built through the real `push_agent` path.
    fn agent_turn() -> aterm_core::BlockList {
        use aterm_core::{AgentBlock, AgentBlockKind, AgentTextRole};
        use std::time::Instant;
        let now = Instant::now();
        let mut list = aterm_core::BlockList::new();
        list.push_agent(AgentBlock::new(
            AgentBlockKind::UserPrompt,
            "fix the failing block-render test",
            now,
        ));
        list.push_agent(
            AgentBlock::new(
                AgentBlockKind::AssistantText,
                "Read the failing test, then patch the hairline logic.",
                now,
            )
            .with_text_role(AgentTextRole::Plan),
        );
        list.push_agent(
            AgentBlock::new(AgentBlockKind::ToolCall, "read_file (auto)", now)
                .with_tool_use_id("t1")
                .with_tool_display("read_file", Some("src/render/blocks.rs".to_string()))
                .with_badge(AgentBadge::Auto),
        );
        list.push_agent(
            AgentBlock::new(AgentBlockKind::ToolResult, "fn draw(&self) { ... }", now)
                .with_tool_use_id("t1"),
        );
        list.push_agent(
            AgentBlock::new(AgentBlockKind::ToolCall, "edit_file (needs approval)", now)
                .with_tool_use_id("t2")
                .with_tool_display("edit_file", Some("src/render/blocks.rs".to_string()))
                .with_edit_stats(1, 1)
                .with_badge(AgentBadge::NeedsApproval),
        );
        list.push_agent(
            AgentBlock::new(
                AgentBlockKind::ToolResult,
                "-        self.hairline(b.top);\n+        if i > 0 { self.hairline(b.top); }",
                now,
            )
            .with_tool_use_id("t2"),
        );
        list.push_agent(
            AgentBlock::new(AgentBlockKind::ToolCall, "run_command (auto)", now)
                .with_tool_use_id("t3")
                .with_tool_display("run_command", Some("cargo test render::blocks".to_string()))
                .with_badge(AgentBadge::Auto),
        );
        list.push_agent(
            AgentBlock::new(
                AgentBlockKind::ToolResult,
                "test result: ok. 3 passed; 0 failed",
                now,
            )
            .with_tool_use_id("t3"),
        );
        list.push_agent(
            AgentBlock::new(
                AgentBlockKind::AssistantText,
                "Fixed. Guarded the hairline with i > 0; all three tests pass.",
                now,
            )
            .with_text_role(AgentTextRole::Summary),
        );
        list
    }

    #[test]
    fn agent_turn_reskin_inks_in_both_themes() {
        // T-9.6 AC1/AC4: the full turn (header + plan + tool rows + diff results +
        // summary) renders in BOTH themes. The exact per-kind token colors are asserted by
        // the pure `agent_palette_maps_each_role_to_its_token_in_both_themes`; here we
        // prove the whole turn actually inks on screen (any_ink cannot tell the tokens
        // apart) and stays a single glyph draw call (T-1.8), in both themes.
        let Some((device, queue, format)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let (_cw, ch) = cell_px(SCALE);
        let top = f32::from(space::S12) * SCALE;
        let edge = f32::from(space::S8) * SCALE;
        // 9 steps, each 1 content row + a gap between -> ~17 gapped rows; give headroom.
        let (w, h) = (420u32, (top + 26.0 * ch) as u32);

        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let theme = *Theme::for_kind(kind);
            let mut atlas = GlyphAtlas::new(&device, format);
            let mut tl = TimelineRenderer::new(&device);
            let blocks = agent_turn();
            let l = layout(&blocks, false, Scroll::default(), 30);
            let rb = render(&device, &queue, &mut atlas, &mut tl, &l, &theme, w, h);

            // The `◊` header glyph inks in the gutter marker band on the top row.
            let gutter_w = f32::from(space::S4) * SCALE;
            assert!(
                rb.any_ink(
                    edge as u32,
                    top as u32,
                    (edge + gutter_w) as u32,
                    (top + ch) as u32,
                    40
                ),
                "{kind:?}: the agent header glyph inks in the gutter band"
            );
            // The turn's body inks across the content column somewhere below the header.
            let content_x = edge + gutter_w + f32::from(space::S4) * SCALE;
            assert!(
                rb.any_ink(
                    content_x as u32,
                    (top + ch) as u32,
                    (w as f32 - edge) as u32,
                    (top + 18.0 * ch) as u32,
                    40
                ),
                "{kind:?}: the turn's plan / tool / result / summary rows ink"
            );
            assert_eq!(
                tl.last_glyph_draw_calls(),
                1,
                "{kind:?}: the whole agent turn is ONE glyph draw call"
            );
        }
    }

    /// A turn whose gated call resolved to approved, plus one that resolved to rejected -
    /// the two [`AgentBlockKind::Approval`] states the session injects (ticket T-9.7).
    fn resolved_gate_turn() -> aterm_core::BlockList {
        use aterm_core::{AgentBlock, AgentBlockKind};
        use std::time::Instant;
        let now = Instant::now();
        let mut list = aterm_core::BlockList::new();
        list.push_agent(AgentBlock::new(
            AgentBlockKind::UserPrompt,
            "free up disk space",
            now,
        ));
        list.push_agent(
            AgentBlock::new(AgentBlockKind::ToolCall, "run_command", now)
                .with_tool_use_id("g1")
                .with_tool_display("run_command", Some("rm -rf ./target".to_string()))
                .with_badge(AgentBadge::Blocked),
        );
        // Approved: a success ✓ line.
        list.push_agent(AgentBlock::new(
            AgentBlockKind::Approval,
            "Approved - the command was run.",
            now,
        ));
        // Rejected: a danger ✕ line (is_error = true).
        list.push_agent(
            AgentBlock::new(
                AgentBlockKind::Approval,
                "Rejected - the command was not run.",
                now,
            )
            .with_error(true),
        );
        list
    }

    #[test]
    fn resolved_gate_states_ink_in_both_themes() {
        // T-9.7 AC5 (the resolved half): the approved (✓) + rejected (✕) `Approval` states
        // the session injects render to the timeline in BOTH themes, each a single glyph
        // draw. The pending card is the separate `approval_render` overlay (its own test).
        let Some((device, queue, format)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let (_cw, ch) = cell_px(SCALE);
        let top = f32::from(space::S12) * SCALE;
        let edge = f32::from(space::S8) * SCALE;
        let (w, h) = (420u32, (top + 12.0 * ch) as u32);

        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let theme = *Theme::for_kind(kind);
            let mut atlas = GlyphAtlas::new(&device, format);
            let mut tl = TimelineRenderer::new(&device);
            let blocks = resolved_gate_turn();
            let l = layout(&blocks, false, Scroll::default(), 30);
            let rb = render(&device, &queue, &mut atlas, &mut tl, &l, &theme, w, h);
            // The ✓ / ✕ status glyphs + resolution text ink in the content column below the
            // header + tool-call rows.
            let gutter_w = f32::from(space::S4) * SCALE;
            let content_x = edge + gutter_w;
            assert!(
                rb.any_ink(
                    content_x as u32,
                    (top + 2.0 * ch) as u32,
                    (w as f32 - edge) as u32,
                    (top + 10.0 * ch) as u32,
                    40
                ),
                "{kind:?}: the resolved approved/rejected gate lines ink"
            );
            assert_eq!(
                tl.last_glyph_draw_calls(),
                1,
                "{kind:?}: the resolved gate turn is ONE glyph draw call"
            );
        }
    }
}
