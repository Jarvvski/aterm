//! The live block-timeline compositor (ticket T-4.6): the THIRD front-end over the
//! shared [`GlyphAtlas`], after the grid ([`crate::grid_render`]) and prose
//! ([`crate::prose`]).
//!
//! Where the grid draws the raw VT viewport, this draws the Warp-style block timeline:
//! the virtualized [`TimelineLayout`] (ticket T-2.7) turned into on-screen command
//! blocks styled to the iA component spec ([`crate::components`]) - a left-gutter status
//! marker, the re-rendered command line, the captured output rows, hairline separators,
//! and the "... +N lines" collapse affordance. Finished blocks draw from their immutable
//! captured `output` ([`aterm_core::RowSnapshot`], byte-replayed at finish), so they are
//! immune to the live grid's scrollback eviction.
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

use aterm_tokens::{type_scale, Theme};

use crate::atlas::{GlyphAtlas, GlyphInstance, InstanceBuffer, RectInstance};
use crate::cell_render::{emit_cell, CellCtx};
use crate::components::{CommandBlockStyle, GutterStyle, RiskBadge, RiskState};
use crate::grid_render::FrameSize;
use crate::text::{resolve_color, FontFamily, GridCell};
use crate::timeline::{GutterMarker, TimelineLayout, TimelineMode, TimelineRow, GAP_ROWS};
use crate::window::cell_px;
use aterm_core::AgentBadge;
use aterm_tokens::space;

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
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &mut GlyphAtlas,
        layout: &TimelineLayout,
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

        let sig = signature(layout, width, height, px_key, theme);
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
        // Top breathing band. The matching BOTTOM `space::S12` band is NOT a second
        // offset here - the caller (`gpu::prepare`) already shrank `viewport_rows` by
        // 2*S12, so the last row's bottom lands one S12 above the surface foot. Both
        // bands are one constant; edit them together (see gpu.rs viewport_rows).
        let top_margin = f32::from(space::S12) * scale; // canvas breathing room (top)
        let edge = f32::from(space::S8) * scale; // horizontal canvas gutter (both sides)
        let pad = f32::from(space::S4) * scale; // intra-block content padding
        let metrics = atlas.cell_metrics(FontFamily::Grid, px);
        let baseline_off = (ch - metrics.line) * 0.5 + metrics.ascent;
        let cmd = CommandBlockStyle::resolve(theme);
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

        for vb in &layout.visible {
            let gutter = GutterStyle::resolve(vb.gutter, theme);

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
            if vb.index > 0 && vb.top_in_viewport >= GAP_ROWS as i64 {
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

            for (k, row) in vb.rows.iter().enumerate() {
                let y = top_margin + (vb.first_row_in_viewport + k as i64) as f32 * ch;
                match row {
                    TimelineRow::Command => {
                        let Some(cb) = command_block else { continue };
                        // Gutter status marker (Mono glyph in the marker color).
                        let marker = grid_glyph(gutter.glyph, gutter.color, canvas);
                        emit_cell(
                            atlas,
                            queue,
                            &marker,
                            (marker_x, y),
                            &ctx,
                            &mut self.bg_instances,
                            &mut self.glyph_instances,
                        );
                        // The re-rendered command line (Mono fg.primary).
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
                        // A trailing tag in the marker color: the failed exit code, or
                        // the heuristic/interactive label - so the gutter state reads in
                        // text too (color is never the only signal).
                        let mut tag = String::new();
                        if let Some(code) = gutter.exit_code {
                            tag = format!("[{code}]");
                        } else if let Some(label) = gutter.label {
                            tag = format!("[{label}]");
                        }
                        if !tag.is_empty() {
                            x += cw; // one-cell gap after the command
                            for c in tag.chars() {
                                let cell = grid_glyph(c, gutter.color, canvas);
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
                    }
                    TimelineRow::Output(i) => {
                        if let Some(snap_row) = command_block.and_then(|c| c.output.get(*i)) {
                            for (col, sc) in snap_row.cells.iter().enumerate() {
                                if sc.wide_spacer {
                                    continue;
                                }
                                let mut fg = resolve_color(sc.fg, theme, true);
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
                        // An agent transcript step (T-5.10): draw the gutter marker on
                        // its first line, then this line of its (pre-glossed,
                        // pre-sanitized) text in Mono fg.primary. A gated tool-call step
                        // ALSO draws its risk-gate badge inline at the head of line 0
                        // (T-5.11): the badge LABEL in the saturated semantic color,
                        // always paired with text (color-blind safety); the parsed
                        // reason gloss is already in `ab.text`. Real card styling is
                        // EPIC-4 (T-4.6); this is the data binding.
                        let Some(ab) = agent_block else { continue };
                        if *line == 0 {
                            let marker = grid_glyph(gutter.glyph, gutter.color, canvas);
                            emit_cell(
                                atlas,
                                queue,
                                &marker,
                                (marker_x, y),
                                &ctx,
                                &mut self.bg_instances,
                                &mut self.glyph_instances,
                            );
                        }
                        let mut x = content_x;
                        // The risk-gate badge prefixes line 0 of a gated tool call.
                        if *line == 0 {
                            if let Some(badge) = ab.badge {
                                let rb = RiskBadge::resolve(risk_state_for(badge), theme);
                                for c in rb.label.chars() {
                                    let cell = grid_glyph(c, rb.gutter_color, canvas);
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
                                x += cw; // a space between the badge and the gloss
                            }
                        }
                        if let Some(text_line) = ab.text.split('\n').nth(*line) {
                            for c in text_line.chars() {
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
        tl.prepare(&device, &queue, &mut atlas, &l, &theme, size);
        // An unchanged layout must early-out with NO allocation (the steady-state
        // present path; the same zero-alloc discipline as the grid).
        let allocs = crate::alloc_probe::count_allocs(|| {
            let drew = tl.prepare(&device, &queue, &mut atlas, &l, &theme, size);
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
            !tl.prepare(&device, &queue, &mut atlas, &alt, &theme, size),
            "alt-screen mode draws no timeline"
        );
        assert_eq!(tl.bg_instances.len(), 0);
        assert_eq!(tl.glyph_instances.len(), 0);

        // Empty timeline: nothing to draw.
        let empty = aterm_core::BlockList::new();
        let el = layout(&empty, false, Scroll::default(), 12);
        assert!(
            !tl.prepare(&device, &queue, &mut atlas, &el, &theme, size),
            "an empty timeline draws nothing"
        );
    }
}
