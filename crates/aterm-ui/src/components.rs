//! Token-driven component STYLE descriptors (ticket T-4.6).
//!
//! The iA component specs ([`07-ia-design-language.md`] §5) made concrete: pure,
//! theme-aware *style* resolvers - one per component - that turn a piece of semantic
//! UI state into the colors, dimensions, glyph, and type-style the renderer paints.
//! This is the **style** half of T-4.6; the **geometry + draw** half is the timeline
//! compositor (the `timeline_render` GPU front-end). The split mirrors the one between
//! [`crate::timeline`] (pure geometry) and the GPU front-end, and between
//! [`crate::indicator`] (pure presentation) and the renderer that paints it.
//!
//! ## Why a pure layer
//! Every value here is read from [`aterm_tokens`] - there is NOT ONE hardcoded color
//! (AC1) - and nothing here touches a GPU or a window, so the whole component system
//! is exhaustively unit-tested in BOTH themes with no device (the crate's "pure logic,
//! heavily unit-tested, no window" rule). The renderer consumes these descriptors and
//! is left as pure geometry + atlas lookups.
//!
//! ## Crate-boundary note (the risk-gate badge)
//! `aterm-ui` must never depend on `aterm-agent` (the locked dependency arrow), so the
//! badge speaks a UI-local [`RiskState`] (Allowed / NeedsApproval / Blocked), NOT
//! `aterm_agent::Risk`. `aterm-app` (which sees both crates) maps the agent's verdict
//! onto it. The badge ALWAYS carries a text label beside its color (AC2 / color-blind
//! safety): color is the fast signal, never the only one.
//!
//! ## Motion (AC4)
//! Exactly three animations are allowed ([`Animation`]), each `<= 220ms`
//! ([`aterm_tokens::motion`]). The prompt routing-chip toggle reuses the same
//! `motion.fast` [`Animation::CrossFade`] as the gate badge - it is the cross-fade
//! animation, not a fourth kind - so the cap of three holds.

use aterm_tokens::{legible_against, motion, space, type_scale, Rgba, Theme, TypeStyle};

use crate::timeline::GutterMarker;

// ---------------------------------------------------------------------------
// Color derivation (renderer-side, never a token edit)
// ---------------------------------------------------------------------------

/// Per-channel sRGB blend `c -> bg` by `t` (`0.0` keeps `c`, `1.0` is `bg`), alpha
/// from `c`. The "weak tint" of a saturated semantic color is derived this way rather
/// than added to the palette: per `design-system.md` §3 tints/remaps are a *render*
/// concern, not a token edit, exactly like the light-paper ANSI legibility remap.
fn mix(c: Rgba, bg: Rgba, t: f32) -> Rgba {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Rgba {
        r: f(c.r, bg.r),
        g: f(c.g, bg.g),
        b: f(c.b, bg.b),
        a: c.a,
    }
}

/// How far a semantic color is pulled toward the surface for its chip "weak tint".
/// High enough that the saturated same-hue text reads clearly on it in both themes
/// (verified by [`tests::chip_text_is_legible_on_its_fill`]); the dedicated
/// `accent_primary_weak` token is used verbatim for the `Info` variant instead.
const WEAK_TINT_T: f32 = 0.84;

/// The WCAG contrast floor a chip's text must clear against its fill - the 3:1
/// large-text / UI bar (`design-system.md` §3), the same bar the light-paper ANSI
/// remap uses. The amber `caution` token sits just under it on a pale light fill, so
/// the text is pulled (via [`legible_against`]) the minimal amount to reach it; for
/// every other variant the color already clears the floor and the pull is a no-op.
pub const CHIP_MIN_CONTRAST: f32 = 3.0;

// ---------------------------------------------------------------------------
// Status chip - the generic small pill every other component reuses
// ---------------------------------------------------------------------------

/// The variants of the generic status chip ([`07-ia-design-language.md`] §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipVariant {
    /// Default chrome chip: surface fill, secondary text, the ONLY variant with a
    /// hairline border.
    Neutral,
    /// Accent chip (the routing-target AGENT chip / informational): the dedicated
    /// `accent_primary_weak` fill + `accent_primary` text.
    Info,
    Success,
    Caution,
    Danger,
}

