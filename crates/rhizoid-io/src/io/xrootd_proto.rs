//! The XRootD wire format (protocol 5, as in `XProtocol.hh`): the requests
//! the client sends, the response frames, and the bodies it reads. Pure
//! functions over bytes, shared by the blocking and async sources and
//! reached by a fuzz target: nothing here trusts a length it reads.
//!
//! Every integer is big-endian. A request is a 24-byte header
//! (`streamid[2] requestid:u16 params[16] dlen:i32`) and `dlen` bytes; a
//! response is an 8-byte header (`streamid[2] status:u16 dlen:i32`) and
//! `dlen` bytes.

use std::io;
use std::time::Duration;

use bytes::Bytes;

/// The port of `root://` URLs that name none.
pub(super) const DEFAULT_PORT: u16 = 1094;
/// The protocol version announced: 5.0.0, the first that negotiates TLS,
/// so a server that requires TLS says so instead of failing later.
const PROTOCOL_VERSION: u32 = 0x0000_0500;

// Request codes (`XRequestTypes`).
const AUTH: u16 = 3000;
const CLOSE: u16 = 3003;
const PROTOCOL: u16 = 3006;
const LOGIN: u16 = 3007;
const OPEN: u16 = 3010;
const READ: u16 = 3013;
const STAT: u16 = 3017;
const READV: u16 = 3025;

// Response status codes (`XResponseType`).
pub(super) const OK: u16 = 0;
pub(super) const OKSOFAR: u16 = 4000;
pub(super) const ATTN: u16 = 4001;
pub(super) const AUTHMORE: u16 = 4002;
pub(super) const ERROR: u16 = 4003;
pub(super) const REDIRECT: u16 = 4004;
pub(super) const WAIT: u16 = 4005;
pub(super) const WAITRESP: u16 = 4006;

/// `kXR_attn` action: an asynchronous response, embedded.
const ASYNRESP: i32 = 5008;

// `kXR_protocol` response flags.
const GOTO_TLS: u32 = 0x4000_0000;
const TLS_LOGIN: u32 = 0x0400_0000;

/// The most elements in one `kXR_readv` (`XrdProto::maxRvecsz`).
pub(super) const MAX_READV_ELEMENTS: usize = 1024;
/// The most bytes in one `kXR_readv` element (`XrdProto::maxRVdsz`).
pub(super) const MAX_READV_ELEMENT: u64 = 2_097_136;
/// The most body bytes accepted for a response that carries no file data
/// (errors, redirects, login and open replies).
pub(super) const MAX_CONTROL_BODY: usize = 256 << 10;
/// The longest wait a server may impose at once.
pub(super) const MAX_WAIT: Duration = Duration::from_secs(60);
/// The longest path and query accepted, in bytes.
const MAX_PATH: usize = 16 << 10;

/// Identifies a request on a connection, echoed by its response.
pub(super) type StreamId = [u8; 2];
/// A file handle, as `kXR_open` returns it.
pub(super) type FileHandle = [u8; 4];

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn unsupported(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message.into())
}

/// A request: the 24-byte header and its data.
fn frame(sid: StreamId, request: u16, params: [u8; 16], data: &[u8]) -> Vec<u8> {
    // Data is a path (bounded by `Url::parse`), a credential or a readv
    // list (bounded by `MAX_READV_ELEMENTS`): never near 2 GiB.
    let dlen = u32::try_from(data.len()).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(24 + data.len());
    out.extend_from_slice(&sid);
    out.extend_from_slice(&request.to_be_bytes());
    out.extend_from_slice(&params);
    out.extend_from_slice(&dlen.to_be_bytes());
    out.extend_from_slice(data);
    out
}

/// The initial handshake (`0 0 0 4 2012`), then `kXR_protocol`, sent
/// together as clients do.
pub(super) fn handshake_and_protocol(sid: StreamId) -> Vec<u8> {
    let mut out = Vec::with_capacity(44);
    out.extend_from_slice(&[0; 12]);
    out.extend_from_slice(&4u32.to_be_bytes());
    out.extend_from_slice(&2012u32.to_be_bytes());
    let mut p = [0u8; 16];
    p[..4].copy_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    // flags (p[4]) 0: neither TLS nor signing; expect: kXR_ExpLogin.
    p[5] = 3;
    out.extend_from_slice(&frame(sid, PROTOCOL, p, &[]));
    out
}

