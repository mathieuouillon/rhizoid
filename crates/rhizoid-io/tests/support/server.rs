//! A small HTTP/1.1 file server for source tests, with switchable
//! behaviours: multi-range or not, ranges ignored, `HEAD` refused, and
//! injected faults (503s, truncated bodies, wrong `Content-Range`, a
//! changed `ETag`).

#![allow(dead_code, reason = "each test file uses part of it")]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read as _, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// What the server does; change it while it runs.
#[derive(Debug, Default)]
pub(crate) struct Behaviour {
    /// Answer multi-range requests with `multipart/byteranges` (else `200`).
    pub(crate) multipart: AtomicBool,
    /// Ignore every `Range` header (answer `200` with the whole file).
    pub(crate) ignore_ranges: AtomicBool,
    /// Answer `HEAD` with `405`.
    pub(crate) no_head: AtomicBool,
    /// Answer the next N requests with `503`.
    pub(crate) fail_next: AtomicUsize,
    /// Cut the body of the next N range responses in half.
    pub(crate) truncate_next: AtomicUsize,
    /// Shift the `Content-Range` of the next N range responses.
    pub(crate) wrong_range_next: AtomicUsize,
    /// The current `ETag`.
    pub(crate) etag: Mutex<String>,
    /// Requests served.
    pub(crate) requests: AtomicU64,
    /// Body bytes sent in `206` responses.
    pub(crate) range_bytes: AtomicU64,
}

pub(crate) struct Server {
    port: u16,
    pub(crate) behaviour: Arc<Behaviour>,
}

fn take(counter: &AtomicUsize) -> bool {
    let mut n = counter.load(Ordering::Relaxed);
    while n > 0 {
        match counter.compare_exchange(n, n - 1, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(now) => n = now,
        }
    }
    false
}

impl Server {
    /// Serves `files` (name → bytes) at `http://127.0.0.1:PORT/NAME`.
    pub(crate) fn start(files: Vec<(String, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let behaviour = Arc::new(Behaviour {
            multipart: AtomicBool::new(true),
            etag: Mutex::new("\"v1\"".to_owned()),
            ..Behaviour::default()
        });
        let files: Arc<HashMap<String, Vec<u8>>> = Arc::new(files.into_iter().collect());
        let b = Arc::clone(&behaviour);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (files, b) = (Arc::clone(&files), Arc::clone(&b));
                std::thread::spawn(move || {
                    let _ = serve(stream, &files, &b);
                });
            }
        });
        Self { port, behaviour }
    }

    pub(crate) fn url(&self, name: &str) -> String {
        format!("http://127.0.0.1:{}/{name}", self.port)
    }
}

fn parse_ranges(v: &str, len: u64) -> Option<Vec<(u64, u64)>> {
    let spec = v.trim().strip_prefix("bytes=")?;
    spec.split(',')
        .map(|r| {
            let (a, b) = r.trim().split_once('-')?;
            let a: u64 = a.parse().ok()?;
            let b: u64 = if b.is_empty() {
                len - 1
            } else {
                b.parse().ok()?
            };
            (a <= b && b < len).then_some((a, b))
        })
        .collect()
}

