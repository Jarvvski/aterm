//! The input-latency measure + its pass/fail gate (ticket T-7.3).
//!
//! Frame rate and input latency are *different* metrics: a renderer can hold a solid
//! 60/120fps and still feel laggy, because keystroke->visible-glyph latency is its own
//! pipeline ([`09-performance-60fps.md`] §2.5 + §9; Ghostty's candid 24ms/41ms vs
//! zutty's 7ms data point). So this module gates latency SEPARATELY from the T-7.2
//! frame-time scenarios.
//!
//! Like the T-7.1 recorder and the T-7.2 scenario gate, this is the PURE, headless
//! core: it turns a set of per-keystroke [`LatencySample`]s into a percentile summary
//! ([`LatencyStats`]) and a pass/fail [`LatencyVerdict`] against the
//! [`LatencyGate`], with no window, no GPU, and no clock - so the whole gate is
//! unit-tested on any host. The *live* measure (inject a synthetic keystroke into the
//! real app loop, time the present of the frame that carries the new glyph) is the
//! on-hardware `latency_driver` binary, mirroring how the scenario model is pure while
//! `scenario_driver` is on-hardware.
//!
//! ## The gate (from [`09-performance-60fps.md`] §9)
//!
//! keystroke->glyph **median <= 1.5 frames** and **p99 <= 3 frames** at the active
//! refresh, over the GNOME-46 methodology's ~120 iterations (reported median + p25/p75 +
//! outliers), needing at least [`MIN_ITERATIONS`] samples to be conclusive (below that a
//! headless/no-display run is [`LatencyVerdict::Inconclusive`], never a false pass).
//!
//! A "frame" here is one **present interval**, and it is measured against the run's OWN
//! observed present cadence (the median inter-present interval the driver saw), falling
//! back to the nominal [`Refresh`] deadline only when that observed interval is
//! unavailable. Self-calibrating to the observed cadence (rather than a hardcoded 60/120
//! Hz) is deliberate: the shared CI runner's surface may not present at exactly the
//! nominal refresh, so a hardcoded denominator would silently bias the gate (a slow
//! runner false-fails, a fast one false-passes). It also cleanly divides labour with
//! T-7.2: that gate is the ABSOLUTE frame-time floor ("is every present under 16ms"),
//! while this gate is the keystroke's slip RELATIVE to the steady beat ("does the glyph
//! appear promptly given the cadence").
//!
//! **What each arm means.** Because the measure is present-ordering-based (below), the
//! **median sits at ~1.0 present interval by construction** in a healthy pipeline - so
//! the `median <= 1.5` arm is a **cadence-hold floor / sanity check**, not a tight budget.
//! The real regression signal is the **p99 tail**: when a present genuinely slips a vsync
//! (the render missed its deadline for that keystroke's frame), that keystroke's glyph
//! lands a present interval late and shows up in the tail - `p99 <= 3 intervals` catches a
//! keystroke whose glyph slipped ~2+ extra presents. (Consistent with the frame-budget
//! being a tail-latency problem, §2.4.)
//!
//! ## What the software measure captures (and what it does not)
//!
//! The in-process software path times **model-mutation -> the present of the frame that
//! renders it**: the driver applies the keystroke to the `InputModel` at a frame
//! boundary (so the *current* frame does not carry it) and records the present of the
//! next frame that does. That captures the dominant, controllable part of the pipeline -
//! frame build + GPU encode/submit + the vsync present wait. In particular a render that
//! blows the frame budget makes the next present *slip* a whole vsync (Fifo blocks), and
//! that shows up directly as a ~2-frame measurement - the "fast fps but laggy" regression
//! a frame-rate metric alone would miss.
//!
//! It deliberately does NOT capture: (a) the OS/winit event-dispatch latency *before* the
//! model mutation; (b) the sub-frame phase of a keystroke arriving at a random point
//! within the vsync interval; and (c) a renderer that draws the input a frame *behind*
//! (a hypothetical input double-buffer). (c) is a blind spot because the measure uses
//! frame *ordering* - it assumes the render path draws the current input synchronously in
//! the same frame it reads it, which is true of the architecture today (`redraw` reads
//! `input()` and renders it that frame) but is not *proven* per-frame without pixel
//! read-back. All three are exactly what a keyboard-to-photon hardware rig measures for
//! ground truth - which is why the [`LatencyProbe`] hardware interface exists (stubbed,
//! not required for v1; owner open-question #3: software acceptable for v1). The software
//! number is therefore a *lower bound* on true keystroke->photon latency and a
//! present-scheduling regression detector, not the absolute truth the light-sensor rig
//! would give.
//!
//! [`09-performance-60fps.md`]: ../../../docs/research/09-performance-60fps.md

