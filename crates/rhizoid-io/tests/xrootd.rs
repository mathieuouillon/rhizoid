//! `root://` files: every fixture reads through `XrootdSource` (and,
//! with `async`, `AsyncXrootdSource`) as it does locally, against a test
//! server that speaks the XRootD protocol (`support/xrootd_server.rs`):
//! vector reads, responses in parts, a manager's redirect with a token,
//! `unix` authentication, waits and asynchronous answers, transient
//! faults retried on a new connection, and what is refused (other
//! authentication, TLS, missing files, directories, a file that changed).
//!
//! With `RHIZOID_XROOTD_NETWORK=1`, a CMS Open Data file is read from
//! `root://eospublic.cern.ch`. With `RHIZOID_XROOTD_SERVER` naming a
//! directory of a real xrootd server that holds the fixtures (say
//! `root://127.0.0.1:21094//fixtures`), every fixture reads from it as
//! locally. With `RHIZOID_ROOT` naming a `root` built
//! with XRootD, ROOT reads a fixture from the test server, a check of the
//! server the client is tested against.

#![cfg(all(feature = "xrootd", not(target_arch = "wasm32")))]

mod support;

use std::io::ErrorKind;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use rhizoid_io::columnar::Column;
use rhizoid_io::io::{FileOffset, ReadAt, XrootdOptions, XrootdSource};
use rhizoid_io::{Array, File, ReadOptions};
use support::xrootd_server::{Auth, Server};

const FIXTURES: [&str; 4] = [
    "uproot_trees.root",
    "uproot_hists.root",
    "uproot_rntuple.root",
    "root_tree.root",
];

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn server() -> Server {
    Server::start(
        FIXTURES
            .iter()
            .map(|n| ((*n).to_owned(), std::fs::read(data(n)).expect("fixture")))
            .collect(),
    )
}

fn fast() -> XrootdOptions {
    XrootdOptions::default()
        .backoff(Duration::from_millis(1))
        .timeout(Duration::from_secs(10))
}

fn open(source: impl ReadAt + 'static) -> File {
    File::from_source(source, ReadOptions::default()).expect("open")
}

