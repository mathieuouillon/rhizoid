//! [`BatchStream`]: batches of an async tree or `RNTuple`, and
//! [`ClusterReader`]: one batch's bytes, decoded wherever the caller likes.

use core::fmt;
use core::future::Future;
use core::ops::Range;
use core::pin::Pin;
use core::task::{Context, Poll, ready};
use std::sync::Arc;

use futures_core::Stream;
use rhizoid_source::{AsyncReadAt, FileOffset};

use super::{AsyncRNTuple, AsyncSource, AsyncTree, MAX_ROUNDS, Prefetched, normalize};
use crate::core::error::{Error, ErrorKind, Result};
use crate::ntuple::RNTuple;
use crate::tree::{Batch, Readable, ReaderBuilder, SyncReadable, Tree, read_batch};

/// An async handle a [`BatchStream`] reads: [`AsyncTree`] or
/// [`AsyncRNTuple`]. Sealed; its items are internal.
pub trait AsyncReadable: Readable + Unpin {
    #[doc(hidden)]
    type Sync: SyncReadable;
    #[doc(hidden)]
    type Source: AsyncSource;
    #[doc(hidden)]
    fn parts(&self) -> (&Self::Sync, &Self::Source);
}

impl<S: AsyncSource> AsyncReadable for AsyncTree<S> {
    type Sync = Tree;
    type Source = S;
    fn parts(&self) -> (&Tree, &S) {
        (self.sync(), self.source())
    }
}

impl<S: AsyncSource> AsyncReadable for AsyncRNTuple<S> {
    type Sync = RNTuple;
    type Source = S;
    fn parts(&self) -> (&RNTuple, &S) {
        (self.sync(), self.source())
    }
}

/// The batches of an [`AsyncTree`].
pub type TreeStream<S = Arc<dyn AsyncReadAt>> = BatchStream<AsyncTree<S>>;

/// The batches of an [`AsyncRNTuple`].
pub type RNTupleStream<S = Arc<dyn AsyncReadAt>> = BatchStream<AsyncRNTuple<S>>;

impl<S: AsyncSource> ReaderBuilder<AsyncTree<S>> {
    /// The reader: a [`Stream`] of batches, each fetched in one request
    /// and then decoded.
    pub fn build(self) -> TreeStream<S> {
        BatchStream::new(self)
    }
}

impl<S: AsyncSource> ReaderBuilder<AsyncRNTuple<S>> {
    /// The reader: a [`Stream`] of batches, each fetched in one request
    /// and then decoded.
    pub fn build(self) -> RNTupleStream<S> {
        BatchStream::new(self)
    }
}

/// A batch's runs, its fetched bytes, and the round it is in.
type Fetched = (Vec<Range<u64>>, Arc<Prefetched>, usize);

enum State<F> {
    Idle,
    Fetching {
        runs: Vec<Range<u64>>,
        store: Arc<Prefetched>,
        ranges: Vec<Range<FileOffset>>,
        fetch: F,
        round: usize,
    },
}

/// Batches of entries of an async tree or `RNTuple`, as a [`Stream`]; made
/// by [`ReaderBuilder::build`].
///
/// Each batch's bytes (every basket or page its columns need) are fetched
/// in one `read_ranges` call, then decoded. To decode on other threads
/// while the next batch is fetched, take batches with
/// [`next_cluster`](Self::next_cluster) instead.
pub struct BatchStream<H: AsyncReadable> {
    handle: H,
    columns: Arc<[String]>,
    plan: std::vec::IntoIter<Vec<Range<u64>>>,
    state: State<<H::Source as AsyncSource>::Fetch>,
}

impl<H: AsyncReadable> fmt::Debug for BatchStream<H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BatchStream")
            .field("columns", &self.columns)
            .field("batches_left", &self.plan.len())
            .finish_non_exhaustive()
    }
}

impl<H: AsyncReadable> BatchStream<H> {
    fn new(builder: ReaderBuilder<H>) -> Self {
        let (handle, columns, plan) = builder.into_plan();
        Self {
            handle,
            columns: columns.into(),
            plan: plan.into_iter(),
            state: State::Idle,
        }
    }

    /// The columns read, in order.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// Starts fetching `ranges` into `store` for `runs`.
    fn start(
        &mut self,
        runs: Vec<Range<u64>>,
        store: Arc<Prefetched>,
        ranges: Vec<Range<FileOffset>>,
        round: usize,
    ) {
        let fetch = self.handle.parts().1.fetch(ranges.clone());
        self.state = State::Fetching {
            runs,
            store,
            ranges,
            fetch,
            round,
        };
    }

