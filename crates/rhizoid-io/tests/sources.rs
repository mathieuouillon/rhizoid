//! M3's exit criterion: every file reads the same through every source —
//! local, coalesced, cached, and HTTP against servers that answer
//! multi-range requests, only single ranges, or refuse `HEAD`; faults are
//! retried or reported; the over-read bound holds.

#![cfg(feature = "http")]

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use rhizoid_io::io::{
    Coalesce, Counted, FileOffset, HttpOptions, HttpPool, HttpSource, ReadAt, ReadCache,
};
use rhizoid_io::{File, ReadOptions};
use support::server::Server;

const FIXTURES: [&str; 3] = [
    "uproot_trees.root",
    "uproot_hists.root",
    "uproot_rntuple.root",
];

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

/// Everything readable in a file, as text: keys, objects, and the first
/// `entries` entries of every readable branch of every tree.
fn fingerprint(file: &File, entries: u64) -> Vec<String> {
    let mut out = Vec::new();
    for k in file.keys() {
        out.push(k.to_string());
        let name = format!("{};{}", k.name(), k.cycle());
        match k.class_name() {
            "TTree" | "TNtuple" | "TNtupleD" => {
                let tree = file.tree(&name).expect("tree");
                for b in tree.readable_branches() {
                    let col = tree.read(b.name(), 0..b.num_entries().min(entries));
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
                    let col = nt.read(f.name(), 0..nt.num_entries().min(entries));
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

fn at(a: u64, b: u64) -> core::ops::Range<FileOffset> {
    FileOffset::new(a)..FileOffset::new(b)
}

fn open(source: impl ReadAt + 'static) -> File {
    File::from_source(source, ReadOptions::default()).expect("open")
}

fn fast() -> HttpOptions {
    HttpOptions::default().backoff(Duration::from_millis(1))
}

fn server() -> Server {
    Server::start(
        FIXTURES
            .iter()
            .map(|n| ((*n).to_owned(), std::fs::read(data(n)).expect("fixture")))
            .collect(),
    )
}

#[test]
fn every_source_reads_like_a_local_file() {
    let server = server();
    let pool = HttpPool::new().expect("pool");
    for name in FIXTURES {
        let local = fingerprint(&File::open(data(name)).expect("local"), u64::MAX);
        let bytes = std::fs::read(data(name)).expect("read");
        let url = server.url(name);
        let check = |label: &str, file: File| {
            assert_eq!(
                fingerprint(&file, u64::MAX),
                local,
                "{name} through {label}"
            );
        };
        check("Coalesce<Vec<u8>>", open(Coalesce::new(bytes.clone())));
        check(
            "ReadCache<Coalesce<std::fs::File>>",
            open(ReadCache::new(Coalesce::new(
                std::fs::File::open(data(name)).expect("file"),
            ))),
        );
        check(
            "HttpSource",
            open(HttpSource::with(&url, pool.clone(), fast()).expect("http")),
        );
        check(
            "HttpSource::open",
            open(HttpSource::open(&url).expect("http")),
        );

        server.behaviour.multipart.store(false, Ordering::Relaxed);
        check(
            "HttpSource, single ranges only",
            open(Coalesce::new(
                HttpSource::with(&url, pool.clone(), fast()).expect("http"),
            )),
        );
        server.behaviour.multipart.store(true, Ordering::Relaxed);

        server.behaviour.no_head.store(true, Ordering::Relaxed);
        check(
            "HttpSource, HEAD refused",
            open(HttpSource::with(&url, pool.clone(), fast()).expect("http")),
        );
        server.behaviour.no_head.store(false, Ordering::Relaxed);
    }
}

#[test]
fn faults_are_retried_or_reported() {
    let server = server();
    let url = server.url("uproot_hists.root");
    let b = &server.behaviour;
    let pool = HttpPool::new().expect("pool");

    // Two 503s, then success.
    b.fail_next.store(2, Ordering::Relaxed);
    let file = open(HttpSource::with(&url, pool.clone(), fast()).expect("retried"));
    assert!(file.check().expect("check").is_ok());

    // More failures than retries: an error naming the retries.
    b.fail_next.store(10, Ordering::Relaxed);
    let e = HttpSource::with(&url, pool.clone(), fast().retries(2)).expect_err("gives up");
    assert!(e.to_string().contains("after 2 retries"), "{e}");
    b.fail_next.store(0, Ordering::Relaxed);

    // A truncated body is retried; a wrong Content-Range is refused.
    let src = HttpSource::with(&url, pool.clone(), fast()).expect("open");
    b.truncate_next.store(1, Ordering::Relaxed);
    assert_eq!(src.read_at(at(100, 200)).expect("retried").len(), 100);
    b.wrong_range_next.store(1, Ordering::Relaxed);
    let e = src.read_at(at(100, 200)).expect_err("mislabelled");
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidData, "{e}");

    // The file changes on the server: refused, not mixed.
    *b.etag.lock().expect("etag") = "\"v2\"".to_owned();
    let e = src.read_at(at(100, 200)).expect_err("changed");
    assert!(e.to_string().contains("changed"), "{e}");
    *b.etag.lock().expect("etag") = "\"v1\"".to_owned();

    // A server that ignores ranges: refused, the body is not downloaded.
    b.ignore_ranges.store(true, Ordering::Relaxed);
    let e = src.read_at(at(100, 200)).expect_err("no ranges");
    assert_eq!(e.kind(), std::io::ErrorKind::Unsupported, "{e}");
    b.ignore_ranges.store(false, Ordering::Relaxed);

    let e = HttpSource::with(server.url("missing.root"), pool, fast()).expect_err("404");
    assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn coalescing_stays_within_the_over_read_bound() {
    let server = server();
    let url = server.url("uproot_trees.root");
    let inner =
        Counted::new(HttpSource::with(&url, HttpPool::new().expect("pool"), fast()).expect("http"));
    let fetched = inner.stats_handle();
    let outer = Counted::new(Coalesce::new(inner));
    let needed = outer.stats_handle();
    let file = open(outer);
    let tree = file.tree("events").expect("tree");
    for name in ["f32", "jet_pt", "u64", "xyz"] {
        tree.read(name, ..).expect("read");
    }
    let (n, f) = (needed(), fetched());
    assert!(
        f.bytes * 4 <= n.bytes * 5,
        "fetched {} bytes for {} needed",
        f.bytes,
        n.bytes
    );
    assert!(f.ranges <= n.ranges, "coalescing never adds requests");
    let sent_bytes = server.behaviour.range_bytes.load(Ordering::Relaxed);
    assert_eq!(
        sent_bytes, f.bytes,
        "the server sent exactly what was fetched"
    );
}

/// The corpus, read over HTTP, matches the local files.
#[test]
fn corpus_over_http() {
    let Ok(dir) = std::env::var("RHIZOID_CORPUS") else {
        return;
    };
    let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
        .expect("dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "root"))
        .map(|p| {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (name, std::fs::read(&p).expect("read"))
        })
        .collect();
    files.sort();
    let names: Vec<String> = files.iter().map(|(n, _)| n.clone()).collect();
    let server = Server::start(files);
    for name in names {
        let local = fingerprint(&File::open(Path::new(&dir).join(&name)).expect("local"), 50);
        let remote = fingerprint(
            &open(HttpSource::open(server.url(&name)).expect("http")),
            50,
        );
        assert_eq!(remote, local, "{name}");
    }
}