/// Keys, objects and the first entries of every readable column, as text.
fn fingerprint(file: &File) -> Vec<String> {
    let mut out = Vec::new();
    for k in file.keys() {
        out.push(k.to_string());
        let name = format!("{};{}", k.name(), k.cycle());
        match k.class_name() {
            "TTree" | "TNtuple" | "TNtupleD" => {
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

fn at(r: Range<u64>) -> Range<FileOffset> {
    FileOffset::new(r.start)..FileOffset::new(r.end)
}

/// Deterministic ranges over `len` bytes: small and large, some empty,
/// some overlapping.
fn ranges(len: u64, n: usize) -> Vec<Range<u64>> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let start = x % len;
            let size = match x % 5 {
                0 => 0,
                1 => x % 7,
                _ => x % 4000,
            };
            start..(start + size).min(len)
        })
        .collect()
}

#[test]
fn every_fixture_reads_like_a_local_file() {
    let server = server();
    for name in FIXTURES {
        let local = fingerprint(&File::open(data(name)).expect("local"));
        let url = server.url(name);
        assert_eq!(
            fingerprint(&File::open_url(&url).expect("open_url")),
            local,
            "{name} through File::open_url"
        );
        assert_eq!(
            fingerprint(&open(XrootdSource::with(&url, fast()).expect("xrootd"))),
            local,
            "{name} through XrootdSource, uncached"
        );
    }
    let b = &server.behaviour;
    assert!(
        b.readvs.load(Ordering::Relaxed) > 0,
        "vector reads were used"
    );
    assert!(b.reads.load(Ordering::Relaxed) > 0);
}

#[test]
fn vector_reads_return_the_bytes_asked() {
    let server = server();
    let name = "uproot_trees.root";
    let bytes = std::fs::read(data(name)).expect("fixture");
    let len = u64::try_from(bytes.len()).expect("len");
    let src = XrootdSource::with(&server.url(name), fast()).expect("open");
    assert_eq!(src.len().expect("len"), len);
    let check = |label: &str, src: &XrootdSource| {
        let rs = ranges(len, 3000);
        let asked: Vec<Range<FileOffset>> = rs.iter().cloned().map(at).collect();
        let got = src.read_ranges(&asked).expect("readv");
        for (r, g) in rs.iter().zip(&got) {
            let want = &bytes
                [usize::try_from(r.start).expect("start")..usize::try_from(r.end).expect("end")];
            assert_eq!(&g[..], want, "{label}: {r:?}");
        }
        assert_eq!(
            &src.read_at(at(0..len)).expect("all")[..],
            &bytes[..],
            "{label}"
        );
    };
    check("whole answers", &src);
    // Answers in parts (`kXR_oksofar`), joined.
    server.behaviour.part_size.store(1000, Ordering::Relaxed);
    check("answers in parts", &src);
    // More than 1024 ranges: several kXR_readv, none over the limit
    // (the server asserts it).
    assert!(server.behaviour.readvs.load(Ordering::Relaxed) >= 6);
    // Out of bounds.
    let e = src.read_at(at(len - 1..len + 1)).expect_err("past the end");
    assert_eq!(e.kind(), ErrorKind::UnexpectedEof);
    // Empty ranges need no request.
    let before = server.behaviour.reads.load(Ordering::Relaxed);
    assert!(src.read_ranges(&[at(5..5)]).expect("empty")[0].is_empty());
    assert_eq!(server.behaviour.reads.load(Ordering::Relaxed), before);
}

#[test]
fn threads_share_one_connection() {
    let server = server();
    let name = "uproot_trees.root";
    let bytes = std::fs::read(data(name)).expect("fixture");
    let len = u64::try_from(bytes.len()).expect("len");
    let src = XrootdSource::with(&server.url(name), fast()).expect("open");
    std::thread::scope(|s| {
        for t in 0..8u64 {
            let (src, bytes) = (&src, &bytes);
            s.spawn(move || {
                for r in ranges(len - t, 200) {
                    let got = src.read_at(at(r.clone())).expect("read");
                    let want = &bytes[usize::try_from(r.start).expect("start")
                        ..usize::try_from(r.end).expect("end")];
                    assert_eq!(&got[..], want);
                }
            });
        }
    });
    assert_eq!(server.behaviour.connections.load(Ordering::Relaxed), 1);
}

#[test]
fn managers_redirect_with_a_token() {
    let data_server = server();
    data_server
        .behaviour
        .require_token
        .store(true, Ordering::Relaxed);
    let manager = Server::start(Vec::new());
    manager
        .behaviour
        .redirect_to
        .store(data_server.port(), Ordering::Relaxed);
    let name = "uproot_hists.root";
    let file = File::open_url(&manager.url(name)).expect("redirected");
    assert_eq!(
        fingerprint(&file),
        fingerprint(&File::open(data(name)).expect("local"))
    );
    let opened = data_server.behaviour.opened.lock().expect("opened").clone();
    assert_eq!(opened, [format!("/{name}?token=secret&x=1")]);
    // Without the manager's token, the data server refuses.
    let e = XrootdSource::with(&data_server.url(name), fast()).expect_err("no token");
    assert_eq!(e.kind(), ErrorKind::PermissionDenied, "{e}");
    // A manager that redirects to itself forever.
    let lost = Server::start(Vec::new());
    lost.behaviour
        .redirect_to
        .store(lost.port(), Ordering::Relaxed);
    let e = XrootdSource::with(&lost.url(name), fast().max_redirects(3)).expect_err("loop");
    assert!(e.to_string().contains("more than 3 redirects"), "{e}");
}

#[test]
fn logins_authenticate_with_unix_or_refuse() {
    let server = server();
    let name = "uproot_hists.root";
    *server.behaviour.auth.lock().expect("auth") = Auth::Unix;
    let url = format!("root://reader@127.0.0.1:{}//{name}", server.port());
    let src = XrootdSource::with(&url, fast()).expect("unix");
    assert!(open(src).check().expect("check").is_ok());
    // Without a user in the URL, a generic name: the local user name is
    // not sent.
    XrootdSource::with(&server.url(name), fast()).expect("unix");
    assert_eq!(
        *server.behaviour.users.lock().expect("users"),
        ["reader", "rhizoid"]
    );
    *server.behaviour.auth.lock().expect("auth") = Auth::GsiOnly;
    let e = XrootdSource::with(&url, fast()).expect_err("gsi");
    assert_eq!(e.kind(), ErrorKind::Unsupported);
    assert!(e.to_string().contains("gsi"), "{e}");
    *server.behaviour.auth.lock().expect("auth") = Auth::None;
    server.behaviour.tls_required.store(true, Ordering::Relaxed);
    let e = XrootdSource::with(&url, fast()).expect_err("tls");
    assert_eq!(e.kind(), ErrorKind::Unsupported);
    let e = XrootdSource::new(&url.replace("root://", "roots://")).expect_err("roots");
    assert_eq!(e.kind(), ErrorKind::Unsupported);
}

#[test]
fn missing_files_and_directories_are_refused() {
    let server = server();
    let e = XrootdSource::with(&server.url("missing.root"), fast()).expect_err("404");
    assert_eq!(e.kind(), ErrorKind::NotFound, "{e}");
    let e = XrootdSource::with(&server.url("dir"), fast()).expect_err("dir");
    assert_eq!(e.kind(), ErrorKind::InvalidInput, "{e}");
    let e = XrootdSource::new("http://example.org/f.root").expect_err("scheme");
    assert_eq!(e.kind(), ErrorKind::InvalidInput);
    // Nobody listening: retried, then reported.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        l.local_addr().expect("addr").port()
    };
    let e = XrootdSource::with(
        &format!("root://127.0.0.1:{port}//f.root"),
        fast().retries(1),
    )
    .expect_err("refused");
    assert!(e.to_string().contains("after 1 retries"), "{e}");
}

