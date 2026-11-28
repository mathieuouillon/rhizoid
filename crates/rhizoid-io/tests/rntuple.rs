//! `RNTuple` reading: uproot's fixture with known values, and (optionally)
//! every field of every `RNTuple` in the corpus against uproot.

mod support;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use rhizoid_io::columnar::{Column, Jagged};
use rhizoid_io::{DynObject, ErrorKind, File, Value};
use support::leaves::column_leaves;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/uproot_rntuple.root")
}

#[test]
fn reads_uproot_rntuple() {
    let file = File::open(fixture()).expect("open");
    let nt = file.rntuple("events").expect("rntuple");
    assert_eq!(nt.num_entries(), 500);
    assert_eq!(nt.clusters(), [0..300, 300..500]);
    assert_eq!(nt.format_version()[0], 1);

    let i32s = nt.field::<i32>("i32").expect("i32").read(..).expect("read");
    assert!(i32s.iter().zip(0_i32..).all(|(&v, i)| v == i * 7 - 100));
    let x = nt
        .field::<f64>("x")
        .expect("x")
        .read(295..305)
        .expect("read");
    assert!((x[0] - 147.5).abs() < f64::EPSILON);
    assert_eq!(x.len(), 10);
    let flags = nt
        .field::<bool>("flag")
        .expect("flag")
        .read(..)
        .expect("read");
    assert!(flags.iter().enumerate().all(|(i, &f)| f == (i % 2 == 0)));
    let labels = nt
        .field::<String>("label")
        .expect("label")
        .read(298..302)
        .expect("read");
    assert_eq!(labels, ["entry298", "entry299", "entry300", "entry301"]);

    // Jagged across the cluster boundary: entry i has i % 3 values,
    // counting from the start of its chunk (0 and 300).
    let jets = nt
        .field::<Jagged<f32>>("jet_pt")
        .expect("jets")
        .read(298..303)
        .expect("read");
    let lens: Vec<usize> = jets.iter().map(<[f32]>::len).collect();
    assert_eq!(lens, [1, 2, 0, 1, 2]);
    assert_eq!(jets.get(3), Some(&[0.0][..]));
    let ids = nt
        .field::<Jagged<i64>>("ids")
        .expect("ids")
        .read(..)
        .expect("read");
    assert_eq!(ids.len(), 500);

    assert_eq!(
        nt.field::<f32>("x").expect_err("type").kind(),
        ErrorKind::TypeMismatch
    );
    assert_eq!(
        nt.field::<f64>("nope").expect_err("absent").kind(),
        ErrorKind::NotFound
    );
    assert_eq!(
        file.rntuple("nope").expect_err("absent").kind(),
        ErrorKind::NotFound
    );
    assert!(matches!(nt.read("label", ..).expect("read"), Column::Strings(s) if s.len() == 500));
}

/// uproot lists a class's own members first, then its base classes in
/// reverse order (`:_1` before `:_0`), each the same way; rhizoid keeps
/// ROOT's order (bases first, in order). Reorder for the comparison.
fn uproot_order(v: Value) -> Value {
    match v {
        Value::Object(o) => {
            let mut out = DynObject::new(o.class());
            for (name, m) in o.members() {
                out.set(name, uproot_order(m.clone()));
            }
            for (i, b) in o.bases().iter().enumerate().rev() {
                out.set(
                    format!(":_{i}"),
                    uproot_order(Value::Object(Box::new(b.clone()))),
                );
            }
            Value::Object(Box::new(out))
        }
        Value::List(items) => Value::List(items.into_iter().map(uproot_order).collect()),
        v => v,
    }
}

type Oracle = HashMap<(String, String), (String, Vec<String>)>;

/// `(ntuple, field) → (counts, leaves)` from uproot.
fn uproot(python: &str, file: &Path, names: &[String]) -> Oracle {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/cross/uproot_leaves_rntuple.py");
    let out = std::process::Command::new(python)
        .arg(script)
        .arg(file)
        .arg("300")
        .args(names)
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "{}: {}",
        file.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let mut map = Oracle::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut it = line.split('\t');
        let (Some(n), Some(f), Some(kind)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let rest: Vec<String> = it.map(str::to_owned).collect();
        let key = (n.to_owned(), f.to_owned());
        match kind {
            "counts" => {
                map.insert(key, (rest.join(" "), Vec::new()));
            }
            "leaves" => {
                let counts = map.remove(&key).map(|(c, _)| c).unwrap_or_default();
                map.insert(
                    key,
                    (counts, rest.into_iter().filter(|x| !x.is_empty()).collect()),
                );
            }
            _ => {
                map.insert(key, ("error".to_owned(), rest));
            }
        }
    }
    map
}

#[derive(Default)]
struct Tally {
    ntuples: usize,
    matched: usize,
    unverified: usize,
    unsupported: Vec<String>,
    differs: Vec<String>,
}

