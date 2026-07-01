//! The Tier-2 stress scenarios + the pass/fail gate (ticket T-7.2).
//!
//! This module is the PURE, headless-testable core of the "60fps always" proof
//! ([`09-performance-60fps.md`] §9): it declares the seven named scenarios
//! ([`ScenarioKind`]) as deterministic input programs ([`Scenario`]) and encodes the
//! gate ([`Gate::evaluate`]) that turns a run's recorded frames ([`FrameStats`] +
//! [`RunFacts`]) into a pass/fail [`Verdict`]. It spawns nothing, opens no window, and
//! reads no clock - so the whole gate is unit-tested on any host (the crate's "pure
//! logic is heavily unit-tested with no window" rule), exactly like the T-7.1 recorder.
//!
//! The *live* replay - spawning the generators, driving the real `aterm-ui` app loop
//! with the [`crate::scenario`] scripts, and feeding the recorder real vsync-paced
//! frames - is the on-hardware `scenario_driver` binary (mirroring how T-7.1's
//! present-loop hook is on-hardware while its recorder module is pure).
//!
//! ## The gate (from [`09-performance-60fps.md`] §9)
//!
//! The BLOCKING gate is the **60fps floor** (owner open-question #4: hard-gate the
//! 60Hz floor, track 120Hz): steady-state per scenario, `present` p50 <= 16.67ms,
//! p99 <= 16.0ms (never exceed), max <= 16.0ms (non-resize; resize gets a declared
//! one-frame spike allowance), and **0** dropped frames (flood/resize get a small
//! declared budget). `idle` asserts ~0 frames drawn after keep-warm; `output_flood`
//! asserts render is decoupled from the byte rate (frames track vsync, not bytes). The
//! 120Hz target is computed too but is **informational only** ([`Target120`]).
//!
//! [`09-performance-60fps.md`]: ../../../docs/research/09-performance-60fps.md
//! [`FrameStats`]: aterm_ui::FrameStats

use aterm_ui::{FrameSample, FrameStats, Refresh};
use serde::{Deserialize, Serialize};

/// The p99 / max frame-time hard floor in milliseconds: the 60Hz deadline a frame must
/// never exceed ([`09-performance-60fps.md`] §9, "60Hz, never exceed"). Deliberately
/// 16.0, a hair under the 16.67ms nominal 60Hz interval, so a frame that slips to the
/// *next* vsync is caught.
pub const FLOOR_P99_MS: f32 = 16.0;

/// A one-frame present spike (ms) a resize animation is allowed for the
/// `presentsWithTransaction` handoff ([`09-performance-60fps.md`] §9: "resize allowed a
/// one-frame transaction spike"). Applied only to the `window_resize` scenario's
/// `frame_max` gate.
pub const RESIZE_SPIKE_MS: f32 = 33.4;

/// Minimum recorded frames for an *active* (non-idle) scenario's percentiles to be
/// meaningful (~0.5s @ 60Hz). Below this the run is [`Verdict::Inconclusive`] rather
/// than a false pass - the guard that keeps a headless/no-display CI run (which records
/// ~no frames) from silently "passing" every gate.
pub const MIN_ACTIVE_SAMPLES: usize = 30;

/// Decoupling frame ceiling for `output_flood`: presented frames may exceed the
/// vsync-implied count by at most this factor. This arm catches a regression that
/// abandons vsync pacing (e.g. drops `Fifo` present) and starts drawing per byte-chunk:
/// it would present far past vsync. On its own it is NOT sufficient (under `Fifo` a
/// present cannot beat vsync anyway), which is why the decoupling gate ALSO requires a
/// sustained byte firehose ([`MIN_FLOOD_BYTES`]) - see [`Gate::evaluate`].
pub const DECOUPLE_SLACK: f32 = 2.0;

/// The minimum bytes the model must have drained during the `output_flood` measured
/// window for the run to count as an actual flood. This is the falsifiable half of the
/// decoupling proof: the byte firehose kept flowing (huge `bytes_fed`) WHILE the
/// renderer presented only ~vsync frames. If a regression coupled the render into the
/// byte-drain path, the vsync-blocked present would throttle the drain and `bytes_fed`
/// would collapse below this floor - so the gate fires. ~4 MiB is far below what `yes`
/// sustains over a multi-second window, yet far above what a render-throttled drain
/// would manage.
pub const MIN_FLOOD_BYTES: u64 = 4 * 1024 * 1024;

/// The seven named Tier-2 stress scenarios ([`09-performance-60fps.md`] §9 table). The
/// [`Self::name`]s are the exact `domain.md` vocabulary used in the ticket, the JSON,
/// and the CI logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioKind {
    /// Page-down through a 100k-line buffer: scroll throughput + damage.
    FastScroll,
    /// `yes` for a bounded window: PTY backpressure + render/byte decoupling.
    OutputFlood,
    /// A 1M-line ring, jump to the top: memory + scroll into deep history.
    LargeScrollback,
    /// Inject streamed agent tokens while replaying keystrokes: concurrent stream +
    /// live edit.
    AgentStreamWhileTyping,
    /// Animate the window 800->1600px over ~1s: reflow + the `presentsWithTransaction`
    /// path.
    WindowResize,
    /// Repeated full-grid alt-screen repaints (vim/htop-style): full invalidation.
    FullscreenTuiRedraw,
    /// Sit idle for 5s: down-clock + ~0 frames drawn after keep-warm.
    Idle,
}