#[test]
fn waits_and_asynchronous_answers_are_honoured() {
    let server = server();
    let name = "uproot_trees.root";
    let src = XrootdSource::with(&server.url(name), fast()).expect("open");
    let b = &server.behaviour;
    let rs: Vec<Range<FileOffset>> = ranges(20_000, 50).into_iter().map(at).collect();
    let want = src.read_ranges(&rs).expect("read");
    b.wait_next.store(3, Ordering::Relaxed);
    assert_eq!(src.read_ranges(&rs).expect("after waits"), want);
    b.waitresp_next.store(2, Ordering::Relaxed);
    assert_eq!(src.read_ranges(&rs).expect("asynchronous"), want);
    assert_eq!(b.connections.load(Ordering::Relaxed), 1, "no reconnection");
    // The stat comes from kXR_stat when the open does not give it.
    b.no_stat_in_open.store(true, Ordering::Relaxed);
    let src = XrootdSource::with(&server.url(name), fast()).expect("open");
    assert_eq!(
        src.len().expect("len"),
        std::fs::metadata(data(name)).expect("meta").len()
    );
    assert_eq!(b.stats.load(Ordering::Relaxed), 1);
}

#[test]
fn transient_faults_are_retried_on_a_new_connection() {
    let server = server();
    let name = "uproot_hists.root";
    let local = fingerprint(&File::open(data(name)).expect("local"));
    let b = &server.behaviour;
    let src = XrootdSource::with(&server.url(name), fast()).expect("open");
    let rs: Vec<Range<FileOffset>> = ranges(4000, 20).into_iter().map(at).collect();
    let want = src.read_ranges(&rs).expect("read");

    b.overloaded_next.store(2, Ordering::Relaxed);
    assert_eq!(src.read_ranges(&rs).expect("overloaded, retried"), want);
    b.drop_next.store(1, Ordering::Relaxed);
    assert_eq!(src.read_ranges(&rs).expect("dropped, retried"), want);
    b.short_next.store(1, Ordering::Relaxed);
    assert_eq!(src.read_at(rs[3].clone()).expect("short, retried"), want[3]);
    b.redirect_read_next.store(1, Ordering::Relaxed);
    assert_eq!(src.read_ranges(&rs).expect("redirected"), want);
    assert!(b.connections.load(Ordering::Relaxed) >= 4);
    assert_eq!(fingerprint(&open(src)), local);

    // A stall: the request times out, and is retried.
    let src = XrootdSource::with(
        &server.url(name),
        fast().timeout(Duration::from_millis(300)),
    )
    .expect("open");
    b.stall_next.store(1, Ordering::Relaxed);
    assert_eq!(src.read_ranges(&rs).expect("stalled, retried"), want);
    // Every attempt stalls: an error naming the retries.
    b.stall_next.store(10, Ordering::Relaxed);
    let src = XrootdSource::with(
        &server.url(name),
        fast().timeout(Duration::from_millis(100)).retries(2),
    )
    .expect("open");
    let e = src.read_ranges(&rs).expect_err("gives up");
    assert_eq!(e.kind(), ErrorKind::TimedOut, "{e}");
    assert!(e.to_string().contains("after 2 retries"), "{e}");
    b.stall_next.store(0, Ordering::Relaxed);

    // The file changes between connections: refused, not mixed.
    let src = XrootdSource::with(&server.url(name), fast()).expect("open");
    b.size_skew.store(10, Ordering::Relaxed);
    b.drop_next.store(1, Ordering::Relaxed);
    let e = src.read_ranges(&rs).expect_err("changed");
    assert!(e.to_string().contains("changed"), "{e}");
}

