//! [`Counted`]: what a source was asked for, to measure I/O.

use std::io;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use rhizoid_source::{
    AsyncReadAt, BoxFuture, FileOffset, LocalAsyncReadAt, LocalBoxFuture, ReadAt,
};

/// Counts the calls and bytes that go through a source.
#[derive(Debug)]
pub struct Counted<S> {
    inner: S,
    stats: Arc<Counters>,
}

#[derive(Debug, Default)]
struct Counters {
    calls: AtomicU64,
    ranges: AtomicU64,
    bytes: AtomicU64,
}

/// A snapshot of what a [`Counted`] source served.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SourceStats {
    /// Calls to `read_at` and `read_ranges`.
    pub calls: u64,
    /// Ranges requested.
    pub ranges: u64,
    /// Bytes returned.
    pub bytes: u64,
}

impl<S> Counted<S> {
    /// Wraps `inner`.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            stats: Arc::default(),
        }
    }

    /// What has been served so far.
    pub fn stats(&self) -> SourceStats {
        SourceStats {
            calls: self.stats.calls.load(Ordering::Relaxed),
            ranges: self.stats.ranges.load(Ordering::Relaxed),
            bytes: self.stats.bytes.load(Ordering::Relaxed),
        }
    }

    /// A handle that reads the same counters after the source is moved
    /// into a `File`.
    pub fn stats_handle(&self) -> impl Fn() -> SourceStats + Send + Sync + 'static {
        let c = Arc::clone(&self.stats);
        move || SourceStats {
            calls: c.calls.load(Ordering::Relaxed),
            ranges: c.ranges.load(Ordering::Relaxed),
            bytes: c.bytes.load(Ordering::Relaxed),
        }
    }

    fn count(&self, ranges: usize, out: &[Bytes]) {
        let s = &self.stats;
        s.calls.fetch_add(1, Ordering::Relaxed);
        s.ranges
            .fetch_add(u64::try_from(ranges).unwrap_or(u64::MAX), Ordering::Relaxed);
        let bytes: usize = out.iter().map(Bytes::len).sum();
        s.bytes
            .fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
    }
}

impl<S: AsyncReadAt> AsyncReadAt for Counted<S> {
    fn len(&self) -> BoxFuture<'_, io::Result<u64>> {
        self.inner.len()
    }

    fn read_at(&self, range: Range<FileOffset>) -> BoxFuture<'_, io::Result<Bytes>> {
        Box::pin(async move {
            let b = self.inner.read_at(range).await?;
            self.count(1, core::slice::from_ref(&b));
            Ok(b)
        })
    }

    fn read_ranges(&self, ranges: Vec<Range<FileOffset>>) -> BoxFuture<'_, io::Result<Vec<Bytes>>> {
        Box::pin(async move {
            let n = ranges.len();
            let out = self.inner.read_ranges(ranges).await?;
            self.count(n, &out);
            Ok(out)
        })
    }
}

impl<S: LocalAsyncReadAt> LocalAsyncReadAt for Counted<S> {
    fn len(&self) -> LocalBoxFuture<'_, io::Result<u64>> {
        self.inner.len()
    }

    fn read_at(&self, range: Range<FileOffset>) -> LocalBoxFuture<'_, io::Result<Bytes>> {
        Box::pin(async move {
            let b = self.inner.read_at(range).await?;
            self.count(1, core::slice::from_ref(&b));
            Ok(b)
        })
    }

    fn read_ranges(
        &self,
        ranges: Vec<Range<FileOffset>>,
    ) -> LocalBoxFuture<'_, io::Result<Vec<Bytes>>> {
        Box::pin(async move {
            let n = ranges.len();
            let out = self.inner.read_ranges(ranges).await?;
            self.count(n, &out);
            Ok(out)
        })
    }
}

impl<S: ReadAt> ReadAt for Counted<S> {
    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }

    fn read_at(&self, range: Range<FileOffset>) -> io::Result<Bytes> {
        let b = self.inner.read_at(range)?;
        self.count(1, core::slice::from_ref(&b));
        Ok(b)
    }

    fn read_ranges(&self, ranges: &[Range<FileOffset>]) -> io::Result<Vec<Bytes>> {
        let out = self.inner.read_ranges(ranges)?;
        self.count(ranges.len(), &out);
        Ok(out)
    }
}
