//! Writing flat trees: every leaf type the writer supports, read back
//! by us, and checked against ROOT and uproot.
//!
//! `tests/cross/root_tree.C` makes ROOT write a tree with the same branches
//! and values as [`rows`] (the fixture `tests/data/root_tree.root`). We
//! write those rows too; then:
//!
//! - we read our file as we read ROOT's, value for value;
//! - with `RHIZOID_ORACLE_PYTHON` (a Python with uproot), uproot reads both
//!   files the same;
//! - with `RHIZOID_ROOT` (a `root` executable), ROOT reads both files the
//!   same, entry by entry, without a warning.

use std::path::{Path, PathBuf};

use rhizoid_io::tree::{Branch, TreeOptions};
use rhizoid_io::{ErrorKind, File, Row, WritableFile};

/// The rows of `root_tree.C`.
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
    #[row(count = "n")]
    jag: Vec<f64>,
    str: String,
}

/// Row `i` of `root_tree.C` (which writes rows 0 to 19).
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

/// The rows of `root_tree.C`.
fn rows() -> Vec<Every> {
    (0..20).map(row).collect()
}

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn out(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rhizoid-tree-write-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.join(name)
}

fn write_every(path: &Path, options: TreeOptions) {
    rhizoid_io::create(path, |f| {
        let mut t = f.tree_with::<Every>("t", options.title("every leaf type"))?;
        t.extend(&rows())?;
        t.finish()
    })
    .expect("write");
}

#[test]
fn every_leaf_type_reads_back_as_written() {
    let path = out("every.root");
    write_every(&path, TreeOptions::default());
    let file = File::open(&path).expect("open");
    let report = file.check().expect("check");
    assert!(report.is_ok(), "{report:?}");
    let tree = file.tree("t").expect("tree");
    assert_eq!(tree.num_entries(), 20);
    let read: Vec<Every> = tree
        .rows::<Every>()
        .expect("rows")
        .map(Result::unwrap)
        .collect();
    assert_eq!(read, rows());
    // The counter is a branch of its own, before the array it counts.
    let names: Vec<&str> = tree.branches().iter().map(Branch::name).collect();
    assert_eq!(
        names,
        [
            "b", "i8", "u8", "i16", "u16", "i32", "u32", "i64", "u64", "f32", "f64", "xyz", "n",
            "jag", "str"
        ]
    );
}

#[test]
fn we_read_our_tree_as_we_read_roots() {
    let path = out("ours.root");
    write_every(&path, TreeOptions::default());
    let theirs: Vec<Every> = File::open(data("root_tree.root"))
        .expect("open")
        .tree("t")
        .expect("tree")
        .rows::<Every>()
        .expect("rows")
        .map(Result::unwrap)
        .collect();
    assert_eq!(theirs, rows());
}

/// The members of an object as `path = value` lines, but for `skip`.
fn flatten(prefix: &str, o: &rhizoid_io::DynObject, skip: &[&str], out: &mut Vec<String>) {
    for base in o.bases() {
        flatten(&format!("{prefix}[{}]", base.class()), base, skip, out);
    }
    for (name, v) in o.members() {
        if skip.contains(&name) {
            continue;
        }
        let path = format!("{prefix}.{name}");
        match v {
            rhizoid_io::Value::Object(o) => {
                out.push(format!("{path}: {}", o.class()));
                flatten(&path, o, skip, out);
            }
            rhizoid_io::Value::List(items) => {
                for (i, item) in items.iter().enumerate() {
                    match item {
                        rhizoid_io::Value::Object(o) => {
                            out.push(format!("{path}[{i}]: {}", o.class()));
                            flatten(&format!("{path}[{i}]"), o, skip, out);
                        }
                        v => out.push(format!("{path}[{i}] = {v}")),
                    }
                }
            }
            v => out.push(format!("{path} = {v}")),
        }
    }
}

