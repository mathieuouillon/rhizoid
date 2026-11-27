//! Bounded coalescing of range reads.
//!
//! ROOT interleaves the baskets of different branches, so merging ranges
//! across large gaps (`object_store`'s Parquet-tuned 1 MiB) can download most
//! of a file to read two branches. Two ranges are merged only when the gap
//! between them is small **and** the merged request still fetches at most
//! `max_over_read` times the bytes actually needed.

use std::io;
use std::ops::Range;

use bytes::Bytes;
use rhizoid_source::{
    AsyncReadAt, BoxFuture, FileOffset, LocalAsyncReadAt, LocalBoxFuture, ReadAt,
};

/// The bounds of [`Coalesce`].
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CoalesceOptions {
    max_gap: u64,
    max_over_read: f64,
}

impl Default for CoalesceOptions {
    /// Gaps of at most 64 KiB, at most 1.25× over-read.
    fn default() -> Self {
        Self {
            max_gap: 64 << 10,
            max_over_read: 1.25,
        }
    }
}

impl CoalesceOptions {
    /// The largest gap merged over. The default is 64 KiB.
    #[must_use]
    pub fn max_gap(mut self, bytes: u64) -> Self {
        self.max_gap = bytes;
        self
    }

    /// The most a merged request may fetch, as a multiple of the bytes
    /// needed (at least 1). The default is 1.25.
    #[must_use]
    pub fn max_over_read(mut self, ratio: f64) -> Self {
        self.max_over_read = if ratio.is_finite() {
            ratio.max(1.0)
        } else {
            1.0
        };
        self
    }
}

/// One merged request and the caller's ranges it serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Group {
    pub(crate) fetch: Range<u64>,
    /// `(index in the caller's list, range)`.
    pub(crate) members: Vec<(usize, Range<u64>)>,
}

/// Plans the requests for `ranges`: sorted by start, a range joins the
/// current group while both bounds hold. Every group fetches at most
/// `max_over_read` times its needed bytes, so the plan does too.
#[expect(
    clippy::cast_precision_loss,
    reason = "byte counts far below 2^52 compare exactly enough as f64"
)]
pub(crate) fn plan(ranges: &[Range<u64>], options: CoalesceOptions) -> Vec<Group> {
    let mut order: Vec<usize> = (0..ranges.len()).collect();
    order.sort_by_key(|&i| (ranges[i].start, ranges[i].end));
    let mut groups: Vec<Group> = Vec::new();
    // Bytes needed by the current group: the union of its members.
    let mut needed = 0_u64;
    for i in order {
        let r = ranges[i].clone();
        if let Some(g) = groups.last_mut() {
            let gap = r.start.saturating_sub(g.fetch.end);
            let new_bytes = r.end.saturating_sub(r.start.max(g.fetch.end));
            let span = r.end.max(g.fetch.end) - g.fetch.start;
            let within = (span as f64) <= options.max_over_read * ((needed + new_bytes) as f64);
            if gap <= options.max_gap && within {
                g.fetch.end = g.fetch.end.max(r.end);
                g.members.push((i, r));
                needed += new_bytes;
                continue;
            }
        }
        needed = r.end - r.start;
        groups.push(Group {
            fetch: r.clone(),
            members: vec![(i, r)],
        });
    }
    groups
}

/// Wraps a source so that [`read_ranges`](ReadAt::read_ranges) merges
/// nearby ranges into fewer requests, within [`CoalesceOptions`]. The
/// requests then go to the source's own `read_ranges`, which may run them
/// in parallel.
#[derive(Debug)]
pub struct Coalesce<S> {
    inner: S,
    options: CoalesceOptions,
}

impl<S> Coalesce<S> {
    /// Wraps `inner` with the default bounds.
    pub fn new(inner: S) -> Self {
        Self::with_options(inner, CoalesceOptions::default())
    }

    /// Wraps `inner` with the given bounds.
    pub fn with_options(inner: S, options: CoalesceOptions) -> Self {
        Self { inner, options }
    }

    /// The wrapped source.
    pub fn get_ref(&self) -> &S {
        &self.inner
    }

    /// The wrapped source, by value.
    pub fn into_inner(self) -> S {
        self.inner
    }
}

fn u64_range(r: &Range<FileOffset>) -> Range<u64> {
    r.start.get()..r.end.get().max(r.start.get())
}

