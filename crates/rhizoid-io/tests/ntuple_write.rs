//! Writing `RNTuple`s: every field type the writer supports, read
//! back by us, and checked against ROOT and uproot.
//!
//! `tests/cross/root_ntuple.C` makes ROOT write an `RNTuple` with the same
//! fields and values as [`rows`] (the fixture `tests/data/root_ntuple.root`).
//! We write those entries too; then:
//!
//! - we read our file as we read ROOT's, value for value (the metadata is
//!   compared with ROOT's record for record in `ntuple::write`'s tests);
//! - with `RHIZOID_ORACLE_PYTHON` (a Python with uproot), uproot reads both
//!   files the same;
//! - with `RHIZOID_ROOT` (a `root` executable), ROOT reads ours as it reads
//!   its own, with its checks on (page checksums), through
//!   `RNTupleReader` and `RNTupleInspector`, without a warning.

use std::path::{Path, PathBuf};

use rhizoid_io::ntuple::RNTupleOptions;
use rhizoid_io::{ErrorKind, File, ReadOptions, Row, WritableFile, WriteOptions};

mod support;

/// The entries of `root_ntuple.C`.
#[derive(Row, Clone, Debug, PartialEq)]
struct Every {
    b: bool,
    i8: i8,
    u8: u8,
    i16: i16,
    u16: u16,
    i32: i32,
    u32: u32,
    i64: i64,
    u64: u64,
    f32: f32,
    f64: f64,
    xyz: [f32; 3],
    jag: Vec<f64>,
    str: String,
}

/// Entry `i` of `root_ntuple.C` (which writes entries 0 to 19).
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "the C++ casts, wrapping as they do"
)]
fn row(i: i32) -> Every {
    Every {
        b: i % 3 == 0,
        i8: (-i) as i8,
        u8: (200 + i) as u8,
        i16: (-1000 * i) as i16,
        u16: (60000 + i) as u16,
        i32: i.wrapping_mul(-100_000),
        u32: 4_000_000_000 + i as u32,
        i64: -10_000_000_000 * i64::from(i),
        u64: 18_000_000_000_000_000_000 + i as u64,
        f32: 0.5 * i as f32,
        f64: 0.25 * f64::from(i),
        xyz: [0, 1, 2].map(|j| i as f32 + 0.125 * j as f32),
        jag: (0..i % 5).map(|j| f64::from(i * 10 + j)).collect(),
        str: format!(
            "entry {i}{}",
            if i % 4 == 0 {
                " is a longer string"
            } else {
                ""
            }
        ),
    }
}

fn rows() -> Vec<Every> {
    (0..20).map(row).collect()
}

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn out(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rhizoid-ntuple-write-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.join(name)
}

fn write_every(path: &Path, rows: &[Every], options: RNTupleOptions) {
    rhizoid_io::create(path, |f| {
        let mut w = f.rntuple_with::<Every>("t", options)?;
        w.extend(rows)?;
        w.finish()
    })
    .expect("write");
}

fn read_every(path: &Path) -> Vec<Every> {
    File::open(path)
        .expect("open")
        .rntuple("t")
        .expect("rntuple")
        .rows::<Every>()
        .expect("rows")
        .map(Result::unwrap)
        .collect()
}

#[test]
fn every_field_type_reads_back_as_written() {
    let path = out("every.root");
    write_every(&path, &rows(), RNTupleOptions::default());
    let file = File::open(&path).expect("open");
    let report = file.check().expect("check");
    assert!(report.is_ok(), "{report:?}");
    let keys: Vec<String> = file.keys().map(|k| k.class_name().to_owned()).collect();
    assert_eq!(keys, ["ROOT::RNTuple"]);
    let ntuple = file.rntuple("t").expect("rntuple");
    assert_eq!(ntuple.num_entries(), 20);
    assert_eq!(ntuple.clusters().len(), 1);
    assert_eq!(ntuple.clusters()[0], 0..20);
    assert!(
        ntuple.written_by().starts_with("rhizoid-io "),
        "{}",
        ntuple.written_by()
    );
    assert_eq!(read_every(&path), rows());
}

#[test]
fn we_read_our_ntuple_as_we_read_roots() {
    assert_eq!(read_every(&data("root_ntuple.root")), rows());
}

