//! Objects end to end: read uproot's histograms, write ours and read them
//! back, and (optionally) cross-check both directions with uproot.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rhizoid_io::objects::{Axis, Hist1, Hist2, StringObject};
use rhizoid_io::{DynObject, ErrorKind, File, Value, WritableFile, WriteOptions};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("rhizoid-obj-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&p).expect("temp dir");
        Self(p)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Written by `tests/cross/uproot_hists.py` (uproot 5.7, uncompressed).
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/uproot_hists.root")
}

fn counting(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| f64::from(u32::try_from(i).expect("small")))
        .collect()
}

#[test]
fn reads_uproot_histograms() {
    let file = File::open(fixture()).expect("open");

    let h: Hist1<f64> = file.get("h_d").expect("TH1D");
    assert_eq!(h.title(), "a TH1D");
    assert_eq!(h.values_flow(), counting(12));
    assert_eq!(h.values(), &counting(12)[1..11]);
    assert_eq!(
        h.sumw2(),
        counting(12).iter().map(|v| v * 2.0).collect::<Vec<_>>()
    );
    assert_eq!(h.edges(), counting(11));
    assert_eq!(h.x_axis().title(), "x");
    let stats = h.stats();
    assert_eq!(
        (stats.entries, stats.tsumw, stats.tsumwx, stats.tsumwx2),
        (66.0, 60.0, 330.0, 2100.0)
    );

    let f: Hist1<f32> = file.get("h_f;1").expect("TH1F");
    assert_eq!(f.values().len(), 10);
    let i: Hist1<i32> = file.get("h_i").expect("TH1I");
    assert_eq!(i.values_flow().last(), Some(&11));

    let h2: Hist2<f64> = file.get("h2_d").expect("TH2D");
    assert_eq!(h2.values_flow(), counting(84));
    assert_eq!(h2.y_axis().edges(), [-1.0, -0.5, 0.0, 0.25, 0.5, 1.0]);
    assert!(h2.y_axis().is_variable());
    // In-range cell (0, 0) is global bin 1 + 12·1.
    assert_eq!(h2.values()[0], 13.0);
    let _: Hist2<f32> = file.get("h2_f").expect("TH2F");

    let note: StringObject = file.get("note").expect("TObjString");
    assert_eq!(note.as_str(), "hello rhizoid");

    let e = file.get::<Hist1<f64>>("h_f").expect_err("wrong class");
    assert_eq!(e.kind(), ErrorKind::ClassMismatch);
    assert_eq!(e.object_path(), Some("h_f"));
    assert_eq!(
        file.get_dyn("nope").expect_err("absent").kind(),
        ErrorKind::NotFound
    );
    assert_eq!(file.diagnostics(), []);

    // Members are reachable by name through the bases.
    let obj = file.get_dyn("h_d").expect("dyn");
    assert_eq!(obj.f64("fEntries").expect("entries"), 66.0);
    assert_eq!(obj.get("fLineColor"), Some(&Value::I16(602)));
    assert_eq!(obj.base("TH1").map(DynObject::class), Some("TH1"));
}

