//! A `root://` source: the XRootD protocol over TCP, in pure Rust (no
//! `libXrdCl`).
//!
//! - One connection per source, multiplexed: requests carry stream ids, a
//!   reader thread hands each response to its request, so reads from many
//!   threads (or tasks) are in flight together.
//! - Opening follows the protocol: handshake, `kXR_protocol`, `kXR_login`
//!   (anonymous, or the `unix` protocol when the server asks for it),
//!   `kXR_open` with the file's stat. Redirects (`kXR_redirect`, as EOS
//!   managers send to their storage nodes, with capabilities in the CGI)
//!   are followed, waits (`kXR_wait`, `kXR_waitresp`) honoured.
//! - Reads: `kXR_read`, and `kXR_readv` for several ranges (at most 1024
//!   elements of at most 2 MiB each, as servers accept).
//! - Every answer is checked against the request (lengths, offsets, file
//!   handle); a response larger than asked closes the connection.
//! - Transient failures (connection lost, timeouts, overloaded or
//!   restarted servers) are retried on a new connection with exponential
//!   backoff, from the URL first given. The file must not change while it
//!   is read: a reopened file must have the size and modification time
//!   seen at first.
//! - Authentication: none, or `unix`. GSI, Kerberos, tokens (`ztn`) and
//!   TLS (`roots://`) are `Unsupported`.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::ops::Range;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
#[cfg(feature = "async")]
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use rhizoid_source::{FileOffset, ReadAt};

use super::xrootd_proto::{
    self as proto, ATTN, FileHandle, Header, MAX_CONTROL_BODY, MAX_READV_ELEMENT,
    MAX_READV_ELEMENTS, OK, OKSOFAR, Outcome, Stat, StreamId, Url, WAITRESP,
};
use super::{Coalesce, ReadCache};

/// The most bytes asked by one request.
const MAX_REQUEST: u64 = 16 << 20;
/// The most `kXR_wait`s honoured for one request.
pub(super) const MAX_WAITS: u32 = 10;

/// How an XRootD source connects, waits and retries.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct XrootdOptions {
    pub(super) retries: u32,
    pub(super) backoff: Duration,
    pub(super) timeout: Duration,
    pub(super) max_redirects: u32,
    pub(super) max_in_flight: usize,
}

impl Default for XrootdOptions {
    /// 3 retries from 100 ms, a 60 s timeout, 16 redirects, 8 requests in
    /// flight per read.
    fn default() -> Self {
        Self {
            retries: 3,
            backoff: Duration::from_millis(100),
            timeout: Duration::from_secs(60),
            max_redirects: 16,
            max_in_flight: 8,
        }
    }
}

impl XrootdOptions {
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

    /// How long to wait to connect, and for each response (longer when
    /// the server asks to wait), at least 1 ms. The default is 60 s.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout.max(Duration::from_millis(1));
        self
    }

    /// Redirects followed for one open or read. The default is 16.
    #[must_use]
    pub fn max_redirects(mut self, n: u32) -> Self {
        self.max_redirects = n;
        self
    }

    /// Requests of one read in flight at once (at least 1). The default
    /// is 8.
    #[must_use]
    pub fn max_in_flight(mut self, n: usize) -> Self {
        self.max_in_flight = n.max(1);
        self
    }
}

/// Why an attempt failed.
#[derive(Debug)]
pub(super) enum Failure {
    /// Worth another attempt, on a new connection.
    Retry(io::Error),
    /// Go there and start again.
    Redirect(Url),
    Fatal(io::Error),
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// A final response: status and body (the `kXR_oksofar` parts joined).
#[derive(Debug)]
pub(super) struct Reply {
    pub(super) status: u16,
    pub(super) body: Vec<u8>,
}

/// Where a response is gathered for its request.
#[derive(Debug)]
struct Slot {
    state: Mutex<SlotState>,
    ready: Condvar,
    /// The most data bytes the request may receive.
    max_body: usize,
}

#[derive(Debug, Default)]
struct SlotState {
    body: Vec<u8>,
    done: Option<io::Result<Reply>>,
    #[cfg(feature = "async")]
    waker: Option<Waker>,
    /// Time the server asked for with `kXR_waitresp`.
    extra: Duration,
}

impl Slot {
    fn finish(&self, result: io::Result<Reply>) {
        let mut s = lock(&self.state);
        if s.done.is_none() {
            s.done = Some(result);
        }
        #[cfg(feature = "async")]
        if let Some(w) = s.waker.take() {
            w.wake();
        }
        drop(s);
        self.ready.notify_all();
    }
}

/// The requests in flight on a connection, by stream id.
#[derive(Debug, Default)]
pub(super) struct Demux {
    slots: Mutex<Slots>,
}

#[derive(Debug, Default)]
struct Slots {
    map: HashMap<StreamId, Arc<Slot>>,
    next: u16,
    /// Why the connection is gone.
    broken: Option<(io::ErrorKind, String)>,
}

impl Demux {
    fn register(&self, max_body: usize) -> io::Result<(StreamId, Arc<Slot>)> {
        let mut s = lock(&self.slots);
        if let Some((kind, why)) = &s.broken {
            return Err(io::Error::new(*kind, why.clone()));
        }
        if s.map.len() >= 0x8000 {
            return Err(io::Error::other("too many XRootD requests in flight"));
        }
        let sid = loop {
            s.next = s.next.wrapping_add(1).max(1); // 0 is the server's own
            let sid = s.next.to_be_bytes();
            if !s.map.contains_key(&sid) {
                break sid;
            }
        };
        let slot = Arc::new(Slot {
            state: Mutex::new(SlotState::default()),
            ready: Condvar::new(),
            max_body,
        });
        s.map.insert(sid, Arc::clone(&slot));
        Ok((sid, slot))
    }

