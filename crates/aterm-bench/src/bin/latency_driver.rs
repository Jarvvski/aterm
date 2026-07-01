//! The input-latency driver (ticket T-7.3): the on-hardware software measure of
//! keystroke->visible-glyph latency against the REAL `aterm-ui` app loop.
//!
//! It is the live sibling of the pure [`aterm_bench::latency`] model (which owns the
//! percentile summary + the gate + the JSON, unit-tested with no window), exactly as
//! `scenario_driver` is the live sibling of `aterm_bench::scenario`. Per iteration it
//! applies a synthetic keystroke to the input box's `InputModel` at a frame boundary -
//! so the *current* frame does not carry it - and records the wall-clock delay to the
//! present of the NEXT frame, the first one that renders the new glyph. Over
//! [`TARGET_ITERATIONS`] iterations that yields the median + p25/p75 + outliers the
//! GNOME-46 methodology reports, gated at median <= 1.5 frames / p99 <= 3 frames.
//!
//! ## What this captures (and the honest limits)
//!
//! This times **model-mutation -> the frame that presents it**: frame build + GPU
//! encode/submit + the vsync present wait, i.e. the dominant, controllable part of the
//! pipeline. It does NOT capture the OS/winit event dispatch before the model mutation,
//! nor the sub-frame arrival phase of a real keystroke - both of which only a
//! keyboard-to-photon hardware rig (stubbed at [`aterm_bench::latency::HardwareRig`],
//! owner open-question #3) measures. So this software number is a *lower bound* on true
//! keystroke->photon latency and a regression detector, not the absolute ground truth.
//!
//! Like `scenario_driver` it is headless-safe: where the runner cannot present (no
//! display / GPU) it captures too few iterations and reports
//! [`LatencyVerdict::Inconclusive`], exiting 0 - never a false pass or a false failure.
//!
//! Usage: `latency_driver [--gate] [--out <path>] [--display-link]`
//! - `--gate`: exit non-zero if the run hard-fails the latency gate (the nightly mode).
//! - `--out <path>`: write the JSON [`LatencyReport`] to `<path>` (else stdout).
//! - `--display-link`: use the self-bridged CADisplayLink vsync clock (real ProMotion),
//!   which also raises the frame-equivalent gate to 120Hz (the panel's true cadence).
//!
//! [`09-performance-60fps.md`]: ../../../docs/research/09-performance-60fps.md

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_core::{InputEvent, InputModel};
use aterm_ui::{
    run_with_recorder, FrameRecorder, FrameSample, Refresh, RenderConfig, ThemeKind, UiCallbacks,
    DEFAULT_CAPACITY,
};

use aterm_bench::latency::{
    LatencyGate, LatencyReport, LatencySample, LatencyVerdict, TARGET_ITERATIONS,
};

/// Frames to present before the first measurement, so the glyph atlas + pipeline are
/// warm and the present cadence is steady (~1s @60Hz). Warmup frames are neither
/// injected into nor recorded.
const WARMUP_FRAMES: u64 = 60;

/// Frames to idle between one recorded iteration and the next keystroke, so consecutive
/// measurements are independent presses rather than one saturated beat.
const GAP_FRAMES: u64 = 2;