    /// Polls until the next batch's planned bytes are fetched: its runs,
    /// its bytes, and the round it is in.
    fn poll_fetched(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<Fetched>>> {
        loop {
            match core::mem::replace(&mut self.state, State::Idle) {
                State::Idle => {
                    let Some(runs) = self.plan.next() else {
                        return Poll::Ready(Ok(None));
                    };
                    let sync = self.handle.parts().0;
                    let ranges = normalize(
                        self.columns
                            .iter()
                            .flat_map(|c| sync.plan(c, &runs))
                            .collect(),
                    );
                    let store = Arc::new(Prefetched::new(sync.file_len()));
                    if ranges.is_empty() {
                        return Poll::Ready(Ok(Some((runs, store, 0))));
                    }
                    self.start(runs, store, ranges, 0);
                }
                State::Fetching {
                    runs,
                    store,
                    ranges,
                    mut fetch,
                    round,
                } => match Pin::new(&mut fetch).poll(cx) {
                    Poll::Pending => {
                        self.state = State::Fetching {
                            runs,
                            store,
                            ranges,
                            fetch,
                            round,
                        };
                        return Poll::Pending;
                    }
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e.into())),
                    Poll::Ready(Ok(bytes)) => {
                        if let Err(e) = store.insert(&ranges, bytes) {
                            return Poll::Ready(Err(e));
                        }
                        return Poll::Ready(Ok(Some((runs, store, round))));
                    }
                },
            }
        }
    }

    /// The next batch's bytes, fetched, as a [`ClusterReader`] to decode
    /// on any thread (with the default batch size, one cluster). `None`
    /// after the last batch.
    ///
    /// The bytes are those the plan names: every basket or page of the
    /// batch's columns in its entries, the baskets of counter branches, and
    /// for `RNTuple` columns below a collection, every page in the batch's
    /// clusters.
    ///
    /// # Errors
    ///
    /// The source's errors.
    pub async fn next_cluster(&mut self) -> Result<Option<ClusterReader<H::Sync>>> {
        let fetched = core::future::poll_fn(|cx| self.poll_fetched(cx)).await?;
        Ok(fetched.map(|(runs, store, _)| ClusterReader {
            handle: self.handle.parts().0.with_source(Arc::clone(&store) as _),
            columns: Arc::clone(&self.columns),
            runs,
            store,
        }))
    }
}

impl<H: AsyncReadable> Stream for BatchStream<H> {
    type Item = Result<Batch>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Batch>>> {
        let this = self.get_mut();
        loop {
            let (runs, store, round) = match ready!(this.poll_fetched(cx)) {
                Err(e) => return Poll::Ready(Some(Err(e))),
                Ok(None) => return Poll::Ready(None),
                Ok(Some(f)) => f,
            };
            let handle = this.handle.parts().0.with_source(Arc::clone(&store) as _);
            let out = read_batch(&handle, &this.columns, runs.clone());
            match store.take_missing() {
                None => return Poll::Ready(Some(out)),
                Some(_) if round + 1 >= MAX_ROUNDS => {
                    return Poll::Ready(Some(Err(Error::new(
                        ErrorKind::LimitExceeded,
                        format!("a batch needed more than {MAX_ROUNDS} dependent requests"),
                    ))));
                }
                Some(missing) => this.start(runs, store, normalize(missing), round + 1),
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.plan.len() + usize::from(matches!(self.state, State::Fetching { .. }));
        (n, Some(n))
    }
}

/// One batch's bytes, fetched by [`BatchStream::next_cluster`]: decode it
/// with [`decode`](Self::decode) on any thread (`spawn_blocking`, rayon)
/// while the stream fetches the next one.
pub struct ClusterReader<H> {
    handle: H,
    columns: Arc<[String]>,
    runs: Vec<Range<u64>>,
    store: Arc<Prefetched>,
}

impl<H> fmt::Debug for ClusterReader<H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClusterReader")
            .field("entries", &self.runs)
            .field("columns", &self.columns)
            .finish_non_exhaustive()
    }
}

impl<H: SyncReadable> ClusterReader<H> {
    /// The entries of the batch, as runs.
    pub fn entry_ranges(&self) -> &[Range<u64>] {
        &self.runs
    }

    /// Decodes the batch. Synchronous: no I/O, only the bytes in hand.
    ///
    /// # Errors
    ///
    /// Decompression and format errors; `Io` if decoding needed bytes the
    /// plan did not fetch (reading the batch through the stream fetches
    /// them).
    pub fn decode(self) -> Result<Batch> {
        let out = read_batch(&self.handle, &self.columns, self.runs);
        match self.store.take_missing() {
            None => out,
            Some(missing) => Err(Error::new(
                ErrorKind::Io,
                format!(
                    "decoding the batch needed {} byte ranges its plan did not fetch",
                    missing.len()
                ),
            )),
        }
    }
}
