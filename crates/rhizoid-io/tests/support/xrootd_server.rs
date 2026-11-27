//! A small XRootD server for source tests, written from the protocol
//! (`XProtocol.hh`): handshake, `kXR_protocol`, `kXR_login`, `kXR_auth`
//! (`unix`), `kXR_open`, `kXR_stat`, `kXR_read`, `kXR_readv`, `kXR_close`.
//! Switchable behaviours: a manager that redirects opens to a data server
//! with a token, authentication required, TLS required, and injected
//! faults (waits, asynchronous answers, transient errors, dropped
//! connections, stalls, redirects while reading, a file that changes).

#![allow(dead_code, reason = "each test file uses part of it")]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

const OK: u16 = 0;
const OKSOFAR: u16 = 4000;
const ATTN: u16 = 4001;
const ERROR: u16 = 4003;
const REDIRECT: u16 = 4004;
const WAIT: u16 = 4005;
const WAITRESP: u16 = 4006;

/// What the server asks of logins.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Auth {
    #[default]
    None,
    Unix,
    GsiOnly,
}

/// What the server does; change it while it runs.
#[derive(Debug, Default)]
pub(crate) struct Behaviour {
    pub(crate) auth: Mutex<Auth>,
    /// Flags `kXR_gotoTLS` in the protocol answer.
    pub(crate) tls_required: AtomicBool,
    /// Redirect every open to this port (with `?token=secret`).
    pub(crate) redirect_to: AtomicU16,
    /// Refuse opens without `token=secret` in the CGI.
    pub(crate) require_token: AtomicBool,
    /// Answer opens without the stat (the client then asks `kXR_stat`).
    pub(crate) no_stat_in_open: AtomicBool,
    /// Data answers go out in `kXR_oksofar` parts of this many bytes.
    pub(crate) part_size: AtomicUsize,
    /// Answer the next N data requests with `kXR_wait 0`.
    pub(crate) wait_next: AtomicUsize,
    /// Answer the next N data requests with `kXR_waitresp`, then the
    /// answer inside `kXR_attn`.
    pub(crate) waitresp_next: AtomicUsize,
    /// Answer the next N data requests with `kXR_Overloaded`.
    pub(crate) overloaded_next: AtomicUsize,
    /// Close the connection on the next N data requests.
    pub(crate) drop_next: AtomicUsize,
    /// Never answer the next N data requests.
    pub(crate) stall_next: AtomicUsize,
    /// Redirect the next N data requests to this same server.
    pub(crate) redirect_read_next: AtomicUsize,
    /// Add this to the size reported for opens.
    pub(crate) size_skew: AtomicU64,
    /// Return this many bytes too few for the next N `kXR_read`s.
    pub(crate) short_next: AtomicUsize,
    pub(crate) connections: AtomicU64,
    pub(crate) logins: AtomicU64,
    pub(crate) opens: AtomicU64,
    pub(crate) reads: AtomicU64,
    pub(crate) readvs: AtomicU64,
    pub(crate) readv_elements: AtomicU64,
    pub(crate) closes: AtomicU64,
    pub(crate) stats: AtomicU64,
    /// User names received by `kXR_auth`.
    pub(crate) users: Mutex<Vec<String>>,
    /// Paths (with CGI) received by `kXR_open`.
    pub(crate) opened: Mutex<Vec<String>>,
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

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Server {
    /// Serves `files` (name → bytes) at `root://127.0.0.1:PORT//NAME`.
    pub(crate) fn start(files: Vec<(String, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let behaviour = Arc::new(Behaviour::default());
        let files: Arc<HashMap<String, Vec<u8>>> = Arc::new(
            files
                .into_iter()
                .map(|(n, d)| (format!("/{n}"), d))
                .collect(),
        );
        let b = Arc::clone(&behaviour);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (files, b) = (Arc::clone(&files), Arc::clone(&b));
                b.connections.fetch_add(1, Ordering::Relaxed);
                std::thread::spawn(move || {
                    let _ = serve(stream, &files, &b, port);
                });
            }
        });
        Self { port, behaviour }
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    pub(crate) fn url(&self, name: &str) -> String {
        format!("root://127.0.0.1:{}//{name}", self.port)
    }
}

fn frame(sid: [u8; 2], status: u16, body: &[u8]) -> Vec<u8> {
    let mut f = sid.to_vec();
    f.extend_from_slice(&status.to_be_bytes());
    f.extend_from_slice(&u32::try_from(body.len()).expect("len").to_be_bytes());
    f.extend_from_slice(body);
    f
}

fn error(sid: [u8; 2], code: i32, message: &str) -> Vec<u8> {
    let mut body = code.to_be_bytes().to_vec();
    body.extend_from_slice(message.as_bytes());
    body.push(0);
    frame(sid, ERROR, &body)
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b.try_into().expect("4 bytes"))
}

fn be64(b: &[u8]) -> u64 {
    u64::from_be_bytes(b.try_into().expect("8 bytes"))
}

