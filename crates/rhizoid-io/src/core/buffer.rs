//! Big-endian cursors over untrusted bytes: [`RBuffer`] reads, [`WBuffer`] writes.
//!
//! Every read is bounds-checked and returns `Result`; nothing indexes a slice
//! directly (STYLE §5).

use rhizoid_source::FileOffset;

use super::error::{Context, Error, ErrorKind, Result};

/// A big-endian read cursor.
///
/// `origin` is the position of `data[0]` counted from the start of the key
/// that holds it: ROOT's class tags and back-references count from there
/// [RB§6], so the constructor requires it. `base` is the file offset of
/// `data[0]`, used only in error messages.
#[derive(Debug, Clone)]
pub(crate) struct RBuffer<'a> {
    data: &'a [u8],
    pos: usize,
    origin: u32,
    base: Option<FileOffset>,
}

impl<'a> RBuffer<'a> {
    /// Creates a cursor. `origin` is where `data` starts within its key.
    pub(crate) fn new(data: &'a [u8], origin: u32) -> Self {
        Self {
            data,
            pos: 0,
            origin,
            base: None,
        }
    }

    /// Records the file offset of `data[0]`, for error messages.
    #[must_use]
    pub(crate) fn at(mut self, base: FileOffset) -> Self {
        self.base = Some(base);
        self
    }

    /// Current position counted from the start of the key (`origin + pos`).
    pub(crate) fn key_position(&self) -> Result<u32> {
        u32::try_from(self.pos)
            .ok()
            .and_then(|p| p.checked_add(self.origin))
            .ok_or_else(|| Error::new(ErrorKind::LimitExceeded, "buffer position exceeds 32 bits"))
    }

    /// Current position in `data`.
    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    /// Moves to `pos` in `data` (at most its length).
    pub(crate) fn seek(&mut self, pos: usize) -> Result<()> {
        if pos > self.data.len() {
            return Err(self.eof(pos - self.pos.min(pos)));
        }
        self.pos = pos;
        Ok(())
    }

