//! `ObjectStoreSource` reads every fixture like a local file, through the
//! local and HTTP backends; a replaced object is refused.

#[cfg(feature = "http")]
#[path = "../../rhizoid-io/tests/support/mod.rs"]
mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use object_store::ObjectStoreExt as _;
use object_store::local::LocalFileSystem;
use object_store::memory::InMemory;
use rhizoid_io::io::ReadAt;
use rhizoid_io::{File, FileOffset, ReadOptions};
use rhizoid_object_store::ObjectStoreSource;

const FIXTURES: [&str; 2] = ["uproot_trees.root", "uproot_hists.root"];

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../rhizoid-io/tests/data")
}

/// Keys, objects, and every readable branch, as text.
fn fingerprint(file: &File) -> Vec<String> {
    let mut out = Vec::new();
    for k in file.keys() {
        out.push(k.to_string());
        let name = format!("{};{}", k.name(), k.cycle());
        if k.class_name() == "TTree" {
            let tree = file.tree(&name).expect("tree");
            for b in tree.readable_branches() {
                out.push(format!(
                    "{}: {:?}",
                    b.name(),
                    tree.read(b.name(), ..).map_err(|e| e.to_string())
                ));
            }
        } else {
            out.push(format!(
                "{:?}",
                file.get_dyn(&name).map_err(|e| e.to_string())
            ));
        }
    }
    out
}

/// The same as [`fingerprint`], through the async API.
async fn fingerprint_async(file: &rhizoid_io::AsyncFile) -> Vec<String> {
    let mut out = Vec::new();
    for k in file.keys() {
        out.push(k.to_string());
        let name = format!("{};{}", k.name(), k.cycle());
        if k.class_name() == "TTree" {
            let tree = file.tree(&name).await.expect("tree");
            for b in tree.readable_branches() {
                out.push(format!(
                    "{}: {:?}",
                    b.name(),
                    tree.read(b.name(), ..).await.map_err(|e| e.to_string())
                ));
            }
        } else {
            out.push(format!(
                "{:?}",
                file.get_dyn(&name).await.map_err(|e| e.to_string())
            ));
        }
    }
    out
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt")
}

fn open(source: impl ReadAt + 'static) -> File {
    File::from_source(source, ReadOptions::default()).expect("open")
}

#[test]
fn local_store_reads_like_a_local_file() {
    let store = Arc::new(LocalFileSystem::new_with_prefix(data_dir()).expect("store"));
    for name in FIXTURES {
        let local = fingerprint(&File::open(data_dir().join(name)).expect("local"));
        let direct = open(ObjectStoreSource::new(store.clone(), name).expect("source"));
        assert_eq!(fingerprint(&direct), local, "{name}");
        let wrapped = open(ObjectStoreSource::open(store.clone(), name).expect("source"));
        assert_eq!(fingerprint(&wrapped), local, "{name}, coalesced");
    }
    let url = format!(
        "file:///{}",
        data_dir()
            .join(FIXTURES[0])
            .canonicalize()
            .expect("path")
            .display()
            .to_string()
            .replace('\\', "/")
            .trim_start_matches("//?/")
            .trim_start_matches('/')
    );
    let from_url = open(ObjectStoreSource::from_url(&url).expect("file url"));
    assert_eq!(from_url.tree("events").expect("tree").num_entries(), 1001);
    let e = ObjectStoreSource::new(store, "missing.root").expect_err("absent");
    assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn async_reads_match_sync_reads() {
    let local_store = Arc::new(LocalFileSystem::new_with_prefix(data_dir()).expect("store"));
    let memory = Arc::new(InMemory::new());
    let rt = runtime();
    for name in FIXTURES {
        let local = fingerprint(&File::open(data_dir().join(name)).expect("local"));
        let bytes = std::fs::read(data_dir().join(name)).expect("read");
        rt.block_on(memory.put(&object_store::path::Path::from(name), bytes.into()))
            .expect("put");
        for (label, store) in [
            (
                "local",
                local_store.clone() as Arc<dyn object_store::ObjectStore>,
            ),
            (
                "memory",
                memory.clone() as Arc<dyn object_store::ObjectStore>,
            ),
        ] {
            let got = rt.block_on(async {
                let source = ObjectStoreSource::open_async(store, name)
                    .await
                    .expect("source");
                let file = File::open_async(source).await.expect("open");
                fingerprint_async(&file).await
            });
            assert_eq!(got, local, "{name} through {label}");
        }
    }
    // The source can be made and dropped inside a runtime.
    rt.block_on(async {
        let s = ObjectStoreSource::new_async(local_store, FIXTURES[0])
            .await
            .expect("source");
        drop(s);
    });
}

#[test]
fn a_replaced_object_is_refused() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("rt");
    let store = Arc::new(InMemory::new());
    let path = object_store::path::Path::from("f.root");
    let v1 = std::fs::read(data_dir().join(FIXTURES[1])).expect("read");
    rt.block_on(store.put(&path, v1.clone().into()))
        .expect("put");
    let source = ObjectStoreSource::new(store.clone(), "f.root").expect("source");
    assert_eq!(
        source
            .read_at(FileOffset::new(0)..FileOffset::new(4))
            .expect("read")
            .as_ref(),
        b"root"
    );

    let mut v2 = v1;
    v2[200] ^= 1;
    rt.block_on(store.put(&path, v2.into())).expect("put");
    let e = source
        .read_at(FileOffset::new(0)..FileOffset::new(4))
        .expect_err("changed");
    assert!(e.to_string().contains("changed"), "{e}");

    // From inside a runtime the sync methods refuse to block it.
    let e = rt.block_on(async { source.read_at(FileOffset::new(0)..FileOffset::new(4)) });
    assert_eq!(
        e.expect_err("inside a runtime").kind(),
        std::io::ErrorKind::Unsupported
    );
}

#[cfg(feature = "http")]
#[test]
fn http_store_reads_like_a_local_file() {
    use object_store::http::HttpBuilder;
    let server = support::server::Server::start(
        FIXTURES
            .iter()
            .map(|n| {
                (
                    (*n).to_owned(),
                    std::fs::read(data_dir().join(n)).expect("fixture"),
                )
            })
            .collect(),
    );
    let base = server.url("");
    // object_store refuses plain HTTP unless asked; the test server is local.
    let options = object_store::ClientOptions::new().with_allow_http(true);
    let store = Arc::new(
        HttpBuilder::new()
            .with_url(base)
            .with_client_options(options)
            .build()
            .expect("store"),
    );
    // Another store for async reads: a store pools connections on the
    // runtime that opened them (see the crate docs).
    let async_store = Arc::new(
        HttpBuilder::new()
            .with_url(server.url(""))
            .with_client_options(object_store::ClientOptions::new().with_allow_http(true))
            .build()
            .expect("store"),
    );
    let rt = runtime();
    for name in FIXTURES {
        let local = fingerprint(&File::open(data_dir().join(name)).expect("local"));
        let remote = open(ObjectStoreSource::open(store.clone(), name).expect("source"));
        assert_eq!(fingerprint(&remote), local, "{name}");
        let got = rt.block_on(async {
            let source = ObjectStoreSource::open_async(async_store.clone(), name)
                .await
                .expect("source");
            let file = File::open_async(source).await.expect("open");
            fingerprint_async(&file).await
        });
        assert_eq!(got, local, "{name}, async");
    }
}
