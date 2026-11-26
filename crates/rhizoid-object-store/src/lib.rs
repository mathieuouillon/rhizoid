//! Read ROOT files from object stores with **rhizoid**: S3, GCS, Azure,
//! HTTP and local files, through the [`object_store`] crate.
//!
//! ```
//! use std::sync::{Arc, OnceLock};
//! use object_store::local::LocalFileSystem;
//! use rhizoid_object_store::ObjectStoreSource;
//! # let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../rhizoid-io/tests/data");
//!
//! let store = Arc::new(LocalFileSystem::new_with_prefix(dir)?);
//! let source = ObjectStoreSource::open(store, "uproot_trees.root")?;   // coalesced and cached
//! let file = rhizoid_io::File::from_source(source, rhizoid_io::ReadOptions::default())?;
//! assert_eq!(file.tree("events")?.num_entries(), 1001);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! With the `aws`, `gcp`, `azure` or `http` feature,
//! [`ObjectStoreSource::from_url`] opens `s3://…`, `gs://…`, `az://…` or
//! `https://…` URLs, configured from the environment as `object_store`
//! does.
//!
//! # How reads are made
//!
//! - Each range is one `get` with a byte range; several run concurrently
//!   (10 at a time). `object_store`'s own `get_ranges` is not used: it
//!   merges ranges across 1 MiB gaps, which on ROOT's interleaved baskets
//!   can download most of a file. Wrap the source in
//!   [`Coalesce`] (as [`ObjectStoreSource::open`]
//!   does) for merging within rhizoid's bounds.
//! - Every request carries the `ETag` seen at open (`If-Match`), so a file
//!   replaced while it is read is an error, not a mix of two versions.
//! - Every response's length is checked against the request.
//! - `ReadAt` (sync) runs the requests on one runtime shared by every
//!   source in the process (two threads, started on first use); from
//!   inside an async runtime, use the [`AsyncReadAt`] implementation and
//!   the `_async` constructors instead (the sync methods return an error
//!   there rather than block it).
//! - A store pools its connections on the runtime that opened them. Use
//!   one store for sync sources and another for async ones: a connection
//!   pooled by a runtime that is not running stalls the request that picks
//!   it up.
//!
//! Versioned with `object_store`: this release uses `object_store` 0.14.

use std::future::Future;
use std::io;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use futures_util::future::BoxFuture;
use futures_util::{StreamExt as _, TryStreamExt as _};
use object_store::path::Path;
use object_store::{GetOptions, GetRange, ObjectStore, ObjectStoreExt as _};
use rhizoid_io::io::{Coalesce, ReadCache};
use rhizoid_source::{AsyncReadAt, FileOffset, ReadAt};

/// Requests in flight for one `read_ranges` call.
const MAX_PARALLEL: usize = 10;

/// A file in an object store, readable by `rhizoid-io`.
#[derive(Debug)]
pub struct ObjectStoreSource {
    store: Arc<dyn ObjectStore>,
    path: Path,
    len: u64,
    e_tag: Option<String>,
}

fn store_error(e: &object_store::Error) -> io::Error {
    let kind = match e {
        object_store::Error::NotFound { .. } => io::ErrorKind::NotFound,
        object_store::Error::Precondition { .. } => io::ErrorKind::InvalidData,
        object_store::Error::NotSupported { .. } => io::ErrorKind::Unsupported,
        _ => io::ErrorKind::Other,
    };
    let message = match e {
        object_store::Error::Precondition { .. } => {
            format!("the object changed in the store while it was read: {e}")
        }
        _ => e.to_string(),
    };
    io::Error::new(kind, message)
}

impl ObjectStoreSource {
    /// The object at `path` in `store`.
    ///
    /// # Errors
    ///
    /// If the shared runtime cannot start, or the object's metadata cannot
    /// be read (`NotFound` if it does not exist).
    pub fn new(store: Arc<dyn ObjectStore>, path: impl Into<Path>) -> io::Result<Self> {
        let path = path.into();
        let meta = block_on(store.head(&path))?.map_err(|e| store_error(&e))?;
        Ok(Self::with_meta(store, path, meta))
    }

    /// The object at `path` in `store`, from async code (for
    /// [`AsyncReadAt`], `rhizoid_io::File::open_async`).
    ///
    /// # Errors
    ///
    /// If the object's metadata cannot be read (`NotFound` if it does not
    /// exist).
    pub async fn new_async(store: Arc<dyn ObjectStore>, path: impl Into<Path>) -> io::Result<Self> {
        let path = path.into();
        let meta = store.head(&path).await.map_err(|e| store_error(&e))?;
        Ok(Self::with_meta(store, path, meta))
    }

    /// [`new_async`](Self::new_async), with bounded coalescing.
    ///
    /// # Errors
    ///
    /// As [`new_async`](Self::new_async).
    pub async fn open_async(
        store: Arc<dyn ObjectStore>,
        path: impl Into<Path>,
    ) -> io::Result<Coalesce<Self>> {
        Ok(Coalesce::new(Self::new_async(store, path).await?))
    }