impl ScenarioKind {
    /// The canonical snake_case scenario name (the ticket / JSON / CI identifier).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            ScenarioKind::FastScroll => "fast_scroll",
            ScenarioKind::OutputFlood => "output_flood",
            ScenarioKind::LargeScrollback => "large_scrollback",
            ScenarioKind::AgentStreamWhileTyping => "agent_stream_while_typing",
            ScenarioKind::WindowResize => "window_resize",
            ScenarioKind::FullscreenTuiRedraw => "fullscreen_tui_redraw",
            ScenarioKind::Idle => "idle",
        }
    }
}

/// A deterministic OUTPUT source the driver spawns for a scenario - the "feed
/// deterministic PTY input" side. Generators PRINT their output (so there is no PTY
/// stdin-echo doubling), matching the research's own examples (`yes` for a flood, a
/// numbered-line buffer to scroll through).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Generator {
    /// Emit `lines` numbered plaintext lines then stop (`seq`-style) - the scroll /
    /// scrollback buffers.
    Seq { lines: u64 },
    /// Flood `y\n` for `run_ms` milliseconds (`yes`-style) then stop - the backpressure
    /// / decoupling source.
    Yes { run_ms: u32 },
    /// A full-screen alt-screen repaint loop (a vim/htop-style TUI redraw). Runs until
    /// the PTY closes at scenario teardown (like [`Self::Yes`]), so full-grid
    /// invalidation covers the ENTIRE measured window - a fixed repaint COUNT could
    /// finish early on a fast host and leave the tail presenting a frozen frame.
    AltScreenRepaint,
}

/// A synthetic input-box edit (typing / caret motion / mode toggle), replayed straight
/// into the host's `InputModel` reducer - the "synthetic input events" side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputAction {
    /// Insert this text at the caret.
    Type(String),
    /// Delete the char before the caret.
    Backspace,
    /// Move the caret one column left / right.
    Left,
    Right,
    /// Jump the caret to the line start / end.
    Home,
    End,
    /// Flip Shell <-> Agent (text preserved).
    ToggleMode,
}

/// A synthetic timeline scroll action, replayed through the renderer's scroll-lock
/// (the same path the wheel / PageUp / PageDown bindings drive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrollAction {
    /// Scroll up / down by `rows` display rows (toward older / newer output).
    LinesUp(u32),
    LinesDown(u32),
    /// Page up / down (one viewport).
    PageUp,
    PageDown,
    /// Jump to the oldest / newest content.
    ToTop,
    ToBottom,
}

/// The kind of agent transcript step to open for the `agent_stream_while_typing`
/// scenario. Maps onto `aterm_core::AgentBlockKind` in the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStepKind {
    AssistantText,
    Thinking,
}

/// One timed step of a scenario's synthetic-input script. The driver applies the steps
/// in order, evenly paced across the measured window; `Idle` inserts an explicit pause.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DriverAction {
    /// Apply an input-box edit.
    Input(InputAction),
    /// Scroll the timeline.
    Scroll(ScrollAction),
    /// Resize the window to `width` x `height` logical px (the resize animation).
    Resize { width: u32, height: u32 },
    /// Open a new agent transcript step with initial text.
    AgentOpen { kind: AgentStepKind, text: String },
    /// Stream a text delta into the open agent step.
    AgentToken(String),
    /// Pause for `ms` (pacing / the idle window) - no state change.
    Idle { ms: u32 },
}

/// The declared pass/fail thresholds for a scenario. The scenario carries the
/// **blocking** 60fps-floor gate; the 120Hz target is evaluated separately and
/// informationally ([`Target120`]).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Gate {
    /// The active refresh the deadlines derive from (the blocking gate is always the
    /// 60Hz floor; this records what cadence the run targeted).
    pub refresh: Refresh,
    /// Max allowed `present` p50 (ms).
    pub p50_max_ms: f32,
    /// Max allowed `present` p99 (ms) - the [`FLOOR_P99_MS`] hard floor.
    pub p99_max_ms: f32,
    /// Max allowed single `present` interval (ms). [`FLOOR_P99_MS`] for a steady-state
    /// scenario; [`RESIZE_SPIKE_MS`] for the resize animation.
    pub frame_max_ms: f32,
    /// Dropped-frame budget: `0` steady-state; a small declared count for flood/resize.
    pub dropped_budget: usize,
    /// When `Some(n)`, this is the `idle` gate: assert at most `n` frames drawn in the
    /// measured (post-keep-warm) window, and skip the percentile checks entirely.
    pub idle_max_frames: Option<u64>,
    /// When `true`, assert render is decoupled from the byte rate (`output_flood`).
    pub require_decoupled: bool,
}

impl Gate {
    /// The blocking 60fps-floor gate: p50 <= 16.67ms, p99 <= 16.0ms, max <= 16.0ms, 0
    /// dropped frames. The steady-state default every scenario starts from.
    #[must_use]
    pub fn floor_60hz() -> Self {
        Self {
            refresh: Refresh::Hz60,
            p50_max_ms: Refresh::Hz60.deadline_ms(),
            p99_max_ms: FLOOR_P99_MS,
            frame_max_ms: FLOOR_P99_MS,
            dropped_budget: 0,
            idle_max_frames: None,
            require_decoupled: false,
        }
    }

    /// Builder: allow a bounded dropped-frame budget (flood / resize).
    #[must_use]
    pub fn with_dropped_budget(mut self, budget: usize) -> Self {
        self.dropped_budget = budget;
        self
    }