/// The resolved look of a status chip: fill + text + an optional hairline border
/// (present only on [`ChipVariant::Neutral`]), plus the flat-rect geometry tokens.
/// `radius` is tiny by design (iA is "essentially flat rectangles").
#[derive(Debug, Clone, Copy)]
pub struct ChipStyle {
    pub fill: Rgba,
    pub text: Rgba,
    /// Hairline border color, `None` for every variant except `Neutral`.
    pub border: Option<Rgba>,
    pub radius_px: u16,
    pub pad_x: u16,
    pub pad_y: u16,
    /// Quattro `type.label` - the dense-chrome register.
    pub type_style: TypeStyle,
}

impl ChipStyle {
    /// Resolve a chip variant against `theme`. `Info` uses the dedicated
    /// `accent_primary_weak` token; the three semantic variants derive a weak tint of
    /// their saturated color (toward the surface) for the fill and keep the saturated
    /// color as the text (the "weak tint + saturated text" spec).
    #[must_use]
    pub fn resolve(variant: ChipVariant, theme: &Theme) -> Self {
        let c = &theme.colors;
        let (fill, raw_text, border) = match variant {
            ChipVariant::Neutral => (c.bg_surface, c.fg_secondary, Some(c.hairline)),
            // The chip label is SMALL text, so it takes `accent_primary_text` (the
            // AA-small-text accent variant, #1577C2 on light; == accent_primary on
            // dark) rather than the large/UI `accent_primary` - which only clears
            // 2.67:1 on the light weak fill (Recommendation 8 in the design doc).
            ChipVariant::Info => (c.accent_primary_weak, c.accent_primary_text, None),
            ChipVariant::Success => (mix(c.success, c.bg_surface, WEAK_TINT_T), c.success, None),
            ChipVariant::Caution => (mix(c.caution, c.bg_surface, WEAK_TINT_T), c.caution, None),
            ChipVariant::Danger => (mix(c.danger, c.bg_surface, WEAK_TINT_T), c.danger, None),
        };
        // Pull the text the minimal amount to clear the UI contrast floor on its own
        // fill (a no-op for every variant except amber `caution` on the light fill).
        let text = legible_against(raw_text, fill, CHIP_MIN_CONTRAST);
        Self {
            fill,
            text,
            border,
            radius_px: space::RADIUS_SM,
            pad_x: space::S2,
            pad_y: space::S1,
            type_style: type_scale::LABEL,
        }
    }
}

// ---------------------------------------------------------------------------
// Command-block gutter marker
// ---------------------------------------------------------------------------

/// The drawn shape of a gutter marker, kept distinct from color so the meaning still
/// reads in monochrome / for a color-blind eye (the shape carries it, color
/// reinforces - the same discipline as [`crate::indicator`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GutterShape {
    /// A filled dot that pulses (the one allowed running animation).
    PulsingDot,
    /// A filled dot (failed / has an exit code).
    Dot,
    /// A thin success tick.
    Tick,
    /// A hollow dot (finished, exit unknown).
    HollowDot,
    /// A half-filled dot (heuristic / approximate boundary).
    HalfDot,
    /// A right-pointing marker for a full-screen ("ran vim") interactive block.
    Interactive,
}

/// The resolved gutter marker for a command block: its shape + glyph + semantic
/// color, the failed-exit code (rendered in `type.caption`), and an optional terse
/// label (e.g. "approx" for a heuristic block). `pulsing` is true ONLY for a running
/// block - the single pulsing-dot animation the motion budget allows.
#[derive(Debug, Clone, Copy)]
pub struct GutterStyle {
    pub shape: GutterShape,
    pub glyph: char,
    pub color: Rgba,
    pub exit_code: Option<i32>,
    pub label: Option<&'static str>,
    pub pulsing: bool,
}