    fn get(&self, sid: StreamId) -> Option<Arc<Slot>> {
        lock(&self.slots).map.get(&sid).cloned()
    }

    fn remove(&self, sid: StreamId, slot: &Arc<Slot>) {
        let mut s = lock(&self.slots);
        if s.map.get(&sid).is_some_and(|x| Arc::ptr_eq(x, slot)) {
            s.map.remove(&sid);
        }
    }

    fn is_broken(&self) -> bool {
        lock(&self.slots).broken.is_some()
    }

    /// Fails every request in flight and every later one.
    fn break_with(&self, e: &io::Error) {
        let slots: Vec<Arc<Slot>> = {
            let mut s = lock(&self.slots);
            if s.broken.is_none() {
                s.broken = Some((e.kind(), format!("XRootD connection lost: {e}")));
            }
            s.map.drain().map(|(_, slot)| slot).collect()
        };
        for slot in slots {
            slot.finish(Err(io::Error::new(
                e.kind(),
                format!("XRootD connection lost: {e}"),
            )));
        }
    }

    /// Reads one response from the server and hands it to its request.
    /// An error means the stream can no longer be trusted.
    pub(super) fn pump(&self, r: &mut impl Read) -> io::Result<()> {
        let mut h = [0u8; 8];
        r.read_exact(&mut h)?;
        let mut header = proto::parse_header(h)?;
        if header.status == ATTN {
            let Some(rest) = header.len.checked_sub(16) else {
                return discard(r, header.len);
            };
            let mut head = [0u8; 16];
            r.read_exact(&mut head)?;
            match proto::parse_asynresp(&head)? {
                Some(inner) if inner.len == rest => header = inner,
                Some(_) => return Err(invalid("kXR_asynresp lengths disagree")),
                None => return discard(r, rest),
            }
        }
        let Some(slot) = self.get(header.sid) else {
            // A request given up (timed out, or its future dropped).
            return discard(r, header.len);
        };
        let data = matches!(header.status, OK | OKSOFAR);
        let room = if data {
            slot.max_body.saturating_sub(lock(&slot.state).body.len())
        } else {
            MAX_CONTROL_BODY
        };
        if header.len > room {
            return Err(invalid(format!(
                "the server sent {} bytes, more than the request asked",
                header.len
            )));
        }
        let body = read_body(r, header.len)?;
        match header.status {
            OKSOFAR => {
                let mut s = lock(&slot.state);
                if s.body.is_empty() {
                    s.body = body;
                } else {
                    s.body.extend_from_slice(&body);
                }
            }
            WAITRESP => {
                lock(&slot.state).extra = proto::parse_wait(&body);
            }
            status => {
                self.remove(header.sid, &slot);
                let body = if status == OK {
                    let mut s = lock(&slot.state);
                    let mut all = std::mem::take(&mut s.body);
                    if all.is_empty() {
                        all = body;
                    } else {
                        all.extend_from_slice(&body);
                    }
                    all
                } else {
                    body
                };
                slot.finish(Ok(Reply { status, body }));
            }
        }
        Ok(())
    }
}

fn read_body(r: &mut impl Read, len: usize) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    body.try_reserve_exact(len)
        .map_err(|_| io::Error::new(io::ErrorKind::OutOfMemory, "response too large"))?;
    let n = r
        .take(u64::try_from(len).unwrap_or(u64::MAX))
        .read_to_end(&mut body)?;
    if n != len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the connection closed inside a response",
        ));
    }
    Ok(body)
}

fn discard(r: &mut impl Read, len: usize) -> io::Result<()> {
    let len = u64::try_from(len).unwrap_or(u64::MAX);
    if io::copy(&mut r.take(len), &mut io::sink())? != len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the connection closed inside a response",
        ));
    }
    Ok(())
}

/// A request sent, its response awaited.
#[derive(Debug)]
pub(super) struct Ticket {
    sid: StreamId,
    slot: Arc<Slot>,
    demux: Arc<Demux>,
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.demux.remove(self.sid, &self.slot);
    }
}

pub(super) fn timed_out(timeout: Duration) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!("no XRootD response within {timeout:?}"),
    )
}