#[test]
fn written_objects_read_back() {
    let dir = TempDir::new("write");
    let path = dir.path("hists.root");

    // Models built from data, as a histogram library would hand them over.
    let cells = counting(102);
    let mut pt = Hist1::<f64>::new("h_pt", "p_{T}", Axis::uniform(100, 0.0, 200.0).expect("x"))
        .with_values_flow(cells.clone())
        .expect("cells")
        .with_sumw2(cells.iter().map(|c| c * 0.5).collect())
        .expect("sumw2");
    pt.x_axis_mut().set_title("p_{T} [GeV]");
    pt.stats_mut().entries = 5.0;
    pt.stats_mut().tsumwx = 92.8;
    let eta = Hist2::<f32>::new(
        "h_eta_phi",
        "#eta vs #phi",
        Axis::uniform(10, -2.5, 2.5).expect("x"),
        Axis::variable(&[-3.2, -1.0, 0.0, 1.0, 3.2]).expect("y"),
    )
    .with_values_flow((0..72_u8).map(f32::from).collect())
    .expect("cells");
    let mut counts = Hist1::<i32>::new("counts", "", Axis::uniform(3, 0.0, 3.0).expect("x"))
        .with_values_flow(vec![0, 0, 1, 0, 0])
        .expect("cells");

    rhizoid_io::create(&path, |f| {
        f.put("h_pt", &pt)?;
        f.put("h_eta_phi", &eta)?;
        f.put("counts", &counts)?;
        f.put("note", &StringObject::new("v1.2"))?;
        // The same name again: a new cycle.
        counts.set_values_flow(vec![0, 0, 1, 1, 0])?;
        f.put("counts", &counts)
    })
    .expect("write");

    let file = File::open(&path).expect("open");
    let keys: Vec<String> = file
        .keys()
        .map(|k| format!("{};{} {}", k.name(), k.cycle(), k.class_name()))
        .collect();
    assert_eq!(
        keys,
        [
            "h_pt;1 TH1D",
            "h_eta_phi;1 TH2F",
            "counts;1 TH1I",
            "note;1 TObjString",
            "counts;2 TH1I"
        ]
    );
    assert_eq!(
        file.root().key("h_pt", None).map(|k| k.title().to_owned()),
        Some("p_{T}".into())
    );

    let back: Hist1<f64> = file.get("h_pt").expect("h_pt");
    assert_eq!(back.values_flow(), pt.values_flow());
    assert_eq!(back.sumw2(), pt.sumw2());
    assert_eq!(back.stats(), pt.stats());
    assert_eq!(back.x_axis().title(), "p_{T} [GeV]");
    let back: Hist2<f32> = file.get("h_eta_phi").expect("h2");
    assert_eq!(back.values_flow(), eta.values_flow());
    assert_eq!(back.y_axis().edges(), [-3.2, -1.0, 0.0, 1.0, 3.2]);
    assert_eq!(
        file.get::<Hist1<i32>>("counts;1").expect("c1").values(),
        [0, 1, 0]
    );
    assert_eq!(
        file.get::<Hist1<i32>>("counts").expect("c2").values(),
        [0, 1, 1]
    );
    assert_eq!(
        file.get::<StringObject>("note").expect("note").as_str(),
        "v1.2"
    );

    let classes: Vec<String> = file
        .streamer_infos()
        .expect("infos")
        .iter()
        .map(|i| i.class_name().to_owned())
        .collect();
    for c in [
        "TH1D",
        "TH2F",
        "TH1I",
        "TObjString",
        "TH1",
        "TH2",
        "TAxis",
        "TNamed",
    ] {
        assert!(
            classes.iter().any(|x| x == c),
            "{c} not stored: {classes:?}"
        );
    }
    assert!(file.check().expect("check").is_ok());
    assert_eq!(file.diagnostics(), []);
}

#[test]
fn objects_read_from_a_file_are_written_back_unchanged() {
    let source = File::open(fixture()).expect("open");
    let dir = TempDir::new("copy");
    let path = dir.path("copy.root");
    let mut out = WritableFile::create(&path).expect("create");
    for name in ["h_d", "h_f", "h_i", "h2_d", "h2_f", "note"] {
        out.put(name, &source.get_dyn(name).expect("read"))
            .expect("put");
    }
    // Through a typed model too: the members it does not model survive.
    let mut h: Hist1<f64> = source.get("h_d").expect("typed");
    h.set_title("retitled");
    out.put("typed", &h).expect("put typed");
    out.close().expect("close");

    let copy = File::open(&path).expect("open copy");
    for name in ["h_d", "h_f", "h_i", "h2_d", "h2_f", "note"] {
        assert_eq!(
            copy.get_dyn(name).expect("reread"),
            source.get_dyn(name).expect("read"),
            "{name}"
        );
    }
    let mut expected = source.get_dyn("h_d").expect("read");
    expected.set("fTitle", "retitled");
    assert_eq!(copy.get_dyn("typed").expect("typed"), expected);
}