impl GutterStyle {
    /// Resolve the gutter style for a block's [`GutterMarker`] against `theme`
    /// ([`07-ia-design-language.md`] §5 command block).
    #[must_use]
    pub fn resolve(marker: GutterMarker, theme: &Theme) -> Self {
        let c = &theme.colors;
        match marker {
            // Running: a pulsing accent dot (the sole running animation; no spinner).
            // The gutter glyphs are Nerd-Font PUA icons (`nf-fa-*`), NOT the BMP geometric
            // shapes (●○◐▸): those are absent from the bundled Mono Nerd Font and would
            // render as `.notdef` boxes. The PUA icons are present + auto-centered into the
            // cell by the T-4.4 constraint table (FIT_CENTER); the
            // `gutter_glyphs_exist_in_the_bundled_grid_font` test guards their presence.
            GutterMarker::Running => Self {
                shape: GutterShape::PulsingDot,
                glyph: '\u{f111}', // nf-fa-circle (filled dot)
                color: c.accent_primary,
                exit_code: None,
                label: None,
                pulsing: true,
            },
            // Exit 0: a success tick (a tick, not a dot - shape distinguishes it from the
            // failure dot for a color-blind reading).
            GutterMarker::Ok => Self {
                shape: GutterShape::Tick,
                glyph: '\u{f00c}', // nf-fa-check
                color: c.success,
                exit_code: None,
                label: None,
                pulsing: false,
            },
            // Non-zero exit: a danger dot carrying the code (drawn in type.caption).
            GutterMarker::Failed(code) => Self {
                shape: GutterShape::Dot,
                glyph: '\u{f111}', // nf-fa-circle (filled dot)
                color: c.danger,
                exit_code: Some(code),
                label: None,
                pulsing: false,
            },
            // Finished but exit unknown (Ctrl-C / missing D): a muted hollow dot.
            GutterMarker::Unknown => Self {
                shape: GutterShape::HollowDot,
                glyph: '\u{f10c}', // nf-fa-circle-o (hollow dot)
                color: c.fg_muted,
                exit_code: None,
                label: None,
                pulsing: false,
            },
            // A full-screen app block ("ran vim"): a secondary right-pointing marker.
            GutterMarker::Interactive => Self {
                shape: GutterShape::Interactive,
                glyph: '\u{f0da}', // nf-fa-caret-right (▸-class)
                color: c.fg_secondary,
                exit_code: None,
                label: Some("tui"),
                pulsing: false,
            },
            // Heuristic boundary (not integration-confirmed): a caution half-dot,
            // labelled so the approximation is loud (mirrors the integration indicator).
            GutterMarker::Approximate => Self {
                shape: GutterShape::HalfDot,
                glyph: '\u{f042}', // nf-fa-circle-half-stroke (half dot)
                color: c.caution,
                exit_code: None,
                label: Some("approx"),
                pulsing: false,
            },
        }
    }
}

/// The flat-rectangle geometry of a command block (no heavy box; delimited by a
/// hairline top + bottom only - [`07-ia-design-language.md`] §5). The command line is
/// `fg.primary`; the output uses the per-theme ANSI palette on the canvas; a collapsed
/// block's "... +N lines" affordance is `fg.muted`. All colors via tokens.
#[derive(Debug, Clone, Copy)]
pub struct CommandBlockStyle {
    /// Left gutter width (`space.4`).
    pub gutter_px: u16,
    /// The hairline drawn at the block's top and bottom edges.
    pub hairline: Rgba,
    /// The re-rendered command line color.
    pub command_fg: Rgba,
    /// `type.caption` color for the "… +N lines" collapse affordance and exit codes.
    pub caption_fg: Rgba,
    pub command_type: TypeStyle,
    pub caption_type: TypeStyle,
}

impl CommandBlockStyle {
    #[must_use]
    pub fn resolve(theme: &Theme) -> Self {
        let c = &theme.colors;
        Self {
            gutter_px: space::S4,
            hairline: c.hairline,
            command_fg: c.fg_primary,
            caption_fg: c.fg_muted,
            command_type: type_scale::GRID,
            caption_type: type_scale::CAPTION,
        }
    }
}