/// `kXR_login` as user `user` (at most 8 bytes are sent).
pub(super) fn login(sid: StreamId, pid: u32, user: &str) -> Vec<u8> {
    let mut p = [0u8; 16];
    p[..4].copy_from_slice(&pid.to_be_bytes());
    for (d, s) in p[4..12].iter_mut().zip(user.bytes()) {
        *d = s;
    }
    // ability2 (p[12]) and ability (p[13]) 0: plain redirects only;
    // capver: kXR_ver004 (asynchronous kXR_attn responses not asked).
    p[14] = 4;
    frame(sid, LOGIN, p, b"xrd.appname=rhizoid")
}

/// `kXR_auth` with the `unix` protocol's credentials: `unix\0user\0`.
pub(super) fn auth_unix(sid: StreamId, user: &str) -> Vec<u8> {
    let mut p = [0u8; 16];
    p[12..].copy_from_slice(b"unix");
    let mut cred = b"unix\0".to_vec();
    cred.extend_from_slice(user.as_bytes());
    cred.push(0);
    frame(sid, AUTH, p, &cred)
}

/// `kXR_open` for reading, asking for the file's stat (`kXR_retstat`).
pub(super) fn open(sid: StreamId, path: &str) -> Vec<u8> {
    let mut p = [0u8; 16];
    // mode (p[0..2]) 0; options: kXR_open_read | kXR_retstat.
    p[2..4].copy_from_slice(&(0x0010u16 | 0x0400).to_be_bytes());
    frame(sid, OPEN, p, path.as_bytes())
}

/// `kXR_stat` of an open file, by handle.
pub(super) fn stat(sid: StreamId, fh: FileHandle) -> Vec<u8> {
    let mut p = [0u8; 16];
    p[12..].copy_from_slice(&fh);
    frame(sid, STAT, p, &[])
}

/// `kXR_read` of `len` bytes at `offset`.
pub(super) fn read(sid: StreamId, fh: FileHandle, offset: u64, len: u32) -> Vec<u8> {
    let mut p = [0u8; 16];
    p[..4].copy_from_slice(&fh);
    p[4..12].copy_from_slice(&offset.to_be_bytes());
    p[12..].copy_from_slice(&len.to_be_bytes());
    frame(sid, READ, p, &[])
}

/// `kXR_readv` of `(offset, len)` elements, each at most
/// [`MAX_READV_ELEMENT`] bytes, at most [`MAX_READV_ELEMENTS`] of them.
pub(super) fn readv(sid: StreamId, fh: FileHandle, elements: &[(u64, u32)]) -> Vec<u8> {
    let mut list = Vec::with_capacity(16 * elements.len());
    for &(offset, len) in elements {
        list.extend_from_slice(&fh);
        list.extend_from_slice(&len.to_be_bytes());
        list.extend_from_slice(&offset.to_be_bytes());
    }
    frame(sid, READV, [0; 16], &list)
}

/// `kXR_close`.
pub(super) fn close(sid: StreamId, fh: FileHandle) -> Vec<u8> {
    let mut p = [0u8; 16];
    p[..4].copy_from_slice(&fh);
    frame(sid, CLOSE, p, &[])
}

/// A response header.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct Header {
    pub(super) sid: StreamId,
    pub(super) status: u16,
    pub(super) len: usize,
}

/// Decodes a response header; a negative length is an error.
pub(super) fn parse_header(b: [u8; 8]) -> io::Result<Header> {
    let dlen = i32::from_be_bytes([b[4], b[5], b[6], b[7]]);
    let len =
        usize::try_from(dlen).map_err(|_| invalid(format!("negative response length {dlen}")))?;
    Ok(Header {
        sid: [b[0], b[1]],
        status: u16::from_be_bytes([b[2], b[3]]),
        len,
    })
}