#[test]
fn clusters_and_pages_split_by_size() {
    let path = out("split.root");
    let rows: Vec<Every> = (0..2000).map(row).collect();
    write_every(
        &path,
        &rows,
        RNTupleOptions::default()
            .cluster_bytes(50_000)
            .page_bytes(1000),
    );
    let ntuple = File::open(&path)
        .expect("open")
        .rntuple("t")
        .expect("rntuple");
    assert!(ntuple.clusters().len() > 3, "{:?}", ntuple.clusters());
    assert_eq!(read_every(&path), rows);
    // A selection inside the third cluster reads as the whole does.
    let third = ntuple.clusters()[2].clone();
    let some: Vec<f64> = ntuple
        .field::<f64>("f64")
        .expect("field")
        .read(third.clone())
        .expect("read");
    let (a, b) = (
        usize::try_from(third.start).expect("start"),
        usize::try_from(third.end).expect("end"),
    );
    let expected: Vec<f64> = rows[a..b].iter().map(|r| r.f64).collect();
    assert_eq!(some, expected);
}

#[test]
fn tuples_name_their_fields_and_dropped_writers_finish() {
    let path = out("tuples.root");
    let mut f = WritableFile::create(&path).expect("create");
    {
        let mut w = f
            .rntuple_of::<(i32, Vec<f64>, String), 3>("t", ["run", "e", "label"])
            .expect("rntuple");
        w.fill(&(1, vec![0.5], "a".to_owned())).expect("fill");
        w.fill(&(2, vec![], "bb".to_owned())).expect("fill");
        // Dropped unfinished: finished when the file closes.
    }
    f.close().expect("close");
    let ntuple = File::open(&path)
        .expect("open")
        .rntuple("t")
        .expect("rntuple");
    let read: Vec<(i32, Vec<f64>, String)> = ntuple
        .rows_of::<(i32, Vec<f64>, String), 3>(["run", "e", "label"])
        .expect("rows")
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        read,
        [(1, vec![0.5], "a".to_owned()), (2, vec![], "bb".to_owned())]
    );
}

#[test]
fn empty_ntuples_long_strings_and_trees_alongside() {
    let path = out("edge.root");
    let long = "x".repeat(300);
    rhizoid_io::create(&path, |f| {
        f.rntuple_of::<(f32,), 1>("empty", ["x"])?.finish()?;
        let mut w = f.rntuple_of::<(String,), 1>("strings", ["s"])?;
        w.fill(&(long.clone(),))?;
        w.fill(&(String::new(),))?;
        // A tree and an RNTuple written at once.
        let mut t = f.tree_of::<(i32,), 1>("tree", ["x"])?;
        t.fill(&(7,))?;
        w.finish()?;
        t.finish()
    })
    .expect("write");
    let file = File::open(&path).expect("open");
    let empty = file.rntuple("empty").expect("rntuple");
    assert_eq!(empty.num_entries(), 0);
    let s: Vec<(String,)> = file
        .rntuple("strings")
        .expect("rntuple")
        .rows_of::<(String,), 1>(["s"])
        .expect("rows")
        .map(Result::unwrap)
        .collect();
    assert_eq!(s, [(long,), (String::new(),)]);
    assert_eq!(file.tree("tree").expect("tree").num_entries(), 1);
}

#[test]
fn what_cannot_be_written_is_refused() {
    let path = out("refused.root");
    rhizoid_io::create(&path, |f| {
        let e = f.rntuple::<(i32,)>("t").expect_err("tuple without names");
        assert_eq!(e.kind(), ErrorKind::InvalidInput);
        let e = f
            .rntuple_of::<(rhizoid_io::Value,), 1>("t", ["v"])
            .expect_err("objects");
        assert_eq!(e.kind(), ErrorKind::Unsupported);
        let e = f
            .rntuple_of::<(i32, i32), 2>("t", ["a", "a"])
            .expect_err("duplicate");
        assert_eq!(e.kind(), ErrorKind::InvalidInput);
        let e = f
            .rntuple_of::<(i32,), 1>("t", ["a.b"])
            .expect_err("bad name");
        assert_eq!(e.kind(), ErrorKind::InvalidInput);
        Ok::<_, rhizoid_io::Error>(())
    })
    .expect("write");
}

