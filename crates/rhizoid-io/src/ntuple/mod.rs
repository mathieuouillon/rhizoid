//! Reading `RNTuple`s: [`RNTuple`], [`Field`], [`FieldReader`].
//!
//! An `RNTuple` stored in a ROOT file is found by its anchor key
//! (`ROOT::RNTuple`); the anchor points at the header and footer
//! envelopes, and the footer at the page lists of its clusters.
//!
//! Fields of simple types read into the same [`Column`]s as `TTree`
//! branches (scalars, fixed arrays, `std::vector`/`RVec` of scalars,
//! strings), typed with the same [`ColumnType`]s; every other field
//! (records, nested collections, variants, tuples, …) reads as one
//! [`Value`] per entry.
//!
//! ```
//! use rhizoid_io::columnar::Jagged;
//! # let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/uproot_rntuple.root");
//! let file = rhizoid_io::File::open(path)?;
//! let ntuple = file.rntuple("events")?;
//! let x: Vec<f64> = ntuple.field::<f64>("x")?.read(..)?;
//! let jets = ntuple.field::<Jagged<f32>>("jet_pt")?.read(0..10)?;
//! assert_eq!(x.len(), usize::try_from(ntuple.num_entries()).unwrap_or_default());
//! # Ok::<(), rhizoid_io::Error>(())
//! ```

mod format;
mod pages;
pub(crate) mod write;

/// The readers of an `RNTuple`, shared with trees (defined in [`tree`](crate::tree)).
pub use crate::tree::{
    EntrySelection, Projection, RNTupleReader, RNTupleReaderBuilder, RNTupleRows,
};
pub use write::{RNTupleOptions, RNTupleWriter};

use core::fmt;
use core::marker::PhantomData;
use core::ops::Range;
use std::sync::Arc;

use rhizoid_source::FileOffset;

use crate::columnar::{Column, ColumnType, Interpretation, extend_array};
use crate::compress;
use crate::core::error::{Context, Error, ErrorKind, Result};
use crate::file::File;
use crate::streamer::{Array, DynObject, Scalar, Value};
use crate::tree::{Readable, ReaderBuilder, SyncReadable, sealed};
use rhizoid_source::ReadAt;

use format::{COLLECTION, Cluster, ColumnRecord, FieldRecord, LEAF, RECORD, STREAMER, VARIANT};
use pages::Decoded;

/// The columns of one position of a field, one per representation.
type Alternatives = Vec<u32>;

/// How a field is stored, and so read.
#[derive(Clone, Debug)]
enum Kind {
    /// One value per element.
    Scalar {
        column: Alternatives,
        scalar: Scalar,
    },
    /// `n` values per element in the field's own column (`std::bitset<N>`).
    Fixed {
        column: Alternatives,
        scalar: Scalar,
        n: u64,
    },
    /// `n` elements of the child per element (`std::array<T, N>`).
    Repeated {
        child: u32,
        n: u64,
    },
    /// `std::string`: offsets and characters.
    Text {
        index: Alternatives,
        chars: Alternatives,
    },
    /// A collection: offsets into its children's elements.
    Collection {
        index: Alternatives,
        children: Vec<u32>,
    },
    /// Sub-fields sharing the element.
    Record {
        children: Vec<u32>,
    },
    /// One alternative per element (`std::variant`).
    Variant {
        switch: Alternatives,
        alternatives: Vec<u32>,
    },
    /// The size of a collection (`RNTupleCardinality<T>`).
    Cardinality {
        index: Alternatives,
        wide: bool,
    },
    /// A wrapper read as its only child (`std::atomic<T>`).
    Transparent {
        child: u32,
    },
    Unsupported(String),
}

#[derive(Debug)]
struct Model {
    fields: Vec<FieldRecord>,
    children: Vec<Vec<u32>>,
    top: Vec<u32>,
    columns: Vec<ColumnRecord>,
    field_columns: Vec<Vec<Alternatives>>,
    kinds: Vec<Kind>,
    clusters: Vec<Cluster>,
}

impl Model {
    fn new(schema: format::Schema, clusters: Vec<Cluster>) -> Self {
        let n = schema.fields.len();
        let mut children = vec![Vec::new(); n];
        let mut top = Vec::new();
        for (i, f) in schema.fields.iter().enumerate() {
            let id = u32::try_from(i).unwrap_or(u32::MAX);
            if f.parent == id {
                top.push(id);
            } else if let Some(c) =
                children.get_mut(usize::try_from(f.parent).unwrap_or(usize::MAX))
            {
                c.push(id);
            }
        }
        // Columns per field, by representation, then by position.
        let mut by_repr: Vec<Vec<Vec<u32>>> = vec![Vec::new(); n];
        for (i, c) in schema.columns.iter().enumerate() {
            let Some(groups) = by_repr.get_mut(usize::try_from(c.field).unwrap_or(usize::MAX))
            else {
                continue;
            };
            let r = usize::from(c.representation);
            if groups.len() <= r {
                groups.resize(r + 1, Vec::new());
            }
            groups[r].push(u32::try_from(i).unwrap_or(u32::MAX));
        }
        let mut field_columns: Vec<Vec<Alternatives>> = by_repr
            .iter()
            .map(|groups| {
                let positions = groups.first().map_or(0, Vec::len);
                (0..positions)
                    .map(|p| groups.iter().filter_map(|g| g.get(p).copied()).collect())
                    .collect()
            })
            .collect();
        for a in &schema.aliases {
            if let Some(cols) =
                field_columns.get_mut(usize::try_from(a.field).unwrap_or(usize::MAX))
            {
                cols.push(vec![a.physical]);
            }
        }
        let mut model = Self {
            fields: schema.fields,
            children,
            top,
            columns: schema.columns,
            field_columns,
            kinds: Vec::new(),
            clusters,
        };
        model.kinds = (0..n).map(|f| model.kind(f)).collect();
        model
    }

