//! An HTTP(S) source [§4.3]: `Range` requests, checked and retried.
//!
//! - Several ranges go in one multi-range request (`Range: bytes=a-b,c-d`),
//!   answered as `multipart/byteranges`. When a server answers `200` (it
//!   ignores ranges in that form) the body is not read, and the source
//!   switches to single ranges, at most `max_parallel` at a time.
//! - Every response is checked against the request: status `206`, each
//!   `Content-Range`, the total length, the body length. A truncated or
//!   mislabelled body is never decoded.
//! - Transient failures (5xx, 429, connection or body errors) are retried
//!   with exponential backoff, `Retry-After` honoured.
//! - The file must not change while it is read: requests carry the `ETag`
//!   (`If-Match`) or `Last-Modified` (`If-Unmodified-Since`) seen at open,
//!   and a `412` is an error.

use std::io;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use reqwest::StatusCode;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{ACCEPT_RANGES, CONTENT_LENGTH, HeaderName, RANGE};
use rhizoid_source::{FileOffset, ReadAt};

use super::http_common::{
    Failure, HttpOptions, assemble, boundary, check_part, check_status, content_range, header,
    invalid, parse_multipart, range_spec, transport, validator,
};
use super::{Coalesce, ReadCache};

/// An HTTP client shared by many sources, with a cap on requests in flight.
/// Cheap to clone; clones share the client and the cap. There is no global
/// pool: create one and pass it to the sources that should share it.
#[derive(Clone, Debug)]
pub struct HttpPool {
    client: Client,
    permits: Arc<Permits>,
}

#[derive(Debug)]
struct Permits {
    free: Mutex<usize>,
    freed: Condvar,
}

struct Permit<'a>(&'a Permits);

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        *self.0.free.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        self.0.freed.notify_one();
    }
}

impl HttpPool {
    /// A pool with at most 64 requests in flight and a 30 s timeout.
    ///
    /// # Errors
    ///
    /// If the HTTP client cannot be built (TLS initialisation).
    pub fn new() -> io::Result<Self> {
        Self::with_limits(64, Duration::from_secs(30))
    }

    /// A pool with the given cap on requests in flight (at least 1) and
    /// per-request timeout.
    ///
    /// # Errors
    ///
    /// If the HTTP client cannot be built.
    pub fn with_limits(max_in_flight: usize, timeout: Duration) -> io::Result<Self> {
        let client = Client::builder()
            .timeout(timeout)
            .user_agent(concat!("rhizoid/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(io::Error::other)?;
        Ok(Self {
            client,
            permits: Arc::new(Permits {
                free: Mutex::new(max_in_flight.max(1)),
                freed: Condvar::new(),
            }),
        })
    }

    fn acquire(&self) -> Permit<'_> {
        let p = &*self.permits;
        let mut free = p.free.lock().unwrap_or_else(PoisonError::into_inner);
        while *free == 0 {
            free = p.freed.wait(free).unwrap_or_else(PoisonError::into_inner);
        }
        *free -= 1;
        Permit(p)
    }
}

/// A file served over HTTP(S), read with `Range` requests.
#[derive(Debug)]
pub struct HttpSource {
    url: String,
    pool: HttpPool,
    options: HttpOptions,
    len: u64,
    /// The precondition that pins the version read at open.
    validator: Option<(HeaderName, String)>,
    multi_range: AtomicBool,
}

impl HttpSource {
    /// Opens `url` with a pool of its own and the default options.
    ///
    /// # Errors
    ///
    /// If the server cannot be reached, the file does not exist, or the
    /// server does not serve byte ranges.
    pub fn new(url: impl Into<String>) -> io::Result<Self> {
        Self::with(url, HttpPool::new()?, HttpOptions::default())
    }

