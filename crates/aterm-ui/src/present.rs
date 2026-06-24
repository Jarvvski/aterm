//! Frame pacing: the keep-warm present scheduler and the vsync-clock seam.
//!
//! This module owns the *logic* the 60fps floor depends on, kept deliberately
//! separate from the GPU and the OS so it can be reasoned about and unit-tested
//! with a deterministic injected clock (no window, no display, no `unsafe`).
//!
//! ## Why "keep-warm"
//!
//! On a ProMotion panel the refresh rate is not fixed: "if you consistently
//! present a drawable on every frame, the display continues at a constant refresh
//! rate, but as soon as you neglect to draw a frame its refresh rate drops" (see
//! [`09-performance-60fps.md`] §2.2, citing Zed's 120fps work). So to *hold* 120Hz
//! through an interaction we must present on **every** vsync while the user is
//! active - even vsyncs where nothing changed - and only stop once activity has
//! been quiet for a beat. The mitigation Zed shipped, and the one we copy: after
//! any input or PTY activity, present every vsync for ~1s ("keep-warm"), then go
//! fully idle (zero frames drawn, the thread sleeps) until the next activity.
//!
//! [`PresentScheduler`] is that state machine. Given a stream of *activity*
//! signals (a keystroke, a resize, or a freshly published grid snapshot - detected
//! via [`crate`]'s [`aterm_core::Snapshot::version`]) and the current time, it
//! answers one question per vsync: present this frame, or idle? It does **not**
//! itself draw, sleep, or talk to the display link - that wiring lives in
//! [`crate::app`] (the winit-driven default) and, on macOS, the self-bridged
//! `CADisplayLink` clock.

use std::time::{Duration, Instant};

/// Default keep-warm window. Present every vsync for ~1s after the last activity,
/// then idle to zero frames. Matches Zed's ProMotion down-clock mitigation
/// ([`09-performance-60fps.md`] §2.2, Recommendation 4).
pub const DEFAULT_KEEP_WARM: Duration = Duration::from_secs(1);

/// The scheduler's verdict for a single vsync opportunity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameDecision {
    /// Within the keep-warm window: present this vsync (hold the refresh rate).
    Present,
    /// The keep-warm window has elapsed with no activity: draw nothing and let the
    /// loop go idle (zero frames) until the next activity re-arms it.
    Idle,
}

impl FrameDecision {
    /// Convenience: did this verdict ask for a present?
    #[must_use]
    pub fn is_present(self) -> bool {
        matches!(self, FrameDecision::Present)
    }
}

/// The keep-warm present scheduler.
///
/// Pure and clock-injected: every method that cares about time takes `now:
/// Instant` from the caller, so the whole state machine is deterministic under
/// test. The render loop calls [`Self::note_activity`] / [`Self::observe_version`]
/// as signals arrive and [`Self::decide`] once per vsync.
#[derive(Debug, Clone)]
pub struct PresentScheduler {
    /// How long after the last activity we keep presenting every vsync.
    keep_warm: Duration,
    /// Instant of the most recent activity, or `None` if we have been idle since
    /// construction (the cold state - decide → Idle).
    last_activity: Option<Instant>,
    /// The last grid-snapshot version we treated as activity, so a *new* published
    /// frame re-arms keep-warm but re-reading the same frame does not.
    last_version: u64,
}

impl Default for PresentScheduler {
    fn default() -> Self {
        Self::new(DEFAULT_KEEP_WARM)
    }
}

impl PresentScheduler {
    /// Build a scheduler with an explicit keep-warm window. Starts **cold**: with
    /// no activity recorded, [`Self::decide`] returns [`FrameDecision::Idle`] until
    /// the first [`Self::note_activity`]/[`Self::observe_version`]. The app arms it
    /// on window-resume and on the first published snapshot.
    #[must_use]
    pub fn new(keep_warm: Duration) -> Self {
        Self {
            keep_warm,
            last_activity: None,
            last_version: 0,
        }
    }

    /// Record an activity (a keystroke, paste, resize, focus, or any explicit
    /// "something happened"): (re)arm the keep-warm window from `now`.
    pub fn note_activity(&mut self, now: Instant) {
        self.last_activity = Some(now);
    }

    /// Treat a freshly observed grid-snapshot `version` as activity. Returns `true`
    /// if `version` differs from the last one we saw (a new published frame), in
    /// which case keep-warm is re-armed; `false` if it is the same frame (no-op).
    ///
    /// The version is monotonic ([`aterm_core::Snapshot::version`]); the seeded
    /// pre-publish snapshot is version 0, so observing 0 before any publish is
    /// correctly treated as "no new frame".
    pub fn observe_version(&mut self, version: u64, now: Instant) -> bool {
        if version != self.last_version {
            self.last_version = version;
            self.note_activity(now);
            true
        } else {
            false
        }
    }

