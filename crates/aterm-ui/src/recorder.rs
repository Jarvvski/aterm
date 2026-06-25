//! Tier-2 in-process frame recorder (ticket T-7.1).
//!
//! The instrumentation the "60fps always" proof reads ([`09-performance-60fps.md`]
//! §9, Recommendation 7). A fixed-capacity ring buffer of per-frame samples,
//! recorded ALLOCATION-FREE on the hot path: the backing storage is pre-sized at
//! construction and [`FrameRecorder::record`] overwrites in place, so the recording
//! path never touches the allocator (the steady-state property the 60fps floor
//! itself depends on - asserted in the tests via the crate alloc probe).
//!
//! [`FrameRecorder::to_json`] dumps the captured window for offline
//! percentile/histogram analysis; [`FrameStats::from_samples`] computes
//! p50/p99/max + the dropped-frame count in-process for the same data.
//!
//! Pure and OS/GPU-free in the spirit of [`crate::present`]: the deadline math is
//! driven by caller-supplied millisecond timings, so the whole module is
//! deterministic under test with no window, display, or GPU.
//!
//! ## GPU frame time (AC4)
//!
//! [`FrameSample::gpu_frame_ms`] is an `Option` and is `None` today: the wgpu
//! device is created without [`wgpu::Features::TIMESTAMP_QUERY`] (see
//! [`crate::gpu`]), so real GPU-side timing needs a timestamp query set + a resolve
//! pass that this crate does not yet wire. That limitation is documented per the
//! ticket ("GPU frame time is captured, OR the limitation is documented with the
//! fallback"); the recorder is shaped to carry the value the moment the query path
//! lands. The fallback for GPU timing today is Instruments / Metal System Trace.
//!
//! [`09-performance-60fps.md`]: ../../../docs/research/09-performance-60fps.md

use serde::{Deserialize, Serialize};

/// Target display cadence. Sets the per-frame present deadline a frame is judged
/// dropped against. The 60Hz floor is the hard gate; 120Hz is informational
/// ([`09-performance-60fps.md`] §9, owner open-question #4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refresh {
    /// 60 Hz: a 16.67 ms frame deadline (the hard floor).
    Hz60,
    /// 120 Hz ProMotion: an 8.33 ms frame deadline (tracked, not blocking).
    Hz120,
}

impl Refresh {
    /// Nominal per-frame deadline in milliseconds (`1000 / Hz`): 16.67 ms @ 60Hz,
    /// 8.33 ms @ 120Hz.
    #[must_use]
    pub fn deadline_ms(self) -> f32 {
        match self {
            Refresh::Hz60 => 1000.0 / 60.0,
            Refresh::Hz120 => 1000.0 / 120.0,
        }
    }
}

/// Default capacity for [`FrameRecorder::new`]'s ring: ~17 s @ 120Hz / ~34 s @
/// 60Hz, enough to cover a scripted stress scenario (T-7.2) without growing.
pub const DEFAULT_CAPACITY: usize = 2048;

/// Default drop tolerance (ms) added to the refresh deadline before a frame counts
/// as dropped, absorbing sub-millisecond scheduling jitter near the vsync boundary
/// so only a genuinely missed present (interval ~= 2x the deadline) is flagged.
pub const DEFAULT_TOLERANCE_MS: f32 = 2.0;

/// The raw per-frame timings the present loop measures and hands to the recorder.
/// `frame_dropped` is NOT supplied here - the recorder DERIVES it from
/// `present_interval_ms` against the configured [`Refresh`] deadline + tolerance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameTiming {
    /// CPU frame-build time (snapshot -> encode/submit on the render thread), ms.
    pub cpu_frame_ms: f32,
    /// GPU frame time, ms; `None` when GPU timestamps are unavailable (see module
    /// docs - the device requests no `TIMESTAMP_QUERY` feature today).
    pub gpu_frame_ms: Option<f32>,
    /// Interval since the previous present (vsync-to-vsync delta), ms.
    pub present_interval_ms: f32,
    /// Cells the renderer touched this frame (damage extent).
    pub dirty_cells: u32,
    /// Heap allocations during the frame build (debug instrumentation); `None` when
    /// not measured (release / no alloc probe armed).
    pub allocations: Option<u32>,
}

