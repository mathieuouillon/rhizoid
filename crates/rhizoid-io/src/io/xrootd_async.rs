//! [`AsyncXrootdSource`]: the async twin of `XrootdSource`, on any
//! executor.
//!
//! Reads share the multiplexed connection: a read sends its requests and
//! awaits the responses, which the connection's reader thread hands over
//! (no thread is blocked per read). Connecting, logging in and opening
//! (the handshake, redirects, retries) run on a short-lived thread of
//! their own, awaited.

use std::future::Future;
use std::io;
use std::ops::Range;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_timer::Delay;
use rhizoid_source::{AsyncReadAt, BoxFuture, FileOffset};

use super::Coalesce;
use super::xrootd::{
    Core, Failure, MAX_WAITS, Op, Plan, Reply, Session, Ticket, XrootdOptions, check_ranges,
    open_session, outcome_failure, retrying, timed_out, transport,
};
use super::xrootd_proto::{self as proto, Outcome, Url};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A value set once by a thread, awaited by tasks.
#[derive(Debug)]
struct Latch<T> {
    state: Mutex<(Option<T>, Vec<Waker>)>,
}

impl<T: Clone> Latch<T> {
    fn new() -> Self {
        Self {
            state: Mutex::new((None, Vec::new())),
        }
    }

    fn set(&self, value: T) {
        let wakers = {
            let mut s = lock(&self.state);
            s.0 = Some(value);
            std::mem::take(&mut s.1)
        };
        for w in wakers {
            w.wake();
        }
    }

    fn is_set(&self) -> bool {
        lock(&self.state).0.is_some()
    }

    fn poll(&self, cx: &Context<'_>) -> Poll<T> {
        let mut s = lock(&self.state);
        if let Some(v) = &s.0 {
            return Poll::Ready(v.clone());
        }
        if !s.1.iter().any(|w| w.will_wake(cx.waker())) {
            s.1.push(cx.waker().clone());
        }
        Poll::Pending
    }

    async fn get(&self) -> T {
        std::future::poll_fn(|cx| self.poll(cx)).await
    }
}

/// A connection attempt's result, as its waiters get it.
type Connected = Result<Arc<Session>, Shared>;

/// A failure that can be handed to several waiters.
#[derive(Clone, Debug)]
struct Shared {
    retry: bool,
    kind: io::ErrorKind,
    message: String,
}

impl From<Failure> for Shared {
    fn from(f: Failure) -> Self {
        let (retry, e) = match f {
            Failure::Retry(e) => (true, e),
            Failure::Fatal(e) => (false, e),
            Failure::Redirect(u) => (
                true,
                io::Error::other(format!("unexpected redirect to {}", u.display())),
            ),
        };
        Self {
            retry,
            kind: e.kind(),
            message: e.to_string(),
        }
    }
}

impl From<Shared> for Failure {
    fn from(s: Shared) -> Self {
        let e = io::Error::new(s.kind, s.message);
        if s.retry {
            Self::Retry(e)
        } else {
            Self::Fatal(e)
        }
    }
}

/// Runs `f` on a thread of its own; awaits its result.
async fn on_thread<T: Clone + Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> io::Result<T> {
    let latch = Arc::new(Latch::new());
    let l = Arc::clone(&latch);
    std::thread::Builder::new()
        .name("rhizoid-xrootd-open".into())
        .spawn(move || l.set(f()))?;
    Ok(latch.get().await)
}

/// Awaits a response, at most `timeout` (plus what the server asked with
/// `kXR_waitresp`).
async fn wait_reply(ticket: &Ticket, timeout: Duration) -> io::Result<Reply> {
    let start = Instant::now();
    let mut delay = Delay::new(timeout);
    std::future::poll_fn(|cx| {
        if let Poll::Ready(r) = ticket.poll(cx) {
            return Poll::Ready(r);
        }
        while Pin::new(&mut delay).poll(cx).is_ready() {
            let total = timeout + ticket.extra();
            let left = total.saturating_sub(start.elapsed());
            if left.is_zero() {
                return Poll::Ready(Err(timed_out(total)));
            }
            delay.reset(left);
        }
        Poll::Pending
    })
    .await
}

/// A file served by XRootD (`root://`), read from async code: the
/// protocol, checks and retries of `XrootdSource`, over one multiplexed
/// connection whose responses wake the reads that wait for them.
///
/// Runs on any executor (no tokio needed). Sending a request writes to
/// the socket from the polling task: requests are small, but a stalled
/// server can hold that write for up to the timeout.
#[derive(Debug)]
pub struct AsyncXrootdSource {
    core: Arc<Core>,
    connecting: Mutex<Option<Arc<Latch<Connected>>>>,
}

