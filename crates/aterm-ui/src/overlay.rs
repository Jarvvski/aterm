//! The async, debounced highlight + ghost-text overlay worker (ticket T-3.5).
//!
//! aterm computes syntax highlight, error underlines, and fish-style ghost-text
//! suggestions for the in-progress input line OFF the render thread, so a burst of
//! keystrokes never stalls the 60fps loop. The pure computation lives in
//! [`aterm_core::highlight`] ([`highlight_for`] / [`ghost_for`]); this module is the
//! aterm-ui half: a dedicated worker thread that debounces requests and posts the
//! last-good result back over a channel. The host ([`aterm-app`]'s `Session`) drains
//! the results each wake and applies them to its [`aterm_core::InputModel`]
//! (`set_highlight` / `set_ghost`); the render path only ever READS that last-good
//! overlay, so it can never block on the highlighter (AC3).
//!
//! ## Debounce + short-circuit (AC1)
//!
//! A request carries an `immediate` flag. Ordinary typing sends debounced requests:
//! the worker coalesces a burst and only computes once the input has been quiet for
//! [`DEFAULT_DEBOUNCE`] (~90ms, inside the research's 80-150ms band), so the underline
//! appears after the pause, not on every keystroke. Space / paste / a mode toggle send
//! an `immediate` request that short-circuits the debounce for instant feedback.
//!
//! ## Staleness
//!
//! The worker computes for the text as of the request; by the time a result lands the
//! buffer may have advanced (that is the whole point of debouncing). The model absorbs
//! this: [`aterm_core::InputModel::ghost_tail`] re-derives the visible tail against the
//! live text (a diverged suggestion simply stops being a prefix and hides), and the
//! highlight is recomputed on the next request, so a briefly-lagging overlay
//! self-corrects. The `generation` counter rides along so a consumer can identify the
//! freshest result; [`OverlayWorker::poll`] already returns only the latest drained.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use aterm_core::{
    ghost_for, highlight_for, GhostText, Highlight, HistoryRing, HistoryScope, InputMode,
};

/// Debounce window for ordinary (non-`immediate`) recomputes. ~90ms sits inside the
/// research's 80-150ms band (`05-unified-input-ux.md` §1); tune against the frame
/// budget if needed. Space/paste/toggle bypass it via [`OverlayRequest::immediate`].
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(90);

/// A request to recompute the overlay for the current input line. The host sends one
/// after each edit; the worker debounces bursts (unless `immediate`) and computes the
/// last one.
#[derive(Debug, Clone)]
pub struct OverlayRequest {
    /// Monotonic id the host bumps per request, echoed back in [`OverlayResult`] so the
    /// freshest result is identifiable.
    pub generation: u64,
    /// The full input text to highlight / suggest against.
    pub text: String,
    /// The current input mode (Shell highlights + suggests; Agent is prose - no spans,
    /// no ghost).
    pub mode: InputMode,
    /// The history lens for ghost suggestions (per-mode, or widened to all).
    pub scope: HistoryScope,
    /// A cheap snapshot of the history ring the ghost is drawn from. `Arc` so a send is
    /// a refcount bump, not a deep copy of the ring.
    pub history: Arc<HistoryRing>,
    /// Short-circuit the debounce for instant feedback (space / paste / mode toggle /
    /// IME commit). Ordinary typing leaves this `false` so the burst is coalesced.
    pub immediate: bool,
}

/// The computed overlay for one request: the style spans and the ghost suggestion.
/// Applied by the host to its [`aterm_core::InputModel`].
#[derive(Debug, Clone)]
pub struct OverlayResult {
    /// The [`OverlayRequest::generation`] this result was computed for.
    pub generation: u64,
    /// The syntax-highlight / error-underline spans (empty in Agent mode).
    pub highlight: Highlight,
    /// The fish-style ghost suggestion, if any (always `None` in Agent mode).
    pub ghost: Option<GhostText>,
}

/// The overlay worker: owns a background thread that debounces [`OverlayRequest`]s and
/// posts [`OverlayResult`]s back. Construct once ([`Self::new`]); [`Self::request`]
/// after each edit (non-blocking); [`Self::poll`] each wake to drain the latest result.
/// Dropping it disconnects the channel (the worker exits) and joins the thread.
pub struct OverlayWorker {
    /// `Some` until drop; taken in [`Drop`] to disconnect the worker before joining.
    tx: Option<Sender<OverlayRequest>>,
    rx: Receiver<OverlayResult>,
    handle: Option<JoinHandle<()>>,
}