impl Ticket {
    /// Waits for the response, blocking, at most `timeout` (plus what the
    /// server asked with `kXR_waitresp`).
    pub(super) fn wait(&self, timeout: Duration) -> io::Result<Reply> {
        let start = Instant::now();
        let mut s = lock(&self.slot.state);
        loop {
            if let Some(done) = s.done.take() {
                return done;
            }
            let total = timeout + s.extra;
            let left = total.saturating_sub(start.elapsed());
            if left.is_zero() {
                return Err(timed_out(total));
            }
            s = self
                .slot
                .ready
                .wait_timeout(s, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Polls for the response.
    #[cfg(feature = "async")]
    pub(super) fn poll(&self, cx: &Context<'_>) -> Poll<io::Result<Reply>> {
        let mut s = lock(&self.slot.state);
        if let Some(done) = s.done.take() {
            return Poll::Ready(done);
        }
        s.waker = Some(cx.waker().clone());
        Poll::Pending
    }

    /// Time the server asked to wait for this response.
    #[cfg(feature = "async")]
    pub(super) fn extra(&self) -> Duration {
        lock(&self.slot.state).extra
    }
}

/// A logged-in connection, its responses read by a thread of its own.
#[derive(Debug)]
pub(super) struct Connection {
    demux: Arc<Demux>,
    writer: Mutex<TcpStream>,
}

impl Connection {
    fn start(stream: TcpStream) -> io::Result<Self> {
        stream.set_read_timeout(None)?;
        let mut reader = stream.try_clone()?;
        let demux = Arc::new(Demux::default());
        let d = Arc::clone(&demux);
        std::thread::Builder::new()
            .name("rhizoid-xrootd".into())
            .spawn(move || {
                let e = loop {
                    if let Err(e) = d.pump(&mut reader) {
                        break e;
                    }
                };
                d.break_with(&e);
                let _ = reader.shutdown(Shutdown::Both);
            })?;
        Ok(Self {
            demux,
            writer: Mutex::new(stream),
        })
    }

    /// Sends the request `build` makes with its stream id.
    pub(super) fn submit(
        &self,
        max_body: usize,
        build: impl FnOnce(StreamId) -> Vec<u8>,
    ) -> io::Result<Ticket> {
        let (sid, slot) = self.demux.register(max_body)?;
        let ticket = Ticket {
            sid,
            slot,
            demux: Arc::clone(&self.demux),
        };
        let bytes = build(sid);
        let mut w = lock(&self.writer);
        if let Err(e) = w.write_all(&bytes) {
            let _ = w.shutdown(Shutdown::Both);
            drop(w);
            self.demux.break_with(&e);
            return Err(e);
        }
        Ok(ticket)
    }

    fn is_broken(&self) -> bool {
        self.demux.is_broken()
    }

    pub(super) fn shutdown(&self) {
        let _ = lock(&self.writer).shutdown(Shutdown::Both);
    }
}

impl Drop for Connection {
    /// Ends the reader thread, which holds a clone of the socket.
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Reads one whole response from a stream not yet handed to a reader
/// thread (the handshake and `kXR_protocol`).
fn read_frame(stream: &mut TcpStream) -> io::Result<(Header, Vec<u8>)> {
    let mut h = [0u8; 8];
    stream.read_exact(&mut h)?;
    let header = proto::parse_header(h)?;
    if header.len > MAX_CONTROL_BODY {
        return Err(invalid("oversized response during the handshake"));
    }
    let body = read_body(stream, header.len)?;
    Ok((header, body))
}

pub(super) fn transport(e: io::Error) -> Failure {
    match e.kind() {
        io::ErrorKind::OutOfMemory | io::ErrorKind::InvalidData => Failure::Fatal(e),
        _ => Failure::Retry(e),
    }
}

/// Connects, shakes hands and checks the protocol answer.
fn connect(url: &Url, options: &XrootdOptions) -> Result<Connection, Failure> {
    let addrs = url.endpoint().to_socket_addrs().map_err(Failure::Retry)?;
    let mut last = io::Error::new(io::ErrorKind::NotFound, "the host has no address");
    let mut stream = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, options.timeout) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last = e,
        }
    }
    let mut stream = stream.ok_or_else(|| {
        Failure::Retry(io::Error::new(
            last.kind(),
            format!("cannot connect to {}: {last}", url.endpoint()),
        ))
    })?;
    stream.set_nodelay(true).map_err(transport)?;
    stream
        .set_read_timeout(Some(options.timeout))
        .map_err(transport)?;
    stream
        .set_write_timeout(Some(options.timeout))
        .map_err(transport)?;
    stream
        .write_all(&proto::handshake_and_protocol([0, 1]))
        .map_err(transport)?;
    let (h, body) = read_frame(&mut stream).map_err(transport)?;
    proto::check_handshake(h, &body).map_err(Failure::Fatal)?;
    let (h, body) = read_frame(&mut stream).map_err(transport)?;
    if h.sid != [0, 1] {
        return Err(Failure::Fatal(invalid("unexpected answer to kXR_protocol")));
    }
    match proto::classify(h.status, body, url) {
        Outcome::Done(body) => proto::check_protocol(&body).map_err(Failure::Fatal)?,
        other => return Err(outcome_failure(other)),
    }
    Connection::start(stream).map_err(transport)
}

pub(super) fn outcome_failure(o: Outcome) -> Failure {
    match o {
        Outcome::Done(_) => Failure::Fatal(invalid("unexpected answer")),
        Outcome::Wait(_) => Failure::Retry(io::Error::other("the server keeps asking to wait")),
        Outcome::Redirect(url) => Failure::Redirect(url),
        Outcome::Retry(e) => Failure::Retry(e),
        Outcome::Fatal(e) => Failure::Fatal(e),
    }
}

/// One control request, waits honoured: the body of its `kXR_ok`.
fn call(
    conn: &Connection,
    url: &Url,
    options: &XrootdOptions,
    build: impl Fn(StreamId) -> Vec<u8>,
) -> Result<Vec<u8>, Failure> {
    for _ in 0..=MAX_WAITS {
        let ticket = conn.submit(MAX_CONTROL_BODY, &build).map_err(transport)?;
        let reply = ticket.wait(options.timeout).map_err(transport)?;
        match proto::classify(reply.status, reply.body, url) {
            Outcome::Done(body) => return Ok(body),
            Outcome::Wait(d) => std::thread::sleep(d),
            other => return Err(outcome_failure(other)),
        }
    }
    Err(outcome_failure(Outcome::Wait(Duration::ZERO)))
}

/// An open file on a logged-in connection.
#[derive(Debug)]
pub(super) struct Session {
    pub(super) conn: Connection,
    pub(super) fh: FileHandle,
    pub(super) stat: Stat,
    /// Where the file was opened (after redirects).
    pub(super) url: Url,
}

impl Drop for Session {
    fn drop(&mut self) {
        // Best effort: the server also closes the file with the connection.
        // Not waited for: a drop must not block (it may run in a task).
        let fh = self.fh;
        let _ = self
            .conn
            .submit(MAX_CONTROL_BODY, |sid| proto::close(sid, fh));
        self.conn.shutdown();
    }
}

/// Logs in, authenticating with `unix` when the server asks for it.
fn login(conn: &Connection, url: &Url, options: &XrootdOptions) -> Result<(), Failure> {
    let user = url.user.as_deref().unwrap_or("rhizoid");
    let pid = std::process::id();
    let body = call(conn, url, options, |sid| proto::login(sid, pid, user))?;
    let protocols = proto::login_protocols(&body).map_err(Failure::Fatal)?;
    if protocols.is_empty() {
        return Ok(());
    }
    if !protocols.iter().any(|p| p == "unix") {
        return Err(Failure::Fatal(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "{} requires authentication by {}; only unix (and none) is supported",
                url.endpoint(),
                protocols.join(", ")
            ),
        )));
    }
    call(conn, url, options, |sid| proto::auth_unix(sid, user)).map(drop)
}

