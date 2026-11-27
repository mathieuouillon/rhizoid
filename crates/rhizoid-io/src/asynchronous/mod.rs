//! Async reading (feature `async`): [`AsyncFile`],
//! [`AsyncTree`], [`AsyncRNTuple`], and streams of batches
//! ([`BatchStream`]) whose [`next_cluster`](BatchStream::next_cluster)
//! hands over the bytes of a batch so it can be decoded elsewhere while the
//! next one is fetched.
//!
//! ```
//! # fn block_on<F: std::future::Future>(f: F) -> F::Output {
//! #     let mut f = std::pin::pin!(f);
//! #     let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
//! #     loop { if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) { return v; } }
//! # }
//! # block_on(async {
//! # let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/uproot_trees.root");
//! // Any `AsyncReadAt`: here the bytes in memory.
//! let bytes: Vec<u8> = std::fs::read(path)?;
//! let file = rhizoid_io::File::open_async(bytes).await?;
//! let tree = file.tree("events").await?;
//! let f32s: Vec<f32> = tree.branch::<f32>("f32")?.read(0..10).await?;
//! assert_eq!(f32s.len(), 10);
//! # Ok::<(), rhizoid_io::Error>(())
//! # }).unwrap();
//! ```
//!
//! # Sources
//!
//! An [`AsyncFile`] reads from an `Arc<dyn AsyncReadAt>`, whose futures
//! are `Send`, so its own futures are `Send` too and run on any executor
//! (`tokio::spawn`). A [`LocalAsyncFile`] reads from an
//! `Rc<dyn LocalAsyncReadAt>`, for single-threaded runtimes and the
//! browser's `fetch`. Both have the same methods.
//!
//! # How it works
//!
//! Every read runs the same decoding code as the sync API, against the
//! bytes fetched so far: the decoders never do I/O themselves. When the
//! code asks for bytes that are not there, the run is abandoned, the
//! missing ranges of its first unserved request are fetched with one
//! `read_ranges` call, and the run starts again.
//!
//! - Reads whose byte ranges follow from the metadata (branches, fields,
//!   batches) fetch all of them first, in one request, and then decode in
//!   one run.
//! - Metadata (the header, directories, class descriptions, an `RNTuple`'s
//!   envelopes) takes one request per dependent read, like the sync API.
//! - Nothing is decoded from bytes that were not fetched, and nothing
//!   from an abandoned run is kept.
//!
//! [`File::check`] has no async twin: it walks every record of a file, one
//! dependent read at a time. Writing is synchronous (from async code, use
//! `spawn_blocking`).

mod handles;
mod stream;

use core::fmt;
use core::future::Future;
use core::ops::Range;
use std::collections::BTreeMap;
use std::io;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

use bytes::Bytes;
use rhizoid_source::{
    AsyncReadAt, BoxFuture, FileOffset, LocalAsyncReadAt, LocalBoxFuture, ReadAt,
};

use crate::core::error::{Diagnostic, Error, ErrorKind, Result};
use crate::core::header::FileHeader;
use crate::file::{Directory, File, KeyInfo, ReadOptions};
use crate::streamer::{DynObject, FromDyn, StreamerInfo};
use crate::tree::sealed;

pub use handles::{AsyncBranchReader, AsyncFieldReader, AsyncRNTuple, AsyncTree};
pub use stream::{AsyncReadable, BatchStream, ClusterReader, RNTupleStream, TreeStream};

/// An async byte source a file reads from: `Arc<dyn AsyncReadAt>` (`Send`
/// futures) or `Rc<dyn LocalAsyncReadAt>`. Sealed; its methods are
/// internal.
pub trait AsyncSource: sealed::Sealed + Clone + Unpin {
    #[doc(hidden)]
    type Fetch: Future<Output = io::Result<Vec<Bytes>>> + Unpin;
    #[doc(hidden)]
    type Len: Future<Output = io::Result<u64>> + Unpin;
    #[doc(hidden)]
    fn fetch(&self, ranges: Vec<Range<FileOffset>>) -> Self::Fetch;
    #[doc(hidden)]
    fn total_len(&self) -> Self::Len;
}

// Generic over the object's lifetime, so that futures holding the source
// are provably `Send` (a `'static`-only impl defeats the compiler's check).
impl sealed::Sealed for Arc<dyn AsyncReadAt + '_> {}