/// The header embedded in a `kXR_attn` body that carries an asynchronous
/// response (`actnum = kXR_asynresp`, 4 reserved bytes, then the header);
/// `None` for the other actions.
pub(super) fn parse_asynresp(head: &[u8; 16]) -> io::Result<Option<Header>> {
    let action = i32::from_be_bytes([head[0], head[1], head[2], head[3]]);
    if action != ASYNRESP {
        return Ok(None);
    }
    let mut h = [0u8; 8];
    h.copy_from_slice(&head[8..]);
    parse_header(h).map(Some)
}

fn be_i32(body: &[u8], at: usize) -> Option<i32> {
    let b = body.get(at..at + 4)?;
    Some(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// Text from the server: up to the first NUL, lossily decoded.
fn text(body: &[u8]) -> String {
    let end = body.iter().position(|&b| b == 0).unwrap_or(body.len());
    String::from_utf8_lossy(&body[..end]).trim().to_owned()
}

/// The answer to the handshake: protocol version and server type.
pub(super) fn check_handshake(header: Header, body: &[u8]) -> io::Result<()> {
    if header.sid != [0, 0] || header.status != OK || body.len() != 8 {
        return Err(invalid(
            "the server did not answer the XRootD handshake (is it an XRootD server?)",
        ));
    }
    Ok(())
}

/// The `kXR_protocol` answer: fails if the server requires TLS.
pub(super) fn check_protocol(body: &[u8]) -> io::Result<()> {
    let flags = be_i32(body, 4).ok_or_else(|| invalid("short kXR_protocol response"))?;
    let flags = u32::from_be_bytes(flags.to_be_bytes());
    if flags & (GOTO_TLS | TLS_LOGIN) != 0 {
        return Err(unsupported(
            "the server requires TLS (roots://), which is not supported",
        ));
    }
    Ok(())
}

/// The security protocols a `kXR_login` answer asks for: the body is a
/// 16-byte session id, then, when authentication is required, a token
/// such as `&P=unix&P=gsi,v:10000,...`. Empty when none is required.
pub(super) fn login_protocols(body: &[u8]) -> io::Result<Vec<String>> {
    let sec = body
        .get(16..)
        .ok_or_else(|| invalid("short kXR_login response"))?;
    let sec = text(sec);
    Ok(sec
        .split('&')
        .filter_map(|p| p.strip_prefix("P="))
        .map(|p| p.split(',').next().unwrap_or_default().to_owned())
        .filter(|p| !p.is_empty())
        .collect())
}

/// A `kXR_error` body: code and message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ServerError {
    pub(super) code: i32,
    pub(super) message: String,
}

pub(super) fn parse_error(body: &[u8]) -> ServerError {
    ServerError {
        code: be_i32(body, 0).unwrap_or(0),
        message: body.get(4..).map(text).unwrap_or_default(),
    }
}

impl ServerError {
    /// Whether a new attempt (on a new connection) may succeed.
    fn transient(&self) -> bool {
        matches!(
            self.code,
            3004 // kXR_FileNotOpen: the server restarted
            | 3007 // kXR_IOError
            | 3012 // kXR_ServerError
            | 3014 // kXR_noserver
            | 3017 // kXR_Cancelled
            | 3020 // kXR_inProgress
            | 3024 // kXR_Overloaded
            | 3034 // kXR_ReqTimedOut
            | 3035 // kXR_TimerExpired
        )
    }

    fn kind(&self) -> io::ErrorKind {
        match self.code {
            3011 | 3029 => io::ErrorKind::NotFound, // kXR_NotFound, kXR_noReplicas
            3010 | 3030 => io::ErrorKind::PermissionDenied, // kXR_NotAuthorized, kXR_AuthFailed
            3013 | 3028 => io::ErrorKind::Unsupported, // kXR_Unsupported, kXR_TLSRequired
            // kXR_ArgInvalid..kXR_ArgTooLong, kXR_NotFile, kXR_isDirectory
            3000..=3002 | 3015 | 3016 => io::ErrorKind::InvalidInput,
            _ => io::ErrorKind::Other,
        }
    }

    fn into_io(self) -> io::Error {
        io::Error::new(
            self.kind(),
            format!("XRootD error {}: {}", self.code, self.message),
        )
    }
}

/// A `kXR_wait` body: seconds (at most [`MAX_WAIT`]) and a message.
pub(super) fn parse_wait(body: &[u8]) -> Duration {
    let secs = be_i32(body, 0).unwrap_or(0);
    Duration::from_secs(u64::try_from(secs).unwrap_or(0)).min(MAX_WAIT)
}

/// A `kXR_redirect` body (`port:i32`, then `host[?cgi]` or a whole URL),
/// resolved against the URL being opened: the path stays, the redirect's
/// CGI (capabilities, tokens) replaces the query.
pub(super) fn parse_redirect(body: &[u8], current: &Url) -> io::Result<Url> {
    let port = be_i32(body, 0).ok_or_else(|| invalid("short kXR_redirect response"))?;
    let target = text(body.get(4..).unwrap_or_default());
    if target.is_empty() {
        return Err(invalid("kXR_redirect names no host"));
    }
    let (place, cgi) = match target.split_once('?') {
        // A second `?` (an extra token) joins the query.
        Some((p, q)) => (p, Some(q.replace('?', "&"))),
        None => (target.as_str(), None),
    };
    let cgi = cgi.filter(|q| !q.is_empty());
    if place.contains("://") {
        let mut url = Url::parse(place)?;
        if url.path == "/" {
            url.path.clone_from(&current.path);
        }
        url.user.clone_from(&current.user);
        url.query = cgi.or(url.query);
        return Ok(url);
    }
    let (host, port_in_host) = split_host_port(place)?;
    let port = match (u16::try_from(port), port_in_host) {
        (Ok(p), _) if p > 0 => p,
        (_, Some(p)) => p,
        _ => DEFAULT_PORT,
    };
    Ok(Url {
        host,
        port,
        user: current.user.clone(),
        path: current.path.clone(),
        query: cgi.or_else(|| current.query.clone()),
    })
}

/// What `kXR_stat` (or `kXR_open` with `kXR_retstat`) says of a file.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct Stat {
    pub(super) size: u64,
    pub(super) flags: u32,
    pub(super) mtime: i64,
}