    /// The bytes from `start` to the current position.
    pub(crate) fn since(&self, start: usize) -> &'a [u8] {
        self.data.get(start..self.pos).unwrap_or_default()
    }

    /// The file offset of the current position, when known.
    pub(crate) fn file_offset(&self) -> Option<FileOffset> {
        self.here()
    }

    /// Reads the next `u32` without consuming it.
    pub(crate) fn peek_u32(&self) -> Result<u32> {
        let mut c = self.clone();
        c.read_u32()
    }

    /// Length of the whole buffer.
    pub(crate) fn len(&self) -> usize {
        self.data.len()
    }

    /// Bytes left to read.
    pub(crate) fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn here(&self) -> Option<FileOffset> {
        let pos = u64::try_from(self.pos).ok()?;
        self.base?.checked_add(pos)
    }

    #[cold]
    fn eof(&self, needed: usize) -> Error {
        let e = Error::new(
            ErrorKind::UnexpectedEof,
            format!(
                "unexpected end of buffer (needed {needed}, had {})",
                self.remaining()
            ),
        );
        match self.here() {
            Some(o) => e.with_context(Context::Offset(o)),
            None => e,
        }
    }

    /// Takes the next `n` bytes.
    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or_else(|| self.eof(n))?;
        let s = self.data.get(self.pos..end).ok_or_else(|| self.eof(n))?;
        self.pos = end;
        Ok(s)
    }

    /// Skips `n` bytes.
    pub(crate) fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let s = self.take(N)?;
        <[u8; N]>::try_from(s).map_err(|_| self.eof(N))
    }

    pub(crate) fn read_u8(&mut self) -> Result<u8> {
        Ok(u8::from_be_bytes(self.array()?))
    }
    pub(crate) fn read_i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.array()?))
    }
    pub(crate) fn read_u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }
    pub(crate) fn read_i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }
    pub(crate) fn read_u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.array()?))
    }
    pub(crate) fn read_i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.array()?))
    }
    pub(crate) fn read_i8(&mut self) -> Result<i8> {
        Ok(i8::from_be_bytes(self.array()?))
    }
    pub(crate) fn read_u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.array()?))
    }
    pub(crate) fn read_f32(&mut self) -> Result<f32> {
        Ok(f32::from_be_bytes(self.array()?))
    }
    pub(crate) fn read_f64(&mut self) -> Result<f64> {
        Ok(f64::from_be_bytes(self.array()?))
    }

    /// Reads a NUL-terminated string (class names after `kNewClassTag`).
    pub(crate) fn read_cstring(&mut self, max: usize) -> Result<String> {
        let rest = self.data.get(self.pos..).unwrap_or_default();
        let window = rest.get(..max.min(rest.len())).unwrap_or_default();
        let n = window
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| Error::malformed("unterminated class name"))?;
        let s = String::from_utf8_lossy(self.take(n)?).into_owned();
        self.skip(1)?;
        Ok(s)
    }

    /// Reads a non-negative `i32` (a length or count on disk) as `u32`.
    pub(crate) fn read_len32(&mut self, what: &'static str) -> Result<u32> {
        let at = self.here();
        let v = self.read_i32()?;
        u32::try_from(v).map_err(|_| {
            let e = Error::malformed(format!("negative {what} ({v})"));
            match at {
                Some(o) => e.with_context(Context::Offset(o)),
                None => e,
            }
        })
    }

    /// Reads a file offset stored in 4 (`large == false`) or 8 bytes.
    pub(crate) fn read_seek(&mut self, large: bool) -> Result<FileOffset> {
        let at = self.here();
        let v: i64 = if large {
            self.read_i64()?
        } else {
            i64::from(self.read_i32()?)
        };
        let v = u64::try_from(v).map_err(|_| {
            let e = Error::malformed(format!("negative file offset ({v})"));
            match at {
                Some(o) => e.with_context(Context::Offset(o)),
                None => e,
            }
        })?;
        Ok(FileOffset::new(v))
    }

    /// Reads a ROOT string: one length byte, or 255 followed by an `i32`
    /// length [§3]. Invalid UTF-8 is replaced, as names are nearly always ASCII.
    pub(crate) fn read_string(&mut self) -> Result<String> {
        let n = self.read_u8()?;
        let len = if n == 255 {
            usize::try_from(self.read_len32("string length")?)
                .map_err(|_| Error::malformed("string length does not fit in memory"))?
        } else {
            usize::from(n)
        };
        let bytes = self.take(len)?;
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// A big-endian write buffer.
///
/// `origin` is where `data[0]` will sit within its key, mirroring
/// [`RBuffer`]: class tags written into the buffer count from the key start.
#[derive(Debug, Default, Clone)]
pub(crate) struct WBuffer {
    data: Vec<u8>,
    origin: u32,
}

impl WBuffer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A buffer whose first byte sits at `origin` within its key.
    pub(crate) fn with_origin(origin: u32) -> Self {
        Self {
            data: Vec::new(),
            origin,
        }
    }

    /// Current position counted from the start of the key.
    pub(crate) fn key_position(&self) -> Result<u32> {
        u32::try_from(self.data.len())
            .ok()
            .and_then(|p| p.checked_add(self.origin))
            .ok_or_else(|| Error::new(ErrorKind::LimitExceeded, "object exceeds 4 GiB"))
    }

    /// Overwrites four bytes at `at` (a position in this buffer).
    pub(crate) fn patch_u32(&mut self, at: usize, v: u32) -> Result<()> {
        self.data
            .get_mut(at..at + 4)
            .ok_or_else(|| Error::malformed("patch slot out of range"))?
            .copy_from_slice(&v.to_be_bytes());
        Ok(())
    }

    pub(crate) fn len(&self) -> usize {
        self.data.len()
    }

    pub(crate) fn into_inner(self) -> Vec<u8> {
        self.data
    }

    pub(crate) fn write_bytes(&mut self, b: &[u8]) {
        self.data.extend_from_slice(b);
    }
    pub(crate) fn write_u8(&mut self, v: u8) {
        self.data.push(v);
    }
    pub(crate) fn write_i16(&mut self, v: i16) {
        self.write_bytes(&v.to_be_bytes());
    }
    pub(crate) fn write_u16(&mut self, v: u16) {
        self.write_bytes(&v.to_be_bytes());
    }
    pub(crate) fn write_i32(&mut self, v: i32) {
        self.write_bytes(&v.to_be_bytes());
    }
    pub(crate) fn write_u32(&mut self, v: u32) {
        self.write_bytes(&v.to_be_bytes());
    }
    pub(crate) fn write_i64(&mut self, v: i64) {
        self.write_bytes(&v.to_be_bytes());
    }
    pub(crate) fn write_i8(&mut self, v: i8) {
        self.write_bytes(&v.to_be_bytes());
    }
    pub(crate) fn write_u64(&mut self, v: u64) {
        self.write_bytes(&v.to_be_bytes());
    }
    pub(crate) fn write_f32(&mut self, v: f32) {
        self.write_bytes(&v.to_be_bytes());
    }
    pub(crate) fn write_f64(&mut self, v: f64) {
        self.write_bytes(&v.to_be_bytes());
    }

    /// Writes a NUL-terminated string.
    pub(crate) fn write_cstring(&mut self, s: &str) {
        self.write_bytes(s.as_bytes());
        self.write_u8(0);
    }

    /// Writes a length or count as a non-negative `i32`.
    pub(crate) fn write_len32(&mut self, v: u32) -> Result<()> {
        let v = i32::try_from(v).map_err(|_| {
            Error::new(
                ErrorKind::LimitExceeded,
                "value too large for a 32-bit field",
            )
        })?;
        self.write_i32(v);
        Ok(())
    }

    /// Writes a file offset in 4 or 8 bytes.
    pub(crate) fn write_seek(&mut self, v: FileOffset, large: bool) -> Result<()> {
        if large {
            let v = i64::try_from(v.get())
                .map_err(|_| Error::new(ErrorKind::LimitExceeded, "offset too large"))?;
            self.write_i64(v);
        } else {
            let v = i32::try_from(v.get()).map_err(|_| {
                Error::new(ErrorKind::LimitExceeded, "offset needs the 8-byte form")
            })?;
            self.write_i32(v);
        }
        Ok(())
    }

    /// Writes a ROOT string (1-byte length, or 255 + `i32` length).
    pub(crate) fn write_string(&mut self, s: &str) -> Result<()> {
        let bytes = s.as_bytes();
        match u8::try_from(bytes.len()) {
            Ok(n) if n < 255 => self.write_u8(n),
            _ => {
                self.write_u8(255);
                let n = u32::try_from(bytes.len())
                    .map_err(|_| Error::new(ErrorKind::LimitExceeded, "string too long"))?;
                self.write_len32(n)?;
            }
        }
        self.write_bytes(bytes);
        Ok(())
    }

    /// Writes zero bytes.
    pub(crate) fn write_zeros(&mut self, n: usize) {
        self.data.resize(self.data.len() + n, 0);
    }

    /// Writes an object header (byte count + version) and back-patches the byte
    /// count after `body` runs, so it cannot be forgotten [RB§6].
    pub(crate) fn object(
        &mut self,
        version: i16,
        body: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        let start = self.data.len();
        self.write_u32(0);
        self.write_i16(version);
        body(self)?;
        self.patch_byte_count(start)
    }

    /// Fills the byte-count slot reserved at `start` with the number of bytes
    /// written after it, `| 0x40000000` [RB§6].
    pub(crate) fn patch_byte_count(&mut self, start: usize) -> Result<()> {
        let count = self.data.len() - start - 4;
        let count = u32::try_from(count)
            .ok()
            .filter(|c| *c < 0x3FFF_FFFE)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::LimitExceeded,
                    "object too large for a byte count",
                )
            })?;
        self.patch_u32(start, count | 0x4000_0000)
    }
}

