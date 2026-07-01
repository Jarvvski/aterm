//! The Tier-2 scenario driver (ticket T-7.2): the on-hardware replay of the seven
//! scripted stress scenarios ([`aterm_bench::scenario`]) against the REAL `aterm-ui`
//! app loop, with the T-7.1 frame recorder installed. It drives deterministic PTY
//! output (generator subprocesses) + synthetic input events (typing / scroll / resize /
//! streamed agent tokens), buckets the recorded frames per scenario, evaluates the
//! 60fps-floor gate, and dumps the JSON [`RunReport`] + a pass/fail exit code.
//!
//! This is the analogue of how T-7.1's recorder is a pure module while its present-loop
//! hook is on-hardware: the [`aterm_bench::scenario`] model + gate are exhaustively
//! unit-tested with no window, and THIS binary is the live piece that needs a real
//! display / GPU / vsync - so it is nightly-only and, where it cannot present (a
//! headless runner), reports every scenario [`Verdict::Inconclusive`] rather than a
//! false pass.
//!
//! ## Fidelity caveat (GitHub-hosted runners)
//!
//! The true "60fps always" proof wants a real ProMotion panel; a GitHub-hosted
//! `macos-14` runner is Apple Silicon but is NOT ProMotion and is timing-noisy
//! ([`09-performance-60fps.md`] §9). So the nightly gate is the **60fps floor** (16ms -
//! the blocking gate regardless), treated as a smoke/regression signal; the 120Hz
//! target is tracked, not blocked; and a genuine 120Hz confirmation is a manual
//! on-hardware run (`--display-link` opts into the CADisplayLink vsync source).
//!
//! Usage: `scenario_driver [--gate] [--out <path>] [--display-link]`
//! - `--gate`: exit non-zero if any scenario hard-fails (the nightly CI mode).
//! - `--out <path>`: write the JSON report to `<path>` (else stdout).
//! - `--display-link`: use the self-bridged CADisplayLink vsync clock (real hardware).
//!
//! [`09-performance-60fps.md`]: ../../../docs/research/09-performance-60fps.md

use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_core::{
    AgentBlock, AgentBlockKind, AgentInjector, Engine, InputEvent, InputModel, Motion,
    PtyDimensions,
};
use aterm_ui::{
    run_with_recorder, FrameRecorder, FrameSample, Refresh, RenderConfig, ScrollCommand, ThemeKind,
    UiCallbacks, Window, DEFAULT_CAPACITY,
};
use winit::dpi::LogicalSize;

use aterm_bench::scenario::{
    all_scenarios, AgentStepKind, DriverAction, Generator, InputAction, RunReport, Scenario,
    ScenarioReport, ScrollAction, Verdict,
};