/// Stat text: `id size flags modtime` (newer servers add fields).
pub(super) fn parse_stat(body: &[u8]) -> io::Result<Stat> {
    let t = text(body);
    let mut fields = t.split_ascii_whitespace();
    let bad = || invalid(format!("malformed stat response {t:?}"));
    let _id = fields.next().ok_or_else(bad)?;
    let size = fields.next().and_then(|s| s.parse().ok()).ok_or_else(bad)?;
    let flags = fields.next().and_then(|s| s.parse().ok()).ok_or_else(bad)?;
    let mtime = fields.next().and_then(|s| s.parse().ok()).ok_or_else(bad)?;
    Ok(Stat { size, flags, mtime })
}

impl Stat {
    /// Fails for a directory or anything that is not a plain file.
    pub(super) fn check_file(self) -> io::Result<Self> {
        // kXR_isDir = 2, kXR_other = 4.
        if self.flags & 2 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the path is a directory",
            ));
        }
        if self.flags & 4 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the path is not a regular file",
            ));
        }
        Ok(self)
    }
}

/// A `kXR_open` answer: the handle, and the stat when the server sent it
/// (`fhandle[4] cpsize:i32 cptype[4] stat...`).
pub(super) fn parse_open(body: &[u8]) -> io::Result<(FileHandle, Option<Stat>)> {
    let fh: FileHandle = body
        .get(..4)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| invalid("short kXR_open response"))?;
    let stat = match body.get(12..) {
        Some(s) if !text(s).is_empty() => Some(parse_stat(s)?),
        _ => None,
    };
    Ok((fh, stat))
}