/// Connects to `start`, follows redirects, logs in and opens the file.
pub(super) fn open_session(start: &Url, options: &XrootdOptions) -> Result<Session, Failure> {
    let mut url = start.clone();
    for _ in 0..=options.max_redirects {
        let attempt = (|| {
            let conn = connect(&url, options)?;
            login(&conn, &url, options)?;
            let path = url.open_path();
            let body = call(&conn, &url, options, |sid| proto::open(sid, &path))?;
            let (fh, info) = proto::parse_open(&body).map_err(Failure::Fatal)?;
            let info = if let Some(s) = info {
                s
            } else {
                let body = call(&conn, &url, options, |sid| proto::stat(sid, fh))?;
                proto::parse_stat(&body).map_err(Failure::Fatal)?
            };
            Ok(Session {
                conn,
                fh,
                stat: info.check_file().map_err(Failure::Fatal)?,
                url: url.clone(),
            })
        })();
        match attempt {
            Err(Failure::Redirect(to)) => url = to,
            other => return other,
        }
    }
    Err(Failure::Fatal(io::Error::other(format!(
        "more than {} redirects opening {}",
        options.max_redirects,
        start.display()
    ))))
}

/// One request of a read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Op {
    Read { offset: u64, len: u32 },
    ReadV(Vec<(u64, u32)>),
}

impl Op {
    pub(super) fn request(&self, sid: StreamId, fh: FileHandle) -> Vec<u8> {
        match self {
            Self::Read { offset, len } => proto::read(sid, fh, *offset, *len),
            Self::ReadV(elements) => proto::readv(sid, fh, elements),
        }
    }

    pub(super) fn max_body(&self) -> usize {
        let n = match self {
            Self::Read { len, .. } => u64::from(*len),
            Self::ReadV(e) => e.iter().map(|&(_, l)| u64::from(l) + 16).sum(),
        };
        usize::try_from(n).unwrap_or(usize::MAX)
    }

    /// The bytes of each element, checked against the request.
    pub(super) fn decode(&self, body: Vec<u8>, fh: FileHandle) -> Result<Vec<Bytes>, Failure> {
        let body = Bytes::from(body);
        match self {
            Self::Read { offset, len } => {
                if u64::try_from(body.len()).ok() != Some(u64::from(*len)) {
                    return Err(Failure::Retry(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!(
                            "kXR_read gave {} bytes at {offset}, {len} were asked",
                            body.len()
                        ),
                    )));
                }
                Ok(vec![body])
            }
            Self::ReadV(elements) => proto::split_readv(&body, fh, elements).map_err(|e| {
                if e.kind() == io::ErrorKind::UnexpectedEof {
                    Failure::Retry(e)
                } else {
                    Failure::Fatal(e)
                }
            }),
        }
    }
}

/// The requests that read some ranges, and which parts make each range.
#[derive(Debug, Default)]
pub(super) struct Plan {
    pub(super) ops: Vec<Op>,
    /// Per range: `(op, element)` in order.
    parts: Vec<Vec<(usize, usize)>>,
}

