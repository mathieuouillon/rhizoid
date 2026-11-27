//! The API's contract beyond round trips: paths, cycles and directories;
//! what each call refuses, and with which error; read limits; edge values
//! (empty writers, long and non-ASCII strings); reproducible output; and
//! handles shared across threads.

#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};

use rhizoid_io::objects::{Axis, Graph, Hist1, Named};
use rhizoid_io::{ErrorKind, File, ReadLimits, ReadOptions, Row, WriteOptions};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("rhizoid-api-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&p).expect("mkdir");
        Self(p)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn hist(name: &str, fill: f64) -> Hist1<f64> {
    Hist1::<f64>::new(
        name,
        "a histogram",
        Axis::uniform(3, 0.0, 3.0).expect("axis"),
    )
    .with_values_flow(vec![0.0, fill, 0.0, 0.0, 0.0])
    .expect("cells")
}

fn kind<T: std::fmt::Debug>(r: rhizoid_io::Result<T>) -> ErrorKind {
    r.expect_err("an error").kind()
}

#[derive(Row, Clone, Debug, PartialEq)]
struct Event {
    id: i32,
    x: f32,
    #[row(count = "n")]
    hits: Vec<f64>,
    label: String,
}

fn events(n: i32) -> Vec<Event> {
    (0..n)
        .map(|i| Event {
            id: i,
            x: 0.5 * f32::from(i16::try_from(i).expect("small")),
            hits: (0..i % 4).map(f64::from).collect(),
            label: format!("event {i}"),
        })
        .collect()
}

fn write_data(path: &Path) {
    rhizoid_io::create(path, |f| {
        f.put("h", &hist("h", 1.0))?;
        f.put("h", &hist("h", 2.0))?;
        f.put(
            "g",
            &Graph::new("g", "a graph", vec![1.0, 2.0], vec![3.0, 4.0])?,
        )?;
        f.mkdir("a/b")?.put("n", &Named::new("n", "nested"))?;
        let mut t = f.tree::<Event>("t")?;
        t.extend(&events(50))?;
        t.finish()?;
        let mut nt = f.rntuple::<Event>("nt")?;
        nt.extend(&events(50))?;
        nt.finish()
    })
    .expect("write");
}

#[test]
fn paths_name_cycles_and_directories() {
    let dir = TempDir::new("paths");
    let path = dir.path("paths.root");
    write_data(&path);
    let file = File::open(&path).expect("open");

    // Writing a name twice makes a new cycle; the plain name is the highest.
    let cycles: Vec<u16> = file
        .keys()
        .filter(|k| k.name() == "h")
        .map(|k| k.cycle())
        .collect();
    assert_eq!(cycles.len(), 2, "{cycles:?}");
    assert_eq!(file.get::<Hist1<f64>>("h").expect("h").values()[0], 2.0);
    assert_eq!(file.get::<Hist1<f64>>("h;1").expect("h;1").values()[0], 1.0);
    assert_eq!(file.get::<Hist1<f64>>("h;2").expect("h;2").values()[0], 2.0);

    // Nested directories, by path and step by step.
    assert_eq!(file.get::<Named>("a/b/n").expect("a/b/n").title(), "nested");
    let b = file.root().subdir("a").expect("a").subdir("b").expect("b");
    assert_eq!(b.path(), "a/b");
    assert_eq!(b.get::<Named>("n").expect("n").title(), "nested");
    assert!(
        file.root()
            .keys()
            .any(|k| k.name() == "a" && k.is_directory())
    );

    // What a path can fail with.
    assert_eq!(kind(file.get_dyn("nothing")), ErrorKind::NotFound);
    assert_eq!(kind(file.get_dyn("h;9")), ErrorKind::NotFound);
    assert_eq!(kind(file.get_dyn("a/nothing/n")), ErrorKind::NotFound);
    assert_eq!(kind(file.get_dyn("a")), ErrorKind::InvalidPath);
    assert!(
        file.root().subdir("h").is_err(),
        "an object is not a directory"
    );
    assert_eq!(kind(file.get::<Graph>("h")), ErrorKind::ClassMismatch);
}

/// A row type whose names are in no tree written here.
#[derive(Row, Debug)]
struct Other {
    energy: f64,
}

#[test]
fn trees_and_rntuples_refuse_misuse() {
    let dir = TempDir::new("misuse");
    let path = dir.path("misuse.root");
    write_data(&path);
    let file = File::open(&path).expect("open");

    // A tree is not an RNTuple, and the other way round.
    assert!(file.tree("nt").is_err());
    assert!(file.rntuple("t").is_err());
    assert!(file.tree("h").is_err());
    assert_eq!(kind(file.tree("missing")), ErrorKind::NotFound);
    assert_eq!(kind(file.rntuple("missing")), ErrorKind::NotFound);

    let t = file.tree("t").expect("tree");
    assert_eq!(kind(t.branch::<f64>("id")), ErrorKind::TypeMismatch);
    assert_eq!(kind(t.branch::<i32>("missing")), ErrorKind::NotFound);
    assert_eq!(kind(t.read("missing", ..)), ErrorKind::NotFound);
    assert_eq!(kind(t.read("id", 40..60)), ErrorKind::InvalidInput);
    assert_eq!(kind(t.read("id", 60..)), ErrorKind::InvalidInput);
    assert_eq!(t.read("id", 50..50).map(|c| c.len()).expect("empty"), 0);

    let nt = file.rntuple("nt").expect("rntuple");
    assert_eq!(kind(nt.field::<f64>("id")), ErrorKind::TypeMismatch);
    assert_eq!(kind(nt.field::<i32>("missing")), ErrorKind::NotFound);
    assert_eq!(kind(nt.read("id", 0..51)), ErrorKind::InvalidInput);

    // A row type whose names are not in the tree.
    assert!(t.rows::<Other>().is_err());
    assert!(nt.rows::<Other>().is_err());
}

#[test]
fn trees_and_rntuples_read_back_typed_and_untyped() {
    let dir = TempDir::new("back");
    let path = dir.path("back.root");
    write_data(&path);
    let file = File::open(&path).expect("open");
    let want = events(50);
    let t = file.tree("t").expect("tree");
    let rows: Vec<Event> = t
        .rows::<Event>()
        .expect("rows")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(rows, want);
    let nt = file.rntuple("nt").expect("rntuple");
    let rows: Vec<Event> = nt
        .rows::<Event>()
        .expect("rows")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(rows, want);
    // Typed columns over a range, from both.
    let ids: Vec<i32> = t
        .branch::<i32>("id")
        .expect("id")
        .read(10..20)
        .expect("read");
    assert_eq!(ids, (10..20).collect::<Vec<_>>());
    let ids: Vec<i32> = nt
        .field::<i32>("id")
        .expect("id")
        .read(10..20)
        .expect("read");
    assert_eq!(ids, (10..20).collect::<Vec<_>>());
}

#[test]
fn read_limits_refuse_large_objects() {
    let dir = TempDir::new("limits");
    let path = dir.path("limits.root");
    let big = Hist1::<f64>::new("big", "", Axis::uniform(10_000, 0.0, 1.0).expect("axis"));
    rhizoid_io::create(&path, |f| f.put("big", &big)).expect("write");
    let tight = ReadOptions::default().limits(ReadLimits::untrusted().max_object_bytes(1024));
    let file = File::open_with(&path, tight).expect("open");
    assert_eq!(kind(file.get_dyn("big")), ErrorKind::LimitExceeded);
    // The default limits read it.
    let file = File::open(&path).expect("open");
    assert_eq!(
        file.get::<Hist1<f64>>("big").expect("big").values().len(),
        10_000
    );
}

#[test]
fn empty_writers_and_edge_strings_round_trip() {
    #[derive(Row, Clone, Debug, PartialEq)]
    struct Text {
        s: String,
    }
    let dir = TempDir::new("edges");
    let path = dir.path("edges.root");
    let texts: Vec<Text> = [
        "",
        "a",
        "é ü ✓ 漢字",
        &"x".repeat(254),
        &"y".repeat(255),
        &"z".repeat(256),
        &"w".repeat(70_000),
    ]
    .iter()
    .map(|s| Text { s: (*s).to_owned() })
    .collect();
    rhizoid_io::create(&path, |f| {
        f.tree::<Event>("empty_tree")?.finish()?;
        f.rntuple::<Event>("empty_nt")?.finish()?;
        let mut t = f.tree::<Text>("texts")?;
        t.extend(&texts)?;
        t.finish()?;
        let mut nt = f.rntuple::<Text>("texts_nt")?;
        nt.extend(&texts)?;
        nt.finish()
    })
    .expect("write");
    let file = File::open(&path).expect("open");
    assert!(file.check().expect("check").is_ok());
    assert_eq!(file.tree("empty_tree").expect("tree").num_entries(), 0);
    assert_eq!(file.rntuple("empty_nt").expect("nt").num_entries(), 0);
    assert_eq!(
        file.tree("empty_tree")
            .expect("tree")
            .rows::<Event>()
            .expect("rows")
            .count(),
        0
    );
    let back: Vec<Text> = file
        .tree("texts")
        .expect("tree")
        .rows::<Text>()
        .expect("rows")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(back, texts);
    let back: Vec<Text> = file
        .rntuple("texts_nt")
        .expect("nt")
        .rows::<Text>()
        .expect("rows")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(back, texts);
}

#[test]
fn reproducible_files_are_byte_identical() {
    let dir = TempDir::new("repro");
    // The same file name in two directories: the name is part of the file.
    let write = |sub: &str| {
        std::fs::create_dir_all(dir.path(sub)).expect("mkdir");
        let path = dir.path(sub).join("same.root");
        rhizoid_io::create_with(&path, WriteOptions::default().reproducible(true), |f| {
            f.put("h", &hist("h", 1.0))?;
            f.mkdir("d")?.put("n", &Named::new("n", "t"))?;
            let mut t = f.tree::<Event>("t")?;
            t.extend(&events(100))?;
            t.finish()?;
            let mut nt = f.rntuple::<Event>("nt")?;
            nt.extend(&events(100))?;
            nt.finish()
        })
        .expect("write");
        std::fs::read(path).expect("read")
    };
    let (a, b) = (write("a"), write("b"));
    assert!(a == b, "two reproducible writes differ");
}

#[test]
fn handles_are_shared_across_threads() {
    let dir = TempDir::new("threads");
    let path = dir.path("threads.root");
    rhizoid_io::create(&path, |f| {
        let mut t = f.tree_with::<Event>(
            "t",
            rhizoid_io::tree::TreeOptions::default().cluster_entries(500),
        )?;
        t.extend(&events(5_000))?;
        t.finish()
    })
    .expect("write");
    let file = File::open(&path).expect("open");
    let tree = file.tree("t").expect("tree");
    let want: Vec<Event> = events(5_000);
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|k| {
                let (file, tree) = (file.clone(), tree.clone());
                s.spawn(move || {
                    // Each thread reads a different window, and all of it.
                    let lo = k * 500;
                    let ids: Vec<i32> = tree
                        .branch::<i32>("id")
                        .expect("id")
                        .read(lo..lo + 500)
                        .expect("read");
                    let rows: Vec<Event> = file
                        .tree("t")
                        .expect("tree")
                        .rows::<Event>()
                        .expect("rows")
                        .map(|r| r.expect("row"))
                        .collect();
                    (lo, ids, rows)
                })
            })
            .collect();
        for h in handles {
            let (lo, ids, rows) = h.join().expect("thread");
            let lo = i32::try_from(lo).expect("small");
            assert_eq!(ids, (lo..lo + 500).collect::<Vec<_>>());
            assert_eq!(rows, want);
        }
    });
}

