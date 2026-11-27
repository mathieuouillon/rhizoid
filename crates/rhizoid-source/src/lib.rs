//! Byte-source traits for the **rhizoid** ROOT ecosystem.
//!
//! # Overview
//!
//! This crate defines how rhizoid moves bytes: positioned reads ([`ReadAt`],
//! [`AsyncReadAt`], [`LocalAsyncReadAt`]) and positioned writes ([`WriteAt`]),
//! all addressed by a [`FileOffset`]. It knows nothing about the ROOT format.
//!
//! It is deliberately tiny and aims to stay at 1.x forever, so that anyone
//! writing a source (a storage system, a test double, a cache) can depend on it
//! alone, without depending on `rhizoid-io`.
//!
//! # Usage
//!
//! ```toml
//! [dependencies]
//! rhizoid-source = "1.0"
//! ```
//!
//! # Example
//!
//! ```
//! use rhizoid_source::{FileOffset, ReadAt};
//!
//! let data: Vec<u8> = b"root and more".to_vec();
//! let bytes = data.read_at(FileOffset::new(0)..FileOffset::new(4))?;
//! assert_eq!(&bytes[..], b"root");
//! # Ok::<(), std::io::Error>(())
//! ```

use core::fmt;
use std::io;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

pub use bytes::Bytes;
pub use futures_core::future::{BoxFuture, LocalBoxFuture};

/// A position in a ROOT file, in bytes from the start.
///
/// Whether it is stored in 4 or 8 bytes on disk is decided when a record is
/// encoded, never by the caller.
#[doc(alias = "fSeekKey")]
#[doc(alias = "Long64_t")]
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)] // no Default: no offset is "obvious"
pub struct FileOffset(u64);

impl FileOffset {
    /// Offset of the first record of every file ROOT writes.
    #[doc(alias = "kBEGIN")]
    pub const BEGIN: Self = Self(100);

    /// Past this offset, records use 8-byte offsets (ROOT's `kStartBigFile`).
    #[doc(alias = "kStartBigFile")]
    pub const START_BIG_FILE: Self = Self(2_000_000_000);

    /// Creates an offset.
    #[must_use]
    pub const fn new(bytes: u64) -> Self {
        Self(bytes)
    }

    /// Returns the offset as a number of bytes.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns `self + len`, or `None` on overflow.
    #[must_use]
    pub const fn checked_add(self, len: u64) -> Option<Self> {
        match self.0.checked_add(len) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// Returns `true` if a record at this position must use 8-byte offsets.
    #[must_use]
    pub const fn is_large(self) -> bool {
        self.0 > Self::START_BIG_FILE.0
    }
}

impl fmt::Display for FileOffset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

impl From<FileOffset> for u64 {
    fn from(o: FileOffset) -> u64 {
        o.0
    }
}

/// Converts a byte range to `usize` bounds, failing cleanly on 32-bit targets.
fn usize_range(range: &Range<FileOffset>) -> io::Result<Range<usize>> {
    let start = usize::try_from(range.start.get()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let end = usize::try_from(range.end.get()).map_err(|_| io::ErrorKind::InvalidInput)?;
    if start > end {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("inverted range {start}..{end}"),
        ));
    }
    Ok(start..end)
}

/// Synchronous positioned reads, shared between threads.
///
/// Reads take `&self`, so one source can serve many threads at once, as
/// `std::os::unix::fs::FileExt::read_at` does.
pub trait ReadAt: Send + Sync {
    /// Returns the total length of the source in bytes.
    ///
    /// # Errors
    ///
    /// Any I/O error of the underlying source.
    fn len(&self) -> io::Result<u64>;

    /// Returns `true` if the source has no bytes.
    ///
    /// # Errors
    ///
    /// Any I/O error of the underlying source.
    fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Reads exactly the bytes in `range`.
    ///
    /// # Errors
    ///
    /// `UnexpectedEof` if the range extends past the end of the source, and
    /// any I/O error of the underlying source.
    fn read_at(&self, range: Range<FileOffset>) -> io::Result<Bytes>;

    /// Reads several ranges (a vector read). The default reads them one after
    /// another; remote sources override it with a batched request.
    ///
    /// # Errors
    ///
    /// The first error of any range.
    fn read_ranges(&self, ranges: &[Range<FileOffset>]) -> io::Result<Vec<Bytes>> {
        ranges.iter().map(|r| self.read_at(r.clone())).collect()
    }
}

