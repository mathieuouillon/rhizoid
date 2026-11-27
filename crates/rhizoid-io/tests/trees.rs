//! `TTree` reading end to end, against a tree uproot wrote
//! (`tests/cross/uproot_trees.py`), and (optionally) against uproot itself.
#![expect(
    clippy::float_cmp,
    reason = "the values written are exact binary fractions"
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rhizoid_io::columnar::{Column, Interpretation, Jagged};
use rhizoid_io::streamer::Scalar;
use rhizoid_io::{ErrorKind, File, Value};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/uproot_trees.root")
}

/// The chunks the generator wrote; jagged values restart in each.
const CHUNKS: [(u64, u64); 3] = [(0, 600), (600, 1000), (1000, 1001)];

fn entries() -> impl Iterator<Item = u64> {
    0..1001
}

/// `jet_pt` as the generator builds it: `i % 4` values per entry, counting
/// by 0.5 from the start of the entry's chunk.
fn jet_pt() -> Vec<Vec<f32>> {
    let mut out = Vec::new();
    for (lo, hi) in CHUNKS {
        let mut k = 0_u16;
        for i in lo..hi {
            let n = i % 4;
            out.push(
                (0..n)
                    .map(|_| {
                        k += 1;
                        f32::from(k - 1) * 0.5
                    })
                    .collect(),
            );
        }
    }
    out
}

#[test]
fn reads_every_leaf_type() {
    let file = File::open(fixture()).expect("open");
    let tree = file.tree("events").expect("tree");
    assert_eq!(
        (tree.name(), tree.title(), tree.num_entries()),
        ("events", "test events", 1001)
    );
    assert_eq!(tree.clusters(), [0..600, 600..1000, 1000..1001]);
    assert_eq!(tree.branches().len(), 16);

    let ints = |f: fn(u64) -> i64| entries().map(f).collect::<Vec<_>>();
    let as_i64 = |v: Vec<i32>| v.into_iter().map(i64::from).collect::<Vec<_>>();
    assert_eq!(
        tree.branch::<bool>("b").expect("b").read(..).expect("read"),
        entries().map(|i| i % 2 == 0).collect::<Vec<_>>()
    );
    assert_eq!(
        tree.branch::<i8>("i8")
            .expect("i8")
            .read(..)
            .expect("read")
            .into_iter()
            .map(i64::from)
            .collect::<Vec<_>>(),
        ints(|i| i64::try_from(i % 100).expect("small") - 50)
    );
    assert_eq!(
        tree.branch::<u8>("u8")
            .expect("u8")
            .read(..)
            .expect("read")
            .into_iter()
            .map(u64::from)
            .collect::<Vec<_>>(),
        entries().map(|i| i % 200).collect::<Vec<_>>()
    );
    assert_eq!(
        as_i64(
            tree.branch::<i32>("i32")
                .expect("i32")
                .read(..)
                .expect("read")
        ),
        ints(|i| i64::try_from(i).expect("small") * 1000 - 77)
    );
    assert_eq!(
        tree.branch::<i64>("i64")
            .expect("i64")
            .read(..)
            .expect("read"),
        ints(|i| i64::try_from(i).expect("small") * 1_000_000_000_000 - 5)
    );
    assert_eq!(
        tree.branch::<u64>("u64")
            .expect("u64")
            .read(..)
            .expect("read"),
        entries()
            .map(|i| i * 10_000_000_000_000)
            .collect::<Vec<_>>()
    );
    let f32s = tree
        .branch::<f32>("f32")
        .expect("f32")
        .read(..)
        .expect("read");
    assert!(
        f32s.iter()
            .zip(entries())
            .all(|(&v, i)| f64::from(v) == f64::from(u32::try_from(i).expect("small")) * 0.25)
    );
    let xyz = tree
        .branch::<[f32; 3]>("xyz")
        .expect("xyz")
        .read(..)
        .expect("read");
    assert_eq!(xyz[7], [7.0, 14.0, 21.0]);
    assert_eq!(xyz.len(), 1001);
}

