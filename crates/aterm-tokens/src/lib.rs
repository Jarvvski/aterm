//! aterm-tokens — design tokens as typed Rust constants. Leaf crate, no deps.
//!
//! This crate is the compile-time reification of `docs/design/tokens.toml`
//! (which itself mirrors `docs/design/design-system.md`). Values here MUST stay
//! identical to that file; the doc owns intent, the toml owns values, this crate
//! owns the typed surface the renderer consumes.
//!
//! The palette is the warm two-theme set from the vision mock adopted as the UI
//! north star in ADR-0011 (`docs/design/vision-mock/AtermWindow.dc.html`): a warm
//! near-black dark and a warm "paper" light. It carries a two-accent mode model —
//! shell blue (`accent_primary`) plus agent purple (`accent_agent`), resolved by
//! [`SemanticColors::mode_accent`] — an elevated-surface tone (`bg_elev`) for
//! popovers/menus, and surface tints (`hairline`, `selection_bg`, the weak fills)
//! that the mock defines as alpha over the canvas and this crate stores
//! pre-composited to opaque (they only ever sit on canvas, and opaque keeps the
//! WCAG/legibility math and the renderer's background rects correct). The old
//! "derived accent, owner-confirm" note is resolved by ADR-0011: the accent is
//! now the mock's blue by owner decision.
//!
//! Every contrast claim is recomputed against these hexes by the `wcag_*` tests;
//! the one intentional sub-AA pair (`fg_muted`, the faint meta tone) is annotated
//! there with its permitted use. When `docs/design/tokens.toml` changes, re-sync
//! these consts (a future `build.rs` codegen pass could generate this module from
//! the toml).

#![allow(clippy::unreadable_literal)]

/// 8-bit-per-channel sRGB color with alpha. Stored as four `u8` so it is cheap
/// to embed as a `const`, and convertible to the linear `f32` quad the GPU wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    /// Construct from a packed `0xRRGGBB` value (alpha = 255).
    pub const fn hex(rgb: u32) -> Self {
        Self {
            r: ((rgb >> 16) & 0xFF) as u8,
            g: ((rgb >> 8) & 0xFF) as u8,
            b: (rgb & 0xFF) as u8,
            a: 0xFF,
        }
    }

    /// Construct from a packed `0xRRGGBBAA` value.
    pub const fn hexa(rgba: u32) -> Self {
        Self {
            r: ((rgba >> 24) & 0xFF) as u8,
            g: ((rgba >> 16) & 0xFF) as u8,
            b: ((rgba >> 8) & 0xFF) as u8,
            a: (rgba & 0xFF) as u8,
        }
    }

    /// Pack back into `0xRRGGBBAA`.
    pub const fn to_u32(self) -> u32 {
        ((self.r as u32) << 24) | ((self.g as u32) << 16) | ((self.b as u32) << 8) | (self.a as u32)
    }

    /// sRGB → linear, returned as a wgpu-friendly `[r, g, b, a]` in 0.0..=1.0.
    /// Suitable for a clear color / vertex color. Uses the standard sRGB EOTF.
    pub fn to_linear_f32(self) -> [f32; 4] {
        fn c(u: u8) -> f32 {
            let s = u as f32 / 255.0;
            if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        }
        [c(self.r), c(self.g), c(self.b), self.a as f32 / 255.0]
    }

    /// Non-color-managed `[r, g, b, a]` in 0.0..=1.0 (raw byte / 255). Use only
    /// where a non-sRGB surface is in play; prefer `to_linear_f32`.
    pub fn to_unorm_f32(self) -> [f32; 4] {
        [
            self.r as f32 / 255.0,
            self.g as f32 / 255.0,
            self.b as f32 / 255.0,
            self.a as f32 / 255.0,
        ]
    }

    /// WCAG 2.1 relative luminance (0.0 black .. 1.0 white): linearize each sRGB
    /// channel, then weight `0.2126 R + 0.7152 G + 0.0722 B`. Alpha is ignored -
    /// contrast is defined over opaque colors and every token here is opaque.
    ///
    /// The linearization matches [`Rgba::to_linear_f32`] (sRGB EOTF, 0.04045
    /// breakpoint); WCAG's published 0.03928 is a known rounding of the same sRGB
    /// boundary, so the corrected 0.04045 is used (as modern tooling does). The
    /// difference is far below the assertion margins.
    pub fn relative_luminance(self) -> f32 {
        fn lin(u: u8) -> f32 {
            let s = u as f32 / 255.0;
            if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * lin(self.r) + 0.7152 * lin(self.g) + 0.0722 * lin(self.b)
    }
}