#[test]
fn reproducible_object_files_are_byte_identical() {
    let dir = TempDir::new("repro");
    let write = |sub: &str| {
        std::fs::create_dir_all(dir.path(sub)).expect("dir");
        let path = dir.path(sub).join("h.root");
        let h = Hist1::<f64>::new("h", "t", Axis::uniform(10, 0.0, 1.0).expect("x"));
        rhizoid_io::create_with(&path, WriteOptions::default().reproducible(true), |f| {
            f.put("h", &h)
        })
        .expect("write");
        std::fs::read(path).expect("read")
    };
    assert_eq!(write("a"), write("b"));
}

#[test]
fn put_rejects_what_it_cannot_write() {
    let dir = TempDir::new("reject");
    let mut out = WritableFile::create(dir.path("x.root")).expect("create");
    let unknown = DynObject::new("MyClass").with("fX", 1);
    assert_eq!(
        out.put("x", &unknown).expect_err("unknown class").kind(),
        ErrorKind::Unsupported
    );
    let typo = DynObject::new("TObjString").with("fStrin", "x");
    assert_eq!(
        out.put("x", &typo).expect_err("typo").kind(),
        ErrorKind::InvalidInput
    );
    let wrong = DynObject::new("TObjString").with("fString", 3.5);
    assert_eq!(
        out.put("x", &wrong).expect_err("wrong type").kind(),
        ErrorKind::InvalidInput
    );
    let note = StringObject::new("ok");
    for bad in ["", "a/b", "a;1"] {
        assert_eq!(
            out.put(bad, &note).expect_err("bad name").kind(),
            ErrorKind::InvalidPath
        );
    }
    // A failed put leaves the file writable.
    out.put("ok", &note).expect("put");
    out.close().expect("close");
    let file = File::open(dir.path("x.root")).expect("open");
    assert_eq!(file.keys().count(), 1);
}

/// `KEY FIELD VALUE...` lines from `uproot_dump.py`.
fn uproot_dump(python: &str, file: &Path) -> HashMap<(String, String), String> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/cross/uproot_dump.py");
    let out = std::process::Command::new(python)
        .arg(script)
        .arg(file)
        .output()
        .expect("run uproot_dump.py");
    assert!(
        out.status.success(),
        "uproot failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut parts = l.splitn(3, ' ');
            let key = parts.next()?.to_owned();
            let field = parts.next()?.to_owned();
            Some(((key, field), parts.next().unwrap_or_default().to_owned()))
        })
        .collect()
}

fn floats(s: &str) -> Vec<f64> {
    s.split_whitespace()
        .map(|x| x.parse().expect("float"))
        .collect()
}

fn field<'a>(dump: &'a HashMap<(String, String), String>, key: &str, f: &str) -> &'a str {
    dump.get(&(key.to_owned(), f.to_owned()))
        .unwrap_or_else(|| unreachable!("uproot reported no {f} for {key}"))
}

