//! The rows `write_rows` writes, shared with the differential check
//! (`examples/differential.rs`), which has ROOT and uproot read the same
//! files.

use std::fmt::Write as _;

use arbitrary::Arbitrary;
use rhizoid_io::ntuple::RNTupleOptions;
use rhizoid_io::tree::TreeOptions;
use rhizoid_io::{Algorithm, Compression, Row, WritableFile, WriteOptions};

use crate::Shared;

/// One row: every kind of leaf a flat tree holds.
#[derive(Arbitrary, Row, Clone, Debug)]
pub struct Event {
    /// `flag/O`.
    pub flag: bool,
    /// `small/B`.
    pub small: i8,
    /// `id/I`.
    pub id: i32,
    /// `big/l`.
    pub big: u64,
    /// `x/D`.
    pub x: f64,
    /// `y/F`.
    pub y: f32,
    /// `xyz[3]/s`.
    pub xyz: [u16; 3],
    /// `hits[n]/F`.
    #[row(count = "n")]
    pub hits: Vec<f32>,
    /// `label/C`.
    pub label: String,
}

/// How to write them.
#[derive(Arbitrary, Debug)]
pub struct Input {
    /// The rows (the first 256).
    pub rows: Vec<Event>,
    /// An `RNTuple` rather than a tree.
    pub rntuple: bool,
    /// The codec, modulo 4: zlib, LZ4, and two the writer refuses.
    pub algorithm: u8,
    /// The level, modulo 10.
    pub level: u8,
    /// Entries per tree cluster.
    pub cluster_entries: u8,
    /// Bytes per `RNTuple` cluster.
    pub cluster_bytes: u16,
    /// Bytes per `RNTuple` page.
    pub page_bytes: u16,
}

/// Whether two rows are the same, floats by their bits.
pub fn same(a: &Event, b: &Event) -> bool {
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

/// Writes the input's rows as `t`: the rows and the file, or `None` when
/// its compression is one the writer refuses (as it must for those).
///
/// # Panics
///
/// If writing fails otherwise: that is a bug the fuzzer reports.
pub fn write(input: &Input) -> Option<(Vec<Event>, Vec<u8>)> {
    let rows: Vec<Event> = input.rows.iter().take(256).cloned().collect();
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
            return None;
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
    Some((rows, sink.bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The rows as the differential check prints them, every reader alike:
/// `entry field value` lines, floats as their bits in hex, strings as
/// their bytes in hex.
pub fn dump(rows: &[Event]) -> String {
    let mut s = String::new();
    for (i, r) in rows.iter().enumerate() {
        let hits: Vec<String> = r
            .hits
            .iter()
            .map(|h| format!("{:08x}", h.to_bits()))
            .collect();
        let _ = writeln!(s, "{i} flag {}", u8::from(r.flag));
        let _ = writeln!(s, "{i} small {}", r.small);
        let _ = writeln!(s, "{i} id {}", r.id);
        let _ = writeln!(s, "{i} big {}", r.big);
        let _ = writeln!(s, "{i} x {:016x}", r.x.to_bits());
        let _ = writeln!(s, "{i} y {:08x}", r.y.to_bits());
        let _ = writeln!(s, "{i} xyz {} {} {}", r.xyz[0], r.xyz[1], r.xyz[2]);
        let _ = writeln!(s, "{i} hits {} {}", r.hits.len(), hits.join(" "));
        let _ = writeln!(s, "{i} label {}", hex(r.label.as_bytes()));
    }
    s
}