/// One recorded frame. `frame` is a monotonic index from the recorder; everything
/// else mirrors the [`FrameTiming`] it was built from, plus the derived
/// `frame_dropped`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct FrameSample {
    /// Monotonic frame index since the recorder was created.
    pub frame: u64,
    pub cpu_frame_ms: f32,
    pub gpu_frame_ms: Option<f32>,
    pub present_interval_ms: f32,
    pub dirty_cells: u32,
    pub allocations: Option<u32>,
    /// `present_interval_ms` exceeded the refresh deadline + tolerance (a missed
    /// present).
    pub frame_dropped: bool,
}

/// Fixed-capacity ring buffer of [`FrameSample`]s. The backing `Vec` is pre-sized
/// to `capacity` at construction; [`Self::record`] writes by index and never
/// allocates. When full it overwrites the oldest sample.
#[derive(Debug, Clone)]
pub struct FrameRecorder {
    /// Always `len == capacity`; used as a ring indexed by `head`/`len_valid`.
    samples: Vec<FrameSample>,
    /// Next write index.
    head: usize,
    /// Number of valid samples (`<= capacity`).
    len_valid: usize,
    /// Monotonic frame counter (total recorded, not the ring length).
    frame: u64,
    refresh: Refresh,
    tolerance_ms: f32,
}

impl FrameRecorder {
    /// Create a recorder with an explicit ring `capacity` (clamped to at least 1)
    /// and target [`Refresh`]. The single backing allocation happens here, NOT on
    /// the record path.
    #[must_use]
    pub fn new(capacity: usize, refresh: Refresh) -> Self {
        let capacity = capacity.max(1);
        Self {
            samples: vec![FrameSample::default(); capacity],
            head: 0,
            len_valid: 0,
            frame: 0,
            refresh,
            tolerance_ms: DEFAULT_TOLERANCE_MS,
        }
    }

    /// Builder: override the drop tolerance (ms) added to the deadline.
    #[must_use]
    pub fn with_tolerance_ms(mut self, tolerance_ms: f32) -> Self {
        self.tolerance_ms = tolerance_ms;
        self
    }

    /// Record one frame's timings. Sanitizes non-finite inputs (see
    /// [`finite_or_zero`]), computes `frame_dropped` (interval > deadline +
    /// tolerance), assigns the monotonic frame index, and stores the sample by
    /// index - ALLOCATION-FREE. Returns the stored [`FrameSample`] for convenience.
    pub fn record(&mut self, timing: FrameTiming) -> FrameSample {
        let deadline = self.refresh.deadline_ms() + self.tolerance_ms;
        // Sanitize non-finite (NaN/Inf) timings to 0.0 BEFORE storing: serde_json
        // serializes a non-finite f32 as JSON `null` (it does NOT error), which the
        // non-Option f32 fields then reject on parse - silently breaking the JSON
        // round-trip the analysis relies on. Live timings (Instant deltas) are
        // always finite, so this only hardens the public `FrameTiming` surface
        // against garbage; a non-finite interval becomes 0.0 (benign, not a counted
        // drop) rather than polluting the dropped-frame signal.
        let cpu_frame_ms = finite_or_zero(timing.cpu_frame_ms);
        let present_interval_ms = finite_or_zero(timing.present_interval_ms);
        let gpu_frame_ms = timing.gpu_frame_ms.map(finite_or_zero);
        let sample = FrameSample {
            frame: self.frame,
            cpu_frame_ms,
            gpu_frame_ms,
            present_interval_ms,
            dirty_cells: timing.dirty_cells,
            allocations: timing.allocations,
            frame_dropped: present_interval_ms > deadline,
        };
        let cap = self.samples.len();
        self.samples[self.head] = sample;
        self.head = (self.head + 1) % cap;
        self.len_valid = (self.len_valid + 1).min(cap);
        self.frame += 1;
        sample
    }

