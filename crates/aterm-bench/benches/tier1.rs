//! Tier-1 instruction-count micro-benches (ticket T-1.7).
//!
//! `iai-callgrind` runs each benchmark ONCE under valgrind/callgrind and reports a
//! deterministic *instruction count* - immune to the wall-clock noise of shared CI
//! runners - so it can gate every PR on a >N% regression (start 5%; see
//! `09-performance-60fps.md` section 9, Recommendation 7). These cover the
//! GPU-free hot paths the 60fps floor depends on: VT parse, the grid->snapshot
//! frame build, damage computation, and OSC scanning.
//!
//! Each benchmark uses a `setup` function so the cost of building the input (the
//! `Terminal`, the payload, the pre-fed grid state) is NOT attributed to the count
//! - only the operation under test is measured.
//!
//! Block segmentation is deliberately NOT in this instruction-count gate:
//! `BlockSegmenter::apply` reads the wall clock (`Instant::now()`) to stamp block
//! start/finish times, which is non-deterministic under callgrind and would defeat
//! the noise-immunity the gate relies on. It stays a criterion throughput bench
//! (`benches/engine.rs`); admitting it to Tier-1 needs the segmenter made
//! clock-injectable (a small `aterm-core` change, out of this bench ticket).
//!
//! NOTE: this requires valgrind, which is a Linux-CI concern; `cargo bench -p
//! aterm-bench --bench tier1` does not run on macOS (no valgrind). The harness
//! itself compiles on every platform. The per-cell GPU instance-buffer build is
//! added when the grid fast-path lands (T-1.6); `snapshot_into` is its CPU-side
//! precursor and the GPU-free frame-build proxy measured here.

use std::hint::black_box;

use iai_callgrind::{library_benchmark, library_benchmark_group, main};

use aterm_bench::fixtures;
use aterm_core::{Damage, OscScanner, Snapshot, Terminal};

const ROWS: usize = 24;
const COLS: usize = 80;

// --- (a) VT parse + (b) grid mutation / scroll -------------------------------

/// Setup (uncounted): a fresh terminal plus the payload to feed it.
fn fresh_term(payload: Vec<u8>) -> (Terminal, Vec<u8>) {
    (Terminal::new(ROWS, COLS), payload)
}

#[library_benchmark(setup = fresh_term)]
#[bench::plaintext_scroll(fixtures::plaintext_scroll())]
#[bench::sgr_heavy(fixtures::sgr_heavy())]
#[bench::unicode(fixtures::unicode())]
#[bench::alt_screen(fixtures::alt_screen())]
fn parse(input: (Terminal, Vec<u8>)) -> Terminal {
    let (mut term, bytes) = input;
    term.feed(black_box(&bytes));
    term // returned so iai black-boxes it; feed cannot be optimized away
}

// --- (d) frame-build CPU work: grid -> Snapshot (the zero-alloc reuse path) ---

/// Setup (uncounted): a terminal already fed `payload`, plus a pre-allocated
/// snapshot buffer - so the bench measures only `snapshot_into` (the steady-state
/// reuse path that the render loop hits every frame), not the parse or the alloc.
fn fed_term_and_buf(payload: Vec<u8>) -> (Terminal, Snapshot) {
    let mut term = Terminal::new(ROWS, COLS);
    term.feed(&payload);
    let buf = Snapshot::empty(term.rows(), term.cols());
    (term, buf)
}

#[library_benchmark(setup = fed_term_and_buf)]
#[bench::plaintext(fixtures::plaintext_scroll())]
#[bench::sgr_heavy(fixtures::sgr_heavy())]
fn snapshot_into(input: (Terminal, Snapshot)) -> Snapshot {
    let (term, mut buf) = input;
    term.snapshot_into(black_box(&mut buf));
    buf
}

// --- (c) damage computation --------------------------------------------------

/// Setup (uncounted): a terminal fed `payload` and NOT drained, so the counted
/// `take_damage` hits the cheap `Damage::Full` early-return (the scroll / clear /
/// alt-screen path, which marks the whole grid dirty).
fn fed_term(payload: Vec<u8>) -> Terminal {
    let mut term = Terminal::new(ROWS, COLS);
    term.feed(&payload);
    term
}

/// Setup (uncounted): fill the grid, DRAIN the initial full damage, then feed a
/// small in-place edit - so PARTIAL line damage is pending and the counted
/// `take_damage` measures the `Damage::Lines(Vec<..>)` *collection*, which is the
/// realistic per-frame path the damage-tracking renderer (T-1.8) actually rides
/// (only a handful of lines change per frame). Without the drain, alacritty stays
/// fully-damaged and `take_damage` would take the trivial `Full` arm.
fn fed_term_pending_partial(edit: Vec<u8>) -> Terminal {
    let mut term = Terminal::new(ROWS, COLS);
    term.feed(&fixtures::plaintext_scroll());
    let _ = term.take_damage(); // drain the initial full damage
    term.feed(&edit); // small in-place edit -> a few partially-damaged lines
    term
}

#[library_benchmark(setup = fed_term)]
#[bench::full_scroll(fixtures::plaintext_scroll())]
#[bench::full_altscreen(fixtures::alt_screen())]
fn damage_full(mut term: Terminal) -> Damage {
    black_box(term.take_damage())
}

#[library_benchmark(setup = fed_term_pending_partial)]
#[bench::partial_lines(fixtures::partial_edit())]
fn damage_partial(mut term: Terminal) -> Damage {
    black_box(term.take_damage())
}

// --- OSC scan ----------------------------------------------------------------

/// Setup (uncounted): a fresh untrusted scanner plus the payload.
fn scanner_and_payload(payload: Vec<u8>) -> (OscScanner, Vec<u8>) {
    (OscScanner::untrusted(), payload)
}

#[library_benchmark(setup = scanner_and_payload)]
#[bench::prompt_cycle(fixtures::prompt_cycle())]
fn osc_scan(input: (OscScanner, Vec<u8>)) -> usize {
    let (mut scanner, bytes) = input;
    let scan = scanner.scan(black_box(&bytes));
    black_box(scan.marks.len())
}

library_benchmark_group!(
    name = tier1;
    benchmarks = parse, snapshot_into, damage_full, damage_partial, osc_scan
);

main!(library_benchmark_groups = tier1);
