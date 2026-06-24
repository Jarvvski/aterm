//! The winit `ApplicationHandler` that opens the window, brings up the GPU
//! renderer, and runs the event loop. This is the UI crate's runnable surface;
//! `aterm-app` reuses it and feeds the terminal snapshot + input through the
//! [`UiCallbacks`] seam.
//!
//! ## Frame pacing (ticket T-1.5)
//!
//! The loop is driven by the [`PresentScheduler`] keep-warm state machine, not a
//! free-running spin. After any activity (a keystroke, a resize, or a newly
//! published grid snapshot) we present every vsync for ~1s; the surface present
//! mode is `Fifo`, so each present blocks until the display's vsync and the redraw
//! cadence is paced to the panel refresh with no busy loop. Once activity has been
//! quiet for the keep-warm window we stop drawing entirely (`decide` → `Idle`) and
//! drop to a coarse idle wake that draws zero frames until the next activity or a
//! freshly published snapshot - the "idle to zero frames" requirement.
//!
//! The *precise ProMotion vsync source* the locked decision calls for - a
//! self-bridged `CADisplayLink` - is layered on top of this in the macOS interop
//! module (ticket T-1.5, second change) behind a seam; this default winit-driven
//! loop is the portable, fully-reasoned baseline it refines.

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use aterm_core::Snapshot;
use aterm_tokens::{Theme, ThemeKind};

use crate::gpu::GpuRenderer;
use crate::present::{DisplayLink, PresentScheduler};
use crate::renderer::{Frame, Renderer};
use crate::window::{grid_dims, window_attributes};

/// Anti-busy-spin wake floor while warm. Shorter than a 120Hz frame (8.3ms) so we
/// never under-shoot a vsync, but long enough that an occluded window (whose
/// present does not block) cannot spin a CPU. The real cadence while warm is set
/// by the `Fifo` present blocking on vsync, not by this timer.
const WARM_WAKE: Duration = Duration::from_millis(4);

/// Idle wake interval. Once the keep-warm window has elapsed we draw nothing, but
/// poll the published snapshot version this coarsely so output produced while idle
/// (a background process printing with no recent input) still appears within a
/// beat. Drawn frames stay at zero while idle; this is a few cheap version reads a
/// second, not a render. A true model→render wake mailbox is a clean follow-up.
const IDLE_WAKE: Duration = Duration::from_millis(100);

/// Hooks the host app implements to drive the UI. All are optional-ish: the UI
/// crate can run standalone with a no-op implementation ([`HeadlessCallbacks`]).
pub trait UiCallbacks {
    /// Called once the window exists; lets the app stash the window handle.
    fn on_ready(&mut self, _window: Arc<Window>) {}

    /// Provide the terminal snapshot to draw this frame (or `None` to just clear).
    ///
    /// Returns an `Arc` so the renderer borrows the published grid without a
    /// per-frame deep clone - the consumer side of the engine's zero-alloc publish
    /// (ticket T-1.5 AC5). The host hands back a cheap `Arc` clone, not a copy of
    /// the cells.
    fn snapshot(&mut self) -> Option<Arc<Snapshot>> {
        None
    }

    /// The version of the latest published snapshot, read *cheaply* (no grid
    /// clone). The pacing loop calls this every wake to detect new output and
    /// (re)arm keep-warm without paying for a full [`Self::snapshot`] when it is
    /// only going to idle. Defaults to 0 (a host with no engine never advances).
    fn snapshot_version(&mut self) -> u64 {
        0
    }

    /// The shell-integration indicator state to show this frame (ticket T-2.6).
    /// Defaults to "no integration" for a host with no engine (e.g.
    /// [`HeadlessCallbacks`]); a real session returns its engine's live status.
    fn integration_status(&mut self) -> aterm_core::Integration {
        aterm_core::Integration::from(aterm_core::IntegrationReason::UnsupportedShell)
    }

    /// A key was pressed; return bytes to forward to the PTY (Shell mode), if any.
    fn on_key(&mut self, _text: Option<&str>, _named: Option<NamedKey>) -> Option<Vec<u8>> {
        None
    }