use aterm_ui::Refresh;
use serde::{Deserialize, Serialize};

/// The median gate: keystroke->glyph median <= 1.5 present intervals
/// ([`09-performance-60fps.md`] §9). A **cadence-hold floor**: the ordering-based measure
/// pins the median at ~1.0 interval in a healthy pipeline, so this arm trips only on a
/// gross systematic slip - the falsifiable regression signal is the p99 tail
/// ([`LATENCY_P99_MAX_FRAMES`]).
pub const LATENCY_MEDIAN_MAX_FRAMES: f32 = 1.5;

/// The blocking tail gate: keystroke->glyph p99 <= 3 present intervals
/// ([`09-performance-60fps.md`] §9). The real signal - a keystroke whose glyph slipped ~2+
/// extra presents (the render missed a vsync for its frame) lands here.
pub const LATENCY_P99_MAX_FRAMES: f32 = 3.0;

/// Minimum conclusive iteration count (the AC floor: report over `>= 100` iterations).
/// Below this the run is [`LatencyVerdict::Inconclusive`] rather than a false pass - the
/// guard that keeps a headless/no-display CI run (which captures ~no keystrokes) from
/// silently "passing" the gate.
pub const MIN_ITERATIONS: usize = 100;

/// The driver's target iteration count (the GNOME-46 methodology: ~120 iterations,
/// comfortably above [`MIN_ITERATIONS`] so a few dropped iterations still leave a
/// conclusive run).
pub const TARGET_ITERATIONS: u32 = 120;

/// One keystroke->glyph measurement: the wall-clock delay from applying the keystroke to
/// the present of the frame that first renders the new glyph.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatencySample {
    /// 0-based iteration index (measurement order).
    pub iteration: u32,
    /// Measured keystroke->glyph latency (ms).
    pub press_to_glyph_ms: f32,
}

/// The percentile summary of a latency run, in BOTH milliseconds and present-interval
/// equivalents (the gate is stated in intervals; the raw ms are kept for the
/// histogram/offline analysis). Reports the GNOME-46 quartet - median + p25/p75 +
/// `outliers` - alongside the p99 tail and the min/max envelope. `Deserialize` so the
/// whole report round-trips as a typed struct, not just via `serde_json::Value`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatencyStats {
    /// Samples summarized.
    pub count: usize,
    /// The nominal refresh (the fallback denominator + what cadence the run targeted).
    pub refresh: Refresh,
    /// The present-interval (ms) the frame-equivalents are divided by: the run's OBSERVED
    /// median inter-present interval when supplied, else `refresh.deadline_ms()`. Recorded
    /// so a reader can see which denominator the gate actually used.
    pub interval_ms: f32,
    pub min_ms: f32,
    pub p25_ms: f32,
    pub median_ms: f32,
    pub p75_ms: f32,
    pub p99_ms: f32,
    pub max_ms: f32,
    /// `median_ms` expressed in present intervals (the gated quantity; ~1.0 in a healthy
    /// pipeline - a cadence-hold floor).
    pub median_frames: f32,
    /// `p99_ms` expressed in present intervals (the gated tail quantity - the real signal).
    pub p99_frames: f32,
    /// Count of upper outliers: samples above the Tukey fence `p75 + 1.5 * IQR`
    /// (`IQR = p75 - p25`). The slow-keystroke tail the box-plot methodology flags.
    pub outliers: usize,
}

