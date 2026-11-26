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