/// Waits until `counter` passes `above`, for at most two seconds.
fn passes(counter: &std::sync::atomic::AtomicU64, above: u64) -> bool {
    for _ in 0..200 {
        if counter.load(Ordering::Relaxed) > above {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// What the options bound: waits and redirects past them are errors, each
/// redirect reading on a new connection; the time the server asks for
/// with `kXR_waitresp` adds to the timeout.
#[test]
fn waits_and_redirects_are_bounded() {
    let server = server();
    let url = server.url("uproot_hists.root");
    let b = &server.behaviour;
    let src = XrootdSource::with(&url, fast().retries(0).max_redirects(2)).expect("open");
    assert_eq!(src.url(), url);
    let want = src.read_at(at(0..100)).expect("read");
    // Ten waits are honoured, not eleven.
    b.wait_next.store(10, Ordering::Relaxed);
    assert_eq!(src.read_at(at(0..100)).expect("ten waits"), want);
    b.wait_next.store(11, Ordering::Relaxed);
    assert!(src.read_at(at(0..100)).is_err(), "eleven waits");
    b.wait_next.store(0, Ordering::Relaxed);
    assert_eq!(src.read_at(at(0..100)).expect("again"), want);
    // Two redirects, each to a new connection; not three.
    let before = b.connections.load(Ordering::Relaxed);
    b.redirect_read_next.store(2, Ordering::Relaxed);
    assert_eq!(src.read_at(at(0..100)).expect("two redirects"), want);
    assert_eq!(b.connections.load(Ordering::Relaxed), before + 2);
    b.redirect_read_next.store(3, Ordering::Relaxed);
    let e = src.read_at(at(0..100)).expect_err("three redirects");
    assert!(e.to_string().contains("more than 2 redirects"), "{e}");
    b.redirect_read_next.store(0, Ordering::Relaxed);
    // An answer announced for one second comes after 600 ms, past the
    // 300 ms timeout: still in time.
    let src = XrootdSource::with(&url, fast().retries(0).timeout(Duration::from_millis(300)))
        .expect("open");
    b.waitresp_delay_ms.store(600, Ordering::Relaxed);
    b.waitresp_next.store(1, Ordering::Relaxed);
    assert_eq!(src.read_at(at(0..100)).expect("late answer"), want);
    b.waitresp_delay_ms.store(0, Ordering::Relaxed);
}

/// A short vector read is retried; data that cannot be trusted is not;
/// connections refused while opening are retried; a dropped source closes
/// its file and its connection, and so does a refused one.
#[test]
fn faults_are_retried_or_not_and_sources_close() {
    let server = server();
    let url = server.url("uproot_hists.root");
    let b = &server.behaviour;
    let src = XrootdSource::with(&url, fast()).expect("open");
    let rs: Vec<Range<FileOffset>> = ranges(4000, 20).into_iter().map(at).collect();
    let want = src.read_ranges(&rs).expect("read");
    b.short_readv_next.store(1, Ordering::Relaxed);
    assert_eq!(src.read_ranges(&rs).expect("short, retried"), want);
    b.bad_attn_next.store(1, Ordering::Relaxed);
    let e = src.read_ranges(&rs).expect_err("untrusted");
    assert_eq!(e.kind(), ErrorKind::InvalidData, "{e}");

    b.hangup_next.store(2, Ordering::Relaxed);
    let src = XrootdSource::with(&url, fast().retries(2)).expect("after two hang-ups");
    assert_eq!(src.read_ranges(&rs).expect("read"), want);
    b.hangup_next.store(1, Ordering::Relaxed);
    assert!(XrootdSource::with(&url, fast().retries(0)).is_err());

    let (closes, gone) = (
        b.closes.load(Ordering::Relaxed),
        b.disconnects.load(Ordering::Relaxed),
    );
    drop(src);
    assert!(passes(&b.closes, closes), "no kXR_close");
    assert!(passes(&b.disconnects, gone), "the connection stays");
    // A connection refused after the handshake is closed too.
    b.tls_required.store(true, Ordering::Relaxed);
    let gone = b.disconnects.load(Ordering::Relaxed);
    assert!(XrootdSource::with(&url, fast()).is_err());
    assert!(passes(&b.disconnects, gone), "the refused connection stays");
}

#[cfg(feature = "async")]
mod asynchronous {
    use std::future::Future;
    use std::pin::pin;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    use rhizoid_io::io::{AsyncReadAt, AsyncXrootdSource};

    use super::*;

    struct Unpark(std::thread::Thread);

    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

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

    #[test]
    fn async_reads_match_the_blocking_source() {
        let server = server();
        let name = "uproot_trees.root";
        let bytes = std::fs::read(data(name)).expect("fixture");
        let len = u64::try_from(bytes.len()).expect("len");
        let src = block_on(AsyncXrootdSource::with(&server.url(name), fast())).expect("open");
        assert_eq!(block_on(AsyncReadAt::len(&src)).expect("len"), len);
        let rs = ranges(len, 1500);
        let got = block_on(src.read_ranges(rs.iter().cloned().map(at).collect())).expect("read");
        for (r, g) in rs.iter().zip(&got) {
            let want = &bytes
                [usize::try_from(r.start).expect("start")..usize::try_from(r.end).expect("end")];
            assert_eq!(&g[..], want, "{r:?}");
        }
        // Faults: retried on a new connection, opened on a thread.
        server.behaviour.drop_next.store(1, Ordering::Relaxed);
        server.behaviour.waitresp_next.store(1, Ordering::Relaxed);
        let again = block_on(src.read_ranges(rs.iter().cloned().map(at).collect())).expect("again");
        assert_eq!(again, got);
        // Timeouts.
        let slow = block_on(AsyncXrootdSource::with(
            &server.url(name),
            fast().timeout(Duration::from_millis(100)).retries(1),
        ))
        .expect("open");
        server.behaviour.stall_next.store(10, Ordering::Relaxed);
        let e = block_on(slow.read_at(at(0..10))).expect_err("stalled");
        assert_eq!(e.kind(), ErrorKind::TimedOut, "{e}");
        server.behaviour.stall_next.store(0, Ordering::Relaxed);
        // A whole file, through AsyncFile.
        let file = block_on(File::open_async(
            block_on(AsyncXrootdSource::open(&server.url(name))).expect("open"),
        ))
        .expect("async file");
        let tree = block_on(file.tree("events")).expect("tree");
        let col = block_on(tree.read("jet_pt", ..)).expect("column");
        let local = File::open(data(name)).expect("local");
        assert_eq!(
            format!("{col:?}"),
            format!(
                "{:?}",
                local
                    .tree("events")
                    .expect("tree")
                    .read("jet_pt", ..)
                    .expect("column")
            )
        );
    }

    /// As `waits_and_redirects_are_bounded`, asynchronously; and reads
    /// started together share one connection.
    #[test]
    fn async_waits_and_redirects_are_bounded() {
        let server = server();
        let url = server.url("uproot_hists.root");
        let b = &server.behaviour;
        let src = block_on(AsyncXrootdSource::with(
            &url,
            fast().retries(0).max_redirects(2),
        ))
        .expect("open");
        assert_eq!(src.url(), url);
        let read = |src: &AsyncXrootdSource| block_on(src.read_at(at(0..100)));
        let want = read(&src).expect("read");
        b.wait_next.store(10, Ordering::Relaxed);
        assert_eq!(read(&src).expect("ten waits"), want);
        b.wait_next.store(11, Ordering::Relaxed);
        assert!(read(&src).is_err(), "eleven waits");
        b.wait_next.store(0, Ordering::Relaxed);
        assert_eq!(read(&src).expect("again"), want);
        let before = b.connections.load(Ordering::Relaxed);
        b.redirect_read_next.store(2, Ordering::Relaxed);
        assert_eq!(read(&src).expect("two redirects"), want);
        assert_eq!(b.connections.load(Ordering::Relaxed), before + 2);
        b.redirect_read_next.store(3, Ordering::Relaxed);
        let e = read(&src).expect_err("three redirects");
        assert!(e.to_string().contains("more than 2 redirects"), "{e}");
        b.redirect_read_next.store(0, Ordering::Relaxed);
        // After a dropped connection, reads started together reconnect once.
        b.drop_next.store(1, Ordering::Relaxed);
        assert!(read(&src).is_err(), "dropped, no retry");
        let before = b.connections.load(Ordering::Relaxed);
        let both = block_on(async {
            let (x, y) = (src.read_at(at(0..100)), src.read_at(at(100..200)));
            (x.await, y.await)
        });
        assert!(both.0.is_ok() && both.1.is_ok(), "{both:?}");
        assert_eq!(b.connections.load(Ordering::Relaxed), before + 1);
        let slow = block_on(AsyncXrootdSource::with(
            &url,
            fast().retries(0).timeout(Duration::from_millis(300)),
        ))
        .expect("open");
        b.waitresp_delay_ms.store(600, Ordering::Relaxed);
        b.waitresp_next.store(1, Ordering::Relaxed);
        assert_eq!(read(&slow).expect("late answer"), want);
        b.waitresp_delay_ms.store(0, Ordering::Relaxed);
    }

    /// With `RHIZOID_XROOTD_SERVER`: the async source against a real
    /// xrootd server, every fixture as locally.
    #[test]
    fn async_reads_from_a_real_server() {
        let Some(base) = real_server() else {
            return;
        };
        for name in REAL_FIXTURES {
            let url = format!("{base}/{name}");
            let file = block_on(File::open_async(
                block_on(AsyncXrootdSource::open(&url)).expect("open"),
            ))
            .expect("async file");
            assert!(block_on(file.check()).expect("check").is_ok(), "{name}");
            let bytes = std::fs::read(data(name)).expect("fixture");
            let len = u64::try_from(bytes.len()).expect("len");
            let src = block_on(AsyncXrootdSource::new(&url)).expect("open");
            let rs = ranges(len, 1500);
            let got =
                block_on(src.read_ranges(rs.iter().cloned().map(at).collect())).expect("read");
            for (r, g) in rs.iter().zip(&got) {
                let want = &bytes[usize::try_from(r.start).expect("start")
                    ..usize::try_from(r.end).expect("end")];
                assert_eq!(&g[..], want, "{name} {r:?}");
            }
        }
    }
}

/// The fixtures a real server is expected to hold, for
/// `RHIZOID_XROOTD_SERVER`.
const REAL_FIXTURES: [&str; 5] = [
    "uproot_trees.root",
    "uproot_hists.root",
    "uproot_rntuple.root",
    "root_tree.root",
    "root_codecs.root",
];

/// `RHIZOID_XROOTD_SERVER`, without a trailing slash.
fn real_server() -> Option<String> {
    let base = std::env::var("RHIZOID_XROOTD_SERVER").ok()?;
    Some(base.trim_end_matches('/').to_owned()).filter(|b| !b.is_empty())
}

/// A real xrootd server (`RHIZOID_XROOTD_SERVER` names a directory of it
/// holding the fixtures): every fixture reads as locally, through
/// `File::open_url` and the bare source, and vector reads return the
/// bytes asked.
#[test]
fn a_real_server_serves_every_fixture() {
    let Some(base) = real_server() else {
        return;
    };
    for name in REAL_FIXTURES {
        let url = format!("{base}/{name}");
        let local = fingerprint(&File::open(data(name)).expect("local"));
        assert_eq!(
            fingerprint(&File::open_url(&url).expect("open_url")),
            local,
            "{name} through File::open_url"
        );
        let src = XrootdSource::new(&url).expect("xrootd");
        let bytes = std::fs::read(data(name)).expect("fixture");
        let len = u64::try_from(bytes.len()).expect("len");
        assert_eq!(src.len().expect("len"), len);
        let rs = ranges(len, 3000);
        let asked: Vec<Range<FileOffset>> = rs.iter().cloned().map(at).collect();
        let got = src.read_ranges(&asked).expect("readv");
        for (r, g) in rs.iter().zip(&got) {
            let want = &bytes
                [usize::try_from(r.start).expect("start")..usize::try_from(r.end).expect("end")];
            assert_eq!(&g[..], want, "{name} {r:?}");
        }
        assert_eq!(&src.read_at(at(0..len)).expect("all")[..], &bytes[..]);
        assert_eq!(fingerprint(&open(src)), local, "{name} uncached");
    }
    let e = XrootdSource::new(&format!("{base}/missing.root")).expect_err("missing");
    assert_eq!(e.kind(), ErrorKind::NotFound, "{e}");
    let e = XrootdSource::new(&base).expect_err("a directory");
    assert_eq!(e.kind(), ErrorKind::InvalidInput, "{e}");
}

/// A public file from CERN Open Data (CMS `NanoAOD`, 19 MB), opened through
/// the EOS manager's redirect. Reads the tree's metadata and two columns.
#[test]
fn eospublic() {
    if std::env::var_os("RHIZOID_XROOTD_NETWORK").is_none() {
        return;
    }
    let url = "root://eospublic.cern.ch//eos/opendata/cms/Run2016H/BTagCSV/NANOAOD/\
               UL2016_MiniAODv2_NanoAODv9-v1/260000/D0EF4579-C811-5B47-A066-4C6013CEA1AA.root";
    let file = File::open_url(url).expect("open");
    let tree = file.tree("Events").expect("Events");
    let n = tree.num_entries();
    assert!(n > 0);
    let run = tree.read("run", ..).expect("run");
    let Column::Values(Array::U32(runs)) = run else {
        unreachable!("run is a UInt_t branch: {run:?}")
    };
    assert_eq!(runs.len(), usize::try_from(n).expect("entries"));
    // Run2016H: runs 281613 to 284044.
    assert!(
        runs.iter().all(|r| (281_613..=284_044).contains(r)),
        "{:?}",
        &runs[..runs.len().min(10)]
    );
    let pt = tree.read("Jet_pt", 0..n.min(1000)).expect("Jet_pt");
    let Column::Jagged {
        values: Array::F32(pts),
        ..
    } = pt
    else {
        unreachable!("Jet_pt is a jagged float branch: {pt:?}")
    };
    assert!(!pts.is_empty() && pts.iter().all(|p| *p > 0.0 && p.is_finite()));
}

/// ROOT (its XRootD client) reads a fixture from the test server: the
/// server the client is tested against speaks the protocol as ROOT
/// expects. Skipped when ROOT cannot load its XRootD client
/// (`libXrdCl`).
#[test]
fn root_reads_from_the_test_server() {
    let Some(root) = std::env::var_os("RHIZOID_ROOT").filter(|r| !r.is_empty()) else {
        return;
    };
    let server = server();
    let url = server.url("root_tree.root");
    let script = format!(
        "{{ auto f = TFile::Open(\"{url}\"); if (!f || f->IsZombie()) {{ printf(\"OPEN FAILED\\n\"); return; }} \
         auto t = f->Get<TTree>(\"t\"); if (!t) {{ printf(\"NO TREE\\n\"); return; }} \
         Long64_t bytes = 0; for (Long64_t i = 0; i < t->GetEntries(); ++i) bytes += t->GetEntry(i); \
         printf(\"ENTRIES %lld READ %lld\\n\", t->GetEntries(), bytes); }}"
    );
    let dir = std::env::temp_dir().join(format!("rhizoid-xrd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let mac = dir.join("read.C");
    std::fs::write(&mac, script).expect("macro");
    let out = std::process::Command::new(root)
        .args(["-b", "-q", "-l"])
        .arg(&mac)
        .output()
        .expect("run root");
    let _ = std::fs::remove_dir_all(&dir);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if text.contains("libXrdCl") {
        // This ROOT was built with XRootD, but its client library is not
        // installed (`TNetXNGFile` cannot load): nothing to check.
        return;
    }
    let local = File::open(data("root_tree.root")).expect("local");
    let entries = local.tree("t").expect("tree").num_entries();
    assert!(
        text.contains(&format!("ENTRIES {entries} READ")),
        "ROOT read from the test server:\n{text}"
    );
    assert!(server.behaviour.opens.load(Ordering::Relaxed) >= 1);
}