impl<'a> AsyncSource for Arc<dyn AsyncReadAt + 'a> {
    type Fetch = BoxFuture<'a, io::Result<Vec<Bytes>>>;
    type Len = BoxFuture<'a, io::Result<u64>>;

    fn fetch(&self, ranges: Vec<Range<FileOffset>>) -> Self::Fetch {
        let s = Arc::clone(self);
        Box::pin(async move { s.read_ranges(ranges).await })
    }

    fn total_len(&self) -> Self::Len {
        let s = Arc::clone(self);
        Box::pin(async move { s.len().await })
    }
}

impl sealed::Sealed for Rc<dyn LocalAsyncReadAt + '_> {}

impl<'a> AsyncSource for Rc<dyn LocalAsyncReadAt + 'a> {
    type Fetch = LocalBoxFuture<'a, io::Result<Vec<Bytes>>>;
    type Len = LocalBoxFuture<'a, io::Result<u64>>;

    fn fetch(&self, ranges: Vec<Range<FileOffset>>) -> Self::Fetch {
        let s = Rc::clone(self);
        Box::pin(async move { s.read_ranges(ranges).await })
    }

    fn total_len(&self) -> Self::Len {
        let s = Rc::clone(self);
        Box::pin(async move { s.len().await })
    }
}

/// The bytes fetched for one read, served to the sync decoders as a
/// [`ReadAt`]. A request for bytes not fetched fails and is remembered (the
/// first one only: later requests of an abandoned run may come from an
/// error path).
pub(crate) struct Prefetched {
    len: u64,
    chunks: Mutex<BTreeMap<(u64, u64), Bytes>>,
    missing: Mutex<Option<Vec<Range<FileOffset>>>>,
}

impl fmt::Debug for Prefetched {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Prefetched")
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl Prefetched {
    pub(crate) fn new(len: u64) -> Self {
        Self {
            len,
            chunks: Mutex::new(BTreeMap::new()),
            missing: Mutex::new(None),
        }
    }

    /// Adds the answer to a fetch of `ranges`, checking its shape.
    pub(crate) fn insert(&self, ranges: &[Range<FileOffset>], bytes: Vec<Bytes>) -> Result<()> {
        if bytes.len() != ranges.len() {
            return Err(Error::new(
                ErrorKind::Io,
                format!(
                    "the source answered {} ranges for {}",
                    bytes.len(),
                    ranges.len()
                ),
            ));
        }
        let mut chunks = self.chunks.lock().unwrap_or_else(PoisonError::into_inner);
        for (r, b) in ranges.iter().zip(bytes) {
            let (start, end) = (r.start.get(), r.end.get());
            if u64::try_from(b.len()).ok() != Some(end.saturating_sub(start)) {
                return Err(Error::new(
                    ErrorKind::UnexpectedEof,
                    format!("the source answered {} bytes for {start}..{end}", b.len()),
                ));
            }
            chunks.insert((start, end), b);
        }
        Ok(())
    }

    /// The missing ranges of the first request that could not be served.
    pub(crate) fn take_missing(&self) -> Option<Vec<Range<FileOffset>>> {
        self.missing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn find(&self, start: u64, end: u64) -> Option<Bytes> {
        let chunks = self.chunks.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(b) = chunks.get(&(start, end)) {
            return Some(b.clone());
        }
        chunks
            .range(..=(start, u64::MAX))
            .rev()
            .find(|((_, e), _)| *e >= end)
            .and_then(|((s, _), b)| {
                let a = usize::try_from(start - s).ok()?;
                let z = usize::try_from(end - s).ok()?;
                b.get(a..z).map(|_| b.slice(a..z))
            })
    }

    fn check(&self, r: &Range<FileOffset>) -> io::Result<()> {
        if r.start > r.end {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("inverted range {}..{}", r.start.get(), r.end.get()),
            ));
        }
        if r.end.get() > self.len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "range {}..{} past the end ({} bytes)",
                    r.start.get(),
                    r.end.get(),
                    self.len
                ),
            ));
        }
        Ok(())
    }

    fn miss(&self, ranges: Vec<Range<FileOffset>>) -> io::Error {
        let mut m = self.missing.lock().unwrap_or_else(PoisonError::into_inner);
        if m.is_none() {
            *m = Some(ranges);
        }
        io::Error::new(io::ErrorKind::WouldBlock, "bytes not fetched yet")
    }
}