    /// The ring's capacity (the pre-sized backing length).
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.samples.len()
    }

    /// Number of valid samples currently held (`<= capacity`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.len_valid
    }

    /// Whether no frame has been recorded yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len_valid == 0
    }

    /// Total frames recorded since construction (may exceed `capacity`; the ring
    /// only retains the most recent `capacity`).
    #[must_use]
    pub fn frames_recorded(&self) -> u64 {
        self.frame
    }

    /// The held samples, oldest first. Allocates - call off the hot path (dump /
    /// analysis time), never per frame.
    #[must_use]
    pub fn samples(&self) -> Vec<FrameSample> {
        let cap = self.samples.len();
        if self.len_valid < cap {
            // Not yet wrapped: valid samples are `[0, len_valid)` in order.
            self.samples[..self.len_valid].to_vec()
        } else {
            // Wrapped: oldest is at `head`, newest at `head - 1`.
            let mut out = Vec::with_capacity(cap);
            out.extend_from_slice(&self.samples[self.head..]);
            out.extend_from_slice(&self.samples[..self.head]);
            out
        }
    }

    /// Dump the held samples (oldest first) as a JSON array, consumable by an
    /// offline analysis script computing p50/p99/max/dropped (or by
    /// [`FrameStats::from_samples`] in-process). Off the hot path. All stored
    /// timings are finite (sanitized at [`Self::record`]), so the output is always
    /// valid, re-parseable JSON; the empty-array fallback is a belt-and-suspenders
    /// that is not reachable in practice.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.samples()).unwrap_or_else(|_| "[]".to_string())
    }

    /// Compute the percentile summary over the currently held samples.
    #[must_use]
    pub fn stats(&self) -> FrameStats {
        FrameStats::from_samples(&self.samples())
    }
}

/// Percentile summary over a window of [`FrameSample`]s - the "60fps always" proof
/// reads `present_*`/`dropped`; `cpu_*` tracks frame-build headroom.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct FrameStats {
    /// Samples summarized.
    pub count: usize,
    pub cpu_p50_ms: f32,
    pub cpu_p99_ms: f32,
    pub cpu_max_ms: f32,
    pub present_p50_ms: f32,
    pub present_p99_ms: f32,
    pub present_max_ms: f32,
    /// Frames flagged `frame_dropped`.
    pub dropped: usize,
    /// `dropped` as a percentage of `count` (0.0 when `count == 0`).
    pub dropped_pct: f32,
}

impl FrameStats {
    /// Summarize a slice of samples. Empty input yields an all-zero summary.
    #[must_use]
    pub fn from_samples(samples: &[FrameSample]) -> Self {
        let count = samples.len();
        if count == 0 {
            return Self {
                count: 0,
                cpu_p50_ms: 0.0,
                cpu_p99_ms: 0.0,
                cpu_max_ms: 0.0,
                present_p50_ms: 0.0,
                present_p99_ms: 0.0,
                present_max_ms: 0.0,
                dropped: 0,
                dropped_pct: 0.0,
            };
        }
        let mut cpu: Vec<f32> = samples.iter().map(|s| s.cpu_frame_ms).collect();
        let mut present: Vec<f32> = samples.iter().map(|s| s.present_interval_ms).collect();
        cpu.sort_by(f32::total_cmp);
        present.sort_by(f32::total_cmp);
        let dropped = samples.iter().filter(|s| s.frame_dropped).count();
        Self {
            count,
            cpu_p50_ms: percentile(&cpu, 50.0),
            cpu_p99_ms: percentile(&cpu, 99.0),
            cpu_max_ms: *cpu.last().unwrap(),
            present_p50_ms: percentile(&present, 50.0),
            present_p99_ms: percentile(&present, 99.0),
            present_max_ms: *present.last().unwrap(),
            dropped,
            dropped_pct: (dropped as f32 / count as f32) * 100.0,
        }
    }
}

/// Replace a non-finite (NaN/Inf) timing with `0.0`. serde_json emits JSON `null`
/// for a non-finite `f32`, which the non-`Option` sample fields reject on parse, so
/// sanitizing at record time keeps every dump valid, re-ingestible JSON.
fn finite_or_zero(ms: f32) -> f32 {
    if ms.is_finite() {
        ms
    } else {
        0.0
    }
}