impl OverlayWorker {
    /// Spawn the worker with the given debounce window (use [`DEFAULT_DEBOUNCE`] outside
    /// tests). The thread lives until this handle drops.
    #[must_use]
    pub fn new(debounce: Duration) -> Self {
        let (req_tx, req_rx) = channel::<OverlayRequest>();
        let (res_tx, res_rx) = channel::<OverlayResult>();
        let handle = thread::Builder::new()
            .name("aterm-overlay".to_string())
            .spawn(move || run(&req_rx, &res_tx, debounce))
            .expect("spawn overlay worker thread");
        Self {
            tx: Some(req_tx),
            rx: res_rx,
            handle: Some(handle),
        }
    }

    /// Queue a recompute (non-blocking). A dead worker (should not happen before drop)
    /// silently drops the request. This is the only call on the keystroke path, and it
    /// is a single channel send - the highlighter never runs here.
    pub fn request(&self, req: OverlayRequest) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(req);
        }
    }

    /// Drain any posted results and return the freshest (or `None`). Called each wake by
    /// the host; non-blocking. Returning only the latest means a backlog collapses to
    /// the newest overlay in one apply.
    pub fn poll(&self) -> Option<OverlayResult> {
        let mut latest = None;
        while let Ok(res) = self.rx.try_recv() {
            latest = Some(res);
        }
        latest
    }
}