impl<S: ReadAt> ReadAt for Coalesce<S> {
    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }

    fn read_at(&self, range: Range<FileOffset>) -> io::Result<Bytes> {
        self.inner.read_at(range)
    }

    fn read_ranges(&self, ranges: &[Range<FileOffset>]) -> io::Result<Vec<Bytes>> {
        let (groups, fetches) = requests(ranges, self.options);
        let responses = self.inner.read_ranges(&fetches)?;
        split(&groups, responses, ranges.len())
    }
}

/// The merged requests for `ranges`.
fn requests(
    ranges: &[Range<FileOffset>],
    options: CoalesceOptions,
) -> (Vec<Group>, Vec<Range<FileOffset>>) {
    let wanted: Vec<Range<u64>> = ranges.iter().map(u64_range).collect();
    let groups = plan(&wanted, options);
    let fetches = groups
        .iter()
        .map(|g| FileOffset::new(g.fetch.start)..FileOffset::new(g.fetch.end))
        .collect();
    (groups, fetches)
}

/// The caller's ranges, cut from the answers to the merged requests.
fn split(groups: &[Group], responses: Vec<Bytes>, n: usize) -> io::Result<Vec<Bytes>> {
    if responses.len() != groups.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the source returned the wrong number of ranges",
        ));
    }
    let mut out = vec![Bytes::new(); n];
    for (g, bytes) in groups.iter().zip(responses) {
        if u64::try_from(bytes.len()).ok() != Some(g.fetch.end - g.fetch.start) {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("short read: {} bytes for {:?}", bytes.len(), g.fetch),
            ));
        }
        for (i, r) in &g.members {
            let (a, b) = (
                usize::try_from(r.start - g.fetch.start).map_err(io::Error::other)?,
                usize::try_from(r.end - g.fetch.start).map_err(io::Error::other)?,
            );
            if let Some(slot) = out.get_mut(*i) {
                *slot = bytes.slice(a..b);
            }
        }
    }
    Ok(out)
}

impl<S: AsyncReadAt> AsyncReadAt for Coalesce<S> {
    fn len(&self) -> BoxFuture<'_, io::Result<u64>> {
        self.inner.len()
    }

    fn read_at(&self, range: Range<FileOffset>) -> BoxFuture<'_, io::Result<Bytes>> {
        self.inner.read_at(range)
    }

    fn read_ranges(&self, ranges: Vec<Range<FileOffset>>) -> BoxFuture<'_, io::Result<Vec<Bytes>>> {
        let (groups, fetches) = requests(&ranges, self.options);
        Box::pin(async move {
            let responses = self.inner.read_ranges(fetches).await?;
            split(&groups, responses, ranges.len())
        })
    }
}