impl LatencyStats {
    /// Summarize a slice of latency samples. `observed_interval_ms` is the run's measured
    /// median inter-present interval, used as the frame-equivalent denominator (so the
    /// gate self-calibrates to the actual present cadence); `None` (or a non-finite /
    /// non-positive value) falls back to `refresh.deadline_ms()`. Non-finite samples are
    /// dropped, so a garbage timing can never poison the percentiles or the JSON
    /// round-trip (serde emits `null` for a non-finite f32, which the non-`Option` fields
    /// reject on parse). Empty input yields an all-zero summary (which the gate treats as
    /// [`LatencyVerdict::Inconclusive`] via the [`MIN_ITERATIONS`] floor, not a pass).
    #[must_use]
    pub fn from_samples(
        samples: &[LatencySample],
        refresh: Refresh,
        observed_interval_ms: Option<f32>,
    ) -> Self {
        // The frame denominator: the observed present cadence, else the nominal refresh.
        let interval_ms = observed_interval_ms
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or_else(|| refresh.deadline_ms())
            .max(f32::MIN_POSITIVE);
        // Drop non-finite samples so a NaN/Inf can never pollute the percentiles or break
        // the JSON round-trip. Finite live timings are the norm; this hardens the public
        // sample surface (and a future hardware probe) against garbage.
        let mut ms: Vec<f32> = samples
            .iter()
            .map(|s| s.press_to_glyph_ms)
            .filter(|v| v.is_finite())
            .collect();
        let count = ms.len();
        if count == 0 {
            return Self {
                count: 0,
                refresh,
                interval_ms,
                min_ms: 0.0,
                p25_ms: 0.0,
                median_ms: 0.0,
                p75_ms: 0.0,
                p99_ms: 0.0,
                max_ms: 0.0,
                median_frames: 0.0,
                p99_frames: 0.0,
                outliers: 0,
            };
        }
        ms.sort_by(f32::total_cmp);
        let p25 = percentile(&ms, 25.0);
        let median = percentile(&ms, 50.0);
        let p75 = percentile(&ms, 75.0);
        let p99 = percentile(&ms, 99.0);
        // Tukey upper fence for the slow-outlier count (box-plot whisker).
        let iqr = p75 - p25;
        let upper_fence = p75 + 1.5 * iqr;
        let outliers = ms.iter().filter(|&&v| v > upper_fence).count();
        Self {
            count,
            refresh,
            interval_ms,
            min_ms: ms[0],
            p25_ms: p25,
            median_ms: median,
            p75_ms: p75,
            p99_ms: p99,
            max_ms: *ms.last().unwrap(),
            median_frames: median / interval_ms,
            p99_frames: p99 / interval_ms,
            outliers,
        }
    }
}

/// The declared keystroke->glyph pass/fail thresholds ([`09-performance-60fps.md`] §9),
/// in frames at the active refresh so the same numbers hold at 60Hz and 120Hz.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatencyGate {
    /// The active refresh the frame thresholds are judged at.
    pub refresh: Refresh,
    /// Max allowed median latency (frames): [`LATENCY_MEDIAN_MAX_FRAMES`].
    pub median_max_frames: f32,
    /// Max allowed p99 latency (frames): [`LATENCY_P99_MAX_FRAMES`].
    pub p99_max_frames: f32,
    /// Minimum conclusive iteration count: [`MIN_ITERATIONS`].
    pub min_iterations: usize,
}