    /// Builder: allow a larger single-frame spike (the resize transaction handoff).
    #[must_use]
    pub fn with_frame_max_ms(mut self, ms: f32) -> Self {
        self.frame_max_ms = ms;
        self
    }

    /// Builder: turn this into the `idle` gate (assert <= `max_frames` drawn, skip
    /// percentiles).
    #[must_use]
    pub fn idle(mut self, max_frames: u64) -> Self {
        self.idle_max_frames = Some(max_frames);
        self
    }

    /// Builder: require render/byte decoupling (`output_flood`).
    #[must_use]
    pub fn require_decoupled(mut self) -> Self {
        self.require_decoupled = true;
        self
    }

    /// Evaluate this gate against a run's recorded percentiles + facts -> a
    /// [`Verdict`]. Pure: this is the whole pass/fail decision, exhaustively tested.
    #[must_use]
    pub fn evaluate(&self, stats: &FrameStats, facts: &RunFacts) -> Verdict {
        // The idle gate is about NOT drawing: check only the frame budget.
        if let Some(budget) = self.idle_max_frames {
            return if facts.frames_drawn <= budget {
                Verdict::Pass
            } else {
                Verdict::Fail {
                    breaches: vec![Breach::IdleFramesDrawn {
                        observed: facts.frames_drawn,
                        budget,
                    }],
                }
            };
        }

        // Active scenario: too few frames means the run could not measure (headless /
        // no display) - inconclusive, NOT a pass.
        if stats.count < MIN_ACTIVE_SAMPLES {
            return Verdict::Inconclusive {
                samples: stats.count,
                required: MIN_ACTIVE_SAMPLES,
            };
        }

        let mut breaches = Vec::new();
        if stats.present_p50_ms > self.p50_max_ms {
            breaches.push(Breach::P50 {
                observed_ms: stats.present_p50_ms,
                limit_ms: self.p50_max_ms,
            });
        }
        if stats.present_p99_ms > self.p99_max_ms {
            breaches.push(Breach::P99 {
                observed_ms: stats.present_p99_ms,
                limit_ms: self.p99_max_ms,
            });
        }
        if stats.present_max_ms > self.frame_max_ms {
            breaches.push(Breach::MaxFrame {
                observed_ms: stats.present_max_ms,
                limit_ms: self.frame_max_ms,
            });
        }
        if stats.dropped > self.dropped_budget {
            breaches.push(Breach::DroppedFrames {
                observed: stats.dropped,
                budget: self.dropped_budget,
            });
        }
        if self.require_decoupled {
            // Render/byte decoupling (AC4) has TWO falsifiable halves, both required:
            //
            // (a) The byte firehose actually kept flowing: `bytes_fed` (bytes the model
            //     drained during the window) is huge. If a regression coupled the render
            //     INTO the byte-drain path, the vsync-blocked present would throttle the
            //     drain and `bytes_fed` would collapse - so a stalled flood fails here.
            //     This is the honest signal a frame-count ceiling alone cannot give
            //     (under `Fifo` a present can never beat vsync, so frames_drawn alone is
            //     ~vsync for BOTH a healthy and a naively-coupled render).
            // (b) The renderer stayed vsync-paced: `frames_drawn` did not balloon past
            //     the vsync-implied count. This catches a regression that abandons vsync
            //     pacing (drops `Fifo`) and draws per byte-chunk.
            if facts.bytes_fed < MIN_FLOOD_BYTES {
                breaches.push(Breach::FloodStalled {
                    bytes_fed: facts.bytes_fed,
                    required: MIN_FLOOD_BYTES,
                });
            }
            let vsync_frames = facts.vsync_frames(self.refresh);
            let ceiling = (vsync_frames as f32 * DECOUPLE_SLACK) as u64;
            if facts.frames_drawn > ceiling {
                breaches.push(Breach::NotDecoupled {
                    frames_drawn: facts.frames_drawn,
                    vsync_frames,
                });
            }
        }

        if breaches.is_empty() {
            Verdict::Pass
        } else {
            Verdict::Fail { breaches }
        }
    }
}

/// The non-percentile facts of a run the gate needs, alongside [`FrameStats`]: how many
/// frames were actually presented, how many PTY bytes were fed, and how long the
/// measured window ran. Supplied by the driver (or synthesized in tests).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RunFacts {
    /// Frames presented during the measured window.
    pub frames_drawn: u64,
    /// PTY bytes the generator produced during the measured window (the decoupling
    /// denominator - huge for a flood).
    pub bytes_fed: u64,
    /// Wall-clock duration of the measured window (ms).
    pub elapsed_ms: f32,
}

impl RunFacts {
    /// The number of frames a vsync-paced render would present over the measured
    /// window at `refresh` (`elapsed / deadline`, at least 1). The decoupling ceiling
    /// is a small multiple of this.
    #[must_use]
    pub fn vsync_frames(&self, refresh: Refresh) -> u64 {
        let deadline = refresh.deadline_ms().max(f32::MIN_POSITIVE);
        ((self.elapsed_ms / deadline).round() as u64).max(1)
    }
}