impl<S: LocalAsyncReadAt> LocalAsyncReadAt for Coalesce<S> {
    fn len(&self) -> LocalBoxFuture<'_, io::Result<u64>> {
        self.inner.len()
    }

    fn read_at(&self, range: Range<FileOffset>) -> LocalBoxFuture<'_, io::Result<Bytes>> {
        self.inner.read_at(range)
    }

    fn read_ranges(
        &self,
        ranges: Vec<Range<FileOffset>>,
    ) -> LocalBoxFuture<'_, io::Result<Vec<Bytes>>> {
        let (groups, fetches) = requests(&ranges, self.options);
        Box::pin(async move {
            let responses = self.inner.read_ranges(fetches).await?;
            split(&groups, responses, ranges.len())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn nearby_ranges_merge_and_far_ones_do_not() {
        let o = CoalesceOptions::default();
        let g = plan(&[0..100, 110..200, 1_000_000..1_000_010], o);
        assert_eq!(
            g.iter().map(|g| g.fetch.clone()).collect::<Vec<_>>(),
            [0..200, 1_000_000..1_000_010]
        );
        // A small gap between big ranges merges; a gap that would exceed
        // 1.25x does not, even under 64 KiB.
        assert_eq!(plan(&[0..1000, 1100..2000], o).len(), 1);
        assert_eq!(plan(&[0..10, 1000..1010], o).len(), 2);
        // Overlaps and duplicates are served from one fetch.
        let g = plan(&[50..60, 0..100, 0..100], o);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].members.len(), 3);
    }

    #[test]
    fn options_bound_the_merging() {
        // No gap allowed: only touching ranges merge.
        let tight = CoalesceOptions::default().max_gap(0);
        assert_eq!(plan(&[0..10, 10..20, 21..30], tight).len(), 2);
        // A generous over-read merges ranges a default plan keeps apart.
        let loose = CoalesceOptions::default().max_over_read(100.0);
        assert_eq!(plan(&[0..10, 1000..1010], loose).len(), 1);
        // Below 1, or not a number: 1 (never fetch less than needed).
        for ratio in [0.5, f64::NAN, f64::INFINITY] {
            let o = CoalesceOptions::default().max_over_read(ratio);
            assert_eq!(plan(&[0..10, 100..110], o).len(), 2, "{ratio}");
        }
        let src = Coalesce::with_options(vec![1_u8, 2, 3], tight);
        assert_eq!(src.get_ref(), &[1, 2, 3]);
        assert_eq!(src.into_inner(), [1, 2, 3]);
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
    fn async_sources_coalesce_the_same_way() {
        let data: Vec<u8> = (0..=255).collect();
        let at = |a: u64, b: u64| FileOffset::new(a)..FileOffset::new(b);
        let src = Coalesce::new(data.clone());
        let got =
            ready(AsyncReadAt::read_ranges(&src, vec![at(200, 210), at(0, 4)])).expect("read");
        assert_eq!(
            (got[0].as_ref(), got[1].as_ref()),
            (&data[200..210], &data[0..4])
        );
        assert_eq!(
            &ready(AsyncReadAt::read_at(&src, at(9, 11))).expect("read")[..],
            [9, 10]
        );
        assert_eq!(ready(AsyncReadAt::len(&src)).expect("len"), 256);
        let got = ready(LocalAsyncReadAt::read_ranges(
            &src,
            vec![at(1, 2), at(3, 5)],
        ))
        .expect("read");
        assert_eq!((got[0].as_ref(), got[1].as_ref()), (&[1][..], &[3, 4][..]));
        assert_eq!(
            &ready(LocalAsyncReadAt::read_at(&src, at(0, 1))).expect("read")[..],
            [0]
        );
        assert_eq!(ready(LocalAsyncReadAt::len(&src)).expect("len"), 256);
    }

    #[test]
    fn results_come_back_in_the_callers_order() {
        let data: Vec<u8> = (0..=255).cycle().take(10_000).collect();
        let src = Coalesce::new(data.clone());
        let rs = [5000..5100, 0..10, 9990..10_000, 20..30];
        let ranges: Vec<_> = rs
            .iter()
            .map(|r| FileOffset::new(r.start)..FileOffset::new(r.end))
            .collect();
        let got = ReadAt::read_ranges(&src, &ranges).expect("read");
        for (r, b) in rs.iter().zip(&got) {
            let (a, e) = (
                usize::try_from(r.start).expect("small"),
                usize::try_from(r.end).expect("small"),
            );
            assert_eq!(b.as_ref(), &data[a..e]);
        }
    }

    proptest! {
        /// The bound holds for any set of ranges: what is fetched is at most
        /// 1.25x the union of what is needed, and covers every range.
        #[test]
        fn over_read_is_bounded(raw in proptest::collection::vec((0u64..2_000_000, 1u64..200_000), 1..60)) {
            let ranges: Vec<Range<u64>> = raw.iter().map(|&(s, n)| s..s + n).collect();
            let groups = plan(&ranges, CoalesceOptions::default());
            let fetched: u64 = groups.iter().map(|g| g.fetch.end - g.fetch.start).sum();
            let mut sorted = ranges.clone();
            sorted.sort_by_key(|r| r.start);
            let (mut union, mut end) = (0_u64, 0_u64);
            for r in &sorted {
                union += r.end.saturating_sub(r.start.max(end));
                end = end.max(r.end);
            }
            prop_assert!(fetched * 4 <= union * 5, "fetched {fetched} for {union}");
            for g in &groups {
                for (i, r) in &g.members {
                    prop_assert_eq!(&ranges[*i], r);
                    prop_assert!(g.fetch.start <= r.start && r.end <= g.fetch.end);
                }
            }
            prop_assert_eq!(groups.iter().map(|g| g.members.len()).sum::<usize>(), ranges.len());
        }
    }
}