#[test]
fn reads_jagged_branches() {
    let file = File::open(fixture()).expect("open");
    let tree = file.tree("events").expect("tree");
    let jets = tree
        .branch::<Jagged<f32>>("jet_pt")
        .expect("jets")
        .read(..)
        .expect("read");
    assert_eq!(
        jets.iter().map(<[f32]>::to_vec).collect::<Vec<_>>(),
        jet_pt()
    );
    let counts = tree
        .branch::<i32>("njet_pt")
        .expect("counts")
        .read(..)
        .expect("read");
    assert!(
        jets.iter()
            .zip(&counts)
            .all(|(j, &n)| usize::try_from(n).ok() == Some(j.len()))
    );
    let q = tree
        .branch::<Jagged<i32>>("jet_q")
        .expect("q")
        .read(598..602)
        .expect("read");
    assert_eq!(
        q.iter().map(<[i32]>::to_vec).collect::<Vec<_>>(),
        [vec![-895, -896], vec![-897, -898, -899], vec![], vec![0]]
    );
}

#[test]
fn ranges_across_baskets_match_full_reads() {
    let file = File::open(fixture()).expect("open");
    let tree = file.tree("events").expect("tree");
    let all = tree.read("jet_pt", ..).expect("all");
    let full = tree
        .branch::<Jagged<f32>>("jet_pt")
        .expect("b")
        .read(..)
        .expect("read");
    for (lo, hi) in [
        (0, 0),
        (0, 1),
        (590, 610),
        (599, 1001),
        (1000, 1001),
        (1001, 1001),
    ] {
        let part = tree
            .branch::<Jagged<f32>>("jet_pt")
            .expect("b")
            .read(lo..hi)
            .expect("part");
        let lo_u = usize::try_from(lo).expect("small");
        assert_eq!(part.len(), usize::try_from(hi - lo).expect("small"));
        for (k, e) in part.iter().enumerate() {
            assert_eq!(Some(e), full.get(lo_u + k), "entry {}", lo_u + k);
        }
        let xyz = tree
            .branch::<[f32; 3]>("xyz")
            .expect("xyz")
            .read(lo..hi)
            .expect("xyz");
        assert!(
            xyz.iter()
                .enumerate()
                .all(|(k, v)| v[0] == f32::from(u16::try_from(lo_u + k).expect("small")))
        );
    }
    let inclusive = tree
        .branch::<[f32; 3]>("xyz")
        .expect("xyz")
        .read(599..=600)
        .expect("xyz");
    assert_eq!(
        inclusive,
        [[599.0, 1198.0, 1797.0], [600.0, 1200.0, 1800.0]]
    );
    assert_eq!(all.len(), 1001);
    assert!(matches!(all, Column::Jagged { .. }));
}

#[test]
fn reads_fetch_only_the_baskets_they_need() {
    let file = File::open(fixture()).expect("open");
    let tree = file.tree("events").expect("tree");
    tree.read("f64", 10..20).expect("read");
    let s = tree.read_stats();
    assert_eq!((s.requests, s.baskets), (1, 1));
    tree.read("f64", 590..610).expect("read");
    assert_eq!(tree.read_stats().baskets, 3);
    assert!(tree.read_stats().bytes_decompressed >= tree.read_stats().bytes_read);
}

#[test]
fn wrong_types_names_and_ranges_are_errors() {
    let file = File::open(fixture()).expect("open");
    let tree = file.tree("events").expect("tree");
    let e = tree.branch::<f64>("f32").expect_err("type");
    assert_eq!(e.kind(), ErrorKind::TypeMismatch);
    assert!(e.to_string().contains("holds f32, not f64"), "{e}");
    assert_eq!(
        tree.branch::<f32>("jet_pt").expect_err("jagged").kind(),
        ErrorKind::TypeMismatch
    );
    assert_eq!(
        tree.branch::<[f32; 2]>("xyz").expect_err("len").kind(),
        ErrorKind::TypeMismatch
    );
    assert_eq!(
        tree.branch::<f32>("nope").expect_err("absent").kind(),
        ErrorKind::NotFound
    );
    assert_eq!(
        tree.read("f32", 1000..1002).expect_err("range").kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        file.tree("nope").expect_err("no tree").kind(),
        ErrorKind::NotFound
    );

    let branch = tree.get_branch("jet_pt").expect("branch");
    assert_eq!(
        branch.interpretation().expect("i"),
        &Interpretation::Jagged {
            scalar: Scalar::F32,
            inner: 1
        }
    );
    assert_eq!(branch.leaves()[0].count_leaf(), Some("njet_pt"));
    assert!(tree.get_branch("njet_pt").expect("counter").leaves()[0].is_counter());
}

