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
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use aterm_core::{BlockList, Snapshot};
use aterm_tokens::{Theme, ThemeKind};

use crate::gpu::GpuRenderer;
use crate::present::{DisplayLink, PresentScheduler};
use crate::recorder::{FrameRecorder, FrameTiming};
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

/// Keyboard modifier state at the time of a key press, in a renderer-neutral form
/// so the host routes on plain bools rather than winit's `ModifiersState`. `cmd`
/// is the macOS Command key (winit `SUPER`); `alt` is Option/Alt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub cmd: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
}

/// One key press handed to [`UiCallbacks::on_key`]. The UI crate builds it from the
/// winit `KeyEvent` plus the tracked modifier state, so the host routes input
/// without depending on winit beyond the [`NamedKey`] re-export (ticket T-3.3).
///
/// - `named`: `Some` for a named key (Enter, Escape, Tab, arrows, ...).
/// - `ch`: the logical character of a `Character` key (e.g. `/`), present
///   regardless of whether the OS produced insertion `text` - macOS suppresses
///   `text` under Command, so a `Cmd-/` chord is only visible via `ch` + `mods`.
/// - `text`: the OS insertion string (`None` under command modifiers on macOS).
/// - `mods`: the active modifier state.
#[derive(Debug, Clone, Copy)]
pub struct KeyPress<'a> {
    pub named: Option<NamedKey>,
    pub ch: Option<char>,
    pub text: Option<&'a str>,
    pub mods: Mods,
}

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

    /// Provide the published block list for the virtualized timeline this frame
    /// (ticket T-2.7), or `None` for a host with no engine (e.g.
    /// [`HeadlessCallbacks`]). Returns an `Arc` so the renderer borrows it without a
    /// per-frame deep copy - the consumer side of the model thread's block publish,
    /// mirroring [`Self::snapshot`].
    fn blocks(&mut self) -> Option<Arc<BlockList>> {
        None
    }

    /// The shell-integration indicator state to show this frame (ticket T-2.6).
    /// Defaults to "no integration" for a host with no engine (e.g.
    /// [`HeadlessCallbacks`]); a real session returns its engine's live status.
    fn integration_status(&mut self) -> aterm_core::Integration {
        aterm_core::Integration::from(aterm_core::IntegrationReason::UnsupportedShell)
    }

    /// The unified-input state to draw this frame (ticket T-3.6): the host's
    /// `Session`-owned [`aterm_core::InputModel`], or `None` for a host with no input
    /// (e.g. [`HeadlessCallbacks`]), in which case no input box is drawn. Borrowed (not
    /// cloned) so the box reads the live buffer with no per-frame allocation, mirroring
    /// [`Self::snapshot`]/[`Self::blocks`]; the renderer only reads it.
    fn input(&self) -> Option<&aterm_core::InputModel> {
        None
    }

    /// The autonomy posture to show in the always-visible indicator this frame
    /// (ticket T-5.11), or `None` for a host with no agent (e.g.
    /// [`HeadlessCallbacks`]), in which case no autonomy chip is drawn. A real session
    /// maps its `aterm_agent::AutonomyMode` onto the UI-local
    /// [`crate::components::AutonomyMode`]. `Copy`, returned by value.
    fn autonomy_mode(&self) -> Option<crate::components::AutonomyMode> {
        None
    }

    /// A key was pressed; return bytes to forward to the PTY (Shell mode), if any.
    /// `key` carries the named key / logical character / insertion text and the
    /// live modifier state, so the host can route the real `Cmd-/` toggle chord and
    /// `Opt-Enter` (ticket T-3.3).
    fn on_key(&mut self, _key: KeyPress<'_>) -> Option<Vec<u8>> {
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

/// The minimum WCAG contrast every ANSI foreground should clear against the
/// canvas on a light-background ("paper") theme. 3:1 is the WCAG large-text / UI
/// threshold - the right bar for decorative monospace output (a full 4.5:1
/// body-text bar would over-darken the brights and invert the bright>normal
/// ordering). The dark theme is exempt (its dim slots are intentional); see
/// [`effective_theme`].
const LIGHT_ANSI_MIN_CONTRAST: f32 = 3.0;

/// Resolve a [`ThemeKind`] to the concrete [`Theme`] the renderer draws, applying
/// the light-"paper" ANSI legibility remap.
///
/// On a light-background theme the saturated bright ANSI colors (bright cyan /
/// yellow especially) wash out against the paper canvas; per `design-system.md`
/// §3 this is corrected at *render* time (never by editing the token values), so
/// the palette is passed through
/// [`AnsiPalette::with_fg_legibility`](aterm_tokens::AnsiPalette::with_fg_legibility)
/// against the canvas. A dark-background theme is returned unchanged - its
/// near-canvas slots (dim / comment gray) are deliberate and must not be lifted.
#[must_use]
pub fn effective_theme(kind: ThemeKind) -> Theme {
    let mut theme = *Theme::for_kind(kind);
    // Gate on a light background (luminance > 0.5): only there do the brights wash
    // out, and there no slot is a deliberate light-gray-on-light, so lifting every
    // failing entry to the floor is safe.
    if theme.colors.bg_canvas.relative_luminance() > 0.5 {
        theme.ansi = theme
            .ansi
            .with_fg_legibility(theme.colors.bg_canvas, LIGHT_ANSI_MIN_CONTRAST);
    }
    theme
}

/// Map winit's window theme onto our [`ThemeKind`] (the follow-OS-appearance path).
fn theme_kind_from_winit(t: winit::window::Theme) -> ThemeKind {
    match t {
        winit::window::Theme::Light => ThemeKind::Light,
        winit::window::Theme::Dark => ThemeKind::Dark,
    }
}

/// The application: owns the window, renderer, theme, host callbacks, and the
/// keep-warm present scheduler.
pub struct AtermApp<C: UiCallbacks> {
    window: Option<Arc<Window>>,
    renderer: Option<GpuRenderer>,
    /// The rendered theme for this frame: [`effective_theme`] of `theme_kind` (so
    /// the light legibility remap is already baked in).
    theme: Theme,
    /// The active theme variant. `theme` is its rendered form.
    theme_kind: ThemeKind,
    /// Follow the OS appearance: when `true`, `Window::theme()` at launch and later
    /// `WindowEvent::ThemeChanged` events drive the active theme. Opt-in via
    /// [`AtermApp::with_follow_system`]; default `false` so the configured theme
    /// wins (matching the app's "light paper" config default). The follow-system
    /// *default* is owner open-question #1 (follow-OS vs paper), still unconfirmed -
    /// this exposes the capability without changing the shipped default.
    follow_system: bool,
    callbacks: C,
    title: String,
    scheduler: PresentScheduler,
    config: RenderConfig,
    /// The macOS vsync clock, if installed (opt-in + creation succeeded). `None`
    /// means the winit-driven present loop is driving presentation.
    display_link: Option<DisplayLink>,
    /// Tier-2 frame recorder (ticket T-7.1), if installed via
    /// [`Self::with_frame_recorder`]. `None` (the default) makes the per-frame
    /// instrumentation zero-cost: the present path skips it entirely. The scenario
    /// driver (T-7.2) installs one to capture a scripted stress run.
    recorder: Option<FrameRecorder>,
    /// Instant of the previous present, for the recorder's `present_interval`
    /// (vsync-to-vsync delta). `None` until the first present.
    last_present_at: Option<Instant>,
    /// The live keyboard modifier state, tracked from winit `ModifiersChanged` and
    /// folded into each [`KeyPress`] (so the host sees `Cmd-/` / `Opt-Enter`;
    /// ticket T-3.3). winit reports modifier transitions separately from key
    /// presses, so we hold the latest here.
    mods: ModifiersState,
}

impl<C: UiCallbacks> AtermApp<C> {
    pub fn new(theme_kind: ThemeKind, callbacks: C) -> Self {
        Self {
            window: None,
            renderer: None,
            theme: effective_theme(theme_kind),
            theme_kind,
            follow_system: false,
            callbacks,
            title: "aterm".to_string(),
            scheduler: PresentScheduler::default(),
            config: RenderConfig::default(),
            display_link: None,
            recorder: None,
            last_present_at: None,
            mods: ModifiersState::empty(),
        }
    }

    /// Override the render-loop configuration (e.g. to opt into the CADisplayLink
    /// vsync clock). Builder-style so `run`/`run_with` can stay thin.
    #[must_use]
    pub fn with_render_config(mut self, config: RenderConfig) -> Self {
        self.config = config;
        self
    }

    /// Follow the OS appearance: when enabled, the active theme tracks
    /// `Window::theme()` at launch and `WindowEvent::ThemeChanged` thereafter.
    /// Default off (the configured theme wins). The follow-system *default* is
    /// owner open-question #1 and unconfirmed; this builder exposes the capability
    /// without changing the shipped "light paper" default.
    #[must_use]
    pub fn with_follow_system(mut self, follow: bool) -> Self {
        self.follow_system = follow;
        self
    }

    /// The active theme variant (light / dark).
    #[must_use]
    pub fn theme_kind(&self) -> ThemeKind {
        self.theme_kind
    }

    /// Switch the active theme at runtime - an explicit override, so it stops
    /// following the OS appearance. Grid colors re-resolve against the new theme on
    /// the next frame with NO grid reallocation: the published snapshot is
    /// unchanged, and the renderer's rebuild gate (keyed partly on the theme) simply
    /// re-resolves each cell into its existing instance buffers. Repaints promptly.
    pub fn set_theme(&mut self, kind: ThemeKind) {
        self.follow_system = false;
        self.apply_theme_kind(kind);
    }

    /// Toggle light↔dark at runtime (an explicit override; see [`Self::set_theme`]).
    pub fn toggle_theme(&mut self) {
        self.set_theme(self.theme_kind.toggle());
    }

    /// Set the active variant + its rendered ([`effective_theme`]) form, then re-arm
    /// keep-warm and repaint if the window is up. A no-op when the variant is
    /// unchanged. Does NOT touch `follow_system`, so it serves both the explicit
    /// switch and the follow-OS path.
    fn apply_theme_kind(&mut self, kind: ThemeKind) {
        if self.theme_kind == kind {
            return;
        }
        self.theme_kind = kind;
        self.theme = effective_theme(kind);
        // A theme change is activity: re-arm keep-warm and repaint promptly.
        self.scheduler.note_activity(Instant::now());
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Install a Tier-2 [`FrameRecorder`] (ticket T-7.1). Each presented frame is
    /// then timed and recorded; read the captured window back with
    /// [`Self::frame_recorder`]. Without this, the present path does no
    /// instrumentation. The scenario driver (T-7.2) installs a recorder sized for
    /// its run.
    #[must_use]
    pub fn with_frame_recorder(mut self, recorder: FrameRecorder) -> Self {
        self.recorder = Some(recorder);
        self
    }

    /// The installed frame recorder, if any - for the driver/analysis to read
    /// percentiles or dump JSON after a run.
    #[must_use]
    pub fn frame_recorder(&self) -> Option<&FrameRecorder> {
        self.recorder.as_ref()
    }

    /// Draw exactly one frame: clear to the canvas color and, if the host has a
    /// snapshot, the grid text. Called only when the scheduler says to present.
    ///
    /// When a [`FrameRecorder`] is installed (T-7.1) the frame is timed and
    /// recorded; with none installed (the default) this is exactly the bare
    /// build-and-render with no added work.
    fn redraw(&mut self) {
        // Frame-start clock ONLY when a recorder is installed: with none (the
        // default) the whole instrumentation block below is skipped, so this is
        // exactly the bare build-and-render - no clock read, no per-frame work.
        let frame_start = self.recorder.is_some().then(Instant::now);
        let snapshot = self.callbacks.snapshot();
        let blocks = self.callbacks.blocks();
        let integration = self.callbacks.integration_status();
        if let Some(renderer) = self.renderer.as_mut() {
            let frame = Frame {
                theme: &self.theme,
                snapshot: snapshot.as_deref(),
                blocks: blocks.as_deref(),
                integration,
                // Borrows `self.callbacks` immutably; `renderer` borrows the disjoint
                // `self.renderer` field, so the two coexist (no per-frame alloc).
                input: self.callbacks.input(),
                autonomy: self.callbacks.autonomy_mode(),
            };
            if let Err(e) = renderer.render(frame) {
                log::warn!("frame render error: {e}");
            }
        }
        if let (Some(recorder), Some(started)) = (self.recorder.as_mut(), frame_start) {
            // cpu_frame_ms = build + encode + submit on this (render) thread. GPU
            // time is None (the device requests no TIMESTAMP_QUERY feature - see
            // recorder module docs, T-7.1 AC4). present_interval is the delta from
            // the previous present; `last_present_at` is cleared when the scheduler
            // goes idle (see `RedrawRequested`), so the first frame of a warm burst
            // reports 0 (a fresh burst, NOT a dropped frame) rather than the whole
            // idle gap. Dirty extent: the renderer rebuilds the whole visible grid
            // on any change (rebuild-or-skip; partial-damage redraw is a future
            // optimization), so a snapshot-driven draw touches the visible grid; a
            // bare clear (no snapshot) is zero cells.
            let cpu_frame_ms = started.elapsed().as_secs_f32() * 1000.0;
            let present_interval_ms = self
                .last_present_at
                .map(|prev| started.duration_since(prev).as_secs_f32() * 1000.0)
                .unwrap_or(0.0);
            let dirty_cells = snapshot
                .as_deref()
                .map(|s| u32::try_from(s.rows.saturating_mul(s.cols)).unwrap_or(u32::MAX))
                .unwrap_or(0);
            recorder.record(FrameTiming {
                cpu_frame_ms,
                gpu_frame_ms: None,
                present_interval_ms,
                dirty_cells,
                allocations: None,
            });
            self.last_present_at = Some(started);
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

        // Follow the OS appearance at launch if asked (later changes arrive as
        // `WindowEvent::ThemeChanged`). When off, the configured theme stays.
        // `apply_theme_kind`'s repaint is a no-op here (the window field is set
        // below); the end-of-`resumed` `request_redraw` paints the chosen theme.
        if self.follow_system {
            if let Some(t) = window.theme() {
                self.apply_theme_kind(theme_kind_from_winit(t));
            }
        }

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
            WindowEvent::ModifiersChanged(modifiers) => {
                // winit reports modifier transitions separately from key presses;
                // hold the latest so the next `KeyPress` carries Cmd/Opt/Ctrl/Shift
                // (ticket T-3.3). Not activity on its own - a bare modifier press
                // never needs a repaint.
                self.mods = modifiers.state();
            }
            WindowEvent::ThemeChanged(t) => {
                // The OS appearance changed: follow it live (re-resolves grid colors
                // with no realloc, repaints) when in follow-system mode; otherwise
                // the configured/overridden theme stands.
                if self.follow_system {
                    self.apply_theme_kind(theme_kind_from_winit(t));
                }
            }
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
                // The logical character of a Character key (e.g. `/`), needed for
                // the `Cmd-/` chord since macOS suppresses `text` under Command.
                let ch = match &logical_key {
                    Key::Character(s) => s.chars().next(),
                    _ => None,
                };
                let txt = text.as_ref().map(|t| t.as_str());
                let mods = Mods {
                    cmd: self.mods.super_key(),
                    alt: self.mods.alt_key(),
                    ctrl: self.mods.control_key(),
                    shift: self.mods.shift_key(),
                };
                if let Some(bytes) = self.callbacks.on_key(KeyPress {
                    named,
                    ch,
                    text: txt,
                    mods,
                }) {
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
                } else {
                    // Idle: this vsync draws zero frames (the keep-warm window
                    // elapsed). Forget the last present so the FIRST frame of the
                    // next warm burst reports a fresh interval (0) instead of the
                    // whole idle gap - which the recorder would otherwise miscount
                    // as a dropped frame (T-7.1).
                    self.last_present_at = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_tokens::contrast_ratio;

    #[test]
    fn effective_theme_makes_light_paper_brights_legible() {
        // AC (T-4.2): the riskiest combo (bright cyan/yellow on light paper) is
        // legible in the rendered theme - the renderer remap, not a token edit.
        let light = effective_theme(ThemeKind::Light);
        let bg = light.colors.bg_canvas;
        for c in [light.ansi.bright_cyan, light.ansi.bright_yellow] {
            assert!(
                contrast_ratio(c, bg) >= LIGHT_ANSI_MIN_CONTRAST,
                "light bright {c:?} must clear {LIGHT_ANSI_MIN_CONTRAST}:1 on paper, got {:.2}",
                contrast_ratio(c, bg)
            );
        }
        // Every rendered light ANSI fg clears the floor against the canvas.
        for i in 0u8..=15 {
            assert!(contrast_ratio(light.ansi.by_index(i), bg) >= LIGHT_ANSI_MIN_CONTRAST);
        }
    }

    #[test]
    fn effective_theme_leaves_dark_palette_untouched() {
        // The dark theme's dim slots (bright-black/comment gray near the canvas) are
        // intentional; the legibility remap must skip a dark background entirely.
        let dark = effective_theme(ThemeKind::Dark);
        let raw = *Theme::for_kind(ThemeKind::Dark);
        for i in 0u8..=15 {
            assert_eq!(dark.ansi.by_index(i), raw.ansi.by_index(i));
        }
    }

    #[test]
    fn winit_theme_maps_to_kind() {
        assert_eq!(
            theme_kind_from_winit(winit::window::Theme::Light),
            ThemeKind::Light
        );
        assert_eq!(
            theme_kind_from_winit(winit::window::Theme::Dark),
            ThemeKind::Dark
        );
    }

    #[test]
    fn set_and_toggle_theme_switch_kind_and_stop_following_os() {
        // The runtime switch state machine, exercised headlessly (no window/GPU).
        let mut app = AtermApp::new(ThemeKind::Light, HeadlessCallbacks).with_follow_system(true);
        assert_eq!(app.theme_kind(), ThemeKind::Light);

        app.set_theme(ThemeKind::Dark);
        assert_eq!(app.theme_kind(), ThemeKind::Dark);
        assert_eq!(
            app.theme.colors.bg_canvas,
            Theme::for_kind(ThemeKind::Dark).colors.bg_canvas,
            "the rendered theme switched to dark"
        );
        assert!(
            !app.follow_system,
            "an explicit set_theme is an override and stops following the OS"
        );

        app.toggle_theme();
        assert_eq!(app.theme_kind(), ThemeKind::Light);
    }
}