/// WCAG 2.1 contrast ratio between two opaque colors, in `1.0..=21.0`. Order does
/// not matter: `(L_lighter + 0.05) / (L_darker + 0.05)`. Used to verify the token
/// palette meets WCAG thresholds for real, rather than trusting the dossier's
/// hand-estimated ratios. Reference: black-on-white is exactly `21.0`, any color
/// against itself is `1.0`.
pub fn contrast_ratio(a: Rgba, b: Rgba) -> f32 {
    let (la, lb) = (a.relative_luminance(), b.relative_luminance());
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Per-channel sRGB interpolation `a + (b - a)*t` (`t` clamped to `0.0..=1.0`),
/// alpha taken from `a`. A blunt value pull, not perceptual color science - just
/// enough to nudge a color toward black/white for [`legible_against`].
fn lerp_rgba(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Rgba {
        r: mix(a.r, b.r),
        g: mix(a.g, b.g),
        b: mix(a.b, b.b),
        a: a.a,
    }
}

/// Pull `fg` toward black or white just far enough to reach `min_ratio` WCAG
/// contrast against `bg`, and return it (unchanged if it already clears the
/// floor).
///
/// The pull is toward whichever of black/white can reach the higher contrast
/// against `bg` (black on a light bg, white on a dark bg), found by a binary
/// search on the sRGB blend so the hue is broadly preserved while the value is
/// moved the minimal amount. If even the full endpoint cannot reach `min_ratio`
/// (impossible for a normal bg, since black-or-white maximizes contrast) the
/// endpoint is returned as the best effort.
///
/// This is the pure primitive behind the light-"paper" ANSI legibility remap.
/// Per `design-system.md` §3 that remap is a **renderer** concern, never a token
/// edit - the shipped palette values stay verbatim; a caller (the renderer/app)
/// decides whether and where to apply this. `min_ratio <= 1.0` makes it a no-op.
#[must_use]
pub fn legible_against(fg: Rgba, bg: Rgba, min_ratio: f32) -> Rgba {
    if contrast_ratio(fg, bg) >= min_ratio {
        return fg;
    }
    let black = Rgba {
        r: 0,
        g: 0,
        b: 0,
        a: fg.a,
    };
    let white = Rgba {
        r: 0xFF,
        g: 0xFF,
        b: 0xFF,
        a: fg.a,
    };
    // The endpoint that maximizes achievable contrast against `bg`.
    let target = if contrast_ratio(black, bg) >= contrast_ratio(white, bg) {
        black
    } else {
        white
    };
    // Best effort if even the full endpoint cannot reach the floor.
    if contrast_ratio(target, bg) < min_ratio {
        return target;
    }
    // Smallest blend toward `target` that meets the floor. Binary search is valid
    // because the predicate "contrast(blend, bg) >= floor" is single-threshold
    // (false then true) in t: blending toward the max-contrast endpoint only lifts
    // contrast past the crossover, and the sub-floor precondition (predicate false
    // at t=0) plus the endpoint-meets-floor guard above (predicate true at t=1)
    // bracket that threshold. Raw contrast can dip once near a luminance crossover,
    // but entirely inside the still-sub-floor region, so the boolean stays monotone.
    // 16 iterations → ~1/65536 of the blend factor, well under a u8 step.
    let mut lo = 0.0f32; // fg
    let mut hi = 1.0f32; // target
    for _ in 0..16 {
        let mid = (lo + hi) * 0.5;
        if contrast_ratio(lerp_rgba(fg, target, mid), bg) >= min_ratio {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    lerp_rgba(fg, target, hi)
}

/// The two themes that ship day one. Both are drawn from one hue family so ANSI
/// output and chrome never clash (see design-system.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemeKind {
    /// Light "paper".
    Light,
    /// Dark.
    Dark,
}

impl ThemeKind {
    /// The other theme - the target of a runtime light↔dark toggle.
    #[must_use]
    pub const fn toggle(self) -> ThemeKind {
        match self {
            ThemeKind::Light => ThemeKind::Dark,
            ThemeKind::Dark => ThemeKind::Light,
        }
    }
}

/// Semantic color roles. Field names mirror `[color.*]` in tokens.toml.
#[derive(Debug, Clone, Copy)]
pub struct SemanticColors {
    pub bg_canvas: Rgba,
    pub bg_surface: Rgba,
    pub bg_surface_alt: Rgba,
    /// Elevated surface — popovers, the gate approve-menu, the completion menu
    /// (the mock's `--bg-elev`). The mock has a single tone above canvas, so
    /// `bg_surface` shares this value; they stay distinct tokens so downstream
    /// surfaces can diverge without another token migration.
    pub bg_elev: Rgba,
    pub fg_primary: Rgba,
    pub fg_secondary: Rgba,
    /// The faint meta tone (the mock's `ink-faint`): timestamps, exit captions,
    /// placeholder text, the "+N lines" affordance. Intentionally sub-AA — see
    /// the `fg_muted_is_intentionally_sub_aa` test.
    pub fg_muted: Rgba,
    pub fg_faint: Rgba,
    /// Shell-mode accent (blue). Pair with `accent_agent` via
    /// [`SemanticColors::mode_accent`].
    pub accent_primary: Rgba,
    /// Agent-mode accent (purple) — the mock's `--agent`, sanctioned by ADR-0011.
    pub accent_agent: Rgba,
    pub accent_primary_text: Rgba,
    pub accent_primary_weak: Rgba,
    pub hairline: Rgba,
    pub hairline_strong: Rgba,
    pub selection_bg: Rgba,
    pub success: Rgba,
    pub caution: Rgba,
    /// Low-emphasis caution fill — the gate card background (the mock's
    /// `--warn-bg`). Stored with alpha; composited over the canvas.
    pub caution_weak: Rgba,
    pub danger: Rgba,
    pub info: Rgba,
    /// Window traffic-light dot colors (the mock's warm macOS-control hues), left to
    /// right: close (red), minimize (amber), zoom (green). These are CHROME constants -
    /// identical in both themes (they are the standard macOS control colors, not
    /// palette-derived). NO LONGER DRAWN: the T-9.9 rework (2026-07-02) replaced the
    /// drawn dots with the REAL native traffic-light buttons; the tokens remain as the
    /// mock's palette record (per the ADR-0011 amendment).
    pub chrome_close: Rgba,
    pub chrome_minimize: Rgba,
    pub chrome_zoom: Rgba,
}

/// Which input mode an accent resolves for. Mirrors the domain `Mode (Shell |
/// Agent)` (`docs/agents/domain.md`); defined here rather than imported from
/// `aterm-core` because `aterm-tokens` is a leaf crate. The app maps its
/// `InputModel` mode onto this to ask a theme for "the current mode color".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Enter routes to the live shell — accent is `accent_primary` (blue).
    Shell,
    /// Enter routes to the agent loop — accent is `accent_agent` (purple).
    Agent,
}

impl SemanticColors {
    /// The accent for the current input mode: `Shell -> accent_primary` (blue),
    /// `Agent -> accent_agent` (purple). This is the mock's `--mode` custom
    /// property: the prompt glyph, caret tint, and mode chip ask for the resolved
    /// mode color instead of branching on the mode themselves. The two-accent
    /// model is sanctioned by ADR-0011 (relaxing the old one-accent rule).
    #[must_use]
    pub const fn mode_accent(&self, mode: Mode) -> Rgba {
        match mode {
            Mode::Shell => self.accent_primary,
            Mode::Agent => self.accent_agent,
        }
    }
}

/// ANSI 16-color palette, index 0..=15. Mirrors `[ansi.*]` in tokens.toml.
#[derive(Debug, Clone, Copy)]
pub struct AnsiPalette {
    pub black: Rgba,
    pub red: Rgba,
    pub green: Rgba,
    pub yellow: Rgba,
    pub blue: Rgba,
    pub magenta: Rgba,
    pub cyan: Rgba,
    pub white: Rgba,
    pub bright_black: Rgba,
    pub bright_red: Rgba,
    pub bright_green: Rgba,
    pub bright_yellow: Rgba,
    pub bright_blue: Rgba,
    pub bright_magenta: Rgba,
    pub bright_cyan: Rgba,
    pub bright_white: Rgba,
}

impl AnsiPalette {
    /// Index into the 16-color palette by ANSI color index 0..=15.
    /// Out-of-range (16..=255) → black; callers wanting full 256-color
    /// resolution should use [`AnsiPalette::indexed`].
    pub const fn by_index(&self, idx: u8) -> Rgba {
        match idx {
            0 => self.black,
            1 => self.red,
            2 => self.green,
            3 => self.yellow,
            4 => self.blue,
            5 => self.magenta,
            6 => self.cyan,
            7 => self.white,
            8 => self.bright_black,
            9 => self.bright_red,
            10 => self.bright_green,
            11 => self.bright_yellow,
            12 => self.bright_blue,
            13 => self.bright_magenta,
            14 => self.bright_cyan,
            15 => self.bright_white,
            _ => self.black,
        }
    }

    /// Resolve a full 256-color ANSI index to an `Rgba`.
    ///
    /// - `0..=15` resolve through THIS theme's 16-color palette ([`by_index`]),
    ///   so the low colors belong to the theme's hue family.
    /// - `16..=231` are the standard xterm 6×6×6 color cube (theme-independent):
    ///   each channel takes one of `{0, 95, 135, 175, 215, 255}`.
    /// - `232..=255` are the standard 24-step grayscale ramp (`8 + n*10`),
    ///   theme-independent.
    ///
    /// The 216-cube and grayscale ramp are fixed by the xterm specification, not
    /// by taste, so they are computed rather than themed - matching every other
    /// terminal. Only the first 16 entries carry the theme.
    ///
    /// [`by_index`]: AnsiPalette::by_index
    pub const fn indexed(&self, idx: u8) -> Rgba {
        match idx {
            0..=15 => self.by_index(idx),
            16..=231 => {
                // 6×6×6 cube. n in 0..=215; component level 0 → 0, else 55+40*level.
                let n = idx - 16;
                let r = n / 36;
                let g = (n / 6) % 6;
                let b = n % 6;
                Rgba {
                    r: cube_level(r),
                    g: cube_level(g),
                    b: cube_level(b),
                    a: 0xFF,
                }
            }
            232..=255 => {
                // 24-step grayscale ramp: 8, 18, 28, ... 238.
                let v = 8 + (idx - 232) * 10;
                Rgba {
                    r: v,
                    g: v,
                    b: v,
                    a: 0xFF,
                }
            }
        }
    }

    /// Return a copy of this 16-color palette with every entry pulled (via
    /// [`legible_against`]) to clear `min_ratio` contrast against `bg`; entries
    /// already clearing the floor are unchanged.
    ///
    /// This is the light-"paper" legibility remap: on a light background the
    /// saturated bright ANSI colors (bright cyan/yellow especially) wash out, so
    /// the renderer applies this against `bg_canvas` to keep terminal output
    /// readable. **Intended for light-background themes only** - a dark theme's
    /// intentionally-dim slots (e.g. bright-black/comment gray sit near the dark
    /// canvas on purpose) would be wrongly lifted, so the caller gates on a light
    /// background before applying.
    #[must_use]
    pub fn with_fg_legibility(&self, bg: Rgba, min_ratio: f32) -> AnsiPalette {
        let f = |c: Rgba| legible_against(c, bg, min_ratio);
        AnsiPalette {
            black: f(self.black),
            red: f(self.red),
            green: f(self.green),
            yellow: f(self.yellow),
            blue: f(self.blue),
            magenta: f(self.magenta),
            cyan: f(self.cyan),
            white: f(self.white),
            bright_black: f(self.bright_black),
            bright_red: f(self.bright_red),
            bright_green: f(self.bright_green),
            bright_yellow: f(self.bright_yellow),
            bright_blue: f(self.bright_blue),
            bright_magenta: f(self.bright_magenta),
            bright_cyan: f(self.bright_cyan),
            bright_white: f(self.bright_white),
        }
    }
}

/// One channel of the xterm 6×6×6 color cube: level 0 → 0, levels 1..=5 →
/// `55 + 40*level` (i.e. 95, 135, 175, 215, 255).
const fn cube_level(level: u8) -> u8 {
    if level == 0 {
        0
    } else {
        55 + level * 40
    }
}

/// A complete theme: semantic roles + ANSI palette.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub kind: ThemeKind,
    pub colors: SemanticColors,
    pub ansi: AnsiPalette,
}