    /// The instant at which the keep-warm window expires, or `None` if cold. The
    /// render loop can `ControlFlow::WaitUntil` this to schedule the single idle
    /// transition after a burst of activity ends.
    #[must_use]
    pub fn warm_until(&self) -> Option<Instant> {
        self.last_activity.map(|t| t + self.keep_warm)
    }

    /// Whether we are still within the keep-warm window at `now`. The boundary is
    /// half-open: at exactly `last_activity + keep_warm` we are **no longer** warm
    /// (so a window armed at T idles at T+keep_warm, not T+keep_warm+ε).
    #[must_use]
    pub fn is_warm(&self, now: Instant) -> bool {
        match self.warm_until() {
            Some(until) => now < until,
            None => false,
        }
    }

    /// The verdict for this vsync opportunity: [`FrameDecision::Present`] while
    /// warm, [`FrameDecision::Idle`] once the window has elapsed.
    #[must_use]
    pub fn decide(&self, now: Instant) -> FrameDecision {
        if self.is_warm(now) {
            FrameDecision::Present
        } else {
            FrameDecision::Idle
        }
    }

    /// The configured keep-warm window (for the loop's pacing math / tests).
    #[must_use]
    pub fn keep_warm(&self) -> Duration {
        self.keep_warm
    }
}

// ---------------------------------------------------------------------------
// The vsync-clock seam: a self-bridged CADisplayLink (macOS) with a no-op
// fallback elsewhere.
// ---------------------------------------------------------------------------
//
// The locked render decision calls for "a vsync render loop on a self-bridged
// CADisplayLink". `DisplayLink` is that bridge: it asks the window's `NSView` for
// a macOS-14+ `CADisplayLink`, attaches it to the current (main) run loop, and
// invokes a Rust callback once per vsync - the app turns each tick into a
// `request_redraw`, and the [`PresentScheduler`] decides whether that vsync
// actually draws. It is the swappable clock *source*; the default winit-driven
// present loop in [`crate::app`] is the portable fallback (and what runs when the
// link cannot be created, e.g. headless or non-macOS).
//
// IMPORTANT (verification status): this `unsafe` Objective-C interop is
// COMPILE-VERIFIED only. It cannot be exercised headlessly (no display, no run
// loop firing), so the actual vsync cadence, the ProMotion 120Hz hold, and the
// retain/teardown behavior must be confirmed in a manual run on real hardware
// (ticket T-1.5 AC3 + T-7.2). The bridge is therefore OPT-IN
// ([`crate::app::RenderConfig::display_link`], default off): the proven winit
// loop drives presentation until the link is validated.

#[cfg(target_os = "macos")]
mod display_link {
    //! The macOS `CADisplayLink` bridge, pinned to winit 0.30's objc2 0.5
    //! generation (objc2 0.5.2 / objc2-foundation 0.2.2 / objc2-quartz-core
    //! 0.2.2) so the `NSView` we pull from the winit window shares winit's
    //! runtime types. Uses the 0.5-era `declare_class!` (NOT 0.6 `define_class!`).

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
    use objc2::{declare_class, msg_send, msg_send_id, mutability, sel, ClassType, DeclaredClass};
    use objc2_foundation::{NSRunLoop, NSRunLoopCommonModes};
    use objc2_quartz_core::CADisplayLink;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    /// Ivars for the display-link target: the per-vsync Rust callback. The target
    /// is created, used, and dropped entirely on the main thread, so the boxed
    /// `Fn` needs no `Send` bound. `dealloc` (auto-generated by `declare_class!`)
    /// drops these ivars, releasing the callback.
    struct VsyncIvars {
        on_vsync: Box<dyn Fn()>,
    }

    declare_class!(
        /// A tiny `NSObject` subclass used purely as the `CADisplayLink` target.
        struct VsyncTarget;

        unsafe impl ClassType for VsyncTarget {
            type Super = NSObject;
            type Mutability = mutability::InteriorMutable;
            const NAME: &'static str = "ATerm_VsyncTarget";
        }

        impl DeclaredClass for VsyncTarget {
            type Ivars = VsyncIvars;
        }

        unsafe impl VsyncTarget {
            /// The selector `step:` the display link invokes once per vsync. Fires
            /// the Rust callback; must never block the run-loop thread.
            #[method(step:)]
            fn step(&self, _link: &CADisplayLink) {
                // A panic must NEVER unwind across the Objective-C frame (abort or
                // UB). The installed callback (request_redraw) does not panic, but
                // shield it so a future richer callback cannot take the process
                // down from inside the run-loop callback.
                let cb = &self.ivars().on_vsync;
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(cb)).is_err() {
                    log::error!("vsync callback panicked; suppressed at the ObjC boundary");
                }
            }
        }

