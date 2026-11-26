//! M5's exit criterion: every file reads the same through the async API as
//! through the sync one — objects, every branch and field, batch streams
//! and `next_cluster` — over several async sources; reads of a column take
//! one request; broken files fail the same way.

#![cfg(feature = "async")]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

use rhizoid_io::asynchronous::{AsyncReadable, AsyncSource, BatchStream};
use rhizoid_io::io::{Coalesce, Counted, Inline};
use rhizoid_io::tree::{Batch, BatchSize, EntrySelection};
use rhizoid_io::{AsyncFile, File, ReadOptions};

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

/// Every fixture, then every corpus file when `RHIZOID_CORPUS` is set.
fn files() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = FIXTURES.iter().map(|n| data(n)).collect();
    if let Ok(dir) = std::env::var("RHIZOID_CORPUS") {
        let mut more: Vec<PathBuf> = std::fs::read_dir(dir)
            .expect("corpus dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "root"))
            .collect();
        more.sort();
        out.extend(more);
    }
    out
}

struct Unpark(std::thread::Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// A minimal executor: polls on this thread, parking while pending.
fn block_on<F: Future>(f: F) -> F::Output {
    let mut f = pin!(f);
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park();
    }
}

fn next<H: AsyncReadable>(s: &mut BatchStream<H>) -> Option<rhizoid_io::Result<Batch>> {
    use futures_core::Stream;
    block_on(std::future::poll_fn(|cx| {
        std::pin::Pin::new(&mut *s).poll_next(cx)
    }))
}

const ENTRIES: u64 = 2000;

fn is_tree(class: &str) -> bool {
    matches!(class, "TTree" | "TNtuple" | "TNtupleD")
}