    fn kind(&self, f: usize) -> Kind {
        let (Some(rec), Some(cols), Some(kids)) = (
            self.fields.get(f),
            self.field_columns.get(f),
            self.children.get(f),
        ) else {
            return Kind::Unsupported("no such field".to_owned());
        };
        let scalar_of = |alts: &Alternatives| {
            alts.first()
                .and_then(|&c| self.columns.get(usize::try_from(c).ok()?))
                .and_then(|c| pages::scalar_of(c.kind))
        };
        if rec.type_name.starts_with("ROOT::RNTupleCardinality")
            && let Some(index) = cols.first()
        {
            return Kind::Cardinality {
                index: index.clone(),
                wide: rec.type_name.contains("64"),
            };
        }
        match rec.role {
            LEAF => match (cols.as_slice(), rec.repetition, kids.as_slice()) {
                ([c], 0, []) => scalar_of(c).map_or_else(
                    || Kind::Unsupported(format!("column type of `{}`", rec.type_name)),
                    |scalar| Kind::Scalar {
                        column: c.clone(),
                        scalar,
                    },
                ),
                ([c], n, []) => scalar_of(c).map_or_else(
                    || Kind::Unsupported(format!("column type of `{}`", rec.type_name)),
                    |scalar| Kind::Fixed {
                        column: c.clone(),
                        scalar,
                        n,
                    },
                ),
                ([index, chars], 0, []) => Kind::Text {
                    index: index.clone(),
                    chars: chars.clone(),
                },
                ([], n, [child]) if n > 0 => Kind::Repeated { child: *child, n },
                ([], 0, [child]) => Kind::Transparent { child: *child },
                _ => Kind::Unsupported(format!(
                    "leaf field `{}` of type `{}`",
                    rec.name, rec.type_name
                )),
            },
            COLLECTION => match cols.as_slice() {
                [index] => Kind::Collection {
                    index: index.clone(),
                    children: kids.clone(),
                },
                _ => Kind::Unsupported(format!(
                    "collection `{}` without one offset column",
                    rec.name
                )),
            },
            RECORD => Kind::Record {
                children: kids.clone(),
            },
            VARIANT => match cols.as_slice() {
                [switch] => Kind::Variant {
                    switch: switch.clone(),
                    alternatives: kids.clone(),
                },
                _ => Kind::Unsupported(format!("variant `{}` without a switch column", rec.name)),
            },
            STREAMER => {
                Kind::Unsupported(format!("`{}` is stored by a ROOT streamer", rec.type_name))
            }
            r => Kind::Unsupported(format!("field role {r}")),
        }
    }

    fn kind_of(&self, f: u32) -> &Kind {
        static MISSING: Kind = Kind::Transparent { child: u32::MAX };
        self.kinds
            .get(usize::try_from(f).unwrap_or(usize::MAX))
            .unwrap_or(&MISSING)
    }

    /// The first column of a field's subtree: its elements are the field's.
    fn principal(&self, f: u32) -> Option<&Alternatives> {
        let i = usize::try_from(f).ok()?;
        if let Some(c) = self.field_columns.get(i)?.first() {
            return Some(c);
        }
        self.children
            .get(i)?
            .iter()
            .find_map(|&c| self.principal(c))
    }

    fn column(&self, c: u32) -> Option<&ColumnRecord> {
        self.columns.get(usize::try_from(c).ok()?)
    }

    /// The first element of the first present alternative in a cluster.
    fn first_element(&self, alts: &[u32], cluster: usize) -> Option<u64> {
        let cl = self.clusters.get(cluster)?;
        alts.iter()
            .find_map(|&c| cl.columns.get(usize::try_from(c).ok()?)?.first_element)
    }

    fn interpretation(&self, f: u32) -> Result<Interpretation> {
        let rec = self.fields.get(usize::try_from(f).unwrap_or(usize::MAX));
        let class = || rec.map_or_else(String::new, |r| r.type_name.clone());
        Ok(match self.kind_of(f) {
            Kind::Scalar { scalar, .. } => Interpretation::Primitive(*scalar),
            Kind::Fixed { scalar, n, .. } => Interpretation::FixedArray {
                scalar: *scalar,
                len: usize::try_from(*n).unwrap_or(usize::MAX),
            },
            Kind::Repeated { child, n } => match self.kind_of(*child) {
                Kind::Scalar { scalar, .. } => Interpretation::FixedArray {
                    scalar: *scalar,
                    len: usize::try_from(*n).unwrap_or(usize::MAX),
                },
                _ => Interpretation::Objects { class: class() },
            },
            Kind::Text { .. } => Interpretation::Strings,
            Kind::Collection { children, .. } => match children.as_slice() {
                [c] => match self.kind_of(*c) {
                    Kind::Scalar { scalar, .. } => Interpretation::Jagged {
                        scalar: *scalar,
                        inner: 1,
                    },
                    _ => Interpretation::Objects { class: class() },
                },
                _ => Interpretation::Objects { class: class() },
            },
            Kind::Cardinality { wide, .. } => {
                Interpretation::Primitive(if *wide { Scalar::U64 } else { Scalar::U32 })
            }
            Kind::Transparent { child } => return self.interpretation(*child),
            Kind::Record { .. } | Kind::Variant { .. } => {
                Interpretation::Objects { class: class() }
            }
            Kind::Unsupported(why) => return Err(Error::new(ErrorKind::Unsupported, why.clone())),
        })
    }
}

/// An `RNTuple`: its fields and clusters. Cheap to clone; `Send + Sync`.
#[derive(Clone)]
pub struct RNTuple {
    inner: Arc<Inner>,
    file: File,
}

