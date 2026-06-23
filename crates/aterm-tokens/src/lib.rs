//! aterm-tokens — design tokens as typed Rust constants. Leaf crate, no deps.
//!
//! This crate is the compile-time reification of `docs/design/tokens.toml`
//! (which itself mirrors `docs/design/design-system.md`). Values here MUST stay
//! identical to that file; the doc owns intent, the toml owns values, this crate
//! owns the typed surface the renderer consumes.
//!
//! OWNER CONFIRMATION REQUIRED: `accent_primary` (#1A93E8 light / #4DA6F0 dark)
//! is DERIVED + WCAG-checked, NOT sampled from the live iA Writer app. Treat the
//! accent blue (and all contrast claims) as provisional until reconciled against
//! a final palette with a real WCAG library. When `docs/design/tokens.toml`
//! changes, re-sync these consts (ideally via a future `build.rs` codegen pass —
//! TODO(ticket EPIC-4): generate this module from tokens.toml at build time).

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

/// Semantic color roles. Field names mirror `[color.*]` in tokens.toml.
#[derive(Debug, Clone, Copy)]
pub struct SemanticColors {
    pub bg_canvas: Rgba,
    pub bg_surface: Rgba,
    pub bg_surface_alt: Rgba,
    pub fg_primary: Rgba,
    pub fg_secondary: Rgba,
    pub fg_muted: Rgba,
    pub fg_faint: Rgba,
    /// DERIVED accent blue — owner confirmation pending (see crate docs).
    pub accent_primary: Rgba,
    pub accent_primary_text: Rgba,
    pub accent_primary_weak: Rgba,
    pub hairline: Rgba,
    pub hairline_strong: Rgba,
    pub selection_bg: Rgba,
    pub success: Rgba,
    pub caution: Rgba,
    pub danger: Rgba,
    pub info: Rgba,
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
    /// Index into the palette by ANSI color index 0..=15. Out-of-range → black.
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
        bg_canvas: Rgba::hex(0xFAF9F6),
        bg_surface: Rgba::hex(0xF1F0EC),
        bg_surface_alt: Rgba::hex(0xE9E7E1),
        fg_primary: Rgba::hex(0x2A2A28),
        fg_secondary: Rgba::hex(0x5C5B57),
        fg_muted: Rgba::hex(0x8A8984),
        fg_faint: Rgba::hex(0xB5B4AE),
        accent_primary: Rgba::hex(0x1A93E8), // DERIVED — owner confirm
        accent_primary_text: Rgba::hex(0x1577C2),
        accent_primary_weak: Rgba::hex(0xD6EAFB),
        hairline: Rgba::hex(0xE0DED8),
        hairline_strong: Rgba::hex(0xC9C7C0),
        selection_bg: Rgba::hex(0xCFE3F7),
        success: Rgba::hex(0x1E8E5A),
        caution: Rgba::hex(0xB0820E),
        danger: Rgba::hex(0xC2185B),
        info: Rgba::hex(0x1A93E8),
    },
    ansi: AnsiPalette {
        black: Rgba::hex(0x2A2A28),
        red: Rgba::hex(0xC30771),
        green: Rgba::hex(0x10A778),
        yellow: Rgba::hex(0xA8800E),
        blue: Rgba::hex(0x1A6FB0),
        magenta: Rgba::hex(0x7C3F9E),
        cyan: Rgba::hex(0x138D9E),
        white: Rgba::hex(0x5C5B57),
        bright_black: Rgba::hex(0x5C5B57),
        bright_red: Rgba::hex(0xE0306F),
        bright_green: Rgba::hex(0x1EB886),
        bright_yellow: Rgba::hex(0xC39A14),
        bright_blue: Rgba::hex(0x1A93E8),
        bright_magenta: Rgba::hex(0x9B5BC0),
        bright_cyan: Rgba::hex(0x20A5BA),
        bright_white: Rgba::hex(0x2A2A28),
    },
};

/// Dark theme. Values copied verbatim from `[color.dark]` / `[ansi.dark]`.
pub const DARK: Theme = Theme {
    kind: ThemeKind::Dark,
    colors: SemanticColors {
        bg_canvas: Rgba::hex(0x1C1C1C),
        bg_surface: Rgba::hex(0x262626),
        bg_surface_alt: Rgba::hex(0x303030),
        fg_primary: Rgba::hex(0xE6E5E1),
        fg_secondary: Rgba::hex(0xB8B7B2),
        fg_muted: Rgba::hex(0x7A7A75),
        fg_faint: Rgba::hex(0x4A4A46),
        accent_primary: Rgba::hex(0x4DA6F0), // DERIVED — owner confirm
        accent_primary_text: Rgba::hex(0x4DA6F0),
        accent_primary_weak: Rgba::hex(0x1E3A52),
        hairline: Rgba::hex(0x343433),
        hairline_strong: Rgba::hex(0x454544),
        selection_bg: Rgba::hex(0x34465A),
        success: Rgba::hex(0x5FD7A7),
        caution: Rgba::hex(0xE0B341),
        danger: Rgba::hex(0xE85A95),
        info: Rgba::hex(0x4DA6F0),
    },
    ansi: AnsiPalette {
        black: Rgba::hex(0x1C1C1C),
        red: Rgba::hex(0xE85A95),
        green: Rgba::hex(0x5FD7A7),
        yellow: Rgba::hex(0xE0B341),
        blue: Rgba::hex(0x4DA6F0),
        magenta: Rgba::hex(0xB893BE),
        cyan: Rgba::hex(0x4FB8CC),
        white: Rgba::hex(0xE6E5E1),
        bright_black: Rgba::hex(0x5A5A55),
        bright_red: Rgba::hex(0xF277A8),
        bright_green: Rgba::hex(0x7DE6BC),
        bright_yellow: Rgba::hex(0xF3E430),
        bright_blue: Rgba::hex(0x74BFF7),
        bright_magenta: Rgba::hex(0xCBAAD0),
        bright_cyan: Rgba::hex(0x6FCFE0),
        bright_white: Rgba::hex(0xFFFFFF),
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
            Rgba::hex(0xFAF9F6)
        );
    }
}