/// One breached gate condition, with the observed value and the limit it exceeded - so
/// a CI failure reads as "output_flood p99 18.4ms > 16.0ms", not just "failed".
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Breach {
    P50 {
        observed_ms: f32,
        limit_ms: f32,
    },
    P99 {
        observed_ms: f32,
        limit_ms: f32,
    },
    MaxFrame {
        observed_ms: f32,
        limit_ms: f32,
    },
    DroppedFrames {
        observed: usize,
        budget: usize,
    },
    IdleFramesDrawn {
        observed: u64,
        budget: u64,
    },
    NotDecoupled {
        frames_drawn: u64,
        vsync_frames: u64,
    },
    /// The flood did not sustain a byte firehose (`bytes_fed` below the floor) - the
    /// model's byte-drain was throttled, which is what a render coupled into the drain
    /// path would cause. Half of the `output_flood` decoupling proof.
    FloodStalled {
        bytes_fed: u64,
        required: u64,
    },
}

/// The gate outcome for one scenario. `Inconclusive` is distinct from both pass and
/// fail: it means the run recorded too few frames to judge (a headless/no-display
/// environment), and the driver treats it as a loud non-fatal skip rather than a green
/// pass or a red breach.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    // A struct variant (not `Fail(Vec<..>)`): an internally-tagged (`tag = "result"`)
    // enum cannot serialize a tuple/newtype variant wrapping a sequence, so the
    // breaches live in a named field.
    Fail { breaches: Vec<Breach> },
    Inconclusive { samples: usize, required: usize },
}

impl Verdict {
    /// Whether this verdict is a hard failure (the only outcome that blocks release).
    #[must_use]
    pub fn is_fail(&self) -> bool {
        matches!(self, Verdict::Fail { .. })
    }

    /// Whether this verdict is a clean pass.
    #[must_use]
    pub fn is_pass(&self) -> bool {
        matches!(self, Verdict::Pass)
    }
}

/// The informational 120Hz-target check ([`09-performance-60fps.md`] §9, owner
/// open-question #4: track 120fps, do not block on it). Computed from the same
/// percentiles but NEVER affecting the [`Verdict`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Target120 {
    /// Whether the run also met the 120Hz target (p50 <= 8.33, p99 <= 8.0, max <= 16).
    pub met: bool,
    pub p50_ms: f32,
    pub p99_ms: f32,
}

impl Target120 {
    /// Evaluate the (non-blocking) 120Hz target over a run's percentiles.
    #[must_use]
    pub fn from_stats(stats: &FrameStats) -> Self {
        let p50_target = Refresh::Hz120.deadline_ms();
        Self {
            met: stats.present_p50_ms <= p50_target
                && stats.present_p99_ms <= 8.0
                && stats.present_max_ms <= FLOOR_P99_MS,
            p50_ms: stats.present_p50_ms,
            p99_ms: stats.present_p99_ms,
        }
    }
}

/// A fully-declared scenario: its identity + blocking gate + the deterministic input
/// program (an optional output [`Generator`] plus the timed synthetic-input `script`)
/// and the driver's timing knobs. Reviewable *source*, byte-identical every run (no rng,
/// no clock) - the property the whole harness rests on.
#[derive(Debug, Clone, PartialEq)]
pub struct Scenario {
    pub kind: ScenarioKind,
    pub gate: Gate,
    /// Engine scrollback ring size for this scenario (1M for `large_scrollback`,
    /// [`aterm_core::DEFAULT_SCROLLBACK`] otherwise).
    pub scrollback: usize,
    /// The output source spawned at the start of the run, if any.
    pub setup: Option<Generator>,
    /// The timed synthetic-input program applied across the measured window.
    pub script: Vec<DriverAction>,
    /// Warm-up window (ms) after `setup` starts, before frames are measured - lets the
    /// output settle and, for `idle`, elapses the keep-warm window so the measured part
    /// is genuinely idle.
    pub warmup_ms: u32,
    /// Duration (ms) of the measured window (where the gate applies).
    pub measure_ms: u32,
}

impl Scenario {
    /// The scenario's canonical name.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.kind.name()
    }
}

/// The default scrollback for scenarios that do not need a deep ring.
const DEFAULT_RING: usize = aterm_core::DEFAULT_SCROLLBACK;

/// Build the `fast_scroll` scenario: seq 100k lines, then sweep the timeline downward
/// page by page (scroll throughput + damage).
fn fast_scroll() -> Scenario {
    let mut script = vec![DriverAction::Scroll(ScrollAction::ToTop)];
    // ~120 page-downs (2s @ 60fps of continuous relayout): enough repeated
    // scroll->layout->present to expose a throughput regression without traversing all
    // 100k lines (the stress is the repeated relayout, not the distance).
    for _ in 0..120 {
        script.push(DriverAction::Scroll(ScrollAction::PageDown));
        script.push(DriverAction::Idle { ms: 12 });
    }
    Scenario {
        kind: ScenarioKind::FastScroll,
        gate: Gate::floor_60hz(),
        scrollback: 100_000,
        setup: Some(Generator::Seq { lines: 100_000 }),
        script,
        warmup_ms: 400,
        measure_ms: 2_000,
    }
}

/// Build the `output_flood` scenario: `yes` for the measured window, asserting render
/// stays vsync-paced (decoupled from the byte firehose).
fn output_flood() -> Scenario {
    Scenario {
        kind: ScenarioKind::OutputFlood,
        gate: Gate::floor_60hz()
            // A flood may drop a bounded handful of frames as the coalescer catches up.
            .with_dropped_budget(2)
            .require_decoupled(),
        scrollback: DEFAULT_RING,
        setup: Some(Generator::Yes { run_ms: 5_000 }),
        script: vec![DriverAction::Idle { ms: 5_000 }],
        warmup_ms: 300,
        measure_ms: 5_000,
    }
}