/// Splits a `kXR_readv` answer into the elements asked, in order: each is
/// a 16-byte header (`fhandle rlen offset`) and its bytes. The headers
/// must match the request.
pub(super) fn split_readv(
    body: &Bytes,
    fh: FileHandle,
    elements: &[(u64, u32)],
) -> io::Result<Vec<Bytes>> {
    let mut out = Vec::with_capacity(elements.len());
    let mut pos = 0usize;
    for &(offset, len) in elements {
        let head = body.get(pos..pos + 16).ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "truncated kXR_readv response")
        })?;
        let got_len = be_i32(head, 4).unwrap_or(-1);
        let got_offset = head
            .get(8..16)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_be_bytes);
        if head[..4] != fh || got_offset != Some(offset) {
            return Err(invalid(format!(
                "kXR_readv element {got_offset:?} does not match the requested offset {offset}"
            )));
        }
        if u32::try_from(got_len).ok() != Some(len) {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("kXR_readv gave {got_len} bytes at {offset}, {len} were asked"),
            ));
        }
        let start = pos + 16;
        let end = start + usize::try_from(len).map_err(|_| invalid("element too large"))?;
        if end > body.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated kXR_readv response",
            ));
        }
        out.push(body.slice(start..end));
        pos = end;
    }
    if pos != body.len() {
        return Err(invalid("kXR_readv response has trailing bytes"));
    }
    Ok(out)
}

/// What a response to a data request means for the caller.
#[derive(Debug)]
pub(super) enum Outcome {
    /// `kXR_ok`: the body.
    Done(Vec<u8>),
    /// `kXR_wait`: send the request again after this long.
    Wait(Duration),
    /// `kXR_redirect`: go there and start again.
    Redirect(Url),
    /// Worth another attempt on a new connection.
    Retry(io::Error),
    Fatal(io::Error),
}

/// Classifies a final response.
pub(super) fn classify(status: u16, body: Vec<u8>, current: &Url) -> Outcome {
    match status {
        OK => Outcome::Done(body),
        WAIT => Outcome::Wait(parse_wait(&body)),
        REDIRECT => match parse_redirect(&body, current) {
            Ok(url) => Outcome::Redirect(url),
            Err(e) => Outcome::Fatal(e),
        },
        ERROR => {
            let e = parse_error(&body);
            if e.transient() {
                Outcome::Retry(e.into_io())
            } else {
                Outcome::Fatal(e.into_io())
            }
        }
        AUTHMORE => Outcome::Fatal(unsupported(
            "the server asks for more authentication than the unix protocol gives",
        )),
        other => Outcome::Fatal(invalid(format!(
            "unexpected XRootD response status {other}"
        ))),
    }
}

/// A `root://` URL: `root://[user@]host[:port]//path[?query]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Url {
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) user: Option<String>,
    /// Absolute: `root://host//eos/f.root` is `/eos/f.root`.
    pub(super) path: String,
    pub(super) query: Option<String>,
}

/// `host`, `host:port`, `[v6]` or `[v6]:port`.
fn split_host_port(s: &str) -> io::Result<(String, Option<u16>)> {
    let bad = || io::Error::new(io::ErrorKind::InvalidInput, format!("bad host {s:?}"));
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (h, after) = rest.split_once(']').ok_or_else(bad)?;
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p),
            None if after.is_empty() => None,
            None => return Err(bad()),
        };
        if h.is_empty()
            || !h
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
        {
            return Err(bad());
        }
        (h.to_owned(), port)
    } else {
        let (h, port) = match s.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (s, None),
        };
        let ok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_');
        if h.is_empty() || h.len() > 255 || !h.chars().all(ok) {
            return Err(bad());
        }
        (h.to_owned(), port)
    };
    let port = match port {
        Some(p) => Some(p.parse::<u16>().ok().filter(|&p| p > 0).ok_or_else(bad)?),
        None => None,
    };
    Ok((host, port))
}

