//! `StreamerInfo::compute_checksum` against the checksums ROOT stored, for
//! every class description written by ROOT 6 in the fixtures and, with
//! `RHIZOID_CORPUS`, in the corpus.
//!
//! ROOT stores its dictionary's checksum (`TClass::GetCheckSum`); ours is
//! computed from the description (`TStreamerInfo::GetCheckSum`). The two
//! agree but for classes whose members are `std::unique_ptr` (hashed under
//! that name by the dictionary, as a pointer in the description). ROOT 5
//! files used older variants of the checksum.

use std::path::{Path, PathBuf};

use rhizoid_io::File;

fn files() -> Vec<PathBuf> {
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let mut out: Vec<PathBuf> = ["root_hists.root", "uproot_trees.root", "uproot_hists.root"]
        .iter()
        .map(|n| data.join(n))
        .collect();
    if let Ok(dir) = std::env::var("RHIZOID_CORPUS") {
        let mut more: Vec<PathBuf> = std::fs::read_dir(dir)
            .expect("corpus")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "root"))
            .collect();
        more.sort();
        out.extend(more);
    }
    out
}

#[test]
fn checksums_are_root_s() {
    /// The checksums of classes ROOT writes no description of (from
    /// ROOT 6.42's `TClass::GetCheckSum`).
    const KNOWN: [(&str, u32); 7] = [
        ("TArrayC", 0xae87_9936),
        ("TArrayS", 0x035c_9314),
        ("TArrayI", 0xd9d5_71c7),
        ("TArrayL", 0xfcfc_b489),
        ("TArrayL64", 0x18e7_0794),
        ("TArrayF", 0x5a0b_f6f1),
        ("TArrayD", 0x7139_ef34),
    ];
    /// Where the two checksums differ, and why:
    /// - `TF1`: members held by `std::unique_ptr`;
    /// - `TObjArray`, `TRefTable`: abstract bases whose dictionary
    ///   checksums no file stores;
    /// - ATLAS `xAOD` templates: names the dictionary resolves through
    ///   typedefs.
    const EXCEPTIONS: [&str; 6] = [
        "TF1",
        "TObjArray",
        "TRefTable",
        "DataVector<xAOD::TriggerMenuJson_v1>",
        "DataVector<xAOD::TruthMetaData_v1>",
        "xAOD::CutBookkeeperContainer_v1",
    ];
    /// Collections are checksummed through ROOT's collection proxy, not
    /// from members; this crate never writes their descriptions.
    fn is_collection(class: &str) -> bool {
        [
            "vector<",
            "list<",
            "deque<",
            "set<",
            "multiset<",
            "map<",
            "multimap<",
            "unordered_",
        ]
        .iter()
        .any(|p| class.trim_start_matches("std::").starts_with(p))
    }
    let (mut checked, mut differ) = (0, Vec::new());
    for path in files() {
        let file = File::open(&path).expect("open");
        if file.header().version() < 60000 {
            continue;
        }
        let infos = file.streamer_infos().expect("descriptions");
        let base = |name: &str, version: i32| {
            infos
                .iter()
                .find(|i| i.class_name() == name && (version <= 0 || i.class_version() == version))
                .or_else(|| infos.iter().find(|i| i.class_name() == name))
                .map(rhizoid_io::streamer::StreamerInfo::checksum)
                .or_else(|| KNOWN.iter().find(|(n, _)| *n == name).map(|(_, c)| *c))
        };
        for info in &infos {
            // Classes ROOT writes without a computed checksum.
            if info.checksum() == 0
                || info.class_version() <= 0
                || EXCEPTIONS.contains(&info.class_name())
                || is_collection(info.class_name())
            {
                continue;
            }
            checked += 1;
            let ours = info.compute_checksum(base);
            if ours != info.checksum() {
                differ.push(format!(
                    "{}: {} v{} stored {:#010x}, computed {ours:#010x}",
                    path.file_name().expect("name").to_string_lossy(),
                    info.class_name(),
                    info.class_version(),
                    info.checksum()
                ));
            }
        }
    }
    differ.sort();
    differ.dedup();
    assert!(
        differ.is_empty(),
        "{checked} checked, {} differ:\n{}",
        differ.len(),
        differ.join("\n")
    );
    assert!(checked > 20);
}