/// Light "paper" theme. Values copied verbatim from `[color.light]` / `[ansi.light]`.
pub const LIGHT: Theme = Theme {
    kind: ThemeKind::Light,
    colors: SemanticColors {
        bg_canvas: Rgba::hex(0xFAF7EF),           // --bg
        bg_surface: Rgba::hex(0xF2EDE1),          // = bg_elev (the mock's one raise)
        bg_surface_alt: Rgba::hex(0xE9E2D1),      // derived further raise (hover/output)
        bg_elev: Rgba::hex(0xF2EDE1),             // --bg-elev
        fg_primary: Rgba::hex(0x26231B),          // --ink
        fg_secondary: Rgba::hex(0x6C6555),        // --ink-dim
        fg_muted: Rgba::hex(0xA89F8C),            // --ink-faint (faint meta; sub-AA)
        fg_faint: Rgba::hex(0xBCB4A3),            // derived fainter step (disabled)
        accent_primary: Rgba::hex(0x2F7DC2),      // --accent
        accent_agent: Rgba::hex(0x7458BD),        // --agent
        accent_primary_text: Rgba::hex(0x2B73B4), // darker accent, 4.65:1 (AA body)
        // The mock's alpha tints, pre-composited over the canvas (they only ever
        // sit on canvas). Opaque so contrast/legibility math and the renderer's
        // background rects behave exactly as the pre-reskin tokens did.
        accent_primary_weak: Rgba::hex(0xE2E8EA), // accent @ 12% over canvas
        hairline: Rgba::hex(0xE5E2DA),            // ink @ 10% over canvas (--hairline)
        hairline_strong: Rgba::hex(0xD4D1C9),     // ink @ 18% (section dividers)
        selection_bg: Rgba::hex(0xC5D7E3),        // accent @ 26% over canvas
        success: Rgba::hex(0x5C8A56),             // --ok
        caution: Rgba::hex(0xB57D2C),             // --warn
        caution_weak: Rgba::hex(0xF4EDDF),        // warn @ 8% over canvas (--warn-bg)
        danger: Rgba::hex(0xBF5A40),              // --err
        info: Rgba::hex(0x2F7DC2),                // = accent_primary
        chrome_close: Rgba::hex(0xE0655A),        // traffic-light red (chrome, both themes)
        chrome_minimize: Rgba::hex(0xDFA63F),     // traffic-light amber (chrome, both themes)
        chrome_zoom: Rgba::hex(0x7CAE5B),         // traffic-light green (chrome, both themes)
    },
    // ANSI re-tuned to the warm family (structure per T-4.2; hues warmed). On light
    // "paper" the base colors carry (index 7/15 are dark foregrounds), and the light
    // brights (cyan/yellow/green) sit sub-3:1 by design — the renderer's light
    // legibility remap lifts them. Eyeball vs real output is deferred to EPIC-7.
    ansi: AnsiPalette {
        black: Rgba::hex(0x26231B),
        red: Rgba::hex(0xB0502F),
        green: Rgba::hex(0x4E7D48),
        yellow: Rgba::hex(0x9C6A22),
        blue: Rgba::hex(0x2C6EA9),
        magenta: Rgba::hex(0x63499F),
        cyan: Rgba::hex(0x2F7D74),
        white: Rgba::hex(0x6C6555),
        bright_black: Rgba::hex(0xA89F8C),
        bright_red: Rgba::hex(0xC96A44),
        bright_green: Rgba::hex(0x77A56A),
        bright_yellow: Rgba::hex(0xDCB45A),
        bright_blue: Rgba::hex(0x3D88CC),
        bright_magenta: Rgba::hex(0x8F74CF),
        bright_cyan: Rgba::hex(0x57C3B6),
        bright_white: Rgba::hex(0x26231B),
    },
};