    /// Opens `url` with a shared pool and options.
    ///
    /// # Errors
    ///
    /// As [`HttpSource::new`].
    pub fn with(url: impl Into<String>, pool: HttpPool, options: HttpOptions) -> io::Result<Self> {
        let mut source = Self {
            url: url.into(),
            pool,
            options,
            len: 0,
            validator: None,
            multi_range: AtomicBool::new(options.multi_range),
        };
        source.probe()?;
        Ok(source)
    }

    /// `url` as remote files are best read: coalesced range requests
    /// (bounded, see [`Coalesce`]) and a cache of the head and recent
    /// metadata reads.
    ///
    /// # Errors
    ///
    /// As [`HttpSource::new`].
    pub fn open(url: impl Into<String>) -> io::Result<ReadCache<Coalesce<Self>>> {
        Ok(ReadCache::new(Coalesce::new(Self::new(url)?)))
    }

    /// The URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Runs `attempt` with retries.
    fn retrying<T>(&self, mut attempt: impl FnMut() -> Result<T, Failure>) -> io::Result<T> {
        let mut wait = self.options.backoff;
        for n in 0..=self.options.retries {
            match attempt() {
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
                    std::thread::sleep(after.unwrap_or(wait).max(wait));
                    wait = wait.saturating_mul(2);
                }
            }
        }
        Err(io::Error::other("no attempt was made"))
    }

    fn request(&self, method: reqwest::Method) -> RequestBuilder {
        let mut req = self.pool.client.request(method, &self.url);
        if let Some((name, value)) = &self.validator {
            req = req.header(name, value);
        }
        req
    }

    /// Learns the length and a validator: `HEAD`, or a one-byte `GET` when
    /// `HEAD` is refused.
    fn probe(&mut self) -> io::Result<()> {
        let head = self.retrying(|| {
            let _permit = self.pool.acquire();
            let resp = self
                .pool
                .client
                .head(&self.url)
                .send()
                .map_err(|e| transport(&e))?;
            if resp.status() == StatusCode::METHOD_NOT_ALLOWED
                || resp.status() == StatusCode::NOT_IMPLEMENTED
            {
                return Ok(None);
            }
            check_status(resp.status(), resp.headers())?;
            Ok(Some(resp.headers().clone()))
        })?;
        let (len, headers) = if let Some(h) = head {
            {
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
            }
        } else {
            {
                let h = self.retrying(|| {
                    let _permit = self.pool.acquire();
                    let resp = self
                        .pool
                        .client
                        .get(&self.url)
                        .header(RANGE, "bytes=0-0")
                        .send()
                        .map_err(|e| transport(&e))?;
                    check_status(resp.status(), resp.headers())?;
                    if resp.status() != StatusCode::PARTIAL_CONTENT {
                        return Err(Failure::Fatal(io::Error::new(
                            io::ErrorKind::Unsupported,
                            "the server does not serve byte ranges",
                        )));
                    }
                    Ok(resp.headers().clone())
                })?;
                let total = content_range(&h)
                    .and_then(|(_, _, t)| t)
                    .ok_or_else(|| invalid("the server gave no total length"))?;
                (total, h)
            }
        };
        self.len = len;
        self.validator = validator(&headers);
        Ok(())
    }