impl LatencyGate {
    /// The blocking latency gate at `refresh`: median <= 1.5 frames, p99 <= 3 frames,
    /// over at least [`MIN_ITERATIONS`] iterations.
    #[must_use]
    pub fn frames_gate(refresh: Refresh) -> Self {
        Self {
            refresh,
            median_max_frames: LATENCY_MEDIAN_MAX_FRAMES,
            p99_max_frames: LATENCY_P99_MAX_FRAMES,
            min_iterations: MIN_ITERATIONS,
        }
    }

    /// Evaluate this gate against a run's percentile summary -> a [`LatencyVerdict`].
    /// Pure: the whole pass/fail decision, exhaustively tested. Too few iterations is
    /// [`LatencyVerdict::Inconclusive`] (a headless run), never a pass.
    #[must_use]
    pub fn evaluate(&self, stats: &LatencyStats) -> LatencyVerdict {
        if stats.count < self.min_iterations {
            return LatencyVerdict::Inconclusive {
                iterations: stats.count,
                required: self.min_iterations,
            };
        }
        let mut breaches = Vec::new();
        if stats.median_frames > self.median_max_frames {
            breaches.push(LatencyBreach::MedianFrames {
                observed: stats.median_frames,
                limit: self.median_max_frames,
            });
        }
        if stats.p99_frames > self.p99_max_frames {
            breaches.push(LatencyBreach::P99Frames {
                observed: stats.p99_frames,
                limit: self.p99_max_frames,
            });
        }
        if breaches.is_empty() {
            LatencyVerdict::Pass
        } else {
            LatencyVerdict::Fail { breaches }
        }
    }
}

/// One breached latency threshold, with the observed value + the limit it exceeded (both
/// in frames) - so a CI failure reads "median 1.8 frames > 1.5", not just "failed".
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LatencyBreach {
    /// The median keystroke->glyph latency exceeded [`LatencyGate::median_max_frames`].
    MedianFrames { observed: f32, limit: f32 },
    /// The p99 keystroke->glyph latency exceeded [`LatencyGate::p99_max_frames`].
    P99Frames { observed: f32, limit: f32 },
}

/// The latency gate outcome. `Inconclusive` (too few iterations - a headless/no-display
/// run) is distinct from both pass and fail, exactly like [`crate::scenario::Verdict`],
/// so the driver treats it as a loud non-fatal skip rather than a green pass or a red
/// breach.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum LatencyVerdict {
    Pass,
    // Struct variant (not `Fail(Vec<..>)`): an internally-tagged (`tag = "result"`) enum
    // cannot serialize a tuple/newtype variant wrapping a sequence, so the breaches live
    // in a named field (same shape as `scenario::Verdict`).
    Fail { breaches: Vec<LatencyBreach> },
    Inconclusive { iterations: usize, required: usize },
}

impl LatencyVerdict {
    /// Whether this verdict is a hard failure (the only outcome that blocks release).
    #[must_use]
    pub fn is_fail(&self) -> bool {
        matches!(self, LatencyVerdict::Fail { .. })
    }

    /// Whether this verdict is a clean pass.
    #[must_use]
    pub fn is_pass(&self) -> bool {
        matches!(self, LatencyVerdict::Pass)
    }
}

/// A full latency run's report: the percentile summary, the blocking verdict, and the
/// raw samples for offline histogram/box-plot analysis. Serializes to the JSON the
/// `latency_driver` dumps (and CI reads); `Deserialize` too, so an analysis step can read
/// it back as a typed struct rather than poking at `serde_json::Value`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyReport {
    pub refresh: Refresh,
    pub stats: LatencyStats,
    pub verdict: LatencyVerdict,
    /// Raw per-keystroke samples (measurement order) - the histogram/box-plot input.
    pub samples: Vec<LatencySample>,
}

