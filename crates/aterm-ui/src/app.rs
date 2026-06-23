//! The winit `ApplicationHandler` that opens the window, brings up the GPU
//! renderer, and runs the event loop. This is the UI crate's runnable surface;
//! `aterm-app` reuses it and feeds the terminal snapshot + input through the
//! [`UiCallbacks`] seam.

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use aterm_core::Snapshot;
use aterm_tokens::{Theme, ThemeKind};

use crate::gpu::GpuRenderer;
use crate::renderer::{Frame, Renderer};

/// Hooks the host app implements to drive the UI. All are optional-ish: the UI
/// crate can run standalone with a no-op implementation ([`HeadlessCallbacks`]).
pub trait UiCallbacks {
    /// Called once the window exists; lets the app stash the window handle.
    fn on_ready(&mut self, _window: Arc<Window>) {}

    /// Provide the terminal snapshot to draw this frame (or `None` to just clear).
    fn snapshot(&mut self) -> Option<Snapshot> {
        None
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

/// The application: owns the window, renderer, theme, and host callbacks.
pub struct AtermApp<C: UiCallbacks> {
    window: Option<Arc<Window>>,
    renderer: Option<GpuRenderer>,
    theme: Theme,
    callbacks: C,
    title: String,
}

impl<C: UiCallbacks> AtermApp<C> {
    pub fn new(theme_kind: ThemeKind, callbacks: C) -> Self {
        Self {
            window: None,
            renderer: None,
            theme: *Theme::for_kind(theme_kind),
            callbacks,
            title: "aterm".to_string(),
        }
    }

    /// Approximate the grid cell size for the active grid type style, in physical
    /// pixels. Used to translate a pixel resize into a cols x rows PTY resize.
    fn cell_px(&self, scale: f32) -> (f32, f32) {
        let g = aterm_tokens::type_scale::GRID;
        // iM Writing Mono advance is ~0.6em; line box = size * line_height.
        let w = g.size_pt * 0.6 * scale;
        let h = g.size_pt * g.line_height * scale;
        (w.max(1.0), h.max(1.0))
    }

    fn redraw(&mut self) {
        let snapshot = self.callbacks.snapshot();
        if let Some(renderer) = self.renderer.as_mut() {
            let frame = Frame {
                theme: &self.theme,
                snapshot: snapshot.as_ref(),
            };
            if let Err(e) = renderer.render(frame) {
                log::warn!("frame render error: {e}");
            }
        }
    }
}

impl<C: UiCallbacks> ApplicationHandler for AtermApp<C> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(LogicalSize::new(960.0, 600.0));
        let window = match event_loop.create_window(attrs) {
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
                let (cw, ch) = self.cell_px(scale);
                let cols = ((size.width as f32) / cw).floor().max(1.0) as u16;
                let rows = ((size.height as f32) / ch).floor().max(1.0) as u16;
                self.callbacks
                    .on_resize(cols, rows, size.width, size.height);
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
                if named == Some(NamedKey::Escape) {
                    // Esc closes for the scaffold; the app overrides this to
                    // toggle modes once input routing is wired.
                }
                let txt = text.as_ref().map(|t| t.as_str());
                if let Some(bytes) = self.callbacks.on_key(txt, named) {
                    // Forwarding happens inside the callback (it owns the PTY);
                    // the returned bytes let a headless host observe input.
                    let _ = bytes;
                }
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Continuous redraw keeps the frame cadence; a real build gates this on
        // PTY activity + a DisplayLink to honor the 60fps floor without spinning.
        // TODO(ticket EPIC-1.5): drive redraws off a CVDisplayLink, not a spin.
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
}

/// Convenience entry point: build the event loop, run `AtermApp` to completion.
/// Blocks until the window closes. Returns once the loop exits.
pub fn run<C: UiCallbacks>(
    theme_kind: ThemeKind,
    callbacks: C,
) -> Result<(), winit::error::EventLoopError> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = AtermApp::new(theme_kind, callbacks);
    event_loop.run_app(&mut app)
}
