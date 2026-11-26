//! Moving bytes [§4.3]: the source traits (re-exported from
//! `rhizoid-source`), adapters that make remote reads cheap
//! ([`Coalesce`], [`ReadCache`]), and, with the `http` feature, an HTTP
//! source ([`HttpSource`], [`HttpPool`]).
//!
//! Adapters compose: a remote file is typically read through
//! `ReadCache<Coalesce<HttpSource>>`, which [`HttpSource::open`] builds.

mod cache;
mod coalesce;
#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
mod http;
#[cfg(all(feature = "http", feature = "async"))]
mod http_async;
#[cfg(feature = "http")]
mod http_common;
mod inline;
mod stats;

pub use rhizoid_source::{AsyncReadAt, FileOffset, LocalAsyncReadAt, ReadAt, WriteAt};

pub use cache::ReadCache;
pub use coalesce::{Coalesce, CoalesceOptions};
#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
pub use http::{HttpPool, HttpSource};
#[cfg(all(feature = "http", feature = "async"))]
pub use http_async::AsyncHttpSource;
#[cfg(feature = "http")]
pub use http_common::HttpOptions;
pub use inline::Inline;
pub use stats::{Counted, SourceStats};