/// Build the `large_scrollback` scenario: a 1M-line ring, then jump to the top and page
/// through deep history (memory + deep-scroll).
fn large_scrollback() -> Scenario {
    let mut script = vec![DriverAction::Scroll(ScrollAction::ToTop)];
    for _ in 0..60 {
        script.push(DriverAction::Scroll(ScrollAction::PageDown));
        script.push(DriverAction::Idle { ms: 16 });
    }
    Scenario {
        kind: ScenarioKind::LargeScrollback,
        gate: Gate::floor_60hz(),
        scrollback: 1_000_000,
        setup: Some(Generator::Seq { lines: 1_000_000 }),
        script,
        warmup_ms: 800,
        measure_ms: 2_000,
    }
}

/// Build the `agent_stream_while_typing` scenario: stream assistant tokens into the
/// timeline while replaying keystrokes into the input box (concurrent stream + edit).
fn agent_stream_while_typing() -> Scenario {
    let mut script = vec![
        DriverAction::Input(InputAction::ToggleMode), // into Agent mode
        DriverAction::AgentOpen {
            kind: AgentStepKind::AssistantText,
            text: String::new(),
        },
    ];
    // Alternate a streamed token and a typed character, ~80 rounds, so the timeline's
    // last agent block mutates while the input box's InputModel is also edited.
    let words = [
        "Let ",
        "me ",
        "check ",
        "the ",
        "grid. ",
        "Streaming ",
        "tokens. ",
    ];
    for i in 0..80 {
        script.push(DriverAction::AgentToken(words[i % words.len()].to_string()));
        script.push(DriverAction::Input(InputAction::Type("x".to_string())));
        script.push(DriverAction::Idle { ms: 20 });
    }
    Scenario {
        kind: ScenarioKind::AgentStreamWhileTyping,
        gate: Gate::floor_60hz(),
        scrollback: DEFAULT_RING,
        setup: None,
        script,
        warmup_ms: 200,
        measure_ms: 2_000,
    }
}

/// Build the `window_resize` scenario: animate the window 800->1600px over ~1s (reflow
/// + the `presentsWithTransaction` path), allowing a bounded resize spike/drop budget.
fn window_resize() -> Scenario {
    let mut script = Vec::new();
    // 60 steps over ~1s: 800 -> 1600 wide, height tracks 2:3-ish for a real reflow.
    for step in 0..=60u32 {
        let width = 800 + (800 * step) / 60;
        let height = 600 + (600 * step) / 60;
        script.push(DriverAction::Resize { width, height });
        script.push(DriverAction::Idle { ms: 16 });
    }
    Scenario {
        kind: ScenarioKind::WindowResize,
        gate: Gate::floor_60hz()
            // The transaction handoff is allowed one spike frame + a small drop budget.
            .with_frame_max_ms(RESIZE_SPIKE_MS)
            .with_dropped_budget(2),
        scrollback: DEFAULT_RING,
        setup: Some(Generator::Seq { lines: 2_000 }),
        script,
        warmup_ms: 300,
        // 2s (not 1.1s) so the measured window records >= 100 frames at BOTH refreshes
        // (>=120 @ 60Hz, >=240 @ 120Hz). That matters because the nearest-rank p99 of a
        // <100-frame window collapses to the max, which would make the single allowed
        // transaction spike (`RESIZE_SPIKE_MS` in `frame_max`) ALSO trip the tight
        // p99<=16ms gate. With >=100 frames the single worst (spike) frame falls
        // strictly past the p99 rank, so p99 stays tight while `frame_max` absorbs the
        // one spike - matching "resize allowed a one-frame transaction spike" (§9). The
        // ~1s animation is paced across this window (a short settle tail follows).
        measure_ms: 2_000,
    }
}

/// Build the `fullscreen_tui_redraw` scenario: repeated full-screen alt-screen repaints
/// (full-grid invalidation every frame).
fn fullscreen_tui_redraw() -> Scenario {
    Scenario {
        kind: ScenarioKind::FullscreenTuiRedraw,
        gate: Gate::floor_60hz(),
        scrollback: DEFAULT_RING,
        setup: Some(Generator::AltScreenRepaint),
        script: vec![DriverAction::Idle { ms: 2_000 }],
        warmup_ms: 300,
        measure_ms: 2_000,
    }
}

/// Build the `idle` scenario: after the keep-warm window elapses, assert ~0 frames are
/// drawn over a 5s idle window.
fn idle() -> Scenario {
    Scenario {
        kind: ScenarioKind::Idle,
        gate: Gate::floor_60hz().idle(2),
        scrollback: DEFAULT_RING,
        setup: None,
        script: vec![DriverAction::Idle { ms: 5_000 }],
        // Elapse the ~1s keep-warm window (plus margin) so the measured part is idle.
        warmup_ms: 1_500,
        measure_ms: 5_000,
    }
}

/// The seven scenarios in table order ([`09-performance-60fps.md`] §9). Deterministic
/// and byte-identical across calls (built programmatically, no rng/clock/env).
#[must_use]
pub fn all_scenarios() -> Vec<Scenario> {
    vec![
        fast_scroll(),
        output_flood(),
        large_scrollback(),
        agent_stream_while_typing(),
        window_resize(),
        fullscreen_tui_redraw(),
        idle(),
    ]
}

