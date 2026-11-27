//! Writing `RNTuple`s [§4.9, Writer]: [`RNTupleWriter`].
//!
//! The layout is what ROOT 6.42's `RNTupleWriter` writes for the same
//! fields (format 1.1.0.2, `tests/cross/root_ntuple.C`): the header
//! envelope first; then, cluster by cluster, one `RBlob` holding the
//! cluster's pages, each compressed on its own and followed by its XXH3
//! checksum; at the end, the page list and the footer envelopes and the
//! anchor (`ROOT::RNTuple`), the only key in the directory.
//!
//! Fields: numbers and `bool` (one column each, split encodings as ROOT
//! chooses them), `std::array<T, N>` (`[T; N]`), `std::vector<T>`
//! (`Vec<T>`, an offset column and the items), `std::string` (offsets and
//! characters).

use core::fmt;
use core::marker::PhantomData;

use super::format::{COLLECTION, FOOTER, HEADER, LEAF, PAGE_LIST};
use super::pages::kind;
use crate::compress;
use crate::core::error::{Error, ErrorKind, Result};
use crate::streamer::Scalar;
use crate::tree::write::Target;
use crate::tree::{LeafBuffer, LeafKind, LeafShape};
use crate::user::Row;
use crate::write::WritableFile;

/// The format version written: ROOT 6.42's.
pub(crate) const FORMAT_VERSION: [u16; 4] = [1, 1, 0, 2];
/// The largest key ROOT writes (`fMaxKeySize`, 1 GiB); larger blobs would
/// be chunked, which this writer does not do.
pub(crate) const MAX_KEY_SIZE: u64 = 0x4000_0000;

#[cold]
fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

fn too_large() -> Error {
    Error::new(ErrorKind::LimitExceeded, "RNTuple too large")
}

/// Options for an `RNTuple`.
#[derive(Clone, Debug)]
pub struct RNTupleOptions {
    description: String,
    cluster_bytes: usize,
    page_bytes: usize,
}

impl Default for RNTupleOptions {
    fn default() -> Self {
        Self {
            description: String::new(),
            cluster_bytes: 64 << 20,
            page_bytes: 1 << 20,
        }
    }
}

impl RNTupleOptions {
    /// Sets the description. The default is empty.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// The uncompressed size of the entries buffered before a cluster is
    /// written. The default is 64 MiB, and at most 512 MiB; memory use is
    /// about this much per `RNTuple` being written.
    #[must_use]
    pub fn cluster_bytes(mut self, bytes: usize) -> Self {
        self.cluster_bytes = bytes.clamp(1, 512 << 20);
        self
    }

    /// The largest uncompressed page. The default is 1 MiB, ROOT's.
    #[must_use]
    pub fn page_bytes(mut self, bytes: usize) -> Self {
        self.page_bytes = bytes.clamp(8, 64 << 20);
        self
    }
}

/// A little-endian builder of envelopes, frames and records.
#[derive(Default)]
struct Le(Vec<u8>);

impl Le {
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn string(&mut self, s: &str) -> Result<()> {
        self.u32(u32::try_from(s.len()).map_err(|_| invalid("string too long"))?);
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }

    /// A record frame: `i64` size (itself included), then the record.
    fn record(&mut self, f: impl FnOnce(&mut Self) -> Result<()>) -> Result<()> {
        let start = self.0.len();
        self.i64(0);
        f(self)?;
        self.patch(start, false)
    }

    /// A list frame: `i64` negative size, `u32` count, then the items.
    fn list<T>(
        &mut self,
        items: impl ExactSizeIterator<Item = T>,
        mut f: impl FnMut(&mut Self, T) -> Result<()>,
    ) -> Result<()> {
        let start = self.0.len();
        self.i64(0);
        self.u32(u32::try_from(items.len()).map_err(|_| too_large())?);
        for item in items {
            f(self, item)?;
        }
        self.patch(start, true)
    }

    fn patch(&mut self, start: usize, list: bool) -> Result<()> {
        let size = i64::try_from(self.0.len() - start).map_err(|_| too_large())?;
        let size = if list { -size } else { size };
        self.0[start..start + 8].copy_from_slice(&size.to_le_bytes());
        Ok(())
    }

    /// A locator: `i32` size, `u64` offset.
    fn locator(&mut self, nbytes: u64, offset: u64) -> Result<()> {
        self.i32(i32::try_from(nbytes).map_err(|_| too_large())?);
        self.u64(offset);
        Ok(())
    }