impl Url {
    /// Parses a `root://` or `xroot://` URL. `roots://` (TLS) is
    /// `Unsupported`.
    pub(super) fn parse(s: &str) -> io::Result<Self> {
        let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidInput, format!("{why}: {s:?}"));
        let (scheme, rest) = s.split_once("://").ok_or_else(|| bad("not a URL"))?;
        match scheme.to_ascii_lowercase().as_str() {
            "root" | "xroot" => {}
            "roots" | "xroots" => {
                return Err(unsupported(
                    "roots:// (XRootD over TLS) is not supported; use root://",
                ));
            }
            _ => return Err(bad("not a root:// URL")),
        }
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        let (user, hostport) = match authority.rsplit_once('@') {
            Some((u, h)) => (Some(u), h),
            None => (None, authority),
        };
        let user = match user {
            Some(u) => {
                // `user:password` is not a thing in XRootD.
                let u = u.split(':').next().unwrap_or_default();
                if u.is_empty()
                    || !u
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
                {
                    return Err(bad("bad user name"));
                }
                Some(u.to_owned())
            }
            None => None,
        };
        let (host, port) = split_host_port(hostport)?;
        if path.len() > MAX_PATH {
            return Err(bad("path too long"));
        }
        let (path, query) = match path.split_once('?') {
            Some((p, q)) => (p, Some(q.to_owned()).filter(|q| !q.is_empty())),
            None => (path, None),
        };
        if path.bytes().any(|b| b == 0 || b.is_ascii_control()) {
            return Err(bad("control characters in the path"));
        }
        let path = if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/{path}")
        };
        Ok(Self {
            host,
            port: port.unwrap_or(DEFAULT_PORT),
            user,
            path,
            query,
        })
    }

    /// The path sent with `kXR_open`: path and query.
    pub(super) fn open_path(&self) -> String {
        match &self.query {
            Some(q) => format!("{}?{q}", self.path),
            None => self.path.clone(),
        }
    }

    /// `host:port`, as `connect` takes it.
    pub(super) fn endpoint(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// The URL without its query (which may hold tokens), for messages.
    pub(super) fn display(&self) -> String {
        format!("root://{}/{}", self.endpoint(), self.path)
    }
}