// ---------------------------------------------------------------------------
// Prompt routing-target chip (the unified input box)
// ---------------------------------------------------------------------------

/// Where Enter routes from the one shell-first input box. The hotkey flips only this;
/// the typed text is preserved by the [`aterm_core`] input reducer, not by this chip
/// (a state concern, not a UI one) - so the chip is a PURE function of the mode and a
/// toggle can never disturb the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMode {
    Shell,
    Agent,
}

/// The routing-target chip at the input's left edge. SHELL is the neutral chip; AGENT
/// is the accent (`Info`) chip. Toggling cross-fades between the two within
/// `motion.fast` ([`Animation::CrossFade`]); the caret stays one accent blue (the
/// locked decision - recoloring per mode is the owner-confirm alternative).
#[derive(Debug, Clone, Copy)]
pub struct PromptChip {
    pub mode: PromptMode,
    pub label: &'static str,
    pub chip: ChipStyle,
}

impl PromptChip {
    #[must_use]
    pub fn resolve(mode: PromptMode, theme: &Theme) -> Self {
        let (label, variant) = match mode {
            PromptMode::Shell => ("SHELL", ChipVariant::Neutral),
            PromptMode::Agent => ("AGENT", ChipVariant::Info),
        };
        Self {
            mode,
            label,
            chip: ChipStyle::resolve(variant, theme),
        }
    }

    /// The animation that plays when the routing target toggles: a `motion.fast`
    /// cross-fade (NOT a fourth animation - it is the shared [`Animation::CrossFade`]).
    #[must_use]
    pub fn toggle_animation() -> Animation {
        Animation::CrossFade
    }
}

// ---------------------------------------------------------------------------
// Agent card
// ---------------------------------------------------------------------------

/// The agent-card container ([`07-ia-design-language.md`] §5): a `bg.surface` rect with
/// `radius.md`, a 1px hairline, `space.4` padding and a `space.6` gap from neighbors. A
/// header (Duo medium `type.heading`) + status chip sits above a Duo `type.body` prose
/// body capped at `MEASURE_CH`; reasoning/plan text is de-emphasized to `fg.secondary`.
/// This styles WHATEVER the agent-step data model (T-5.10) provides; it owns no data.
#[derive(Debug, Clone, Copy)]
pub struct AgentCardStyle {
    pub fill: Rgba,
    pub border: Rgba,
    pub radius_px: u16,
    pub pad_px: u16,
    pub gap_px: u16,
    /// Duo medium 500 - the step-title register.
    pub heading_type: TypeStyle,
    pub heading_fg: Rgba,
    /// Duo body prose, measure-capped.
    pub body_type: TypeStyle,
    pub body_fg: Rgba,
    /// De-emphasized reasoning/plan text.
    pub reasoning_fg: Rgba,
    /// The ~72ch prose measure (the grid stays uncapped).
    pub measure_ch: u16,
}

impl AgentCardStyle {
    #[must_use]
    pub fn resolve(theme: &Theme) -> Self {
        let c = &theme.colors;
        Self {
            fill: c.bg_surface,
            border: c.hairline,
            radius_px: space::RADIUS_MD,
            pad_px: space::S4,
            gap_px: space::S6,
            heading_type: type_scale::HEADING,
            heading_fg: c.fg_primary,
            body_type: type_scale::BODY,
            body_fg: c.fg_primary,
            reasoning_fg: c.fg_secondary,
            measure_ch: type_scale::MEASURE_CH,
        }
    }
}

// ---------------------------------------------------------------------------
// Risk-gate badge (UI-local 3-state; ALWAYS color + label)
// ---------------------------------------------------------------------------

/// The UI-local risk verdict the badge renders. NOT `aterm_agent::Risk` (the crate
/// boundary forbids the dependency); `aterm-app` maps the agent's `RiskAssessment`
/// onto these three states. The auto-safe default means a proven-safe command is
/// `Allowed`; everything escalated is `NeedsApproval`, and a destructive/blocked
/// verdict is `Blocked`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskState {
    Allowed,
    NeedsApproval,
    Blocked,
}