impl LatencyReport {
    /// Assemble a report from a gate + the run's samples + the run's observed median
    /// present interval (the self-calibrating frame denominator; `None` falls back to the
    /// gate's nominal refresh - see [`LatencyStats::from_samples`]). Computes the summary +
    /// the verdict; `overall_pass` for a run is simply `!verdict.is_fail()`.
    #[must_use]
    pub fn new(
        gate: &LatencyGate,
        samples: Vec<LatencySample>,
        observed_interval_ms: Option<f32>,
    ) -> Self {
        // Drop any non-finite sample up front so the STORED samples match the summarized
        // set AND the whole report round-trips (serde emits `null` for a non-finite f32,
        // which the sample's non-Option field would reject on parse). Live timings are
        // always finite; this hardens against a future probe emitting garbage.
        let mut samples = samples;
        samples.retain(|s| s.press_to_glyph_ms.is_finite());
        let stats = LatencyStats::from_samples(&samples, gate.refresh, observed_interval_ms);
        let verdict = gate.evaluate(&stats);
        Self {
            refresh: gate.refresh,
            stats,
            verdict,
            samples,
        }
    }

    /// True iff the run did not hard-fail (a pass or an inconclusive run - the latter
    /// never blocks, mirroring the scenario gate).
    #[must_use]
    pub fn overall_pass(&self) -> bool {
        !self.verdict.is_fail()
    }

    /// Serialize to pretty JSON for the dump artifact.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }
}

// --- Hardware rig interface (AC3: stubbed + documented, NOT required for v1) ----------

/// Which latency-measurement path produced a run - recorded in the report and used to
/// pick a probe. The software path is the in-process `latency_driver`; the hardware path
/// is the keyboard-to-photon rig this module only stubs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeKind {
    /// The in-process software measure (model-mutation -> frame present).
    Software,
    /// A hardware keyboard-to-photon rig (light sensor + a microcontroller injecting the
    /// keypress) - the GNOME-46 ground-truth methodology. Stubbed in v1.
    HardwareRig,
}

/// A source of keystroke->photon latency measurements. The software driver implements
/// the live measure directly; this trait is the seam a hardware rig plugs into for
/// ground-truth keyboard-to-photon timing without changing the gate or the report shape.
///
/// A rig (per [`09-performance-60fps.md`] §9, refs [13][14]): a light sensor taped to the
/// screen over the input caret + a microcontroller (e.g. a Teensy) that both presses a
/// key and timestamps the photon the sensor sees. That captures the FULL path - the OS
/// event dispatch and the sub-frame arrival phase the software measure cannot - which is
/// why it is the ground truth. It is out of scope to BUILD for v1 (owner
/// open-question #3: software acceptable), so v1 ships only [`HardwareRig`], a documented
/// stub that reports itself unavailable and yields no samples.
pub trait LatencyProbe {
    /// Which measurement path this probe represents.
    fn kind(&self) -> ProbeKind;

    /// Whether this probe can actually take a measurement in the current environment.
    /// The software driver is available wherever it can present; the hardware stub is
    /// never available in v1.
    fn is_available(&self) -> bool;

    /// Take one keystroke->photon measurement (ms), or `None` if unavailable. The
    /// hardware stub always returns `None`.
    fn measure_once(&mut self) -> Option<f32>;
}

/// The stubbed hardware keyboard-to-photon rig (AC3). Documents the ground-truth
/// interface described above but is deliberately inert in v1: [`Self::is_available`] is
/// `false` and [`Self::measure_once`] yields `None`, so a run that (mis)selected the
/// hardware path produces zero samples and an honest [`LatencyVerdict::Inconclusive`]
/// rather than pretending to measure. Wiring a real sensor is a future ticket.
#[derive(Debug, Default, Clone, Copy)]
pub struct HardwareRig;

impl LatencyProbe for HardwareRig {
    fn kind(&self) -> ProbeKind {
        ProbeKind::HardwareRig
    }

    fn is_available(&self) -> bool {
        false
    }

    fn measure_once(&mut self) -> Option<f32> {
        None
    }
}