impl AsyncXrootdSource {
    /// Opens `url` (`root://host[:port]//path`) with the default options.
    ///
    /// # Errors
    ///
    /// As `XrootdSource::new`.
    pub async fn new(url: &str) -> io::Result<Self> {
        Self::with(url, XrootdOptions::default()).await
    }

    /// Opens `url` with options.
    ///
    /// # Errors
    ///
    /// As `XrootdSource::new`.
    pub async fn with(url: &str, options: XrootdOptions) -> io::Result<Self> {
        let start = Url::parse(url)?;
        let s = start.clone();
        let first = on_thread(move || {
            retrying(&options, &s, || open_session(&s, &options))
                .map(Arc::new)
                .map_err(|e| Shared::from(Failure::Fatal(e)))
        })
        .await?
        .map_err(|s| io::Error::new(s.kind, s.message))?;
        Ok(Self {
            core: Arc::new(Core::new(start, url, options, first)),
            connecting: Mutex::new(None),
        })
    }

    /// `url` with bounded coalescing ([`Coalesce`]), as remote files are
    /// best read.
    ///
    /// # Errors
    ///
    /// As `XrootdSource::new`.
    pub async fn open(url: &str) -> io::Result<Coalesce<Self>> {
        Ok(Coalesce::new(Self::new(url).await?))
    }

    /// The URL, as given.
    pub fn url(&self) -> &str {
        &self.core.given
    }

    /// The live session, or a new one (one connection attempt at a time,
    /// awaited by every read that needs it).
    async fn session(&self) -> Result<Arc<Session>, Failure> {
        if let Some(s) = self.core.current() {
            return Ok(s);
        }
        let latch = {
            let mut c = lock(&self.connecting);
            match &*c {
                Some(l) if !l.is_set() => Arc::clone(l),
                _ => {
                    let l = Arc::new(Latch::new());
                    let (core, done) = (Arc::clone(&self.core), Arc::clone(&l));
                    std::thread::Builder::new()
                        .name("rhizoid-xrootd-open".into())
                        .spawn(move || {
                            let start = core.start();
                            let r = open_session(&start, &core.options)
                                .and_then(|s| core.install(s))
                                .map_err(Shared::from);
                            done.set(r);
                        })
                        .map_err(Failure::Retry)?;
                    *c = Some(Arc::clone(&l));
                    l
                }
            }
        };
        latch.get().await.map_err(Failure::from)
    }

    async fn run_on(
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
                    let reply = wait_reply(&ticket, timeout).await.map_err(transport)?;
                    match proto::classify(reply.status, reply.body, &session.url) {
                        Outcome::Done(body) => {
                            results[i] = Some(op.decode(body, session.fh)?);
                            break;
                        }
                        Outcome::Wait(d) if waits < MAX_WAITS => {
                            waits += 1;
                            Delay::new(d).await;
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

    async fn run(&self, ops: &[Op]) -> io::Result<Vec<Vec<Bytes>>> {
        let mut results: Vec<Option<Vec<Bytes>>> = vec![None; ops.len()];
        let options = self.core.options;
        let (mut retries, mut redirects, mut wait) = (0, 0, options.backoff);
        loop {
            let attempt = match self.session().await {
                Ok(session) => {
                    let done = self.run_on(&session, ops, &mut results).await;
                    self.core.after(&session, &done);
                    done
                }
                Err(f) => Err(f),
            };
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
                    Delay::new(wait).await;
                    wait = wait.saturating_mul(2);
                }
            }
        }
    }

    async fn read_ranges_impl(&self, ranges: Vec<Range<FileOffset>>) -> io::Result<Vec<Bytes>> {
        let rs = check_ranges(&ranges, self.core.len)?;
        let plan = Plan::new(&rs);
        if plan.ops.is_empty() {
            return Ok(vec![Bytes::new(); rs.len()]);
        }
        let results = self.run(&plan.ops).await?;
        plan.assemble(&results)
    }

    async fn read_at_impl(&self, range: Range<FileOffset>) -> io::Result<Bytes> {
        let mut out = self.read_ranges_impl(vec![range]).await?;
        out.pop().ok_or_else(|| io::Error::other("no bytes read"))
    }
}

impl AsyncReadAt for AsyncXrootdSource {
    fn len(&self) -> BoxFuture<'_, io::Result<u64>> {
        Box::pin(core::future::ready(Ok(self.core.len)))
    }

    fn read_at(&self, range: Range<FileOffset>) -> BoxFuture<'_, io::Result<Bytes>> {
        Box::pin(self.read_at_impl(range))
    }

    fn read_ranges(&self, ranges: Vec<Range<FileOffset>>) -> BoxFuture<'_, io::Result<Vec<Bytes>>> {
        Box::pin(self.read_ranges_impl(ranges))
    }
}