    /// Wraps `body` into an envelope: preamble (type, length), body, XXH3.
    fn envelope(kind: u16, body: &[u8]) -> Vec<u8> {
        let len = body.len() as u64 + 16;
        let mut out = Vec::with_capacity(body.len() + 16);
        out.extend_from_slice(&(u64::from(kind) | len << 16).to_le_bytes());
        out.extend_from_slice(body);
        let checksum = xxhash_rust::xxh3::xxh3_64(&out);
        out.extend_from_slice(&checksum.to_le_bytes());
        out
    }
}

/// Where a column's elements come from.
#[derive(Clone, Copy, Debug)]
enum Source {
    /// A field's values.
    Values(usize),
    /// The end offsets of a field's jagged arrays or strings.
    Offsets(usize),
    /// A field's string characters.
    Chars(usize),
}

#[derive(Clone, Debug)]
struct FieldSpec {
    parent: u32,
    role: u16,
    name: String,
    type_name: String,
    repetition: u64,
}

#[derive(Clone, Copy, Debug)]
struct ColumnSpec {
    kind: u16,
    bits: u16,
    field: u32,
    source: Source,
    /// Elements written in earlier clusters.
    written: u64,
}

#[derive(Clone, Copy, Debug)]
struct PageRecord {
    elements: u32,
    nbytes: u64,
    offset: u64,
}

#[derive(Debug)]
struct ClusterRecord {
    first_entry: u64,
    entries: u64,
    /// Per column: its pages and the index of its first element.
    columns: Vec<(Vec<PageRecord>, u64)>,
}

use crate::write::EnvelopeRecord as Written;

/// The column type ROOT 6.42 writes for a value, and its bits.
fn column_kind(s: Scalar) -> (u16, u16) {
    match s {
        Scalar::Bool => (kind::BIT, 1),
        Scalar::I8 => (kind::INT8, 8),
        Scalar::U8 => (kind::UINT8, 8),
        Scalar::I16 => (kind::SPLIT_INT16, 16),
        Scalar::U16 => (kind::SPLIT_UINT16, 16),
        Scalar::I32 => (kind::SPLIT_INT32, 32),
        Scalar::U32 => (kind::SPLIT_UINT32, 32),
        Scalar::I64 => (kind::SPLIT_INT64, 64),
        Scalar::U64 => (kind::SPLIT_UINT64, 64),
        Scalar::F32 => (kind::SPLIT_REAL32, 32),
        _ => (kind::SPLIT_REAL64, 64),
    }
}

/// The C++ type ROOT names a value with.
fn type_name(s: Scalar) -> &'static str {
    match s {
        Scalar::Bool => "bool",
        Scalar::I8 => "std::int8_t",
        Scalar::U8 => "std::uint8_t",
        Scalar::I16 => "std::int16_t",
        Scalar::U16 => "std::uint16_t",
        Scalar::I32 => "std::int32_t",
        Scalar::U32 => "std::uint32_t",
        Scalar::I64 => "std::int64_t",
        Scalar::U64 => "std::uint64_t",
        Scalar::F32 => "float",
        _ => "double",
    }
}

/// The page bytes of `n` elements of `width` bytes, little-endian in
/// `le`, encoded for column `kind`.
fn encode_page(kind: u16, width: usize, le: &[u8]) -> Vec<u8> {
    let n = le.len().checked_div(width).unwrap_or(0);
    match kind {
        kind::BIT => {
            let mut out = vec![0u8; n.div_ceil(8)];
            for (i, &b) in le.iter().enumerate() {
                if b != 0 {
                    out[i / 8] |= 1 << (i % 8);
                }
            }
            out
        }
        kind::INT8 | kind::UINT8 | kind::CHAR => le.to_vec(),
        _ => {
            let mut values = le.to_vec();
            match kind {
                kind::SPLIT_INT16 | kind::SPLIT_INT32 | kind::SPLIT_INT64 => {
                    for e in values.chunks_exact_mut(width) {
                        zigzag(e);
                    }
                }
                kind::SPLIT_INDEX64 => {
                    // Delta-encoded within the page.
                    let mut prev = 0u64;
                    for e in values.as_chunks_mut::<8>().0 {
                        let v = u64::from_le_bytes(*e);
                        *e = v.wrapping_sub(prev).to_le_bytes();
                        prev = v;
                    }
                }
                _ => {}
            }
            // Byte-plane split: byte `b` of element `i` goes to `b·n + i`.
            let mut out = vec![0u8; values.len()];
            for (i, e) in values.chunks_exact(width).enumerate() {
                for (b, &byte) in e.iter().enumerate() {
                    out[b * n + i] = byte;
                }
            }
            out
        }
    }
}