/// Keys, objects, and the first entries of every column, as text.
fn fingerprint(file: &File) -> Vec<String> {
    let mut out = Vec::new();
    for k in file.keys() {
        out.push(k.to_string());
        let name = format!("{};{}", k.name(), k.cycle());
        match k.class_name() {
            c if is_tree(c) => {
                let tree = file.tree(&name).expect("tree");
                for b in tree.readable_branches() {
                    let col = tree.read(b.name(), 0..b.num_entries().min(ENTRIES));
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
                    let col = nt.read(f.name(), 0..nt.num_entries().min(ENTRIES));
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

/// The same, through the async API.
async fn fingerprint_async<S: AsyncSource>(file: &AsyncFile<S>) -> Vec<String> {
    let mut out = Vec::new();
    for k in file.keys() {
        out.push(k.to_string());
        let name = format!("{};{}", k.name(), k.cycle());
        match k.class_name() {
            c if is_tree(c) => {
                let tree = file.tree(&name).await.expect("tree");
                for b in tree.readable_branches() {
                    let col = tree.read(b.name(), 0..b.num_entries().min(ENTRIES)).await;
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
                    let col = nt.read(f.name(), 0..nt.num_entries().min(ENTRIES)).await;
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

#[test]
fn every_file_reads_the_same_async() {
    for path in files() {
        let sync = File::open(&path).expect("sync");
        let expected = fingerprint(&sync);
        let bytes = std::fs::read(&path).expect("read");
        let name = path.display();

        let file = block_on(File::open_async(bytes.clone())).expect("Vec<u8>");
        assert_eq!(
            block_on(fingerprint_async(&file)),
            expected,
            "{name}: Vec<u8>"
        );
        assert_eq!(
            file.diagnostics(),
            sync.diagnostics(),
            "{name}: diagnostics"
        );

        let file = block_on(File::open_async(Coalesce::new(Inline(
            std::fs::File::open(&path).expect("file"),
        ))))
        .expect("Coalesce<Inline<File>>");
        assert_eq!(
            block_on(fingerprint_async(&file)),
            expected,
            "{name}: Coalesce<Inline<File>>"
        );

        let file = block_on(File::open_local_async(Inline(bytes))).expect("local");
        assert_eq!(
            block_on(fingerprint_async(&file)),
            expected,
            "{name}: local"
        );
    }
}

/// After opening a tree or `RNTuple`, reading a column takes one request
/// (none when every basket is inside the tree object).
#[test]
fn a_column_is_one_request() {
    for path in files() {
        let source = Counted::new(std::fs::read(&path).expect("read"));
        let stats = source.stats_handle();
        let file = block_on(File::open_async(source)).expect("open");
        let name = path.display();
        for k in file
            .keys()
            .map(|k| {
                (
                    k.class_name().to_owned(),
                    format!("{};{}", k.name(), k.cycle()),
                )
            })
            .collect::<Vec<_>>()
        {
            if is_tree(&k.0) {
                let tree = block_on(file.tree(&k.1)).expect("tree");
                for b in tree.readable_branches() {
                    let before = stats().calls;
                    let n = b.num_entries().min(ENTRIES);
                    if block_on(tree.read(b.name(), 0..n)).is_ok() {
                        let calls = stats().calls - before;
                        assert!(calls <= 1, "{name}: {} took {calls} requests", b.name());
                    }
                }
            } else if k.0 == "ROOT::RNTuple" {
                let nt = block_on(file.rntuple(&k.1)).expect("rntuple");
                for f in nt.fields() {
                    let before = stats().calls;
                    let n = nt.num_entries().min(ENTRIES);
                    if block_on(nt.read(f.name(), 0..n)).is_ok() {
                        let calls = stats().calls - before;
                        assert!(calls <= 1, "{name}: {} took {calls} requests", f.name());
                    }
                }
            }
        }
    }
}

/// Streams give the sync reader's batches, and so do the cluster readers.
#[test]
fn streams_match_the_sync_reader() {
    for path in files() {
        let sync = File::open(&path).expect("sync");
        let bytes = std::fs::read(&path).expect("read");
        let file = block_on(File::open_async(bytes)).expect("open");
        let name = path.display();
        let keys: Vec<(String, String)> = sync
            .keys()
            .map(|k| {
                (
                    k.class_name().to_owned(),
                    format!("{};{}", k.name(), k.cycle()),
                )
            })
            .collect();
        for (class, key) in keys {
            let sizes = [BatchSize::Cluster, BatchSize::entries(97)];
            if is_tree(&class) {
                let tree = sync.tree(&key).expect("tree");
                let atree = block_on(file.tree(&key)).expect("tree");
                let n = tree.num_entries();
                let selection =
                    EntrySelection::from_ranges([0..n.min(150), n / 2..n.min(n / 2 + 300)]);
                for size in sizes {
                    let builder = || {
                        tree.reader()
                            .selection(selection.clone())
                            .expect("sel")
                            .batch_size(size)
                    };
                    let abuilder = || {
                        atree
                            .reader()
                            .selection(selection.clone())
                            .expect("sel")
                            .batch_size(size)
                    };
                    let expected: Vec<String> =
                        builder().build().map(|b| format!("{b:?}")).collect();
                    let mut stream = abuilder().build();
                    let mut got = Vec::new();
                    while let Some(b) = next(&mut stream) {
                        got.push(format!("{b:?}"));
                    }
                    assert_eq!(got, expected, "{name}:{key} stream");
                    let mut stream = abuilder().build();
                    let mut got = Vec::new();
                    while let Some(c) = block_on(stream.next_cluster()).expect("fetch") {
                        got.push(format!("{:?}", c.decode()));
                    }
                    assert_eq!(got, expected, "{name}:{key} next_cluster");
                }
            } else if class == "ROOT::RNTuple" {
                let nt = sync.rntuple(&key).expect("rntuple");
                let ant = block_on(file.rntuple(&key)).expect("rntuple");
                let n = nt.num_entries();
                let selection =
                    EntrySelection::from_ranges([0..n.min(150), n / 2..n.min(n / 2 + 300)]);
                for size in sizes {
                    let builder = || {
                        nt.reader()
                            .selection(selection.clone())
                            .expect("sel")
                            .batch_size(size)
                    };
                    let abuilder = || {
                        ant.reader()
                            .selection(selection.clone())
                            .expect("sel")
                            .batch_size(size)
                    };
                    let expected: Vec<String> =
                        builder().build().map(|b| format!("{b:?}")).collect();
                    let mut stream = abuilder().build();
                    let mut got = Vec::new();
                    while let Some(b) = next(&mut stream) {
                        got.push(format!("{b:?}"));
                    }
                    assert_eq!(got, expected, "{name}:{key} stream");
                    let mut stream = abuilder().build();
                    let mut got = Vec::new();
                    while let Some(c) = block_on(stream.next_cluster()).expect("fetch") {
                        got.push(format!("{:?}", c.decode()));
                    }
                    assert_eq!(got, expected, "{name}:{key} next_cluster");
                }
            }
        }
    }
}

#[test]
fn async_handles_are_send() {
    fn send<T: Send>(_: &T) {}
    fn is<T: Send + Sync + 'static>() {}
    is::<AsyncFile>();
    is::<rhizoid_io::AsyncTree>();
    is::<rhizoid_io::AsyncRNTuple>();
    let bytes = std::fs::read(data("uproot_trees.root")).expect("read");
    let open = File::open_async(bytes);
    send(&open);
    let file = block_on(open).expect("open");
    let get = file.tree("events");
    send(&get);
    let tree = block_on(get).expect("tree");
    let read = tree.read("f32", ..);
    send(&read);
    block_on(read).expect("read");
    let mut stream = tree.reader().build();
    let cluster = stream.next_cluster();
    send(&cluster);
    let reader = block_on(cluster).expect("fetch").expect("one cluster");
    // A cluster reader decodes on another thread.
    let batch = std::thread::spawn(move || reader.decode())
        .join()
        .expect("thread");
    assert!(batch.expect("decode").num_entries() > 0);
}

mod broken {
    use super::*;
    use proptest::prelude::*;

    fn summary(r: rhizoid_io::Result<(Vec<String>, usize)>) -> String {
        match r {
            Ok((keys, diagnostics)) => format!("ok {keys:?} {diagnostics}"),
            Err(e) => format!("err {:?}", e.kind()),
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// A truncated or damaged file opens, recovers or fails exactly as
        /// it does through the sync API.
        #[test]
        fn broken_files_fail_alike(cut in 0usize..30_000, flips in proptest::collection::vec((0usize..30_000, any::<u8>()), 0..4)) {
            let mut bytes = std::fs::read(data("uproot_hists.root")).expect("read");
            for (at, v) in flips {
                if let Some(b) = bytes.get_mut(at) {
                    *b ^= v;
                }
            }
            bytes.truncate(bytes.len().saturating_sub(cut));
            let sync = File::from_source(bytes.clone(), ReadOptions::default())
                .map(|f| (f.keys().map(|k| k.to_string()).collect(), f.diagnostics().len()));
            let r#async = block_on(File::open_async(bytes))
                .map(|f| (f.keys().map(|k| k.to_string()).collect(), f.diagnostics().len()));
            prop_assert_eq!(summary(r#async), summary(sync));
        }
    }
}

/// The async HTTP source against the test server: every file, the
/// server's quirks and faults, and the over-read bound.
#[cfg(feature = "http")]
#[path = "support/mod.rs"]
mod support;

#[cfg(feature = "http")]
mod http {
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use rhizoid_io::FileOffset;
    use rhizoid_io::io::{AsyncHttpSource, AsyncReadAt, HttpOptions};

    use super::support::server::Server;
    use super::*;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt")
    }

    fn fast() -> HttpOptions {
        HttpOptions::default().backoff(Duration::from_millis(1))
    }

    fn server(paths: &[PathBuf]) -> Server {
        Server::start(
            paths
                .iter()
                .map(|p| {
                    (
                        p.file_name().expect("name").to_string_lossy().into_owned(),
                        std::fs::read(p).expect("file"),
                    )
                })
                .collect(),
        )
    }

    async fn source(url: &str, options: HttpOptions) -> AsyncHttpSource {
        AsyncHttpSource::with(url, AsyncHttpSource::client().expect("client"), options)
            .await
            .expect("source")
    }

    /// Every fixture and corpus file, through the async HTTP source as is,
    /// coalesced, against a server without multi-range answers, and one
    /// refusing `HEAD`.
    #[test]
    fn every_file_reads_the_same_over_http() {
        let paths = files();
        let server = server(&paths);
        let rt = runtime();
        for path in &paths {
            let expected = fingerprint(&File::open(path).expect("sync"));
            let url = server.url(&path.file_name().expect("name").to_string_lossy());
            let name = path.display();
            rt.block_on(async {
                let file = File::open_async(source(&url, fast()).await)
                    .await
                    .expect("open");
                assert_eq!(fingerprint_async(&file).await, expected, "{name}");
                let file =
                    File::open_async(AsyncHttpSource::open(url.clone()).await.expect("open"))
                        .await
                        .expect("open");
                assert_eq!(
                    fingerprint_async(&file).await,
                    expected,
                    "{name}, coalesced"
                );
            });
        }
        let b = &server.behaviour;
        for path in &paths[..FIXTURES.len()] {
            let expected = fingerprint(&File::open(path).expect("sync"));
            let url = server.url(&path.file_name().expect("name").to_string_lossy());
            rt.block_on(async {
                b.multipart.store(false, Ordering::Relaxed);
                let file = File::open_async(source(&url, fast()).await)
                    .await
                    .expect("open");
                assert_eq!(fingerprint_async(&file).await, expected, "single ranges");
                b.multipart.store(true, Ordering::Relaxed);
                b.no_head.store(true, Ordering::Relaxed);
                let file = File::open_async(source(&url, fast()).await)
                    .await
                    .expect("open");
                assert_eq!(fingerprint_async(&file).await, expected, "HEAD refused");
                b.no_head.store(false, Ordering::Relaxed);
            });
        }
    }

    fn at(a: u64, b: u64) -> Range<FileOffset> {
        FileOffset::new(a)..FileOffset::new(b)
    }

    use std::ops::Range;

    #[test]
    fn faults_are_retried_or_reported() {
        let server = server(&[data("uproot_hists.root")]);
        let url = server.url("uproot_hists.root");
        let b = &server.behaviour;
        runtime().block_on(async {
            // Two 503s, then success.
            b.fail_next.store(2, Ordering::Relaxed);
            let file = File::open_async(source(&url, fast()).await)
                .await
                .expect("retried");
            assert!(file.keys().count() > 0);

            // More failures than retries: an error naming the retries.
            b.fail_next.store(10, Ordering::Relaxed);
            let e = AsyncHttpSource::with(
                &url,
                AsyncHttpSource::client().expect("client"),
                fast().retries(2),
            )
            .await
            .expect_err("gives up");
            assert!(e.to_string().contains("after 2 retries"), "{e}");
            b.fail_next.store(0, Ordering::Relaxed);

            // A truncated body is retried; a wrong Content-Range is refused.
            let src = source(&url, fast()).await;
            b.truncate_next.store(1, Ordering::Relaxed);
            assert_eq!(src.read_at(at(100, 200)).await.expect("retried").len(), 100);
            b.wrong_range_next.store(1, Ordering::Relaxed);
            let e = src.read_at(at(100, 200)).await.expect_err("mislabelled");
            assert_eq!(e.kind(), std::io::ErrorKind::InvalidData, "{e}");

            // The file changes on the server: refused, not mixed.
            *b.etag.lock().expect("etag") = "\"v2\"".to_owned();
            let e = src.read_at(at(100, 200)).await.expect_err("changed");
            assert!(e.to_string().contains("changed"), "{e}");
            *b.etag.lock().expect("etag") = "\"v1\"".to_owned();

            // A server that ignores ranges: refused, the body is not read.
            b.ignore_ranges.store(true, Ordering::Relaxed);
            let e = src.read_at(at(100, 200)).await.expect_err("no ranges");
            assert_eq!(e.kind(), std::io::ErrorKind::Unsupported, "{e}");
            b.ignore_ranges.store(false, Ordering::Relaxed);

            let e = AsyncHttpSource::new(server.url("missing.root"))
                .await
                .expect_err("404");
            assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
        });
    }

    #[test]
    fn coalescing_stays_within_the_over_read_bound() {
        let server = server(&[data("uproot_trees.root")]);
        let url = server.url("uproot_trees.root");
        runtime().block_on(async {
            let inner = Counted::new(source(&url, fast()).await);
            let fetched = inner.stats_handle();
            let outer = Counted::new(Coalesce::new(inner));
            let needed = outer.stats_handle();
            let file = File::open_async(outer).await.expect("open");
            let tree = file.tree("events").await.expect("tree");
            for name in ["f32", "jet_pt", "u64", "xyz"] {
                tree.read(name, ..).await.expect("read");
            }
            let (n, f) = (needed(), fetched());
            assert!(
                f.bytes * 4 <= n.bytes * 5,
                "fetched {} for {} needed",
                f.bytes,
                n.bytes
            );
            // The server sent exactly what was fetched.
            assert_eq!(
                server.behaviour.range_bytes.load(Ordering::Relaxed),
                f.bytes
            );
        });
    }
}