/// A data answer, in `kXR_oksofar` parts when asked.
fn data_frames(sid: [u8; 2], data: &[u8], part: usize) -> Vec<u8> {
    if part == 0 || data.len() <= part {
        return frame(sid, OK, data);
    }
    let mut out = Vec::new();
    let mut chunks = data.chunks(part).peekable();
    while let Some(c) = chunks.next() {
        let status = if chunks.peek().is_some() { OKSOFAR } else { OK };
        out.extend(frame(sid, status, c));
    }
    out
}

#[expect(
    clippy::too_many_lines,
    reason = "one request handler, read top to bottom"
)]
fn serve(
    mut stream: TcpStream,
    files: &HashMap<String, Vec<u8>>,
    b: &Behaviour,
    port: u16,
) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let mut hs = [0u8; 20];
    stream.read_exact(&mut hs)?;
    assert_eq!(&hs[12..], &[0, 0, 0, 4, 0, 0, 0x07, 0xDC], "handshake");
    let mut reply = vec![0, 0, 0, 0, 0, 0, 0, 8];
    reply.extend_from_slice(&0x520u32.to_be_bytes());
    reply.extend_from_slice(&1u32.to_be_bytes()); // kXR_DataServer
    stream.write_all(&reply)?;
    let mut open: HashMap<[u8; 4], (String, u64)> = HashMap::new();
    let mut next_fh = 1u32;
    let mut logged_in = false;
    loop {
        let mut h = [0u8; 24];
        if stream.read_exact(&mut h).is_err() {
            return Ok(());
        }
        let sid = [h[0], h[1]];
        let request = u16::from_be_bytes([h[2], h[3]]);
        let params = &h[4..20];
        let dlen = usize::try_from(be32(&h[20..])).expect("dlen");
        let mut data = vec![0u8; dlen];
        stream.read_exact(&mut data)?;
        let is_data = matches!(request, 3013 | 3025);
        if is_data {
            if take(&b.drop_next) {
                stream.shutdown(Shutdown::Both)?;
                return Ok(());
            }
            if take(&b.stall_next) {
                continue;
            }
            if take(&b.wait_next) {
                let mut body = 0i32.to_be_bytes().to_vec();
                body.extend_from_slice(b"busy\0");
                stream.write_all(&frame(sid, WAIT, &body))?;
                continue;
            }
            if take(&b.overloaded_next) {
                stream.write_all(&error(sid, 3024, "overloaded"))?;
                continue;
            }
            if take(&b.redirect_read_next) {
                let mut body = i32::from(port).to_be_bytes().to_vec();
                body.extend_from_slice(b"127.0.0.1");
                stream.write_all(&frame(sid, REDIRECT, &body))?;
                continue;
            }
        }
        let out = match request {
            3006 => {
                let mut flags = 1u32; // kXR_isServer
                if b.tls_required.load(Ordering::Relaxed) {
                    flags |= 0x4000_0000;
                }
                let mut body = 0x520u32.to_be_bytes().to_vec();
                body.extend_from_slice(&flags.to_be_bytes());
                frame(sid, OK, &body)
            }
            3007 => {
                b.logins.fetch_add(1, Ordering::Relaxed);
                let mut body = vec![7u8; 16];
                match *lock(&b.auth) {
                    Auth::None => logged_in = true,
                    Auth::Unix => body.extend_from_slice(b"&P=gsi,v:10400,c:ssl&P=unix\0"),
                    Auth::GsiOnly => body.extend_from_slice(b"&P=gsi,v:10400,c:ssl\0"),
                }
                frame(sid, OK, &body)
            }
            3000 => {
                assert_eq!(&params[12..], b"unix");
                let ok = data.starts_with(b"unix\0") && data.ends_with(b"\0");
                if ok {
                    let user = String::from_utf8_lossy(&data[5..data.len() - 1]).into_owned();
                    lock(&b.users).push(user);
                    logged_in = true;
                    frame(sid, OK, &[])
                } else {
                    error(sid, 3030, "bad credentials")
                }
            }
            _ if !logged_in => error(sid, 3010, "not logged in"),
            3010 => {
                b.opens.fetch_add(1, Ordering::Relaxed);
                let full = String::from_utf8_lossy(&data).into_owned();
                lock(&b.opened).push(full.clone());
                let (path, cgi) = full.split_once('?').unwrap_or((&full, ""));
                let options = u16::from_be_bytes([params[2], params[3]]);
                assert_eq!(options & 0x0010, 0x0010, "kXR_open_read");
                let to = b.redirect_to.load(Ordering::Relaxed);
                if to != 0 {
                    let mut body = i32::from(to).to_be_bytes().to_vec();
                    body.extend_from_slice(b"127.0.0.1?token=secret&x=1");
                    frame(sid, REDIRECT, &body)
                } else if b.require_token.load(Ordering::Relaxed)
                    && !cgi.split('&').any(|kv| kv == "token=secret")
                {
                    error(sid, 3010, "no token")
                } else if let Some(file) = files.get(path) {
                    let fh = next_fh.to_be_bytes();
                    next_fh += 1;
                    let size = u64::try_from(file.len()).expect("size")
                        + b.size_skew.load(Ordering::Relaxed);
                    open.insert(fh, (path.to_owned(), size));
                    let mut body = fh.to_vec();
                    if options & 0x0400 != 0 && !b.no_stat_in_open.load(Ordering::Relaxed) {
                        body.extend_from_slice(&[0; 8]);
                        body.extend_from_slice(format!("42 {size} 16 1700000000\0").as_bytes());
                    }
                    frame(sid, OK, &body)
                } else if path == "/dir" {
                    let fh = next_fh.to_be_bytes();
                    let mut body = fh.to_vec();
                    body.extend_from_slice(&[0; 8]);
                    body.extend_from_slice(b"1 4096 2 1700000000\0");
                    frame(sid, OK, &body)
                } else {
                    error(sid, 3011, "no such file")
                }
            }
            3017 => {
                b.stats.fetch_add(1, Ordering::Relaxed);
                let fh: [u8; 4] = params[12..].try_into().expect("fh");
                match open.get(&fh) {
                    Some((_, size)) => {
                        frame(sid, OK, format!("42 {size} 16 1700000000\0").as_bytes())
                    }
                    None => error(sid, 3004, "file not open"),
                }
            }
            3013 => {
                b.reads.fetch_add(1, Ordering::Relaxed);
                let fh: [u8; 4] = params[..4].try_into().expect("fh");
                let offset = usize::try_from(be64(&params[4..12])).expect("offset");
                let len = usize::try_from(be32(&params[12..])).expect("len");
                match open.get(&fh) {
                    Some((path, _)) => {
                        let file = &files[path];
                        let end = (offset + len).min(file.len());
                        let mut bytes = file.get(offset.min(end)..end).unwrap_or_default();
                        if take(&b.short_next) && !bytes.is_empty() {
                            bytes = &bytes[..bytes.len() - 1];
                        }
                        data_frames(sid, bytes, b.part_size.load(Ordering::Relaxed))
                    }
                    None => error(sid, 3004, "file not open"),
                }
            }
            3025 => {
                b.readvs.fetch_add(1, Ordering::Relaxed);
                assert!(dlen % 16 == 0 && dlen / 16 <= 1024, "readv list");
                let mut body = Vec::new();
                let mut bad = None;
                for e in data.as_chunks::<16>().0 {
                    b.readv_elements.fetch_add(1, Ordering::Relaxed);
                    let fh: [u8; 4] = e[..4].try_into().expect("fh");
                    let len = usize::try_from(be32(&e[4..8])).expect("len");
                    let offset = usize::try_from(be64(&e[8..])).expect("offset");
                    assert!(len <= 2_097_136, "readv element of {len} bytes");
                    let Some((path, _)) = open.get(&fh) else {
                        bad = Some(error(sid, 3004, "file not open"));
                        break;
                    };
                    let file = &files[path];
                    let end = (offset + len).min(file.len());
                    let bytes = file.get(offset.min(end)..end).unwrap_or_default();
                    body.extend_from_slice(&fh);
                    body.extend_from_slice(&u32::try_from(bytes.len()).expect("len").to_be_bytes());
                    body.extend_from_slice(&u64::try_from(offset).expect("offset").to_be_bytes());
                    body.extend_from_slice(bytes);
                }
                bad.unwrap_or_else(|| data_frames(sid, &body, b.part_size.load(Ordering::Relaxed)))
            }
            3003 => {
                b.closes.fetch_add(1, Ordering::Relaxed);
                let fh: [u8; 4] = params[..4].try_into().expect("fh");
                open.remove(&fh);
                frame(sid, OK, &[])
            }
            _ => error(sid, 3013, "unsupported request"),
        };
        if is_data && take(&b.waitresp_next) {
            // "Wait for an answer", then the answer, asynchronously.
            stream.write_all(&frame(sid, WAITRESP, &1i32.to_be_bytes()))?;
            std::thread::sleep(Duration::from_millis(20));
            let mut attn = 5008i32.to_be_bytes().to_vec();
            attn.extend_from_slice(&[0; 4]);
            // One embedded response: join the parts.
            let joined = join_parts(&out);
            attn.extend_from_slice(&joined);
            stream.write_all(&frame([0, 0], ATTN, &attn))?;
            continue;
        }
        stream.write_all(&out)?;
    }
}

/// The frames of an answer as one `kXR_ok` frame.
fn join_parts(frames: &[u8]) -> Vec<u8> {
    let mut sid = [0u8; 2];
    let mut status = OK;
    let mut body = Vec::new();
    let mut pos = 0;
    while pos + 8 <= frames.len() {
        sid = [frames[pos], frames[pos + 1]];
        status = u16::from_be_bytes([frames[pos + 2], frames[pos + 3]]);
        let len = usize::try_from(be32(&frames[pos + 4..pos + 8])).expect("len");
        body.extend_from_slice(&frames[pos + 8..pos + 8 + len]);
        pos += 8 + len;
    }
    frame(sid, if status == OKSOFAR { OK } else { status }, &body)
}