impl Drop for OverlayWorker {
    fn drop(&mut self) {
        // Disconnect FIRST (drop the sender) so the worker's blocking recv returns
        // `Disconnected` and the thread exits; only then join. Dropping the sender
        // before the join is what prevents a deadlock (join-then-drop would wait on a
        // thread still blocked on a live channel).
        drop(self.tx.take());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// The worker loop: block for a request, debounce a burst (the newest wins) unless it
/// is `immediate`, compute the overlay off-thread, and post it. Exits when either
/// channel disconnects (the [`OverlayWorker`] dropped).
fn run(rx: &Receiver<OverlayRequest>, tx: &Sender<OverlayResult>, debounce: Duration) {
    while let Ok(mut req) = rx.recv() {
        // Coalesce: keep taking newer requests until the input is quiet for `debounce`.
        // An `immediate` request (including one that arrives mid-wait) skips the wait.
        while !req.immediate {
            match rx.recv_timeout(debounce) {
                Ok(newer) => req = newer,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        let highlight = highlight_for(&req.text, req.mode);
        let ghost = ghost_for(&req.text, req.mode, &req.history, req.scope);
        if tx
            .send(OverlayResult {
                generation: req.generation,
                highlight,
                ghost,
            })
            .is_err()
        {
            return; // consumer gone
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_core::SpanKind;
    use std::time::{Instant, SystemTime};

    fn history_with(entries: &[&str]) -> Arc<HistoryRing> {
        let mut h = HistoryRing::new();
        for (i, e) in entries.iter().enumerate() {
            h.push(
                *e,
                InputMode::Shell,
                SystemTime::UNIX_EPOCH + Duration::from_secs(i as u64),
            );
        }
        Arc::new(h)
    }

    fn req(
        generation: u64,
        text: &str,
        mode: InputMode,
        history: &Arc<HistoryRing>,
        immediate: bool,
    ) -> OverlayRequest {
        OverlayRequest {
            generation,
            text: text.to_string(),
            mode,
            scope: HistoryScope::Mode(mode),
            history: Arc::clone(history),
            immediate,
        }
    }

    /// Poll until a result arrives or `budget` elapses (bounded; never a busy hang).
    fn wait_for_result(worker: &OverlayWorker, budget: Duration) -> Option<OverlayResult> {
        let start = Instant::now();
        loop {
            if let Some(res) = worker.poll() {
                return Some(res);
            }
            if start.elapsed() > budget {
                return None;
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn computes_highlight_and_ghost_off_thread() {
        let history = history_with(&["git status", "git status -s"]);
        let worker = OverlayWorker::new(Duration::from_millis(10));
        worker.request(req(1, "git st", InputMode::Shell, &history, true));
        let res = wait_for_result(&worker, Duration::from_secs(2)).expect("a result arrives");
        assert_eq!(res.generation, 1);
        assert_eq!(
            res.highlight.spans.first().map(|s| s.kind),
            Some(SpanKind::Command),
            "the first token highlights as a command"
        );
        assert_eq!(
            res.ghost.map(|g| g.suggestion),
            Some("git status -s".to_string()),
            "the newest prefix match is suggested (full line)"
        );
    }

    #[test]
    fn agent_mode_has_no_highlight_or_ghost() {
        let history = history_with(&["ask the model"]);
        let worker = OverlayWorker::new(Duration::from_millis(10));
        worker.request(req(1, "ask", InputMode::Agent, &history, true));
        let res = wait_for_result(&worker, Duration::from_secs(2)).expect("a result arrives");
        assert!(res.highlight.spans.is_empty(), "agent prose gets no spans");
        assert!(res.ghost.is_none(), "agent ghost is off by default");
    }

    #[test]
    fn a_burst_coalesces_to_the_latest_request() {
        // AC1 (debounce): a rapid burst of non-immediate requests collapses; the applied
        // result is the LATEST text, never a stale earlier one.
        let history = history_with(&[]);
        let worker = OverlayWorker::new(Duration::from_millis(30));
        worker.request(req(1, "a", InputMode::Shell, &history, false));
        worker.request(req(2, "ab", InputMode::Shell, &history, false));
        worker.request(req(3, "abc", InputMode::Shell, &history, false));
        let res = wait_for_result(&worker, Duration::from_secs(2)).expect("a result arrives");
        // Drain any trailing results; the freshest must be generation 3 ("abc").
        let mut last = res;
        while let Some(next) = worker.poll() {
            last = next;
        }
        assert_eq!(
            last.generation, 3,
            "the burst coalesced to the newest request"
        );
        // "abc" is one command token.
        assert_eq!(
            last.highlight.spans.len(),
            1,
            "highlight is for the latest text"
        );
    }

    #[test]
    fn immediate_request_short_circuits_a_long_debounce() {
        // With a very long debounce, only the `immediate` short-circuit can deliver a
        // result quickly - proving space/paste/toggle bypass the wait (AC1).
        let history = history_with(&[]);
        let worker = OverlayWorker::new(Duration::from_secs(30));
        worker.request(req(1, "ls -la", InputMode::Shell, &history, true));
        let res = wait_for_result(&worker, Duration::from_secs(2))
            .expect("an immediate request does not wait for the debounce");
        assert_eq!(res.generation, 1);
    }

    #[test]
    fn drop_joins_the_worker_without_hanging() {
        // The worker thread must exit on channel disconnect so drop returns promptly
        // (bounded), never wedging the caller.
        let history = history_with(&["cargo build"]);
        let start = Instant::now();
        {
            let worker = OverlayWorker::new(Duration::from_secs(30));
            worker.request(req(1, "car", InputMode::Shell, &history, false));
            // Drop here (debounce is 30s, so the thread is parked in recv_timeout).
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "drop disconnects the channel and joins promptly, not after the debounce"
        );
    }

    #[test]
    fn rapid_request_burst_never_blocks_the_caller() {
        // AC3: the render/main thread must never block on the highlighter. `request()` is
        // a single channel send onto an unbounded queue and computes nothing, so a large
        // burst of requests returns near-instantly regardless of the debounce window. If a
        // future change made `request()` block (e.g. a bounded/sync channel), this fails.
        let history = history_with(&["cargo build", "cargo test"]);
        let worker = OverlayWorker::new(Duration::from_millis(90));
        let start = Instant::now();
        for i in 0..10_000u64 {
            worker.request(req(i, "car", InputMode::Shell, &history, false));
        }
        let elapsed = start.elapsed();
        // If any call blocked on the 90ms debounce/compute, 10k of them would take many
        // minutes; a generous 1s bound proves each send is non-blocking with huge margin.
        assert!(
            elapsed < Duration::from_secs(1),
            "10k request() calls must not block the caller (took {elapsed:?})"
        );
        // And the worker still delivers a final result (the pipeline is live, not wedged).
        assert!(
            wait_for_result(&worker, Duration::from_secs(2)).is_some(),
            "the worker still produces a result after the burst"
        );
    }

    #[test]
    fn an_immediate_request_mid_debounce_short_circuits_the_wait() {
        // The interesting branch of the coalescing loop: an `immediate` request that
        // arrives WHILE the worker is already parked in recv_timeout debouncing a prior
        // non-immediate request must break the wait at once (the type-'ls'-then-space UX).
        let history = history_with(&[]);
        let worker = OverlayWorker::new(Duration::from_secs(30));
        // First a non-immediate request: the worker enters the 30s debounce wait.
        worker.request(req(1, "l", InputMode::Shell, &history, false));
        thread::sleep(Duration::from_millis(20)); // let the worker park in recv_timeout
                                                  // Now an immediate request mid-wait: it must short-circuit the 30s debounce.
        worker.request(req(2, "ls", InputMode::Shell, &history, true));
        let res = wait_for_result(&worker, Duration::from_secs(2))
            .expect("an immediate request mid-debounce short-circuits the wait");
        let mut last = res;
        while let Some(next) = worker.poll() {
            last = next;
        }
        assert_eq!(
            last.generation, 2,
            "the immediate mid-burst request is computed (not stuck behind the 30s debounce)"
        );
    }
}
