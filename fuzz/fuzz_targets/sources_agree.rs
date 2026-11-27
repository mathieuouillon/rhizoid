//! Any bytes read through every kind of source give the same answers: a
//! plain buffer, the read cache, the coalescing source, and the async API
//! (whose reads go through fetch-and-retry rounds). A difference is a bug
//! in one of them.
#![no_main]

use std::fmt::Write as _;
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use libfuzzer_sys::fuzz_target;
use rhizoid_fuzz::untrusted;
use rhizoid_io::io::{Coalesce, ReadCache};
use rhizoid_io::{AsyncFile, File, ReadAt};

const MAX_KEYS: usize = 32;
const MAX_ENTRIES: u64 = 1024;

fn block_on<F: Future>(f: F) -> F::Output {
    let mut f = pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

/// What a file holds, as text: keys, object classes, tree columns.
fn fingerprint(file: &File) -> String {
    let mut s = String::new();
    let keys: Vec<(String, String, u16)> = file
        .keys()
        .take(MAX_KEYS)
        .map(|k| (k.name().to_owned(), k.class_name().to_owned(), k.cycle()))
        .collect();
    for (name, class, cycle) in keys {
        let path = format!("{name};{cycle}");
        let _ = writeln!(s, "KEY {path} {class}");
        let _ = writeln!(
            s,
            "  {:?}",
            file.get_dyn(&path)
                .map(|o| o.class().to_owned())
                .map_err(|e| e.kind())
        );
        if matches!(class.as_str(), "TTree" | "TNtuple" | "TNtupleD")
            && let Ok(t) = file.tree(&path)
        {
            let n = t.num_entries().min(MAX_ENTRIES);
            for b in t.readable_branches() {
                let _ = writeln!(
                    s,
                    "  {} {:?}",
                    b.name(),
                    t.read(b.name(), 0..n).map_err(|e| e.kind())
                );
            }
        }
    }
    s
}

async fn fingerprint_async(file: &AsyncFile) -> String {
    let mut s = String::new();
    let keys: Vec<(String, String, u16)> = file
        .keys()
        .take(MAX_KEYS)
        .map(|k| (k.name().to_owned(), k.class_name().to_owned(), k.cycle()))
        .collect();
    for (name, class, cycle) in keys {
        let path = format!("{name};{cycle}");
        let _ = writeln!(s, "KEY {path} {class}");
        let obj = file.get_dyn(&path).await;
        let _ = writeln!(
            s,
            "  {:?}",
            obj.map(|o| o.class().to_owned()).map_err(|e| e.kind())
        );
        if matches!(class.as_str(), "TTree" | "TNtuple" | "TNtupleD")
            && let Ok(t) = file.tree(&path).await
        {
            let n = t.num_entries().min(MAX_ENTRIES);
            let names: Vec<String> = t
                .readable_branches()
                .iter()
                .map(|b| b.name().to_owned())
                .collect();
            for b in names {
                let col = t.read(&b, 0..n).await;
                let _ = writeln!(s, "  {b} {:?}", col.map_err(|e| e.kind()));
            }
        }
    }
    s
}

fn open(source: impl ReadAt + 'static) -> Option<String> {
    File::from_source(source, untrusted())
        .ok()
        .map(|f| fingerprint(&f))
}

fuzz_target!(|data: &[u8]| {
    let plain = open(data.to_vec());
    assert_eq!(
        open(ReadCache::new(data.to_vec())),
        plain,
        "through the read cache"
    );
    assert_eq!(
        open(Coalesce::new(data.to_vec())),
        plain,
        "through the coalescing source"
    );
    let async_file = block_on(File::open_async_with(data.to_vec(), untrusted()));
    let fingerprint = async_file.ok().map(|f| block_on(fingerprint_async(&f)));
    // The async API stops reads that need too many dependent requests.
    let too_many = fingerprint
        .as_deref()
        .is_some_and(|f| f.contains("LimitExceeded"));
    if !too_many {
        assert_eq!(fingerprint, plain, "through the async API");
    }
});