fn uproot(python: &str, file: &Path) -> String {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/cross/uproot_leaves_rntuple.py");
    let out = std::process::Command::new(python)
        .arg(script)
        .arg(file)
        .arg("100000")
        .arg("t")
        .output()
        .expect("run python");
    assert!(
        out.status.success(),
        "uproot failed on {}: {}",
        file.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn uproot_reads_our_ntuple_as_roots() {
    let Ok(python) = std::env::var("RHIZOID_ORACLE_PYTHON") else {
        return;
    };
    let theirs = uproot(&python, &data("root_ntuple.root"));
    assert!(!theirs.contains("\terror\t"), "{theirs}");
    for (name, options) in [
        ("uproot1.root", RNTupleOptions::default()),
        (
            "uproot_small.root",
            RNTupleOptions::default().cluster_bytes(700).page_bytes(64),
        ),
    ] {
        let path = out(name);
        write_every(&path, &rows(), options);
        assert_eq!(uproot(&python, &path), theirs, "{name}");
    }
}

/// One `ACLiC` build at a time: the tests share the build directory.
static ACLIC: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What ROOT reads from an `RNTuple` (`root_ntuple.C`'s dump), checking it
/// said nothing while reading.
fn root_dump(root: &str, file: &Path) -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/cross/root_ntuple.C");
    let _one = ACLIC
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Per process: other test runs may be compiling the same macro.
    let build = std::env::temp_dir().join(format!("rhizoid-aclic-{}", std::process::id()));
    let out = std::process::Command::new(root)
        .args(["-b", "-q", "-l", "-e"])
        .arg(format!(
            "gSystem->SetBuildDir(\"{}\", true);",
            build.display()
        ))
        .arg(format!(
            "{}+(\"{}\", \"t\")",
            script.display(),
            file.display()
        ))
        .output()
        .expect("run root");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "ROOT failed on {}: {stderr}",
        file.display()
    );
    for line in stdout.lines().chain(stderr.lines()) {
        assert!(
            !(line.contains("Error in <")
                || line.contains("Warning in <")
                || line.contains("Fatal")),
            "ROOT complained reading {}: {line}",
            file.display()
        );
    }
    let mut kept = String::new();
    for line in stdout.lines().filter(|l| {
        !l.starts_with("Processing ")
            && !l.starts_with("Info in <TUnixSystem::ACLiC>")
            && !l.trim().is_empty()
    }) {
        kept.push_str(line);
        kept.push('\n');
    }
    kept
}

/// The clusters in a dump, and the rest.
fn split_clusters(dump: &str) -> (Vec<core::ops::Range<u64>>, String) {
    let mut clusters = Vec::new();
    let mut rest = String::new();
    for line in dump.lines() {
        if let Some(c) = line.strip_prefix("CLUSTER ") {
            let (a, b) = c.split_once(' ').expect("two numbers");
            clusters.push(a.parse().expect("start")..b.parse().expect("end"));
        } else {
            rest.push_str(line);
            rest.push('\n');
        }
    }
    (clusters, rest)
}

/// What `root_ntuple.C` prints for `rows`: `%.17g` prints these values
/// (small binary fractions) as Rust does.
fn expected_dump(rows: &[Every]) -> String {
    use std::fmt::Write;
    let mut s = format!("NTUPLE entries {}\n", rows.len());
    for (name, ty) in [
        ("b", "bool"),
        ("i8", "std::int8_t"),
        ("u8", "std::uint8_t"),
        ("i16", "std::int16_t"),
        ("u16", "std::uint16_t"),
        ("i32", "std::int32_t"),
        ("u32", "std::uint32_t"),
        ("i64", "std::int64_t"),
        ("u64", "std::uint64_t"),
        ("f32", "float"),
        ("f64", "double"),
        ("xyz", "std::array<float,3>"),
        ("jag", "std::vector<double>"),
        ("str", "std::string"),
    ] {
        writeln!(s, "FIELD {name} {ty}").expect("write");
    }
    for (i, r) in rows.iter().enumerate() {
        let xyz: Vec<String> = r.xyz.iter().map(ToString::to_string).collect();
        let mut jag = String::new();
        for v in &r.jag {
            write!(jag, " {v}").expect("write");
        }
        for (name, v) in [
            ("b", u8::from(r.b).to_string()),
            ("i8", r.i8.to_string()),
            ("u8", r.u8.to_string()),
            ("i16", r.i16.to_string()),
            ("u16", r.u16.to_string()),
            ("i32", r.i32.to_string()),
            ("u32", r.u32.to_string()),
            ("i64", r.i64.to_string()),
            ("u64", r.u64.to_string()),
            ("f32", r.f32.to_string()),
            ("f64", r.f64.to_string()),
            ("xyz", xyz.join(" ")),
        ] {
            writeln!(s, "{i} {name} {v}").expect("write");
        }
        writeln!(s, "{i} jag{jag}").expect("write");
        writeln!(s, "{i} str {}", r.str).expect("write");
    }
    writeln!(s, "INSPECT columns 16 entries {}", rows.len()).expect("write");
    s
}

#[test]
fn root_reads_our_ntuple_as_its_own() {
    let Some(root) = std::env::var("RHIZOID_ROOT").ok() else {
        return;
    };
    let theirs = root_dump(&root, &data("root_ntuple.root"));
    assert_eq!(split_clusters(&theirs).1, expected_dump(&rows()));
    for (name, options) in [
        ("root1.root", RNTupleOptions::default()),
        (
            "root_small.root",
            RNTupleOptions::default().cluster_bytes(700).page_bytes(64),
        ),
    ] {
        let path = out(name);
        write_every(&path, &rows(), options);
        let (clusters, rest) = split_clusters(&root_dump(&root, &path));
        assert_eq!(rest, split_clusters(&theirs).1, "{name}");
        let ours = File::open(&path)
            .expect("open")
            .rntuple("t")
            .expect("rntuple");
        assert_eq!(clusters, ours.clusters(), "{name}: ROOT's clusters");
    }
}

/// An empty `RNTuple`, long strings, and a tree in the same file: ROOT
/// reads them all.
#[test]
fn root_reads_edge_cases() {
    let Some(root) = std::env::var("RHIZOID_ROOT").ok() else {
        return;
    };
    let path = out("root_edge.root");
    rhizoid_io::create(&path, |f| {
        f.rntuple_of::<(f32,), 1>("empty", ["x"])?.finish()?;
        let mut w = f.rntuple_of::<(String,), 1>("strings", ["s"])?;
        w.fill(&("x".repeat(300),))?;
        w.fill(&(String::new(),))?;
        let mut t = f.tree_of::<(i32,), 1>("tree", ["x"])?;
        t.fill(&(7,))?;
        w.finish()?;
        t.finish()
    })
    .expect("write");
    let script = out("root_edge.C");
    std::fs::write(
        &script,
        r#"#include <ROOT/RNTupleReader.hxx>
#include <TROOT.h>
// ROOT 6.34 has the RNTuple classes in ROOT::Experimental; later versions
// in ROOT.
#if ROOT_VERSION_CODE < ROOT_VERSION(6, 36, 0)
namespace RNT = ROOT::Experimental;
#else
namespace RNT = ROOT;
#endif
void root_edge(const char *p) {
  auto e = RNT::RNTupleReader::Open("empty", p);
  auto s = RNT::RNTupleReader::Open("strings", p);
  auto v = s->GetView<std::string>("s");
  auto f = TFile::Open(p);
  auto t = f->Get<TTree>("tree");
  printf("EMPTY %llu STRINGS %llu %zu %zu TREE %lld\n", (unsigned long long)e->GetNEntries(),
         (unsigned long long)s->GetNEntries(), v(0).size(), v(1).size(), t->GetEntries());
}
"#,
    )
    .expect("macro");
    let out = std::process::Command::new(&root)
        .args(["-b", "-q", "-l"])
        .arg(format!("{}(\"{}\")", script.display(), path.display()))
        .output()
        .expect("run root");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        !stderr.contains("Error") && !stderr.contains("Warning"),
        "{stderr}"
    );
    assert!(
        stdout.contains("EMPTY 0 STRINGS 2 300 0 TREE 1"),
        "{stdout}{stderr}"
    );
}