impl Plan {
    /// Large ranges are read with `kXR_read` (16 MiB at a time); small ones
    /// go together in `kXR_readv` requests.
    pub(super) fn new(ranges: &[Range<u64>]) -> Self {
        let mut plan = Self {
            ops: Vec::new(),
            parts: vec![Vec::new(); ranges.len()],
        };
        let mut batch: Vec<(u64, u32)> = Vec::new();
        let mut owners: Vec<usize> = Vec::new();
        let mut bytes = 0u64;
        for (i, r) in ranges.iter().enumerate() {
            let len = r.end.saturating_sub(r.start);
            if len == 0 {
                continue;
            }
            if len > MAX_READV_ELEMENT {
                let mut at = r.start;
                while at < r.end {
                    let n = (r.end - at).min(MAX_REQUEST);
                    plan.parts[i].push((plan.ops.len(), 0));
                    plan.ops.push(Op::Read {
                        offset: at,
                        len: u32::try_from(n).unwrap_or(u32::MAX),
                    });
                    at += n;
                }
                continue;
            }
            if batch.len() == MAX_READV_ELEMENTS || bytes + len > MAX_REQUEST {
                plan.flush(&mut batch, &mut owners);
                bytes = 0;
            }
            batch.push((r.start, u32::try_from(len).unwrap_or(u32::MAX)));
            owners.push(i);
            bytes += len;
        }
        plan.flush(&mut batch, &mut owners);
        plan
    }

    fn flush(&mut self, batch: &mut Vec<(u64, u32)>, owners: &mut Vec<usize>) {
        if batch.is_empty() {
            return;
        }
        let op = self.ops.len();
        for (k, &owner) in owners.iter().enumerate() {
            self.parts[owner].push((op, k));
        }
        self.ops.push(if let [(offset, len)] = batch[..] {
            Op::Read { offset, len }
        } else {
            Op::ReadV(batch.clone())
        });
        batch.clear();
        owners.clear();
    }

    /// The bytes of each range, from the bytes of each op.
    pub(super) fn assemble(&self, results: &[Vec<Bytes>]) -> io::Result<Vec<Bytes>> {
        let part = |&(op, k): &(usize, usize)| {
            results
                .get(op)
                .and_then(|r| r.get(k))
                .ok_or_else(|| io::Error::other("a read part is missing"))
        };
        self.parts
            .iter()
            .map(|parts| match parts.as_slice() {
                [] => Ok(Bytes::new()),
                [one] => part(one).cloned(),
                many => {
                    let mut out = BytesMut::new();
                    for p in many {
                        out.extend_from_slice(part(p)?);
                    }
                    Ok(out.freeze())
                }
            })
            .collect()
    }
}

/// Checks ranges against the length; returns them as `u64` ranges.
pub(super) fn check_ranges(ranges: &[Range<FileOffset>], len: u64) -> io::Result<Vec<Range<u64>>> {
    let rs: Vec<Range<u64>> = ranges
        .iter()
        .map(|r| r.start.get()..r.end.get().max(r.start.get()))
        .collect();
    if let Some(r) = rs.iter().find(|r| r.end > len) {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("{r:?} is beyond the {len} bytes of the file"),
        ));
    }
    Ok(rs)
}

/// What the blocking and async sources share: the URL, the file seen at
/// first open, and the current session.
#[derive(Debug)]
pub(super) struct Core {
    pub(super) url: Url,
    pub(super) given: String,
    pub(super) options: XrootdOptions,
    pub(super) len: u64,
    mtime: i64,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    session: Option<Arc<Session>>,
    /// Where the next session starts, after a redirect while reading.
    next: Option<Url>,
}

impl Core {
    /// `start` is where reconnections start: the URL given, not the node
    /// a manager redirected to.
    pub(super) fn new(
        start: Url,
        given: &str,
        options: XrootdOptions,
        first: Arc<Session>,
    ) -> Self {
        Self {
            url: start,
            given: given.to_owned(),
            options,
            len: first.stat.size,
            mtime: first.stat.mtime,
            state: Mutex::new(State {
                session: Some(first),
                next: None,
            }),
        }
    }

    /// The live session, if any.
    pub(super) fn current(&self) -> Option<Arc<Session>> {
        let s = lock(&self.state);
        s.session.as_ref().filter(|s| !s.conn.is_broken()).cloned()
    }

    /// Where a new session starts.
    pub(super) fn start(&self) -> Url {
        lock(&self.state)
            .next
            .take()
            .unwrap_or_else(|| self.url.clone())
    }

    /// Makes `session` current, if it opened the file first seen.
    pub(super) fn install(&self, session: Session) -> Result<Arc<Session>, Failure> {
        if session.stat.size != self.len || session.stat.mtime != self.mtime {
            return Err(Failure::Fatal(invalid(format!(
                "{} changed on the server while it was read ({} bytes, was {})",
                self.url.display(),
                session.stat.size,
                self.len
            ))));
        }
        let session = Arc::new(session);
        lock(&self.state).session = Some(Arc::clone(&session));
        Ok(session)
    }