/// Zigzag-encodes a little-endian signed integer in place.
fn zigzag(e: &mut [u8]) {
    let bits = e.len() * 8;
    let mut b = [0u8; 8];
    b[..e.len()].copy_from_slice(e);
    // Sign-extend to 64 bits, then encode in the element's width.
    let shift = 64 - bits;
    let v = (i64::from_le_bytes(b) << shift) >> shift;
    let z = (v << 1) ^ (v >> 63);
    e.copy_from_slice(&z.to_le_bytes()[..e.len()]);
}

/// An `RNTuple` being written, independent of its entry type.
#[derive(Debug)]
pub(crate) struct NTupleState {
    name: String,
    options: RNTupleOptions,
    shapes: Vec<LeafShape>,
    bufs: Vec<LeafBuffer>,
    fields: Vec<FieldSpec>,
    columns: Vec<ColumnSpec>,
    entries: u64,
    cluster_start: u64,
    clusters: Vec<ClusterRecord>,
    header: Written,
    header_checksum: u64,
    /// The directory it is written in.
    dir: usize,
}

impl NTupleState {
    #[expect(clippy::too_many_lines, reason = "one arm per kind of field")]
    pub(crate) fn new(
        file: &mut WritableFile,
        name: &str,
        options: RNTupleOptions,
        names: &[String],
        shapes: &[Option<LeafShape>],
        dir: usize,
    ) -> Result<Self> {
        if name.is_empty() || name.contains(['/', ';']) {
            return Err(Error::new(
                ErrorKind::InvalidPath,
                format!("invalid RNTuple name `{name}`"),
            ));
        }
        if let Some(k) = file.find_key(dir, name)
            && k.class_name == "ROOT::RNTuple"
            && file.is_old(k.seek_key)
        {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!(
                    "`{name}` is an RNTuple already: extending one is not supported yet (TODOS.md)"
                ),
            ));
        }
        if names.len() != shapes.len() {
            return Err(invalid(format!(
                "a row of {} fields needs {} names, not {}",
                shapes.len(),
                shapes.len(),
                names.len()
            )));
        }
        let mut fields: Vec<FieldSpec> = Vec::new();
        let mut columns: Vec<ColumnSpec> = Vec::new();
        let mut kept = Vec::with_capacity(shapes.len());
        let id = |fields: &Vec<FieldSpec>| u32::try_from(fields.len()).map_err(|_| too_large());
        for (i, (name, shape)) in names.iter().zip(shapes).enumerate() {
            let shape = shape.ok_or_else(|| {
                Error::new(
                    ErrorKind::Unsupported,
                    format!("field {name}: objects cannot be written to an RNTuple yet"),
                )
            })?;
            if name.is_empty() || name.contains(['.', '/']) {
                return Err(invalid(format!("invalid field name `{name}`")));
            }
            if names[..i].contains(name) {
                return Err(invalid(format!("two fields are named `{name}`")));
            }
            kept.push(shape);
            let me = id(&fields)?;
            let value = type_name(shape.scalar);
            let (vkind, vbits) = column_kind(shape.scalar);
            let column = |field: u32, (kind, bits): (u16, u16), source: Source| ColumnSpec {
                kind,
                bits,
                field,
                source,
                written: 0,
            };
            let field = |parent: u32, role: u16, name: &str, type_name: String, repetition: u64| {
                FieldSpec {
                    parent,
                    role,
                    name: name.to_owned(),
                    type_name,
                    repetition,
                }
            };
            match shape.kind {
                LeafKind::One => {
                    fields.push(field(me, LEAF, name, value.to_owned(), 0));
                    columns.push(column(me, (vkind, vbits), Source::Values(i)));
                }
                LeafKind::Fixed(n) => {
                    let ty = format!("std::array<{value},{n}>");
                    fields.push(field(me, LEAF, name, ty, n as u64));
                    let child = id(&fields)?;
                    fields.push(field(me, LEAF, "_0", value.to_owned(), 0));
                    columns.push(column(child, (vkind, vbits), Source::Values(i)));
                }
                LeafKind::Jagged => {
                    let ty = format!("std::vector<{value}>");
                    fields.push(field(me, COLLECTION, name, ty, 0));
                    columns.push(column(me, (kind::SPLIT_INDEX64, 64), Source::Offsets(i)));
                    let child = id(&fields)?;
                    fields.push(field(me, LEAF, "_0", value.to_owned(), 0));
                    columns.push(column(child, (vkind, vbits), Source::Values(i)));
                }
                LeafKind::Object | LeafKind::Objects => {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        format!("field {name}: user classes cannot be written to an RNTuple yet"),
                    ));
                }
                LeafKind::String => {
                    fields.push(field(me, LEAF, name, "std::string".to_owned(), 0));
                    columns.push(column(me, (kind::SPLIT_INDEX64, 64), Source::Offsets(i)));
                    columns.push(column(me, (kind::CHAR, 8), Source::Chars(i)));
                }
            }
        }
        let mut state = Self {
            name: name.to_owned(),
            options,
            shapes: kept,
            bufs: names.iter().map(|_| LeafBuffer::default()).collect(),
            fields,
            columns,
            entries: 0,
            cluster_start: 0,
            clusters: Vec::new(),
            header: Written {
                offset: 0,
                nbytes: 0,
                len: 0,
            },
            header_checksum: 0,
            dir,
        };
        let header = state.header_envelope()?;
        state.header_checksum = u64::from_le_bytes(
            header[header.len() - 8..]
                .try_into()
                .map_err(|_| Error::malformed("envelope"))?,
        );
        state.header = file.append_envelope(dir, &header)?;
        Ok(state)
    }

    fn header_envelope(&self) -> Result<Vec<u8>> {
        let mut b = Le::default();
        b.u64(0); // feature flags
        b.string(&self.name)?;
        b.string(&self.options.description)?;
        b.string(&format!("rhizoid-io {}", env!("CARGO_PKG_VERSION")))?;
        b.list(self.fields.iter(), |b, f| {
            b.record(|b| {
                b.u32(0); // field version
                b.u32(0); // type version
                b.u32(f.parent);
                b.u16(f.role);
                b.u16(u16::from(f.repetition > 0));
                b.string(&f.name)?;
                b.string(&f.type_name)?;
                b.string("")?; // type alias
                b.string("")?; // description
                if f.repetition > 0 {
                    b.u64(f.repetition);
                }
                Ok(())
            })
        })?;
        b.list(self.columns.iter(), |b, c| {
            b.record(|b| {
                b.u16(c.kind);
                b.u16(c.bits);
                b.u32(c.field);
                b.u16(0); // flags
                b.u16(0); // representation
                Ok(())
            })
        })?;
        b.list(core::iter::empty::<()>(), |_, ()| Ok(()))?; // alias columns
        b.list(core::iter::empty::<()>(), |_, ()| Ok(()))?; // extra type information
        Ok(Le::envelope(HEADER, &b.0))
    }

    fn check_full(&self) -> bool {
        self.bufs.iter().map(LeafBuffer::bytes).sum::<usize>() >= self.options.cluster_bytes
    }

    /// A column's elements in the buffered cluster, little-endian, and
    /// their width.
    fn elements(&self, c: &ColumnSpec) -> Result<(Vec<u8>, usize)> {
        match c.source {
            Source::Values(f) => {
                let width = self.shapes[f].scalar.size();
                let data = &self.bufs[f].data;
                let mut le = data.clone();
                if width > 1 {
                    for e in le.chunks_exact_mut(width) {
                        e.reverse();
                    }
                }
                Ok((le, width))
            }
            Source::Offsets(f) => {
                let mut le = Vec::with_capacity(8 * self.bufs[f].counts.len());
                let mut end = 0u64;
                for &n in &self.bufs[f].counts {
                    end += n as u64;
                    le.extend_from_slice(&end.to_le_bytes());
                }
                Ok((le, 8))
            }
            Source::Chars(f) => {
                let buf = &self.bufs[f];
                let mut le = Vec::with_capacity(buf.data.len());
                for (&start, &n) in buf.starts.iter().zip(&buf.counts) {
                    // Skip TLeafC's length prefix.
                    let at = start + if n < 255 { 1 } else { 5 };
                    le.extend_from_slice(
                        buf.data
                            .get(at..at + n)
                            .ok_or_else(|| Error::malformed("string buffer out of step"))?,
                    );
                }
                Ok((le, 1))
            }
        }
    }

    /// Writes the buffered entries as one cluster.
    fn flush(&mut self, file: &mut WritableFile) -> Result<()> {
        let entries = self.entries - self.cluster_start;
        if entries == 0 {
            return Ok(());
        }
        // Pages, column by column, uncompressed.
        let mut pages: Vec<(usize, u32, Vec<u8>)> = Vec::new();
        for (ci, c) in self.columns.iter().enumerate() {
            let (le, width) = self.elements(c)?;
            let per_page = if c.kind == kind::BIT {
                self.options.page_bytes * 8
            } else {
                (self.options.page_bytes / width).max(1)
            };
            for chunk in le.chunks(per_page * width) {
                let n = u32::try_from(chunk.len() / width).map_err(|_| too_large())?;
                pages.push((ci, n, encode_page(c.kind, width, chunk)));
            }
        }
        let setting = file.compression_setting_value();
        let compressed = compress_pages(setting, &pages)?;
        // One blob: each page as stored, then its checksum.
        let obj_len: usize = pages.iter().map(|(_, _, raw)| raw.len()).sum();
        let mut blob = Vec::with_capacity(compressed.iter().map(|p| p.len() + 8).sum());
        let mut placed = Vec::with_capacity(pages.len());
        for p in &compressed {
            placed.push((blob.len() as u64, p.len() as u64));
            blob.extend_from_slice(p);
            blob.extend_from_slice(&xxhash_rust::xxh3::xxh3_64(p).to_le_bytes());
        }
        if blob.len() as u64 > MAX_KEY_SIZE {
            return Err(Error::new(
                ErrorKind::LimitExceeded,
                "an RNTuple cluster larger than 1 GiB once compressed: lower cluster_bytes",
            ));
        }
        let at = file.append_blob(
            self.dir,
            &blob,
            u32::try_from(obj_len).map_err(|_| too_large())?,
        )?;
        let mut columns: Vec<(Vec<PageRecord>, u64)> = self
            .columns
            .iter()
            .map(|c| (Vec::new(), c.written))
            .collect();
        for ((ci, n, _), (offset, nbytes)) in pages.iter().zip(placed) {
            columns[*ci].0.push(PageRecord {
                elements: *n,
                nbytes,
                offset: at + offset,
            });
            self.columns[*ci].written += u64::from(*n);
        }
        self.clusters.push(ClusterRecord {
            first_entry: self.cluster_start,
            entries,
            columns,
        });
        for b in &mut self.bufs {
            b.clear();
        }
        self.cluster_start = self.entries;
        Ok(())
    }

    /// Writes the last cluster, the page list, the footer and the anchor.
    pub(crate) fn finish(mut self, file: &mut WritableFile) -> Result<()> {
        self.flush(file)?;
        let setting = file.compression_setting();
        let compression = u32::try_from(setting).unwrap_or(0);
        let mut b = Le::default();
        b.u64(self.header_checksum);
        b.list(self.clusters.iter(), |b, c| {
            b.record(|b| {
                b.u64(c.first_entry);
                b.u64(c.entries); // flags (top byte) 0
                Ok(())
            })
        })?;
        b.list(self.clusters.iter(), |b, c| {
            b.list(c.columns.iter(), |b, (pages, first)| {
                let start = b.0.len();
                b.i64(0);
                b.u32(u32::try_from(pages.len()).map_err(|_| too_large())?);
                for p in pages {
                    // Negative: the page is followed by its checksum.
                    b.i32(-i32::try_from(p.elements).map_err(|_| too_large())?);
                    b.locator(p.nbytes, p.offset)?;
                }
                b.i64(i64::try_from(*first).map_err(|_| too_large())?);
                b.u32(compression);
                b.patch(start, true)
            })
        })?;
        let page_list = file.append_envelope(self.dir, &Le::envelope(PAGE_LIST, &b.0))?;

        let mut b = Le::default();
        b.u64(0); // feature flags
        b.u64(self.header_checksum);
        b.record(|b| {
            // The schema extension: nothing added after the header.
            for _ in 0..4 {
                b.list(core::iter::empty::<()>(), |_, ()| Ok(()))?;
            }
            Ok(())
        })?;
        b.list(core::iter::once(()), |b, ()| {
            b.record(|b| {
                b.u64(0); // first entry
                b.u64(self.entries);
                b.u32(u32::try_from(self.clusters.len()).map_err(|_| too_large())?);
                b.u64(page_list.len);
                b.locator(page_list.nbytes, page_list.offset)
            })
        })?;
        b.list(core::iter::empty::<()>(), |_, ()| Ok(()))?; // attribute sets
        let footer = file.append_envelope(self.dir, &Le::envelope(FOOTER, &b.0))?;

        file.put_anchor(self.dir, &self.name, &anchor(self.header, footer))
    }
}