/// Initial PTY size for a freshly spawned scenario engine; the first real window resize
/// re-syncs it (the `window_resize` scenario then animates it further).
const INITIAL_DIMS: PtyDimensions = PtyDimensions {
    rows: 40,
    cols: 120,
    pixel_width: 0,
    pixel_height: 0,
};

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let mut gate = false;
    let mut out_path: Option<String> = None;
    let mut display_link = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--gate" => gate = true,
            "--display-link" => display_link = true,
            "--out" => out_path = args.next(),
            "--help" | "-h" => {
                println!("scenario_driver [--gate] [--out <path>] [--display-link]");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("scenario_driver: unknown argument {other:?}");
                return ExitCode::from(2);
            }
        }
    }

    let results: Arc<Mutex<Option<RunReport>>> = Arc::new(Mutex::new(None));
    let callbacks = DriverCallbacks::new(all_scenarios(), Arc::clone(&results));
    // The recorder is judged against the 60Hz floor, so a frame's `frame_dropped` flag
    // reflects the blocking gate's deadline (a frame slipping past ~18.67ms).
    let recorder = FrameRecorder::new(DEFAULT_CAPACITY, Refresh::Hz60);
    let config = RenderConfig { display_link };

    log::info!("scenario_driver: starting the live app loop (display_link={display_link})");
    if let Err(e) = run_with_recorder(ThemeKind::Dark, callbacks, config, recorder) {
        eprintln!("scenario_driver: event loop error: {e}");
        return ExitCode::FAILURE;
    }

    // The driver stored its report before requesting exit. `None` means the loop never
    // produced a run (e.g. no display / GPU on this runner) - inconclusive, not a pass.
    let report = results.lock().unwrap_or_else(|p| p.into_inner()).take();
    let Some(report) = report else {
        eprintln!(
            "scenario_driver: WARNING - no frames were produced (headless / no display?); \
             treating as INCONCLUSIVE, not a pass."
        );
        return ExitCode::SUCCESS;
    };

    let json = report.to_json();
    match &out_path {
        Some(path) => {
            if let Err(e) = std::fs::write(path, &json) {
                eprintln!("scenario_driver: failed to write {path}: {e}");
                return ExitCode::FAILURE;
            }
            log::info!("scenario_driver: wrote report to {path}");
        }
        None => println!("{json}"),
    }

    summarize(&report);

    if gate && !report.overall_pass {
        eprintln!("scenario_driver: GATE FAILED - a scenario breached the 60fps floor.");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Log a one-line-per-scenario human summary (the CI console view).
fn summarize(report: &RunReport) {
    for r in &report.scenarios {
        let verdict = match &r.verdict {
            Verdict::Pass => "PASS".to_string(),
            Verdict::Fail { breaches } => format!("FAIL {breaches:?}"),
            Verdict::Inconclusive { samples, required } => {
                format!("INCONCLUSIVE ({samples}/{required} frames)")
            }
        };
        log::info!(
            "{:<26} {verdict:<10} frames={:<5} p50={:.2} p99={:.2} max={:.2} dropped={} 120hz={}",
            r.scenario,
            r.facts.frames_drawn,
            r.stats.present_p50_ms,
            r.stats.present_p99_ms,
            r.stats.present_max_ms,
            r.stats.dropped,
            r.target_120hz.met,
        );
    }
    if !report.inconclusive.is_empty() {
        log::warn!(
            "inconclusive scenarios (too few frames to judge): {:?}",
            report.inconclusive
        );
    }
    log::info!(
        "overall: {}",
        if report.overall_pass {
            "PASS (60fps floor)"
        } else {
            "FAIL"
        }
    );
}

/// The live driver's [`UiCallbacks`]: it owns the current scenario's engine + input
/// model, advances a time-based state machine from [`UiCallbacks::tick`] (main thread,
/// every wake), and feeds the recorder's per-frame samples into per-scenario buckets.
struct DriverCallbacks {
    scenarios: Vec<Scenario>,
    idx: usize,
    /// The scenario currently running (a clone, so `tick` can mutate `self` freely).
    current: Option<Scenario>,

    /// Set when the current scenario begins its warm-up; `None` means "start it".
    scenario_started: Option<Instant>,
    /// Set when the measured window begins; `None` while warming up.
    measure_started: Option<Instant>,
    /// True only during the measured window (gates `on_frame` sample collection).
    measuring: bool,
    /// True while the app should keep presenting at vsync (warm-up + measure of a
    /// non-idle scenario) - drives [`UiCallbacks::wants_redraw`].
    want_warm: bool,

    /// The next scripted action's due time + the even pacing interval.
    next_action_at: Option<Instant>,
    action_interval: Duration,
    script_idx: usize,

    /// `bytes_drained` at the measure-window start (the decoupling baseline).
    bytes_baseline: u64,
    /// The measured window's recorded frames (the current scenario's samples).
    samples: Vec<FrameSample>,

    /// The current scenario's output source, if any (dropped at teardown -> kills the
    /// generator subprocess + its threads).
    engine: Option<Engine>,
    /// The current scenario's agent-injector (for `agent_stream_while_typing`).
    injector: Option<AgentInjector>,
    /// The synthetic input box (typed into by the scenario).
    input: InputModel,
    /// The window handle (stashed at `on_ready`) - for the resize animation.
    window: Option<Arc<Window>>,
    /// A scroll command queued by the script, drained by [`UiCallbacks::poll_scroll`].
    pending_scroll: Option<ScrollCommand>,

    /// Reports collected so far; folded into the shared [`RunReport`] when done.
    reports: Vec<ScenarioReport>,
    /// Where `main` reads the finished run report from.
    results: Arc<Mutex<Option<RunReport>>>,
    /// True once every scenario has run - drives [`UiCallbacks::should_exit`].
    done: bool,
}

impl DriverCallbacks {
    fn new(scenarios: Vec<Scenario>, results: Arc<Mutex<Option<RunReport>>>) -> Self {
        Self {
            scenarios,
            idx: 0,
            current: None,
            scenario_started: None,
            measure_started: None,
            measuring: false,
            want_warm: false,
            next_action_at: None,
            action_interval: Duration::ZERO,
            script_idx: 0,
            bytes_baseline: 0,
            samples: Vec::new(),
            engine: None,
            injector: None,
            input: InputModel::new(),
            window: None,
            pending_scroll: None,
            reports: Vec::new(),
            results,
            done: false,
        }
    }

    /// Construct the current scenario's engine + injector, reset the input box, and
    /// enter the warm-up phase.
    fn begin_scenario(&mut self, now: Instant) {
        let sc = self.scenarios[self.idx].clone();
        log::info!(
            "scenario {}/{}: {} (warmup {}ms, measure {}ms)",
            self.idx + 1,
            self.scenarios.len(),
            sc.name(),
            sc.warmup_ms,
            sc.measure_ms,
        );
        self.engine = build_engine(&sc);
        self.injector = self.engine.as_ref().and_then(Engine::agent_injector);
        self.input = InputModel::new();
        self.samples.clear();
        self.scenario_started = Some(now);
        self.measure_started = None;
        self.script_idx = 0;
        self.next_action_at = None;
        self.current = Some(sc);
    }

    /// Finalize the current scenario into a [`ScenarioReport`].
    fn finalize_scenario(&mut self, measure: Duration) {
        let sc = self
            .current
            .as_ref()
            .expect("finalize with a current scenario");
        let bytes_now = self
            .engine
            .as_ref()
            .map_or(self.bytes_baseline, engine_bytes);
        let facts = aterm_bench::scenario::RunFacts {
            frames_drawn: self.samples.len() as u64,
            bytes_fed: bytes_now.saturating_sub(self.bytes_baseline),
            elapsed_ms: measure.as_secs_f32() * 1000.0,
        };
        let stats = aterm_ui::FrameStats::from_samples(&self.samples);
        let report = ScenarioReport::new(sc, stats, facts, std::mem::take(&mut self.samples));
        log::info!(
            "  {} -> {:?} (frames={}, p99={:.2}ms, dropped={})",
            report.scenario,
            match &report.verdict {
                Verdict::Pass => "pass",
                Verdict::Fail { .. } => "fail",
                Verdict::Inconclusive { .. } => "inconclusive",
            },
            report.facts.frames_drawn,
            report.stats.present_p99_ms,
            report.stats.dropped,
        );
        self.reports.push(report);
    }

    /// Apply one scripted synthetic-input action.
    fn apply_action(&mut self, action: DriverAction) {
        match action {
            DriverAction::Input(input) => {
                if let Some(ev) = map_input(input) {
                    self.input.reduce(ev);
                }
            }
            DriverAction::Scroll(scroll) => {
                self.pending_scroll = Some(map_scroll(scroll));
            }
            DriverAction::Resize { width, height } => {
                if let Some(win) = self.window.as_ref() {
                    let _ = win.request_inner_size(LogicalSize::new(width, height));
                }
            }
            DriverAction::AgentOpen { kind, text } => {
                if let Some(inj) = self.injector.as_ref() {
                    inj.push_block(AgentBlock::new(map_agent_kind(kind), text, Instant::now()));
                }
            }
            DriverAction::AgentToken(delta) => {
                if let Some(inj) = self.injector.as_ref() {
                    inj.append_text(delta);
                }
            }
            // `Idle` is a pure pacing slot: the even action interval already spaces the
            // script across the measured window, so it does nothing here.
            DriverAction::Idle { .. } => {}
        }
    }

    /// Advance to the next scenario, or finish the run (store the report + request
    /// exit).
    fn advance(&mut self) {
        self.engine = None; // teardown: drop the generator subprocess + its threads
        self.injector = None;
        self.scenario_started = None;
        self.measure_started = None;
        self.measuring = false;
        self.want_warm = false;
        self.idx += 1;
        if self.idx >= self.scenarios.len() {
            let run = RunReport::new(std::mem::take(&mut self.reports));
            *self.results.lock().unwrap_or_else(|p| p.into_inner()) = Some(run);
            self.done = true;
        }
    }
}

impl UiCallbacks for DriverCallbacks {
    fn on_ready(&mut self, window: Arc<Window>) {
        self.window = Some(window);
    }

    fn snapshot_version(&mut self) -> u64 {
        self.engine
            .as_ref()
            .map_or(0, |e| e.latest_snapshot().version)
    }

    fn snapshot(&mut self) -> Option<Arc<aterm_core::Snapshot>> {
        self.engine.as_ref().map(Engine::latest_snapshot)
    }

    fn blocks(&mut self) -> Option<Arc<aterm_core::BlockList>> {
        self.engine.as_ref().map(Engine::latest_blocks)
    }

    fn input(&self) -> Option<&InputModel> {
        Some(&self.input)
    }

    fn on_resize(&mut self, cols: u16, rows: u16, width: u32, height: u32) {
        if let Some(e) = self.engine.as_ref() {
            e.resize(rows, cols, width as u16, height as u16);
        }
    }

    fn tick(&mut self) {
        if self.done {
            return;
        }
        let now = Instant::now();
        if self.scenario_started.is_none() {
            self.begin_scenario(now);
        }
        let started = self.scenario_started.expect("scenario started");
        let (warmup, measure, is_idle, script_len) = {
            let sc = self.current.as_ref().expect("current scenario");
            (
                Duration::from_millis(u64::from(sc.warmup_ms)),
                Duration::from_millis(u64::from(sc.measure_ms)),
                sc.gate.idle_max_frames.is_some(),
                sc.script.len(),
            )
        };

        match self.measure_started {
            None => {
                // Warm-up: wait for the output to settle (and, for idle, for the
                // keep-warm window to elapse), then open the measured window.
                if now >= started + warmup {
                    self.measure_started = Some(now);
                    self.bytes_baseline = self.engine.as_ref().map_or(0, engine_bytes);
                    self.samples.clear();
                    self.script_idx = 0;
                    self.action_interval = measure
                        .checked_div(script_len.max(1) as u32)
                        .unwrap_or(measure);
                    self.next_action_at = Some(now);
                    self.measuring = true;
                } else {
                    self.measuring = false;
                }
            }
            Some(mstart) => {
                // Apply every scripted action whose paced time has arrived FIRST, then
                // check the finalize deadline - so an action due just before the window
                // closes is still replayed rather than silently dropped at the boundary.
                while self.script_idx < script_len && self.next_action_at.is_some_and(|t| now >= t)
                {
                    let action =
                        self.current.as_ref().expect("current").script[self.script_idx].clone();
                    self.apply_action(action);
                    self.script_idx += 1;
                    self.next_action_at = self.next_action_at.map(|t| t + self.action_interval);
                }
                if now >= mstart + measure {
                    self.finalize_scenario(measure);
                    self.advance();
                    return;
                }
                self.measuring = true;
            }
        }
        // Keep presenting at vsync through a non-idle scenario; let an idle scenario
        // genuinely idle (the assertion is that it draws ~0 frames).
        self.want_warm = !is_idle && !self.done;
    }

    fn poll_scroll(&mut self) -> Option<ScrollCommand> {
        self.pending_scroll.take()
    }

    fn wants_redraw(&mut self) -> bool {
        self.want_warm
    }

    fn on_frame(&mut self, sample: FrameSample) {
        if self.measuring {
            self.samples.push(sample);
        }
    }

    fn should_exit(&self) -> bool {
        self.done
    }
}

/// Read the engine's total drained-byte counter (the decoupling denominator).
fn engine_bytes(e: &Engine) -> u64 {
    e.metrics().bytes_drained.load(Ordering::Relaxed)
}

/// Build the scenario's output engine: none for `idle`, the generator subprocess for a
/// scenario with a [`Generator`], or a quiet `cat` keep-alive (so agent/no-output
/// scenarios still have an engine for the injector + block publish).
fn build_engine(sc: &Scenario) -> Option<Engine> {
    if sc.gate.idle_max_frames.is_some() {
        return None;
    }
    match sc.setup {
        Some(gen) => spawn_generator(gen, sc.scrollback),
        None => Engine::spawn_command("/bin/cat", &[], INITIAL_DIMS, sc.scrollback)
            .map_err(|e| log::error!("failed to spawn /bin/cat keep-alive: {e}"))
            .ok(),
    }
}

/// Spawn a [`Generator`] as the scenario's PTY child. Generators PRINT their output, so
/// there is no stdin-echo doubling.
fn spawn_generator(gen: Generator, scrollback: usize) -> Option<Engine> {
    let result = match gen {
        Generator::Seq { lines } => {
            let n = lines.to_string();
            Engine::spawn_command("/usr/bin/seq", &[n.as_str()], INITIAL_DIMS, scrollback)
        }
        Generator::Yes { .. } => {
            // `yes` floods until the PTY closes (scenario teardown drops the engine).
            Engine::spawn_command("/usr/bin/yes", &[], INITIAL_DIMS, scrollback)
        }
        Generator::AltScreenRepaint => Engine::spawn_command(
            "/bin/sh",
            &["-c", ALT_SCREEN_SCRIPT],
            INITIAL_DIMS,
            scrollback,
        ),
    };
    result
        .map_err(|e| log::error!("failed to spawn generator {gen:?}: {e}"))
        .ok()
}

/// A POSIX `sh` full-screen alt-screen repaint loop (a vim/htop-style redraw) via
/// `printf` - deterministic program output, no PTY echo. It repaints FOREVER (until the
/// PTY closes at scenario teardown), so full-grid invalidation covers the entire
/// measured window rather than a fixed count that could finish early on a fast host.
const ALT_SCREEN_SCRIPT: &str = "printf '\\033[?1049h'; i=0; while :; do \
     printf '\\033[2J\\033[H'; r=1; while [ $r -le 24 ]; do \
     printf '\\033[%d;1Hrow %d repaint %d ||||||||||||||||||||' $r $r $i; r=$((r+1)); done; \
     i=$((i+1)); done";

/// Map a scenario [`InputAction`] onto the input reducer's [`InputEvent`].
fn map_input(action: InputAction) -> Option<InputEvent> {
    Some(match action {
        InputAction::Type(s) => InputEvent::Insert(s),
        InputAction::Backspace => InputEvent::Backspace,
        InputAction::Left => InputEvent::Move(Motion::Left, false),
        InputAction::Right => InputEvent::Move(Motion::Right, false),
        InputAction::Home => InputEvent::Move(Motion::Home, false),
        InputAction::End => InputEvent::Move(Motion::End, false),
        InputAction::ToggleMode => InputEvent::ToggleMode,
    })
}

/// Map a scenario [`ScrollAction`] onto a UI [`ScrollCommand`].
fn map_scroll(action: ScrollAction) -> ScrollCommand {
    match action {
        ScrollAction::LinesUp(n) => ScrollCommand::ByRows(-(i64::from(n))),
        ScrollAction::LinesDown(n) => ScrollCommand::ByRows(i64::from(n)),
        ScrollAction::PageUp => ScrollCommand::Page(-1),
        ScrollAction::PageDown => ScrollCommand::Page(1),
        ScrollAction::ToTop => ScrollCommand::ToTop,
        ScrollAction::ToBottom => ScrollCommand::ToBottom,
    }
}

/// Map a scenario [`AgentStepKind`] onto the core `AgentBlockKind`.
fn map_agent_kind(kind: AgentStepKind) -> AgentBlockKind {
    match kind {
        AgentStepKind::AssistantText => AgentBlockKind::AssistantText,
        AgentStepKind::Thinking => AgentBlockKind::Thinking,
    }
}
