//! The writing API: arbitrary rows written as a tree or an `RNTuple`, with
//! any codec, level and cluster size, must read back exactly (floats by
//! their bits), through typed rows. The corpus this grows is also read by
//! ROOT and uproot, nightly (`examples/differential.rs`).
#![no_main]

use libfuzzer_sys::fuzz_target;
use rhizoid_fuzz::rows::{Event, Input, same, write};
use rhizoid_io::{File, ReadOptions};

fuzz_target!(|input: Input| {
    let Some((rows, bytes)) = write(&input) else {
        return;
    };
    let file = File::from_source(bytes, ReadOptions::default()).expect("open");
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
