//! Configuration. STUB: hardcoded defaults today. TODO(ticket EPIC-8): load from
//! `~/.config/aterm/config.toml` (theme, font size, keybindings, provider keys).

use aterm_ui::ThemeKind;

/// App configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub theme: ThemeKind,
    /// Initial grid size (cols, rows) before the first resize.
    pub initial_cols: u16,
    pub initial_rows: u16,
    /// Opt into the self-bridged CADisplayLink vsync clock (macOS). Off by
    /// default; the proven winit-driven present loop drives presentation until the
    /// link path is validated on real ProMotion hardware (ticket T-1.5 AC3).
    pub display_link: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: ThemeKind::Light, // light "paper" by default
            initial_cols: 120,
            initial_rows: 32,
            display_link: false,
        }
    }
}

impl Config {
    /// Load configuration. Currently defaults, with one env override:
    /// `ATERM_DISPLAY_LINK=1` opts into the CADisplayLink vsync clock so the
    /// owner can validate it on hardware without a code change.
    pub fn load() -> Self {
        let mut cfg = Self::default();
        if matches!(
            std::env::var("ATERM_DISPLAY_LINK").as_deref(),
            Ok("1") | Ok("true")
        ) {
            cfg.display_link = true;
        }
        cfg
    }
}