/// Dark theme. Values copied verbatim from `[color.dark]` / `[ansi.dark]`.
pub const DARK: Theme = Theme {
    kind: ThemeKind::Dark,
    colors: SemanticColors {
        bg_canvas: Rgba::hex(0x1B1915),           // --bg
        bg_surface: Rgba::hex(0x221F19),          // = bg_elev (the mock's one raise)
        bg_surface_alt: Rgba::hex(0x2B2820),      // derived further raise (hover/output)
        bg_elev: Rgba::hex(0x221F19),             // --bg-elev
        fg_primary: Rgba::hex(0xECE6D8),          // --ink
        fg_secondary: Rgba::hex(0x9A9382),        // --ink-dim
        fg_muted: Rgba::hex(0x5E584B),            // --ink-faint (faint meta; sub-AA)
        fg_faint: Rgba::hex(0x4A453B),            // derived fainter step (disabled)
        accent_primary: Rgba::hex(0x3D88CC),      // --accent
        accent_agent: Rgba::hex(0x9D86D6),        // --agent
        accent_primary_text: Rgba::hex(0x3D88CC), // = accent_primary, 4.67:1 (AA body)
        // The mock's alpha tints, pre-composited over the canvas (they only ever
        // sit on canvas). Opaque so contrast/legibility math and the renderer's
        // background rects behave exactly as the pre-reskin tokens did.
        accent_primary_weak: Rgba::hex(0x1F262B), // accent @ 12% over canvas
        hairline: Rgba::hex(0x2D2A26),            // ink @ 8.5% over canvas (--hairline)
        hairline_strong: Rgba::hex(0x3C3A34),     // ink @ 16% (section dividers)
        selection_bg: Rgba::hex(0x243645),        // accent @ 26% over canvas
        success: Rgba::hex(0x82AC79),             // --ok
        caution: Rgba::hex(0xD59A4A),             // --warn
        caution_weak: Rgba::hex(0x2C251A),        // warn @ 9% over canvas (--warn-bg)
        danger: Rgba::hex(0xD47257),              // --err
        info: Rgba::hex(0x3D88CC),                // = accent_primary
        chrome_close: Rgba::hex(0xE0655A),        // traffic-light red (chrome, both themes)
        chrome_minimize: Rgba::hex(0xDFA63F),     // traffic-light amber (chrome, both themes)
        chrome_zoom: Rgba::hex(0x7CAE5B),         // traffic-light green (chrome, both themes)
    },
    // ANSI re-tuned to the warm family (structure per T-4.2; hues warmed). On the
    // warm near-black canvas every entry is comfortably legible; brights are the
    // lighter/more-saturated siblings. Eyeball vs real output is deferred to EPIC-7.
    ansi: AnsiPalette {
        black: Rgba::hex(0x1B1915),
        red: Rgba::hex(0xD06E54),
        green: Rgba::hex(0x85B078),
        yellow: Rgba::hex(0xD2A15A),
        blue: Rgba::hex(0x4D8FCA),
        magenta: Rgba::hex(0xA98FD6),
        cyan: Rgba::hex(0x6BB0A6),
        white: Rgba::hex(0xCFC8B8),
        bright_black: Rgba::hex(0x5E584B),
        bright_red: Rgba::hex(0xE08A70),
        bright_green: Rgba::hex(0x9CC590),
        bright_yellow: Rgba::hex(0xE6B56A),
        bright_blue: Rgba::hex(0x6BA6DD),
        bright_magenta: Rgba::hex(0xBBA6E6),
        bright_cyan: Rgba::hex(0x8FC9BF),
        bright_white: Rgba::hex(0xECE6D8),
    },
};

