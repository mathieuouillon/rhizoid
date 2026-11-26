//! The `RNTuple` binary format v1 (ROOT ≥ 6.34): anchor, envelopes, frames,
//! header, footer and page lists [RB§9, ROOT's `BinaryFormatSpecification`].
//!
//! Everything after the anchor is little-endian. Frames carry their size,
//! and readers skip what follows the fields they know, so newer minor
//! versions read; a different epoch and unknown feature flags are refused.

use crate::core::error::{Error, ErrorKind, Result};

/// A little-endian cursor over envelope bytes.
#[derive(Clone, Debug)]
pub(crate) struct Le<'a> {
    data: &'a [u8],
    pos: usize,
}

#[cold]
fn short(what: &str) -> Error {
    Error::new(
        ErrorKind::UnexpectedEof,
        format!("RNTuple metadata ends inside {what}"),
    )
}

impl<'a> Le<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or_else(|| short("a field"))?;
        let s = self
            .data
            .get(self.pos..end)
            .ok_or_else(|| short("a field"))?;
        self.pos = end;
        Ok(s)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        <[u8; N]>::try_from(self.take(N)?).map_err(|_| short("a field"))
    }

    pub(crate) fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    pub(crate) fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    pub(crate) fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.array()?))
    }
    pub(crate) fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    pub(crate) fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.array()?))
    }
    pub(crate) fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.array()?))
    }

    /// A string: `u32` length, then UTF-8 bytes.
    pub(crate) fn string(&mut self) -> Result<String> {
        let n = usize::try_from(self.u32()?).map_err(|_| short("a string"))?;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }

    /// A record frame: `i64` size (≥ 0, including itself), then the
    /// record; whatever `f` leaves unread is skipped.
    pub(crate) fn record<T>(&mut self, f: impl FnOnce(&mut Le<'a>) -> Result<T>) -> Result<T> {
        let start = self.pos;
        let size = self.i64()?;
        let size = usize::try_from(size)
            .map_err(|_| Error::malformed(format!("expected a record frame, found size {size}")))?;
        let end = start
            .checked_add(size)
            .filter(|&e| e <= self.data.len() && size >= 8)
            .ok_or_else(|| short("a record frame"))?;
        let mut inner = Le {
            data: self.data.get(..end).unwrap_or_default(),
            pos: self.pos,
        };
        let v = f(&mut inner)?;
        self.pos = end;
        Ok(v)
    }

    /// A list frame: `i64` size (< 0), `u32` count, then the items; returns
    /// the items and a cursor on what follows them inside the frame.
    pub(crate) fn list_with_tail<T>(
        &mut self,
        mut f: impl FnMut(&mut Le<'a>) -> Result<T>,
    ) -> Result<(Vec<T>, Le<'a>)> {
        let start = self.pos;
        let size = self.i64()?;
        if size >= 0 {
            return Err(Error::malformed(format!(
                "expected a list frame, found size {size}"
            )));
        }
        let size = usize::try_from(size.unsigned_abs()).map_err(|_| short("a list frame"))?;
        let end = start
            .checked_add(size)
            .filter(|&e| e <= self.data.len() && size >= 12)
            .ok_or_else(|| short("a list frame"))?;
        let n = self.u32()?;
        let mut inner = Le {
            data: self.data.get(..end).unwrap_or_default(),
            pos: self.pos,
        };
        // Every item takes at least one byte: bound the allocation.
        let mut items = Vec::with_capacity(usize::try_from(n).unwrap_or(0).min(inner.remaining()));
        for _ in 0..n {
            items.push(f(&mut inner)?);
        }
        self.pos = end;
        Ok((items, inner))
    }

    /// A list frame of items.
    pub(crate) fn list<T>(&mut self, f: impl FnMut(&mut Le<'a>) -> Result<T>) -> Result<Vec<T>> {
        self.list_with_tail(f).map(|(items, _)| items)
    }
}

/// The anchor (`ROOT::RNTuple`, class version 2): where the header and
/// footer are, big-endian, followed by an XXH3-64 checksum [RB§9].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Anchor {
    pub(crate) version: [u16; 4],
    pub(crate) seek_header: u64,
    pub(crate) nbytes_header: u64,
    pub(crate) len_header: u64,
    pub(crate) seek_footer: u64,
    pub(crate) nbytes_footer: u64,
    pub(crate) len_footer: u64,
    pub(crate) max_key_size: u64,
}

impl Anchor {
    /// Decodes the anchor object (as stored in its key, decompressed).
    pub(crate) fn decode(obj: &[u8]) -> Result<Self> {
        let be16 = |at: usize| {
            obj.get(at..at + 2)
                .and_then(|b| <[u8; 2]>::try_from(b).ok())
                .map(u16::from_be_bytes)
        };
        let be64 = |at: usize| {
            obj.get(at..at + 8)
                .and_then(|b| <[u8; 8]>::try_from(b).ok())
                .map(u64::from_be_bytes)
        };
        let bad = || Error::malformed("RNTuple anchor too short");
        if obj.len() < 78 {
            return Err(bad());
        }
        let class_version = be16(4).ok_or_else(bad)?;
        if class_version < 2 {
            return Err(Error::new(
                ErrorKind::UnsupportedVersion,
                format!("RNTuple anchor class version {class_version}"),
            ));
        }
        // The checksum covers everything after the byte count and version.
        let end = obj.len() - 8;
        let stored = be64(end).ok_or_else(bad)?;
        let computed = xxhash_rust::xxh3::xxh3_64(obj.get(6..end).unwrap_or_default());
        if stored != computed {
            return Err(Error::malformed("RNTuple anchor checksum mismatch"));
        }
        let field = |i: usize| be64(14 + 8 * i).ok_or_else(bad);
        let anchor = Self {
            version: [
                be16(6).ok_or_else(bad)?,
                be16(8).ok_or_else(bad)?,
                be16(10).ok_or_else(bad)?,
                be16(12).ok_or_else(bad)?,
            ],
            seek_header: field(0)?,
            nbytes_header: field(1)?,
            len_header: field(2)?,
            seek_footer: field(3)?,
            nbytes_footer: field(4)?,
            len_footer: field(5)?,
            max_key_size: field(6)?,
        };
        if anchor.version[0] != 1 {
            return Err(Error::new(
                ErrorKind::UnsupportedVersion,
                format!(
                    "RNTuple epoch {} (this version reads epoch 1)",
                    anchor.version[0]
                ),
            ));
        }
        Ok(anchor)
    }
}

/// Envelope types.
pub(crate) const HEADER: u16 = 1;
pub(crate) const FOOTER: u16 = 2;
pub(crate) const PAGE_LIST: u16 = 3;

/// Checks an envelope (type, length, XXH3 checksum) and returns a cursor
/// on its payload, after the 8-byte preamble.
pub(crate) fn envelope(data: &[u8], expected: u16) -> Result<(Le<'_>, u64)> {
    if data.len() < 16 {
        return Err(short("an envelope"));
    }
    let (body, sum) = data.split_at(data.len() - 8);
    let stored = u64::from_le_bytes(<[u8; 8]>::try_from(sum).map_err(|_| short("an envelope"))?);
    let checksum = xxhash_rust::xxh3::xxh3_64(body);
    if stored != checksum {
        return Err(Error::malformed("RNTuple envelope checksum mismatch"));
    }
    let mut le = Le::new(body);
    let preamble = le.u64()?;
    let kind = u16::try_from(preamble & 0xFFFF).unwrap_or(0);
    let len = preamble >> 16;
    if kind != expected {
        return Err(Error::malformed(format!(
            "expected RNTuple envelope type {expected}, found {kind}"
        )));
    }
    if usize::try_from(len).ok() != Some(data.len()) {
        return Err(Error::malformed(format!(
            "RNTuple envelope of {} bytes says {len}",
            data.len()
        )));
    }
    Ok((le, checksum))
}

/// Feature flags: no feature is defined in format 1.0, so any bit set is a
/// feature this reader does not know, and the specification says to refuse.
fn feature_flags(le: &mut Le<'_>) -> Result<()> {
    loop {
        let word = le.u64()?;
        if word & !(1 << 63) != 0 {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("RNTuple uses features this version does not know (flags {word:#x})"),
            ));
        }
        if word & (1 << 63) == 0 {
            return Ok(());
        }
    }
}