    /// The window resized to `cols` x `rows` (cells), `width` x `height` (px).
    fn on_resize(&mut self, _cols: u16, _rows: u16, _width: u32, _height: u32) {}
}

/// A no-op callback set so the UI runs standalone (window + clear only).
#[derive(Default)]
pub struct HeadlessCallbacks;
impl UiCallbacks for HeadlessCallbacks {}

/// Render-loop configuration.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderConfig {
    /// Opt into the self-bridged `CADisplayLink` vsync clock (macOS only). Default
    /// `false`: the portable winit-driven present loop drives presentation. The
    /// link path is compile-verified but pending on-hardware validation (T-1.5
    /// AC3 / T-7.2); flip this on to validate it. On non-macOS it is ignored (the
    /// link never constructs and the winit loop is used regardless).
    pub display_link: bool,
}

/// The application: owns the window, renderer, theme, host callbacks, and the
/// keep-warm present scheduler.
pub struct AtermApp<C: UiCallbacks> {
    window: Option<Arc<Window>>,
    renderer: Option<GpuRenderer>,
    theme: Theme,
    callbacks: C,
    title: String,
    scheduler: PresentScheduler,
    config: RenderConfig,
    /// The macOS vsync clock, if installed (opt-in + creation succeeded). `None`
    /// means the winit-driven present loop is driving presentation.
    display_link: Option<DisplayLink>,
}

impl<C: UiCallbacks> AtermApp<C> {
    pub fn new(theme_kind: ThemeKind, callbacks: C) -> Self {
        Self {
            window: None,
            renderer: None,
            theme: *Theme::for_kind(theme_kind),
            callbacks,
            title: "aterm".to_string(),
            scheduler: PresentScheduler::default(),
            config: RenderConfig::default(),
            display_link: None,
        }
    }

    /// Override the render-loop configuration (e.g. to opt into the CADisplayLink
    /// vsync clock). Builder-style so `run`/`run_with` can stay thin.
    #[must_use]
    pub fn with_render_config(mut self, config: RenderConfig) -> Self {
        self.config = config;
        self
    }

    /// Draw exactly one frame: clear to the canvas color and, if the host has a
    /// snapshot, the grid text. Called only when the scheduler says to present.
    fn redraw(&mut self) {
        let snapshot = self.callbacks.snapshot();
        let integration = self.callbacks.integration_status();
        if let Some(renderer) = self.renderer.as_mut() {
            let frame = Frame {
                theme: &self.theme,
                snapshot: snapshot.as_deref(),
                integration,
            };
            if let Err(e) = renderer.render(frame) {
                log::warn!("frame render error: {e}");
            }
        }
    }

    /// Set the control flow for the next wait based on the scheduler state and the
    /// active clock source. Called from `about_to_wait`; never renders.
    ///
    /// - **CADisplayLink installed:** while warm, the link wakes us every vsync, so
    ///   the loop just `Wait`s between ticks; the link is paused when idle so we
    ///   truly drop to zero wakeups, with a coarse poll to still notice output that
    ///   arrives while idle.
    /// - **winit fallback:** a tight guard while warm (the real cadence is the
    ///   `Fifo` present blocking on vsync) and the same coarse idle poll.
    fn schedule_next_wake(&self, event_loop: &ActiveEventLoop, now: Instant) {
        let warm = self.scheduler.is_warm(now);
        match self.display_link.as_ref() {
            Some(link) => {
                // Pause when idle => zero idle wakeups; resume when warm.
                link.set_paused(!warm);
                if warm {
                    // The link drives the cadence; sleep until it (or input) wakes.
                    event_loop.set_control_flow(ControlFlow::Wait);
                } else {
                    // Link paused: still poll coarsely so output-while-idle shows.
                    event_loop.set_control_flow(ControlFlow::WaitUntil(now + IDLE_WAKE));
                }
            }
            None => {
                let interval = if warm { WARM_WAKE } else { IDLE_WAKE };
                event_loop.set_control_flow(ControlFlow::WaitUntil(now + interval));
            }
        }
    }
}