fn slice_range(data: &[u8], range: &Range<FileOffset>) -> io::Result<Bytes> {
    let r = usize_range(range)?;
    data.get(r.clone())
        .map(Bytes::copy_from_slice)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "range {}..{} past the end ({} bytes)",
                    r.start,
                    r.end,
                    data.len()
                ),
            )
        })
}

impl ReadAt for Bytes {
    fn len(&self) -> io::Result<u64> {
        u64::try_from(Bytes::len(self)).map_err(|_| io::ErrorKind::InvalidData.into())
    }
    fn read_at(&self, range: Range<FileOffset>) -> io::Result<Bytes> {
        let r = usize_range(&range)?;
        if r.end > Bytes::len(self) {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "range {}..{} past the end ({} bytes)",
                    r.start,
                    r.end,
                    Bytes::len(self)
                ),
            ));
        }
        Ok(self.slice(r))
    }
}

impl ReadAt for Vec<u8> {
    fn len(&self) -> io::Result<u64> {
        u64::try_from(Vec::len(self)).map_err(|_| io::ErrorKind::InvalidData.into())
    }
    fn read_at(&self, range: Range<FileOffset>) -> io::Result<Bytes> {
        slice_range(self, &range)
    }
}

impl ReadAt for std::fs::File {
    fn len(&self) -> io::Result<u64> {
        Ok(self.metadata()?.len())
    }

    fn read_at(&self, range: Range<FileOffset>) -> io::Result<Bytes> {
        let r = usize_range(&range)?;
        let mut buf = vec![0u8; r.end - r.start];
        read_exact_at(self, &mut buf, range.start.get())?;
        Ok(Bytes::from(buf))
    }
}

#[cfg(unix)]
fn read_exact_at(file: &std::fs::File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(windows)]
fn read_exact_at(file: &std::fs::File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = buf.get_mut(n..).ok_or(io::ErrorKind::InvalidData)?;
                offset += u64::try_from(n).map_err(|_| io::ErrorKind::InvalidData)?;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Elsewhere (WASI, `wasm32-unknown-unknown`), a seek and a read under a
/// lock, so concurrent reads do not interleave.
#[cfg(not(any(unix, windows)))]
static SEEK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(not(any(unix, windows)))]
fn read_exact_at(file: &std::fs::File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    use std::io::{Read as _, Seek as _};
    let _guard = SEEK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut f = file;
    f.seek(io::SeekFrom::Start(offset))?;
    f.read_exact(buf)
}

impl<T: ReadAt + ?Sized> ReadAt for Arc<T> {
    fn len(&self) -> io::Result<u64> {
        (**self).len()
    }
    fn read_at(&self, range: Range<FileOffset>) -> io::Result<Bytes> {
        (**self).read_at(range)
    }
    fn read_ranges(&self, ranges: &[Range<FileOffset>]) -> io::Result<Vec<Bytes>> {
        (**self).read_ranges(ranges)
    }
}

impl<T: ReadAt + ?Sized> ReadAt for Box<T> {
    fn len(&self) -> io::Result<u64> {
        (**self).len()
    }
    fn read_at(&self, range: Range<FileOffset>) -> io::Result<Bytes> {
        (**self).read_at(range)
    }
    fn read_ranges(&self, ranges: &[Range<FileOffset>]) -> io::Result<Vec<Bytes>> {
        (**self).read_ranges(ranges)
    }
}

/// Asynchronous positioned reads with `Send` futures.
///
/// Methods return [`BoxFuture`] (as parquet's `AsyncFileReader` does), so the
/// trait is dyn-compatible and in-flight requests can be stored and polled by
/// hand.
pub trait AsyncReadAt: Send + Sync {
    /// Returns the total length of the source in bytes.
    fn len(&self) -> BoxFuture<'_, io::Result<u64>>;
    /// Returns `true` if the source has no bytes.
    fn is_empty(&self) -> BoxFuture<'_, io::Result<bool>> {
        Box::pin(async move { Ok(self.len().await? == 0) })
    }
    /// Reads exactly the bytes in `range`.
    fn read_at(&self, range: Range<FileOffset>) -> BoxFuture<'_, io::Result<Bytes>>;
    /// Reads several ranges. The default reads them one after another.
    fn read_ranges(&self, ranges: Vec<Range<FileOffset>>) -> BoxFuture<'_, io::Result<Vec<Bytes>>> {
        Box::pin(async move {
            let mut out = Vec::with_capacity(ranges.len());
            for r in ranges {
                out.push(self.read_at(r).await?);
            }
            Ok(out)
        })
    }
}