/// Where a blob is: a file offset and a size.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Locator {
    pub(crate) offset: u64,
    pub(crate) nbytes: u64,
}

fn locator(le: &mut Le<'_>) -> Result<Locator> {
    let size = le.i32()?;
    if let Ok(nbytes) = u64::try_from(size) {
        return Ok(Locator {
            offset: le.u64()?,
            nbytes,
        });
    }
    let kind = (size.unsigned_abs() >> 24) & 0x7F;
    if kind == 1 {
        let nbytes = le.u64()?;
        return Ok(Locator {
            offset: le.u64()?,
            nbytes,
        });
    }
    Err(Error::new(
        ErrorKind::Unsupported,
        format!("RNTuple locator type {kind} (object stores, …)"),
    ))
}

/// A link to another envelope: its uncompressed size and where it is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct EnvLink {
    pub(crate) len: u64,
    pub(crate) locator: Locator,
}

fn env_link(le: &mut Le<'_>) -> Result<EnvLink> {
    Ok(EnvLink {
        len: le.u64()?,
        locator: locator(le)?,
    })
}

/// Field structural roles.
pub(crate) const LEAF: u16 = 0;
pub(crate) const COLLECTION: u16 = 1;
pub(crate) const RECORD: u16 = 2;
pub(crate) const VARIANT: u16 = 3;
pub(crate) const STREAMER: u16 = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FieldRecord {
    pub(crate) parent: u32,
    pub(crate) role: u16,
    pub(crate) name: String,
    pub(crate) type_name: String,
    pub(crate) type_alias: String,
    pub(crate) description: String,
    pub(crate) repetition: u64,
    pub(crate) source: Option<u32>,
}