#[expect(
    clippy::too_many_lines,
    reason = "one request handler, read top to bottom"
)]
fn serve(
    stream: TcpStream,
    files: &HashMap<String, Vec<u8>>,
    b: &Behaviour,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    // Headers and body go out in separate writes: without this, Linux holds
    // the second until the client's delayed ACK (40 ms a request).
    stream.set_nodelay(true)?;
    let mut out = stream;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let mut parts = line.split_whitespace();
        let (method, path) = (
            parts.next().unwrap_or_default().to_owned(),
            parts.next().unwrap_or_default().to_owned(),
        );
        let mut headers = HashMap::new();
        loop {
            let mut h = String::new();
            reader.read_line(&mut h)?;
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
            }
        }
        if let Some(n) = headers
            .get("content-length")
            .and_then(|v| v.parse::<usize>().ok())
        {
            let mut sink = vec![0; n];
            reader.read_exact(&mut sink)?;
        }
        b.requests.fetch_add(1, Ordering::Relaxed);
        let etag = b
            .etag
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let respond = |out: &mut TcpStream,
                       status: &str,
                       extra: &[(&str, String)],
                       body: &[u8]|
         -> std::io::Result<()> {
            let mut head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nETag: {etag}\r\nAccept-Ranges: bytes\r\n",
                body.len()
            );
            for (k, v) in extra {
                let _ = write!(head, "{k}: {v}\r\n");
            }
            head.push_str("\r\n");
            out.write_all(head.as_bytes())?;
            if method != "HEAD" {
                out.write_all(body)?;
            }
            out.flush()
        };
        let Some(data) = files.get(path.trim_start_matches('/')) else {
            respond(&mut out, "404 Not Found", &[], b"")?;
            continue;
        };
        if take(&b.fail_next) {
            respond(
                &mut out,
                "503 Service Unavailable",
                &[("Retry-After", "0".to_owned())],
                b"",
            )?;
            continue;
        }
        if method == "HEAD" {
            if b.no_head.load(Ordering::Relaxed) {
                respond(&mut out, "405 Method Not Allowed", &[], b"")?;
            } else {
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: {etag}\r\nAccept-Ranges: bytes\r\n\r\n",
                    data.len()
                );
                out.write_all(head.as_bytes())?;
                out.flush()?;
            }
            continue;
        }
        if headers.get("if-match").is_some_and(|m| *m != etag) {
            respond(&mut out, "412 Precondition Failed", &[], b"")?;
            continue;
        }
        let len = u64::try_from(data.len()).expect("small");
        let ranges = headers.get("range").and_then(|r| parse_ranges(r, len));
        let ranges = match ranges {
            Some(r) if !b.ignore_ranges.load(Ordering::Relaxed) => r,
            _ => {
                respond(&mut out, "200 OK", &[], data)?;
                continue;
            }
        };
        let slice = |(a, e): (u64, u64)| {
            &data[usize::try_from(a).expect("small")..=usize::try_from(e).expect("small")]
        };
        if ranges.len() > 1 && !b.multipart.load(Ordering::Relaxed) {
            respond(&mut out, "200 OK", &[], data)?;
            continue;
        }
        let (body, content_type, content_range) = if let [(a, e)] = ranges[..] {
            let shift = u64::from(take(&b.wrong_range_next));
            (
                slice((a, e)).to_vec(),
                None,
                Some(format!("bytes {}-{}/{len}", a + shift, e + shift)),
            )
        } else {
            let mut body = Vec::new();
            for &(a, e) in &ranges {
                body.extend_from_slice(format!("\r\n--RHZ\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes {a}-{e}/{len}\r\n\r\n").as_bytes());
                body.extend_from_slice(slice((a, e)));
            }
            body.extend_from_slice(b"\r\n--RHZ--\r\n");
            (
                body,
                Some("multipart/byteranges; boundary=RHZ".to_owned()),
                None,
            )
        };
        let mut extra = Vec::new();
        if let Some(ct) = content_type {
            extra.push(("Content-Type", ct));
        }
        if let Some(cr) = content_range {
            extra.push(("Content-Range", cr));
        }
        if take(&b.truncate_next) {
            let mut head = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nETag: {etag}\r\n",
                body.len()
            );
            for (k, v) in &extra {
                let _ = write!(head, "{k}: {v}\r\n");
            }
            head.push_str("\r\n");
            out.write_all(head.as_bytes())?;
            out.write_all(&body[..body.len() / 2])?;
            out.flush()?;
            let _ = out.shutdown(Shutdown::Both);
            return Ok(());
        }
        b.range_bytes.fetch_add(
            ranges.iter().map(|(a, e)| e - a + 1).sum::<u64>(),
            Ordering::Relaxed,
        );
        respond(&mut out, "206 Partial Content", &extra, &body)?;
    }
}
