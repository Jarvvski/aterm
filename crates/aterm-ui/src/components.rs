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

use aterm_tokens::{legible_against, motion, space, type_scale, Mode, Rgba, Theme, TypeStyle};

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
            // An agent transcript step (ticket T-5.10): a neutral secondary marker,
            // labelled so it reads as agent (not command) content. Real iconography is
            // EPIC-4 (T-4.6); reuses an already-bundled, font-coverage-tested glyph.
            GutterMarker::Agent => Self {
                shape: GutterShape::Interactive,
                glyph: '\u{f0da}', // nf-fa-caret-right (already bundled + tested)
                color: c.fg_secondary,
                exit_code: None,
                label: Some("agent"),
                pulsing: false,
            },
        }
    }
}

/// The flat-rectangle geometry of a command block (no heavy box; delimited by a single
/// `hairline` TOP rule per block, none above the first - the mock / T-9.3). The command
/// line is `fg.primary`; DEFAULT (uncolored) output dims to `fg.secondary` via
/// [`crate::text::resolve_output_color`] (the mock's `ink-dim` body) while explicit
/// ANSI/RGB is preserved; a collapsed block's "... +N lines" affordance is `fg.muted`.
/// All colors via tokens.
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
// Command-block META (right-aligned status dot + duration, the mock's block-meta)
// ---------------------------------------------------------------------------

/// The success-duration threshold in seconds (ticket T-9.3): an exit-0 command that ran
/// at least this long earns the loud `success` dot, while a quicker one reads as a plain
/// `fg_muted` dot - the mock's distinction between `git status` at 0.06s (faint) and
/// `cargo build` at 12.71s (success). A tuning default, NOT a protocol constant.
pub const META_SUCCESS_SECS: f64 = 1.0;

/// The right-aligned command-block META (ticket T-9.3): a status dot + a duration /
/// state caption, mirroring the vision mock's `.block-meta`. The dot COLOR reads the
/// exit state; the "exit N" (failure), "running", "approx", and "tui" text labels keep
/// every state legible WITHOUT color (color-blind safety - color is never the only
/// signal). It is revealed on block hover via the SHARED [`Animation::FocusDim`] slot
/// (NOT a fourth animation), so the 3-animation motion budget still holds.
///
/// This is the mock's reconciliation of the old gutter-status contract (T-4.6): the
/// gutter now carries the accent `❯` prompt glyph, and the status dot + duration move
/// here. The running pulse dot is preserved - relocated into this meta (its `pulsing`).
#[derive(Debug, Clone, Copy)]
pub struct BlockMetaStyle {
    /// The status dot's glyph: a filled dot for running / exit-0 / failure, and the
    /// distinct hollow / half / caret shapes for the other states (the shape reinforces
    /// the color for a color-blind eye, as in [`GutterStyle`]).
    pub dot_glyph: char,
    pub dot_shape: GutterShape,
    /// The dot's semantic color: accent (running), `fg_muted` (quick exit-0), `success`
    /// (a longer exit-0), `danger` (failure), `caution` (approximate), ...
    pub dot_color: Rgba,
    /// The caption text color - the mock's faint meta tone (`fg_muted`) for every state.
    pub text_color: Rgba,
    /// The failed exit code, if any (rendered as "exit N" in the caption).
    pub exit_code: Option<i32>,
    /// A terse state label ("approx" / "tui") for a non-exit state; `None` otherwise.
    pub label: Option<&'static str>,
    /// True only for a running block - the single pulsing-dot animation.
    pub pulsing: bool,
    /// `type.caption` - the small meta register (the mock's ~0.82em).
    pub type_style: TypeStyle,
}

