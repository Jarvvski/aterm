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