impl ReadAt for Prefetched {
    fn len(&self) -> io::Result<u64> {
        Ok(self.len)
    }

    fn read_at(&self, range: Range<FileOffset>) -> io::Result<Bytes> {
        self.check(&range)?;
        match self.find(range.start.get(), range.end.get()) {
            Some(b) => Ok(b),
            None => Err(self.miss(vec![range])),
        }
    }

    fn read_ranges(&self, ranges: &[Range<FileOffset>]) -> io::Result<Vec<Bytes>> {
        let mut out = Vec::with_capacity(ranges.len());
        let mut missing = Vec::new();
        for r in ranges {
            self.check(r)?;
            match self.find(r.start.get(), r.end.get()) {
                Some(b) => out.push(b),
                None => missing.push(r.clone()),
            }
        }
        if missing.is_empty() {
            Ok(out)
        } else {
            Err(self.miss(missing))
        }
    }
}

/// The source of a handle owned by an async file: its sync methods must
/// not be reached.
#[derive(Debug)]
struct Detached;

impl ReadAt for Detached {
    fn len(&self) -> io::Result<u64> {
        Err(detached_error())
    }
    fn read_at(&self, _: Range<FileOffset>) -> io::Result<Bytes> {
        Err(detached_error())
    }
}

fn detached_error() -> io::Error {
    io::Error::other("a handle of an async file read synchronously")
}

pub(crate) fn detached() -> Arc<dyn ReadAt> {
    Arc::new(Detached)
}

/// The most fetch-and-decode rounds one read may take.
const MAX_ROUNDS: usize = 4096;

/// Runs `run` against the bytes fetched so far, fetching what it missed
/// and running it again, until it needs nothing more. `first` is fetched
/// before the first run.
pub(crate) async fn drive<S: AsyncSource, T>(
    source: &S,
    len: u64,
    first: Vec<Range<FileOffset>>,
    mut run: impl FnMut(Arc<dyn ReadAt>) -> Result<T>,
) -> Result<T> {
    let store = Arc::new(Prefetched::new(len));
    let mut next = normalize(first);
    for _ in 0..MAX_ROUNDS {
        if !next.is_empty() {
            let bytes = source.fetch(next.clone()).await?;
            store.insert(&next, bytes)?;
        }
        let out = run(Arc::clone(&store) as Arc<dyn ReadAt>);
        match store.take_missing() {
            None => return out,
            Some(missing) => {
                drop(out);
                next = normalize(missing);
            }
        }
    }
    Err(Error::new(
        ErrorKind::LimitExceeded,
        format!("a read needed more than {MAX_ROUNDS} dependent requests"),
    ))
}

/// Sorted, without duplicates or empty ranges.
pub(crate) fn normalize(mut ranges: Vec<Range<FileOffset>>) -> Vec<Range<FileOffset>> {
    ranges.retain(|r| r.start < r.end);
    ranges.sort_by_key(|r| (r.start, r.end));
    ranges.dedup();
    ranges
}

/// A ROOT file read asynchronously; made by [`File::open_async`] (or
/// [`File::open_local_async`]: [`LocalAsyncFile`]). Cheap to clone.
///
/// Methods without I/O (keys, header) are plain; those that read are
/// `async`. Handles it opens ([`AsyncTree`], [`AsyncRNTuple`]) read through
/// the same source.
#[derive(Clone)]
pub struct AsyncFile<S = Arc<dyn AsyncReadAt>> {
    file: File,
    source: S,
}

/// An [`AsyncFile`] over a source whose futures are not `Send`.
pub type LocalAsyncFile = AsyncFile<Rc<dyn LocalAsyncReadAt>>;

impl<S> fmt::Debug for AsyncFile<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AsyncFile").field(&self.file).finish()
    }
}

impl File {
    /// Opens a file from an async source, with default options.
    ///
    /// The header and the root directory are fetched first, in one request
    /// when they fit in the first 4 KiB, as ROOT writes them.
    ///
    /// # Errors
    ///
    /// As [`File::open`], plus the source's errors.
    pub async fn open_async(source: impl AsyncReadAt + 'static) -> Result<AsyncFile> {
        Self::open_async_with(source, ReadOptions::default()).await
    }