    /// Drops `session` (if still current), to start again from `next`.
    pub(super) fn invalidate(&self, session: &Arc<Session>, next: Option<Url>) {
        let mut s = lock(&self.state);
        if s.session.as_ref().is_some_and(|x| Arc::ptr_eq(x, session)) {
            s.session = None;
            session.conn.shutdown();
        }
        if next.is_some() {
            s.next = next;
        }
    }

    /// After a failed attempt on `session`: a retry or a redirect starts
    /// on a new connection.
    pub(super) fn after<T>(&self, session: &Arc<Session>, done: &Result<T, Failure>) {
        match done {
            Err(Failure::Retry(_)) => self.invalidate(session, None),
            Err(Failure::Redirect(to)) => self.invalidate(session, Some(to.clone())),
            _ => {}
        }
    }

    /// The error after the last retry.
    pub(super) fn exhausted(&self, e: &io::Error) -> io::Error {
        io::Error::new(
            e.kind(),
            format!(
                "{e} (after {} retries) for {}",
                self.options.retries,
                self.url.display()
            ),
        )
    }
}

/// A file served by XRootD (`root://`), read with `kXR_read` and
/// `kXR_readv` over one multiplexed connection.
///
/// Open it with [`XrootdSource::open`] for remote use: coalesced ranges
/// and a cache of the head and metadata, as for HTTP.
#[derive(Debug)]
pub struct XrootdSource {
    core: Core,
    /// Held while a new session is made: one connection attempt at a time.
    connecting: Mutex<()>,
}

impl XrootdSource {
    /// Opens `url` (`root://host[:port]//path`) with the default options.
    ///
    /// # Errors
    ///
    /// If the URL is not a `root://` URL, the server cannot be reached,
    /// the file does not exist or is a directory, or the server asks for
    /// authentication other than `unix` or for TLS (`Unsupported`).
    pub fn new(url: &str) -> io::Result<Self> {
        Self::with(url, XrootdOptions::default())
    }

    /// Opens `url` with options.
    ///
    /// # Errors
    ///
    /// As [`XrootdSource::new`].
    pub fn with(url: &str, options: XrootdOptions) -> io::Result<Self> {
        let start = Url::parse(url)?;
        let first = retrying(&options, &start, || open_session(&start, &options))?;
        Ok(Self {
            core: Core::new(start, url, options, Arc::new(first)),
            connecting: Mutex::new(()),
        })
    }

    /// `url` as remote files are best read: coalesced reads (bounded, see
    /// [`Coalesce`]) and a cache of the head and recent metadata reads.
    ///
    /// # Errors
    ///
    /// As [`XrootdSource::new`].
    pub fn open(url: &str) -> io::Result<ReadCache<Coalesce<Self>>> {
        Ok(ReadCache::new(Coalesce::new(Self::new(url)?)))
    }

    /// The URL, as given.
    pub fn url(&self) -> &str {
        &self.core.given
    }

    fn session(&self) -> Result<Arc<Session>, Failure> {
        if let Some(s) = self.core.current() {
            return Ok(s);
        }
        let _one = lock(&self.connecting);
        // Another thread may have connected meanwhile.
        if let Some(s) = self.core.current() {
            return Ok(s);
        }
        let start = self.core.start();
        let session = open_session(&start, &self.core.options)?;
        self.core.install(session)
    }

    /// Runs the ops on `session`, filling `results`.
    fn run_on(
        &self,
        session: &Session,
        ops: &[Op],
        results: &mut [Option<Vec<Bytes>>],
    ) -> Result<(), Failure> {
        let todo: Vec<usize> = (0..ops.len()).filter(|&i| results[i].is_none()).collect();
        let timeout = self.core.options.timeout;
        for window in todo.chunks(self.core.options.max_in_flight) {
            let mut tickets = Vec::with_capacity(window.len());
            for &i in window {
                let op = &ops[i];
                let t = session
                    .conn
                    .submit(op.max_body(), |sid| op.request(sid, session.fh))
                    .map_err(transport)?;
                tickets.push((i, t));
            }
            for (i, mut ticket) in tickets {
                let op = &ops[i];
                let mut waits = 0;
                loop {
                    let reply = ticket.wait(timeout).map_err(transport)?;
                    match proto::classify(reply.status, reply.body, &session.url) {
                        Outcome::Done(body) => {
                            results[i] = Some(op.decode(body, session.fh)?);
                            break;
                        }
                        Outcome::Wait(d) if waits < MAX_WAITS => {
                            waits += 1;
                            std::thread::sleep(d);
                            ticket = session
                                .conn
                                .submit(op.max_body(), |sid| op.request(sid, session.fh))
                                .map_err(transport)?;
                        }
                        other => return Err(outcome_failure(other)),
                    }
                }
            }
        }
        Ok(())
    }