/// Compresses every page, in parallel with the `rayon` feature.
fn compress_pages(
    setting: crate::Compression,
    pages: &[(usize, u32, Vec<u8>)],
) -> Result<Vec<Vec<u8>>> {
    let one = |raw: &Vec<u8>| -> Result<Vec<u8>> {
        Ok(compress::compress_basket(setting, raw)?.unwrap_or_else(|| raw.clone()))
    };
    #[cfg(feature = "rayon")]
    {
        use rayon::prelude::*;
        pages.par_iter().map(|(_, _, raw)| one(raw)).collect()
    }
    #[cfg(not(feature = "rayon"))]
    {
        pages.iter().map(|(_, _, raw)| one(raw)).collect()
    }
}

/// The anchor object (class version 2): big-endian, its byte count not
/// covering the XXH3 checksum of everything after the version that ends it.
fn anchor(header: Written, footer: Written) -> Vec<u8> {
    let mut a = Vec::with_capacity(78);
    a.extend_from_slice(&(0x4000_0000u32 | 0x42).to_be_bytes());
    a.extend_from_slice(&2u16.to_be_bytes());
    for v in FORMAT_VERSION {
        a.extend_from_slice(&v.to_be_bytes());
    }
    for v in [
        header.offset,
        header.nbytes,
        header.len,
        footer.offset,
        footer.nbytes,
        footer.len,
        MAX_KEY_SIZE,
    ] {
        a.extend_from_slice(&v.to_be_bytes());
    }
    let checksum = xxhash_rust::xxh3::xxh3_64(&a[6..]);
    a.extend_from_slice(&checksum.to_be_bytes());
    a
}

