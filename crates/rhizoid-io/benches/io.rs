//! Benchmarks of the operations users wait on: opening a file, reading a
//! column (flat and jagged, from a tree and an `RNTuple`, zlib and LZ4),
//! and writing trees and `RNTuple`s.
//!
//! ```text
//! cargo bench -p rhizoid-io --bench io
//! ```
//!
//! The data is generated once into `RHIZOID_BENCH_DATA` (default: a
//! `rhizoid-bench` folder in the temporary directory), where
//! `benches/compare.py` times uproot and ROOT on the same files.

#![expect(missing_docs, reason = "criterion's macros")]

use std::hint::black_box;
use std::path::PathBuf;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rhizoid_io::columnar::Jagged;
use rhizoid_io::{Compression, File, Row, WritableFile, WriteOptions};

/// Entries in the generated files.
const ENTRIES: u64 = 1_000_000;
/// Rows written per write benchmark.
const WRITTEN: u64 = 100_000;

#[derive(Row, Clone)]
struct Event {
    id: i32,
    x: f32,
    #[row(count = "n")]
    pt: Vec<f32>,
}

fn event(i: u64) -> Event {
    let k = u32::try_from(i % 9).unwrap_or(0);
    #[expect(clippy::cast_precision_loss, reason = "benchmark data")]
    Event {
        id: i32::try_from(i % 1_000_000).unwrap_or(0),
        x: (i as f32) * 0.5,
        pt: (0..k)
            .map(|j| f32::from(u16::try_from(j).unwrap_or(0)) + 0.25)
            .collect(),
    }
}

fn data_dir() -> PathBuf {
    std::env::var_os("RHIZOID_BENCH_DATA")
        .map_or_else(|| std::env::temp_dir().join("rhizoid-bench"), PathBuf::from)
}

/// The generated files, written once: a tree compressed with zlib and with
/// LZ4, and an `RNTuple`, of `ENTRIES` events each.
fn files() -> [(&'static str, PathBuf); 3] {
    let dir = data_dir();
    std::fs::create_dir_all(&dir).expect("bench data dir");
    let files = [
        ("tree_zlib", dir.join("tree_zlib.root")),
        ("tree_lz4", dir.join("tree_lz4.root")),
        ("rntuple", dir.join("rntuple.root")),
    ];
    for (name, path) in &files {
        if path.exists() {
            continue;
        }
        let compression = if *name == "tree_lz4" {
            Compression::lz4::<4>()
        } else {
            Compression::zlib::<1>()
        };
        rhizoid_io::create_with(
            path,
            WriteOptions::default().compression(compression),
            |f| {
                if *name == "rntuple" {
                    let mut w = f.rntuple::<Event>("events")?;
                    (0..ENTRIES).try_for_each(|i| w.fill(&event(i)))?;
                    w.finish()
                } else {
                    let mut w = f.tree::<Event>("events")?;
                    (0..ENTRIES).try_for_each(|i| w.fill(&event(i)))?;
                    w.finish()
                }
            },
        )
        .expect("write bench data");
    }
    files
}

fn open(c: &mut Criterion) {
    let files = files();
    let mut g = c.benchmark_group("open");
    for (name, path) in &files {
        g.bench_function(*name, |b| {
            b.iter(|| {
                let f = File::open(path).expect("open");
                black_box(f.keys().count())
            });
        });
    }
    g.finish();
}

fn read(c: &mut Criterion) {
    let files = files();
    let mut g = c.benchmark_group("read");
    g.throughput(Throughput::Elements(ENTRIES));
    g.sample_size(20);
    for (name, path) in &files {
        let file = File::open(path).expect("open");
        if *name == "rntuple" {
            let nt = file.rntuple("events").expect("rntuple");
            g.bench_function(BenchmarkId::new("f32", name), |b| {
                b.iter(|| black_box(nt.field::<f32>("x").expect("x").read(..).expect("read")));
            });
            g.bench_function(BenchmarkId::new("jagged_f32", name), |b| {
                b.iter(|| {
                    black_box(
                        nt.field::<Jagged<f32>>("pt")
                            .expect("pt")
                            .read(..)
                            .expect("read"),
                    )
                });
            });
        } else {
            let t = file.tree("events").expect("tree");
            g.bench_function(BenchmarkId::new("f32", name), |b| {
                b.iter(|| black_box(t.branch::<f32>("x").expect("x").read(..).expect("read")));
            });
            g.bench_function(BenchmarkId::new("i32", name), |b| {
                b.iter(|| black_box(t.branch::<i32>("id").expect("id").read(..).expect("read")));
            });
            g.bench_function(BenchmarkId::new("jagged_f32", name), |b| {
                b.iter(|| {
                    black_box(
                        t.branch::<Jagged<f32>>("pt")
                            .expect("pt")
                            .read(..)
                            .expect("read"),
                    )
                });
            });
        }
    }
    g.finish();
}

fn write(c: &mut Criterion) {
    let rows: Vec<Event> = (0..WRITTEN).map(event).collect();
    let mut g = c.benchmark_group("write");
    g.throughput(Throughput::Elements(WRITTEN));
    g.sample_size(20);
    for (name, compression) in [
        ("zlib", Compression::zlib::<1>()),
        ("lz4", Compression::lz4::<4>()),
    ] {
        g.bench_function(BenchmarkId::new("tree", name), |b| {
            b.iter(|| {
                let options = WriteOptions::default().compression(compression);
                let mut f = WritableFile::to_sink(Box::new(Vec::new()), "bench.root", options)
                    .expect("create");
                let mut w = f.tree::<Event>("events").expect("tree");
                w.extend(&rows).expect("fill");
                w.finish().expect("finish");
                f.close().expect("close");
            });
        });
        g.bench_function(BenchmarkId::new("rntuple", name), |b| {
            b.iter(|| {
                let options = WriteOptions::default().compression(compression);
                let mut f = WritableFile::to_sink(Box::new(Vec::new()), "bench.root", options)
                    .expect("create");
                let mut w = f.rntuple::<Event>("events").expect("rntuple");
                w.extend(&rows).expect("fill");
                w.finish().expect("finish");
                f.close().expect("close");
            });
        });
    }
    g.finish();
}

criterion_group!(benches, open, read, write);
criterion_main!(benches);
