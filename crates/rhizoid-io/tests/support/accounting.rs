//! Checks of what a writer records about its own output: each branch's
//! byte totals against the baskets on disk, and a sink whose bytes can be
//! looked at while a file is being written.

#![allow(dead_code, reason = "each test file uses part of it")]

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use rhizoid_io::{DynObject, File, FileOffset, Value, WriteAt};

/// The objects of a `TObjArray` member.
fn objects<'a>(o: &'a DynObject, member: &str) -> Vec<&'a DynObject> {
    o.get(member)
        .and_then(Value::as_object)
        .and_then(|a| a.get("fObjects"))
        .and_then(Value::as_list)
        .map(|l| l.iter().filter_map(Value::as_object).collect())
        .unwrap_or_default()
}

fn int(o: &DynObject, member: &str) -> i64 {
    o.get(member)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("{} has no integer {member}", o.class()))
        .expect("a member")
}

fn ints(o: &DynObject, member: &str) -> Vec<i64> {
    let a = o
        .get(member)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{} has no array {member}", o.class()))
        .expect("a member");
    (0..a.len())
        .map(|i| a.get(i).and_then(|v| v.as_i64()).expect("an integer"))
        .collect()
}

/// A big-endian integer of `bytes` at `at`.
fn be(bytes: &[u8], at: usize, len: usize) -> i64 {
    bytes[at..at + len]
        .iter()
        .fold(0i64, |acc, &b| (acc << 8) | i64::from(b))
}

/// Checks one branch and those under it; returns their summed
/// `(fTotBytes, fZipBytes)` and basket count.
fn branch(file: &[u8], obj: &DynObject, path: &str) -> (i64, i64, usize) {
    let name = format!(
        "{path}/{}",
        obj.get("fName").and_then(Value::as_str).unwrap_or("?")
    );
    let n = usize::try_from(int(obj, "fWriteBasket")).expect("fWriteBasket");
    let (bytes, seeks) = (ints(obj, "fBasketBytes"), ints(obj, "fBasketSeek"));
    let (mut tot, mut zip) = (0, 0);
    for k in 0..n {
        let at = usize::try_from(seeks[k]).expect("seek");
        // The basket's key header: Nbytes, version, ObjLen, datime, KeyLen.
        let nbytes = be(file, at, 4);
        assert_eq!(nbytes, bytes[k], "{name}: basket {k}'s fBasketBytes");
        tot += be(file, at + 6, 4) + be(file, at + 14, 2);
        zip += nbytes;
    }
    // ROOT's table size: room for the next basket, and 10 at least.
    assert_eq!(
        int(obj, "fMaxBaskets"),
        i64::try_from((n + 1).max(10)).expect("small"),
        "{name}: fMaxBaskets"
    );
    assert_eq!(int(obj, "fTotBytes"), tot, "{name}: fTotBytes");
    assert_eq!(int(obj, "fZipBytes"), zip, "{name}: fZipBytes");
    let mut baskets = n;
    for sub in objects(obj, "fBranches") {
        let (sub_tot, sub_zip, sub_baskets) = branch(file, sub, &name);
        tot += sub_tot;
        zip += sub_zip;
        baskets += sub_baskets;
    }
    (tot, zip, baskets)
}

/// Each branch's `fTotBytes` and `fZipBytes` are its baskets' (`ObjLen +
/// KeyLen` and `Nbytes`, as ROOT counts them), its basket tables as long
/// as ROOT makes them, and the tree's totals are its branches' summed; the reader reports the same. Returns the number of
/// baskets.
pub(crate) fn check_byte_totals(path: &Path, tree: &str) -> usize {
    let file = std::fs::read(path).expect("read");
    let f = File::open(path).expect("open");
    let t: DynObject = f.get(tree).expect("tree object");
    let (mut tot, mut zip, mut baskets) = (0, 0, 0);
    for b in objects(&t, "fBranches") {
        let (bt, bz, k) = branch(&file, b, "");
        tot += bt;
        zip += bz;
        baskets += k;
    }
    assert!(baskets > 0, "no basket");
    assert_eq!(int(&t, "fTotBytes"), tot, "the tree's fTotBytes");
    assert_eq!(int(&t, "fZipBytes"), zip, "the tree's fZipBytes");
    let reader = f.tree(tree).expect("tree");
    for b in reader.branches() {
        let o = objects(&t, "fBranches")
            .into_iter()
            .find(|o| o.get("fName").and_then(Value::as_str) == Some(b.name()))
            .expect("the branch object");
        assert_eq!(
            i64::try_from(b.uncompressed_bytes()).ok(),
            Some(int(o, "fTotBytes"))
        );
        assert_eq!(
            i64::try_from(b.compressed_bytes()).ok(),
            Some(int(o, "fZipBytes"))
        );
    }
    baskets
}

/// An in-memory sink whose bytes stay readable while a file writes to it.
#[derive(Clone, Default)]
pub(crate) struct Shared(Arc<Mutex<Vec<u8>>>);

impl Shared {
    pub(crate) fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    pub(crate) fn bytes(&self) -> Vec<u8> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl WriteAt for Shared {
    fn write_all_at(&mut self, at: FileOffset, buf: &[u8]) -> std::io::Result<()> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .write_all_at(at, buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