/// Nearest-rank percentile over an already-sorted, non-empty slice. `p` in
/// `0.0..=100.0`. Rank = `ceil(p/100 * n)`, clamped to `1..=n`, 1-based. Matches the
/// T-7.1 recorder's percentile so the two harnesses agree.
fn percentile(sorted: &[f32], p: f32) -> f32 {
    debug_assert!(!sorted.is_empty());
    let n = sorted.len();
    let rank = (p / 100.0 * n as f32).ceil() as usize;
    let idx = rank.clamp(1, n) - 1;
    sorted[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build `n` samples each with the given latency (ms).
    fn flat(n: u32, ms: f32) -> Vec<LatencySample> {
        (0..n)
            .map(|i| LatencySample {
                iteration: i,
                press_to_glyph_ms: ms,
            })
            .collect()
    }

    /// Build samples from an explicit ms list.
    fn from_ms(list: &[f32]) -> Vec<LatencySample> {
        list.iter()
            .enumerate()
            .map(|(i, &ms)| LatencySample {
                iteration: i as u32,
                press_to_glyph_ms: ms,
            })
            .collect()
    }

    #[test]
    fn stats_compute_quartiles_median_p99_and_frames() {
        // 1..=100 ms. nearest-rank: p25 -> rank ceil(25)=25 -> the 25th value = 25.
        // median -> rank 50 -> 50; p75 -> 75; p99 -> rank ceil(99)=99 -> 99; max 100.
        let ms: Vec<f32> = (1..=100).map(|v| v as f32).collect();
        let s = LatencyStats::from_samples(&from_ms(&ms), Refresh::Hz60, None);
        assert_eq!(s.count, 100);
        assert!((s.p25_ms - 25.0).abs() < 1e-6);
        assert!((s.median_ms - 50.0).abs() < 1e-6);
        assert!((s.p75_ms - 75.0).abs() < 1e-6);
        assert!((s.p99_ms - 99.0).abs() < 1e-6);
        assert!((s.min_ms - 1.0).abs() < 1e-6);
        assert!((s.max_ms - 100.0).abs() < 1e-6);
        // With no observed interval, the denominator is the nominal 60Hz deadline.
        assert!((s.interval_ms - Refresh::Hz60.deadline_ms()).abs() < 1e-4);
        assert!((s.median_frames - 50.0 / Refresh::Hz60.deadline_ms()).abs() < 1e-4);
        assert!((s.p99_frames - 99.0 / Refresh::Hz60.deadline_ms()).abs() < 1e-4);
    }

    #[test]
    fn frame_equivalents_fall_back_to_the_nominal_refresh() {
        // With no observed cadence, the SAME 25ms median is 1.5 frames @60Hz but 3.0
        // frames @120Hz (the nominal-refresh fallback denominator).
        let ms = flat(120, 25.0);
        let at60 = LatencyStats::from_samples(&ms, Refresh::Hz60, None);
        let at120 = LatencyStats::from_samples(&ms, Refresh::Hz120, None);
        assert!((at60.median_frames - 1.5).abs() < 1e-3, "25ms = 1.5f @60Hz");
        assert!(
            (at120.median_frames - 3.0).abs() < 1e-3,
            "25ms = 3.0f @120Hz"
        );
    }

    #[test]
    fn observed_interval_self_calibrates_the_frame_denominator() {
        // The fix for the hardcoded-refresh miscalibration: frames divide by the run's
        // OWN observed present interval when supplied, not the nominal refresh. The same
        // 25ms samples are ~1.0 interval at a 25ms cadence but ~2.0 at a 12.5ms cadence -
        // regardless of the nominal refresh passed.
        let ms = flat(120, 25.0);
        let at_cadence = LatencyStats::from_samples(&ms, Refresh::Hz60, Some(25.0));
        assert!((at_cadence.interval_ms - 25.0).abs() < 1e-4);
        assert!(
            (at_cadence.median_frames - 1.0).abs() < 1e-3,
            "25ms latency at a 25ms cadence is ~1.0 interval, got {}",
            at_cadence.median_frames
        );
        let fast_cadence = LatencyStats::from_samples(&ms, Refresh::Hz60, Some(12.5));
        assert!(
            (fast_cadence.median_frames - 2.0).abs() < 1e-3,
            "25ms latency at a 12.5ms cadence is ~2.0 intervals"
        );
        // A non-finite / non-positive observed interval falls back to the nominal refresh.
        let bad = LatencyStats::from_samples(&ms, Refresh::Hz60, Some(0.0));
        assert!((bad.interval_ms - Refresh::Hz60.deadline_ms()).abs() < 1e-4);
        let nan = LatencyStats::from_samples(&ms, Refresh::Hz60, Some(f32::NAN));
        assert!((nan.interval_ms - Refresh::Hz60.deadline_ms()).abs() < 1e-4);
    }

    #[test]
    fn outliers_use_the_tukey_upper_fence() {
        // p25=25, p75=75 over 1..=100 -> IQR 50, fence 75 + 75 = 150. Nothing over 100
        // exceeds it -> zero outliers in the uniform set.
        let uniform: Vec<f32> = (1..=100).map(|v| v as f32).collect();
        assert_eq!(
            LatencyStats::from_samples(&from_ms(&uniform), Refresh::Hz60, None).outliers,
            0
        );
        // Add a few far-out spikes well past the fence -> counted as outliers.
        let mut spiky = uniform.clone();
        spiky.extend_from_slice(&[400.0, 500.0, 600.0]);
        let s = LatencyStats::from_samples(&from_ms(&spiky), Refresh::Hz60, None);
        assert!(
            s.outliers >= 3,
            "the three 400ms+ spikes are outliers: {s:?}"
        );
    }

    #[test]
    fn non_finite_samples_are_dropped_and_report_round_trips_typed() {
        // M2: a NaN/Inf sample is dropped (never poisons the percentiles) AND the report
        // still round-trips - serde emits `null` for a non-finite f32, which the
        // non-Option fields reject on parse, so a leaked non-finite would break the dump.
        let mut list = flat(102, 16.0);
        list[10].press_to_glyph_ms = f32::NAN;
        list[20].press_to_glyph_ms = f32::INFINITY;
        let gate = LatencyGate::frames_gate(Refresh::Hz60);
        let report = LatencyReport::new(&gate, list, Some(16.0));
        assert_eq!(
            report.stats.count, 100,
            "the 2 non-finite samples were dropped"
        );
        assert!(report.stats.median_ms.is_finite() && report.stats.median_frames.is_finite());
        // L1: the whole report deserializes back into a typed struct (not just Value).
        let json = report.to_json();
        let back: LatencyReport =
            serde_json::from_str(&json).expect("report must round-trip as a typed struct");
        assert_eq!(back.stats, report.stats);
        assert_eq!(back.verdict, report.verdict);
    }

    #[test]
    fn clean_run_passes_the_frames_gate() {
        // ~1.1 intervals median (18ms at a 16.67ms cadence), tail 30ms (~1.8) - healthy.
        let mut list = vec![18.0_f32; 118];
        list.push(30.0);
        list.push(30.0);
        let gate = LatencyGate::frames_gate(Refresh::Hz60);
        let v = gate.evaluate(&LatencyStats::from_samples(
            &from_ms(&list),
            Refresh::Hz60,
            None,
        ));
        assert!(v.is_pass(), "a ~1.1-interval median run passes: {v:?}");
    }

    #[test]
    fn median_over_one_and_a_half_intervals_fails() {
        // The cadence-hold floor: a gross systematic slip (median 30ms at a 16.67ms
        // cadence = 1.8 intervals) trips the median arm.
        let gate = LatencyGate::frames_gate(Refresh::Hz60);
        let v = gate.evaluate(&LatencyStats::from_samples(
            &flat(120, 30.0),
            Refresh::Hz60,
            None,
        ));
        assert!(v.is_fail());
        let LatencyVerdict::Fail { breaches } = &v else {
            panic!("expected fail, got {v:?}")
        };
        assert!(
            breaches.iter().any(|b| matches!(
                b,
                LatencyBreach::MedianFrames { limit, .. } if (*limit - 1.5).abs() < 1e-6
            )),
            "the median breach names the 1.5-interval limit: {breaches:?}"
        );
    }

    #[test]
    fn p99_over_three_intervals_fails_even_with_a_fast_median() {
        // The real tail signal: a fast median (16ms ~1 interval) but 5% of keystrokes
        // slip to 60ms (~3.6 intervals at a 16.67ms cadence) pushes p99 over 3 -> fail on
        // the tail alone (a keystroke whose glyph slipped ~2+ extra presents).
        let mut list = vec![16.0_f32; 114];
        list.extend_from_slice(&[60.0; 6]);
        let gate = LatencyGate::frames_gate(Refresh::Hz60);
        let v = gate.evaluate(&LatencyStats::from_samples(
            &from_ms(&list),
            Refresh::Hz60,
            None,
        ));
        assert!(v.is_fail());
        assert!(
            matches!(&v, LatencyVerdict::Fail { breaches } if breaches.iter().any(|b| matches!(b, LatencyBreach::P99Frames { .. }))),
            "a heavy tail is a P99Frames breach: {v:?}"
        );
    }

    #[test]
    fn too_few_iterations_is_inconclusive_not_a_pass() {
        // The false-pass guard: < MIN_ITERATIONS samples (a headless run captured ~none)
        // is inconclusive, never a pass - even if the few it has are blazing fast.
        let gate = LatencyGate::frames_gate(Refresh::Hz120);
        let v = gate.evaluate(&LatencyStats::from_samples(
            &flat(10, 1.0),
            Refresh::Hz120,
            None,
        ));
        assert!(
            matches!(v, LatencyVerdict::Inconclusive { iterations: 10, required } if required == MIN_ITERATIONS),
            "10 samples is inconclusive: {v:?}"
        );
        assert!(!v.is_pass() && !v.is_fail());
        // Exactly MIN_ITERATIONS fast samples IS conclusive (and passes).
        let ok = gate.evaluate(&LatencyStats::from_samples(
            &flat(MIN_ITERATIONS as u32, 4.0),
            Refresh::Hz120,
            None,
        ));
        assert!(
            ok.is_pass(),
            "100 fast samples is a conclusive pass: {ok:?}"
        );
    }

    #[test]
    fn report_json_is_valid_and_carries_the_verdict_and_samples() {
        // AC: the run dumps valid JSON consumable by an analysis step. A failing run
        // (median 30ms at a 16.67ms cadence = 1.8) -> parse -> verdict + round-tripped
        // sample.
        let gate = LatencyGate::frames_gate(Refresh::Hz60);
        let report = LatencyReport::new(&gate, flat(120, 30.0), None);
        assert!(!report.overall_pass(), "1.8-interval median fails");
        let json = report.to_json();
        let v: serde_json::Value = serde_json::from_str(&json).expect("latency JSON must parse");
        assert_eq!(v["verdict"]["result"], "fail");
        assert_eq!(v["stats"]["count"], 120);
        // A sample round-trips (LatencySample is Deserialize) - the histogram input.
        let parsed: LatencySample =
            serde_json::from_value(v["samples"][0].clone()).expect("sample must round-trip");
        assert_eq!(parsed.press_to_glyph_ms, 30.0);
    }

    #[test]
    fn hardware_rig_is_a_documented_unavailable_stub() {
        // AC3: the hardware-rig interface is stubbed - present as a type, honest about
        // being unavailable in v1, and yielding no samples (never a fake measurement).
        let mut rig = HardwareRig;
        assert_eq!(rig.kind(), ProbeKind::HardwareRig);
        assert!(!rig.is_available(), "the v1 stub is not available");
        assert_eq!(rig.measure_once(), None, "the stub yields no measurement");
    }
}