struct Inner {
    path: String,
    name: String,
    description: String,
    writer: String,
    version: [u16; 4],
    entries: u64,
    clusters: Vec<Range<u64>>,
    model: Model,
}

impl fmt::Debug for RNTuple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RNTuple")
            .field("name", &self.inner.name)
            .field("entries", &self.inner.entries)
            .field("fields", &self.inner.model.top.len())
            .finish_non_exhaustive()
    }
}

/// Which elements of a column a read needs.
#[derive(Clone, Debug)]
enum Need {
    /// These elements.
    Elements(Vec<Range<u64>>),
    /// Every element in the clusters the read touches.
    Clusters,
}

impl Need {
    fn times(&self, n: u64) -> Self {
        match self {
            Self::Elements(rs) => Self::Elements(
                rs.iter()
                    .map(|r| r.start.saturating_mul(n)..r.end.saturating_mul(n))
                    .collect(),
            ),
            Self::Clusters => Self::Clusters,
        }
    }

    /// With the element before each range: offsets columns read it.
    fn with_previous(&self) -> Self {
        match self {
            Self::Elements(rs) => Self::Elements(
                rs.iter()
                    .map(|r| r.start.saturating_sub(1)..r.end)
                    .collect(),
            ),
            Self::Clusters => Self::Clusters,
        }
    }
}

impl sealed::Sealed for RNTuple {}

impl Readable for RNTuple {
    fn num_entries_(&self) -> u64 {
        self.inner.entries
    }
    fn clusters_(&self) -> &[Range<u64>] {
        &self.inner.clusters
    }
    fn every_column(&self) -> Vec<String> {
        self.fields()
            .iter()
            .filter(|f| f.interpretation().is_ok())
            .map(|f| f.name().to_owned())
            .collect()
    }
    fn interpretation_of(&self, name: &str) -> Result<Interpretation> {
        self.field_or_err(name)?.interpretation()
    }
    fn expand(&self, name: &str) -> Result<Vec<String>> {
        self.field_or_err(name)?.interpretation()?;
        Ok(vec![name.to_owned()])
    }
}

impl SyncReadable for RNTuple {
    fn read_column(&self, name: &str, runs: &[Range<u64>]) -> Result<Column> {
        self.read(name, EntrySelection::from_ranges(runs.iter().cloned()))
    }
    fn with_source(&self, source: Arc<dyn ReadAt>) -> Self {
        RNTuple::with_source(self, source)
    }
    fn plan(&self, name: &str, runs: &[Range<u64>]) -> Vec<Range<FileOffset>> {
        self.get_field(name)
            .map(|f| RNTuple::plan(self, f.id, runs))
            .unwrap_or_default()
    }
    fn file_len(&self) -> u64 {
        self.file.len()
    }
}

/// One segment of decoded column data.
struct Segment {
    cluster: usize,
    start: u64,
    data: Decoded,
}

fn zeros(s: Scalar, n: usize) -> Array {
    match s {
        Scalar::Bool => Array::Bool(vec![false; n]),
        Scalar::I8 => Array::I8(vec![0; n]),
        Scalar::U8 => Array::U8(vec![0; n]),
        Scalar::I16 => Array::I16(vec![0; n]),
        Scalar::U16 => Array::U16(vec![0; n]),
        Scalar::I32 => Array::I32(vec![0; n]),
        Scalar::U32 => Array::U32(vec![0; n]),
        Scalar::I64 => Array::I64(vec![0; n]),
        Scalar::U64 => Array::U64(vec![0; n]),
        Scalar::F32 | Scalar::Float16(_) => Array::F32(vec![0.0; n]),
        _ => Array::F64(vec![0.0; n]),
    }
}

/// `a[r]` as a new array.
pub(crate) fn slice(a: &Array, r: Range<usize>) -> Array {
    macro_rules! s {
        ($($v:ident),*) => {
            match a {
                $(Array::$v(v) => Array::$v(v.get(r).unwrap_or_default().to_vec()),)*
            }
        };
    }
    s!(Bool, I8, U8, I16, U16, I32, U32, I64, U64, F32, F64)
}

fn to_usize(n: u64) -> Result<usize> {
    usize::try_from(n)
        .map_err(|_| Error::new(ErrorKind::LimitExceeded, "more elements than memory holds"))
}

impl RNTuple {
    pub(crate) fn open(file: File, path: String, anchor_bytes: &[u8]) -> Result<Self> {
        let anchor = format::Anchor::decode(anchor_bytes)?;
        let read = |seek: u64, nbytes: u64, len: u64| -> Result<Vec<u8>> {
            if len > file.max_object_bytes() {
                return Err(Error::new(
                    ErrorKind::LimitExceeded,
                    format!("RNTuple metadata of {len} bytes"),
                ));
            }
            let at = FileOffset::new(seek);
            let end = at
                .checked_add(nbytes)
                .ok_or_else(|| Error::malformed("RNTuple envelope position"))?;
            let raw = file.source().read_at(at..end)?;
            if nbytes == len {
                Ok(raw.to_vec())
            } else {
                compress::decompress(&raw, to_usize(len)?)
            }
        };
        let header = format::Header::decode(&read(
            anchor.seek_header,
            anchor.nbytes_header,
            anchor.len_header,
        )?)?;
        let footer = format::Footer::decode(
            &read(anchor.seek_footer, anchor.nbytes_footer, anchor.len_footer)?,
            header.checksum,
        )?;
        let mut clusters = Vec::new();
        for g in &footer.groups {
            let l = g.page_list;
            let list = format::page_list(
                &read(l.locator.offset, l.locator.nbytes, l.len)?,
                header.checksum,
            )?;
            if list.len() != usize::try_from(g.num_clusters).unwrap_or(usize::MAX) {
                return Err(Error::malformed(
                    "an RNTuple cluster group lists a different number of clusters",
                ));
            }
            clusters.extend(list);
        }
        clusters.sort_by_key(|c| c.first_entry);
        let entries = clusters.last().map_or(0, |c| c.first_entry + c.entries);
        let ranges = clusters
            .iter()
            .map(|c| c.first_entry..c.first_entry + c.entries)
            .collect();
        let schema = footer.full_schema(&header);
        Ok(Self {
            file,
            inner: Arc::new(Inner {
                path,
                name: header.name.clone(),
                description: header.description.clone(),
                writer: header.writer.clone(),
                version: anchor.version,
                entries,
                clusters: ranges,
                model: Model::new(schema, clusters),
            }),
        })
    }