/// [`AsyncReadAt`] without the `Send` bound, for single-threaded runtimes and
/// browser `fetch`.
pub trait LocalAsyncReadAt {
    /// Returns the total length of the source in bytes.
    fn len(&self) -> LocalBoxFuture<'_, io::Result<u64>>;
    /// Returns `true` if the source has no bytes.
    fn is_empty(&self) -> LocalBoxFuture<'_, io::Result<bool>> {
        Box::pin(async move { Ok(self.len().await? == 0) })
    }
    /// Reads exactly the bytes in `range`.
    fn read_at(&self, range: Range<FileOffset>) -> LocalBoxFuture<'_, io::Result<Bytes>>;
    /// Reads several ranges. The default reads them one after another.
    fn read_ranges(
        &self,
        ranges: Vec<Range<FileOffset>>,
    ) -> LocalBoxFuture<'_, io::Result<Vec<Bytes>>> {
        Box::pin(async move {
            let mut out = Vec::with_capacity(ranges.len());
            for r in ranges {
                out.push(self.read_at(r).await?);
            }
            Ok(out)
        })
    }
}

macro_rules! ready_in_memory {
    ($t:ty) => {
        impl AsyncReadAt for $t {
            fn len(&self) -> BoxFuture<'_, io::Result<u64>> {
                Box::pin(core::future::ready(ReadAt::len(self)))
            }
            fn read_at(&self, range: Range<FileOffset>) -> BoxFuture<'_, io::Result<Bytes>> {
                Box::pin(core::future::ready(ReadAt::read_at(self, range)))
            }
            fn read_ranges(
                &self,
                ranges: Vec<Range<FileOffset>>,
            ) -> BoxFuture<'_, io::Result<Vec<Bytes>>> {
                Box::pin(core::future::ready(ReadAt::read_ranges(self, &ranges)))
            }
        }

        impl LocalAsyncReadAt for $t {
            fn len(&self) -> LocalBoxFuture<'_, io::Result<u64>> {
                Box::pin(core::future::ready(ReadAt::len(self)))
            }
            fn read_at(&self, range: Range<FileOffset>) -> LocalBoxFuture<'_, io::Result<Bytes>> {
                Box::pin(core::future::ready(ReadAt::read_at(self, range)))
            }
            fn read_ranges(
                &self,
                ranges: Vec<Range<FileOffset>>,
            ) -> LocalBoxFuture<'_, io::Result<Vec<Bytes>>> {
                Box::pin(core::future::ready(ReadAt::read_ranges(self, &ranges)))
            }
        }
    };
}

// Bytes in memory are ready at once.
ready_in_memory!(Bytes);
ready_in_memory!(Vec<u8>);

macro_rules! forward_async {
    ($p:ident) => {
        impl<T: AsyncReadAt + ?Sized> AsyncReadAt for $p<T> {
            fn len(&self) -> BoxFuture<'_, io::Result<u64>> {
                (**self).len()
            }
            fn read_at(&self, range: Range<FileOffset>) -> BoxFuture<'_, io::Result<Bytes>> {
                (**self).read_at(range)
            }
            fn read_ranges(
                &self,
                ranges: Vec<Range<FileOffset>>,
            ) -> BoxFuture<'_, io::Result<Vec<Bytes>>> {
                (**self).read_ranges(ranges)
            }
        }

        impl<T: LocalAsyncReadAt + ?Sized> LocalAsyncReadAt for $p<T> {
            fn len(&self) -> LocalBoxFuture<'_, io::Result<u64>> {
                (**self).len()
            }
            fn read_at(&self, range: Range<FileOffset>) -> LocalBoxFuture<'_, io::Result<Bytes>> {
                (**self).read_at(range)
            }
            fn read_ranges(
                &self,
                ranges: Vec<Range<FileOffset>>,
            ) -> LocalBoxFuture<'_, io::Result<Vec<Bytes>>> {
                (**self).read_ranges(ranges)
            }
        }
    };
}

forward_async!(Arc);
forward_async!(Box);

impl<T: LocalAsyncReadAt + ?Sized> LocalAsyncReadAt for Rc<T> {
    fn len(&self) -> LocalBoxFuture<'_, io::Result<u64>> {
        (**self).len()
    }
    fn read_at(&self, range: Range<FileOffset>) -> LocalBoxFuture<'_, io::Result<Bytes>> {
        (**self).read_at(range)
    }
    fn read_ranges(
        &self,
        ranges: Vec<Range<FileOffset>>,
    ) -> LocalBoxFuture<'_, io::Result<Vec<Bytes>>> {
        (**self).read_ranges(ranges)
    }
}