        unsafe impl NSObjectProtocol for VsyncTarget {}
    );

    impl VsyncTarget {
        fn new(on_vsync: Box<dyn Fn()>) -> Retained<Self> {
            let this = Self::alloc().set_ivars(VsyncIvars { on_vsync });
            // SAFETY: standard objc2 0.5 init pattern - finish constructing the
            // allocated, ivar-initialized instance by calling NSObject's `init`.
            unsafe { msg_send_id![super(this), init] }
        }
    }

    /// A live `CADisplayLink` driving a per-vsync callback. Dropping it calls
    /// `invalidate()` first, breaking the link→target reference so no callback
    /// fires after teardown.
    pub struct DisplayLink {
        link: Retained<CADisplayLink>,
        // Kept alive for as long as the link references it (the link does not own
        // its target). Underscore: held for lifetime, not read directly.
        _target: Retained<VsyncTarget>,
    }

    impl DisplayLink {
        /// Create a display link for `window`'s screen and attach it to the
        /// current run loop in common modes (so it keeps firing during live
        /// resize / scroll tracking). Returns `None` if the platform handle is not
        /// AppKit or the OS declines to create the link - the caller then falls
        /// back to the winit-driven present loop.
        ///
        /// MUST be called on the main thread (it touches the `NSView` and the main
        /// run loop). winit only hands out window handles on the main thread.
        pub fn new(window: &Window, on_vsync: impl Fn() + 'static) -> Option<Self> {
            let ns_view = match window.window_handle().ok()?.as_raw() {
                RawWindowHandle::AppKit(h) => h.ns_view,
                _ => return None,
            };
            let target = VsyncTarget::new(Box::new(on_vsync));

            // SAFETY: `ns_view` points at a live `NSView` (valid while `window`
            // lives; we are on the main thread). `displayLinkWithTarget:selector:`
            // is the macOS 14+ `NSView` entry point; objc2-app-kit 0.2.2 does not
            // wrap it, so we send it raw against the view object. On an OS that
            // predates it the selector is UNRECOGNIZED and a raw send would THROW
            // (an ObjC exception unwinding across this frame), NOT return nil - so
            // we gate on `respondsToSelector:` first and fall back to the winit
            // loop if absent. The returned link is autoreleased; `msg_send_id!`
            // retains it (the `Option` binding also tolerates a nil return).
            let view: &AnyObject = unsafe { ns_view.cast::<AnyObject>().as_ref() };
            let responds: bool = unsafe {
                msg_send![view, respondsToSelector: sel!(displayLinkWithTarget:selector:)]
            };
            if !responds {
                log::info!(
                    "NSView lacks displayLinkWithTarget:selector: (pre-macOS-14); falling back"
                );
                return None;
            }
            let link: Option<Retained<CADisplayLink>> = unsafe {
                msg_send_id![view, displayLinkWithTarget: &*target, selector: sel!(step:)]
            };
            let link = link?;

            // SAFETY: attach to this thread's run loop. `NSRunLoopCommonModes` is
            // a framework static; `addToRunLoop:forMode:` retains the link.
            unsafe {
                let run_loop = NSRunLoop::currentRunLoop();
                link.addToRunLoop_forMode(&run_loop, NSRunLoopCommonModes);
            }
            Some(Self {
                link,
                _target: target,
            })
        }

        /// Pause/resume vsync callbacks. The app pauses the link when the
        /// scheduler goes idle (zero wakeups) and resumes it on activity.
        pub fn set_paused(&self, paused: bool) {
            // SAFETY: simple property set on a live CADisplayLink, main thread.
            unsafe { self.link.setPaused(paused) };
        }
    }

    impl Drop for DisplayLink {
        fn drop(&mut self) {
            // SAFETY: invalidate BEFORE the target drops, so the link cannot fire
            // into a freed callback. Idempotent and safe on a live link.
            unsafe { self.link.invalidate() };
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod display_link {
    //! Non-macOS fallback: there is no `CADisplayLink`, so construction always
    //! returns `None` and the app uses its winit-driven present loop.
    use winit::window::Window;

    pub struct DisplayLink;

    impl DisplayLink {
        pub fn new(_window: &Window, _on_vsync: impl Fn() + 'static) -> Option<Self> {
            None
        }
        pub fn set_paused(&self, _paused: bool) {}
    }
}

pub use display_link::DisplayLink;

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed base instant; all test times are deterministic offsets from it.
    fn base() -> Instant {
        Instant::now()
    }

    const KW: Duration = Duration::from_secs(1);

    #[test]
    fn cold_by_default_decides_idle() {
        let s = PresentScheduler::new(KW);
        let t = base();
        assert_eq!(s.decide(t), FrameDecision::Idle);
        assert!(!s.is_warm(t));
        assert_eq!(s.warm_until(), None);
    }

    #[test]
    fn activity_arms_present_within_window() {
        let mut s = PresentScheduler::new(KW);
        let t0 = base();
        s.note_activity(t0);
        assert_eq!(s.decide(t0), FrameDecision::Present);
        // Just before the edge: still warm.
        assert_eq!(
            s.decide(t0 + KW - Duration::from_millis(1)),
            FrameDecision::Present
        );
    }

    #[test]
    fn window_edge_is_half_open() {
        let mut s = PresentScheduler::new(KW);
        let t0 = base();
        s.note_activity(t0);
        // Exactly at the edge → no longer warm (idle).
        assert_eq!(s.decide(t0 + KW), FrameDecision::Idle);
        assert!(!s.is_warm(t0 + KW));
        // One tick past → idle.
        assert_eq!(
            s.decide(t0 + KW + Duration::from_millis(1)),
            FrameDecision::Idle
        );
    }

    #[test]
    fn idle_after_window_then_reactivity_rewarms() {
        let mut s = PresentScheduler::new(KW);
        let t0 = base();
        s.note_activity(t0);
        let cold = t0 + KW + Duration::from_secs(5);
        assert_eq!(s.decide(cold), FrameDecision::Idle);
        // New activity well after the window re-arms relative to the *new* time.
        s.note_activity(cold);
        assert_eq!(s.decide(cold), FrameDecision::Present);
        assert_eq!(
            s.decide(cold + KW - Duration::from_millis(1)),
            FrameDecision::Present
        );
        assert_eq!(s.decide(cold + KW), FrameDecision::Idle);
    }

    #[test]
    fn observe_new_version_arms_same_version_does_not() {
        let mut s = PresentScheduler::new(KW);
        let t0 = base();
        // Seeded snapshot version 0 before any publish: not new, stays cold.
        assert!(!s.observe_version(0, t0));
        assert_eq!(s.decide(t0), FrameDecision::Idle);
        // First real publish (version 1): new → arms.
        assert!(s.observe_version(1, t0));
        assert_eq!(s.decide(t0), FrameDecision::Present);
        // Re-reading the same version later is a no-op (does NOT extend the window).
        let later = t0 + KW - Duration::from_millis(1);
        assert!(!s.observe_version(1, later));
        // The window still expires relative to the original arm at t0.
        assert_eq!(s.decide(t0 + KW), FrameDecision::Idle);
    }

    #[test]
    fn observe_jumped_version_arms() {
        // Coalescing can advance the version by many between observations; any
        // change is "new output" and must re-arm.
        let mut s = PresentScheduler::new(KW);
        let t0 = base();
        assert!(s.observe_version(1, t0));
        let t1 = t0 + Duration::from_millis(500);
        assert!(s.observe_version(42, t1));
        // Window now runs from t1.
        assert_eq!(
            s.decide(t1 + KW - Duration::from_millis(1)),
            FrameDecision::Present
        );
        assert_eq!(s.decide(t1 + KW), FrameDecision::Idle);
    }

    #[test]
    fn warm_until_tracks_last_activity() {
        let mut s = PresentScheduler::new(KW);
        let t0 = base();
        s.note_activity(t0);
        assert_eq!(s.warm_until(), Some(t0 + KW));
        let t1 = t0 + Duration::from_millis(250);
        s.note_activity(t1);
        assert_eq!(s.warm_until(), Some(t1 + KW));
    }

    #[test]
    fn input_extends_window_during_streaming() {
        // Simulates typing during a stream: each keystroke pushes the idle point
        // out, so the panel stays warm across the whole interaction.
        let mut s = PresentScheduler::new(KW);
        let mut t = base();
        s.note_activity(t);
        for _ in 0..10 {
            t += Duration::from_millis(200); // 200ms < 1s window: always warm
            assert_eq!(s.decide(t), FrameDecision::Present);
            s.note_activity(t);
        }
        // Stop typing: 1s later we go idle.
        assert_eq!(s.decide(t + KW), FrameDecision::Idle);
    }

    #[test]
    fn default_uses_one_second_keep_warm() {
        let s = PresentScheduler::default();
        assert_eq!(s.keep_warm(), DEFAULT_KEEP_WARM);
        assert_eq!(s.keep_warm(), Duration::from_secs(1));
    }
}