/// The tree object, member by member, is ROOT's but for where the baskets
/// are, how large they came out, and ROOT's empty slots in `fBaskets`.
#[test]
fn tree_object_matches_roots() {
    let path = out("object.root");
    write_every(&path, TreeOptions::default());
    let dump = |p: &Path| -> Vec<String> {
        let f = File::open(p).expect("open");
        let t: rhizoid_io::DynObject = f.get("t").expect("tree object");
        let mut out = Vec::new();
        let skip = [
            "fTotBytes",
            "fZipBytes",
            "fBasketBytes",
            "fBasketSeek",
            "fBaskets",
        ];
        flatten("t", &t, &skip, &mut out);
        out
    };
    let (ours, theirs) = (dump(&path), dump(&data("root_tree.root")));
    for (a, b) in ours.iter().zip(&theirs) {
        assert_eq!(a, b);
    }
    assert_eq!(ours.len(), theirs.len());
}

#[test]
fn clusters_hold_the_same_entries_in_every_branch() {
    let path = out("clusters.root");
    write_every(&path, TreeOptions::default().cluster_entries(7));
    let file = File::open(&path).expect("open");
    let tree = file.tree("t").expect("tree");
    assert_eq!(tree.clusters(), [0..7, 7..14, 14..20]);
    for b in tree.branches() {
        assert_eq!(b.num_baskets(), 3, "{}", b.name());
    }
    let read: Vec<Every> = tree
        .rows::<Every>()
        .expect("rows")
        .map(Result::unwrap)
        .collect();
    assert_eq!(read, rows());
    let t: rhizoid_io::DynObject = file.get("t").expect("tree object");
    assert_eq!(
        t.get("fAutoFlush").and_then(rhizoid_io::Value::as_i64),
        Some(7)
    );
}

#[test]
fn the_first_cluster_ends_at_the_byte_budget_and_sets_the_size() {
    let path = out("budget.root");
    // About 130 bytes a row: the first cluster ends at row 8.
    write_every(&path, TreeOptions::default().cluster_bytes(1000));
    let file = File::open(&path).expect("open");
    let tree = file.tree("t").expect("tree");
    let clusters = tree.clusters();
    let first = clusters[0].end;
    assert!(first > 1 && first < 20, "{clusters:?}");
    assert!(
        clusters[..clusters.len() - 1]
            .iter()
            .all(|c| c.end - c.start == first)
    );
    let read: Vec<Every> = tree
        .rows::<Every>()
        .expect("rows")
        .map(Result::unwrap)
        .collect();
    assert_eq!(read, rows());
}

#[derive(Row, Debug, PartialEq, Clone)]
struct Muons {
    #[row(count = "nMuon")]
    pt: Vec<f32>,
    #[row(count = "nMuon")]
    eta: Vec<f32>,
    // Default counter: n_jets.
    jets: Vec<i32>,
}

#[test]
fn fields_can_share_a_counter() {
    let path = out("shared.root");
    let rows = vec![
        Muons {
            pt: vec![1.0, 2.0],
            eta: vec![0.1, 0.2],
            jets: vec![],
        },
        Muons {
            pt: vec![],
            eta: vec![],
            jets: vec![7, 8, 9],
        },
        Muons {
            pt: vec![3.0],
            eta: vec![0.3],
            jets: vec![1],
        },
    ];
    rhizoid_io::create(&path, |f| {
        let mut t = f.tree::<Muons>("Events")?;
        t.extend(&rows)?;
        // Different lengths under one counter: refused, and not written.
        let bad = Muons {
            pt: vec![1.0],
            eta: vec![],
            jets: vec![],
        };
        let e = t.fill(&bad).expect_err("lengths differ");
        assert_eq!(e.kind(), ErrorKind::InvalidInput);
        assert!(e.to_string().contains("share a counter"), "{e}");
        assert_eq!(t.entries(), 3);
        t.finish()
    })
    .expect("write");
    let tree = File::open(&path)
        .expect("open")
        .tree("Events")
        .expect("tree");
    let names: Vec<&str> = tree.branches().iter().map(Branch::name).collect();
    assert_eq!(names, ["nMuon", "pt", "eta", "n_jets", "jets"]);
    let read: Vec<Muons> = tree
        .rows::<Muons>()
        .expect("rows")
        .map(Result::unwrap)
        .collect();
    assert_eq!(read, rows);
    let n: Vec<i32> = tree
        .branch::<i32>("nMuon")
        .expect("nMuon")
        .read(..)
        .expect("read");
    assert_eq!(n, [2, 0, 1]);
}

