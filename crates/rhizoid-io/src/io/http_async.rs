//! [`AsyncHttpSource`]: the async twin of the blocking HTTP source, for
//! async code on native targets (reqwest on tokio) and in the browser
//! (`fetch`).

use std::future::Future;
use std::io;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use futures_util::{StreamExt as _, TryStreamExt as _};
use reqwest::header::{ACCEPT_RANGES, CONTENT_LENGTH, HeaderName, RANGE};
use reqwest::{Client, Method, RequestBuilder, StatusCode};
#[cfg(not(target_arch = "wasm32"))]
use rhizoid_source::{AsyncReadAt, BoxFuture};
use rhizoid_source::{FileOffset, LocalAsyncReadAt, LocalBoxFuture};

use super::Coalesce;
use super::http_common::{
    Failure, HttpOptions, assemble, boundary, check_part, check_status, content_range, header,
    invalid, parse_multipart, range_spec, transport, validator,
};

/// A file served over HTTP(S), read with `Range` requests from async code:
/// the requests, checks and retries of the blocking `HttpSource` —
/// multi-range requests with a fallback to single ranges, every response
/// checked, transient failures retried with backoff, the version pinned
/// with `If-Match` / `If-Unmodified-Since`.
///
/// - On native targets it implements [`AsyncReadAt`] and runs on tokio,
///   as reqwest does.
/// - In the browser (`wasm32`) it uses `fetch` and implements
///   [`LocalAsyncReadAt`] (its futures are not `Send`): open it with
///   `File::open_local_async`. The server must allow the origin (CORS) and
///   expose `Content-Range`, `Content-Length` and `ETag` to it.
///
/// Concurrency is per read: single-range requests run at most
/// `max_parallel` at a time. Share one [`Client`] between sources to share
/// connections.
#[derive(Debug)]
pub struct AsyncHttpSource {
    url: String,
    client: Client,
    options: HttpOptions,
    len: u64,
    validator: Option<(HeaderName, String)>,
    multi_range: AtomicBool,
}

/// Waits, on any executor.
async fn sleep(d: Duration) {
    futures_timer::Delay::new(d).await;
}

impl AsyncHttpSource {
    /// Opens `url` with a client of its own and the default options.
    ///
    /// # Errors
    ///
    /// If the server cannot be reached, the file does not exist, or the
    /// server does not serve byte ranges.
    pub async fn new(url: impl Into<String>) -> io::Result<Self> {
        Self::with(url, Self::client()?, HttpOptions::default()).await
    }

    /// Opens `url` with a shared client and options.
    ///
    /// # Errors
    ///
    /// As [`AsyncHttpSource::new`].
    pub async fn with(
        url: impl Into<String>,
        client: Client,
        options: HttpOptions,
    ) -> io::Result<Self> {
        let mut source = Self {
            url: url.into(),
            client,
            options,
            len: 0,
            validator: None,
            multi_range: AtomicBool::new(options.multi_range),
        };
        source.probe().await?;
        Ok(source)
    }

    /// `url` with bounded coalescing ([`Coalesce`]), as remote files are
    /// best read.
    ///
    /// # Errors
    ///
    /// As [`AsyncHttpSource::new`].
    pub async fn open(url: impl Into<String>) -> io::Result<Coalesce<Self>> {
        Ok(Coalesce::new(Self::new(url).await?))
    }

    /// A client as the sources make their own: rhizoid's user agent and,
    /// on native targets, a 30 s timeout.
    ///
    /// # Errors
    ///
    /// If the client cannot be built (TLS initialisation).
    pub fn client() -> io::Result<Client> {
        let builder = Client::builder().user_agent(concat!("rhizoid/", env!("CARGO_PKG_VERSION")));
        #[cfg(not(target_arch = "wasm32"))]
        let builder = builder.timeout(Duration::from_secs(30));
        builder.build().map_err(io::Error::other)
    }