impl Theme {
    /// Resolve a theme by kind.
    pub const fn for_kind(kind: ThemeKind) -> &'static Theme {
        match kind {
            ThemeKind::Light => &LIGHT,
            ThemeKind::Dark => &DARK,
        }
    }

    /// The resolved mode accent for this theme (see
    /// [`SemanticColors::mode_accent`]).
    #[must_use]
    pub const fn mode_accent(&self, mode: Mode) -> Rgba {
        self.colors.mode_accent(mode)
    }
}

/// Font family names. Mirrors `[font]` in tokens.toml and is wired to the faces
/// bundled under `assets/fonts/` (iM Writing Nerd Font, OFL 1.1).
pub mod font {
    /// Terminal grid (constant advance, mandatory): input echo, output, code, diffs.
    /// Maps to `iMWritingMonoNerdFontMono-*.ttf`.
    pub const GRID: &str = "iM Writing Mono Nerd Font Mono";
    /// Agent prose / transcript body. Maps to `iMWritingDuoNerdFont-*.ttf`.
    pub const PROSE: &str = "iM Writing Duo";
    /// Dense chrome: status strip, chips, palette. Maps to `iMWritingQuatNerdFont-*.ttf`.
    pub const UI: &str = "iM Writing Quattro";
    /// Used until Duo/Quattro are first-class in the layout engine.
    pub const FALLBACK_PROPORTIONAL: &str = "system-ui";

    pub const WEIGHT_REGULAR: u16 = 400;
    pub const WEIGHT_MEDIUM: u16 = 500;
    pub const WEIGHT_SEMIBOLD: u16 = 600;
    pub const WEIGHT_BOLD: u16 = 700;
}

/// Which font family a type role uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontRole {
    Grid,
    Prose,
    Ui,
}

/// A single entry in the type scale. Mirrors `[type.*]` in tokens.toml.
#[derive(Debug, Clone, Copy)]
pub struct TypeStyle {
    /// Logical points; the renderer scales by the backing (HiDPI) factor.
    pub size_pt: f32,
    /// Unitless line-height multiplier.
    pub line_height: f32,
    pub font: FontRole,
    /// Optional weight override; `None` → `WEIGHT_REGULAR`.
    pub weight: Option<u16>,
}

/// The type scale. Mirrors `[type.*]`.
pub mod type_scale {
    use super::{FontRole, TypeStyle};

    /// Caps prose width (grid is uncapped). iA middle line-length option.
    pub const MEASURE_CH: u16 = 72;

    pub const GRID: TypeStyle = TypeStyle {
        size_pt: 13.0,
        line_height: 1.30,
        font: FontRole::Grid,
        weight: None,
    };
    pub const BODY: TypeStyle = TypeStyle {
        size_pt: 14.0,
        line_height: 1.50,
        font: FontRole::Prose,
        weight: None,
    };
    pub const HEADING: TypeStyle = TypeStyle {
        size_pt: 16.0,
        line_height: 1.35,
        font: FontRole::Prose,
        weight: Some(super::font::WEIGHT_MEDIUM),
    };
    pub const LABEL: TypeStyle = TypeStyle {
        size_pt: 11.0,
        line_height: 1.30,
        font: FontRole::Ui,
        weight: None,
    };
    pub const CAPTION: TypeStyle = TypeStyle {
        size_pt: 10.5,
        line_height: 1.30,
        font: FontRole::Ui,
        weight: None,
    };
}

