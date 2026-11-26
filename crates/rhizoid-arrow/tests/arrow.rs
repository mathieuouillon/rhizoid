//! Arrow conversion on uproot's tree fixture and (optionally) the corpus.

use std::path::{Path, PathBuf};

use arrow_array::Array as _;
use arrow_array::cast::AsArray;
use arrow_array::types::{Float32Type, Int32Type};
use arrow_schema::DataType;
use rhizoid_arrow::{RecordBatches, TreeExt, to_arrow};
use rhizoid_io::columnar::Jagged;
use rhizoid_io::tree::BatchSize;
use rhizoid_io::{ErrorKind, File};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../rhizoid-io/tests/data/uproot_trees.root")
}

#[test]
fn record_batches_have_arrow_types_and_values() {
    let file = File::open(fixture()).expect("open");
    let tree = file.tree("events").expect("tree");
    let batches: Vec<_> = tree
        .record_batches(["b", "u64", "xyz", "jet_pt", "jet_q"], BatchSize::Cluster)
        .expect("batches")
        .collect::<rhizoid_io::Result<_>>()
        .expect("read");
    assert_eq!(batches.len(), 3);
    let schema = batches[0].schema();
    let types: Vec<DataType> = schema
        .fields()
        .iter()
        .map(|f| f.data_type().clone())
        .collect();
    assert_eq!(types[0], DataType::Boolean);
    assert_eq!(types[1], DataType::UInt64);
    assert!(
        matches!(&types[2], DataType::FixedSizeList(f, 3) if f.data_type() == &DataType::Float32)
    );
    assert!(matches!(&types[3], DataType::List(f) if f.data_type() == &DataType::Float32));
    assert_eq!(
        batches
            .iter()
            .map(arrow_array::RecordBatch::num_rows)
            .sum::<usize>(),
        1001
    );

    let jets = tree
        .branch::<Jagged<f32>>("jet_pt")
        .expect("b")
        .read(0..600)
        .expect("read");
    let list = batches[0].column(3).as_list::<i32>();
    for (i, entry) in jets.iter().enumerate() {
        let values = list.value(i);
        assert_eq!(
            values.as_primitive::<Float32Type>().values().as_ref(),
            entry
        );
    }
    let q = batches[0].column(4).as_list::<i32>().value(3);
    assert_eq!(
        q.as_primitive::<Int32Type>().values().as_ref(),
        [-3, -4, -5]
    );
}

#[test]
fn readers_with_selections_convert_too() {
    let file = File::open(fixture()).expect("open");
    let tree = file.tree("events").expect("tree");
    let reader = tree
        .reader()
        .projection("f64")
        .expect("p")
        .selection(10..20)
        .expect("s")
        .build();
    let batches: Vec<_> = RecordBatches::from(reader)
        .collect::<rhizoid_io::Result<_>>()
        .expect("read");
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].num_rows(), 10);
}

/// Every readable branch and every `RNTuple` field of the corpus converts.
#[test]
fn corpus_converts() {
    let Ok(dir) = std::env::var("RHIZOID_CORPUS") else {
        return;
    };
    let (mut ok, mut unsupported) = (0, Vec::new());
    for entry in std::fs::read_dir(dir).expect("dir") {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|x| x != "root") {
            continue;
        }
        let file = File::open(&path).expect("open");
        let names: Vec<String> = file
            .keys()
            .filter(|k| matches!(k.class_name(), "TTree" | "TNtuple" | "TNtupleD"))
            .map(|k| k.name().to_owned())
            .collect();
        let mut ntuples: Vec<String> = file
            .keys()
            .filter(|k| k.class_name() == "ROOT::RNTuple")
            .map(|k| k.name().to_owned())
            .collect();
        ntuples.dedup();
        for name in ntuples {
            let nt = file.rntuple(&name).expect("rntuple");
            for f in nt.fields() {
                let col = nt
                    .read(f.name(), 0..nt.num_entries().min(50))
                    .expect("read");
                match to_arrow(&col) {
                    Ok(a) => {
                        assert_eq!(a.len(), col.len(), "{}", f.name());
                        ok += 1;
                    }
                    Err(e) => unsupported.push(format!("{name}:{}: {e}", f.name())),
                }
            }
        }
        for name in names {
            let tree = file.tree(&name).expect("tree");
            for b in tree.readable_branches() {
                let col = match tree.read(b.name(), 0..b.num_entries().min(50)) {
                    Ok(c) => c,
                    Err(e) if e.kind() == ErrorKind::Unsupported => {
                        unsupported.push(format!("{}: {e}", b.name()));
                        continue;
                    }
                    Err(e) => unreachable!("{}: {e}", b.name()),
                };
                match to_arrow(&col) {
                    Ok(a) => {
                        assert_eq!(a.len(), col.len(), "{}", b.name());
                        ok += 1;
                    }
                    Err(e) if e.kind() == ErrorKind::Unsupported => {
                        unsupported.push(format!("{}: {e}", b.name()));
                    }
                    Err(e) => unreachable!("{}: {e}", b.name()),
                }
            }
        }
    }
    assert!(ok > 4000, "only {ok} branches converted");
    assert!(unsupported.is_empty(), "{unsupported:#?}");
}