/// Wall-clock backstop: if the run has not gathered its iterations within this window
/// (e.g. the compositor throttled presents to a crawl), finalize with whatever was
/// captured - which, if too few, is honestly [`LatencyVerdict::Inconclusive`] - rather
/// than spinning forever. The loop also exits fast if the window/GPU never initializes.
const MAX_RUN: Duration = Duration::from_secs(30);

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let mut gate_on = false;
    let mut out_path: Option<String> = None;
    let mut display_link = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--gate" => gate_on = true,
            "--display-link" => display_link = true,
            "--out" => out_path = args.next(),
            "--help" | "-h" => {
                println!("latency_driver [--gate] [--out <path>] [--display-link]");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("latency_driver: unknown argument {other:?}");
                return ExitCode::from(2);
            }
        }
    }

    // On a real ProMotion panel (opted into via the CADisplayLink clock) presents land
    // every 8.33ms, so the frame-equivalent gate is judged at 120Hz; otherwise the
    // conservative 60Hz floor (a GitHub-hosted runner is not ProMotion). This mirrors
    // how `scenario_driver` treats the two cadences.
    let refresh = if display_link {
        Refresh::Hz120
    } else {
        Refresh::Hz60
    };
    let gate = LatencyGate::frames_gate(refresh);

    let results: Arc<Mutex<Option<LatencyReport>>> = Arc::new(Mutex::new(None));
    let callbacks = LatencyCallbacks::new(gate, Arc::clone(&results));
    // A small recorder is installed only so the per-frame `on_frame` hook fires (the
    // measurement clock); its frame_dropped derivation is irrelevant to latency.
    let recorder = FrameRecorder::new(DEFAULT_CAPACITY, refresh);
    let config = RenderConfig { display_link };

    log::info!("latency_driver: starting the live app loop (display_link={display_link}, gate@{refresh:?})");
    if let Err(e) = run_with_recorder(ThemeKind::Dark, callbacks, config, recorder) {
        eprintln!("latency_driver: event loop error: {e}");
        return ExitCode::FAILURE;
    }

    let report = results.lock().unwrap_or_else(|p| p.into_inner()).take();
    let Some(report) = report else {
        eprintln!(
            "latency_driver: WARNING - no frames were produced (headless / no display?); \
             treating as INCONCLUSIVE, not a pass."
        );
        return ExitCode::SUCCESS;
    };

    let json = report.to_json();
    match &out_path {
        Some(path) => {
            if let Err(e) = std::fs::write(path, &json) {
                eprintln!("latency_driver: failed to write {path}: {e}");
                return ExitCode::FAILURE;
            }
            log::info!("latency_driver: wrote report to {path}");
        }
        None => println!("{json}"),
    }

    summarize(&report);

    if gate_on && report.verdict.is_fail() {
        eprintln!("latency_driver: GATE FAILED - keystroke->glyph latency breached the gate.");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Log the one-line human summary (the CI console view).
fn summarize(report: &LatencyReport) {
    let s = &report.stats;
    let verdict = match &report.verdict {
        LatencyVerdict::Pass => "PASS".to_string(),
        LatencyVerdict::Fail { breaches } => format!("FAIL {breaches:?}"),
        LatencyVerdict::Inconclusive {
            iterations,
            required,
        } => format!("INCONCLUSIVE ({iterations}/{required} iterations)"),
    };
    log::info!(
        "input-latency {verdict} n={} median={:.2}ms ({:.2}f) p25={:.2}ms p75={:.2}ms \
         p99={:.2}ms ({:.2}f) max={:.2}ms outliers={} cadence={:.2}ms refresh={:?}",
        s.count,
        s.median_ms,
        s.median_frames,
        s.p25_ms,
        s.p75_ms,
        s.p99_ms,
        s.p99_frames,
        s.max_ms,
        s.outliers,
        s.interval_ms,
        s.refresh,
    );
}

/// The live driver's [`UiCallbacks`]: it owns the input box's `InputModel` and drives a
/// keystroke-inject-and-measure state machine from the per-frame [`Self::on_frame`] hook.
struct LatencyCallbacks {
    /// The synthetic input box the keystrokes edit (drawn every frame via
    /// [`UiCallbacks::input`], so a mutation genuinely re-renders the box).
    input: InputModel,
    /// The gate the run is judged against (carries the target refresh).
    gate: LatencyGate,

    /// Frames presented since launch (the measurement clock; the `on_frame` sample's own
    /// index is per-recorder and not used here).
    frames_seen: u64,
    /// When set, a keystroke was applied on frame `.1` at instant `.0` and we are waiting
    /// for the first later frame that renders it (glyph-present time).
    press: Option<(Instant, u64)>,
    /// Frames still to idle before injecting the next keystroke (the inter-iteration
    /// gap), counted down after a recorded iteration.
    cooldown: u64,
    /// Alternate Insert (a new glyph) and Backspace (removing it) so the buffer stays
    /// bounded and every iteration is a genuine visible change; starts with an Insert.
    insert_next: bool,

    /// The recorded keystroke->glyph samples.
    samples: Vec<LatencySample>,
    /// Inter-present intervals (ms) observed during measurement - their median is the
    /// self-calibrating frame denominator (so the gate is judged against the run's actual
    /// present cadence, not a hardcoded refresh).
    present_intervals: Vec<f32>,
    /// 0-based iteration counter (also each sample's `iteration`).
    iteration: u32,
    /// First-tick instant, for the [`MAX_RUN`] backstop.
    started: Option<Instant>,

    /// Where `main` reads the finished report from.
    results: Arc<Mutex<Option<LatencyReport>>>,
    /// True once the target iterations are gathered (or the backstop fired).
    done: bool,
}

impl LatencyCallbacks {
    fn new(gate: LatencyGate, results: Arc<Mutex<Option<LatencyReport>>>) -> Self {
        Self {
            input: InputModel::new(),
            gate,
            frames_seen: 0,
            press: None,
            cooldown: 0,
            insert_next: true,
            samples: Vec::new(),
            present_intervals: Vec::new(),
            iteration: 0,
            started: None,
            results,
            done: false,
        }
    }

    /// Finalize the run into the shared [`LatencyReport`] and request exit. The observed
    /// median present interval (self-calibrating frame denominator) is computed from the
    /// intervals seen during measurement.
    fn finish(&mut self) {
        if self.done {
            return;
        }
        let observed_interval = median_ms(&mut self.present_intervals);
        let report = LatencyReport::new(
            &self.gate,
            std::mem::take(&mut self.samples),
            observed_interval,
        );
        *self.results.lock().unwrap_or_else(|p| p.into_inner()) = Some(report);
        self.done = true;
    }
}

impl UiCallbacks for LatencyCallbacks {
    fn input(&self) -> Option<&InputModel> {
        Some(&self.input)
    }

    fn tick(&mut self) {
        // Wall-clock backstop only: if presents crawled and the run cannot reach its
        // iterations, finalize (honestly Inconclusive if too few) rather than hang.
        let now = Instant::now();
        let started = *self.started.get_or_insert(now);
        if !self.done && now.duration_since(started) > MAX_RUN {
            log::warn!(
                "latency_driver: MAX_RUN elapsed with {}/{} iterations - finalizing.",
                self.iteration,
                TARGET_ITERATIONS
            );
            self.finish();
        }
    }

    fn on_frame(&mut self, sample: FrameSample) {
        if self.done {
            return;
        }
        self.frames_seen += 1;
        let now = Instant::now();

        // Warm the atlas/pipeline + present cadence before measuring.
        if self.frames_seen <= WARMUP_FRAMES {
            return;
        }

        // Track the steady present cadence (post-warmup) - its median is the
        // self-calibrating frame denominator. The first post-warmup frame's interval can
        // be a fresh-burst 0 (the scheduler clears `last_present_at` when it idles), so
        // only finite positive intervals count.
        if sample.present_interval_ms.is_finite() && sample.present_interval_ms > 0.0 {
            self.present_intervals.push(sample.present_interval_ms);
        }

        // A keystroke is in flight: the first frame AFTER the one it was applied on is the
        // first to render the new glyph, so its present time is the glyph-visible time.
        if let Some((press_at, press_frame)) = self.press {
            if self.frames_seen > press_frame {
                let ms = now.duration_since(press_at).as_secs_f32() * 1000.0;
                self.samples.push(LatencySample {
                    iteration: self.iteration,
                    press_to_glyph_ms: ms,
                });
                self.press = None;
                self.iteration += 1;
                self.cooldown = GAP_FRAMES;
                if self.iteration >= TARGET_ITERATIONS {
                    self.finish();
                }
            }
            return;
        }

        // Idle the inter-iteration gap before the next press.
        if self.cooldown > 0 {
            self.cooldown -= 1;
            return;
        }

        // Apply the next keystroke AFTER this frame's render (this frame will NOT carry
        // it), then stamp the press so the NEXT presented frame measures the latency.
        if self.iteration < TARGET_ITERATIONS {
            let ev = if self.insert_next {
                InputEvent::Insert("a".to_string())
            } else {
                InputEvent::Backspace
            };
            self.insert_next = !self.insert_next;
            self.input.reduce(ev);
            self.press = Some((now, self.frames_seen));
        }
    }

    fn wants_redraw(&mut self) -> bool {
        // Keep presenting every vsync through the measurement (so the "next frame"
        // arrives promptly); stop asking once finished.
        !self.done
    }

    fn should_exit(&self) -> bool {
        self.done
    }
}

/// Median of the given ms values (sorts in place), or `None` if empty. Used to reduce the
/// observed present intervals to the single self-calibrating frame denominator.
fn median_ms(values: &mut [f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f32::total_cmp);
    Some(values[values.len() / 2])
}