/// Spacing scale (px, 4px base) + radii + hairline width. Mirrors `[space]`.
pub mod space {
    pub const S0: u16 = 0;
    pub const S1: u16 = 4; // inline gaps (chip padding)
    pub const S2: u16 = 8; // label-to-value, icon-to-text
    pub const S3: u16 = 12; // intra-block padding
    pub const S4: u16 = 16; // block content padding / horizontal gutter
    pub const S6: u16 = 24; // between blocks (vertical rhythm)
    pub const S8: u16 = 32; // section breaks, agent-card outer margin
    pub const S12: u16 = 48; // top/bottom canvas breathing room

    pub const RADIUS_SM: u16 = 4; // chips, badges
    pub const RADIUS_MD: u16 = 6; // agent card
    pub const HAIRLINE_WIDTH: u16 = 1;
}

/// Motion durations (ms) + easing. Mirrors `[motion]`. All <= 220ms; must never
/// threaten the 60fps floor (120fps ProMotion).
pub mod motion {
    pub const FAST_MS: u16 = 90; // gate badge cross-fade, routing-chip toggle
    pub const BASE_MS: u16 = 140; // block insert (fade + 4px rise)
    pub const SLOW_MS: u16 = 220; // focus-mode dim ramp
    /// Decelerate easing control points.
    pub const EASING_CUBIC_BEZIER: [f32; 4] = [0.2, 0.0, 0.0, 1.0];
}

/// Caret tokens. Mirrors `[caret]`.
pub mod caret {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CaretStyle {
        Bar,
        Block,
        Underline,
    }

    /// Default = thin blue bar; honor DECSCUSR block/underline in raw apps.
    pub const DEFAULT_STYLE: CaretStyle = CaretStyle::Bar;
    pub const BAR_WIDTH_PX: u16 = 2;
    /// Block caret fill opacity; glyph drawn in bg_canvas (inverse).
    pub const BLOCK_OPACITY: f32 = 0.70;
    pub const BLINK_SOFT: bool = true;
    pub const BLINK_SUPPRESS_WHILE_TYPING: bool = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_unpacks_channels() {
        let c = Rgba::hex(0xFAF9F6);
        assert_eq!((c.r, c.g, c.b, c.a), (0xFA, 0xF9, 0xF6, 0xFF));
    }

    #[test]
    fn hexa_roundtrips() {
        let c = Rgba::hexa(0x11223344);
        assert_eq!(c.to_u32(), 0x11223344);
    }

    #[test]
    fn linear_clear_is_in_unit_range() {
        let [r, g, b, a] = LIGHT.colors.bg_canvas.to_linear_f32();
        for v in [r, g, b, a] {
            assert!((0.0..=1.0).contains(&v));
        }
    }

    #[test]
    fn ansi_index_maps() {
        assert_eq!(DARK.ansi.by_index(12), DARK.ansi.bright_blue);
        assert_eq!(DARK.ansi.by_index(255), DARK.ansi.black); // out-of-range → black
    }

    #[test]
    fn theme_for_kind_resolves() {
        assert_eq!(Theme::for_kind(ThemeKind::Dark).kind, ThemeKind::Dark);
        assert_eq!(
            Theme::for_kind(ThemeKind::Light).colors.bg_canvas,
            Rgba::hex(0xFAF7EF)
        );
    }

    #[test]
    fn theme_switch_returns_distinct_value_sets() {
        // AC: switching Theme returns the correct value set (the two themes are
        // genuinely different palettes, resolved by kind).
        let light = Theme::for_kind(ThemeKind::Light);
        let dark = Theme::for_kind(ThemeKind::Dark);
        assert_ne!(light.colors.bg_canvas, dark.colors.bg_canvas);
        assert_ne!(light.colors.fg_primary, dark.colors.fg_primary);
        assert_eq!(light.colors.fg_primary, Rgba::hex(0x26231B));
        assert_eq!(dark.colors.fg_primary, Rgba::hex(0xECE6D8));
    }

    #[test]
    fn contrast_ratio_endpoints_are_correct() {
        // Sanity-check the WCAG computation against its known fixed points before
        // trusting it on the palette: black-on-white is the 21:1 maximum, and any
        // color against itself is 1:1.
        let white = Rgba::hex(0xFFFFFF);
        let black = Rgba::hex(0x000000);
        assert!((contrast_ratio(white, black) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(black, white) - 21.0).abs() < 0.01); // order-free
        assert!((contrast_ratio(white, white) - 1.0).abs() < 1e-4);
        assert!(
            (contrast_ratio(LIGHT.colors.accent_primary, LIGHT.colors.accent_primary) - 1.0).abs()
                < 1e-4
        );
    }

