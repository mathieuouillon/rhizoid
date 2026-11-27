//! Inputs the fuzz targets (`fuzz/`) found, replayed on every test run: each
//! file in `tests/data/fuzz` is opened as a ROOT file and read in full, as
//! the `open_file` target reads it. Errors are fine; a panic, an abort or an
//! unbounded allocation is the bug the input once triggered.
//!
//! Fuzz builds skip checksum verification, so most findings carry checksums
//! that no longer match. Replay them as the fuzzer ran them, in a build
//! with `--cfg fuzzing` (as CI does); a normal build stops at the checksum:
//!
//! ```text
//! RUSTFLAGS="--cfg fuzzing" cargo test -p rhizoid-io --test fuzz_regressions
//! ```
//!
//! To add one: copy the artifact `cargo fuzz` wrote
//! (`fuzz/artifacts/<target>/crash-…`) here under a name saying what it hit.

#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;

use rhizoid_io::{Directory, File, ReadLimits, ReadOptions};

const MAX_ENTRIES: u64 = 4096;

fn read_everything(data: Vec<u8>) {
    let options = ReadOptions::default().limits(ReadLimits::untrusted().max_object_bytes(8 << 20));
    let Ok(file) = File::from_source(data, options) else {
        return;
    };
    let _ = file.diagnostics();
    let _ = file.streamer_infos();
    let _ = file.check();
    walk(&file, &file.root(), 0);
}

fn walk(file: &File, dir: &Directory, depth: usize) {
    let keys: Vec<(String, String, u16, bool)> = dir
        .keys()
        .map(|k| {
            (
                k.name().to_owned(),
                k.class_name().to_owned(),
                k.cycle(),
                k.is_directory(),
            )
        })
        .collect();
    for (name, class, cycle, is_dir) in keys.into_iter().take(64) {
        let path = if dir.path().is_empty() {
            format!("{name};{cycle}")
        } else {
            format!("{}/{name};{cycle}", dir.path())
        };
        let _ = dir.read_raw(&name);
        if is_dir {
            if depth < 4
                && let Ok(sub) = dir.subdir(&name)
            {
                walk(file, &sub, depth + 1);
            }
            continue;
        }
        let _ = file.get_dyn(&path);
        if matches!(class.as_str(), "TTree" | "TNtuple" | "TNtupleD")
            && let Ok(tree) = file.tree(&path)
        {
            let n = tree.num_entries().min(MAX_ENTRIES);
            for b in tree.readable_branches() {
                let _ = tree.read(b.name(), 0..n);
            }
        }
        if class == "ROOT::RNTuple"
            && let Ok(nt) = file.rntuple(&path)
        {
            let n = nt.num_entries().min(MAX_ENTRIES);
            for f in nt.fields() {
                let _ = nt.read(f.name(), 0..n);
                // A whole read too: the entry count is the file's claim.
                let _ = nt.read(f.name(), ..);
            }
        }
    }
}

#[test]
fn every_fuzz_finding_reads_without_crashing() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/fuzz");
    let mut n = 0;
    for entry in std::fs::read_dir(&dir).expect("tests/data/fuzz") {
        let path = entry.expect("entry").path();
        let data = std::fs::read(&path).expect("read");
        read_everything(data);
        n += 1;
    }
    assert!(n > 0, "no fuzz findings in {}", dir.display());
}