/// Writes rows of `R` to an `RNTuple`, from
/// [`FileScope::rntuple`](crate::FileScope::rntuple) or
/// [`WritableFile::rntuple`](crate::WritableFile::rntuple).
///
/// Entries are buffered and written a cluster at a time; with the `rayon`
/// feature, a cluster's pages are compressed in parallel. The `RNTuple` is
/// complete once [`finish`](Self::finish) returns; a writer dropped
/// unfinished is finished when its file closes.
///
/// ```
/// #[derive(rhizoid_io::Row)]
/// struct Event {
///     run: i32,          // std::int32_t
///     xyz: [f32; 3],     // std::array<float,3>
///     e: Vec<f64>,       // std::vector<double>
///     label: String,     // std::string
/// }
/// # let dir = std::env::temp_dir().join(format!("rhizoid-doc-nt-{}", std::process::id()));
/// # std::fs::create_dir_all(&dir)?;
/// # let path = dir.join("events.root");
/// rhizoid_io::create(&path, |f| {
///     let mut w = f.rntuple::<Event>("Events")?;
///     for run in 0..3 {
///         w.fill(&Event { run, xyz: [1.0, 2.0, 3.0], e: vec![0.5; run as usize], label: "ev".into() })?;
///     }
///     w.finish()
/// })?;
/// let ntuple = rhizoid_io::File::open(&path)?.rntuple("Events")?;
/// assert_eq!(ntuple.num_entries(), 3);
/// # Ok::<(), rhizoid_io::Error>(())
/// ```
pub struct RNTupleWriter<'a, R> {
    state: Option<NTupleState>,
    target: Target<'a>,
    _row: PhantomData<fn(&R)>,
}

