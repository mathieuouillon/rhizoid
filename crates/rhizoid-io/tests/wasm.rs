//! WASM (`wasm32-unknown-unknown`): the fixtures decode to exactly what they
//! decode to natively, through the sync API and through `LocalAsyncFile`.
//!
//! Natively, this file checks the digests recorded in
//! `tests/data/fingerprints.txt` (`RHIZOID_BLESS=1` rewrites them). On
//! WASM, run with `wasm-bindgen-test-runner` (see CI), it checks the same
//! digests.

#![cfg(feature = "async")]

use rhizoid_io::{File, ReadOptions};

const FIXTURES: [(&str, &[u8]); 3] = [
    (
        "uproot_trees.root",
        include_bytes!("data/uproot_trees.root"),
    ),
    (
        "uproot_hists.root",
        include_bytes!("data/uproot_hists.root"),
    ),
    (
        "uproot_rntuple.root",
        include_bytes!("data/uproot_rntuple.root"),
    ),
];

const RECORD: &str = include_str!("data/fingerprints.txt");

fn is_tree(class: &str) -> bool {
    matches!(class, "TTree" | "TNtuple" | "TNtupleD")
}

/// Keys, objects, and every column in full, as text.
fn fingerprint(file: &File) -> Vec<String> {
    let mut out = Vec::new();
    for k in file.keys() {
        out.push(k.to_string());
        let name = format!("{};{}", k.name(), k.cycle());
        match k.class_name() {
            c if is_tree(c) => {
                let tree = file.tree(&name).expect("tree");
                for b in tree.readable_branches() {
                    let col = tree.read(b.name(), ..);
                    out.push(format!(
                        "{}: {:?}",
                        b.name(),
                        col.map_err(|e| e.to_string())
                    ));
                }
            }
            "ROOT::RNTuple" => {
                let nt = file.rntuple(&name).expect("rntuple");
                for f in nt.fields() {
                    let col = nt.read(f.name(), ..);
                    out.push(format!(
                        "{}: {:?}",
                        f.name(),
                        col.map_err(|e| e.to_string())
                    ));
                }
            }
            "TDirectory" | "TDirectoryFile" => {}
            _ => out.push(format!(
                "{:?}",
                file.get_dyn(&name).map_err(|e| e.to_string())
            )),
        }
    }
    out
}

/// The digest of a fingerprint: its length and its XXH3.
fn digest(lines: &[String]) -> String {
    let text = lines.join("\n");
    format!(
        "{} {:016x}",
        lines.len(),
        xxhash_rust::xxh3::xxh3_64(text.as_bytes())
    )
}

fn recorded(name: &str) -> Option<&'static str> {
    RECORD
        .lines()
        .find_map(|l| l.strip_prefix(name)?.strip_prefix(' '))
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn native_digests_match_the_record() {
    use std::fmt::Write as _;
    let mut record = String::new();
    for (name, bytes) in FIXTURES {
        let file = File::from_source(bytes.to_vec(), ReadOptions::default()).expect("open");
        let d = digest(&fingerprint(&file));
        writeln!(record, "{name} {d}").expect("write");
        if std::env::var_os("RHIZOID_BLESS").is_none() {
            assert_eq!(
                recorded(name),
                Some(d.as_str()),
                "{name}: rerun with RHIZOID_BLESS=1 after a deliberate change"
            );
        }
    }
    if std::env::var_os("RHIZOID_BLESS").is_some() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/fingerprints.txt");
        std::fs::write(path, record).expect("write the record");
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use rhizoid_io::LocalAsyncFile;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;

    /// The same, through a `LocalAsyncFile`.
    async fn fingerprint_async(file: &LocalAsyncFile) -> Vec<String> {
        let mut out = Vec::new();
        for k in file.keys() {
            out.push(k.to_string());
            let name = format!("{};{}", k.name(), k.cycle());
            match k.class_name() {
                c if is_tree(c) => {
                    let tree = file.tree(&name).await.expect("tree");
                    for b in tree.readable_branches() {
                        let col = tree.read(b.name(), ..).await;
                        out.push(format!(
                            "{}: {:?}",
                            b.name(),
                            col.map_err(|e| e.to_string())
                        ));
                    }
                }
                "ROOT::RNTuple" => {
                    let nt = file.rntuple(&name).await.expect("rntuple");
                    for f in nt.fields() {
                        let col = nt.read(f.name(), ..).await;
                        out.push(format!(
                            "{}: {:?}",
                            f.name(),
                            col.map_err(|e| e.to_string())
                        ));
                    }
                }
                "TDirectory" | "TDirectoryFile" => {}
                _ => out.push(format!(
                    "{:?}",
                    file.get_dyn(&name).await.map_err(|e| e.to_string())
                )),
            }
        }
        out
    }

    #[wasm_bindgen_test]
    fn sync_reads_decode_as_natively() {
        for (name, bytes) in FIXTURES {
            let file = File::from_source(bytes.to_vec(), ReadOptions::default()).expect("open");
            assert_eq!(
                Some(digest(&fingerprint(&file)).as_str()),
                recorded(name),
                "{name}"
            );
        }
    }

    #[wasm_bindgen_test]
    async fn async_reads_decode_as_natively() {
        for (name, bytes) in FIXTURES {
            let file = File::open_local_async(bytes.to_vec()).await.expect("open");
            assert_eq!(
                Some(digest(&fingerprint_async(&file).await).as_str()),
                recorded(name),
                "{name}"
            );
        }
    }

    #[wasm_bindgen_test]
    async fn batches_stream_on_wasm() {
        let (_, bytes) = FIXTURES[0];
        let sync = File::from_source(bytes.to_vec(), ReadOptions::default()).expect("open");
        let file = File::open_local_async(bytes.to_vec()).await.expect("open");
        let tree = sync.tree("events").expect("tree");
        let atree = file.tree("events").await.expect("tree");
        let expected: Vec<String> = tree.reader().build().map(|b| format!("{b:?}")).collect();
        let mut stream = atree.reader().build();
        let mut got = Vec::new();
        while let Some(cluster) = stream.next_cluster().await.expect("fetch") {
            got.push(format!("{:?}", cluster.decode()));
        }
        assert_eq!(got, expected);
    }
}
