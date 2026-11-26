//! What the HTTP sources share: options, response checks, multipart
//! parsing. The transports are in `http` (blocking) and `http_async`.

use std::io;
use std::ops::Range;
use std::time::Duration;

use bytes::Bytes;
use reqwest::StatusCode;
use reqwest::header::{
    CONTENT_RANGE, CONTENT_TYPE, ETAG, HeaderMap, HeaderName, IF_MATCH, IF_UNMODIFIED_SINCE,
    LAST_MODIFIED, RETRY_AFTER,
};

/// How an HTTP source retries and splits its requests.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct HttpOptions {
    pub(super) retries: u32,
    pub(super) backoff: Duration,
    pub(super) max_parallel: usize,
    pub(super) multi_range: bool,
    pub(super) max_ranges_per_request: usize,
}

impl Default for HttpOptions {
    /// 3 retries from 100 ms, 10 requests in parallel, multi-range requests
    /// of up to 64 ranges.
    fn default() -> Self {
        Self {
            retries: 3,
            backoff: Duration::from_millis(100),
            max_parallel: 10,
            multi_range: true,
            max_ranges_per_request: 64,
        }
    }
}

impl HttpOptions {
    /// Retries after a transient failure. The default is 3.
    #[must_use]
    pub fn retries(mut self, n: u32) -> Self {
        self.retries = n;
        self
    }

    /// The first wait before a retry, doubled each time. The default is
    /// 100 ms.
    #[must_use]
    pub fn backoff(mut self, first: Duration) -> Self {
        self.backoff = first;
        self
    }

    /// Single-range requests run in parallel, at most this many (at least
    /// 1). The default is 10.
    #[must_use]
    pub fn max_parallel(mut self, n: usize) -> Self {
        self.max_parallel = n.max(1);
        self
    }

    /// Whether to try multi-range requests. The default is `true`.
    #[must_use]
    pub fn multi_range(mut self, yes: bool) -> Self {
        self.multi_range = yes;
        self
    }
}

/// Why an attempt failed.
pub(super) enum Failure {
    /// Worth retrying, after at least this long.
    Retry(io::Error, Option<Duration>),
    Fatal(io::Error),
}

pub(super) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// `bytes a-b/total` → `(a, b + 1, total)`.
pub(super) fn content_range(headers: &HeaderMap) -> Option<(u64, u64, Option<u64>)> {
    let v = headers.get(CONTENT_RANGE)?.to_str().ok()?;
    parse_content_range(v)
}

fn parse_content_range(v: &str) -> Option<(u64, u64, Option<u64>)> {
    let rest = v.trim().strip_prefix("bytes ")?;
    let (span, total) = rest.split_once('/')?;
    let (a, b) = span.split_once('-')?;
    let (a, b): (u64, u64) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    let total = total.trim().parse().ok();
    (a <= b).then_some((a, b + 1, total))
}

pub(super) fn header(headers: &HeaderMap, name: &HeaderName) -> Option<String> {
    headers.get(name)?.to_str().ok().map(str::to_owned)
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let secs: u64 = header(headers, &RETRY_AFTER)?.trim().parse().ok()?;
    Some(Duration::from_secs(secs.min(30)))
}

/// Classifies a response status: success passes, the rest fail.
pub(super) fn check_status(status: StatusCode, headers: &HeaderMap) -> Result<(), Failure> {
    if status == StatusCode::PRECONDITION_FAILED {
        return Err(Failure::Fatal(invalid(
            "the file changed on the server while it was read",
        )));
    }
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        return Err(Failure::Retry(
            io::Error::other(format!("HTTP {status}")),
            retry_after(headers),
        ));
    }
    if status == StatusCode::NOT_FOUND {
        return Err(Failure::Fatal(io::Error::new(
            io::ErrorKind::NotFound,
            "HTTP 404 Not Found",
        )));
    }
    if !status.is_success() {
        return Err(Failure::Fatal(io::Error::other(format!("HTTP {status}"))));
    }
    Ok(())
}

pub(super) fn transport(e: &reqwest::Error) -> Failure {
    let err = io::Error::other(e.to_string());
    // reqwest has no connection errors of its own in the browser.
    #[cfg(not(target_arch = "wasm32"))]
    let connect = e.is_connect();
    #[cfg(target_arch = "wasm32")]
    let connect = false;
    if connect || e.is_timeout() || e.is_request() || e.is_body() || e.is_decode() {
        Failure::Retry(err, None)
    } else {
        Failure::Fatal(err)
    }
}

