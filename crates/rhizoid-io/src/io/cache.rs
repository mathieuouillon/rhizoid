//! [`ReadCache`]: keeps the head of the file and recent single reads.
//!
//! Opening a file and walking its directories issues small reads (header,
//! directory records, key lists, `StreamerInfo`) that a remote source pays
//! a round trip for each; baskets go through `read_ranges` and are not
//! cached.

use std::collections::VecDeque;
use std::io;
use std::ops::Range;
use std::sync::{Mutex, PoisonError};

use bytes::Bytes;
use rhizoid_source::{FileOffset, ReadAt};

/// The size of the head kept (it holds the file header and, usually, the
/// root directory).
const HEAD: u64 = 4096;

/// Wraps a source, caching its first 4 KiB and its most recent
/// [`read_at`](ReadAt::read_at) results (up to 32 reads or 16 MiB).
#[derive(Debug)]
pub struct ReadCache<S> {
    inner: S,
    head: Mutex<Option<Bytes>>,
    recent: Mutex<VecDeque<(Range<u64>, Bytes)>>,
}

const MAX_ENTRIES: usize = 32;
const MAX_BYTES: usize = 16 << 20;

impl<S> ReadCache<S> {
    /// Wraps `inner`.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            head: Mutex::new(None),
            recent: Mutex::new(VecDeque::new()),
        }
    }

    /// The wrapped source.
    pub fn get_ref(&self) -> &S {
        &self.inner
    }
}

fn slice(b: &Bytes, from: u64, r: &Range<u64>) -> Option<Bytes> {
    let a = usize::try_from(r.start.checked_sub(from)?).ok()?;
    let e = usize::try_from(r.end.checked_sub(from)?).ok()?;
    (e <= b.len()).then(|| b.slice(a..e))
}

impl<S: ReadAt> ReadAt for ReadCache<S> {
    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }

    fn read_at(&self, range: Range<FileOffset>) -> io::Result<Bytes> {
        let r = range.start.get()..range.end.get().max(range.start.get());
        if r.end <= HEAD {
            let mut head = self.head.lock().unwrap_or_else(PoisonError::into_inner);
            if head.is_none() {
                let end = HEAD.min(self.inner.len()?);
                *head = Some(
                    self.inner
                        .read_at(FileOffset::new(0)..FileOffset::new(end))?,
                );
            }
            if let Some(b) = head.as_ref().and_then(|h| slice(h, 0, &r)) {
                return Ok(b);
            }
        }
        {
            let recent = self.recent.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(b) = recent
                .iter()
                .find(|(c, _)| c.start <= r.start && r.end <= c.end)
                .and_then(|(c, b)| slice(b, c.start, &r))
            {
                return Ok(b);
            }
        }
        let bytes = self.inner.read_at(range)?;
        let mut recent = self.recent.lock().unwrap_or_else(PoisonError::into_inner);
        recent.push_front((r, bytes.clone()));
        while recent.len() > MAX_ENTRIES
            || recent.iter().map(|(_, b)| b.len()).sum::<usize>() > MAX_BYTES
        {
            recent.pop_back();
        }
        Ok(bytes)
    }

    fn read_ranges(&self, ranges: &[Range<FileOffset>]) -> io::Result<Vec<Bytes>> {
        self.inner.read_ranges(ranges)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A source of `0, 1, 2, …` (mod 256) that counts the reads it serves.
    struct Counting {
        data: Vec<u8>,
        reads: AtomicUsize,
    }

    impl Counting {
        fn new(len: usize) -> Self {
            Self {
                data: (0..len)
                    .map(|i| u8::try_from(i % 256).unwrap_or(0))
                    .collect(),
                reads: AtomicUsize::new(0),
            }
        }
        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }
    }

    impl ReadAt for Counting {
        fn len(&self) -> io::Result<u64> {
            Ok(self.data.len() as u64)
        }
        fn read_at(&self, range: Range<FileOffset>) -> io::Result<Bytes> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.data.read_at(range)
        }
    }

    fn at(a: u64, b: u64) -> Range<FileOffset> {
        FileOffset::new(a)..FileOffset::new(b)
    }

    #[test]
    fn the_head_is_read_once() {
        let cache = ReadCache::new(Counting::new(10_000));
        for (a, b) in [(0, 100), (100, 400), (3000, 4096), (0, 4096)] {
            let got = cache.read_at(at(a, b)).expect("read");
            let (a, b) = (
                usize::try_from(a).expect("small"),
                usize::try_from(b).expect("small"),
            );
            assert_eq!(&got[..], &cache.get_ref().data[a..b]);
        }
        assert_eq!(cache.get_ref().reads(), 1, "one read of the head");
    }

    #[test]
    fn recent_reads_are_served_from_memory() {
        let cache = ReadCache::new(Counting::new(100_000));
        let first = cache.read_at(at(50_000, 60_000)).expect("read");
        let inside = cache.read_at(at(52_000, 53_000)).expect("read");
        assert_eq!(&inside[..], &first[2_000..3_000]);
        assert_eq!(cache.get_ref().reads(), 1);
        // Past the cached range: a new read.
        cache.read_at(at(59_000, 61_000)).expect("read");
        assert_eq!(cache.get_ref().reads(), 2);
    }

    #[test]
    fn the_oldest_reads_are_dropped() {
        let cache = ReadCache::new(Counting::new(1_000_000));
        for i in 0..=(MAX_ENTRIES as u64) {
            cache
                .read_at(at(10_000 + i * 100, 10_000 + i * 100 + 50))
                .expect("read");
        }
        let before = cache.get_ref().reads();
        // The newest is kept, the first one is gone.
        cache
            .read_at(at(
                10_000 + MAX_ENTRIES as u64 * 100,
                10_000 + MAX_ENTRIES as u64 * 100 + 10,
            ))
            .expect("read");
        assert_eq!(cache.get_ref().reads(), before);
        cache.read_at(at(10_000, 10_010)).expect("read");
        assert_eq!(cache.get_ref().reads(), before + 1);
    }

    #[test]
    fn batched_reads_go_to_the_source() {
        let cache = ReadCache::new(Counting::new(10_000));
        let got = cache
            .read_ranges(&[at(5_000, 5_010), at(6_000, 6_020)])
            .expect("ranges");
        assert_eq!(got.len(), 2);
        assert_eq!(&got[1][..], &cache.get_ref().data[6_000..6_020]);
        assert_eq!(cache.len().expect("len"), 10_000);
    }
}