#[test]
fn the_writer_refuses_what_it_cannot_write() {
    let dir = TempDir::new("refuse");
    // A failing scope returns its error, and the file is still readable.
    let path = dir.path("refuse.root");
    let r = rhizoid_io::create(&path, |f| {
        f.put("ok", &Named::new("ok", "kept"))?;
        f.put("a/b", &Named::new("a/b", "a path is not a name"))
    });
    assert!(r.is_err(), "a key name with a slash");
    let file = File::open(&path).expect("open");
    assert_eq!(file.get::<Named>("ok").expect("ok").title(), "kept");

    for bad in ["", "x;1"] {
        let path = dir.path("bad.root");
        let r = rhizoid_io::create(&path, |f| f.put(bad, &Named::new(bad, "")));
        assert!(r.is_err(), "key name {bad:?}");
    }
    // A compression the writer cannot produce is refused before the file
    // is touched: an existing file keeps its contents.
    let path = dir.path("kept.root");
    rhizoid_io::create(&path, |f| f.put("n", &Named::new("n", "before"))).expect("write");
    let lzma = rhizoid_io::Compression::try_from(205).expect("LZMA level 5");
    let r = rhizoid_io::create_with(&path, WriteOptions::default().compression(lzma), |_| Ok(()));
    assert_eq!(kind(r), ErrorKind::Unsupported);
    assert_eq!(
        File::open(&path)
            .expect("open")
            .get::<Named>("n")
            .expect("n")
            .title(),
        "before"
    );

    let path = dir.path("twice.root");
    let r = rhizoid_io::create(&path, |f| {
        let mut a = f.tree::<Event>("t")?;
        let mut b = f.tree::<Event>("t")?;
        a.fill(&events(1)[0])?;
        b.fill(&events(1)[0])?;
        a.finish()?;
        b.finish()
    });
    // Two trees of one name: refused, or two cycles that both read.
    if r.is_ok() {
        let file = File::open(&path).expect("open");
        assert_eq!(file.keys().filter(|k| k.name() == "t").count(), 2);
    }
}