#[test]
fn tuples_name_their_branches() {
    let path = out("tuples.root");
    let mut f = WritableFile::create(&path).expect("create");
    {
        let mut t = f
            .tree_of::<(i32, Vec<f64>, String), 3>("t", ["run", "e", "label"])
            .expect("tree");
        t.fill(&(1, vec![0.5], "a".to_owned())).expect("fill");
        t.fill(&(2, vec![], "bb".to_owned())).expect("fill");
        // Dropped unfinished: finished when the file closes.
    }
    f.close().expect("close");
    let tree = File::open(&path).expect("open").tree("t").expect("tree");
    let read: Vec<(i32, Vec<f64>, String)> = tree
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
fn empty_trees_and_long_strings() {
    let path = out("edge.root");
    let long = "x".repeat(300);
    rhizoid_io::create(&path, |f| {
        f.tree_of::<(f32,), 1>("empty", ["x"])?.finish()?;
        let mut t = f.tree_of::<(String,), 1>("strings", ["s"])?;
        t.fill(&(long.clone(),))?;
        t.fill(&(String::new(),))?;
        t.finish()
    })
    .expect("write");
    let file = File::open(&path).expect("open");
    assert_eq!(file.tree("empty").expect("tree").num_entries(), 0);
    let s: Vec<(String,)> = file
        .tree("strings")
        .expect("tree")
        .rows_of::<(String,), 1>(["s"])
        .expect("rows")
        .map(Result::unwrap)
        .collect();
    assert_eq!(s, [(long,), (String::new(),)]);
}

#[test]
fn what_cannot_be_written_is_refused() {
    let path = out("refused.root");
    rhizoid_io::create(&path, |f| {
        let e = f.tree::<(i32,)>("t").expect_err("tuple without names");
        assert_eq!(e.kind(), ErrorKind::InvalidInput);
        let e = f
            .tree_of::<(rhizoid_io::Value,), 1>("t", ["v"])
            .expect_err("objects");
        assert_eq!(e.kind(), ErrorKind::Unsupported);
        let e = f
            .tree_of::<(i32, i32), 2>("t", ["a", "a"])
            .expect_err("duplicate");
        assert_eq!(e.kind(), ErrorKind::InvalidInput);
        let e = f
            .tree_of::<(i32, Vec<i32>), 2>("t", ["n_x", "x"])
            .expect_err("counter is a field");
        assert_eq!(e.kind(), ErrorKind::InvalidInput);
        let e = f.tree_of::<(i32,), 1>("t", ["a/b"]).expect_err("bad name");
        assert_eq!(e.kind(), ErrorKind::InvalidInput);
        Ok::<_, rhizoid_io::Error>(())
    })
    .expect("write");
}

fn uproot(python: &str, file: &Path) -> String {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/cross/uproot_dump_tree.py");
    let out = std::process::Command::new(python)
        .arg(script)
        .arg(file)
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
fn uproot_reads_our_tree_as_roots() {
    let Ok(python) = std::env::var("RHIZOID_ORACLE_PYTHON") else {
        return;
    };
    for (name, options) in [
        ("uproot1.root", TreeOptions::default()),
        ("uproot3.root", TreeOptions::default().cluster_entries(7)),
    ] {
        let path = out(name);
        write_every(&path, options);
        assert_eq!(
            uproot(&python, &path),
            uproot(&python, &data("root_tree.root")),
            "{name}"
        );
    }
}

/// What ROOT reads from a tree (`root_tree.C`'s dump), checking it said
/// nothing while reading.
fn root_dump(root: &str, file: &Path) -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/cross/root_tree.C");
    let out = std::process::Command::new(root)
        .args(["-b", "-q", "-l"])
        .arg(format!(
            "{}(\"{}\", \"t\")",
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
            !(line.contains("Error in <") || line.contains("Warning in <")),
            "ROOT complained reading {}: {line}",
            file.display()
        );
    }
    assert!(stdout.contains("FILE recovered 0"), "{stdout}");
    // Without ROOT's `Processing <macro>(<path>)...` line, and the blank
    // line ROOT before 6.42 prints ahead of the output.
    let mut kept = String::new();
    for line in stdout
        .lines()
        .filter(|l| !l.starts_with("Processing ") && !l.trim().is_empty())
    {
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

/// What `root_tree.C` prints for `rows`: `%.17g` prints these values
/// (small binary fractions) as Rust does.
fn expected_dump(rows: &[Every]) -> String {
    use std::fmt::Write;
    let mut s = format!("FILE recovered 0\nTREE entries {}\n", rows.len());
    for (name, leaf, title) in [
        ("b", "TLeafO", "b/O"),
        ("i8", "TLeafB", "i8/B"),
        ("u8", "TLeafB", "u8/b"),
        ("i16", "TLeafS", "i16/S"),
        ("u16", "TLeafS", "u16/s"),
        ("i32", "TLeafI", "i32/I"),
        ("u32", "TLeafI", "u32/i"),
        ("i64", "TLeafL", "i64/L"),
        ("u64", "TLeafL", "u64/l"),
        ("f32", "TLeafF", "f32/F"),
        ("f64", "TLeafD", "f64/D"),
        ("xyz", "TLeafF", "xyz[3]/F"),
        ("n", "TLeafI", "n/I"),
        ("jag", "TLeafD", "jag[n]/D"),
        ("str", "TLeafC", "str/C"),
    ] {
        writeln!(s, "BRANCH {name} {leaf} {title}").expect("write");
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
            ("n", r.jag.len().to_string()),
        ] {
            writeln!(s, "{i} {name} {v}").expect("write");
        }
        writeln!(s, "{i} jag{jag}").expect("write");
        writeln!(s, "{i} str {}", r.str).expect("write");
    }
    s
}

#[test]
fn root_reads_our_tree_as_its_own() {
    let Some(root) = std::env::var("RHIZOID_ROOT").ok() else {
        return;
    };
    let theirs = root_dump(&root, &data("root_tree.root"));
    assert_eq!(split_clusters(&theirs).1, expected_dump(&rows()));
    for (name, options) in [
        ("root1.root", TreeOptions::default()),
        ("root3.root", TreeOptions::default().cluster_entries(7)),
        ("root_lz4.root", TreeOptions::default().cluster_entries(3)),
    ] {
        let path = out(name);
        if name == "root_lz4.root" {
            let options = options.title("every leaf type");
            rhizoid_io::create_with(
                &path,
                rhizoid_io::WriteOptions::default()
                    .compression(rhizoid_io::Compression::lz4::<4>()),
                |f| {
                    let mut t = f.tree_with::<Every>("t", options)?;
                    t.extend(&rows())?;
                    t.finish()
                },
            )
            .expect("write");
        } else {
            write_every(&path, options);
        }
        let (clusters, rest) = split_clusters(&root_dump(&root, &path));
        assert_eq!(rest, split_clusters(&theirs).1, "{name}");
        let ours = File::open(&path).expect("open").tree("t").expect("tree");
        assert_eq!(clusters, ours.clusters(), "{name}: ROOT's clusters");
    }
}

/// A tree of many clusters, cut by the byte budget: ROOT reads every value
/// and walks the clusters as we wrote them.
#[test]
fn root_reads_a_large_tree() {
    let Some(root) = std::env::var("RHIZOID_ROOT").ok() else {
        return;
    };
    let path = out("large.root");
    let rows: Vec<Every> = (0..30_000).map(row).collect();
    rhizoid_io::create(&path, |f| {
        let mut t = f.tree_with::<Every>("t", TreeOptions::default().cluster_bytes(500_000))?;
        t.extend(&rows)?;
        t.finish()
    })
    .expect("write");
    let tree = File::open(&path).expect("open").tree("t").expect("tree");
    assert!(tree.clusters().len() > 4, "{:?}", tree.clusters());
    let (clusters, rest) = split_clusters(&root_dump(&root, &path));
    assert_eq!(clusters, tree.clusters());
    assert!(rest == expected_dump(&rows), "ROOT read other values");
}