    fn run(&self, ops: &[Op]) -> io::Result<Vec<Vec<Bytes>>> {
        let mut results: Vec<Option<Vec<Bytes>>> = vec![None; ops.len()];
        let options = &self.core.options;
        let (mut retries, mut redirects, mut wait) = (0, 0, options.backoff);
        loop {
            let attempt = self.session().and_then(|session| {
                let done = self.run_on(&session, ops, &mut results);
                self.core.after(&session, &done);
                done
            });
            match attempt {
                Ok(()) => {
                    return results
                        .into_iter()
                        .map(|r| r.ok_or_else(|| io::Error::other("a read was not done")))
                        .collect();
                }
                Err(Failure::Fatal(e)) => return Err(e),
                Err(Failure::Redirect(_)) => {
                    redirects += 1;
                    if redirects > options.max_redirects {
                        return Err(io::Error::other(format!(
                            "more than {} redirects reading {}",
                            options.max_redirects,
                            self.core.url.display()
                        )));
                    }
                }
                Err(Failure::Retry(e)) => {
                    if retries == options.retries {
                        return Err(self.core.exhausted(&e));
                    }
                    retries += 1;
                    std::thread::sleep(wait);
                    wait = wait.saturating_mul(2);
                }
            }
        }
    }
}

/// Runs `attempt` until it succeeds, fails for good, or the retries run
/// out; redirects are the attempt's own business.
pub(super) fn retrying<T>(
    options: &XrootdOptions,
    url: &Url,
    mut attempt: impl FnMut() -> Result<T, Failure>,
) -> io::Result<T> {
    let mut wait = options.backoff;
    for n in 0..=options.retries {
        match attempt() {
            Ok(v) => return Ok(v),
            Err(Failure::Fatal(e)) => return Err(e),
            Err(Failure::Redirect(u)) => {
                return Err(io::Error::other(format!(
                    "unexpected redirect to {}",
                    u.display()
                )));
            }
            Err(Failure::Retry(e)) => {
                if n == options.retries {
                    return Err(io::Error::new(
                        e.kind(),
                        format!(
                            "{e} (after {} retries) for {}",
                            options.retries,
                            url.display()
                        ),
                    ));
                }
                std::thread::sleep(wait);
                wait = wait.saturating_mul(2);
            }
        }
    }
    Err(io::Error::other("no attempt was made"))
}

impl ReadAt for XrootdSource {
    fn len(&self) -> io::Result<u64> {
        Ok(self.core.len)
    }

    fn read_at(&self, range: Range<FileOffset>) -> io::Result<Bytes> {
        let mut out = self.read_ranges(std::slice::from_ref(&range))?;
        out.pop().ok_or_else(|| io::Error::other("no bytes read"))
    }

    fn read_ranges(&self, ranges: &[Range<FileOffset>]) -> io::Result<Vec<Bytes>> {
        let rs = check_ranges(ranges, self.core.len)?;
        let plan = Plan::new(&rs);
        if plan.ops.is_empty() {
            return Ok(vec![Bytes::new(); rs.len()]);
        }
        let results = self.run(&plan.ops)?;
        plan.assemble(&results)
    }
}