    /// The same `RNTuple`, reading through `source` (see
    /// `File::with_source`).
    pub(crate) fn with_source(&self, source: Arc<dyn ReadAt>) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            file: self.file.with_source(source),
        }
    }

    /// A reader of every readable top-level field over every entry, one
    /// batch per cluster; narrow it with the builder's methods.
    pub fn reader(&self) -> RNTupleReaderBuilder {
        ReaderBuilder::new(self.clone())
    }

    /// Where a page's bytes are: its locator, and the checksum after it.
    fn page_range(&self, p: &format::Page) -> Range<FileOffset> {
        let extra = if p.checksum { 8 } else { 0 };
        let end = p
            .locator
            .offset
            .saturating_add(p.locator.nbytes)
            .saturating_add(extra)
            .min(self.file.len());
        FileOffset::new(p.locator.offset)..FileOffset::new(end.max(p.locator.offset))
    }

    /// The byte ranges reading field `id` over entries `runs` fetches: the
    /// pages holding its elements where they follow from the entries, and
    /// every page of the touched clusters below a collection or variant,
    /// whose element ranges depend on the data.
    pub(crate) fn plan(&self, id: u32, runs: &[Range<u64>]) -> Vec<Range<FileOffset>> {
        let m = &self.inner.model;
        let mut needs = Vec::new();
        self.plan_field(id, Need::Elements(runs.to_vec()), &mut needs, 0);
        let touched = |c: &Cluster| {
            runs.iter()
                .any(|r| r.start < c.first_entry.saturating_add(c.entries) && r.end > c.first_entry)
        };
        let mut out = Vec::new();
        for (alts, need) in &needs {
            for cl in &m.clusters {
                if matches!(need, Need::Clusters) && !touched(cl) {
                    continue;
                }
                let Some((cp, mut start)) = alts.iter().find_map(|&col| {
                    let cp = cl.columns.get(usize::try_from(col).ok()?)?;
                    Some((cp, cp.first_element?))
                }) else {
                    continue;
                };
                for page in &cp.pages {
                    let end = start.saturating_add(page.elements);
                    let wanted = match need {
                        Need::Clusters => true,
                        Need::Elements(rs) => rs.iter().any(|r| start < r.end && end > r.start),
                    };
                    if wanted && page.elements > 0 {
                        out.push(self.page_range(page));
                    }
                    start = end;
                }
            }
        }
        out
    }

    fn plan_field(&self, id: u32, need: Need, out: &mut Vec<(Alternatives, Need)>, depth: usize) {
        if depth > 256 {
            return;
        }
        let d = depth + 1;
        match self.inner.model.kind_of(id) {
            Kind::Scalar { column, .. } => out.push((column.clone(), need)),
            Kind::Fixed { column, n, .. } => out.push((column.clone(), need.times(*n))),
            Kind::Repeated { child, n } => self.plan_field(*child, need.times(*n), out, d),
            Kind::Text { index, chars } => {
                out.push((index.clone(), need.with_previous()));
                out.push((chars.clone(), Need::Clusters));
            }
            Kind::Collection { index, children } => {
                out.push((index.clone(), need.with_previous()));
                for &c in children {
                    self.plan_field(c, Need::Clusters, out, d);
                }
            }
            Kind::Record { children } => {
                for &c in children {
                    self.plan_field(c, need.clone(), out, d);
                }
            }
            Kind::Variant {
                switch,
                alternatives,
            } => {
                out.push((switch.clone(), need));
                for &a in alternatives {
                    self.plan_field(a, Need::Clusters, out, d);
                }
            }
            Kind::Cardinality { index, .. } => out.push((index.clone(), need.with_previous())),
            Kind::Transparent { child } => self.plan_field(*child, need, out, d),
            Kind::Unsupported(_) => {}
        }
    }

    /// The `RNTuple`'s name.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Its description.
    pub fn description(&self) -> &str {
        &self.inner.description
    }

    /// The software that wrote it (`ROOT v6.34.00`, `uproot 5.7`, …).
    pub fn written_by(&self) -> &str {
        &self.inner.writer
    }

    /// The format version: epoch, major, minor, patch.
    pub fn format_version(&self) -> [u16; 4] {
        self.inner.version
    }

    /// The number of entries.
    pub fn num_entries(&self) -> u64 {
        self.inner.entries
    }

    /// The entry ranges of the clusters.
    pub fn clusters(&self) -> &[Range<u64>] {
        &self.inner.clusters
    }

    /// The top-level fields.
    pub fn fields(&self) -> Vec<Field<'_>> {
        self.inner
            .model
            .top
            .iter()
            .map(|&id| Field { ntuple: self, id })
            .collect()
    }

    /// A field by name; sub-fields of records by dotted path
    /// (`"jet.pt"`).
    pub fn get_field(&self, path: &str) -> Option<Field<'_>> {
        let m = &self.inner.model;
        let mut level: &[u32] = &m.top;
        let mut found = None;
        for part in path.split('.') {
            let id = *level.iter().find(|&&f| {
                m.fields
                    .get(usize::try_from(f).unwrap_or(usize::MAX))
                    .is_some_and(|r| r.name == part)
            })?;
            found = Some(id);
            level = m.children.get(usize::try_from(id).ok()?)?;
        }
        found.map(|id| Field { ntuple: self, id })
    }

    fn field_or_err(&self, path: &str) -> Result<Field<'_>> {
        let f = self.get_field(path).ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                format!("RNTuple `{}` has no field `{path}`", self.inner.path),
            )
        })?;
        // Every step must keep the entry as the element.
        let m = &self.inner.model;
        let mut id = f.id;
        loop {
            let parent = m
                .fields
                .get(usize::try_from(id).unwrap_or(usize::MAX))
                .map_or(id, |r| r.parent);
            if parent == id {
                break;
            }
            if !matches!(
                m.kind_of(parent),
                Kind::Record { .. } | Kind::Transparent { .. }
            ) {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    format!("`{path}` is inside a collection; read the collection instead"),
                ));
            }
            id = parent;
        }
        Ok(f)
    }

    /// Opens a field for typed reading; the type is checked here, once.
    ///
    /// # Errors
    ///
    /// `NotFound`; `Unsupported` for fields this version cannot read;
    /// `TypeMismatch` if `T` does not match the field.
    pub fn field<T: ColumnType>(&self, path: &str) -> Result<FieldReader<'_, T>> {
        let field = self.field_or_err(path)?;
        let i = field.interpretation()?;
        if !T::accepts(&i) {
            return Err(Error::new(
                ErrorKind::TypeMismatch,
                format!("field `{path}` holds {i}, not {}", T::describe()),
            ));
        }
        Ok(FieldReader {
            ntuple: self,
            id: field.id,
            _type: PhantomData,
        })
    }

    /// Reads a field over a selection of entries, without naming its type.
    ///
    /// # Errors
    ///
    /// As [`RNTuple::field`], plus `InvalidInput` for a selection outside
    /// the `RNTuple`, and read, decompression and format errors.
    pub fn read(&self, path: &str, selection: impl Into<EntrySelection>) -> Result<Column> {
        let field = self.field_or_err(path)?;
        self.read_field(field.id, &selection.into())
    }

    fn read_field(&self, id: u32, selection: &EntrySelection) -> Result<Column> {
        let runs = selection.resolve(self.inner.entries)?;
        self.inner.model.interpretation(id)?;
        let label = || {
            let name = self
                .inner
                .model
                .fields
                .get(usize::try_from(id).unwrap_or(usize::MAX))
                .map_or("", |r| r.name.as_str());
            Context::Key(format!("{}:{name}", self.inner.path))
        };
        let mut out: Option<Column> = None;
        for run in runs {
            let col = self.column(id, run).map_err(|e| e.with_context(label()))?;
            out = Some(match out {
                None => col,
                Some(prev) => concat(prev, col)?,
            });
        }
        match out {
            Some(c) => Ok(c),
            None => self.column(id, 0..0).map_err(|e| e.with_context(label())),
        }
    }

    /// Fetches and decodes the pages of a column that hold `range`.
    fn segments(&self, alts: &[u32], range: &Range<u64>) -> Result<Vec<Segment>> {
        let m = &self.inner.model;
        let mut plan = Vec::new();
        for (c, cl) in m.clusters.iter().enumerate() {
            let pick = alts.iter().find_map(|&col| {
                let cp = cl.columns.get(usize::try_from(col).ok()?)?;
                Some((col, cp, cp.first_element?))
            });
            let Some((col, cp, mut start)) = pick else {
                continue;
            };
            for page in &cp.pages {
                let end = start + page.elements;
                if start < range.end && end > range.start && page.elements > 0 {
                    plan.push((c, col, start, *page));
                }
                start = end;
            }
        }
        let ranges: Vec<Range<FileOffset>> =
            plan.iter().map(|(_, _, _, p)| self.page_range(p)).collect();
        let fetched = if ranges.is_empty() {
            Vec::new()
        } else {
            self.file.source().read_ranges(&ranges)?
        };
        let mut out = Vec::with_capacity(plan.len());
        for ((c, col, start, page), bytes) in plan.into_iter().zip(fetched) {
            let rec = m
                .column(col)
                .ok_or_else(|| Error::malformed("page of an unknown column"))?;
            let n = to_usize(page.elements)?;
            let payload = page_payload(&bytes, page.locator.nbytes, page.checksum)?;
            let expected = pages::page_bytes(rec.kind, rec.bits, page.elements);
            if expected > self.file.max_object_bytes() {
                return Err(Error::new(
                    ErrorKind::LimitExceeded,
                    format!("a page of {expected} bytes"),
                ));
            }
            let raw = if u64::try_from(payload.len()).unwrap_or(u64::MAX) < expected {
                std::borrow::Cow::Owned(compress::decompress(payload, to_usize(expected)?)?)
            } else {
                std::borrow::Cow::Borrowed(payload)
            };
            let data = pages::decode(rec.kind, rec.bits, rec.range, &raw, n).map_err(|e| {
                e.with_context(Context::Offset(FileOffset::new(page.locator.offset)))
            })?;
            out.push(Segment {
                cluster: c,
                start,
                data,
            });
        }
        Ok(out)
    }

    /// The values of a column over `range`; elements in no page (a column
    /// added after them) read as zero.
    fn values_of(&self, alts: &[u32], range: &Range<u64>) -> Result<Array> {
        let m = &self.inner.model;
        let scalar = alts
            .first()
            .and_then(|&c| m.column(c))
            .and_then(|c| pages::scalar_of(c.kind))
            .ok_or_else(|| Error::malformed("a column without values"))?;
        let mut out = zeros(scalar, 0);
        let mut at = range.start;
        for seg in self.segments(alts, range)? {
            let Decoded::Values(a) = seg.data else {
                return Err(Error::malformed("a switch column read as values"));
            };
            let end = seg.start + u64::try_from(a.len()).unwrap_or(0);
            let (lo, hi) = (seg.start.max(range.start), end.min(range.end));
            if lo >= hi {
                continue;
            }
            if lo > at {
                extend_array(&mut out, zeros(scalar, to_usize(lo - at)?))?;
            }
            extend_array(
                &mut out,
                slice(&a, to_usize(lo - seg.start)?..to_usize(hi - seg.start)?),
            )?;
            at = hi;
        }
        if at < range.end {
            extend_array(&mut out, zeros(scalar, to_usize(range.end - at)?))?;
        }
        Ok(out)
    }

    /// `(start, ends)` of elements `range` of a collection whose items are
    /// `children`.
    fn spans(
        &self,
        index: &[u32],
        children: &[u32],
        range: &Range<u64>,
    ) -> Result<(u64, Vec<u64>)> {
        let m = &self.inner.model;
        let target = children.iter().find_map(|&c| m.principal(c)).cloned();
        self.spans_of(index, target.as_deref(), range)
    }

    /// `(start, ends)` of elements `range`, offsets based on the first
    /// element of `target` in each cluster (no target: 0).
    fn spans_of(
        &self,
        index: &[u32],
        target: Option<&[u32]>,
        range: &Range<u64>,
    ) -> Result<(u64, Vec<u64>)> {
        if range.start == 0 {
            return Ok((0, self.offsets(index, target, range)?));
        }
        let mut ends = self.offsets(index, target, &(range.start - 1..range.end))?;
        let start = ends.remove(0);
        Ok((start, ends))
    }

    /// The column of field `id` over elements `range` of its element space.
    fn column(&self, id: u32, range: Range<u64>) -> Result<Column> {
        let m = &self.inner.model;
        Ok(match m.kind_of(id) {
            Kind::Scalar { column, .. } => Column::Values(self.values_of(column, &range)?),
            Kind::Fixed { column, n, .. } => Column::Fixed {
                len: to_usize(*n)?,
                values: self.values_of(column, &(range.start * n..range.end * n))?,
            },
            Kind::Repeated { child, n } if matches!(m.kind_of(*child), Kind::Scalar { .. }) => {
                let Column::Values(values) = self.column(*child, range.start * n..range.end * n)?
                else {
                    return Err(Error::malformed("a scalar field read as another column"));
                };
                Column::Fixed {
                    len: to_usize(*n)?,
                    values,
                }
            }
            Kind::Text { index, chars } => Column::Strings(self.strings(index, chars, &range)?),
            Kind::Collection { index, children }
                if children.len() == 1 && matches!(m.kind_of(children[0]), Kind::Scalar { .. }) =>
            {
                let (start, ends) = self.spans(index, children, &range)?;
                let end = ends.last().copied().unwrap_or(start);
                let Column::Values(values) = self.column(children[0], start..end)? else {
                    return Err(Error::malformed("a scalar field read as another column"));
                };
                let mut offsets = Vec::with_capacity(ends.len() + 1);
                offsets.push(0);
                for e in ends {
                    offsets.push(to_usize(e.saturating_sub(start))?);
                }
                Column::Jagged { offsets, values }
            }
            Kind::Cardinality { index, wide } => {
                let (start, ends) = self.spans_of(index, None, &range)?;
                let mut prev = start;
                let counts: Vec<u64> = ends
                    .iter()
                    .map(|&e| {
                        let c = e.saturating_sub(prev);
                        prev = e;
                        c
                    })
                    .collect();
                Column::Values(if *wide {
                    Array::U64(counts)
                } else {
                    Array::U32(
                        counts
                            .into_iter()
                            .map(|c| u32::try_from(c).unwrap_or(u32::MAX))
                            .collect(),
                    )
                })
            }
            Kind::Transparent { child } => self.column(*child, range)?,
            Kind::Unsupported(why) => return Err(Error::new(ErrorKind::Unsupported, why.clone())),
            _ => Column::Objects(self.values(id, &range)?),
        })
    }

    fn strings(&self, index: &[u32], chars: &[u32], range: &Range<u64>) -> Result<Vec<String>> {
        let mut out = Vec::with_capacity(to_usize(range.end - range.start)?);
        // The offsets point into the string's own characters column.
        let (start, ends) = self.spans_of(index, Some(chars), range)?;
        let end = ends.last().copied().unwrap_or(start);
        let Array::U8(bytes) = self.values_of(chars, &(start..end))? else {
            return Err(Error::malformed("string characters of another type"));
        };
        let mut prev = start;
        for e in ends {
            let (a, b) = (to_usize(prev - start)?, to_usize(e.saturating_sub(start))?);
            out.push(String::from_utf8_lossy(bytes.get(a..b).unwrap_or_default()).into_owned());
            prev = e;
        }
        Ok(out)
    }

    /// Global end offsets of elements `range` of an offset column: the
    /// stored offsets are cluster-local, relative to the first element of
    /// `target` (the items' column) in the cluster. Elements in no page (a
    /// deferred column) are empty.
    fn offsets(
        &self,
        index: &[u32],
        target: Option<&[u32]>,
        range: &Range<u64>,
    ) -> Result<Vec<u64>> {
        let m = &self.inner.model;
        let mut out: Vec<Option<u64>> = vec![None; to_usize(range.end - range.start)?];
        for seg in self.segments(index, range)? {
            let Decoded::Values(Array::U64(v)) = seg.data else {
                return Err(Error::malformed("an offset column of another type"));
            };
            let base = match target {
                Some(t) => m.first_element(t, seg.cluster).unwrap_or(0),
                None => 0,
            };
            for (k, local) in v.iter().enumerate() {
                let g = seg.start + u64::try_from(k).unwrap_or(0);
                if g >= range.start
                    && g < range.end
                    && let Some(slot) = out.get_mut(to_usize(g - range.start)?)
                {
                    *slot = Some(base + local);
                }
            }
        }
        let mut last = 0;
        Ok(out
            .into_iter()
            .map(|o| {
                last = o.unwrap_or(last);
                last
            })
            .collect())
    }

    /// The values of field `id` over elements `range`, one per element.
    fn values(&self, id: u32, range: &Range<u64>) -> Result<Vec<Value>> {
        let m = &self.inner.model;
        let n = to_usize(range.end - range.start)?;
        let rec = m
            .fields
            .get(usize::try_from(id).unwrap_or(usize::MAX))
            .ok_or_else(|| Error::malformed("unknown field"))?;
        Ok(match m.kind_of(id) {
            Kind::Scalar { column, .. } => {
                let a = self.values_of(column, range)?;
                (0..n).map(|i| a.get(i).unwrap_or(Value::Null)).collect()
            }
            Kind::Fixed { column, n: len, .. } => {
                let a = self.values_of(column, &(range.start * len..range.end * len))?;
                let len = to_usize(*len)?;
                (0..n)
                    .map(|i| Value::Array(slice(&a, i * len..(i + 1) * len)))
                    .collect()
            }
            Kind::Repeated { child, n: len } => {
                let items = self.values(*child, &(range.start * len..range.end * len))?;
                let len = to_usize(*len)?;
                group(items, &(0..=n).map(|i| i * len).collect::<Vec<_>>())
            }
            Kind::Text { index, chars } => self
                .strings(index, chars, range)?
                .into_iter()
                .map(Value::String)
                .collect(),
            Kind::Collection { index, children } => {
                let (start, ends) = self.spans(index, children, range)?;
                let end = ends.last().copied().unwrap_or(start);
                let items = match children.as_slice() {
                    [c] => self.values(*c, &(start..end))?,
                    cs => self.record_values("", cs, &(start..end))?,
                };
                let mut bounds = vec![0];
                for e in ends {
                    bounds.push(to_usize(e.saturating_sub(start))?);
                }
                group(items, &bounds)
            }
            Kind::Record { children } => self.record_values(&rec.type_name, children, range)?,
            Kind::Variant {
                switch,
                alternatives,
            } => self.variant_values(switch, alternatives, range)?,
            Kind::Transparent { child } => self.values(*child, range)?,
            Kind::Cardinality { .. } => {
                let Column::Values(a) = self.column(id, range.clone())? else {
                    return Err(Error::malformed("a cardinality of another type"));
                };
                (0..n).map(|i| a.get(i).unwrap_or(Value::Null)).collect()
            }
            Kind::Unsupported(why) => return Err(Error::new(ErrorKind::Unsupported, why.clone())),
        })
    }

    /// Records: one object per element, a member per sub-field; sub-fields
    /// named `:_N` are base classes.
    fn record_values(
        &self,
        class: &str,
        children: &[u32],
        range: &Range<u64>,
    ) -> Result<Vec<Value>> {
        let m = &self.inner.model;
        let n = to_usize(range.end - range.start)?;
        let mut objects: Vec<DynObject> = (0..n).map(|_| DynObject::new(class)).collect();
        for &c in children {
            let rec = m
                .fields
                .get(usize::try_from(c).unwrap_or(usize::MAX))
                .ok_or_else(|| Error::malformed("unknown field"))?;
            let values = self.values(c, range)?;
            for (o, v) in objects.iter_mut().zip(values) {
                match (rec.name.starts_with(":_"), v) {
                    (true, Value::Object(base)) => o.push_base(*base),
                    (_, v) => o.push_member(rec.name.clone(), v),
                }
            }
        }
        Ok(objects
            .into_iter()
            .map(|o| Value::Object(Box::new(o)))
            .collect())
    }

    fn variant_values(
        &self,
        switch: &[u32],
        alternatives: &[u32],
        range: &Range<u64>,
    ) -> Result<Vec<Value>> {
        let m = &self.inner.model;
        let n = to_usize(range.end - range.start)?;
        // (element, alternative, global index)
        let mut picks: Vec<Option<(usize, u64)>> = vec![None; n];
        for seg in self.segments(switch, range)? {
            let Decoded::Switch(v) = seg.data else {
                return Err(Error::malformed("a variant without a switch column"));
            };
            for (k, &(index, tag)) in v.iter().enumerate() {
                let g = seg.start + u64::try_from(k).unwrap_or(0);
                if g < range.start || g >= range.end || tag == 0 {
                    continue;
                }
                let alt = usize::try_from(tag - 1).unwrap_or(usize::MAX);
                let Some(&field) = alternatives.get(alt) else {
                    return Err(Error::malformed(format!(
                        "variant tag {tag} of {} alternatives",
                        alternatives.len()
                    )));
                };
                let base = m
                    .principal(field)
                    .and_then(|p| m.first_element(p, seg.cluster))
                    .unwrap_or(0);
                if let Some(slot) = picks.get_mut(to_usize(g - range.start)?) {
                    *slot = Some((alt, base + index));
                }
            }
        }
        let mut out = vec![Value::Null; n];
        for (alt, &field) in alternatives.iter().enumerate() {
            let wanted: Vec<(usize, u64)> = picks
                .iter()
                .enumerate()
                .filter_map(|(i, p)| p.filter(|(a, _)| *a == alt).map(|(_, g)| (i, g)))
                .collect();
            let (Some(lo), Some(hi)) = (
                wanted.iter().map(|(_, g)| *g).min(),
                wanted.iter().map(|(_, g)| *g).max(),
            ) else {
                continue;
            };
            let values = self.values(field, &(lo..hi + 1))?;
            for (i, g) in wanted {
                if let (Some(slot), Some(v)) = (out.get_mut(i), values.get(to_usize(g - lo)?)) {
                    *slot = v.clone();
                }
            }
        }
        Ok(out)
    }
}

