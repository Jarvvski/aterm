//! Configuration. STUB: hardcoded defaults today. TODO(ticket EPIC-8): load from
//! `~/.config/aterm/config.toml` (theme, font size, keybindings, provider keys).

use aterm_agent::AutonomyMode;
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
    /// The baseline autonomy tier a NEW session starts at (ticket T-5.11). The
    /// shipped default is AUTO-SAFE (the locked decision); a session may widen or
    /// narrow at runtime but reverts to this baseline on a new session.
    pub default_autonomy: AutonomyMode,
    /// The autonomy-cycle hotkey (ticket T-5.11): steps the live tier through
    /// ask-always -> auto-safe -> auto-run-in-session. Default `Cmd-Shift-A`;
    /// rebindable via the `ATERM_AUTONOMY_KEY` env override (e.g. `ctrl+a`).
    pub autonomy_cycle: KeyBinding,
    /// The sidebar-toggle hotkey (ticket T-9.2): flips the toggle-sidebar intent. Default
    /// `Cmd-B`; rebindable via the `ATERM_SIDEBAR_KEY` env override. The sidebar panel is
    /// EPIC-10; today this just flips the intent (a no-op stub the panel will consume).
    pub toggle_sidebar: KeyBinding,
    /// The help/modes-explainer hotkey (ticket T-9.5): toggles the one-input-two-destinations
    /// screen. Default `Cmd-?` (Cmd-Shift-/); rebindable via the `ATERM_HELP_KEY` env override.
    pub toggle_help: KeyBinding,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: ThemeKind::Light, // light "paper" by default
            initial_cols: 120,
            initial_rows: 32,
            display_link: false,
            toggle_mode: KeyBinding::default_toggle(),
            default_autonomy: AutonomyMode::AutoSafe, // the locked AUTO-SAFE default
            autonomy_cycle: KeyBinding::default_autonomy_cycle(),
            toggle_sidebar: KeyBinding::default_sidebar_toggle(),
            toggle_help: KeyBinding::default_help(),
        }
    }
}

/// Parse an autonomy tier from a config/env spec (case-insensitive). Recognizes the
/// tier names + a couple of friendly aliases; `None` on an unrecognized spec.
fn parse_autonomy(spec: &str) -> Option<AutonomyMode> {
    match spec.trim().to_ascii_lowercase().as_str() {
        "ask" | "ask-always" | "ask_always" => Some(AutonomyMode::AskAlways),
        "auto-safe" | "auto_safe" | "autosafe" | "safe" => Some(AutonomyMode::AutoSafe),
        "auto-run" | "auto-run-in-session" | "auto_run_in_session" | "auto-run-session" => {
            Some(AutonomyMode::AutoRunInSession)
        }
        _ => None,
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
        if let Ok(spec) = std::env::var("ATERM_AUTONOMY") {
            match parse_autonomy(&spec) {
                Some(mode) => cfg.default_autonomy = mode,
                None => log::warn!(
                    "ignoring invalid ATERM_AUTONOMY={spec:?}; keeping the default (auto-safe)"
                ),
            }
        }
        if let Ok(spec) = std::env::var("ATERM_AUTONOMY_KEY") {
            match KeyBinding::parse(&spec) {
                Some(binding) => cfg.autonomy_cycle = binding,
                None => log::warn!(
                    "ignoring invalid ATERM_AUTONOMY_KEY={spec:?}; keeping the default (Cmd-Shift-A)"
                ),
            }
        }
        if let Ok(spec) = std::env::var("ATERM_SIDEBAR_KEY") {
            match KeyBinding::parse(&spec) {
                Some(binding) => cfg.toggle_sidebar = binding,
                None => log::warn!(
                    "ignoring invalid ATERM_SIDEBAR_KEY={spec:?}; keeping the default (Cmd-B)"
                ),
            }
        }
        if let Ok(spec) = std::env::var("ATERM_HELP_KEY") {
            match KeyBinding::parse(&spec) {
                Some(binding) => cfg.toggle_help = binding,
                None => log::warn!(
                    "ignoring invalid ATERM_HELP_KEY={spec:?}; keeping the default (Cmd-?)"
                ),
            }
        }
        cfg
    }
}