/// The recorded result of one scenario run: its percentiles, the run facts, the
/// blocking [`Verdict`], the informational 120Hz target, and the raw samples for
/// offline histogram analysis. Serializes to the JSON the driver dumps (and CI reads).
#[derive(Debug, Clone, Serialize)]
pub struct ScenarioReport {
    pub scenario: &'static str,
    pub refresh: Refresh,
    pub stats: FrameStats,
    pub facts: RunFacts,
    pub verdict: Verdict,
    pub target_120hz: Target120,
    /// Raw per-frame samples (oldest first) - the offline histogram input.
    pub samples: Vec<FrameSample>,
}

impl ScenarioReport {
    /// Assemble a report from a scenario's gate + its recorded window. Computes the
    /// verdict + the informational 120Hz target from `stats`/`facts`.
    #[must_use]
    pub fn new(
        scenario: &Scenario,
        stats: FrameStats,
        facts: RunFacts,
        samples: Vec<FrameSample>,
    ) -> Self {
        let verdict = scenario.gate.evaluate(&stats, &facts);
        let target_120hz = Target120::from_stats(&stats);
        Self {
            scenario: scenario.name(),
            refresh: scenario.gate.refresh,
            stats,
            facts,
            verdict,
            target_120hz,
            samples,
        }
    }
}

/// A full driver run: every scenario's report + the overall gate outcome. `overall_pass`
/// is false iff ANY scenario hard-failed (inconclusive does not fail the run - see
/// [`Verdict`]).
#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    pub scenarios: Vec<ScenarioReport>,
    /// True iff no scenario produced a [`Verdict::Fail`].
    pub overall_pass: bool,
    /// Scenario names that were [`Verdict::Inconclusive`] (surfaced, not silently
    /// dropped - e.g. a headless CI run records too few frames to judge).
    pub inconclusive: Vec<&'static str>,
}

impl RunReport {
    /// Assemble the run report from the per-scenario reports, deriving `overall_pass`
    /// and the inconclusive list.
    #[must_use]
    pub fn new(scenarios: Vec<ScenarioReport>) -> Self {
        let overall_pass = !scenarios.iter().any(|r| r.verdict.is_fail());
        let inconclusive = scenarios
            .iter()
            .filter(|r| matches!(r.verdict, Verdict::Inconclusive { .. }))
            .map(|r| r.scenario)
            .collect();
        Self {
            scenarios,
            overall_pass,
            inconclusive,
        }
    }

    /// Serialize to pretty JSON for the dump artifact.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `FrameStats` with the given present percentiles + dropped count (cpu fields
    /// are irrelevant to the gate; count is set high enough to be conclusive).
    fn stats(p50: f32, p99: f32, max: f32, dropped: usize) -> FrameStats {
        FrameStats {
            count: 200,
            cpu_p50_ms: 1.0,
            cpu_p99_ms: 2.0,
            cpu_max_ms: 3.0,
            present_p50_ms: p50,
            present_p99_ms: p99,
            present_max_ms: max,
            dropped,
            dropped_pct: (dropped as f32 / 200.0) * 100.0,
        }
    }

    fn facts(frames_drawn: u64, bytes_fed: u64, elapsed_ms: f32) -> RunFacts {
        RunFacts {
            frames_drawn,
            bytes_fed,
            elapsed_ms,
        }
    }

    #[test]
    fn all_seven_scenarios_present_with_exact_names() {
        let s = all_scenarios();
        assert_eq!(s.len(), 7);
        let names: Vec<&str> = s.iter().map(Scenario::name).collect();
        assert_eq!(
            names,
            vec![
                "fast_scroll",
                "output_flood",
                "large_scrollback",
                "agent_stream_while_typing",
                "window_resize",
                "fullscreen_tui_redraw",
                "idle",
            ]
        );
    }

    #[test]
    fn scenarios_are_deterministic_and_nonempty() {
        // Byte-identical across calls (no rng/clock/env) - the property the whole
        // harness rests on, mirroring the fixtures test.
        assert_eq!(all_scenarios(), all_scenarios());
        for sc in all_scenarios() {
            assert!(
                !sc.script.is_empty(),
                "{} must have a non-empty script",
                sc.name()
            );
            assert!(sc.measure_ms > 0);
        }
    }

    #[test]
    fn large_scrollback_sizes_a_deep_ring() {
        let big = all_scenarios()
            .into_iter()
            .find(|s| s.kind == ScenarioKind::LargeScrollback)
            .unwrap();
        assert_eq!(big.scrollback, 1_000_000, "the 1M-line ring");
    }

    #[test]
    fn clean_run_passes_the_floor_gate() {
        // A steady 120Hz-ish run (present ~8.3ms, no drops) passes the 60fps floor.
        let g = Gate::floor_60hz();
        let v = g.evaluate(&stats(8.3, 8.4, 9.0, 0), &facts(240, 1_000, 2_000.0));
        assert!(v.is_pass(), "clean run must pass: {v:?}");
    }

    #[test]
    fn p99_over_the_floor_fails() {
        // AC: a scenario breaching p99 <= 16.0ms fails.
        let g = Gate::floor_60hz();
        let v = g.evaluate(&stats(10.0, 18.4, 20.0, 0), &facts(120, 1_000, 2_000.0));
        assert!(v.is_fail());
        let Verdict::Fail { breaches } = &v else {
            panic!("expected fail, got {v:?}")
        };
        assert!(
            breaches.iter().any(
                |b| matches!(b, Breach::P99 { limit_ms, .. } if (*limit_ms - 16.0).abs() < 1e-6)
            ),
            "the p99 breach names the 16.0ms floor: {breaches:?}"
        );
    }

