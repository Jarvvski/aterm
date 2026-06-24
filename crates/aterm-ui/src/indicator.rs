//! The shell-integration indicator's *presentation* (ticket T-2.6).
//!
//! `aterm-core` owns the integration STATE ([`Integration`]); this module is the
//! pure mapping from that state to what the user sees: one iA-restrained glyph, a
//! short label, the "why?" tooltip, and a semantic token color. It is deliberately
//! a plain value-producing function (no GPU, no layout) so it is unit-tested with
//! no window, and so the eventual renderer just paints what it returns.
//!
//! Scope note (T-2.6): this wires the state to a renderable form. The actual draw -
//! placement in the block gutter / status strip, hover behavior for the tooltip - is
//! EPIC-4 visual polish, which supplies the final tokens; the renderer is handed the
//! live [`Integration`] each frame (see [`crate::renderer::Frame`]).

use aterm_core::{Integration, IntegrationStatus};
use aterm_tokens::{Rgba, Theme};

/// The resolved visual form of the integration indicator for one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegrationIndicator {
    /// A single status glyph. iA-restrained: a filled / half / hollow dot, no color
    /// reliance for the shape (the color reinforces, it does not carry the meaning).
    pub glyph: char,
    /// A terse status label for the chip ("Integrated" / "Heuristic" / "No blocks").
    pub label: &'static str,
    /// The one-click "why?" - populated for every non-Integrated state, `None` when
    /// integration is confirmed (there is nothing to explain). Mirrors
    /// [`Integration::why`].
    pub tooltip: Option<&'static str>,
    /// The semantic token color for the glyph (success / caution / muted), pulled
    /// from the active [`Theme`] so the indicator themes with everything else.
    pub color: Rgba,
}

impl IntegrationIndicator {
    /// Resolve the indicator's visual form from the integration state and theme.
    #[must_use]
    pub fn resolve(integration: Integration, theme: &Theme) -> Self {
        let c = &theme.colors;
        match integration.status {
            // Confirmed marks: a filled dot in the success color. No tooltip - there
            // is nothing missing to explain.
            IntegrationStatus::Integrated => Self {
                glyph: '\u{25CF}', // ● BLACK CIRCLE
                label: "Integrated",
                tooltip: None,
                color: c.success,
            },
            // Approximate fallback: a half-filled dot in the caution color, with the
            // "why?" so the degrade is loud, never silent.
            IntegrationStatus::Heuristic => Self {
                glyph: '\u{25D0}', // ◐ CIRCLE WITH LEFT HALF BLACK
                label: "Heuristic",
                tooltip: integration.why(),
                color: c.caution,
            },
            // No integration at all: a hollow dot in the muted color, plus the why.
            IntegrationStatus::None => Self {
                glyph: '\u{25CB}', // ○ WHITE CIRCLE
                label: "No blocks",
                tooltip: integration.why(),
                color: c.fg_muted,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_core::{Integration, IntegrationReason};
    use aterm_tokens::{Theme, ThemeKind};

    fn theme() -> Theme {
        *Theme::for_kind(ThemeKind::Dark)
    }

    #[test]
    fn integrated_is_a_filled_success_dot_with_no_tooltip() {
        let t = theme();
        let ind =
            IntegrationIndicator::resolve(Integration::from(IntegrationReason::Confirmed), &t);
        assert_eq!(ind.glyph, '\u{25CF}');
        assert_eq!(ind.label, "Integrated");
        assert_eq!(ind.tooltip, None, "nothing missing -> no why");
        assert_eq!(ind.color, t.colors.success);
    }

    #[test]
    fn heuristic_states_are_a_caution_half_dot_carrying_the_why() {
        let t = theme();
        // Both heuristic reasons map to the same glyph/label/color but distinct whys.
        for reason in [
            IntegrationReason::Probing,
            IntegrationReason::HooksSilent,
            IntegrationReason::ShimInstallFailed,
        ] {
            let integ = Integration::from(reason);
            let ind = IntegrationIndicator::resolve(integ, &t);
            assert_eq!(ind.glyph, '\u{25D0}', "{reason:?}");
            assert_eq!(ind.label, "Heuristic");
            assert_eq!(ind.color, t.colors.caution);
            assert_eq!(
                ind.tooltip,
                integ.why(),
                "the tooltip is the state's why ({reason:?})"
            );
            assert!(
                ind.tooltip.is_some(),
                "a heuristic state must explain itself"
            );
        }
    }

    #[test]
    fn none_is_a_muted_hollow_dot_with_the_why() {
        let t = theme();
        let integ = Integration::from(IntegrationReason::UnsupportedShell);
        let ind = IntegrationIndicator::resolve(integ, &t);
        assert_eq!(ind.glyph, '\u{25CB}');
        assert_eq!(ind.label, "No blocks");
        assert_eq!(ind.color, t.colors.fg_muted);
        assert_eq!(ind.tooltip, integ.why());
        assert!(ind.tooltip.is_some());
    }

    #[test]
    fn the_three_statuses_are_visually_distinct() {
        // The glyph alone (not just color) must distinguish the states - the shape
        // carries the meaning so a monochrome/colorblind reading still works.
        let t = theme();
        let glyphs: Vec<char> = [
            IntegrationReason::Confirmed,
            IntegrationReason::HooksSilent,
            IntegrationReason::UnsupportedShell,
        ]
        .into_iter()
        .map(|r| IntegrationIndicator::resolve(Integration::from(r), &t).glyph)
        .collect();
        assert_eq!(glyphs.len(), 3);
        assert!(
            glyphs[0] != glyphs[1] && glyphs[1] != glyphs[2] && glyphs[0] != glyphs[2],
            "each status needs a distinct glyph, got {glyphs:?}"
        );
    }
}