/// On-disk size of a ROOT string.
pub(crate) fn string_len(s: &str) -> usize {
    if s.len() < 255 {
        1 + s.len()
    } else {
        5 + s.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn short_reads_are_errors_with_offsets() {
        let data = [0u8, 1, 2];
        let mut r = RBuffer::new(&data, 0).at(FileOffset::new(100));
        r.skip(1).expect("one byte");
        let e = r.read_u32().expect_err("too short");
        assert_eq!(e.kind(), ErrorKind::UnexpectedEof);
        assert_eq!(e.offset(), Some(FileOffset::new(101)));
    }

    #[test]
    fn negative_lengths_are_malformed() {
        let data = (-5i32).to_be_bytes();
        let mut r = RBuffer::new(&data, 0);
        assert_eq!(
            r.read_len32("Nbytes").expect_err("negative").kind(),
            ErrorKind::Malformed
        );
    }

    #[test]
    fn object_patches_byte_count() {
        let mut w = WBuffer::new();
        w.object(3, |w| {
            w.write_i32(7);
            Ok(())
        })
        .expect("write");
        assert_eq!(w.into_inner(), vec![0x40, 0, 0, 6, 0, 3, 0, 0, 0, 7]);
    }

    proptest! {
        #[test]
        fn strings_round_trip(s in ".{0,600}") {
            let mut w = WBuffer::new();
            w.write_string(&s).expect("write");
            let bytes = w.into_inner();
            prop_assert_eq!(bytes.len(), string_len(&s));
            let mut r = RBuffer::new(&bytes, 0);
            prop_assert_eq!(r.read_string().expect("read"), s);
            prop_assert_eq!(r.remaining(), 0);
        }

        #[test]
        fn seeks_round_trip(v in 0u64..=i64::MAX as u64, large: bool) {
            let mut w = WBuffer::new();
            let fits = large || v <= u64::from(i32::MAX.unsigned_abs());
            let res = w.write_seek(FileOffset::new(v), large);
            prop_assert_eq!(res.is_ok(), fits);
            if fits {
                let bytes = w.into_inner();
                let mut r = RBuffer::new(&bytes, 0);
                prop_assert_eq!(r.read_seek(large).expect("read").get(), v);
            }
        }
    }
}