impl<R> fmt::Debug for RNTupleWriter<'_, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("RNTupleWriter");
        if let Some(s) = &self.state {
            d.field("name", &s.name).field("entries", &s.entries);
        }
        d.finish_non_exhaustive()
    }
}

impl<'a, R: Row> RNTupleWriter<'a, R> {
    pub(crate) fn new(
        mut target: Target<'a>,
        name: &str,
        names: &[String],
        options: RNTupleOptions,
    ) -> Result<Self> {
        let shapes = R::__shapes();
        let dir = target.dir;
        let state = target.with(|f| NTupleState::new(f, name, options, names, &shapes, dir))?;
        Ok(Self {
            state: Some(state),
            target,
            _row: PhantomData,
        })
    }

    /// Appends an entry.
    ///
    /// # Errors
    ///
    /// I/O errors writing a cluster.
    pub fn fill(&mut self, row: &R) -> Result<()> {
        let Some(state) = self.state.as_mut() else {
            return Err(Error::malformed("RNTuple already finished"));
        };
        row.__put(&mut state.bufs);
        state.entries += 1;
        if state.check_full() {
            self.target.with(|f| state.flush(f))?;
        }
        Ok(())
    }

    /// Appends entries.
    ///
    /// # Errors
    ///
    /// As [`fill`](Self::fill).
    pub fn extend<'r>(&mut self, rows: impl IntoIterator<Item = &'r R>) -> Result<()>
    where
        R: 'r,
    {
        rows.into_iter().try_for_each(|r| self.fill(r))
    }

    /// The number of entries appended.
    #[must_use]
    pub fn entries(&self) -> u64 {
        self.state.as_ref().map_or(0, |s| s.entries)
    }

    /// Writes the last cluster, the page list, the footer and the anchor.
    ///
    /// # Errors
    ///
    /// I/O errors.
    pub fn finish(mut self) -> Result<()> {
        match self.state.take() {
            Some(state) => self.target.with(|f| state.finish(f)),
            None => Ok(()),
        }
    }
}