/// The resolved risk-gate badge. It ALWAYS pairs a `label` with the `chip` color (AC2 /
/// color-blind safety) - color alone never carries the verdict. `gutter_color` is the
/// saturated semantic color the badge contributes to the command's gutter alignment, so
/// a scanning eye reads "gutter color = safety state". The parsed reason (the agent's
/// gloss) is `type.caption` text the caller supplies at draw time - the badge stays
/// independent of `aterm-agent`.
#[derive(Debug, Clone, Copy)]
pub struct RiskBadge {
    pub state: RiskState,
    pub label: &'static str,
    pub chip: ChipStyle,
    pub gutter_color: Rgba,
    /// `type.caption` style for the parsed reason the caller renders beside the badge.
    pub caption_type: TypeStyle,
}

impl RiskBadge {
    /// Resolve the badge for a UI risk state against `theme`. Allowed -> success
    /// "auto"; NeedsApproval -> caution "APPROVE?"; Blocked -> danger "BLOCKED". The
    /// label is always present and non-empty for all three states.
    #[must_use]
    pub fn resolve(state: RiskState, theme: &Theme) -> Self {
        let (label, variant) = match state {
            RiskState::Allowed => ("auto", ChipVariant::Success),
            RiskState::NeedsApproval => ("APPROVE?", ChipVariant::Caution),
            RiskState::Blocked => ("BLOCKED", ChipVariant::Danger),
        };
        let chip = ChipStyle::resolve(variant, theme);
        Self {
            state,
            label,
            // The saturated semantic color (the chip's text color for a semantic
            // variant) is what the gutter shows.
            gutter_color: chip.text,
            chip,
            caption_type: type_scale::CAPTION,
        }
    }
}

// ---------------------------------------------------------------------------
// Motion budget (AC4): exactly three animations, each <= 220ms
// ---------------------------------------------------------------------------

/// The ONLY animations aterm plays ([`07-ia-design-language.md`] §4, Recommendation 7).
/// Capping the set at three protects the 60fps floor; every one is `<= 220ms` with the
/// standard decelerate easing. The routing-chip toggle and the gate-state change are
/// the SAME [`Self::CrossFade`] - there is no fourth kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Animation {
    /// A newly inserted block fades in and rises 4px (`motion.base`).
    BlockInsert,
    /// A chip / badge cross-fades on a state change (`motion.fast`): the gate verdict
    /// and the prompt routing-target toggle both use this.
    CrossFade,
    /// Non-active blocks dim (focus mode), `motion.slow`.
    FocusDim,
}

/// A resolved animation: its duration (ms), decelerate easing control points, and the
/// rise distance (non-zero only for [`Animation::BlockInsert`]).
#[derive(Debug, Clone, Copy)]
pub struct MotionSpec {
    pub duration_ms: u16,
    pub easing: [f32; 4],
    pub rise_px: u16,
}

impl Animation {
    /// The complete allowed set - exactly three (AC4).
    pub const ALL: [Animation; 3] = [Self::BlockInsert, Self::CrossFade, Self::FocusDim];