impl BlockMetaStyle {
    /// Resolve the meta for a block's [`GutterMarker`] and (optional, finished) duration.
    /// Exit-0 splits by duration into a faint or a success dot; a failure is always the
    /// `danger` dot; the other states inherit the gutter glyph / shape / color.
    #[must_use]
    pub fn resolve(marker: GutterMarker, duration_secs: Option<f64>, theme: &Theme) -> Self {
        let c = &theme.colors;
        let g = GutterStyle::resolve(marker, theme);
        // Exit 0 splits by duration: a quick command is a faint plain dot (the mock's
        // instant-command meta); only a longer one earns the success color. Both keep a
        // filled-dot shape - the mock treats every finished command as a colored dot, and
        // the "exit N" label (present only on failure) carries the color-blind distinction.
        let (dot_glyph, dot_shape, dot_color) = match marker {
            GutterMarker::Ok => {
                let long = duration_secs.is_some_and(|d| d >= META_SUCCESS_SECS);
                let color = if long { c.success } else { c.fg_muted };
                ('\u{f111}', GutterShape::Dot, color) // nf-fa-circle (filled dot)
            }
            GutterMarker::Failed(_) => ('\u{f111}', GutterShape::Dot, c.danger),
            // Running / Unknown / Interactive / Approximate keep their gutter treatment.
            _ => (g.glyph, g.shape, g.color),
        };
        Self {
            dot_glyph,
            dot_shape,
            dot_color,
            text_color: c.fg_muted,
            exit_code: g.exit_code,
            label: g.label,
            pulsing: g.pulsing,
            type_style: type_scale::CAPTION,
        }
    }

    /// The animation the meta fades in with on block hover: the SHARED `motion.slow`
    /// [`Animation::FocusDim`] slot (NOT a fourth animation - the budget stays three).
    #[must_use]
    pub fn reveal_animation() -> Animation {
        Animation::FocusDim
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

/// How far the mode accent is pulled toward the canvas for the mode-chip FILL - the
/// mock's `color-mix(in srgb, var(--mode) 13%, transparent)` composited over the
/// canvas, stored opaque. Low enough to stay a quiet tint, high enough that the
/// same-hue accent text clears the UI contrast floor on it (guarded by
/// [`tests::mode_chip_text_is_legible_on_its_tint`]).
const MODE_CHIP_TINT_T: f32 = 0.87;

/// The routing-target MODE CHIP at the input's right edge (ticket T-9.4). Both modes
/// now render as a pill in the CURRENT MODE COLOR (the mock's `--mode` chip, ADR-0011):
/// a 1px accent border, a ~13% accent tint fill, and accent text - shell blue
/// ([`aterm_tokens::SemanticColors::accent_primary`]) / agent purple (`accent_agent`).
/// Toggling cross-fades between the two within `motion.fast` ([`Animation::CrossFade`]);
/// the caret + prompt glyph shift to the same mode accent (no longer one fixed blue -
/// this realizes the two-accent model). The `label` is the mock's title-case
/// "Shell" / "Agent".
#[derive(Debug, Clone, Copy)]
pub struct PromptChip {
    pub mode: PromptMode,
    pub label: &'static str,
    pub chip: ChipStyle,
}

impl PromptChip {
    #[must_use]
    pub fn resolve(mode: PromptMode, theme: &Theme) -> Self {
        let c = &theme.colors;
        let (label, accent) = match mode {
            PromptMode::Shell => ("Shell", c.mode_accent(Mode::Shell)),
            PromptMode::Agent => ("Agent", c.mode_accent(Mode::Agent)),
        };
        // A pill in the current mode color: a 1px accent border, a ~13% accent tint fill
        // over the canvas, and accent text pulled the minimal amount to clear the UI
        // contrast floor on that tint (a near-no-op, like the Info chip).
        let fill = mix(accent, c.bg_canvas, MODE_CHIP_TINT_T);
        let text = legible_against(accent, fill, CHIP_MIN_CONTRAST);
        let chip = ChipStyle {
            fill,
            text,
            border: Some(accent),
            radius_px: space::RADIUS_SM,
            pad_x: space::S2,
            pad_y: space::S1,
            type_style: type_scale::LABEL,
        };
        Self { mode, label, chip }
    }

    /// The animation that plays when the routing target toggles: a `motion.fast`
    /// cross-fade (NOT a fourth animation - it is the shared [`Animation::CrossFade`]).
    #[must_use]
    pub fn toggle_animation() -> Animation {
        Animation::CrossFade
    }
}

// ---------------------------------------------------------------------------
// Autonomy-mode indicator chip (the always-visible safety posture)
// ---------------------------------------------------------------------------

/// The autonomy tier the agent runs under (ticket T-5.11), surfaced as an
/// always-visible indicator so the user can never lose track of the safety posture
/// (AC4). UI-local, NOT `aterm_agent::AutonomyMode` (the crate boundary forbids the
/// dependency); `aterm-app` maps the agent's mode onto these three. The ladder runs
/// most-conservative to most-permissive; `AutoSafe` is the shipped default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutonomyMode {
    /// Every command requires explicit confirmation.
    AskAlways,
    /// The default: a proven-safe, non-shell-active command auto-runs.
    AutoSafe,
    /// A session-scoped widening that also auto-runs non-shell-active Caution.
    AutoRunInSession,
}

/// The resolved autonomy-mode indicator chip. Like every chip it ALWAYS pairs a
/// `label` with the `chip` color (color-blind safety): the more permissive the tier,
/// the louder the color (neutral -> success -> caution), but the label alone always
/// carries the meaning. A mode switch cross-fades within `motion.fast`
/// ([`Animation::CrossFade`]) - the same animation the routing chip uses, no fourth.
#[derive(Debug, Clone, Copy)]
pub struct AutonomyChip {
    pub mode: AutonomyMode,
    pub label: &'static str,
    pub chip: ChipStyle,
}

impl AutonomyChip {
    /// Resolve the indicator for `mode` against `theme`. ask-always is the neutral
    /// chrome chip; auto-safe is `success` (the safe-by-default posture); the
    /// auto-run-in-session widening is `caution` so the looser posture reads as
    /// slightly louder. The label is always present and non-empty.
    #[must_use]
    pub fn resolve(mode: AutonomyMode, theme: &Theme) -> Self {
        let (label, variant) = match mode {
            AutonomyMode::AskAlways => ("ASK", ChipVariant::Neutral),
            AutonomyMode::AutoSafe => ("AUTO-SAFE", ChipVariant::Success),
            AutonomyMode::AutoRunInSession => ("AUTO-RUN", ChipVariant::Caution),
        };
        Self {
            mode,
            label,
            chip: ChipStyle::resolve(variant, theme),
        }
    }