impl<R> Drop for RNTupleWriter<'_, R> {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            // No I/O here: the file finishes it when it closes.
            let _ = self.target.with(|f| {
                f.defer_ntuple(state);
                Ok(())
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ntuple::format::{Anchor, Cluster, Footer, Header, page_list};
    use crate::ntuple::pages::{Decoded, decode};
    use crate::streamer::Array;

    fn le<T: Copy>(v: &[T], f: impl Fn(T) -> Vec<u8>) -> Vec<u8> {
        v.iter().flat_map(|&x| f(x)).collect()
    }

    /// Every encoding is undone by the reader's decoder.
    #[test]
    fn pages_decode_to_what_was_encoded() {
        let i16s = [0i16, -1, 1, i16::MIN, i16::MAX, -300];
        let page = encode_page(
            kind::SPLIT_INT16,
            2,
            &le(&i16s, |x| x.to_le_bytes().to_vec()),
        );
        assert_eq!(
            decode(kind::SPLIT_INT16, 16, None, &page, 6).expect("decode"),
            Decoded::Values(Array::I16(i16s.to_vec()))
        );
        let i64s = [0i64, -1, i64::MIN, i64::MAX, 12345];
        let page = encode_page(
            kind::SPLIT_INT64,
            8,
            &le(&i64s, |x| x.to_le_bytes().to_vec()),
        );
        assert_eq!(
            decode(kind::SPLIT_INT64, 64, None, &page, 5).expect("decode"),
            Decoded::Values(Array::I64(i64s.to_vec()))
        );
        let offsets = [0u64, 3, 3, 10, 11];
        let page = encode_page(
            kind::SPLIT_INDEX64,
            8,
            &le(&offsets, |x| x.to_le_bytes().to_vec()),
        );
        assert_eq!(
            decode(kind::SPLIT_INDEX64, 64, None, &page, 5).expect("decode"),
            Decoded::Values(Array::U64(offsets.to_vec()))
        );
        let bits = [true, false, false, true, true, false, true, false, true];
        let page = encode_page(kind::BIT, 1, &le(&bits, |x| vec![u8::from(x)]));
        assert_eq!(page.len(), 2);
        assert_eq!(
            decode(kind::BIT, 1, None, &page, 9).expect("decode"),
            Decoded::Values(Array::Bool(bits.to_vec()))
        );
        let f64s = [0.25f64, -1.5, f64::MAX];
        let page = encode_page(
            kind::SPLIT_REAL64,
            8,
            &le(&f64s, |x| x.to_le_bytes().to_vec()),
        );
        assert_eq!(
            decode(kind::SPLIT_REAL64, 64, None, &page, 3).expect("decode"),
            Decoded::Values(Array::F64(f64s.to_vec()))
        );
    }

    struct Meta {
        anchor: Anchor,
        header: Header,
        footer: Footer,
        clusters: Vec<Cluster>,
    }

    /// An `RNTuple`'s metadata, decoded.
    fn meta(path: &std::path::Path) -> Meta {
        let bytes = std::fs::read(path).expect("read");
        let file = crate::File::open(path).expect("open");
        let anchor = Anchor::decode(&file.root().read_raw("t").expect("anchor")).expect("decode");
        let blob = |seek: u64, nbytes: u64, len: u64| -> Vec<u8> {
            let (seek, nbytes) = (
                usize::try_from(seek).expect("seek"),
                usize::try_from(nbytes).expect("n"),
            );
            let raw = &bytes[seek..seek + nbytes];
            let len = usize::try_from(len).expect("len");
            if raw.len() == len {
                raw.to_vec()
            } else {
                compress::decompress(raw, len).expect("decompress")
            }
        };
        let header = Header::decode(&blob(
            anchor.seek_header,
            anchor.nbytes_header,
            anchor.len_header,
        ))
        .expect("header");
        let footer = Footer::decode(
            &blob(anchor.seek_footer, anchor.nbytes_footer, anchor.len_footer),
            header.checksum,
        )
        .expect("footer");
        let mut clusters = Vec::new();
        for g in &footer.groups {
            let l = g.page_list;
            clusters.extend(
                page_list(
                    &blob(l.locator.offset, l.locator.nbytes, l.len),
                    header.checksum,
                )
                .expect("page list"),
            );
        }
        Meta {
            anchor,
            header,
            footer,
            clusters,
        }
    }

    type Every = (
        bool,
        i8,
        u8,
        i16,
        u16,
        i32,
        u32,
        i64,
        u64,
        f32,
        f64,
        [f32; 3],
        Vec<f64>,
        String,
    );

    /// The entries of `tests/cross/root_ntuple.C`.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "the C++ casts"
    )]
    fn every(i: i32) -> Every {
        (
            i % 3 == 0,
            (-i) as i8,
            (200 + i) as u8,
            (-1000 * i) as i16,
            (60000 + i) as u16,
            -100_000 * i,
            4_000_000_000 + i as u32,
            -10_000_000_000 * i64::from(i),
            18_000_000_000_000_000_000 + i as u64,
            0.5 * i as f32,
            0.25 * f64::from(i),
            [0, 1, 2].map(|j| i as f32 + 0.125 * j as f32),
            (0..i % 5).map(|j| f64::from(i * 10 + j)).collect(),
            format!(
                "entry {i}{}",
                if i % 4 == 0 {
                    " is a longer string"
                } else {
                    ""
                }
            ),
        )
    }

    /// The metadata is ROOT 6.42's for the same fields and entries, record
    /// for record: anchor version, every field and column of the header,
    /// the footer, and the clusters' pages (but where they are and how
    /// large they came out).
    #[test]
    fn metadata_matches_roots() {
        let dir = std::env::temp_dir().join(format!("rhizoid-nt-meta-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("ours.root");
        crate::create(&path, |f| {
            let mut w = f.rntuple_of::<Every, 14>(
                "t",
                [
                    "b", "i8", "u8", "i16", "u16", "i32", "u32", "i64", "u64", "f32", "f64", "xyz",
                    "jag", "str",
                ],
            )?;
            for i in 0..20 {
                w.fill(&every(i))?;
            }
            w.finish()
        })
        .expect("write");
        let theirs = meta(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/root_ntuple.root"),
        );
        let ours = meta(&path);
        assert_eq!(ours.anchor.version, theirs.anchor.version);
        assert_eq!(ours.anchor.max_key_size, theirs.anchor.max_key_size);
        assert_eq!(ours.header.name, theirs.header.name);
        assert_eq!(ours.header.description, theirs.header.description);
        assert_eq!(ours.header.schema.fields, theirs.header.schema.fields);
        assert_eq!(ours.header.schema.columns, theirs.header.schema.columns);
        assert_eq!(ours.header.schema, theirs.header.schema);
        assert_eq!(ours.footer.extension, theirs.footer.extension);
        assert_eq!(ours.footer.groups.len(), theirs.footer.groups.len());
        assert_eq!(ours.clusters.len(), theirs.clusters.len());
        for (a, b) in ours.clusters.iter().zip(&theirs.clusters) {
            assert_eq!((a.first_entry, a.entries), (b.first_entry, b.entries));
            assert_eq!(a.columns.len(), b.columns.len());
            for (i, (x, y)) in a.columns.iter().zip(&b.columns).enumerate() {
                assert_eq!(x.first_element, y.first_element, "column {i}");
                let shape = |c: &crate::ntuple::format::ColumnPages| -> Vec<(u64, bool)> {
                    c.pages.iter().map(|p| (p.elements, p.checksum)).collect()
                };
                assert_eq!(shape(x), shape(y), "column {i}");
            }
        }
    }
}