    /// The motion parameters for this animation, all from [`aterm_tokens::motion`].
    #[must_use]
    pub fn spec(self) -> MotionSpec {
        match self {
            Animation::BlockInsert => MotionSpec {
                duration_ms: motion::BASE_MS,
                easing: motion::EASING_CUBIC_BEZIER,
                rise_px: 4,
            },
            Animation::CrossFade => MotionSpec {
                duration_ms: motion::FAST_MS,
                easing: motion::EASING_CUBIC_BEZIER,
                rise_px: 0,
            },
            Animation::FocusDim => MotionSpec {
                duration_ms: motion::SLOW_MS,
                easing: motion::EASING_CUBIC_BEZIER,
                rise_px: 0,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_tokens::{contrast_ratio, FontRole, ThemeKind};

    fn themes() -> [Theme; 2] {
        [
            *Theme::for_kind(ThemeKind::Light),
            *Theme::for_kind(ThemeKind::Dark),
        ]
    }

    // ----- status chip --------------------------------------------------

    #[test]
    fn chip_variants_pull_only_from_tokens() {
        // AC1: no hardcoded color. Each variant's resolved colors are token fields (or
        // a derived blend of them), pinned for both themes.
        for theme in themes() {
            let c = &theme.colors;
            let neutral = ChipStyle::resolve(ChipVariant::Neutral, &theme);
            assert_eq!(neutral.fill, c.bg_surface);
            assert_eq!(neutral.text, c.fg_secondary);
            assert_eq!(
                neutral.border,
                Some(c.hairline),
                "neutral is the only bordered chip"
            );

            let info = ChipStyle::resolve(ChipVariant::Info, &theme);
            assert_eq!(info.fill, c.accent_primary_weak);
            assert_eq!(info.text, c.accent_primary_text);
            assert_eq!(info.border, None);

            for (variant, sat) in [
                (ChipVariant::Success, c.success),
                (ChipVariant::Caution, c.caution),
                (ChipVariant::Danger, c.danger),
            ] {
                let s = ChipStyle::resolve(variant, &theme);
                let fill = mix(sat, c.bg_surface, WEAK_TINT_T);
                assert_eq!(s.fill, fill, "semantic fill is the derived weak tint");
                assert_eq!(
                    s.text,
                    legible_against(sat, fill, CHIP_MIN_CONTRAST),
                    "semantic chip text is the saturated token, legibility-corrected on its fill"
                );
                assert_eq!(s.border, None, "semantic chips have no border");
            }
        }
    }

    #[test]
    fn chip_geometry_is_flat_and_from_tokens() {
        let s = ChipStyle::resolve(ChipVariant::Neutral, &themes()[0]);
        assert_eq!(s.radius_px, space::RADIUS_SM);
        assert_eq!(s.pad_x, space::S2);
        assert_eq!(s.pad_y, space::S1);
        assert!(
            matches!(s.type_style.font, FontRole::Ui),
            "chips are Quattro"
        );
    }

    #[test]
    fn chip_text_is_legible_on_its_fill() {
        // The "weak tint fill + saturated text" must stay readable in BOTH themes - the
        // reason the derived tint pulls most of the way to the surface.
        for theme in themes() {
            for variant in [
                ChipVariant::Neutral,
                ChipVariant::Info,
                ChipVariant::Success,
                ChipVariant::Caution,
                ChipVariant::Danger,
            ] {
                let s = ChipStyle::resolve(variant, &theme);
                let ratio = contrast_ratio(s.text, s.fill);
                assert!(
                    ratio >= 3.0,
                    "{:?} chip {variant:?}: text-on-fill is {ratio:.2}:1, want >= 3:1",
                    theme.kind
                );
            }
        }
    }

    // ----- gutter marker ------------------------------------------------

    #[test]
    fn gutter_marker_maps_state_to_token_color_and_distinct_shape() {
        for theme in themes() {
            let c = &theme.colors;
            let cases = [
                (
                    GutterMarker::Running,
                    c.accent_primary,
                    GutterShape::PulsingDot,
                ),
                (GutterMarker::Ok, c.success, GutterShape::Tick),
                (GutterMarker::Failed(1), c.danger, GutterShape::Dot),
                (GutterMarker::Unknown, c.fg_muted, GutterShape::HollowDot),
                (
                    GutterMarker::Interactive,
                    c.fg_secondary,
                    GutterShape::Interactive,
                ),
                (GutterMarker::Approximate, c.caution, GutterShape::HalfDot),
            ];
            let mut shapes = Vec::new();
            for (marker, color, shape) in cases {
                let g = GutterStyle::resolve(marker, &theme);
                assert_eq!(g.color, color, "{marker:?} color");
                assert_eq!(g.shape, shape, "{marker:?} shape");
                shapes.push(shape);
            }
            // Every state has a DISTINCT shape (color-blind: shape carries meaning).
            for i in 0..shapes.len() {
                for j in (i + 1)..shapes.len() {
                    assert_ne!(shapes[i], shapes[j], "gutter shapes must all differ");
                }
            }
        }
    }

    #[test]
    fn only_running_pulses_and_failed_carries_its_code() {
        let t = themes()[1];
        assert!(GutterStyle::resolve(GutterMarker::Running, &t).pulsing);
        for m in [
            GutterMarker::Ok,
            GutterMarker::Failed(2),
            GutterMarker::Unknown,
            GutterMarker::Interactive,
            GutterMarker::Approximate,
        ] {
            assert!(!GutterStyle::resolve(m, &t).pulsing, "{m:?} must not pulse");
        }
        assert_eq!(
            GutterStyle::resolve(GutterMarker::Failed(127), &t).exit_code,
            Some(127)
        );
        assert_eq!(
            GutterStyle::resolve(GutterMarker::Ok, &t).exit_code,
            None,
            "a success carries no code"
        );
        assert_eq!(
            GutterStyle::resolve(GutterMarker::Approximate, &t).label,
            Some("approx")
        );
    }

    #[test]
    fn gutter_glyphs_exist_in_the_bundled_grid_font() {
        // The gutter markers render through the Mono GRID font; a glyph missing from the
        // bundled Nerd Font draws `.notdef` (an indistinct box), silently breaking the
        // status indicator - which is exactly what the BMP geometric ●○◐▸ did (absent
        // from this face). A cmap lookup of 0 IS `.notdef`, so every marker glyph must
        // resolve non-zero. Pure font parse: runs on every platform, unlike the
        // macOS-only timeline GPU test (whose "any ink" check a `.notdef` box satisfies).
        use crate::glyph::GlyphRasterizer;
        use crate::text::{FaceStyle, FontFamily};
        let r = GlyphRasterizer::new();
        for marker in [
            GutterMarker::Running,
            GutterMarker::Ok,
            GutterMarker::Failed(1),
            GutterMarker::Unknown,
            GutterMarker::Interactive,
            GutterMarker::Approximate,
        ] {
            let g = GutterStyle::resolve(marker, &themes()[1]);
            let gid = r.glyph_id(FontFamily::Grid, FaceStyle::Regular, g.glyph);
            assert_ne!(
                gid, 0,
                "{marker:?} gutter glyph U+{:04X} is .notdef in the bundled Mono Nerd Font",
                g.glyph as u32
            );
        }
    }

    // ----- prompt routing chip ------------------------------------------

    #[test]
    fn prompt_chip_is_neutral_for_shell_accent_for_agent() {
        for theme in themes() {
            let shell = PromptChip::resolve(PromptMode::Shell, &theme);
            assert_eq!(shell.label, "SHELL");
            assert_eq!(shell.chip.fill, theme.colors.bg_surface);
            assert_eq!(shell.chip.text, theme.colors.fg_secondary);

            let agent = PromptChip::resolve(PromptMode::Agent, &theme);
            assert_eq!(agent.label, "AGENT");
            assert_eq!(agent.chip.fill, theme.colors.accent_primary_weak);
            assert_eq!(agent.chip.text, theme.colors.accent_primary_text);
        }
    }

    #[test]
    fn prompt_toggle_is_a_motion_fast_cross_fade_not_a_fourth_animation() {
        // AC3 (the chip half): the toggle cross-fades within motion.fast. AC4: it is
        // the shared CrossFade animation, so the allowed set stays at three.
        let anim = PromptChip::toggle_animation();
        assert_eq!(anim, Animation::CrossFade);
        assert_eq!(anim.spec().duration_ms, motion::FAST_MS);
        assert!(Animation::ALL.contains(&anim));
    }

    #[test]
    fn prompt_chip_depends_only_on_mode_so_text_is_untouched() {
        // AC3 (the preserve-text half, at the UI layer): the chip is a pure function of
        // the mode, so flipping the mode and flipping it back yields the identical chip
        // - the chip can never be a channel that disturbs the typed text (which the
        // aterm-core input reducer owns and preserves across the toggle).
        let t = themes()[0];
        let shell = PromptChip::resolve(PromptMode::Shell, &t);
        let agent = PromptChip::resolve(PromptMode::Agent, &t);
        let shell_again = PromptChip::resolve(PromptMode::Shell, &t);
        assert_eq!(shell.label, shell_again.label);
        assert_eq!(shell.chip.fill, shell_again.chip.fill);
        assert_ne!(
            shell.chip.fill, agent.chip.fill,
            "the two modes are visually distinct"
        );
    }

    // ----- agent card ---------------------------------------------------

    #[test]
    fn agent_card_is_surface_flat_and_duo() {
        for theme in themes() {
            let c = &theme.colors;
            let card = AgentCardStyle::resolve(&theme);
            assert_eq!(card.fill, c.bg_surface);
            assert_eq!(card.border, c.hairline);
            assert_eq!(card.radius_px, space::RADIUS_MD);
            assert_eq!(card.pad_px, space::S4);
            assert_eq!(card.gap_px, space::S6);
            assert_eq!(card.reasoning_fg, c.fg_secondary);
            assert_eq!(card.measure_ch, type_scale::MEASURE_CH);
            assert!(matches!(card.heading_type.font, FontRole::Prose));
            assert_eq!(
                card.heading_type.weight,
                Some(aterm_tokens::font::WEIGHT_MEDIUM)
            );
            assert!(matches!(card.body_type.font, FontRole::Prose));
        }
    }

    // ----- risk-gate badge ----------------------------------------------

    #[test]
    fn risk_badge_always_has_a_label_beside_its_color_for_all_three_states() {
        // AC2: color is never the only signal - every state pairs a non-empty text
        // label with its semantic color, in BOTH themes.
        for theme in themes() {
            let c = &theme.colors;
            let cases = [
                (RiskState::Allowed, "auto", c.success),
                (RiskState::NeedsApproval, "APPROVE?", c.caution),
                (RiskState::Blocked, "BLOCKED", c.danger),
            ];
            let mut labels = Vec::new();
            for (state, label, sat) in cases {
                let badge = RiskBadge::resolve(state, &theme);
                assert_eq!(badge.label, label);
                assert!(!badge.label.is_empty(), "{state:?} must carry a label");
                // The color is the semantic safety token (legibility-corrected on the
                // chip fill), and the gutter shows that same color.
                let fill = mix(sat, c.bg_surface, WEAK_TINT_T);
                let expected = legible_against(sat, fill, CHIP_MIN_CONTRAST);
                assert_eq!(
                    badge.chip.text, expected,
                    "{state:?} uses the right semantic color"
                );
                assert_eq!(
                    badge.gutter_color, expected,
                    "{state:?} gutter color = the saturated safety color"
                );
                labels.push(badge.label);
            }
            // The three labels are distinct (the verdict reads from text alone).
            assert_ne!(labels[0], labels[1]);
            assert_ne!(labels[1], labels[2]);
            assert_ne!(labels[0], labels[2]);
        }
    }

    // ----- motion budget ------------------------------------------------

    #[test]
    fn motion_is_capped_to_three_animations_each_under_220ms() {
        // AC4: exactly three allowed animations, none over the 220ms ceiling.
        assert_eq!(Animation::ALL.len(), 3);
        for anim in Animation::ALL {
            let spec = anim.spec();
            assert!(
                spec.duration_ms <= motion::SLOW_MS,
                "{anim:?} is {}ms, over the 220ms cap",
                spec.duration_ms
            );
            assert_eq!(spec.easing, motion::EASING_CUBIC_BEZIER);
        }
        // The block-insert rise is the spec'd 4px; the others do not translate.
        assert_eq!(Animation::BlockInsert.spec().rise_px, 4);
        assert_eq!(Animation::CrossFade.spec().rise_px, 0);
        assert_eq!(Animation::FocusDim.spec().rise_px, 0);
    }
}