#[test]
fn empty_trees_read_as_empty() {
    let file = File::open(fixture()).expect("open");
    let tree = file.tree("empty").expect("tree");
    assert_eq!(tree.num_entries(), 0);
    assert_eq!(tree.clusters(), []);
    assert_eq!(
        tree.branch::<f64>("x").expect("x").read(..).expect("read"),
        Vec::<f64>::new()
    );
}

/// `KEY FIELD VALUES...` lines, keyed by `(branch, field)`.
fn uproot_tree(python: &str, file: &Path, tree: &str) -> HashMap<(String, String), Vec<String>> {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/cross/uproot_dump_tree.py");
    let out = std::process::Command::new(python)
        .arg(script)
        .arg(file)
        .arg(tree)
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "uproot failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split(' ');
            let key = (it.next()?.to_owned(), it.next()?.to_owned());
            Some((key, it.map(str::to_owned).collect()))
        })
        .collect()
}

/// Whether our value equals uproot's Python literal: integers exactly,
/// floats numerically, booleans by name.
fn same(ours: &Value, theirs: &str) -> bool {
    match ours {
        Value::Bool(b) => theirs == if *b { "True" } else { "False" },
        Value::F32(_) | Value::F64(_) => theirs.parse::<f64>().ok() == ours.as_f64(),
        Value::U64(u) => theirs.parse::<u64>().ok() == Some(*u),
        v => theirs.parse::<i64>().ok() == v.as_i64(),
    }
}

#[test]
fn uproot_cross_check_trees() {
    let Ok(python) = std::env::var("RHIZOID_ORACLE_PYTHON") else {
        return;
    };
    let file = File::open(fixture()).expect("open");
    let tree = file.tree("events").expect("tree");
    let theirs = uproot_tree(&python, &fixture(), "events");
    for b in tree.branches() {
        let name = b.name();
        let col = tree.read(name, ..).expect("read");
        let (values, counts) = match &col {
            Column::Values(a) | Column::Fixed { values: a, .. } => (a, None),
            Column::Jagged { offsets, values } => (
                values,
                Some(
                    offsets
                        .windows(2)
                        .map(|w| (w[1] - w[0]).to_string())
                        .collect::<Vec<_>>(),
                ),
            ),
            other => unreachable!("unexpected column {other:?}"),
        };
        if let Some(c) = counts {
            assert_eq!(
                &c,
                &theirs[&(name.to_owned(), "counts".to_owned())],
                "{name} counts"
            );
        }
        let expected = &theirs[&(name.to_owned(), "values".to_owned())];
        assert_eq!(values.len(), expected.len(), "{name}");
        for (i, t) in expected.iter().enumerate() {
            let v = values.get(i).expect("value");
            assert!(same(&v, t), "{name}[{i}]: ours {v}, uproot {t}");
        }
    }
}

mod corrupted {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        /// Flipped bytes in baskets and tree metadata give errors, never
        /// panics or runaway allocations.
        #[test]
        fn reading_corrupted_trees_never_panics(
            flips in proptest::collection::vec((0x100_usize..0xb000, any::<u8>()), 1..8),
        ) {
            let mut bytes = std::fs::read(fixture()).expect("fixture");
            for (at, v) in flips {
                if let Some(b) = bytes.get_mut(at) {
                    *b ^= v | 1;
                }
            }
            let Ok(file) = File::from_source(bytes, rhizoid_io::ReadOptions::default()) else {
                return Ok(());
            };
            let Ok(tree) = file.tree("events") else { return Ok(()) };
            let names: Vec<String> = tree.branches().iter().map(|b| b.name().to_owned()).collect();
            for name in names {
                let _ = tree.read(&name, ..);
            }
        }
    }
}

#[expect(clippy::single_range_in_vec_init, reason = "batches of one run")]
mod reader {
    use super::*;
    use rhizoid_io::tree::{BatchSize, EntrySelection};