/// Splits `items` at `bounds` (`[0, …, len]`) into lists; lists of scalars
/// become arrays.
fn group(items: Vec<Value>, bounds: &[usize]) -> Vec<Value> {
    let mut items = items.into_iter();
    bounds
        .windows(2)
        .map(|w| {
            let chunk: Vec<Value> = items.by_ref().take(w[1].saturating_sub(w[0])).collect();
            scalars_to_array(chunk)
        })
        .collect()
}

/// A list whose items are all one scalar type, as an array.
fn scalars_to_array(chunk: Vec<Value>) -> Value {
    macro_rules! try_array {
        ($($v:ident),*) => {
            match chunk.first() {
                $(Some(Value::$v(_)) => {
                    let v: Option<Vec<_>> = chunk.iter().map(|x| if let Value::$v(y) = x { Some(*y) } else { None }).collect();
                    if let Some(v) = v { return Value::Array(Array::$v(v)); }
                })*
                _ => {}
            }
        };
    }
    try_array!(Bool, I8, U8, I16, U16, I32, U32, I64, U64, F32, F64);
    Value::List(chunk)
}

/// Appends one column to another of the same shape.
fn concat(first: Column, more: Column) -> Result<Column> {
    Ok(match (first, more) {
        (Column::Values(mut acc), Column::Values(next)) => {
            extend_array(&mut acc, next)?;
            Column::Values(acc)
        }
        (
            Column::Fixed {
                len,
                values: mut acc,
            },
            Column::Fixed { values: next, .. },
        ) => {
            extend_array(&mut acc, next)?;
            Column::Fixed { len, values: acc }
        }
        (
            Column::Jagged {
                mut offsets,
                values: mut acc,
            },
            Column::Jagged {
                offsets: next_offsets,
                values: next,
            },
        ) => {
            let base = offsets.last().copied().unwrap_or(0);
            offsets.extend(next_offsets.iter().skip(1).map(|v| v + base));
            extend_array(&mut acc, next)?;
            Column::Jagged {
                offsets,
                values: acc,
            }
        }
        (Column::Strings(mut acc), Column::Strings(next)) => {
            acc.extend(next);
            Column::Strings(acc)
        }
        (Column::Objects(mut acc), Column::Objects(next)) => {
            acc.extend(next);
            Column::Objects(acc)
        }
        _ => return Err(Error::malformed("columns of different shapes")),
    })
}

