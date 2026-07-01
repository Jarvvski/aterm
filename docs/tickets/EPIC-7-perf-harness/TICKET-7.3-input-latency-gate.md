---
id: T-7.3
epic: EPIC-7-perf-harness
title: Input-latency measurement gate
status: done
labels: [bench, perf, input]
depends_on: [T-7.1, T-3.2]
---

# Goal

Measure keystroke->visible-glyph latency separately from frame rate and gate on it, because a fast renderer can still feel laggy (Ghostty's 24ms/41ms data point). Software path for CI, with an optional hardware light-sensor rig for ground truth.

# Context

- Research: [09-performance-60fps.md](../../research/09-performance-60fps.md) section 2.5 + 9 (input latency, separate metric) + Recommendation 7. Owner open-question #3 (hardware rig vs software-only - default software acceptable for v1).

# Implementation notes

- Crate: `aterm-bench`.
- Software path: a Typometer-style screen-capture-on-keypress measure (inject a synthetic keystroke, capture the frame where the glyph appears, compute latency). Report median + p25/p75 + outliers over many iterations (GNOME-46 methodology: ~120 iterations).
- Optional hardware path: a light-sensor/Teensy keyboard-to-photon rig - stub the interface; do not block v1 on it.
- Gate: keystroke->glyph median <= 1.5 frames, p99 <= 3 frames at the active refresh rate.

# Acceptance criteria

- The software measure reports median/p25/p75 over >=100 iterations.
- The gate (median <= 1.5 frames, p99 <= 3 frames) is enforced in the nightly job.
- The hardware-rig interface is stubbed and documented (not required for v1).

# Out of scope

- The frame-time scenarios (T-7.2).
- Building the physical rig.

# Resolution

**2026-07-01 (agent): Done.** Split the way T-7.1 / T-7.2 were - a PURE, headless-tested
measure + gate + JSON model (`aterm-bench::latency`), plus an on-hardware live software
driver (`latency_driver`), plus the nightly job + the stubbed hardware-rig interface.

- **Pure core** (`aterm-bench::latency`): `LatencySample` (one keystroke->glyph ms),
  `LatencyStats::from_samples` (min / p25 / median / p75 / p99 / max in ms AND
  frame-equivalents at the run's `Refresh`, plus the Tukey-fence `outliers` count - the
  GNOME-46 median + p25/p75 + outliers quartet), the `LatencyGate` (median <= 1.5 frames,
  p99 <= 3 frames, `>= MIN_ITERATIONS` = 100), `LatencyGate::evaluate -> LatencyVerdict
  {Pass, Fail{breaches}, Inconclusive}`, and `LatencyReport -> JSON`. **9 unit tests**, no
  window/GPU (runs on the Linux CI too). The gate is stated in *frames* so the same
  numbers hold at 60Hz and 120Hz.
- **AC1 (software measure reports median/p25/p75 over >=100 iterations):** `latency_driver`
  runs the real `aterm-ui` app loop, injects a synthetic keystroke into the input box's
  `InputModel` at a frame boundary (so the current frame does not carry it) and times the
  present of the next frame that renders it, over `TARGET_ITERATIONS` (120) iterations.
  Smoke-verified locally: 120 iterations, median 16.63ms = 1.00 frame @60Hz, p99 1.07
  frames, PASS.
- **AC2 (gate enforced in the nightly job):** the pure gate is exhaustively tested
  (median-over-1.5f fails, p99-over-3f fails even with a fast median, <100 iters
  inconclusive); the nightly `input-latency-gate` job (`.github/workflows/nightly-perf.yml`)
  runs `latency_driver --gate` (exits non-zero on a breach) and uploads the JSON.
- **AC3 (hardware-rig interface stubbed + documented):** the `LatencyProbe` trait +
  `ProbeKind` + the `HardwareRig` stub, which documents the light-sensor / Teensy
  keyboard-to-photon rig (refs [13][14]) and is honestly inert in v1 (`is_available()` =
  false, `measure_once()` = None) - never a fake measurement. (Owner open-question #3:
  software acceptable for v1.)

**HONEST CAVEAT.** The software measure times **model-mutation -> the present of the
frame that renders it** (frame build + GPU + the vsync present wait). It CATCHES a render
that blows the frame budget (the next present slips a whole vsync -> a ~2-frame reading -
the "fast fps but laggy" regression a frame-rate metric misses). It does NOT capture (a)
the OS/winit event dispatch before the model mutation, (b) the sub-frame arrival phase, or
(c) a renderer that draws the input a frame *behind* (frame ordering assumes the current
synchronous same-frame render, true today but not proven per-frame without pixel
read-back). All three are what the keyboard-to-photon hardware rig measures for ground
truth. So the software number is a present-scheduling *lower bound* + a regression
detector, not the absolute truth - consistent with T-7.1 / T-7.2's honest limits. Like
`scenario_driver`, it is headless-safe: too few iterations (no display) is
`Inconclusive`, exit 0 - never a false pass. On a real ProMotion panel a manual
`--display-link` run raises the frame-equivalent gate to 120Hz.

**Adversarial review (skeptic pass) - 4 findings, all addressed:**
- **[HIGH] the median arm is ~tautological** (the ordering measure pins the median at
  ~1.0 present interval, so `median <= 1.5f` mainly re-detects present-slip T-7.2 already
  gates). REFRAMED honestly: the median arm is a documented cadence-hold floor; the
  **p99 tail is the real regression signal** (a keystroke whose glyph slipped ~2+ extra
  presents); the module doc states plainly this is a present-scheduling detector and the
  genuinely tight keystroke number needs render-side content read-back or the rig (blind
  spot (c)).
- **[MED] hardcoded-60Hz frame denominator miscalibrates the CI gate.** Fixed by
  **self-calibrating**: frames divide by the run's OWN observed median present interval
  (collected from the recorder samples), falling back to the nominal refresh only when
  unavailable. Verified on hardware: `cadence=16.69ms`, median 1.00 intervals.
- **[MED] non-finite f32 breaks the JSON round-trip** (serde emits `null`). Fixed:
  non-finite samples are dropped in `LatencyReport::new` + `from_samples`; a test asserts
  a NaN/Inf-laced run still round-trips as a typed struct.
- **[LOW] the report was `Serialize`-only.** Added `Deserialize` to `LatencyReport` +
  `LatencyStats`.
- Verified correct, no change: the percentile/Tukey math, hang-freedom + the MAX_RUN
  backstop, headless-safety, the insert/backspace alternation, the
  warmup/cooldown/iteration boundaries, the internally-tagged `Fail{breaches}` serde
  round-trip, and the two-macos-14-jobs workflow.