/// Every handle and value of the API can cross threads; a writer can be
/// moved to one. (An auto trait lost is a breaking change: this keeps it
/// from happening by accident.)
#[test]
fn public_types_are_send_and_sync() {
    fn send_sync<T: Send + Sync + 'static>() {}
    fn send<T: Send + 'static>() {}
    send_sync::<rhizoid_io::Error>();
    send_sync::<rhizoid_io::columnar::Column>();
    send_sync::<rhizoid_io::DynObject>();
    send_sync::<rhizoid_io::Value>();
    send_sync::<rhizoid_io::Array>();
    send_sync::<File>();
    send_sync::<rhizoid_io::Directory>();
    send_sync::<rhizoid_io::Tree>();
    send_sync::<rhizoid_io::RNTuple>();
    send_sync::<rhizoid_io::CheckReport>();
    send_sync::<ReadOptions>();
    send_sync::<Hist1<f64>>();
    send_sync::<Axis>();
    send_sync::<Graph>();
    send_sync::<Named>();
    send::<rhizoid_io::WritableFile>();
}

/// A directory handle belongs to the file that made it.
#[test]
fn a_directory_of_one_file_is_refused_by_another() {
    let mut a =
        rhizoid_io::WritableFile::to_sink(Box::new(Vec::new()), "a.root", WriteOptions::default())
            .expect("a");
    let mut b =
        rhizoid_io::WritableFile::to_sink(Box::new(Vec::new()), "b.root", WriteOptions::default())
            .expect("b");
    let sub = a.mkdir("sub").expect("mkdir");
    a.put_in(sub, "n", &Named::new("n", "in a"))
        .expect("its own directory");
    assert_eq!(
        kind(b.put_in(sub, "n", &Named::new("n", "t"))),
        ErrorKind::InvalidInput
    );
    // The root is every file's.
    a.put_in(rhizoid_io::DirId::ROOT, "top", &Named::new("top", "t"))
        .expect("root of a");
    b.put_in(rhizoid_io::DirId::ROOT, "top", &Named::new("top", "t"))
        .expect("root of b");
}