    /// One range, as one `206` response.
    fn get_range(&self, r: &Range<u64>) -> io::Result<Bytes> {
        if r.start >= r.end {
            return Ok(Bytes::new());
        }
        self.retrying(|| {
            let _permit = self.pool.acquire();
            let resp = self
                .request(reqwest::Method::GET)
                .header(RANGE, format!("bytes={}-{}", r.start, r.end - 1))
                .send()
                .map_err(|e| transport(&e))?;
            check_status(resp.status(), resp.headers())?;
            if resp.status() != StatusCode::PARTIAL_CONTENT {
                // Not read: it would be the whole file.
                return Err(Failure::Fatal(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("the server ignored the byte range (HTTP {})", resp.status()),
                )));
            }
            let got = content_range(resp.headers());
            check_part(self.len, got, r)?;
            let body = resp.bytes().map_err(|e| transport(&e))?;
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
    }

    /// Several ranges in one request. `Ok(None)` when the server does not
    /// answer multi-range requests with the ranges asked.
    fn get_multi(&self, ranges: &[Range<u64>]) -> io::Result<Option<Vec<Bytes>>> {
        let spec = range_spec(ranges);
        self.retrying(|| {
            let _permit = self.pool.acquire();
            let resp = self
                .request(reqwest::Method::GET)
                .header(RANGE, spec.as_str())
                .send()
                .map_err(|e| transport(&e))?;
            check_status(resp.status(), resp.headers())?;
            if resp.status() != StatusCode::PARTIAL_CONTENT {
                return Ok(None); // a 200: dropped unread
            }
            let headers = resp.headers().clone();
            let boundary = boundary(&headers);
            let body = resp.bytes().map_err(|e| transport(&e))?;
            let parts = if let Some(b) = boundary {
                parse_multipart(&body, &b).map_err(|e| Failure::Retry(e, None))?
            } else {
                {
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
                }
            };
            assemble(&parts, ranges)
        })
    }

    /// Single ranges, at most `max_parallel` at a time.
    fn get_parallel(&self, ranges: &[Range<u64>]) -> io::Result<Vec<Bytes>> {
        let next = AtomicUsize::new(0);
        let out: Mutex<Vec<Option<io::Result<Bytes>>>> =
            Mutex::new((0..ranges.len()).map(|_| None).collect());
        let workers = self.options.max_parallel.min(ranges.len());
        std::thread::scope(|s| {
            for _ in 0..workers {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(r) = ranges.get(i) else { break };
                        let got = self.get_range(r);
                        let failed = got.is_err();
                        if let Some(slot) = out
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .get_mut(i)
                        {
                            *slot = Some(got);
                        }
                        if failed {
                            // Stop handing out work; the error is reported.
                            next.store(ranges.len(), Ordering::Relaxed);
                            break;
                        }
                    }
                });
            }
        });
        let results = out.into_inner().unwrap_or_else(PoisonError::into_inner);
        if let Some(Some(Err(e))) = results.iter().find(|r| matches!(r, Some(Err(_)))) {
            return Err(io::Error::new(e.kind(), e.to_string()));
        }
        results
            .into_iter()
            .map(|r| r.unwrap_or_else(|| Err(io::Error::other("a range was not read"))))
            .collect()
    }
}

impl ReadAt for HttpSource {
    fn len(&self) -> io::Result<u64> {
        Ok(self.len)
    }

    fn read_at(&self, range: Range<FileOffset>) -> io::Result<Bytes> {
        let r = range.start.get()..range.end.get();
        if r.end > self.len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("{r:?} is beyond the {} bytes of the file", self.len),
            ));
        }
        self.get_range(&r)
    }

    fn read_ranges(&self, ranges: &[Range<FileOffset>]) -> io::Result<Vec<Bytes>> {
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
        let wanted: Vec<usize> = (0..rs.len()).filter(|&i| rs[i].start < rs[i].end).collect();
        let mut out = vec![Bytes::new(); rs.len()];
        let mut todo: Vec<usize> = wanted;
        if self.multi_range.load(Ordering::Relaxed) && todo.len() > 1 {
            let mut left = Vec::new();
            for chunk in todo.chunks(self.options.max_ranges_per_request) {
                let chunk_ranges: Vec<Range<u64>> = chunk.iter().map(|&i| rs[i].clone()).collect();
                if let Some(parts) = self.get_multi(&chunk_ranges)? {
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
        let single: Vec<Range<u64>> = todo.iter().map(|&i| rs[i].clone()).collect();
        for (&i, b) in todo.iter().zip(self.get_parallel(&single)?) {
            out[i] = b;
        }
        Ok(out)
    }
}