    fn with_meta(store: Arc<dyn ObjectStore>, path: Path, meta: object_store::ObjectMeta) -> Self {
        Self {
            store,
            path,
            len: meta.size,
            e_tag: meta.e_tag,
        }
    }

    /// [`new`](Self::new), wrapped as remote files are best read: bounded
    /// coalescing and a cache of the head and recent metadata reads.
    ///
    /// # Errors
    ///
    /// As [`new`](Self::new).
    pub fn open(
        store: Arc<dyn ObjectStore>,
        path: impl Into<Path>,
    ) -> io::Result<ReadCache<Coalesce<Self>>> {
        Ok(ReadCache::new(Coalesce::new(Self::new(store, path)?)))
    }

    /// The object named by a URL (`s3://bucket/key`, `gs://…`, `az://…`,
    /// `https://…`, `file:///…`), with the store configured from the
    /// environment (credentials, region), as `object_store` does.
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a URL `object_store` does not support with the
    /// enabled features; then as [`new`](Self::new).
    pub fn from_url(url: &str) -> io::Result<Self> {
        let (store, path) = parse_url(url)?;
        Self::new(store, path)
    }

    /// [`from_url`](Self::from_url), from async code.
    ///
    /// # Errors
    ///
    /// As [`from_url`](Self::from_url).
    pub async fn from_url_async(url: &str) -> io::Result<Self> {
        let (store, path) = parse_url(url)?;
        Self::new_async(store, path).await
    }

    /// The object's path in its store.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn get(&self, r: Range<u64>) -> impl Future<Output = io::Result<Bytes>> + Send + 'static {
        let (store, path, e_tag, len) = (
            Arc::clone(&self.store),
            self.path.clone(),
            self.e_tag.clone(),
            self.len,
        );
        async move {
            if r.start >= r.end {
                return Ok(Bytes::new());
            }
            if r.end > len {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("{r:?} is beyond the {len} bytes of the object"),
                ));
            }
            let options = GetOptions {
                range: Some(GetRange::Bounded(r.clone())),
                if_match: e_tag,
                ..GetOptions::default()
            };
            let bytes = store
                .get_opts(&path, options)
                .await
                .map_err(|e| store_error(&e))?
                .bytes()
                .await
                .map_err(|e| store_error(&e))?;
            if u64::try_from(bytes.len()).ok() != Some(r.end - r.start) {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("{} bytes received for {r:?}", bytes.len()),
                ));
            }
            Ok(bytes)
        }
    }

    fn get_many(
        &self,
        ranges: &[Range<FileOffset>],
    ) -> impl Future<Output = io::Result<Vec<Bytes>>> + Send + 'static {
        let requests: Vec<_> = ranges
            .iter()
            .map(|r| self.get(r.start.get()..r.end.get()))
            .collect();
        futures_util::stream::iter(requests)
            .buffered(MAX_PARALLEL)
            .try_collect()
    }
}

fn parse_url(url: &str) -> io::Result<(Arc<dyn ObjectStore>, Path)> {
    let parsed = url::Url::parse(url)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("{url}: {e}")))?;
    let (store, path) = object_store::parse_url_opts(&parsed, std::env::vars())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("{url}: {e}")))?;
    Ok((Arc::from(store), path))
}

/// The runtime of the sync methods: one per process, started on first use.
/// Its worker threads keep driving the connections a store pools between
/// calls; a runtime per source would leave another live source's pooled
/// connections undriven, and requests on them would stall until timeout.
static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// Runs `f` to completion on [`RUNTIME`], unless called from inside an
/// async runtime, where blocking would stall (or panic) it.
fn block_on<F: Future>(f: F) -> io::Result<F::Output> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the synchronous source was called from inside an async runtime; use its AsyncReadAt implementation",
        ));
    }
    let rt = if let Some(rt) = RUNTIME.get() {
        rt
    } else {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("rhizoid-object-store")
            .enable_all()
            .build()?;
        RUNTIME.get_or_init(|| rt)
    };
    Ok(rt.block_on(f))
}

impl ReadAt for ObjectStoreSource {
    fn len(&self) -> io::Result<u64> {
        Ok(self.len)
    }

    fn read_at(&self, range: Range<FileOffset>) -> io::Result<Bytes> {
        block_on(self.get(range.start.get()..range.end.get()))?
    }

    fn read_ranges(&self, ranges: &[Range<FileOffset>]) -> io::Result<Vec<Bytes>> {
        block_on(self.get_many(ranges))?
    }
}

impl AsyncReadAt for ObjectStoreSource {
    fn len(&self) -> BoxFuture<'_, io::Result<u64>> {
        let len = self.len;
        Box::pin(async move { Ok(len) })
    }

    fn read_at(&self, range: Range<FileOffset>) -> BoxFuture<'_, io::Result<Bytes>> {
        Box::pin(self.get(range.start.get()..range.end.get()))
    }

    fn read_ranges(&self, ranges: Vec<Range<FileOffset>>) -> BoxFuture<'_, io::Result<Vec<Bytes>>> {
        Box::pin(self.get_many(&ranges))
    }
}