/// Positioned writes.
pub trait WriteAt: Send {
    /// Writes all of `buf` at `offset`, extending the sink if needed.
    ///
    /// # Errors
    ///
    /// Any I/O error of the underlying sink.
    fn write_all_at(&mut self, offset: FileOffset, buf: &[u8]) -> io::Result<()>;

    /// Flushes buffered data to the operating system.
    ///
    /// # Errors
    ///
    /// Any I/O error of the underlying sink.
    fn flush(&mut self) -> io::Result<()>;

    /// Durability barrier: every earlier write is on stable storage when this
    /// returns. Update mode refuses sinks that do not support it, because its
    /// commit protocol depends on it.
    ///
    /// # Errors
    ///
    /// `Unsupported` by default; any I/O error of the underlying sink.
    fn sync(&mut self) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

impl WriteAt for Vec<u8> {
    fn write_all_at(&mut self, offset: FileOffset, buf: &[u8]) -> io::Result<()> {
        let start = usize::try_from(offset.get()).map_err(|_| io::ErrorKind::InvalidInput)?;
        let end = start
            .checked_add(buf.len())
            .ok_or(io::ErrorKind::InvalidInput)?;
        if self.len() < end {
            self.resize(end, 0);
        }
        self.get_mut(start..end)
            .ok_or(io::ErrorKind::InvalidInput)?
            .copy_from_slice(buf);
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn sync(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl WriteAt for std::fs::File {
    fn write_all_at(&mut self, offset: FileOffset, buf: &[u8]) -> io::Result<()> {
        write_all_at(self, buf, offset.get())
    }
    fn flush(&mut self) -> io::Result<()> {
        io::Write::flush(self)
    }
    fn sync(&mut self) -> io::Result<()> {
        self.sync_data()
    }
}

#[cfg(unix)]
fn write_all_at(file: &std::fs::File, buf: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, buf, offset)
}

#[cfg(windows)]
fn write_all_at(file: &std::fs::File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_write(buf, offset) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => {
                buf = buf.get(n..).ok_or(io::ErrorKind::InvalidData)?;
                offset += u64::try_from(n).map_err(|_| io::ErrorKind::InvalidData)?;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn write_all_at(file: &std::fs::File, buf: &[u8], offset: u64) -> io::Result<()> {
    use std::io::{Seek as _, Write as _};
    let _guard = SEEK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut f = file;
    f.seek(io::SeekFrom::Start(offset))?;
    f.write_all(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn off(n: u64) -> FileOffset {
        FileOffset::new(n)
    }

    #[test]
    fn vec_reads_exact_ranges_and_rejects_past_end() {
        let v = b"0123456789".to_vec();
        assert_eq!(
            &ReadAt::read_at(&v, off(2)..off(5)).expect("in range")[..],
            b"234"
        );
        assert!(ReadAt::read_at(&v, off(8)..off(12)).is_err());
    }

    #[test]
    fn inverted_range_is_an_error_not_a_panic() {
        let v = b"0123".to_vec();
        assert!(ReadAt::read_at(&v, off(3)..off(1)).is_err());
    }

    #[test]
    fn vec_write_extends_and_overwrites() {
        let mut v = Vec::new();
        v.write_all_at(off(4), b"ab").expect("write");
        assert_eq!(v, b"\0\0\0\0ab");
        v.write_all_at(off(0), b"xy").expect("write");
        assert_eq!(v, b"xy\0\0ab");
    }

    #[test]
    fn large_threshold() {
        assert!(!FileOffset::START_BIG_FILE.is_large());
        assert!(off(2_000_000_001).is_large());
        assert_eq!(FileOffset::BEGIN.to_string(), "0x64");
    }

    #[test]
    fn file_round_trip() {
        let dir = std::env::temp_dir().join(format!("rhizoid-source-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let path = dir.join("rt.bin");
        let mut f = std::fs::File::options()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .expect("open");
        f.write_all_at(off(3), b"hello").expect("write");
        f.sync().expect("sync");
        assert_eq!(ReadAt::len(&f).expect("len"), 8);
        assert_eq!(&f.read_at(off(3)..off(8)).expect("read")[..], b"hello");
        drop(f);
        std::fs::remove_dir_all(&dir).ok();
    }
}