    #[test]
    fn wcag_contrast_key_pairs_meet_thresholds() {
        // AC: recompute WCAG contrast for the warm-palette key pairs with a REAL
        // computation (the pre-reskin ratios were estimates). For BOTH themes,
        // every ratio below is measured against the new mock hexes:
        //   - fg.primary on canvas          >= 7:1   (AAA body)
        //   - fg.secondary on canvas        >= 4.5:1 (AA body)
        //   - accent.primary_text on canvas >= 4.5:1 (AA small text — its whole job)
        //   - accent.primary / accent.agent >= 3:1   (AA large/UI: caret, glyph, chip)
        //   - success / caution / danger    >= 3:1   (AA large/UI: gutter, gate, chip)
        // The one intentional exception (fg_muted, sub-AA) is asserted separately in
        // `fg_muted_is_intentionally_sub_aa`.
        for theme in [&LIGHT, &DARK] {
            let c = &theme.colors;
            let bg = c.bg_canvas;
            let aaa_body = [("fg_primary", c.fg_primary)];
            let aa_body = [
                ("fg_secondary", c.fg_secondary),
                ("accent_primary_text", c.accent_primary_text),
            ];
            let aa_ui = [
                ("accent_primary", c.accent_primary),
                ("accent_agent", c.accent_agent),
                ("success", c.success),
                ("caution", c.caution),
                ("danger", c.danger),
            ];
            for (name, col) in aaa_body {
                let r = contrast_ratio(col, bg);
                assert!(
                    r >= 7.0,
                    "{:?}: {name} on canvas is {r:.2}:1, want >= 7:1 (AAA)",
                    theme.kind
                );
            }
            for (name, col) in aa_body {
                let r = contrast_ratio(col, bg);
                assert!(
                    r >= 4.5,
                    "{:?}: {name} on canvas is {r:.2}:1, want >= 4.5:1 (AA body)",
                    theme.kind
                );
            }
            for (name, col) in aa_ui {
                let r = contrast_ratio(col, bg);
                assert!(
                    r >= 3.0,
                    "{:?}: {name} on canvas is {r:.2}:1, want >= 3:1 (AA large/UI)",
                    theme.kind
                );
            }
        }
    }

    #[test]
    fn fg_muted_is_intentionally_sub_aa() {
        // The mock's faint ink tone (`--ink-faint`) is deliberately below the 3:1
        // UI floor: it carries only de-emphasized, non-essential meta — timestamps,
        // exit captions, placeholder text, the "+N lines" affordance — never text
        // that is the sole carrier of essential information. Recorded here so the
        // sub-AA choice is explicit and reviewed, not accidental (ADR-0011 / T-9.1).
        // If a future palette lifts it past 3:1, promote it and delete this guard.
        for theme in [&LIGHT, &DARK] {
            let r = contrast_ratio(theme.colors.fg_muted, theme.colors.bg_canvas);
            assert!(
                r < 3.0,
                "{:?}: fg_muted on canvas is {r:.2}:1 — it now clears 3:1; promote it \
                 and drop this annotation",
                theme.kind
            );
        }
    }

    #[test]
    fn mode_accent_resolves_per_mode_and_theme() {
        // AC: the "current mode accent" resolver returns shell->primary,
        // agent->agent, for both modes and both themes, and the two accents differ.
        for theme in [&LIGHT, &DARK] {
            let c = &theme.colors;
            assert_eq!(
                c.mode_accent(Mode::Shell),
                c.accent_primary,
                "{:?} shell",
                theme.kind
            );
            assert_eq!(
                c.mode_accent(Mode::Agent),
                c.accent_agent,
                "{:?} agent",
                theme.kind
            );
            assert_ne!(
                c.accent_primary, c.accent_agent,
                "{:?}: the two accents differ",
                theme.kind
            );
            // The Theme wrapper delegates to the same resolver.
            assert_eq!(theme.mode_accent(Mode::Shell), c.accent_primary);
            assert_eq!(theme.mode_accent(Mode::Agent), c.accent_agent);
        }
    }

    #[test]
    fn chrome_traffic_dots_are_distinct_and_shared_across_themes() {
        // The mock's traffic-light hues, kept as the palette record (no longer drawn -
        // the T-9.9 rework uses the REAL native buttons). They are CHROME - the standard
        // macOS control colors - so they are IDENTICAL in both themes and the three
        // differ from each other (a red / amber / green a scanning eye can tell apart).
        for theme in [&LIGHT, &DARK] {
            let c = &theme.colors;
            assert_ne!(c.chrome_close, c.chrome_minimize);
            assert_ne!(c.chrome_minimize, c.chrome_zoom);
            assert_ne!(c.chrome_close, c.chrome_zoom);
        }
        assert_eq!(LIGHT.colors.chrome_close, DARK.colors.chrome_close);
        assert_eq!(LIGHT.colors.chrome_minimize, DARK.colors.chrome_minimize);
        assert_eq!(LIGHT.colors.chrome_zoom, DARK.colors.chrome_zoom);
        // The mock's exact warm hues.
        assert_eq!(DARK.colors.chrome_close, Rgba::hex(0xE0655A));
        assert_eq!(DARK.colors.chrome_minimize, Rgba::hex(0xDFA63F));
        assert_eq!(DARK.colors.chrome_zoom, Rgba::hex(0x7CAE5B));
    }

    #[test]
    fn theme_kind_toggles() {
        assert_eq!(ThemeKind::Light.toggle(), ThemeKind::Dark);
        assert_eq!(ThemeKind::Dark.toggle(), ThemeKind::Light);
        assert_eq!(ThemeKind::Light.toggle().toggle(), ThemeKind::Light);
    }

    #[test]
    fn indexed_low_16_resolve_the_themed_palette() {
        // Anchor the low-16 path through the public `indexed` entry to ground-truth
        // dossier hex (NOT to `by_index`, which would be tautological), so a
        // mis-wired theme slot is actually caught.
        assert_eq!(LIGHT.ansi.indexed(2), Rgba::hex(0x4E7D48)); // light green
        assert_eq!(LIGHT.ansi.indexed(12), Rgba::hex(0x3D88CC)); // light bright_blue
        assert_eq!(DARK.ansi.indexed(1), Rgba::hex(0xD06E54)); // dark red
        assert_eq!(DARK.ansi.indexed(11), Rgba::hex(0xE6B56A)); // dark bright_yellow
                                                                // Contract: 0..=15 delegate to the 16-color accessor (not the cube formula).
        for i in 0u8..=15 {
            assert_eq!(DARK.ansi.indexed(i), DARK.ansi.by_index(i));
        }
    }