/// The multipart boundary of a `multipart/byteranges` response.
pub(super) fn boundary(headers: &HeaderMap) -> Option<String> {
    let ct = header(headers, &CONTENT_TYPE)?;
    if !ct.to_ascii_lowercase().contains("multipart/byteranges") {
        return None;
    }
    let b = ct
        .split(';')
        .find_map(|p| p.trim().strip_prefix("boundary="))?;
    Some(b.trim_matches('"').to_owned())
}

/// The precondition that pins the version read: a strong `ETag`
/// (`If-Match`), else `Last-Modified` (`If-Unmodified-Since`).
pub(super) fn validator(headers: &HeaderMap) -> Option<(HeaderName, String)> {
    match (header(headers, &ETAG), header(headers, &LAST_MODIFIED)) {
        (Some(etag), _) if !etag.starts_with("W/") => Some((IF_MATCH, etag)),
        (_, Some(date)) => Some((IF_UNMODIFIED_SINCE, date)),
        _ => None,
    }
}

/// Checks a part's `Content-Range` against the range asked, and the total
/// against the length seen at open.
pub(super) fn check_part(
    len: u64,
    got: Option<(u64, u64, Option<u64>)>,
    want: &Range<u64>,
) -> Result<(), Failure> {
    match got {
        Some((a, b, total))
            if a == want.start && b == want.end && total.is_none_or(|t| t == len) =>
        {
            Ok(())
        }
        Some((a, b, Some(t))) if t != len => Err(Failure::Fatal(invalid(format!(
            "the file is now {t} bytes, it was {len} (Content-Range {a}-{b})"
        )))),
        other => Err(Failure::Fatal(invalid(format!(
            "Content-Range {other:?} does not match the requested {want:?}"
        )))),
    }
}

/// Picks out the ranges asked from the parts received; `None` if one is
/// missing.
pub(super) fn assemble(
    parts: &[(u64, u64, Bytes)],
    ranges: &[Range<u64>],
) -> Result<Option<Vec<Bytes>>, Failure> {
    let mut out = Vec::with_capacity(ranges.len());
    for r in ranges {
        let Some((a, _, bytes)) = parts.iter().find(|(a, e, _)| *a <= r.start && r.end <= *e)
        else {
            return Ok(None);
        };
        let (lo, hi) = (
            usize::try_from(r.start - a).map_err(|e| Failure::Fatal(io::Error::other(e)))?,
            usize::try_from(r.end - a).map_err(|e| Failure::Fatal(io::Error::other(e)))?,
        );
        out.push(bytes.slice(lo..hi));
    }
    Ok(Some(out))
}

/// The `Range` header value of several ranges.
pub(super) fn range_spec(ranges: &[Range<u64>]) -> String {
    let spec: Vec<String> = ranges
        .iter()
        .map(|r| format!("{}-{}", r.start, r.end - 1))
        .collect();
    format!("bytes={}", spec.join(","))
}

/// The parts of a `multipart/byteranges` body: `(start, end, bytes)`.
pub(super) fn parse_multipart(body: &Bytes, boundary: &str) -> io::Result<Vec<(u64, u64, Bytes)>> {
    let delim = format!("--{boundary}");
    let find = |from: usize, needle: &[u8]| -> Option<usize> {
        body.get(from..)?
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|p| p + from)
    };
    let mut parts = Vec::new();
    let mut pos = find(0, delim.as_bytes()).ok_or_else(|| invalid("no multipart boundary"))?;
    loop {
        pos += delim.len();
        if body.get(pos..pos + 2) == Some(b"--") {
            break;
        }
        let headers_end =
            find(pos, b"\r\n\r\n").ok_or_else(|| invalid("truncated multipart headers"))?;
        let headers = String::from_utf8_lossy(body.get(pos..headers_end).unwrap_or_default());
        let range = headers
            .lines()
            .find_map(|l| {
                let (name, value) = l.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("content-range")
                    .then(|| parse_content_range(value))?
            })
            .ok_or_else(|| invalid("a multipart part has no Content-Range"))?;
        let start = headers_end + 4;
        let len = usize::try_from(range.1 - range.0).map_err(io::Error::other)?;
        let end = start + len;
        if end > body.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated multipart body",
            ));
        }
        parts.push((range.0, range.1, body.slice(start..end)));
        pos = find(end, delim.as_bytes()).ok_or_else(|| invalid("unterminated multipart body"))?;
    }
    Ok(parts)
}