fn field_record(le: &mut Le<'_>) -> Result<FieldRecord> {
    le.u32()?; // field version
    le.u32()?; // type version
    let parent = le.u32()?;
    let role = le.u16()?;
    let flags = le.u16()?;
    let (name, type_name, type_alias, description) =
        (le.string()?, le.string()?, le.string()?, le.string()?);
    let repetition = if flags & 0x01 != 0 { le.u64()? } else { 0 };
    let source = if flags & 0x02 != 0 {
        Some(le.u32()?)
    } else {
        None
    };
    if flags & 0x04 != 0 {
        le.u32()?; // ROOT streamer checksum
    }
    Ok(FieldRecord {
        parent,
        role,
        name,
        type_name,
        type_alias,
        description,
        repetition,
        source,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ColumnRecord {
    pub(crate) kind: u16,
    pub(crate) bits: u16,
    pub(crate) field: u32,
    pub(crate) representation: u16,
    pub(crate) first_element: u64,
    pub(crate) range: Option<(f64, f64)>,
}

fn column_record(le: &mut Le<'_>) -> Result<ColumnRecord> {
    let kind = le.u16()?;
    let bits = le.u16()?;
    let field = le.u32()?;
    let flags = le.u16()?;
    let representation = le.u16()?;
    let first_element = if flags & 0x01 != 0 {
        le.i64()?.unsigned_abs()
    } else {
        0
    };
    let range = if flags & 0x02 != 0 {
        Some((le.f64()?, le.f64()?))
    } else {
        None
    };
    Ok(ColumnRecord {
        kind,
        bits,
        field,
        representation,
        first_element,
        range,
    })
}

/// An alias column: a field reading another field's physical column.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct AliasColumn {
    pub(crate) physical: u32,
    pub(crate) field: u32,
}

fn alias_column(le: &mut Le<'_>) -> Result<AliasColumn> {
    Ok(AliasColumn {
        physical: le.u32()?,
        field: le.u32()?,
    })
}

/// The schema: fields, columns and alias columns.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Schema {
    pub(crate) fields: Vec<FieldRecord>,
    pub(crate) columns: Vec<ColumnRecord>,
    pub(crate) aliases: Vec<AliasColumn>,
}

impl Schema {
    fn read(le: &mut Le<'_>) -> Result<Self> {
        let fields = le.list(|l| l.record(field_record))?;
        let columns = le.list(|l| l.record(column_record))?;
        let aliases = le.list(|l| l.record(alias_column))?;
        le.list(|l| l.record(|_| Ok(())))?; // extra type information
        Ok(Self {
            fields,
            columns,
            aliases,
        })
    }

    fn extend(&mut self, other: Self) {
        self.fields.extend(other.fields);
        self.columns.extend(other.columns);
        self.aliases.extend(other.aliases);
    }
}

/// The header envelope.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Header {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) writer: String,
    pub(crate) schema: Schema,
    pub(crate) checksum: u64,
}