fn compare_file(python: &str, path: &Path, t: &mut Tally) {
    let file = File::open(path).expect("open");
    let mut names: Vec<String> = file
        .keys()
        .filter(|k| k.class_name() == "ROOT::RNTuple")
        .map(|k| k.name().to_owned())
        .collect();
    names.dedup();
    if names.is_empty() {
        return;
    }
    let theirs = uproot(python, path, &names);
    let fname = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for name in &names {
        t.ntuples += 1;
        let nt = match file.rntuple(name) {
            Ok(nt) => nt,
            Err(e) => {
                t.differs.push(format!("{fname}:{name}: cannot open: {e}"));
                continue;
            }
        };
        for field in nt.fields() {
            let label = format!("{fname}:{name}:{}", field.name());
            let Some((counts, values)) = theirs.get(&(name.clone(), field.name().to_owned()))
            else {
                continue;
            };
            if let Err(e) = field.interpretation() {
                t.unsupported.push(format!("{label}: {e}"));
                continue;
            }
            let col = match nt.read(field.name(), 0..nt.num_entries().min(300)) {
                Ok(c) => c,
                Err(e) => {
                    t.differs.push(format!("{label}: read failed: {e}"));
                    continue;
                }
            };
            if counts == "error" {
                t.unverified += 1;
                continue;
            }
            let col = match col {
                Column::Objects(v) => Column::Objects(v.into_iter().map(uproot_order).collect()),
                c => c,
            };
            let Some(ours) = column_leaves(&col) else {
                t.unsupported.push(format!("{label}: undecoded values"));
                continue;
            };
            let their_counts: Vec<usize> =
                counts.split(' ').filter_map(|c| c.parse().ok()).collect();
            let our_counts: Vec<usize> = ours.iter().map(Vec::len).collect();
            let flat: Vec<_> = ours.iter().flatten().collect();
            if our_counts != their_counts {
                t.differs.push(format!(
                    "{label}: leaves per entry differ: ours {:?}…, uproot {:?}…",
                    &our_counts[..our_counts.len().min(6)],
                    &their_counts[..their_counts.len().min(6)]
                ));
            } else if let Some(i) = flat
                .iter()
                .zip(values)
                .position(|(o, th)| !o.matches(th, 0.0))
            {
                t.differs.push(format!(
                    "{label}: leaf {i}: ours {:?}, uproot {}",
                    flat[i], values[i]
                ));
            } else {
                t.matched += 1;
            }
        }
    }
}

#[test]
fn rntuples_match_uproot() {
    let Ok(python) = std::env::var("RHIZOID_ORACLE_PYTHON") else {
        return;
    };
    let mut files = vec![fixture()];
    if let Ok(dir) = std::env::var("RHIZOID_CORPUS") {
        let mut more: Vec<PathBuf> = std::fs::read_dir(dir)
            .expect("dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "root"))
            .collect();
        more.sort();
        files.extend(more);
    }
    let mut t = Tally::default();
    for path in &files {
        compare_file(&python, path, &mut t);
    }
    let mut report = format!(
        "{} RNTuples: {} fields match uproot, {} unsupported, {} read where uproot cannot, {} differ\n",
        t.ntuples,
        t.matched,
        t.unsupported.len(),
        t.unverified,
        t.differs.len()
    );
    for u in &t.unsupported {
        let _ = writeln!(report, "unsupported: {u}");
    }
    for d in &t.differs {
        let _ = writeln!(report, "DIFFERS: {d}");
    }
    if let Ok(out) = std::env::var("RHIZOID_RNTUPLE_REPORT") {
        std::fs::write(out, &report).expect("report");
    }
    assert!(t.differs.is_empty(), "{report}");
}

mod corrupted {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        /// Flipped bytes in pages and metadata give errors, never panics or
        /// runaway allocations.
        #[test]
        fn reading_corrupted_rntuples_never_panics(
            flips in proptest::collection::vec((0x100_usize..12_400, any::<u8>()), 1..8),
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
            let Ok(nt) = file.rntuple("events") else { return Ok(()) };
            let names: Vec<String> = nt.fields().iter().map(|f| f.name().to_owned()).collect();
            for name in names {
                let _ = nt.read(&name, ..);
            }
        }
    }
}

/// Packed reals as ROOT writes them (`tests/cross/root_ntuple_packed.C`):
/// quantized to a range, truncated, and in half precision; and a field
/// added late.
#[test]
fn packed_reals_read_as_root_wrote_them() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/root_ntuple_packed.root");
    let file = File::open(path).expect("open");
    let nt = file.rntuple("nt").expect("rntuple");
    let floats = |name: &str| nt.field::<f32>(name).expect(name).read(..).expect(name);
    let close = |got: &[f64], want: &[f64], within: f64| {
        assert_eq!(got.len(), want.len());
        for (g, w) in got.iter().zip(want) {
            assert!((g - w).abs() <= within, "{got:?} against {want:?}");
        }
    };
    let quant: Vec<f64> = floats("quant").into_iter().map(f64::from).collect();
    let want: Vec<f64> = (0..8).map(|i| -2.0 + f64::from(i)).collect();
    // 20 bits over a width of 8.
    close(&quant, &want, 8.0 / f64::from((1 << 20) - 1));
    let dquant = nt
        .field::<f64>("dquant")
        .expect("dquant")
        .read(..)
        .expect("dquant");
    let want: Vec<f64> = (0..8).map(|i| -1.0 + 0.25 * f64::from(i)).collect();
    close(&dquant, &want, 2.0 / f64::from(u32::MAX));
    // Values with short mantissas survive truncation and half precision.
    let want: Vec<f32> = (0..8u8).map(|i| 1.5 * f32::from(i)).collect();
    assert_eq!(floats("trunc"), want);
    let want: Vec<f32> = (0..8u8).map(|i| 0.5 * f32::from(i) - 1.0).collect();
    assert_eq!(floats("half"), want);
    // Added after four entries, in the footer's schema extension: the
    // entries before read it as zero.
    let late = nt
        .field::<i32>("late")
        .expect("late")
        .read(..)
        .expect("late");
    assert_eq!(late, [0, 0, 0, 0, 40, 50, 60, 70]);
}
