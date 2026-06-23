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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: ThemeKind::Light, // light "paper" by default
            initial_cols: 120,
            initial_rows: 32,
        }
    }
}

impl Config {
    /// Load configuration. Currently returns defaults.
    pub fn load() -> Self {
        Self::default()
    }
}