    /// The animation that plays when the autonomy tier switches: the shared
    /// `motion.fast` cross-fade (NOT a fourth animation).
    #[must_use]
    pub fn switch_animation() -> Animation {
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

    // ----- command-block meta -------------------------------------------

    #[test]
    fn block_meta_maps_state_to_token_color_shape_and_label() {
        // T-9.3 AC1/AC2: the meta dot color reads the exit state, the shape reinforces
        // it, and the failure "exit N" / approx labels keep it legible without color.
        // The caption text is the faint meta tone (fg_muted) in every state, both themes.
        for theme in themes() {
            let c = &theme.colors;
            // Exit 0, long: the loud success dot.
            let ok_long = BlockMetaStyle::resolve(GutterMarker::Ok, Some(12.71), &theme);
            assert_eq!(ok_long.dot_color, c.success);
            assert_eq!(ok_long.dot_shape, GutterShape::Dot);
            assert_eq!(ok_long.exit_code, None);
            assert!(!ok_long.pulsing);
            // Exit 0, quick (< threshold): a faint plain dot (the mock's instant meta).
            let ok_quick = BlockMetaStyle::resolve(GutterMarker::Ok, Some(0.06), &theme);
            assert_eq!(ok_quick.dot_color, c.fg_muted);
            assert_eq!(ok_quick.dot_shape, GutterShape::Dot);
            // Failure: the danger dot carrying its exit code.
            let failed = BlockMetaStyle::resolve(GutterMarker::Failed(1), Some(2.34), &theme);
            assert_eq!(failed.dot_color, c.danger);
            assert_eq!(failed.exit_code, Some(1));
            // Running: the accent pulsing dot (the single running animation, relocated
            // from the gutter into the meta).
            let running = BlockMetaStyle::resolve(GutterMarker::Running, None, &theme);
            assert_eq!(running.dot_color, c.accent_primary);
            assert!(running.pulsing);
            // Approximate: a caution half-dot, labelled so the approximation is loud.
            let approx = BlockMetaStyle::resolve(GutterMarker::Approximate, Some(0.01), &theme);
            assert_eq!(approx.dot_color, c.caution);
            assert_eq!(approx.dot_shape, GutterShape::HalfDot);
            assert_eq!(approx.label, Some("approx"));

            // Every state's caption ink is the faint meta tone.
            for meta in [ok_long, ok_quick, failed, running, approx] {
                assert_eq!(meta.text_color, c.fg_muted);
                assert!(matches!(meta.type_style.font, FontRole::Ui));
            }
        }
    }

    #[test]
    fn block_meta_reveal_reuses_the_focus_dim_slot_not_a_fourth_animation() {
        // T-9.3 AC3: the hover fade reuses an EXISTING animation slot (FocusDim), so the
        // allowed set stays at three and the <=220ms motion budget holds.
        let anim = BlockMetaStyle::reveal_animation();
        assert_eq!(anim, Animation::FocusDim);
        assert!(Animation::ALL.contains(&anim));
        assert!(anim.spec().duration_ms <= motion::SLOW_MS);
    }

    // ----- prompt routing chip ------------------------------------------

    #[test]
    fn prompt_chip_is_mode_colored_for_both_modes() {
        // T-9.4 / ADR-0011: both modes are a pill in the current MODE color (no longer
        // neutral SHELL / accent AGENT). Shell = accent_primary (blue), Agent =
        // accent_agent (purple); border + text are the accent, the fill its ~13% tint.
        for theme in themes() {
            let c = &theme.colors;
            let shell = PromptChip::resolve(PromptMode::Shell, &theme);
            assert_eq!(shell.label, "Shell");
            assert_eq!(shell.chip.border, Some(c.accent_primary));
            let shell_fill = mix(c.accent_primary, c.bg_canvas, MODE_CHIP_TINT_T);
            assert_eq!(shell.chip.fill, shell_fill);
            assert_eq!(
                shell.chip.text,
                legible_against(c.accent_primary, shell_fill, CHIP_MIN_CONTRAST)
            );

            let agent = PromptChip::resolve(PromptMode::Agent, &theme);
            assert_eq!(agent.label, "Agent");
            assert_eq!(agent.chip.border, Some(c.accent_agent));
            let agent_fill = mix(c.accent_agent, c.bg_canvas, MODE_CHIP_TINT_T);
            assert_eq!(agent.chip.fill, agent_fill);
            assert_eq!(
                agent.chip.text,
                legible_against(c.accent_agent, agent_fill, CHIP_MIN_CONTRAST)
            );

            // The two modes are visually distinct (shell blue vs agent purple).
            assert_ne!(shell.chip.border, agent.chip.border);
            assert_ne!(shell.chip.fill, agent.chip.fill);
        }
    }

    #[test]
    fn mode_chip_text_is_legible_on_its_tint() {
        // The mode color reads as the chip TEXT on its own ~13% tint fill; it must clear
        // the 3:1 UI-contrast floor in both themes and both modes (color-blind + a11y).
        for theme in themes() {
            for mode in [PromptMode::Shell, PromptMode::Agent] {
                let chip = PromptChip::resolve(mode, &theme).chip;
                let ratio = contrast_ratio(chip.text, chip.fill);
                assert!(
                    ratio >= 3.0,
                    "{:?} {mode:?} chip: text-on-tint is {ratio:.2}:1, want >= 3:1",
                    theme.kind
                );
            }
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

    // ----- autonomy-mode indicator --------------------------------------

    #[test]
    fn autonomy_chip_always_has_a_distinct_label_beside_its_color_for_all_tiers() {
        // AC4 + AC6: the autonomy posture is always shown as a non-empty text label
        // paired with a color, in BOTH themes, and the three tiers are distinguishable
        // from text alone (never color-only).
        for theme in themes() {
            let cases = [
                AutonomyMode::AskAlways,
                AutonomyMode::AutoSafe,
                AutonomyMode::AutoRunInSession,
            ];
            let mut labels = Vec::new();
            for mode in cases {
                let chip = AutonomyChip::resolve(mode, &theme);
                assert!(!chip.label.is_empty(), "{mode:?} must carry a label");
                labels.push(chip.label);
            }
            assert_ne!(labels[0], labels[1]);
            assert_ne!(labels[1], labels[2]);
            assert_ne!(labels[0], labels[2]);
        }
        // The autonomy switch reuses the shared cross-fade (no fourth animation).
        assert_eq!(AutonomyChip::switch_animation(), Animation::CrossFade);
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