    #[test]
    fn a_single_max_spike_fails_steady_state_but_passes_under_resize_allowance() {
        // A one-frame 33ms spike fails a steady-state scenario (max <= 16)...
        let steady = Gate::floor_60hz();
        assert!(steady
            .evaluate(&stats(8.0, 9.0, 33.0, 0), &facts(120, 1_000, 2_000.0))
            .is_fail());
        // ...but passes the resize gate, which declares a one-frame transaction spike.
        let resize = Gate::floor_60hz().with_frame_max_ms(RESIZE_SPIKE_MS);
        assert!(resize
            .evaluate(&stats(8.0, 9.0, 33.0, 0), &facts(120, 1_000, 2_000.0))
            .is_pass());
    }

    #[test]
    fn dropped_frames_respect_the_budget() {
        // Steady-state: any drop fails.
        assert!(Gate::floor_60hz()
            .evaluate(&stats(8.0, 9.0, 10.0, 1), &facts(120, 1_000, 2_000.0))
            .is_fail());
        // Flood: within the declared budget passes, one over fails.
        let flood = Gate::floor_60hz().with_dropped_budget(2);
        assert!(flood
            .evaluate(&stats(8.0, 9.0, 10.0, 2), &facts(120, 1_000, 2_000.0))
            .is_pass());
        assert!(flood
            .evaluate(&stats(8.0, 9.0, 10.0, 3), &facts(120, 1_000, 2_000.0))
            .is_fail());
    }

    #[test]
    fn idle_gate_asserts_near_zero_frames() {
        // AC: idle asserts ~0 frames drawn after keep-warm.
        let g = Gate::floor_60hz().idle(2);
        // 0 frames in the idle window: pass (percentiles ignored, even if empty).
        assert!(g
            .evaluate(&stats(0.0, 0.0, 0.0, 0), &facts(0, 0, 5_000.0))
            .is_pass());
        // Still drawing frames while "idle": fail.
        let v = g.evaluate(&stats(0.0, 0.0, 0.0, 0), &facts(50, 0, 5_000.0));
        assert!(v.is_fail());
        assert!(matches!(
            v,
            Verdict::Fail { ref breaches } if matches!(breaches[0], Breach::IdleFramesDrawn { observed: 50, budget: 2 })
        ));
    }

    #[test]
    fn output_flood_gates_render_byte_decoupling() {
        // AC: output_flood shows render decoupled from byte-rate. Decoupling has TWO
        // falsifiable halves - a sustained byte firehose AND vsync-paced frames - and
        // both must hold. Over a 5s window @60Hz, vsync implies ~300 frames.
        let flood = Gate::floor_60hz()
            .with_dropped_budget(2)
            .require_decoupled();
        let huge_bytes = 200 * 1024 * 1024;
        // Decoupled: the model drained 200MB while the renderer presented only ~vsync
        // frames -> pass.
        assert!(
            flood
                .evaluate(&stats(8.0, 9.0, 10.0, 1), &facts(300, huge_bytes, 5_000.0))
                .is_pass(),
            "huge byte firehose + vsync-paced frames = decoupled"
        );
        // The flood stalled: only a trickle of bytes drained (a render coupled INTO the
        // byte-drain path throttles it) -> FloodStalled, even though frames look fine.
        let stalled = flood.evaluate(&stats(8.0, 9.0, 10.0, 0), &facts(300, 1_000, 5_000.0));
        assert!(stalled.is_fail());
        assert!(
            matches!(&stalled, Verdict::Fail { breaches } if breaches.iter().any(|x| matches!(x, Breach::FloodStalled { .. }))),
            "a throttled byte-drain is a FloodStalled breach: {stalled:?}"
        );
        // Frames ballooned past vsync (a regression that abandons vsync pacing and draws
        // per chunk) -> NotDecoupled, even with the firehose flowing.
        let coupled = flood.evaluate(
            &stats(8.0, 9.0, 10.0, 1),
            &facts(6_000, huge_bytes, 5_000.0),
        );
        assert!(coupled.is_fail());
        assert!(
            matches!(&coupled, Verdict::Fail { breaches } if breaches.iter().any(|x| matches!(x, Breach::NotDecoupled { .. }))),
            "ballooned frame count is a NotDecoupled breach: {coupled:?}"
        );
    }

    #[test]
    fn window_resize_measure_window_keeps_p99_below_the_max_spike() {
        // The window_resize gate allows a single transaction spike via `frame_max`
        // (RESIZE_SPIKE_MS) while keeping p99 tight at the 16ms floor. That only works
        // if the measured window records >= 100 frames, otherwise the nearest-rank p99
        // collapses to the max and the allowed spike would trip p99. Assert the window
        // is sized so a single spike falls strictly past the p99 rank at BOTH refreshes.
        let resize = all_scenarios()
            .into_iter()
            .find(|s| s.kind == ScenarioKind::WindowResize)
            .unwrap();
        for hz in [60.0_f32, 120.0] {
            let frames = (f32::from(resize.measure_ms as u16) / (1000.0 / hz)).floor() as usize;
            assert!(
                frames >= 100,
                "window_resize must record >=100 frames @ {hz}Hz (got {frames}) so the \
                 single allowed spike falls past the p99 rank"
            );
        }
        // Concretely: 200 fast frames + one 33ms spike -> p99 stays fast, max absorbs
        // the spike -> the resize gate passes (it would FAIL if p99 collapsed to max).
        let mut samples: Vec<FrameSample> = (0..200)
            .map(|i| FrameSample {
                frame: i,
                cpu_frame_ms: 2.0,
                gpu_frame_ms: None,
                present_interval_ms: 8.0,
                dirty_cells: 10,
                allocations: None,
                frame_dropped: false,
            })
            .collect();
        samples[100].present_interval_ms = 33.0; // the one transaction spike
        let st = FrameStats::from_samples(&samples);
        assert!(st.present_p99_ms <= 16.0, "p99 excludes the single spike");
        assert!(st.present_max_ms > 16.0, "the spike is the max");
        assert!(
            resize.gate.evaluate(&st, &facts(200, 0, 2_000.0)).is_pass(),
            "one spike + tight p99 passes the resize gate"
        );
    }