/// Fuzz entry: a server's byte stream through the response demultiplexer,
/// with requests of each kind in flight.
#[cfg(feature = "_fuzz")]
pub(crate) fn fuzz_stream(data: &[u8]) {
    let demux = Demux::default();
    let mut tickets = Vec::new();
    for max in [0, 16, 4096, MAX_CONTROL_BODY] {
        if let Ok((sid, slot)) = demux.register(max) {
            tickets.push((sid, slot));
        }
    }
    let mut r = data;
    while demux.pump(&mut r).is_ok() {}
    demux.break_with(&io::Error::other("end"));
    for (_, slot) in tickets {
        let _ = lock(&slot.state).done.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_batch_small_ranges_and_split_large_ones() {
        let big = MAX_REQUEST * 2 + 5;
        let ranges = vec![0..10, 20..20, 30..40, 100..100 + big, 50..60];
        let plan = Plan::new(&ranges);
        assert_eq!(
            plan.ops,
            vec![
                Op::Read {
                    offset: 100,
                    len: u32::try_from(MAX_REQUEST).expect("16 MiB")
                },
                Op::Read {
                    offset: 100 + MAX_REQUEST,
                    len: u32::try_from(MAX_REQUEST).expect("16 MiB")
                },
                Op::Read {
                    offset: 100 + 2 * MAX_REQUEST,
                    len: 5
                },
                Op::ReadV(vec![(0, 10), (30, 10), (50, 10)]),
            ]
        );
        let results = vec![
            vec![Bytes::from(vec![1u8; 16 << 20])],
            vec![Bytes::from(vec![2u8; 16 << 20])],
            vec![Bytes::from_static(b"abcde")],
            vec![
                Bytes::from_static(b"0123456789"),
                Bytes::from_static(b"3333333333"),
                Bytes::from_static(b"5555555555"),
            ],
        ];
        let out = plan.assemble(&results).expect("assemble");
        assert_eq!(&out[0][..], b"0123456789");
        assert!(out[1].is_empty());
        assert_eq!(u64::try_from(out[3].len()).expect("len"), big);
        assert_eq!(&out[3][out[3].len() - 5..], b"abcde");
        assert_eq!(&out[4][..], b"5555555555");
    }

    #[test]
    fn plans_respect_the_readv_limits() {
        let ranges: Vec<Range<u64>> = (0..3000u64).map(|i| i * 10..i * 10 + 5).collect();
        let plan = Plan::new(&ranges);
        assert_eq!(plan.ops.len(), 3);
        assert!(plan.ops.iter().all(|op| match op {
            Op::ReadV(e) => e.len() <= MAX_READV_ELEMENTS,
            Op::Read { .. } => false,
        }));
        let ranges: Vec<Range<u64>> = (0..20u64)
            .map(|i| i * MAX_READV_ELEMENT..(i + 1) * MAX_READV_ELEMENT)
            .collect();
        let plan = Plan::new(&ranges);
        assert!(
            plan.ops
                .iter()
                .all(|op| u64::try_from(op.max_body()).expect("size") <= MAX_REQUEST + 16 * 1024)
        );
        assert_eq!(Plan::new(&[5..5, 7..7]).ops, vec![]);
    }

    fn frame(sid: StreamId, status: u16, body: &[u8]) -> Vec<u8> {
        let mut f = sid.to_vec();
        f.extend_from_slice(&status.to_be_bytes());
        f.extend_from_slice(&u32::try_from(body.len()).expect("len").to_be_bytes());
        f.extend_from_slice(body);
        f
    }

    #[test]
    #[expect(clippy::many_single_char_names, reason = "stream ids a to d")]
    fn the_demux_joins_parts_and_checks_sizes() {
        let demux = Demux::default();
        let (a, slot_a) = demux.register(6).expect("a");
        let (b, slot_b) = demux.register(4).expect("b");
        let mut stream = frame(a, OKSOFAR, b"abc");
        stream.extend(frame([0, 99], OK, b"stray")); // nobody asked: skipped
        stream.extend(frame(b, WAITRESP, &5i32.to_be_bytes()));
        // b's answer, as an asynchronous response inside kXR_attn.
        let inner = frame(b, OK, b"wxyz");
        let mut attn = 5008i32.to_be_bytes().to_vec();
        attn.extend_from_slice(&[0; 4]);
        attn.extend_from_slice(&inner);
        stream.extend(frame([0, 0], ATTN, &attn));
        stream.extend(frame(a, OK, b"def"));
        let mut r = &stream[..];
        while !r.is_empty() {
            demux.pump(&mut r).expect("pump");
        }
        let take = |s: &Arc<Slot>| lock(&s.state).done.take().expect("done").expect("ok");
        assert_eq!(take(&slot_a).body, b"abcdef");
        assert_eq!(lock(&slot_b.state).extra, Duration::from_secs(5));
        assert_eq!(take(&slot_b).body, b"wxyz");
        // More than asked: the stream is refused.
        let (c, _slot) = demux.register(2).expect("c");
        let bad = frame(c, OK, b"toolong");
        assert!(demux.pump(&mut &bad[..]).is_err());
        // An error body is control-sized whatever the request.
        let (d, slot_d) = demux.register(0).expect("d");
        let mut err = 3011i32.to_be_bytes().to_vec();
        err.extend_from_slice(b"no such file\0");
        demux
            .pump(&mut &frame(d, proto::ERROR, &err)[..])
            .expect("error");
        assert_eq!(take(&slot_d).status, proto::ERROR);
        // A broken connection fails what is in flight and what comes.
        let (_, slot_e) = demux.register(10).expect("e");
        assert!(!demux.is_broken());
        demux.break_with(&io::Error::other("gone"));
        assert!(demux.is_broken());
        assert!(lock(&slot_e.state).done.take().expect("done").is_err());
        assert!(demux.register(1).is_err());
    }

    /// A request given up leaves the demux, unless its stream id was
    /// handed to another since.
    #[test]
    fn requests_given_up_are_forgotten() {
        let demux = Arc::new(Demux::default());
        let (a, slot_a) = demux.register(1).expect("a");
        let (_, other) = demux.register(1).expect("other");
        demux.remove(a, &other);
        assert!(demux.get(a).is_some(), "not its slot");
        demux.remove(a, &slot_a);
        assert!(demux.get(a).is_none());
        let (b, slot) = demux.register(1).expect("b");
        drop(Ticket {
            sid: b,
            slot,
            demux: Arc::clone(&demux),
        });
        assert!(demux.get(b).is_none(), "dropped ticket");
    }

    /// An asynchronous answer whose lengths disagree breaks the stream.
    #[test]
    fn asynchronous_answers_must_agree_on_their_length() {
        let demux = Demux::default();
        let (a, _slot) = demux.register(8).expect("a");
        let mut attn = 5008i32.to_be_bytes().to_vec();
        attn.extend_from_slice(&[0; 4]);
        let mut inner = frame(a, OK, b"wxyz");
        inner[4..8].copy_from_slice(&5u32.to_be_bytes());
        attn.extend_from_slice(&inner);
        let e = demux
            .pump(&mut &frame([0, 0], ATTN, &attn)[..])
            .expect_err("disagree");
        assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}");
    }

    #[test]
    fn options_keep_what_they_are_given() {
        let o = XrootdOptions::default()
            .retries(5)
            .backoff(Duration::from_millis(7))
            .timeout(Duration::ZERO)
            .max_redirects(3)
            .max_in_flight(0);
        assert_eq!(
            (
                o.retries,
                o.backoff,
                o.timeout,
                o.max_redirects,
                o.max_in_flight
            ),
            (5, Duration::from_millis(7), Duration::from_millis(1), 3, 1)
        );
        assert_eq!(XrootdOptions::default().max_in_flight(4).max_in_flight, 4);
    }
}
