//! aterm — native macOS GPU terminal binary.
//!
//! Wires `aterm-core` (PTY/VT/blocks), `aterm-ui` (winit window + wgpu/glyphon
//! renderer), and `aterm-agent` (input mode + safety spine). `main` loads config,
//! spawns the login-shell PTY, and runs the UI event loop with a [`Session`] as
//! the callback set so PTY bytes flow to the renderer and keystrokes flow to the
//! shell.

mod config;
mod session;

use config::Config;
use session::Session;

fn main() {
    // Logging: `RUST_LOG=info cargo run -p aterm-app`.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cfg = Config::load();
    log::info!(
        "aterm starting: theme={:?} grid={}x{}",
        cfg.theme,
        cfg.initial_cols,
        cfg.initial_rows
    );

    // Spawn the login shell over a PTY. If this fails we still want to know.
    let session = match Session::spawn(cfg.initial_cols, cfg.initial_rows) {
        Ok(s) => {
            log::info!("login shell PTY spawned ({} blocks)", s.block_count());
            s
        }
        Err(e) => {
            log::error!("failed to spawn shell PTY: {e}");
            std::process::exit(1);
        }
    };

    // Open the window + GPU surface and run the event loop until the window
    // closes. This blocks the main thread (winit requirement).
    let render_config = aterm_ui::RenderConfig {
        display_link: cfg.display_link,
    };
    if let Err(e) = aterm_ui::run_with(cfg.theme, session, render_config) {
        log::error!("event loop error: {e}");
        std::process::exit(1);
    }
}