    #[test]
    fn too_few_samples_is_inconclusive_not_a_pass() {
        // The false-pass guard: a headless run records ~no frames -> inconclusive.
        let g = Gate::floor_60hz();
        let sparse = FrameStats::from_samples(&[]);
        let v = g.evaluate(&sparse, &facts(0, 0, 2_000.0));
        assert!(
            matches!(v, Verdict::Inconclusive { samples: 0, required } if required == MIN_ACTIVE_SAMPLES),
            "empty run is inconclusive, never a pass: {v:?}"
        );
        assert!(!v.is_pass() && !v.is_fail());
    }

    #[test]
    fn target_120hz_is_informational_only() {
        // A run that passes the 60fps floor but not the 120Hz target: still a Pass, but
        // target_120hz.met is false (tracked, non-blocking).
        let sc = &fast_scroll();
        let report = ScenarioReport::new(
            &sc.clone(),
            stats(12.0, 15.0, 15.5, 0),
            facts(120, 0, 2_000.0),
            vec![],
        );
        assert!(report.verdict.is_pass(), "passes the 60fps floor");
        assert!(
            !report.target_120hz.met,
            "but does not meet the 120Hz target"
        );
        // A genuinely fast run meets both.
        let fast =
            ScenarioReport::new(sc, stats(8.0, 7.9, 15.0, 0), facts(240, 0, 2_000.0), vec![]);
        assert!(fast.verdict.is_pass() && fast.target_120hz.met);
    }

    #[test]
    fn run_report_overall_pass_and_inconclusive_tracking() {
        let sc = fast_scroll();
        let pass = ScenarioReport::new(
            &sc,
            stats(8.0, 9.0, 10.0, 0),
            facts(120, 0, 2_000.0),
            vec![],
        );
        let incon = ScenarioReport::new(
            &sc,
            FrameStats::from_samples(&[]),
            facts(0, 0, 2_000.0),
            vec![],
        );
        // All-pass -> overall pass, no inconclusive.
        assert!(RunReport::new(vec![pass.clone()]).overall_pass);
        // A fail anywhere -> overall fail.
        let fail = ScenarioReport::new(
            &sc,
            stats(8.0, 20.0, 22.0, 0),
            facts(120, 0, 2_000.0),
            vec![],
        );
        assert!(!RunReport::new(vec![pass.clone(), fail]).overall_pass);
        // Inconclusive does NOT fail the run but is surfaced.
        let r = RunReport::new(vec![pass, incon]);
        assert!(r.overall_pass, "inconclusive does not fail the run");
        assert_eq!(r.inconclusive, vec!["fast_scroll"]);
    }

    #[test]
    fn run_report_json_is_valid_and_carries_the_gate_result() {
        // AC: a run dumps valid JSON consumable by an analysis step. Serialize ->
        // parse -> assert the structure + the verdict + a round-tripped sample.
        let sc = fast_scroll();
        let sample = FrameSample {
            frame: 0,
            cpu_frame_ms: 2.0,
            gpu_frame_ms: None,
            present_interval_ms: 8.3,
            dirty_cells: 100,
            allocations: None,
            frame_dropped: false,
        };
        let report = ScenarioReport::new(
            &sc,
            stats(8.0, 20.0, 22.0, 5),
            facts(120, 4_096, 2_000.0),
            vec![sample],
        );
        let run = RunReport::new(vec![report]);
        let json = run.to_json();
        let v: serde_json::Value = serde_json::from_str(&json).expect("run JSON must parse");
        assert_eq!(v["overall_pass"], false, "the p99 breach fails the run");
        let s0 = &v["scenarios"][0];
        assert_eq!(s0["scenario"], "fast_scroll");
        assert_eq!(s0["verdict"]["result"], "fail");
        assert_eq!(s0["stats"]["dropped"], 5);
        // The raw sample round-trips (FrameSample is Deserialize) - the histogram input.
        let parsed: FrameSample =
            serde_json::from_value(s0["samples"][0].clone()).expect("sample must round-trip");
        assert_eq!(parsed.present_interval_ms, 8.3);
    }

    #[test]
    fn vsync_frame_count_tracks_the_refresh() {
        // 1000ms @ 60Hz -> 60 frames; @ 120Hz -> 120.
        assert_eq!(facts(0, 0, 1_000.0).vsync_frames(Refresh::Hz60), 60);
        assert_eq!(facts(0, 0, 1_000.0).vsync_frames(Refresh::Hz120), 120);
        // Never zero (avoids a divide-by-nothing ceiling).
        assert_eq!(facts(0, 0, 0.0).vsync_frames(Refresh::Hz60), 1);
    }
}