/// Many clusters and pages, and parallel compression with `rayon`: ROOT
/// reads every value and walks the clusters as we wrote them.
#[test]
fn root_reads_a_large_ntuple() {
    let Some(root) = std::env::var("RHIZOID_ROOT").ok() else {
        return;
    };
    let path = out("large.root");
    let rows: Vec<Every> = (0..30_000).map(row).collect();
    write_every(
        &path,
        &rows,
        RNTupleOptions::default()
            .cluster_bytes(500_000)
            .page_bytes(16_384),
    );
    let ntuple = File::open(&path)
        .expect("open")
        .rntuple("t")
        .expect("rntuple");
    assert!(ntuple.clusters().len() > 4, "{:?}", ntuple.clusters());
    let (clusters, rest) = split_clusters(&root_dump(&root, &path));
    assert_eq!(clusters, ntuple.clusters());
    assert!(rest == expected_dump(&rows), "ROOT read other values");
}

/// `finish` writes the `RNTuple` then, not when the file closes; `entries`
/// counts the entries filled.
#[test]
fn finish_writes_the_ntuple_at_once() {
    let sink = support::accounting::Shared::default();
    let mut f = WritableFile::to_sink(
        Box::new(sink.clone()),
        "finish.root",
        WriteOptions::default(),
    )
    .expect("create");
    let mut w = f.rntuple::<Every>("t").expect("rntuple");
    assert_eq!(w.entries(), 0);
    w.extend(&rows()).expect("fill");
    assert_eq!(w.entries(), 20);
    let before = sink.len();
    w.finish().expect("finish");
    assert!(sink.len() > before, "finish wrote nothing");
    f.close().expect("close");
    let file = File::from_source(sink.bytes(), ReadOptions::default()).expect("open");
    let read: Vec<Every> = file
        .rntuple("t")
        .expect("rntuple")
        .rows()
        .expect("rows")
        .map(Result::unwrap)
        .collect();
    assert_eq!(read, rows());
}