/// The payload of a page, with its checksum (when it has one) checked:
/// an XXH3-64 of the stored bytes, right after the bytes the locator counts
/// (as ROOT writes it).
fn page_payload(bytes: &[u8], nbytes: u64, checksum: bool) -> Result<&[u8]> {
    let n = to_usize(nbytes)?;
    let payload = bytes
        .get(..n)
        .ok_or_else(|| Error::new(ErrorKind::UnexpectedEof, "a page is cut short"))?;
    if checksum {
        let stored = bytes
            .get(n..n + 8)
            .and_then(|s| <[u8; 8]>::try_from(s).ok())
            .ok_or_else(|| Error::new(ErrorKind::UnexpectedEof, "a page checksum is cut short"))?;
        if u64::from_le_bytes(stored) != xxhash_rust::xxh3::xxh3_64(payload) {
            return Err(Error::malformed("RNTuple page checksum mismatch"));
        }
    }
    Ok(payload)
}

/// A field of an `RNTuple`.
#[derive(Copy, Clone)]
pub struct Field<'a> {
    ntuple: &'a RNTuple,
    id: u32,
}

impl fmt::Debug for Field<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Field")
            .field("name", &self.name())
            .field("type", &self.type_name())
            .finish()
    }
}

impl<'a> Field<'a> {
    fn record(&self) -> Option<&'a FieldRecord> {
        self.ntuple
            .inner
            .model
            .fields
            .get(usize::try_from(self.id).ok()?)
    }

    /// The field's name.
    pub fn name(&self) -> &'a str {
        self.record().map_or("", |r| r.name.as_str())
    }

    /// The C++ type name (`std::vector<float>`, …).
    pub fn type_name(&self) -> &'a str {
        self.record().map_or("", |r| r.type_name.as_str())
    }

    /// The field's description.
    pub fn description(&self) -> &'a str {
        self.record().map_or("", |r| r.description.as_str())
    }

    /// The sub-fields.
    pub fn fields(&self) -> Vec<Field<'a>> {
        self.ntuple
            .inner
            .model
            .children
            .get(usize::try_from(self.id).unwrap_or(usize::MAX))
            .map(|c| {
                c.iter()
                    .map(|&id| Field {
                        ntuple: self.ntuple,
                        id,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// How the values are laid out.
    ///
    /// # Errors
    ///
    /// `Unsupported` for fields this version cannot read, saying why.
    pub fn interpretation(&self) -> Result<Interpretation> {
        self.ntuple.inner.model.interpretation(self.id)
    }
}

/// Reads one field as `T`; made by [`RNTuple::field`].
pub struct FieldReader<'a, T> {
    ntuple: &'a RNTuple,
    id: u32,
    _type: PhantomData<fn() -> T>,
}

impl<T> fmt::Debug for FieldReader<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FieldReader")
            .field("field", &self.id)
            .finish_non_exhaustive()
    }
}

impl<T: ColumnType> FieldReader<'_, T> {
    /// Reads a selection of entries: `..`, a range, or an
    /// [`EntrySelection`].
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a selection outside the `RNTuple`; read,
    /// decompression and format errors.
    pub fn read(&self, selection: impl Into<EntrySelection>) -> Result<T::Output> {
        T::from_column(self.ntuple.read_field(self.id, &selection.into())?)
    }
}