    /// [`File::open_async`] with options.
    ///
    /// # Errors
    ///
    /// As [`File::open_async`].
    pub async fn open_async_with(
        source: impl AsyncReadAt + 'static,
        options: ReadOptions,
    ) -> Result<AsyncFile> {
        AsyncFile::from_source(Arc::new(source) as Arc<dyn AsyncReadAt>, options).await
    }

    /// Opens a file from a source whose futures are not `Send` (the
    /// browser's `fetch`, single-threaded runtimes).
    ///
    /// # Errors
    ///
    /// As [`File::open_async`].
    pub async fn open_local_async(
        source: impl LocalAsyncReadAt + 'static,
    ) -> Result<LocalAsyncFile> {
        Self::open_local_async_with(source, ReadOptions::default()).await
    }

    /// [`File::open_local_async`] with options.
    ///
    /// # Errors
    ///
    /// As [`File::open_async`].
    pub async fn open_local_async_with(
        source: impl LocalAsyncReadAt + 'static,
        options: ReadOptions,
    ) -> Result<LocalAsyncFile> {
        AsyncFile::from_source(Rc::new(source) as Rc<dyn LocalAsyncReadAt>, options).await
    }
}

/// How much of the file's start is fetched with the header.
const FIRST_FETCH: u64 = 4096;

impl<S: AsyncSource> AsyncFile<S> {
    /// Opens a file from a shared async source.
    ///
    /// # Errors
    ///
    /// As [`File::open_async`].
    pub async fn from_source(source: S, options: ReadOptions) -> Result<Self> {
        let len = source.total_len().await?;
        let first = vec![FileOffset::new(0)..FileOffset::new(len.min(FIRST_FETCH))];
        let file = drive(&source, len, first, |s| File::from_arc(s, options.clone())).await?;
        Ok(Self {
            file: file.with_source(detached()),
            source,
        })
    }

    /// The file header.
    pub fn header(&self) -> &FileHeader {
        self.file.header()
    }

    /// The file's name, as stored in the file.
    pub fn name(&self) -> &str {
        self.file.name()
    }

    /// The file's title.
    pub fn title(&self) -> &str {
        self.file.title()
    }

    /// The size of the file in bytes.
    pub fn len(&self) -> u64 {
        self.file.len()
    }

    /// Whether the file has no bytes (never true for a valid file).
    pub fn is_empty(&self) -> bool {
        self.file.is_empty()
    }

    /// The keys of the root directory, every cycle, in file order.
    pub fn keys(&self) -> impl Iterator<Item = KeyInfo<'_>> {
        self.file.keys()
    }

    /// Irregularities tolerated while reading (lenient mode).
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.file.diagnostics()
    }

    /// The root directory.
    pub fn root(&self) -> AsyncDirectory<S> {
        AsyncDirectory {
            dir: self.file.root(),
            source: self.source.clone(),
            len: self.file.len(),
        }
    }

    /// Reads an object into its typed model; see [`File::get`].
    ///
    /// # Errors
    ///
    /// As [`File::get`].
    pub async fn get<T: FromDyn>(&self, path: &str) -> Result<T> {
        self.root().get(path).await
    }

    /// Reads an object from its class description; see [`File::get_dyn`].
    ///
    /// # Errors
    ///
    /// As [`File::get_dyn`].
    pub async fn get_dyn(&self, path: &str) -> Result<DynObject> {
        self.root().get_dyn(path).await
    }

    /// Opens a `TTree`; see [`File::tree`].
    ///
    /// # Errors
    ///
    /// As [`File::tree`].
    pub async fn tree(&self, path: &str) -> Result<AsyncTree<S>> {
        self.root().tree(path).await
    }

    /// Opens an `RNTuple`; see [`File::rntuple`].
    ///
    /// # Errors
    ///
    /// As [`File::rntuple`].
    pub async fn rntuple(&self, path: &str) -> Result<AsyncRNTuple<S>> {
        self.root().rntuple(path).await
    }

    /// The class descriptions stored in the file.
    ///
    /// # Errors
    ///
    /// As [`File::streamer_infos`].
    pub async fn streamer_infos(&self) -> Result<Vec<StreamerInfo>> {
        drive(&self.source, self.len(), Vec::new(), |s| {
            self.file.with_source(s).streamer_infos()
        })
        .await
    }
}

