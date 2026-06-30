//! Configuration. STUB: hardcoded defaults today. TODO(ticket EPIC-8): load from
//! `~/.config/aterm/config.toml` (theme, font size, keybindings, provider keys).

use aterm_ui::ThemeKind;

use crate::routing::KeyBinding;

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
    /// The mode-toggle hotkey (ticket T-3.3). Default `Cmd-/`; rebindable - via the
    /// `ATERM_TOGGLE_KEY` env override today (e.g. `ctrl+t`), the `config.toml`
    /// loader later (EPIC-8).
    pub toggle_mode: KeyBinding,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: ThemeKind::Light, // light "paper" by default
            initial_cols: 120,
            initial_rows: 32,
            display_link: false,
            toggle_mode: KeyBinding::default_toggle(),
        }
    }
}

impl Config {
    /// Load configuration. Currently defaults, with two env overrides:
    /// `ATERM_DISPLAY_LINK=1` opts into the CADisplayLink vsync clock, and
    /// `ATERM_TOGGLE_KEY` (e.g. `ctrl+t`) rebinds the mode-toggle hotkey - so the
    /// owner can change either without a code change (the full `config.toml` loader
    /// is EPIC-8).
    pub fn load() -> Self {
        let mut cfg = Self::default();
        if matches!(
            std::env::var("ATERM_DISPLAY_LINK").as_deref(),
            Ok("1") | Ok("true")
        ) {
            cfg.display_link = true;
        }
        if let Ok(spec) = std::env::var("ATERM_TOGGLE_KEY") {
            match KeyBinding::parse(&spec) {
                Some(binding) => cfg.toggle_mode = binding,
                None => log::warn!(
                    "ignoring invalid ATERM_TOGGLE_KEY={spec:?}; keeping the default toggle (Cmd-/)"
                ),
            }
        }
        cfg
    }
}