    #[test]
    fn batches_follow_clusters_projection_and_selection() {
        let file = File::open(fixture()).expect("open");
        let tree = file.tree("events").expect("tree");
        let reader = tree
            .reader()
            .projection(["f32", "jet_pt"])
            .expect("projection");
        assert_eq!(reader.columns(), ["f32", "jet_pt"]);
        let batches: Vec<_> = reader
            .build()
            .collect::<rhizoid_io::Result<_>>()
            .expect("read");
        assert_eq!(batches.len(), 3, "one batch per cluster");
        assert_eq!(batches[1].entry_ranges(), [600..1000]);
        let all = tree
            .branch::<Jagged<f32>>("jet_pt")
            .expect("b")
            .read(..)
            .expect("read");
        let mut k = 0;
        for b in &batches {
            for e in b.get::<Jagged<f32>>("jet_pt").expect("col").iter() {
                assert_eq!(Some(e), all.get(k));
                k += 1;
            }
        }
        assert_eq!(k, 1001);

        // A sparse selection, with offset, limit and a batch size.
        let sel = EntrySelection::from_ranges([5..10, 700..705, 1000..1001]);
        let batches: Vec<_> = tree
            .reader()
            .projection("i32")
            .expect("p")
            .selection(&sel)
            .expect("s")
            .offset(2)
            .limit(7)
            .batch_size(BatchSize::entries(4))
            .build()
            .collect::<rhizoid_io::Result<_>>()
            .expect("read");
        let ranges: Vec<_> = batches.iter().map(|b| b.entry_ranges().to_vec()).collect();
        assert_eq!(ranges, [vec![7..10, 700..701], vec![701..704]]);
        let i32s = batches[0].get::<i32>("i32").expect("i32");
        assert_eq!(i32s, [6923, 7923, 8923, 699_923]);
        assert_eq!(
            tree.reader()
                .selection(990..1002)
                .expect_err("beyond")
                .kind(),
            ErrorKind::InvalidInput
        );
        assert_eq!(
            tree.reader()
                .projection(["nope"])
                .expect_err("absent")
                .kind(),
            ErrorKind::NotFound
        );
    }

    #[test]
    fn sparse_selections_skip_baskets() {
        let file = File::open(fixture()).expect("open");
        let tree = file.tree("events").expect("tree");
        let picked = tree
            .branch::<f64>("f64")
            .expect("b")
            .read(EntrySelection::from_entry_list(&[3, 1000]))
            .expect("read");
        assert_eq!(picked, [0.003, 1.0]);
        let s = tree.read_stats();
        assert_eq!(
            (s.requests, s.baskets),
            (1, 2),
            "the middle basket is not fetched"
        );
    }

    #[cfg(feature = "rayon")]
    #[test]
    fn parallel_batches_equal_sequential_ones() {
        use rayon::prelude::*;
        let file = File::open(fixture()).expect("open");
        let tree = file.tree("events").expect("tree");
        let seq: Vec<_> = tree
            .reader()
            .build()
            .collect::<rhizoid_io::Result<_>>()
            .expect("seq");
        let par: Vec<_> = tree
            .reader()
            .par_batches()
            .collect::<rhizoid_io::Result<_>>()
            .expect("par");
        assert_eq!(seq, par);
        // Clusters are a slice: rayon's parallel iterator comes for free.
        let n: usize = tree
            .clusters()
            .par_iter()
            .map(|c| {
                tree.branch::<Jagged<f32>>("jet_pt")
                    .and_then(|b| b.read(c.clone()))
                    .map(|j| j.len())
                    .expect("cluster")
            })
            .sum();
        assert_eq!(n, 1001);
    }
}

/// A branch of whole objects (split level 0, `root_unsplit.C`): a TH1F an
/// entry, whose axes after the first refer to the first's class tag by
/// position in the basket; every entry decodes.
#[test]
#[expect(clippy::cast_precision_loss, reason = "small counts")]
fn whole_objects_per_entry() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/root_unsplit.root");
    let file = File::open(path).expect("open");
    let hs = match file.tree("t").expect("tree").read("h", ..).expect("read") {
        Column::Objects(v) => v,
        other => unreachable!("{other:?}"),
    };
    assert_eq!(hs.len(), 6);
    for (i, h) in hs.iter().enumerate() {
        let h = h.as_object().expect("an object");
        assert_eq!(h.class(), "TH1F");
        assert_eq!(
            h.get("fName").and_then(Value::as_str),
            Some(format!("h{i}").as_str())
        );
        for axis in ["fXaxis", "fYaxis", "fZaxis"] {
            let a = h.get(axis).and_then(Value::as_object).expect("axis");
            assert_eq!(a.class(), "TAxis");
        }
        let x = h.get("fXaxis").and_then(Value::as_object).expect("x");
        assert_eq!(x.get("fNbins").and_then(Value::as_i64), Some(4));
        assert_eq!(h.get("fEntries").and_then(Value::as_f64), Some(i as f64));
    }
}