    /// The URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Runs `attempt` with retries.
    async fn retrying<T, F: Future<Output = Result<T, Failure>>>(
        &self,
        mut attempt: impl FnMut() -> F,
    ) -> io::Result<T> {
        let mut wait = self.options.backoff;
        for n in 0..=self.options.retries {
            match attempt().await {
                Ok(v) => return Ok(v),
                Err(Failure::Fatal(e)) => return Err(e),
                Err(Failure::Retry(e, after)) => {
                    if n == self.options.retries {
                        return Err(io::Error::new(
                            e.kind(),
                            format!(
                                "{} (after {} retries) for {}",
                                e, self.options.retries, self.url
                            ),
                        ));
                    }
                    sleep(after.unwrap_or(wait).max(wait)).await;
                    wait = wait.saturating_mul(2);
                }
            }
        }
        Err(io::Error::other("no attempt was made"))
    }

    fn request(&self, method: Method) -> RequestBuilder {
        let mut req = self.client.request(method, &self.url);
        if let Some((name, value)) = &self.validator {
            req = req.header(name, value);
        }
        req
    }

    /// Learns the length and a validator: `HEAD`, or a one-byte `GET` when
    /// `HEAD` is refused.
    async fn probe(&mut self) -> io::Result<()> {
        let head = self
            .retrying(|| async {
                let resp = self
                    .client
                    .head(&self.url)
                    .send()
                    .await
                    .map_err(|e| transport(&e))?;
                if resp.status() == StatusCode::METHOD_NOT_ALLOWED
                    || resp.status() == StatusCode::NOT_IMPLEMENTED
                {
                    return Ok(None);
                }
                check_status(resp.status(), resp.headers())?;
                Ok(Some(resp.headers().clone()))
            })
            .await?;
        let (len, headers) = if let Some(h) = head {
            let len = header(&h, &CONTENT_LENGTH)
                .and_then(|v| v.trim().parse().ok())
                .ok_or_else(|| invalid("the server gave no Content-Length"))?;
            if header(&h, &ACCEPT_RANGES).is_some_and(|v| v.trim() == "none") {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "the server does not serve byte ranges",
                ));
            }
            (len, h)
        } else {
            let h = self
                .retrying(|| async {
                    let resp = self
                        .client
                        .get(&self.url)
                        .header(RANGE, "bytes=0-0")
                        .send()
                        .await
                        .map_err(|e| transport(&e))?;
                    check_status(resp.status(), resp.headers())?;
                    if resp.status() != StatusCode::PARTIAL_CONTENT {
                        return Err(Failure::Fatal(io::Error::new(
                            io::ErrorKind::Unsupported,
                            "the server does not serve byte ranges",
                        )));
                    }
                    Ok(resp.headers().clone())
                })
                .await?;
            let total = content_range(&h)
                .and_then(|(_, _, t)| t)
                .ok_or_else(|| invalid("the server gave no total length"))?;
            (total, h)
        };
        self.len = len;
        self.validator = validator(&headers);
        Ok(())
    }

    /// One range, as one `206` response.
    async fn get_range(&self, r: Range<u64>) -> io::Result<Bytes> {
        if r.start >= r.end {
            return Ok(Bytes::new());
        }
        self.retrying(|| async {
            let resp = self
                .request(Method::GET)
                .header(RANGE, format!("bytes={}-{}", r.start, r.end - 1))
                .send()
                .await
                .map_err(|e| transport(&e))?;
            check_status(resp.status(), resp.headers())?;
            if resp.status() != StatusCode::PARTIAL_CONTENT {
                // Not read: it would be the whole file.
                return Err(Failure::Fatal(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("the server ignored the byte range (HTTP {})", resp.status()),
                )));
            }
            check_part(self.len, content_range(resp.headers()), &r)?;
            let body = resp.bytes().await.map_err(|e| transport(&e))?;
            if u64::try_from(body.len()).ok() != Some(r.end - r.start) {
                return Err(Failure::Retry(
                    io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!("{} bytes received for {r:?}", body.len()),
                    ),
                    None,
                ));
            }
            Ok(body)
        })
        .await
    }

    /// Several ranges in one request. `Ok(None)` when the server does not
    /// answer multi-range requests with the ranges asked.
    async fn get_multi(&self, ranges: &[Range<u64>]) -> io::Result<Option<Vec<Bytes>>> {
        let spec = range_spec(ranges);
        self.retrying(|| async {
            let resp = self
                .request(Method::GET)
                .header(RANGE, spec.as_str())
                .send()
                .await
                .map_err(|e| transport(&e))?;
            check_status(resp.status(), resp.headers())?;
            if resp.status() != StatusCode::PARTIAL_CONTENT {
                return Ok(None); // a 200: dropped unread
            }
            let headers = resp.headers().clone();
            let boundary = boundary(&headers);
            let body = resp.bytes().await.map_err(|e| transport(&e))?;
            let parts = if let Some(b) = boundary {
                parse_multipart(&body, &b).map_err(|e| Failure::Retry(e, None))?
            } else {
                let Some((a, e, _)) = content_range(&headers) else {
                    return Ok(None);
                };
                if u64::try_from(body.len()).ok() != Some(e - a) {
                    return Err(Failure::Retry(
                        io::Error::new(io::ErrorKind::UnexpectedEof, "short body"),
                        None,
                    ));
                }
                vec![(a, e, body)]
            };
            assemble(&parts, ranges)
        })
        .await
    }

    async fn read_ranges_impl(&self, ranges: Vec<Range<FileOffset>>) -> io::Result<Vec<Bytes>> {
        let rs: Vec<Range<u64>> = ranges
            .iter()
            .map(|r| r.start.get()..r.end.get().max(r.start.get()))
            .collect();
        if let Some(r) = rs.iter().find(|r| r.end > self.len) {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("{r:?} is beyond the {} bytes of the file", self.len),
            ));
        }
        let mut out = vec![Bytes::new(); rs.len()];
        let mut todo: Vec<usize> = (0..rs.len()).filter(|&i| rs[i].start < rs[i].end).collect();
        if self.multi_range.load(Ordering::Relaxed) && todo.len() > 1 {
            let mut left = Vec::new();
            for chunk in todo.chunks(self.options.max_ranges_per_request) {
                let chunk_ranges: Vec<Range<u64>> = chunk.iter().map(|&i| rs[i].clone()).collect();
                if let Some(parts) = self.get_multi(&chunk_ranges).await? {
                    for (&i, b) in chunk.iter().zip(parts) {
                        out[i] = b;
                    }
                } else {
                    self.multi_range.store(false, Ordering::Relaxed);
                    left.extend_from_slice(chunk);
                }
            }
            todo = left;
        }
        // Owned indices: a closure over `&usize` defeats the `Send` check.
        let requests: Vec<_> = todo
            .clone()
            .into_iter()
            .map(|i| self.get_range(rs[i].clone()))
            .collect();
        let singles: Vec<Bytes> = futures_util::stream::iter(requests)
            .buffered(self.options.max_parallel)
            .try_collect()
            .await?;
        for (&i, b) in todo.iter().zip(singles) {
            out[i] = b;
        }
        Ok(out)
    }

    async fn read_at_impl(&self, range: Range<FileOffset>) -> io::Result<Bytes> {
        let r = range.start.get()..range.end.get();
        if r.end > self.len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("{r:?} is beyond the {} bytes of the file", self.len),
            ));
        }
        self.get_range(r).await
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl AsyncReadAt for AsyncHttpSource {
    fn len(&self) -> BoxFuture<'_, io::Result<u64>> {
        Box::pin(core::future::ready(Ok(self.len)))
    }

    fn read_at(&self, range: Range<FileOffset>) -> BoxFuture<'_, io::Result<Bytes>> {
        Box::pin(self.read_at_impl(range))
    }

    fn read_ranges(&self, ranges: Vec<Range<FileOffset>>) -> BoxFuture<'_, io::Result<Vec<Bytes>>> {
        Box::pin(self.read_ranges_impl(ranges))
    }
}

impl LocalAsyncReadAt for AsyncHttpSource {
    fn len(&self) -> LocalBoxFuture<'_, io::Result<u64>> {
        Box::pin(core::future::ready(Ok(self.len)))
    }

    fn read_at(&self, range: Range<FileOffset>) -> LocalBoxFuture<'_, io::Result<Bytes>> {
        Box::pin(self.read_at_impl(range))
    }

    fn read_ranges(
        &self,
        ranges: Vec<Range<FileOffset>>,
    ) -> LocalBoxFuture<'_, io::Result<Vec<Bytes>>> {
        Box::pin(self.read_ranges_impl(ranges))
    }
}
