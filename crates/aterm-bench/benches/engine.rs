//! Engine throughput benches: VT parse, OSC scan, and block segmentation over a
//! fixed byte buffer. These anchor the 60fps-floor budget — each frame can spend
//! only ~16.6ms, so VT parse + scan of one frame's PTY output must stay well
//! under that. The numbers here are a regression tripwire, not a proof.

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

use aterm_core::{BlockList, BlockSegmenter, OscScanner, Terminal};

/// A representative chunk: prompt marks + SGR-colored output + plain text,
/// repeated to a realistic per-frame size.
fn sample_stream() -> Vec<u8> {
    let unit = b"\x1b]133;A\x07user@host:~/dev$ \x1b]133;B\x07ls -la\x1b]133;C\x07\
\x1b[1;34mdir1\x1b[0m  \x1b[1;34mdir2\x1b[0m  file.txt  README.md\r\n\
\x1b[32m-rw-r--r--\x1b[0m  1 user  staff  4096 Jun 23 12:00 file.txt\r\n\
\x1b]133;D;0\x07";
    let mut buf = Vec::with_capacity(unit.len() * 256);
    for _ in 0..256 {
        buf.extend_from_slice(unit);
    }
    buf
}

fn bench_vt_parse(c: &mut Criterion) {
    let data = sample_stream();
    let mut group = c.benchmark_group("vt_parse");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_function("advance_80x24", |b| {
        b.iter(|| {
            let mut term = Terminal::new(24, 80);
            term.feed(black_box(&data));
            black_box(term.snapshot());
        });
    });
    group.finish();
}

fn bench_osc_scan(c: &mut Criterion) {
    let data = sample_stream();
    let scanner = OscScanner::untrusted();
    let mut group = c.benchmark_group("osc_scan");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_function("scan", |b| {
        b.iter(|| {
            let r = scanner.scan(black_box(&data));
            black_box(r.marks.len())
        });
    });
    group.finish();
}

fn bench_block_segmentation(c: &mut Criterion) {
    let data = sample_stream();
    let scanner = OscScanner::untrusted();
    let scan = scanner.scan(&data);
    let mut group = c.benchmark_group("block_segmentation");
    group.bench_function("segment", |b| {
        b.iter(|| {
            let mut list = BlockList::new();
            let mut seg = BlockSegmenter::new();
            let mut offset = 0usize;
            for mark in &scan.marks {
                seg.apply(black_box(mark), offset, &mut list);
                offset += 8;
            }
            black_box(list.len())
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_vt_parse,
    bench_osc_scan,
    bench_block_segmentation
);
criterion_main!(benches);