/// Nearest-rank percentile over an already-sorted, non-empty slice. `p` in
/// `0.0..=100.0`. Rank = `ceil(p/100 * n)`, clamped to `1..=n`, 1-based.
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

    fn timing(cpu_ms: f32, interval_ms: f32) -> FrameTiming {
        FrameTiming {
            cpu_frame_ms: cpu_ms,
            gpu_frame_ms: None,
            present_interval_ms: interval_ms,
            dirty_cells: 0,
            allocations: None,
        }
    }

    #[test]
    fn refresh_deadlines_match_the_two_cadences() {
        assert!((Refresh::Hz60.deadline_ms() - 16.6667).abs() < 1e-3);
        assert!((Refresh::Hz120.deadline_ms() - 8.3333).abs() < 1e-3);
    }

    #[test]
    fn record_assigns_monotonic_indices_and_keeps_order() {
        let mut r = FrameRecorder::new(8, Refresh::Hz120);
        assert!(r.is_empty());
        for i in 0..5 {
            let s = r.record(timing(i as f32, 8.0));
            assert_eq!(s.frame, i);
        }
        assert_eq!(r.len(), 5);
        assert_eq!(r.frames_recorded(), 5);
        let got: Vec<u64> = r.samples().iter().map(|s| s.frame).collect();
        assert_eq!(got, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn ring_overwrites_oldest_when_full_and_reports_in_order() {
        // AC: capacity bound. Capacity 4, record 7 -> retain frames 3..=6, oldest
        // first, and the total-recorded counter keeps climbing past capacity.
        let mut r = FrameRecorder::new(4, Refresh::Hz120);
        for i in 0..7 {
            r.record(timing(i as f32, 8.0));
        }
        assert_eq!(r.capacity(), 4);
        assert_eq!(r.len(), 4);
        assert_eq!(r.frames_recorded(), 7);
        let frames: Vec<u64> = r.samples().iter().map(|s| s.frame).collect();
        assert_eq!(
            frames,
            vec![3, 4, 5, 6],
            "ring keeps the most recent capacity, in order"
        );
    }

    #[test]
    fn zero_capacity_is_clamped_to_one() {
        let mut r = FrameRecorder::new(0, Refresh::Hz60);
        assert_eq!(r.capacity(), 1);
        r.record(timing(1.0, 16.0));
        r.record(timing(2.0, 16.0));
        assert_eq!(r.len(), 1);
        assert_eq!(
            r.samples()[0].frame,
            1,
            "the single slot holds the newest frame"
        );
    }

    #[test]
    fn frame_dropped_is_derived_from_interval_vs_deadline() {
        // AC: frame_dropped = present_interval exceeded deadline + tolerance.
        // @60Hz deadline 16.67 + default tol 2.0 = 18.67 ms threshold.
        let mut r = FrameRecorder::new(8, Refresh::Hz60);
        assert!(
            !r.record(timing(4.0, 16.67)).frame_dropped,
            "on-cadence is not dropped"
        );
        assert!(
            !r.record(timing(4.0, 18.0)).frame_dropped,
            "within tolerance is not dropped"
        );
        assert!(
            r.record(timing(4.0, 33.3)).frame_dropped,
            "a doubled interval is a dropped frame"
        );
        // @120Hz a 16.67 ms interval (one missed 8.33 ms vsync) is dropped.
        let mut r120 = FrameRecorder::new(8, Refresh::Hz120);
        assert!(r120.record(timing(2.0, 16.67)).frame_dropped);
        assert!(!r120.record(timing(2.0, 8.33)).frame_dropped);
    }

    #[test]
    fn tolerance_is_configurable() {
        // A zero-tolerance recorder flags anything strictly over the deadline.
        let mut r = FrameRecorder::new(4, Refresh::Hz60).with_tolerance_ms(0.0);
        assert!(r.record(timing(1.0, 16.7)).frame_dropped);
        assert!(!r.record(timing(1.0, 16.6)).frame_dropped);
    }

    #[test]
    fn record_is_allocation_free_on_the_hot_path() {
        // AC: no per-frame allocation in the recording path. The backing ring is
        // pre-sized at construction (outside the probe); record() only writes by
        // index, so a steady-state frame allocates nothing.
        let mut r = FrameRecorder::new(256, Refresh::Hz120);
        // Warm the ring so we measure steady-state overwrites, not first-fill.
        for _ in 0..256 {
            r.record(timing(2.0, 8.0));
        }
        let allocs = crate::alloc_probe::count_allocs(|| {
            let s = r.record(timing(2.5, 8.1));
            std::hint::black_box(s);
        });
        assert_eq!(
            allocs, 0,
            "record() must not allocate on the hot path (got {allocs})"
        );
    }

    #[test]
    fn stats_compute_percentiles_max_and_dropped() {
        // present intervals 10,20,30,40,50 @120Hz (deadline 8.33 + tol 2.0 =
        // 10.33). The 10.0 ms frame is just UNDER the threshold (on-cadence), the
        // other four are dropped - so the dropped count must exclude it.
        let mut r = FrameRecorder::new(16, Refresh::Hz120);
        for (cpu, interval) in [
            (1.0, 10.0),
            (2.0, 20.0),
            (3.0, 30.0),
            (4.0, 40.0),
            (5.0, 50.0),
        ] {
            r.record(timing(cpu, interval));
        }
        let s = r.stats();
        assert_eq!(s.count, 5);
        // nearest-rank: p50 of 5 -> rank ceil(2.5)=3 -> idx 2 -> the 3rd value.
        assert!((s.cpu_p50_ms - 3.0).abs() < 1e-6);
        assert!((s.present_p50_ms - 30.0).abs() < 1e-6);
        // p99 of 5 -> rank ceil(4.95)=5 -> idx 4 -> the max.
        assert!((s.cpu_p99_ms - 5.0).abs() < 1e-6);
        assert!((s.cpu_max_ms - 5.0).abs() < 1e-6);
        assert!((s.present_max_ms - 50.0).abs() < 1e-6);
        assert_eq!(
            s.dropped, 4,
            "the 10.0 ms frame is under the 10.33 ms drop threshold"
        );
        assert!((s.dropped_pct - 80.0).abs() < 1e-6);
    }

    #[test]
    fn stats_on_empty_window_are_all_zero() {
        let r = FrameRecorder::new(8, Refresh::Hz60);
        let s = r.stats();
        assert_eq!(s.count, 0);
        assert_eq!(s.dropped, 0);
        assert_eq!(s.dropped_pct, 0.0);
        assert_eq!(s.present_max_ms, 0.0);
    }

    #[test]
    fn json_dump_is_valid_and_round_trips() {
        // AC: a run dumps valid JSON consumable by an analysis step. Dump ->
        // parse back -> identical samples -> same stats (proves it is consumable).
        let mut r = FrameRecorder::new(8, Refresh::Hz60);
        r.record(FrameTiming {
            cpu_frame_ms: 3.5,
            gpu_frame_ms: Some(2.1),
            present_interval_ms: 16.6,
            dirty_cells: 120,
            allocations: Some(0),
        });
        r.record(timing(4.0, 33.3));
        let json = r.to_json();
        let parsed: Vec<FrameSample> =
            serde_json::from_str(&json).expect("recorder JSON must parse");
        assert_eq!(parsed, r.samples());
        // gpu_frame_ms None survives the round trip as null/None.
        assert_eq!(parsed[0].gpu_frame_ms, Some(2.1));
        assert_eq!(parsed[1].gpu_frame_ms, None);
        assert_eq!(parsed[0].dirty_cells, 120);
        // The parsed-from-JSON window analyzes identically to the live one.
        assert_eq!(FrameStats::from_samples(&parsed), r.stats());
    }

    #[test]
    fn non_finite_timings_are_sanitized_to_keep_json_round_tripping() {
        // serde_json emits `null` for a non-finite f32, which the non-Option sample
        // fields reject on parse. record() sanitizes NaN/Inf to 0.0, so a garbage
        // timing still produces valid, re-parseable JSON (and is NOT a false drop).
        let mut r = FrameRecorder::new(4, Refresh::Hz60);
        let s = r.record(FrameTiming {
            cpu_frame_ms: f32::NAN,
            gpu_frame_ms: Some(f32::INFINITY),
            present_interval_ms: f32::INFINITY,
            dirty_cells: 7,
            allocations: None,
        });
        assert_eq!(s.cpu_frame_ms, 0.0);
        assert_eq!(s.present_interval_ms, 0.0);
        assert_eq!(s.gpu_frame_ms, Some(0.0));
        assert!(
            !s.frame_dropped,
            "a sanitized (0.0) interval is benign, not a counted drop"
        );
        // The dump is valid JSON that round-trips (no `null` for the f32 fields).
        let json = r.to_json();
        assert!(!json.contains("null") || json.contains("\"allocations\":null"));
        let parsed: Vec<FrameSample> =
            serde_json::from_str(&json).expect("sanitized dump must still parse");
        assert_eq!(parsed, r.samples());
    }
}