/// A directory of an [`AsyncFile`].
#[derive(Clone)]
pub struct AsyncDirectory<S = Arc<dyn AsyncReadAt>> {
    dir: Directory,
    source: S,
    len: u64,
}

impl<S> fmt::Debug for AsyncDirectory<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AsyncDirectory").field(&self.dir).finish()
    }
}

impl<S: AsyncSource> AsyncDirectory<S> {
    /// The path of this directory from the root (`""` for the root).
    pub fn path(&self) -> &str {
        self.dir.path()
    }

    /// The keys of this directory, every cycle, in file order.
    pub fn keys(&self) -> impl Iterator<Item = KeyInfo<'_>> {
        self.dir.keys()
    }

    /// Finds a key by name; without a cycle, the highest one.
    pub fn key(&self, name: &str, cycle: Option<u16>) -> Option<KeyInfo<'_>> {
        self.dir.key(name, cycle)
    }

    async fn run<T>(&self, run: impl FnMut(Directory) -> Result<T>) -> Result<T> {
        let mut run = run;
        drive(&self.source, self.len, Vec::new(), |s| {
            run(self.dir.with_source(s))
        })
        .await
    }

    /// Opens a subdirectory; see [`Directory::subdir`].
    ///
    /// # Errors
    ///
    /// As [`Directory::subdir`].
    pub async fn subdir(&self, path: &str) -> Result<Self> {
        let dir = self.run(|d| d.subdir(path)).await?;
        Ok(Self {
            dir: dir.with_source(detached()),
            source: self.source.clone(),
            len: self.len,
        })
    }

    /// Reads an object into its typed model; see [`Directory::get`].
    ///
    /// # Errors
    ///
    /// As [`Directory::get`].
    pub async fn get<T: FromDyn>(&self, path: &str) -> Result<T> {
        T::from_dyn(&self.get_dyn(path).await?)
    }

    /// Reads an object from its class description; see
    /// [`Directory::get_dyn`].
    ///
    /// # Errors
    ///
    /// As [`Directory::get_dyn`].
    pub async fn get_dyn(&self, path: &str) -> Result<DynObject> {
        self.run(|d| d.get_dyn(path)).await
    }

    /// Reads an object's decompressed bytes; see [`Directory::read_raw`].
    ///
    /// # Errors
    ///
    /// As [`Directory::read_raw`].
    pub async fn read_raw(&self, name: &str) -> Result<Vec<u8>> {
        self.run(|d| d.read_raw(name)).await
    }

    /// Opens a `TTree`; see [`Directory::tree`].
    ///
    /// # Errors
    ///
    /// As [`Directory::tree`].
    pub async fn tree(&self, path: &str) -> Result<AsyncTree<S>> {
        let tree = self.run(|d| d.tree(path)).await?;
        Ok(AsyncTree::new(&tree, self.source.clone()))
    }

    /// Opens an `RNTuple`; see [`Directory::rntuple`].
    ///
    /// # Errors
    ///
    /// As [`Directory::rntuple`].
    pub async fn rntuple(&self, path: &str) -> Result<AsyncRNTuple<S>> {
        let nt = self.run(|d| d.rntuple(path)).await?;
        Ok(AsyncRNTuple::new(&nt, self.source.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(a: u64, b: u64) -> Range<FileOffset> {
        FileOffset::new(a)..FileOffset::new(b)
    }

    #[test]
    fn prefetched_serves_fetched_bytes_and_remembers_the_first_miss() {
        let p = Prefetched::new(100);
        p.insert(&[at(10, 20)], vec![Bytes::from(vec![7_u8; 10])])
            .expect("insert");
        assert_eq!(p.read_at(at(12, 15)).expect("inside").len(), 3);
        assert!(
            p.read_ranges(&[at(10, 20), at(30, 40), at(50, 60)])
                .is_err()
        );
        assert!(p.read_at(at(0, 5)).is_err());
        assert_eq!(p.take_missing(), Some(vec![at(30, 40), at(50, 60)]));
        assert_eq!(p.take_missing(), None);
        // Past the end is an error of the read itself, not a miss.
        assert_eq!(
            p.read_at(at(90, 110)).expect_err("eof").kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(p.take_missing(), None);
        assert!(
            p.insert(&[at(0, 4)], vec![Bytes::from_static(b"ab")])
                .is_err()
        );
    }
}
