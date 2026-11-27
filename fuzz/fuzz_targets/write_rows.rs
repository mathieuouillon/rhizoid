//! The writing API: arbitrary rows written as a tree or an `RNTuple`, with
//! any codec, level and cluster size, must read back exactly (floats by
//! their bits), through typed rows.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use rhizoid_fuzz::Shared;
use rhizoid_io::ntuple::RNTupleOptions;
use rhizoid_io::tree::TreeOptions;
use rhizoid_io::{Algorithm, Compression, File, ReadOptions, Row, WritableFile, WriteOptions};

#[derive(Arbitrary, Row, Clone, Debug)]
struct Event {
    flag: bool,
    small: i8,
    id: i32,
    big: u64,
    x: f64,
    y: f32,
    xyz: [u16; 3],
    #[row(count = "n")]
    hits: Vec<f32>,
    label: String,
}

#[derive(Arbitrary, Debug)]
struct Input {
    rows: Vec<Event>,
    rntuple: bool,
    algorithm: u8,
    level: u8,
    cluster_entries: u8,
    cluster_bytes: u16,
    page_bytes: u16,
}

fn same(a: &Event, b: &Event) -> bool {
    a.flag == b.flag
        && a.small == b.small
        && a.id == b.id
        && a.big == b.big
        && a.x.to_bits() == b.x.to_bits()
        && a.y.to_bits() == b.y.to_bits()
        && a.xyz == b.xyz
        && a.hits.len() == b.hits.len()
        && a.hits
            .iter()
            .zip(&b.hits)
            .all(|(p, q)| p.to_bits() == q.to_bits())
        && a.label == b.label
}

fuzz_target!(|input: Input| {
    let rows: Vec<Event> = input.rows.into_iter().take(256).collect();
    // The writer produces zlib and LZ4; the codecs it only reads are refused
    // when the setting is made.
    let algorithm = match input.algorithm % 4 {
        0 => Algorithm::Zlib,
        1 => Algorithm::Lz4,
        2 => Algorithm::Zstd,
        _ => Algorithm::Lzma,
    };
    let level = input.level % 10;
    let compression = match Compression::new(algorithm, level) {
        Ok(c) => c,
        Err(e) => {
            assert!(
                matches!(algorithm, Algorithm::Zstd | Algorithm::Lzma) && level > 0,
                "{e}"
            );
            return;
        }
    };
    let sink = Shared::default();
    let options = WriteOptions::default()
        .compression(compression)
        .reproducible(true);
    let mut file =
        WritableFile::to_sink(Box::new(sink.clone()), "fuzz.root", options).expect("create");
    if input.rntuple {
        let options = RNTupleOptions::default()
            .cluster_bytes(usize::from(input.cluster_bytes).max(1))
            .page_bytes(usize::from(input.page_bytes).max(1));
        let mut w = file.rntuple_with::<Event>("t", options).expect("rntuple");
        w.extend(&rows).expect("fill");
        w.finish().expect("finish");
    } else {
        let options =
            TreeOptions::default().cluster_entries(u64::from(input.cluster_entries).max(1));
        let mut w = file.tree_with::<Event>("t", options).expect("tree");
        w.extend(&rows).expect("fill");
        w.finish().expect("finish");
    }
    file.close().expect("close");

    let file = File::from_source(sink.bytes(), ReadOptions::default()).expect("open");
    assert!(file.check().expect("check").is_ok());
    let back: Vec<Event> = if input.rntuple {
        let nt = file.rntuple("t").expect("rntuple");
        assert_eq!(nt.num_entries(), rows.len() as u64);
        nt.rows::<Event>()
            .expect("rows")
            .map(|r| r.expect("row"))
            .collect()
    } else {
        let tree = file.tree("t").expect("tree");
        assert_eq!(tree.num_entries(), rows.len() as u64);
        tree.rows::<Event>()
            .expect("rows")
            .map(|r| r.expect("row"))
            .collect()
    };
    assert_eq!(back.len(), rows.len());
    for (i, (a, b)) in rows.iter().zip(&back).enumerate() {
        assert!(same(a, b), "row {i}: wrote {a:?}, read {b:?}");
    }
});