/// Fuzz entry: every parser of server bytes, on arbitrary input.
#[cfg(feature = "_fuzz")]
pub(crate) fn fuzz_parsers(data: &[u8]) {
    let current = Url {
        host: "h".into(),
        port: DEFAULT_PORT,
        user: None,
        path: "/f".into(),
        query: Some("a=b".into()),
    };
    if let Some(h) = data.first_chunk::<8>() {
        let _ = parse_header(*h);
    }
    if let Some(h) = data.first_chunk::<16>() {
        let _ = parse_asynresp(h);
    }
    let _ = check_protocol(data);
    let _ = login_protocols(data);
    let _ = parse_error(data);
    let _ = parse_wait(data);
    let _ = parse_redirect(data, &current);
    let _ = parse_stat(data);
    let _ = parse_open(data);
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = Url::parse(s);
    }
    let body = Bytes::copy_from_slice(data);
    let elements: Vec<(u64, u32)> = data
        .as_chunks::<12>()
        .0
        .iter()
        .take(8)
        .map(|c| {
            let mut o = [0u8; 8];
            o.copy_from_slice(&c[..8]);
            let mut l = [0u8; 4];
            l.copy_from_slice(&c[8..]);
            (u64::from_be_bytes(o), u32::from_be_bytes(l) % (1 << 16))
        })
        .collect();
    let _ = split_readv(&body, [0, 0, 0, 1], &elements);
    for status in [OK, WAIT, REDIRECT, ERROR, AUTHMORE, 1] {
        let _ = classify(status, data.to_vec(), &current);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("url")
    }

    #[test]
    fn urls_parse() {
        let u = url("root://eospublic.cern.ch//eos/opendata/f.root");
        assert_eq!(
            (u.host.as_str(), u.port, u.path.as_str()),
            ("eospublic.cern.ch", 1094, "/eos/opendata/f.root")
        );
        let u = url("xroot://me@h.org:2000/data/f.root?authz=x&y=1");
        assert_eq!(u.user.as_deref(), Some("me"));
        assert_eq!((u.port, u.path.as_str()), (2000, "/data/f.root"));
        assert_eq!(u.open_path(), "/data/f.root?authz=x&y=1");
        assert_eq!(u.display(), "root://h.org:2000//data/f.root");
        let u = url("root://[::1]:1095//f");
        assert_eq!(
            (u.host.as_str(), u.endpoint()),
            ("::1", "[::1]:1095".into())
        );
        assert_eq!(
            Url::parse("roots://h//f").expect_err("tls").kind(),
            io::ErrorKind::Unsupported
        );
        for bad in [
            "http://h/f",
            "root://",
            "root://h:0//f",
            "root://h:99999//f",
            "root://bad host//f",
            "root://[zz]//f",
            "root://h//f\n",
            "root://@h//f",
        ] {
            assert!(Url::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    #[expect(clippy::many_single_char_names, reason = "one letter per request")]
    fn requests_have_the_wire_layout() {
        let hs = handshake_and_protocol([0, 1]);
        assert_eq!(hs.len(), 44);
        assert_eq!(&hs[12..20], &[0, 0, 0, 4, 0, 0, 0x07, 0xDC]);
        assert_eq!(&hs[20..24], &[0, 1, 0x0B, 0xBE]); // sid, kXR_protocol
        let r = read([0, 2], [1, 2, 3, 4], 0x1_0000_0000, 77);
        assert_eq!(r.len(), 24);
        assert_eq!(&r[2..4], &3013u16.to_be_bytes());
        assert_eq!(&r[4..8], &[1, 2, 3, 4]);
        assert_eq!(&r[8..16], &0x1_0000_0000u64.to_be_bytes());
        assert_eq!(&r[16..20], &77u32.to_be_bytes());
        let v = readv([0, 3], [9, 9, 9, 9], &[(5, 6), (100, 7)]);
        assert_eq!(v.len(), 24 + 32);
        assert_eq!(&v[20..24], &32u32.to_be_bytes());
        assert_eq!(&v[24..28], &[9, 9, 9, 9]);
        assert_eq!(&v[28..32], &6u32.to_be_bytes());
        assert_eq!(&v[32..40], &5u64.to_be_bytes());
        let a = auth_unix([0, 4], "me");
        assert_eq!(&a[16..20], b"unix");
        assert_eq!(&a[24..], b"unix\0me\0");
        let l = login([0, 5], 7, "averylongname");
        assert_eq!(&l[8..16], b"averylon");
        let o = open([0, 6], "/f?x=1");
        assert_eq!(&o[6..8], &0x0410u16.to_be_bytes());
        assert_eq!(&o[24..], b"/f?x=1");
    }

    #[test]
    fn redirects_resolve_against_the_current_url() {
        let cur = url("root://mgm.org//eos/f.root?orig=1");
        let mut body = 1095i32.to_be_bytes().to_vec();
        body.extend_from_slice(b"fst.org?cap.sym=abc&cap.msg=def\0");
        let r = parse_redirect(&body, &cur).expect("redirect");
        assert_eq!((r.host.as_str(), r.port), ("fst.org", 1095));
        assert_eq!(r.open_path(), "/eos/f.root?cap.sym=abc&cap.msg=def");
        // No CGI: the original query stays; port 0 is the default.
        let mut body = 0i32.to_be_bytes().to_vec();
        body.extend_from_slice(b"other.org");
        let r = parse_redirect(&body, &cur).expect("redirect");
        assert_eq!((r.port, r.open_path()), (1094, "/eos/f.root?orig=1".into()));
        // A whole URL, with a negative port (flags).
        let mut body = (-1i32).to_be_bytes().to_vec();
        body.extend_from_slice(b"root://x.org:2000//new/path?t=1");
        let r = parse_redirect(&body, &cur).expect("redirect");
        assert_eq!((r.port, r.open_path()), (2000, "/new/path?t=1".into()));
        assert!(parse_redirect(&[0, 0, 4], &cur).is_err());
        assert!(parse_redirect(&[0, 0, 0, 1], &cur).is_err());
        assert!(parse_redirect(b"\0\0\0\x01bad host", &cur).is_err());
    }

    #[test]
    fn open_stat_and_login_parse() {
        let mut body = vec![1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0];
        body.extend_from_slice(b"1234 5678 16 1700000000\0");
        let (fh, stat) = parse_open(&body).expect("open");
        assert_eq!(fh, [1, 2, 3, 4]);
        assert_eq!(
            stat,
            Some(Stat {
                size: 5678,
                flags: 16,
                mtime: 1_700_000_000
            })
        );
        assert_eq!(parse_open(&[1, 2, 3, 4]).expect("bare").1, None);
        assert!(parse_open(&[1, 2]).is_err());
        assert!(parse_stat(b"1 x 0 0").is_err());
        assert!(
            Stat {
                size: 0,
                flags: 2,
                mtime: 0
            }
            .check_file()
            .is_err()
        );
        let mut login = vec![0u8; 16];
        assert_eq!(login_protocols(&login).expect("none"), Vec::<String>::new());
        login.extend_from_slice(b"&P=gsi,v:10000,c:ssl&P=unix\0");
        assert_eq!(login_protocols(&login).expect("sec"), ["gsi", "unix"]);
        assert!(login_protocols(&[0; 3]).is_err());
    }

    #[test]
    fn readv_answers_are_checked() {
        let fh = [0, 0, 0, 7];
        let mut body = Vec::new();
        for (off, data) in [(10u64, &b"abc"[..]), (50, &b"de"[..])] {
            body.extend_from_slice(&fh);
            body.extend_from_slice(&u32::try_from(data.len()).expect("len").to_be_bytes());
            body.extend_from_slice(&off.to_be_bytes());
            body.extend_from_slice(data);
        }
        let body = Bytes::from(body);
        let parts = split_readv(&body, fh, &[(10, 3), (50, 2)]).expect("readv");
        assert_eq!(parts, [&b"abc"[..], &b"de"[..]]);
        assert!(split_readv(&body, fh, &[(10, 3), (51, 2)]).is_err());
        assert!(split_readv(&body, fh, &[(10, 3)]).is_err()); // trailing
        assert!(split_readv(&body, fh, &[(10, 3), (50, 2), (60, 1)]).is_err());
        assert!(split_readv(&body.slice(..20), fh, &[(10, 3), (50, 2)]).is_err());
        assert!(split_readv(&body, [1, 0, 0, 7], &[(10, 3), (50, 2)]).is_err());
    }

    #[test]
    fn errors_and_waits_classify() {
        let cur = url("root://h//f");
        let err = |code: i32| {
            let mut b = code.to_be_bytes().to_vec();
            b.extend_from_slice(b"msg\0");
            classify(ERROR, b, &cur)
        };
        assert!(matches!(err(3024), Outcome::Retry(_)));
        // What a real xrootd answers when asked to open a directory.
        assert!(matches!(err(3016), Outcome::Fatal(e) if e.kind() == io::ErrorKind::InvalidInput));
        match err(3011) {
            Outcome::Fatal(e) => assert_eq!(e.kind(), io::ErrorKind::NotFound),
            other => unreachable!("{other:?}"),
        }
        match classify(WAIT, 1000i32.to_be_bytes().to_vec(), &cur) {
            Outcome::Wait(d) => assert_eq!(d, MAX_WAIT),
            other => unreachable!("{other:?}"),
        }
        assert!(matches!(classify(WAIT, vec![], &cur), Outcome::Wait(d) if d.is_zero()));
        assert!(matches!(classify(4999, vec![], &cur), Outcome::Fatal(_)));
        assert!(matches!(classify(OK, vec![1], &cur), Outcome::Done(b) if b == [1]));
    }

    #[test]
    fn headers_reject_negative_lengths() {
        assert!(parse_header([0, 1, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF]).is_err());
        let h = parse_header([0, 1, 0x0F, 0xA0, 0, 0, 1, 0]).expect("header");
        assert_eq!((h.sid, h.status, h.len), ([0, 1], OKSOFAR, 256));
        let mut attn = [0u8; 16];
        attn[..4].copy_from_slice(&5008i32.to_be_bytes());
        attn[8..].copy_from_slice(&[0, 9, 0, 0, 0, 0, 0, 3]);
        assert_eq!(
            parse_asynresp(&attn).expect("ok"),
            Some(Header {
                sid: [0, 9],
                status: OK,
                len: 3
            })
        );
        attn[3] = 2;
        assert_eq!(parse_asynresp(&attn).expect("ok"), None);
    }
}