    #[test]
    fn indexed_cube_and_grayscale_match_xterm() {
        let p = DARK.ansi;
        // Cube corners (theme-independent): 16 = (0,0,0), 231 = (5,5,5),
        // 196 = (5,0,0) pure cube red.
        assert_eq!(p.indexed(16), Rgba::hex(0x000000));
        assert_eq!(p.indexed(231), Rgba::hex(0xFFFFFF));
        assert_eq!(p.indexed(196), Rgba::hex(0xFF0000));
        // A mid cube cell 16+1*36+2*6+3 = 67 = (1,2,3) -> (95,135,175).
        assert_eq!(p.indexed(67), Rgba::hex(0x5F87AF));
        // Grayscale ramp endpoints: 232 -> 8, 255 -> 238.
        assert_eq!(p.indexed(232), Rgba::hex(0x080808));
        assert_eq!(p.indexed(255), Rgba::hex(0xEEEEEE));
        // Cube + grayscale are theme-independent: light resolves them identically.
        assert_eq!(LIGHT.ansi.indexed(196), DARK.ansi.indexed(196));
        assert_eq!(LIGHT.ansi.indexed(244), DARK.ansi.indexed(244));
    }

    #[test]
    fn legible_against_is_a_noop_above_floor() {
        // fg.primary on canvas is ~13:1; asking for a 3:1 floor changes nothing.
        let fg = LIGHT.colors.fg_primary;
        let bg = LIGHT.colors.bg_canvas;
        assert!(contrast_ratio(fg, bg) >= 3.0);
        assert_eq!(legible_against(fg, bg, 3.0), fg);
        // A floor at/below 1.0 is always a no-op (every pair is >= 1:1).
        assert_eq!(
            legible_against(LIGHT.ansi.bright_cyan, bg, 1.0),
            LIGHT.ansi.bright_cyan
        );
    }

    #[test]
    fn legible_against_raises_a_sub_floor_pair_and_picks_the_right_direction() {
        // On a LIGHT bg the pull is toward black (darker fg) and clears the floor.
        let light_bg = LIGHT.colors.bg_canvas;
        let cyan = LIGHT.ansi.bright_cyan;
        assert!(
            contrast_ratio(cyan, light_bg) < 3.0,
            "precondition: the risk is real"
        );
        let fixed = legible_against(cyan, light_bg, 3.0);
        assert!(
            contrast_ratio(fixed, light_bg) >= 3.0,
            "remapped bright cyan must clear 3:1 on light paper (got {:.2})",
            contrast_ratio(fixed, light_bg)
        );
        // Endpoint selection (toward black on a light bg) pinned per-channel, so it
        // is verified independent of the contrast math: every channel is pulled DOWN.
        assert!(
            fixed.r <= cyan.r && fixed.g <= cyan.g && fixed.b <= cyan.b,
            "toward-black pull lowers every channel ({fixed:?} vs {cyan:?})"
        );
        // On a DARK bg a too-dim color is pulled toward white (lighter).
        let dark_bg = DARK.colors.bg_canvas;
        let dim = Rgba::hex(0x303030); // barely above the dark canvas
        assert!(contrast_ratio(dim, dark_bg) < 3.0);
        let lit = legible_against(dim, dark_bg, 3.0);
        assert!(contrast_ratio(lit, dark_bg) >= 3.0);
        // Endpoint selection (toward white on a dark bg) pinned per-channel: every
        // channel is pulled UP.
        assert!(
            lit.r >= dim.r && lit.g >= dim.g && lit.b >= dim.b,
            "toward-white pull raises every channel ({lit:?} vs {dim:?})"
        );
    }

    #[test]
    fn light_paper_bright_cyan_yellow_are_legible_after_the_remap() {
        // AC (the riskiest combo): bright cyan + bright yellow on light "paper".
        // The verbatim dossier values fail a 3:1 legibility floor; the renderer's
        // legibility remap (with_fg_legibility against bg_canvas) brings them up.
        let bg = LIGHT.colors.bg_canvas;
        let base = LIGHT.ansi;
        assert!(
            contrast_ratio(base.bright_cyan, bg) < 3.0
                && contrast_ratio(base.bright_yellow, bg) < 3.0,
            "precondition: the raw light brights are sub-3:1 (cyan {:.2}, yellow {:.2})",
            contrast_ratio(base.bright_cyan, bg),
            contrast_ratio(base.bright_yellow, bg),
        );
        let remapped = base.with_fg_legibility(bg, 3.0);
        for (name, c) in [
            ("bright_cyan", remapped.bright_cyan),
            ("bright_yellow", remapped.bright_yellow),
        ] {
            assert!(
                contrast_ratio(c, bg) >= 3.0,
                "{name} must be legible (>=3:1) on light paper after remap, got {:.2}",
                contrast_ratio(c, bg)
            );
        }
        // Every entry of the remapped light palette clears the floor, and an
        // already-legible entry (the dark `black` slot) is untouched.
        for i in 0u8..=15 {
            assert!(contrast_ratio(remapped.by_index(i), bg) >= 3.0);
        }
        assert_eq!(
            remapped.black, base.black,
            "an already-legible slot is unchanged"
        );
    }
}