impl Header {
    pub(crate) fn decode(data: &[u8]) -> Result<Self> {
        let (mut le, checksum) = envelope(data, HEADER)?;
        feature_flags(&mut le)?;
        let (name, description, writer) = (le.string()?, le.string()?, le.string()?);
        let schema = Schema::read(&mut le)?;
        Ok(Self {
            name,
            description,
            writer,
            schema,
            checksum,
        })
    }
}

/// A cluster group: entries and the page list of its clusters.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClusterGroup {
    pub(crate) num_clusters: u32,
    pub(crate) page_list: EnvLink,
}

/// The footer envelope.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Footer {
    pub(crate) extension: Schema,
    pub(crate) groups: Vec<ClusterGroup>,
}

impl Footer {
    pub(crate) fn decode(data: &[u8], header_checksum: u64) -> Result<Self> {
        let (mut le, _) = envelope(data, FOOTER)?;
        feature_flags(&mut le)?;
        if le.u64()? != header_checksum {
            return Err(Error::malformed(
                "the RNTuple footer belongs to another header",
            ));
        }
        let extension = le.record(Schema::read)?;
        let groups = le.list(|l| {
            l.record(|r| {
                r.u64()?; // first entry
                r.u64()?; // entry span
                let num_clusters = r.u32()?;
                Ok(ClusterGroup {
                    num_clusters,
                    page_list: env_link(r)?,
                })
            })
        })?;
        // Linked attribute sets (format 1.0.1) follow; not needed to read.
        Ok(Self { extension, groups })
    }

    pub(crate) fn full_schema(&self, header: &Header) -> Schema {
        let mut s = header.schema.clone();
        s.extend(self.extension.clone());
        s
    }
}

/// One page of a column in a cluster.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Page {
    pub(crate) elements: u64,
    pub(crate) checksum: bool,
    pub(crate) locator: Locator,
}

/// A column's pages in one cluster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ColumnPages {
    pub(crate) pages: Vec<Page>,
    /// The global index of the column's first element in this cluster;
    /// `None` when the column is suppressed in this cluster.
    pub(crate) first_element: Option<u64>,
}

/// A cluster: its entries and each column's pages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cluster {
    pub(crate) first_entry: u64,
    pub(crate) entries: u64,
    pub(crate) columns: Vec<ColumnPages>,
}

/// Decodes a page-list envelope into its clusters.
pub(crate) fn page_list(data: &[u8], header_checksum: u64) -> Result<Vec<Cluster>> {
    let (mut le, _) = envelope(data, PAGE_LIST)?;
    if le.u64()? != header_checksum {
        return Err(Error::malformed(
            "an RNTuple page list belongs to another header",
        ));
    }
    let summaries = le.list(|l| {
        l.record(|r| {
            let first = r.u64()?;
            let raw = r.u64()?;
            if raw >> 56 & 0x01 != 0 {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "sharded RNTuple clusters",
                ));
            }
            Ok((first, raw & 0x00FF_FFFF_FFFF_FFFF))
        })
    })?;
    let columns_per_cluster = le.list(|l| {
        l.list(|c| {
            let (pages, mut tail) = c.list_with_tail(|p| {
                let n = p.i32()?;
                Ok(Page {
                    elements: u64::from(n.unsigned_abs()),
                    checksum: n < 0,
                    locator: locator(p)?,
                })
            })?;
            let offset = tail.i64()?;
            if offset >= 0 {
                tail.u32()?; // compression settings
            }
            Ok(ColumnPages {
                pages,
                first_element: u64::try_from(offset).ok(),
            })
        })
    })?;
    if summaries.len() != columns_per_cluster.len() {
        return Err(Error::malformed(
            "RNTuple page list: clusters and summaries differ in number",
        ));
    }
    Ok(summaries
        .into_iter()
        .zip(columns_per_cluster)
        .map(|((first_entry, entries), columns)| Cluster {
            first_entry,
            entries,
            columns,
        })
        .collect())
}
