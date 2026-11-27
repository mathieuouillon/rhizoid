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

#[cfg(test)]
mod tests {
    use super::*;

    fn at(a: u64, b: u64) -> Range<FileOffset> {
        FileOffset::new(a)..FileOffset::new(b)
    }

    fn ready<T>(mut f: std::pin::Pin<Box<dyn Future<Output = T> + '_>>) -> T {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        loop {
            if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    #[test]
    fn calls_ranges_and_bytes_are_counted() {
        let c = Counted::new((0..100).collect::<Vec<u8>>());
        let handle = c.stats_handle();
        assert_eq!(ReadAt::read_at(&c, at(0, 10)).expect("read").len(), 10);
        ReadAt::read_ranges(&c, &[at(10, 15), at(20, 30)]).expect("ranges");
        let s = c.stats();
        assert_eq!((s.calls, s.ranges, s.bytes), (2, 3, 25));
        assert_eq!(handle(), s, "the handle sees the same counters");
        assert_eq!(ReadAt::len(&c).expect("len"), 100);
    }

    #[test]
    fn async_reads_are_counted_too() {
        let c = Counted::new((0..100).collect::<Vec<u8>>());
        assert_eq!(
            ready(AsyncReadAt::read_at(&c, at(0, 4)))
                .expect("read")
                .len(),
            4
        );
        ready(AsyncReadAt::read_ranges(&c, vec![at(5, 6), at(7, 9)])).expect("ranges");
        assert_eq!(ready(AsyncReadAt::len(&c)).expect("len"), 100);
        let local = Counted::new((0..100).collect::<Vec<u8>>());
        ready(LocalAsyncReadAt::read_at(&local, at(1, 3))).expect("read");
        ready(LocalAsyncReadAt::read_ranges(&local, vec![at(0, 1)])).expect("ranges");
        assert_eq!(ready(LocalAsyncReadAt::len(&local)).expect("len"), 100);
        let (s, l) = (c.stats(), local.stats());
        assert_eq!((s.calls, s.ranges, s.bytes), (2, 3, 7));
        assert_eq!((l.calls, l.ranges, l.bytes), (2, 2, 3));
    }
}