impl<C: UiCallbacks> ApplicationHandler for AtermApp<C> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = match event_loop.create_window(window_attributes(&self.title)) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };
        match GpuRenderer::new(window.clone()) {
            Ok(r) => self.renderer = Some(r),
            Err(e) => {
                log::error!("failed to init GPU renderer: {e}");
                event_loop.exit();
                return;
            }
        }
        self.callbacks.on_ready(window.clone());

        // Opt-in: install the self-bridged CADisplayLink vsync clock. Each tick
        // turns into a redraw request (the scheduler decides whether it draws). If
        // creation fails (non-macOS, headless, OS decline) we silently keep the
        // winit-driven loop. The link calls request_redraw on the main thread.
        if self.config.display_link {
            let win = window.clone();
            match DisplayLink::new(&window, move || win.request_redraw()) {
                Some(link) => {
                    log::info!("CADisplayLink vsync clock installed");
                    self.display_link = Some(link);
                }
                None => {
                    log::info!("CADisplayLink unavailable; using the winit-driven present loop")
                }
            }
        }

        // Window creation is activity: arm keep-warm so the first ~1s presents
        // (the window paints immediately and holds the refresh rate on open).
        self.scheduler.note_activity(Instant::now());
        window.request_redraw();
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(size.width, size.height);
                }
                let scale = self
                    .window
                    .as_ref()
                    .map(|w| w.scale_factor() as f32)
                    .unwrap_or(1.0);
                let (cols, rows) = grid_dims(size.width, size.height, scale);
                self.callbacks
                    .on_resize(cols, rows, size.width, size.height);
                // A resize is activity: re-arm and repaint promptly.
                self.scheduler.note_activity(Instant::now());
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        logical_key,
                        text,
                        ..
                    },
                ..
            } => {
                let named = match &logical_key {
                    Key::Named(n) => Some(*n),
                    _ => None,
                };
                let txt = text.as_ref().map(|t| t.as_str());
                if let Some(bytes) = self.callbacks.on_key(txt, named) {
                    // Forwarding happens inside the callback (it owns the PTY);
                    // the returned bytes let a headless host observe input.
                    let _ = bytes;
                }
                // A keystroke is activity: re-arm keep-warm and repaint.
                self.scheduler.note_activity(Instant::now());
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                // Cheaply notice newly published output and (re)arm keep-warm
                // before deciding - so a frame produced by the model thread keeps
                // the panel warm without a keystroke.
                let version = self.callbacks.snapshot_version();
                self.scheduler.observe_version(version, now);
                // Only pay for the snapshot clone + GPU work when actually warm.
                if self.scheduler.decide(now).is_present() {
                    self.redraw();
                }
            }
            _ => {}
        }
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        // Our scheduled wake elapsed (warm guard or idle poll): ask for a redraw so
        // `RedrawRequested` re-evaluates the scheduler. `Poll` is included for
        // completeness if a host ever forces it; `Wait`/`WaitCancelled` (woken by
        // input or OS) already drive their own redraws from `window_event`.
        if matches!(
            cause,
            StartCause::ResumeTimeReached { .. } | StartCause::Poll
        ) {
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Pick the next wake from the scheduler state. Rendering is NOT driven from
        // here (winit warns against it) - only the control-flow cadence is.
        self.schedule_next_wake(event_loop, Instant::now());
    }
}

/// Convenience entry point with the default render config (winit-driven present
/// loop). Blocks until the window closes.
pub fn run<C: UiCallbacks>(
    theme_kind: ThemeKind,
    callbacks: C,
) -> Result<(), winit::error::EventLoopError> {
    run_with(theme_kind, callbacks, RenderConfig::default())
}

/// Entry point with an explicit [`RenderConfig`] (e.g. to opt into the
/// CADisplayLink vsync clock). Builds the event loop, runs `AtermApp` to
/// completion, and returns once the window closes.
pub fn run_with<C: UiCallbacks>(
    theme_kind: ThemeKind,
    callbacks: C,
    config: RenderConfig,
) -> Result<(), winit::error::EventLoopError> {
    let event_loop = EventLoop::new()?;
    // Start in `Wait`; the scheduler arms a `WaitUntil` cadence from `resumed`
    // onward. (Idle floor: when nothing is happening the loop truly sleeps.)
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = AtermApp::new(theme_kind, callbacks).with_render_config(config);
    event_loop.run_app(&mut app)
}