#[test]
fn uproot_cross_check_objects() {
    let Ok(python) = std::env::var("RHIZOID_ORACLE_PYTHON") else {
        return;
    };

    // uproot wrote the fixture: we read what uproot reads.
    let theirs = uproot_dump(&python, &fixture());
    let file = File::open(fixture()).expect("open");
    for (key, values, sumw2) in [
        (
            "h_d;1",
            file.get::<Hist1<f64>>("h_d")
                .expect("h")
                .values_flow()
                .to_vec(),
            file.get::<Hist1<f64>>("h_d").expect("h").sumw2().to_vec(),
        ),
        (
            "h2_d;1",
            file.get::<Hist2<f64>>("h2_d")
                .expect("h")
                .values_flow()
                .to_vec(),
            file.get::<Hist2<f64>>("h2_d").expect("h").sumw2().to_vec(),
        ),
    ] {
        assert_eq!(floats(field(&theirs, key, "values")), values, "{key}");
        assert_eq!(floats(field(&theirs, key, "sumw2")), sumw2, "{key}");
    }

    // We write: uproot reads our histograms identically.
    let dir = TempDir::new("oracle-obj");
    let path = dir.path("ours.root");
    let mut state = 0x2545_f491_u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        f64::from(state % 2400) / 100.0
    };
    let mut h1 = Hist1::<f64>::new("h1", "one", Axis::uniform(20, -1.0, 1.0).expect("x"))
        .with_values_flow((0..22).map(|_| next()).collect())
        .expect("cells")
        .with_sumw2((0..22).map(|_| next()).collect())
        .expect("sumw2");
    *h1.stats_mut() = {
        let mut s = rhizoid_io::objects::HistStats::default();
        s.entries = 1000.0;
        s.tsumw = next();
        s
    };
    let h2 = Hist2::<f64>::new(
        "h2",
        "two",
        Axis::uniform(4, 0.0, 4.0).expect("x").with_title("x"),
        Axis::variable(&[0.0, 0.5, 2.0]).expect("y").with_title("y"),
    )
    .with_values_flow((0..24).map(|_| next()).collect())
    .expect("cells");
    #[expect(clippy::cast_possible_truncation, reason = "test values fit")]
    let hf = Hist1::<f32>::new(
        "hf",
        "f",
        Axis::variable(&[0.0, 0.1, 1.0, 10.0]).expect("x"),
    )
    .with_values_flow((0..5).map(|_| next() as f32).collect())
    .expect("cells");
    rhizoid_io::create(&path, |f| {
        f.put("h1", &h1)?;
        f.put("h2", &h2)?;
        f.put("hf", &hf)?;
        f.put("s", &StringObject::new("from rhizoid"))
    })
    .expect("write");
    let seen = uproot_dump(&python, &path);
    assert_eq!(field(&seen, "h1;1", "class"), "TH1D");
    assert_eq!(field(&seen, "h1;1", "title"), "one");
    assert_eq!(floats(field(&seen, "h1;1", "values")), h1.values_flow());
    assert_eq!(floats(field(&seen, "h1;1", "sumw2")), h1.sumw2());
    assert_eq!(floats(field(&seen, "h1;1", "edges0")), h1.edges());
    assert_eq!(floats(field(&seen, "h1;1", "fEntries")), [h1.entries()]);
    assert_eq!(floats(field(&seen, "h1;1", "fTsumw")), [h1.stats().tsumw]);
    assert_eq!(floats(field(&seen, "h2;1", "values")), h2.values_flow());
    assert_eq!(floats(field(&seen, "h2;1", "edges1")), [0.0, 0.5, 2.0]);
    assert_eq!(field(&seen, "h2;1", "axistitle1"), "y");
    let hf_values: Vec<f64> = hf.values_flow().iter().copied().map(f64::from).collect();
    assert_eq!(floats(field(&seen, "hf;1", "values")), hf_values);
    assert_eq!(field(&seen, "s;1", "string"), "from rhizoid");
}

mod corrupted {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(300))]

        /// Flipped bytes inside object records give errors, never panics or
        /// runaway allocations.
        #[test]
        fn decoding_corrupted_objects_never_panics(
            flips in proptest::collection::vec((0x0d8_usize..0x8ca0, any::<u8>()), 1..6),
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
            let names: Vec<String> = file.keys().map(|k| k.name().to_owned()).collect();
            for name in names {
                let _ = file.get_dyn(&name);
                let _ = file.get::<Hist1<f64>>(&name);
            }
        }
    }
}
