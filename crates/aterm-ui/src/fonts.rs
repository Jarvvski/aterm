//! Bundled-font loading. The iM Writing Nerd Font faces (OFL 1.1) live under
//! `assets/fonts/` and are embedded into the binary so the terminal grid font is
//! always present regardless of what the user has installed.
//!
//! Family names are sourced from `aterm-tokens::font` so the token layer remains
//! the single source of truth for which family each role uses. The grid renderer
//! ([`crate::grid_render`]) rasterizes these via swash directly; the raw bytes are
//! the interface (no `FontSystem` indirection).

/// The grid (monospace) faces, embedded at compile time. These are the
/// load-bearing terminal faces ([`aterm_tokens::font::GRID`]).
pub const GRID_REGULAR: &[u8] =
    include_bytes!("../../../assets/fonts/iMWritingMonoNerdFontMono-Regular.ttf");
pub const GRID_BOLD: &[u8] =
    include_bytes!("../../../assets/fonts/iMWritingMonoNerdFontMono-Bold.ttf");
pub const GRID_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/iMWritingMonoNerdFontMono-Italic.ttf");
pub const GRID_BOLD_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/iMWritingMonoNerdFontMono-BoldItalic.ttf");

/// Proportional faces for prose / chrome ([`aterm_tokens::font::PROSE`] / `UI`).
pub const PROSE_REGULAR: &[u8] =
    include_bytes!("../../../assets/fonts/iMWritingDuoNerdFont-Regular.ttf");
pub const PROSE_BOLD: &[u8] = include_bytes!("../../../assets/fonts/iMWritingDuoNerdFont-Bold.ttf");
pub const UI_REGULAR: &[u8] =
    include_bytes!("../../../assets/fonts/iMWritingQuatNerdFont-Regular.ttf");
pub const UI_BOLD: &[u8] = include_bytes!("../../../assets/fonts/iMWritingQuatNerdFont-Bold.ttf");

/// The bundled TTF bytes for a `(family, face)` - the single source of truth the
/// rasterizer ([`crate::glyph`]) and the prose shaper ([`crate::prose`]) both route
/// through, so the register split lives in one place.
///
/// The grid family ships all four faces. Prose (Duo) and UI (Quattro) ship only
/// Regular + Bold, so a synthetic Italic / BoldItalic request collapses to the upright
/// weight (Italic -> Regular, BoldItalic -> Bold) rather than tofu - real slanted prose
/// is a later text-polish pass, not a T-4.3 requirement.
#[must_use]
pub(crate) fn face_bytes(
    family: crate::text::FontFamily,
    face: crate::text::FaceStyle,
) -> &'static [u8] {
    use crate::text::{FaceStyle, FontFamily};
    match family {
        FontFamily::Grid => match face {
            FaceStyle::Regular => GRID_REGULAR,
            FaceStyle::Bold => GRID_BOLD,
            FaceStyle::Italic => GRID_ITALIC,
            FaceStyle::BoldItalic => GRID_BOLD_ITALIC,
        },
        FontFamily::Prose => match face {
            FaceStyle::Regular | FaceStyle::Italic => PROSE_REGULAR,
            FaceStyle::Bold | FaceStyle::BoldItalic => PROSE_BOLD,
        },
        FontFamily::Ui => match face {
            FaceStyle::Regular | FaceStyle::Italic => UI_REGULAR,
            FaceStyle::Bold | FaceStyle::BoldItalic => UI_BOLD,
        },
    }
}

/// All bundled faces (grid + prose + UI), for callers that want to iterate them
/// (e.g. a future `FontSystem`-backed proportional path for agent prose - T-3.6 /
/// T-4.6). The grid renderer uses the typed `GRID_*` constants directly.
#[must_use]
pub fn all_bundled() -> [&'static [u8]; 8] {
    [
        GRID_REGULAR,
        GRID_BOLD,
        GRID_ITALIC,
        GRID_BOLD_ITALIC,
        PROSE_REGULAR,
        PROSE_BOLD,
        UI_REGULAR,
        UI_BOLD,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_faces_are_nonempty() {
        // Guards against a broken/missing unzip of the font pack.
        assert!(GRID_REGULAR.len() > 1000);
        assert!(GRID_BOLD.len() > 1000);
        assert!(PROSE_REGULAR.len() > 1000);
        assert!(UI_REGULAR.len() > 1000);
    }
}
