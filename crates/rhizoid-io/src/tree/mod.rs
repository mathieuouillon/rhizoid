//! Reading `TTree`s: [`Tree`], [`Branch`], [`BranchReader`].
//!
//! The tree object itself is decoded by the dynamic decoder from the
//! file's class descriptions; this module turns it into branches, basket
//! tables and clusters, and decodes baskets into [`Column`]s.
//!
//! ```
//! use rhizoid_io::columnar::Jagged;
//! # let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/uproot_trees.root");
//! let file = rhizoid_io::File::open(path)?;
//! let tree = file.tree("events")?;
//! assert_eq!(tree.num_entries(), 1001);
//!
//! let f32s: Vec<f32> = tree.branch::<f32>("f32")?.read(..)?;
//! let jets = tree.branch::<Jagged<f32>>("jet_pt")?.read(0..10)?;
//! for (entry, pts) in jets.iter().enumerate() {
//!     assert_eq!(pts.len(), entry % 4);
//! }
//! # Ok::<(), rhizoid_io::Error>(())
//! ```

mod basket;
mod objects;
mod select;
pub(crate) mod split;
pub(crate) mod write;

#[cfg(feature = "async")]
pub(crate) use select::read_batch;
pub(crate) use select::sealed;
pub use select::{
    Batch, BatchSize, EntrySelection, Projection, RNTupleReader, RNTupleReaderBuilder, RNTupleRows,
    Readable, Reader, ReaderBuilder, Rows, SyncReadable, TreeReader, TreeReaderBuilder, TreeRows,
};
#[doc(hidden)]
pub use write::{LeafBuffer, LeafKind, LeafShape};
pub use write::{TreeOptions, TreeWriter};

use core::fmt;
use core::marker::PhantomData;
use core::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rhizoid_source::{FileOffset, ReadAt};

use crate::columnar::{Column, ColumnType, Interpretation, extend_array};
use crate::core::buffer::RBuffer;
use crate::core::error::{Context, Error, ErrorKind, Result, ResultExt};
use crate::file::File;
use crate::streamer::{
    self, Array, Decoder, DynObject, ElementKind, OnMismatch, Packing, Scalar, StreamerInfo,
    StreamerSet, Value,
};

/// A `TTree`: its branches, basket tables and clusters.
///
/// Cheap to clone; `Send + Sync`; every read takes `&self`.
#[derive(Clone)]
pub struct Tree {
    inner: Arc<Inner>,
    file: File,
}

struct Inner {
    path: String,
    name: String,
    title: String,
    entries: u64,
    branches: Vec<Branch>,
    clusters: Vec<Range<u64>>,
    stats: Stats,
}

#[derive(Default)]
struct Stats {
    requests: AtomicU64,
    baskets: AtomicU64,
    bytes_read: AtomicU64,
    bytes_decompressed: AtomicU64,
}

/// What reading has cost so far (all reads through one [`Tree`] handle and
/// its clones).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReadStats {
    /// Calls to the source (`read_ranges`), one per branch read.
    pub requests: u64,
    /// Baskets fetched and decoded.
    pub baskets: u64,
    /// Bytes fetched from the source.
    pub bytes_read: u64,
    /// Bytes after decompression.
    pub bytes_decompressed: u64,
}

impl fmt::Debug for Tree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tree")
            .field("name", &self.inner.name)
            .field("entries", &self.inner.entries)
            .field("branches", &self.inner.branches.len())
            .finish_non_exhaustive()
    }
}

/// Where a basket is, and which entries it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BasketRef {
    loc: Loc,
    entries: Range<u64>,
}

/// Where a basket's bytes are.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Loc {
    /// A record of its own.
    Disk { seek: FileOffset, bytes: u32 },
    /// Inside the tree object, already decoded (`fBaskets`).
    Embedded(Arc<basket::Basket>),
}

/// A branch of a tree.
#[derive(Clone, Debug, PartialEq)]
pub struct Branch {
    name: String,
    title: String,
    class: String,
    leaves: Vec<Leaf>,
    branches: Vec<Branch>,
    entries: u64,
    baskets: Vec<BasketRef>,
    tot_bytes: u64,
    zip_bytes: u64,
    element: Option<ElementInfo>,
    interpretation: core::result::Result<Interpretation, String>,
    codec: Codec,
}

/// What a `TBranchElement` adds: the member it stores and how.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ElementInfo {
    kind: i32,
    id: i32,
    streamer_type: i32,
    class_name: String,
    class_version: i32,
    /// The element class of a split collection (`fClonesName`).
    clones_name: String,
}

/// How the bytes of one entry are decoded, beyond the interpretation.
#[derive(Clone, Debug, PartialEq)]
enum Codec {
    /// The members of an object without its header (an unsplit top-level
    /// object, `fType` 0 with `fID` −1).
    Members { class: String, version: i16 },
    /// The values the interpretation says, after `header` bytes per entry
    /// (10 for `vector<T>`, 6 for `std::string`, 1 for `//[fN]` arrays).
    Plain { header: usize },
    /// An object with its header, by the class's description.
    Object(String),
    /// A value of a C++ type (STL collections with their header).
    Typed(String),
    /// A pointer: a class tag, then the object.
    Pointer,
    /// The leaves of a leaf list, back to back: name, type, count.
    LeafList(Vec<(String, Scalar, usize)>),
    /// A `char*`: an `i32` length, then the bytes.
    CharStar,
    /// `TObject::fBits` of each element of a split collection: a `u32`,
    /// followed by a 2-byte process ID when `kIsReferenced` (`0x10`) is set.
    Bits,
    /// A `//[fN]` array in each element of a split collection: per element,
    /// a 1-byte flag and the values; the counts are in a sibling branch.
    NestedVar { scalar: Scalar, counts: String },
    /// An STL member of each element of a split collection: a header, then
    /// the elements back to back.
    Elements { item: String },
}

/// A leaf of a branch: the type of its values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leaf {
    name: String,
    title: String,
    class: String,
    len: u32,
    unsigned: bool,
    is_counter: bool,
    count: Option<String>,
}

impl Leaf {
    fn from_dyn(obj: &DynObject) -> Result<Self> {
        let count = match obj.get("fLeafCount") {
            Some(Value::Object(c)) => Some(c.str("fName")?.to_owned()),
            _ => None,
        };
        Ok(Self {
            name: obj.str("fName")?.to_owned(),
            title: obj.str("fTitle")?.to_owned(),
            class: obj.class().to_owned(),
            len: u32::try_from(obj.member("fLen")?).unwrap_or(1).max(1),
            unsigned: obj
                .get("fIsUnsigned")
                .is_some_and(|v| bool::try_from(v).unwrap_or(false)),
            is_counter: obj
                .get("fIsRange")
                .is_some_and(|v| bool::try_from(v).unwrap_or(false)),
            count,
        })
    }

    /// The leaf's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The leaf's title (`name[count]/T` style).
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The leaf's class (`TLeafF`, `TLeafI`, …).
    pub fn class_name(&self) -> &str {
        &self.class
    }

    /// The name of the leaf holding this leaf's length, for variable-size
    /// arrays.
    pub fn count_leaf(&self) -> Option<&str> {
        self.count.as_deref()
    }

    /// Whether other leaves use this one as their length.
    pub fn is_counter(&self) -> bool {
        self.is_counter
    }

    fn scalar(&self) -> Option<Scalar> {
        let u = self.unsigned;
        Some(match self.class.as_str() {
            "TLeafO" => Scalar::Bool,
            "TLeafB" if u => Scalar::U8,
            "TLeafB" => Scalar::I8,
            "TLeafS" if u => Scalar::U16,
            "TLeafS" => Scalar::I16,
            "TLeafI" if u => Scalar::U32,
            "TLeafI" => Scalar::I32,
            "TLeafL" | "TLeafG" if u => Scalar::U64,
            "TLeafL" | "TLeafG" => Scalar::I64,
            "TLeafF" => Scalar::F32,
            "TLeafD" => Scalar::F64,
            "TLeafF16" => Scalar::Float16(Packing::from_title(&self.title, true)),
            "TLeafD32" => Scalar::Double32(Packing::from_title(&self.title, false)),
            _ => return None,
        })
    }
}

fn i64_array(obj: &DynObject, name: &str) -> Result<Vec<i64>> {
    match obj.get(name) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => (0..a.len())
            .map(|i| {
                match a {
                    Array::I32(v) => v.get(i).map(|&x| i64::from(x)),
                    Array::I64(v) => v.get(i).copied(),
                    _ => None,
                }
                .ok_or_else(|| Error::malformed(format!("{name} is not an integer array")))
            })
            .collect(),
        Some(v) => Err(v.mismatch("an array")),
    }
}

fn list<'a>(obj: &'a DynObject, name: &str) -> &'a [Value] {
    obj.get(name)
        .and_then(Value::as_object)
        .and_then(|a| a.get("fObjects"))
        .and_then(Value::as_list)
        .unwrap_or_default()
}

fn count(obj: &DynObject, name: &str) -> Result<u64> {
    let v = obj.member(name)?;
    let i = i64::try_from(v)?;
    u64::try_from(i).map_err(|_| Error::malformed(format!("negative {name} ({i})")))
}

impl Branch {
    fn from_dyn(obj: &DynObject) -> Result<Self> {
        let name = obj.str("fName")?.to_owned();
        let build = || -> Result<Self> {
            let leaves = list(obj, "fLeaves")
                .iter()
                .filter_map(Value::as_object)
                .map(Leaf::from_dyn)
                .collect::<Result<Vec<_>>>()?;
            let branches = list(obj, "fBranches")
                .iter()
                .filter_map(Value::as_object)
                .map(Branch::from_dyn)
                .collect::<Result<Vec<_>>>()?;
            let entries = count(obj, "fEntries")?;
            let written = usize::try_from(count(obj, "fWriteBasket")?).unwrap_or(usize::MAX);
            let (bytes, starts, seeks) = (
                i64_array(obj, "fBasketBytes")?,
                i64_array(obj, "fBasketEntry")?,
                i64_array(obj, "fBasketSeek")?,
            );
            let mut baskets = Vec::with_capacity(written.min(bytes.len()));
            for i in 0..written {
                let field = |v: &[i64], what: &str| -> Result<u64> {
                    v.get(i)
                        .and_then(|&x| u64::try_from(x).ok())
                        .ok_or_else(|| Error::malformed(format!("basket {i} has no valid {what}")))
                };
                let first = field(&starts, "first entry")?;
                let end = if i + 1 < written {
                    starts
                        .get(i + 1)
                        .and_then(|&x| u64::try_from(x).ok())
                        .ok_or_else(|| Error::malformed("basket entry table is too short"))?
                } else {
                    entries
                };
                if end < first || end > entries {
                    return Err(Error::malformed(format!(
                        "basket {i} holds entries {first}..{end} of {entries}"
                    )));
                }
                baskets.push(BasketRef {
                    loc: Loc::Disk {
                        seek: FileOffset::new(field(&seeks, "position")?),
                        bytes: u32::try_from(field(&bytes, "size")?)
                            .map_err(|_| Error::malformed("basket size"))?,
                    },
                    entries: first..end,
                });
            }
            // Baskets not yet written when the tree was saved live in the
            // tree object; they hold the entries after the written ones.
            let mut covered = baskets.last().map_or(0, |b| b.entries.end);
            for item in list(obj, "fBaskets") {
                if let Value::Opaque { class, bytes } = item
                    && class == "TBasket"
                {
                    let b = basket::decode_embedded(bytes)?;
                    let n = u64::try_from(b.entries).unwrap_or(0);
                    if n > 0 {
                        baskets.push(BasketRef {
                            loc: Loc::Embedded(Arc::new(b)),
                            entries: covered..covered + n,
                        });
                        covered += n;
                    }
                }
            }
            let element = if obj.class() == "TBranchElement" {
                let int = |n: &str| -> Result<i32> { i32::try_from(obj.member(n)?) };
                Some(ElementInfo {
                    kind: int("fType")?,
                    id: int("fID")?,
                    streamer_type: int("fStreamerType")?,
                    class_name: obj.str("fClassName")?.to_owned(),
                    class_version: int("fClassVersion")?,
                    clones_name: obj.str("fClonesName").unwrap_or_default().to_owned(),
                })
            } else {
                None
            };
            Ok(Self {
                name: name.clone(),
                title: obj.str("fTitle")?.to_owned(),
                class: obj.class().to_owned(),
                leaves,
                branches,
                entries,
                baskets,
                tot_bytes: count(obj, "fTotBytes").unwrap_or(0),
                zip_bytes: count(obj, "fZipBytes").unwrap_or(0),
                element,
                interpretation: Err("not resolved".to_owned()),
                codec: Codec::Plain { header: 0 },
            })
        };
        build().map_err(|e| e.with_context(Context::Key(format!("branch {name}"))))
    }

    /// Works out how to read this branch and its sub-branches.
    fn resolve(&mut self, infos: &dyn Fn(&str, i32) -> Option<Arc<StreamerInfo>>) {
        for b in &mut self.branches {
            b.resolve(infos);
        }
        let resolved = match self.class.as_str() {
            "TBranch" if self.leaves.len() > 1 => self.interpret_leaf_list(),
            "TBranch" => self
                .interpret_leaves()
                .map(|i| (i, Codec::Plain { header: 0 })),
            "TBranchElement" => self.interpret_element(infos),
            c => Err(format!("branches of class {c} are not supported yet")),
        };
        match resolved {
            Ok((i, codec)) => {
                self.interpretation = Ok(i);
                self.codec = codec;
            }
            Err(why) => self.interpretation = Err(why),
        }
    }

    /// Signed integers read as unsigned when the leaf says so (ROOT stores
    /// `UInt_t` counters as `kCounter`).
    fn signedness(&self, s: Scalar) -> Scalar {
        if !self.leaves.first().is_some_and(|l| l.unsigned) {
            return s;
        }
        match s {
            Scalar::I8 => Scalar::U8,
            Scalar::I16 => Scalar::U16,
            Scalar::I32 => Scalar::U32,
            Scalar::I64 => Scalar::U64,
            s => s,
        }
    }

    fn interpret_element(
        &self,
        infos: &dyn Fn(&str, i32) -> Option<Arc<StreamerInfo>>,
    ) -> core::result::Result<(Interpretation, Codec), String> {
        let Some(e) = &self.element else {
            return Err("missing element information".to_owned());
        };
        let plain = |header| Codec::Plain { header };
        let group = || {
            Err(format!(
                "`{}` groups {} sub-branches; read them by name",
                self.name,
                self.branches.len()
            ))
        };
        // Counters of split TClonesArrays (3) and STL collections (4).
        if matches!(e.kind, 3 | 4) {
            return Ok((Interpretation::Primitive(Scalar::I32), plain(0)));
        }
        if !self.branches.is_empty() {
            return group();
        }
        if e.kind == -1 || e.id < 0 {
            // A whole object per entry (custom streamer, or not split).
            return Ok(whole(&e.class_name, e.kind == -1, e.class_version));
        }
        let info = infos(&e.class_name, e.class_version.abs()).ok_or_else(|| {
            format!(
                "no description of class {} version {}",
                e.class_name, e.class_version
            )
        })?;
        // `fID` counts the members of the description the writer had in
        // memory, where I/O rules may add members the file's description
        // lacks (CMSSW's `reco::ParticleState`): the member of the
        // branch's name, at `fID` if it is there.
        let member = self.name.rsplit('.').next().unwrap_or(&self.name);
        let member = member.split('[').next().unwrap_or(member);
        let at_id = usize::try_from(e.id)
            .ok()
            .and_then(|i| info.elements().get(i));
        let el = at_id
            .filter(|el| el.name() == member)
            .or_else(|| info.elements().iter().find(|el| el.name() == member))
            .or(at_id)
            .ok_or_else(|| format!("class {} has no member {}", e.class_name, e.id))?;
        self.interpret_member(e, el)
    }

    /// A member branch (`fID` ≥ 0): one member per entry (`fType` 0, 1, 2),
    /// or the member of every element of a split collection (31, 41).
    fn interpret_member(
        &self,
        e: &ElementInfo,
        el: &streamer::StreamerElement,
    ) -> core::result::Result<(Interpretation, Codec), String> {
        let plain = |header| Codec::Plain { header };
        let in_collection = matches!(e.kind, 31 | 41);
        let fix = |s: Scalar| self.signedness(s);
        Ok(match (el.kind(), in_collection) {
            (ElementKind::Scalar(s), false) => (Interpretation::Primitive(fix(*s)), plain(0)),
            // TObject::fBits in a collection: a u32, plus a process ID when
            // the object is referenced.
            (ElementKind::Scalar(_), true) if el.type_code() == 15 => (
                Interpretation::Jagged {
                    scalar: Scalar::U32,
                    inner: 1,
                },
                Codec::Bits,
            ),
            (ElementKind::Scalar(s), true) => (
                Interpretation::Jagged {
                    scalar: fix(*s),
                    inner: 1,
                },
                plain(0),
            ),
            (ElementKind::FixedArray { scalar, len }, collection) => {
                let len = usize::try_from(*len).unwrap_or(1);
                let scalar = fix(*scalar);
                if collection {
                    (Interpretation::Jagged { scalar, inner: len }, plain(0))
                } else {
                    (Interpretation::FixedArray { scalar, len }, plain(0))
                }
            }
            (ElementKind::VarArray { scalar, .. }, false) => (
                Interpretation::Jagged {
                    scalar: fix(*scalar),
                    inner: 1,
                },
                plain(1),
            ),
            (ElementKind::TString, false) => (Interpretation::Strings, plain(0)),
            (ElementKind::StdString, false) => (
                Interpretation::Strings,
                plain(if el.type_code() == 300 || e.streamer_type == 300 {
                    6
                } else {
                    0
                }),
            ),
            (ElementKind::Stl { .. }, false) => whole(el.type_name(), false, 0),
            (ElementKind::Object | ElementKind::ObjectAny | ElementKind::Base { .. }, false) => {
                let class = if matches!(el.kind(), ElementKind::Base { .. }) {
                    el.name()
                } else {
                    el.type_name().trim_end_matches('*').trim()
                };
                (
                    Interpretation::Objects {
                        class: class.to_owned(),
                    },
                    Codec::Object(class.to_owned()),
                )
            }
            (ElementKind::ObjectPointer, false) => {
                let class = el.type_name().trim_end_matches('*').trim().to_owned();
                (Interpretation::Objects { class }, Codec::Pointer)
            }
            (ElementKind::CharStar, false) => (Interpretation::Strings, Codec::CharStar),
            (ElementKind::VarArray { scalar, count }, true) => {
                let counts = match self.name.rsplit_once('.') {
                    Some((parent, _)) => format!("{parent}.{count}"),
                    None => count.clone(),
                };
                (
                    Interpretation::Objects {
                        class: format!("{}[{count}] per element", el.type_name()),
                    },
                    Codec::NestedVar {
                        scalar: fix(*scalar),
                        counts,
                    },
                )
            }
            (ElementKind::Stl { .. }, true) => (
                Interpretation::Objects {
                    class: el.type_name().to_owned(),
                },
                Codec::Elements {
                    item: el.type_name().to_owned(),
                },
            ),
            (kind, _) => {
                return Err(format!(
                    "members of kind {kind:?} in branches of type {} are not supported yet",
                    e.kind
                ));
            }
        })
    }

    /// A leaf list (`x/D:y/I`): the leaves back to back in every entry,
    /// read as one object per entry with a member per leaf.
    fn interpret_leaf_list(&self) -> core::result::Result<(Interpretation, Codec), String> {
        let fields = self
            .leaves
            .iter()
            .map(|l| match (l.scalar(), &l.count) {
                (Some(s), None) => Ok((l.name.clone(), s, usize::try_from(l.len).unwrap_or(1))),
                _ => Err(format!(
                    "leaf lists with a {} leaf ({}) are not supported yet",
                    l.class, l.name
                )),
            })
            .collect::<core::result::Result<Vec<_>, _>>()?;
        Ok((
            Interpretation::Objects {
                class: self.title.clone(),
            },
            Codec::LeafList(fields),
        ))
    }

    /// Whether this branch only groups sub-branches (a split object or
    /// member): it has no values of its own; read its sub-branches.
    pub fn is_group(&self) -> bool {
        !self.branches.is_empty()
            && self.baskets.is_empty()
            && self
                .element
                .as_ref()
                .is_some_and(|e| !matches!(e.kind, 3 | 4))
    }

    fn interpret_leaves(&self) -> core::result::Result<Interpretation, String> {
        let [leaf] = self.leaves.as_slice() else {
            return Err(format!(
                "branches with {} leaves (leaf lists) are not supported yet",
                self.leaves.len()
            ));
        };
        let len = usize::try_from(leaf.len).unwrap_or(1);
        if leaf.class == "TLeafC" {
            return Ok(Interpretation::Strings);
        }
        let scalar = leaf
            .scalar()
            .ok_or_else(|| format!("leaves of class {} are not supported yet", leaf.class))?;
        Ok(if leaf.count.is_some() {
            Interpretation::Jagged { scalar, inner: len }
        } else if len > 1 {
            Interpretation::FixedArray { scalar, len }
        } else {
            Interpretation::Primitive(scalar)
        })
    }

    /// The branch's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The branch's title (its leaf list, `x[n]/F`).
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The branch's class (`TBranch`, `TBranchElement`, …).
    pub fn class_name(&self) -> &str {
        &self.class
    }

    /// The leaves.
    pub fn leaves(&self) -> &[Leaf] {
        &self.leaves
    }

    /// The sub-branches.
    pub fn branches(&self) -> &[Branch] {
        &self.branches
    }

    /// The number of entries.
    pub fn num_entries(&self) -> u64 {
        self.entries
    }

    /// The number of baskets on disk.
    pub fn num_baskets(&self) -> usize {
        self.baskets.len()
    }

    /// Bytes of the baskets before compression (`fTotBytes`).
    pub fn uncompressed_bytes(&self) -> u64 {
        self.tot_bytes
    }

    /// Bytes of the baskets on disk (`fZipBytes`).
    pub fn compressed_bytes(&self) -> u64 {
        self.zip_bytes
    }

    /// How the values are laid out.
    ///
    /// # Errors
    ///
    /// `Unsupported` for branches this version cannot read, saying why.
    pub fn interpretation(&self) -> Result<&Interpretation> {
        self.interpretation
            .as_ref()
            .map_err(|why| Error::new(ErrorKind::Unsupported, why.clone()))
    }

    fn readable(&self) -> Vec<&Branch> {
        let mut out: Vec<&Branch> = if self.interpretation.is_ok() {
            vec![self]
        } else {
            Vec::new()
        };
        for b in &self.branches {
            out.extend(b.readable());
        }
        out
    }

    fn with_leaf(&self, leaf: &str) -> Option<&Branch> {
        if self.leaves.iter().any(|l| l.name == leaf) {
            return Some(self);
        }
        self.branches.iter().find_map(|b| b.with_leaf(leaf))
    }

    fn find(&self, name: &str) -> Option<&Branch> {
        if self.name == name {
            return Some(self);
        }
        self.branches.iter().find_map(|b| b.find(name))
    }
}

impl Tree {
    pub(crate) fn from_dyn(file: File, path: String, obj: &DynObject) -> Result<Self> {
        let tree = if obj.class() == "TTree" {
            obj
        } else {
            obj.base("TTree").ok_or_else(|| {
                Error::new(
                    ErrorKind::ClassMismatch,
                    format!("`{path}` is a {}, not a TTree", obj.class()),
                )
            })?
        };
        let entries = count(tree, "fEntries")?;
        let mut branches = list(tree, "fBranches")
            .iter()
            .filter_map(Value::as_object)
            .map(Branch::from_dyn)
            .collect::<Result<Vec<_>>>()
            .context(Context::Key(path.clone()))?;
        let set = file.streamer_set()?;
        let infos = |class: &str, version: i32| {
            set.get(class, version)
                .or_else(|| streamer::builtin().get(class, version))
                .or_else(|| set.latest(class))
                .or_else(|| streamer::builtin().latest(class))
                .cloned()
        };
        for b in &mut branches {
            b.resolve(&infos);
        }
        let clusters = clusters(tree, entries, &branches)?;
        Ok(Self {
            file,
            inner: Arc::new(Inner {
                name: tree.str("fName")?.to_owned(),
                title: tree.str("fTitle")?.to_owned(),
                path,
                entries,
                branches,
                clusters,
                stats: Stats::default(),
            }),
        })
    }

    /// The same tree, reading through `source` (see `File::with_source`).
    pub(crate) fn with_source(&self, source: Arc<dyn ReadAt>) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            file: self.file.with_source(source),
        }
    }

    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    /// The byte ranges reading `branch` over `runs` fetches: its baskets
    /// that some run touches, and those of the branches its decoding reads
    /// (element counts, the counter of a jagged leaf).
    pub(crate) fn plan(&self, branch: &Branch, runs: &[Range<u64>]) -> Vec<Range<FileOffset>> {
        let mut out = Vec::new();
        let mut add = |b: &Branch| {
            for bk in &b.baskets {
                if let Loc::Disk { seek, bytes } = bk.loc
                    && runs
                        .iter()
                        .any(|r| r.start < bk.entries.end && r.end > bk.entries.start)
                    && let Some(end) = seek.checked_add(u64::from(bytes))
                {
                    out.push(seek..end);
                }
            }
        };
        add(branch);
        if let Codec::NestedVar { counts, .. } = &branch.codec
            && let Some(c) = self.get_branch(counts)
        {
            add(c);
        }
        if matches!(branch.codec, Codec::Plain { header: 0 })
            && let Some(count) = branch.leaves.first().and_then(|l| l.count.as_deref())
            && let Some(c) = self.inner.branches.iter().find_map(|b| b.with_leaf(count))
        {
            add(c);
        }
        out
    }

    /// The tree's name.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// The tree's title.
    pub fn title(&self) -> &str {
        &self.inner.title
    }

    /// The number of entries.
    pub fn num_entries(&self) -> u64 {
        self.inner.entries
    }

    /// The top-level branches.
    pub fn branches(&self) -> &[Branch] {
        &self.inner.branches
    }

    /// Every branch that can be read, at every level, in tree order.
    pub fn readable_branches(&self) -> Vec<&Branch> {
        self.inner
            .branches
            .iter()
            .flat_map(Branch::readable)
            .collect()
    }

    /// A branch by name, searched at every level.
    pub fn get_branch(&self, name: &str) -> Option<&Branch> {
        self.inner.branches.iter().find_map(|b| b.find(name))
    }

    /// The entry ranges of the clusters: reading whole clusters reads whole
    /// baskets.
    pub fn clusters(&self) -> &[Range<u64>] {
        &self.inner.clusters
    }

    /// What reading through this handle has cost so far.
    pub fn read_stats(&self) -> ReadStats {
        let s = &self.inner.stats;
        ReadStats {
            requests: s.requests.load(Ordering::Relaxed),
            baskets: s.baskets.load(Ordering::Relaxed),
            bytes_read: s.bytes_read.load(Ordering::Relaxed),
            bytes_decompressed: s.bytes_decompressed.load(Ordering::Relaxed),
        }
    }

    fn branch_or_err(&self, name: &str) -> Result<&Branch> {
        self.get_branch(name).ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                format!("tree `{}` has no branch `{name}`", self.inner.path),
            )
        })
    }

    /// Opens a branch for typed reading. The type is checked here, once.
    ///
    /// # Errors
    ///
    /// `NotFound`; `Unsupported` for branches this version cannot read;
    /// `TypeMismatch` if `T` does not match the branch.
    pub fn branch<T: ColumnType>(&self, name: &str) -> Result<BranchReader<'_, T>> {
        let branch = self.branch_or_err(name)?;
        let i = branch.interpretation()?;
        if !T::accepts(i) {
            return Err(Error::new(
                ErrorKind::TypeMismatch,
                format!("branch `{name}` holds {i}, not {}", T::describe()),
            ));
        }
        Ok(BranchReader {
            tree: self,
            branch,
            _type: PhantomData,
        })
    }

    /// Reads a branch over a selection of entries (`..`, `0..1000`, an
    /// [`EntrySelection`]), without naming its type.
    ///
    /// # Errors
    ///
    /// As [`Tree::branch`], plus `InvalidInput` for a selection outside the
    /// branch, and read, decompression and format errors.
    pub fn read(&self, name: &str, selection: impl Into<EntrySelection>) -> Result<Column> {
        let branch = self.branch_or_err(name)?;
        self.read_selection(branch, &selection.into())
    }

    fn read_selection(&self, branch: &Branch, selection: &EntrySelection) -> Result<Column> {
        let runs = selection.resolve(branch.entries).map_err(|e| {
            e.with_context(Context::Key(format!("{}:{}", self.inner.path, branch.name)))
        })?;
        self.read_runs(branch, &runs)
    }

    /// Reads the entries of `runs` (sorted, disjoint, within the branch):
    /// only the baskets some run touches are fetched.
    pub(crate) fn read_runs(&self, branch: &Branch, runs: &[Range<u64>]) -> Result<Column> {
        let total = branch.entries;
        let interp = branch.interpretation()?.clone();
        let read = || -> Result<Column> {
            let end = runs.last().map_or(0, |r| r.end);
            let covered = branch.baskets.last().map_or(0, |b| b.entries.end);
            if end > covered {
                return Err(Error::malformed(format!(
                    "entries {covered}..{total} are in no basket"
                )));
            }
            let touches = |b: &BasketRef| {
                runs.iter()
                    .any(|r| r.start < b.entries.end && r.end > b.entries.start)
            };
            let baskets: Vec<&BasketRef> = branch.baskets.iter().filter(|b| touches(b)).collect();
            let loaded = self.load(&baskets)?;
            let mut cx = DecodeCx {
                set: if matches!(
                    branch.codec,
                    Codec::Plain { .. } | Codec::CharStar | Codec::Bits
                ) {
                    Arc::new(StreamerSet::default())
                } else {
                    self.file.streamer_set()?
                },
                on_mismatch: self.file.on_mismatch(),
                diagnostics: Vec::new(),
                counts: std::collections::VecDeque::new(),
            };
            if let Codec::NestedVar { counts, .. } = &branch.codec {
                cx.counts = self.element_counts(counts, runs)?;
            }
            let mut out = Builder::new(&interp, &branch.codec, cx);
            for (b, (mut basket, at)) in baskets.iter().zip(loaded) {
                if basket.offsets.is_none() {
                    self.offsets_from_counter(branch, &interp, &b.entries, &mut basket)?;
                }
                for r in runs
                    .iter()
                    .filter(|r| r.start < b.entries.end && r.end > b.entries.start)
                {
                    let lo = usize::try_from(r.start.max(b.entries.start) - b.entries.start)
                        .unwrap_or(0);
                    let hi =
                        usize::try_from(r.end.min(b.entries.end) - b.entries.start).unwrap_or(0);
                    let pushed = out.push(&basket, lo, hi);
                    match at {
                        Some(seek) => pushed.context(Context::Offset(seek))?,
                        None => pushed?,
                    }
                }
            }
            let (column, diagnostics) = out.finish();
            self.file.add_diagnostics(diagnostics);
            Ok(column)
        };
        read().map_err(|e| {
            e.with_context(Context::Key(format!("{}:{}", self.inner.path, branch.name)))
        })
    }

    /// The per-element counts of a split collection, entry by entry, from
    /// the branch `name` (a jagged column of integers).
    fn element_counts(
        &self,
        name: &str,
        runs: &[Range<u64>],
    ) -> Result<std::collections::VecDeque<Vec<usize>>> {
        let branch = self
            .get_branch(name)
            .ok_or_else(|| Error::malformed(format!("no count branch `{name}`")))?;
        let Column::Jagged { offsets, values } = self.read_runs(branch, runs)? else {
            return Err(Error::malformed(format!(
                "count branch `{name}` is not jagged"
            )));
        };
        offsets
            .windows(2)
            .map(|w| {
                (w[0]..w[1])
                    .map(|i| {
                        values
                            .get(i)
                            .and_then(|v| v.as_i64())
                            .and_then(|n| usize::try_from(n).ok())
                            .ok_or_else(|| Error::malformed(format!("bad count in `{name}`")))
                    })
                    .collect()
            })
            .collect()
    }
}

impl Tree {
    /// Fetches the baskets (one vector read for those on disk) and decodes
    /// them; each comes with its position, when it has one.
    fn load(&self, baskets: &[&BasketRef]) -> Result<Vec<(basket::Basket, Option<FileOffset>)>> {
        let ranges: Vec<Range<FileOffset>> = baskets
            .iter()
            .filter_map(|b| match b.loc {
                Loc::Disk { seek, bytes } => Some(
                    seek.checked_add(u64::from(bytes))
                        .map(|e| seek..e)
                        .ok_or_else(|| Error::malformed("basket position overflows")),
                ),
                Loc::Embedded(_) => None,
            })
            .collect::<Result<_>>()?;
        let mut records = if ranges.is_empty() {
            Vec::new()
        } else {
            self.file.source().read_ranges(&ranges)?
        }
        .into_iter();
        let stats = &self.inner.stats;
        stats.requests.fetch_add(1, Ordering::Relaxed);
        let mut out = Vec::with_capacity(baskets.len());
        for b in baskets {
            let (basket, at) = match &b.loc {
                Loc::Disk { seek, .. } => {
                    let record = records
                        .next()
                        .ok_or_else(|| Error::malformed("the source returned too few ranges"))?;
                    stats.baskets.fetch_add(1, Ordering::Relaxed);
                    stats.bytes_read.fetch_add(
                        u64::try_from(record.len()).unwrap_or(u64::MAX),
                        Ordering::Relaxed,
                    );
                    let basket = basket::decode(&record, self.file.max_object_bytes())
                        .context(Context::Offset(*seek))?;
                    stats.bytes_decompressed.fetch_add(
                        u64::try_from(basket.uncompressed).unwrap_or(u64::MAX),
                        Ordering::Relaxed,
                    );
                    (basket, Some(*seek))
                }
                Loc::Embedded(basket) => (basket::Basket::clone(basket), None),
            };
            let n = usize::try_from(b.entries.end - b.entries.start).unwrap_or(usize::MAX);
            if basket.entries != n {
                return Err(Error::malformed(format!(
                    "a basket holds {} entries, the branch says {n}",
                    basket.entries
                )));
            }
            out.push((basket, at));
        }
        Ok(out)
    }

    /// Some writers store jagged leaves without entry offsets; the counter
    /// branch then gives each entry's length (as uproot does).
    fn offsets_from_counter(
        &self,
        branch: &Branch,
        interp: &Interpretation,
        entries: &Range<u64>,
        basket: &mut basket::Basket,
    ) -> Result<()> {
        let (Interpretation::Jagged { scalar, inner }, Codec::Plain { header: 0 }) =
            (interp, &branch.codec)
        else {
            return Ok(());
        };
        let Some(count) = branch.leaves.first().and_then(|l| l.count.as_deref()) else {
            return Ok(());
        };
        let counter = self
            .inner
            .branches
            .iter()
            .find_map(|b| b.with_leaf(count))
            .ok_or_else(|| Error::malformed(format!("no branch holds the counter leaf {count}")))?;
        let Column::Values(counts) = self.read_runs(counter, core::slice::from_ref(entries))?
        else {
            return Err(Error::malformed(format!(
                "counter {count} is not a column of integers"
            )));
        };
        let element = scalar.size() * (*inner).max(1);
        let limit = basket.data.len();
        let mut offsets = Vec::with_capacity(counts.len() + 1);
        let mut at = 0_usize;
        offsets.push(at);
        let mut ok = true;
        macro_rules! walk {
            ($values:expr) => {
                for &n in $values {
                    let next = usize::try_from(n)
                        .ok()
                        .and_then(|n| n.checked_mul(element))
                        .and_then(|b| at.checked_add(b))
                        .filter(|&a| a <= limit);
                    match next {
                        Some(a) => {
                            at = a;
                            offsets.push(at);
                        }
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
            };
        }
        match &counts {
            Array::I8(v) => walk!(v),
            Array::U8(v) => walk!(v),
            Array::I16(v) => walk!(v),
            Array::U16(v) => walk!(v),
            Array::I32(v) => walk!(v),
            Array::U32(v) => walk!(v),
            Array::I64(v) => walk!(v),
            Array::U64(v) => walk!(v),
            _ => ok = false,
        }
        if !ok {
            return Err(Error::malformed(format!(
                "counts in {count} are negative or exceed the basket"
            )));
        }
        basket.offsets = Some(offsets);
        Ok(())
    }
}

/// Reads one branch as `T`; made by [`Tree::branch`].
pub struct BranchReader<'t, T> {
    tree: &'t Tree,
    branch: &'t Branch,
    _type: PhantomData<fn() -> T>,
}

impl<T> fmt::Debug for BranchReader<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BranchReader")
            .field("branch", &self.branch.name)
            .finish_non_exhaustive()
    }
}

impl<T: ColumnType> BranchReader<'_, T> {
    /// Reads a selection of entries: `..` for all of them, a range, or an
    /// [`EntrySelection`].
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a selection outside the branch; read,
    /// decompression and format errors.
    pub fn read(&self, selection: impl Into<EntrySelection>) -> Result<T::Output> {
        T::from_column(self.tree.read_selection(self.branch, &selection.into())?)
    }

    /// The branch read.
    pub fn branch(&self) -> &Branch {
        self.branch
    }
}

/// What a branch read needs to decode objects.
struct DecodeCx {
    set: Arc<StreamerSet>,
    on_mismatch: OnMismatch,
    diagnostics: Vec<crate::Diagnostic>,
    /// Per-element counts for `Codec::NestedVar`, one entry at a time.
    counts: std::collections::VecDeque<Vec<usize>>,
}

/// A whole object or collection per entry, by type name.
fn whole(type_name: &str, custom: bool, version: i32) -> (Interpretation, Codec) {
    let t = type_name.trim().trim_start_matches("std::");
    if matches!(t, "string" | "TString") {
        // `std::string` written by the collection machinery has a header.
        let header = if t == "string" && !custom { 6 } else { 0 };
        return (Interpretation::Strings, Codec::Plain { header });
    }
    if let Some(("vector", item)) = streamer::split_template(t)
        && let Some(scalar) = Scalar::from_type_name(item)
    {
        return (
            Interpretation::Jagged { scalar, inner: 1 },
            Codec::Plain { header: 10 },
        );
    }
    let class = t.to_owned();
    if streamer::split_template(t).is_some() {
        (
            Interpretation::Objects {
                class: class.clone(),
            },
            Codec::Typed(class),
        )
    } else if custom {
        (
            Interpretation::Objects {
                class: class.clone(),
            },
            Codec::Object(class),
        )
    } else {
        let version = i16::try_from(version.unsigned_abs()).unwrap_or(0);
        (
            Interpretation::Objects {
                class: class.clone(),
            },
            Codec::Members { class, version },
        )
    }
}

/// Accumulates the entries of several baskets into one column.
struct Builder {
    kind: Kind,
    header: usize,
    /// Values are `TObject` bits with optional process IDs (`Codec::Bits`).
    bits: bool,
    cx: DecodeCx,
}

enum Kind {
    Values {
        scalar: Scalar,
        len: usize,
        values: Option<Array>,
    },
    Jagged {
        scalar: Scalar,
        inner: usize,
        offsets: Vec<usize>,
        values: Option<Array>,
    },
    Strings {
        out: Vec<String>,
        /// `char*`: an `i32` length instead of ROOT's 1-byte one.
        long: bool,
    },
    Objects {
        codec: Codec,
        out: Vec<Value>,
    },
}

impl Builder {
    fn new(i: &Interpretation, codec: &Codec, cx: DecodeCx) -> Self {
        let header = match codec {
            Codec::Plain { header } => *header,
            _ => 0,
        };
        let kind = match (i, codec) {
            (
                _,
                Codec::Object(_)
                | Codec::Typed(_)
                | Codec::Pointer
                | Codec::Members { .. }
                | Codec::LeafList(_)
                | Codec::NestedVar { .. }
                | Codec::Elements { .. },
            )
            | (Interpretation::Objects { .. }, _) => Kind::Objects {
                codec: codec.clone(),
                out: Vec::new(),
            },
            (Interpretation::Primitive(scalar), _) => Kind::Values {
                scalar: *scalar,
                len: 1,
                values: None,
            },
            (Interpretation::FixedArray { scalar, len }, _) => Kind::Values {
                scalar: *scalar,
                len: *len,
                values: None,
            },
            (Interpretation::Jagged { scalar, inner }, _) => Kind::Jagged {
                scalar: *scalar,
                inner: (*inner).max(1),
                offsets: vec![0],
                values: None,
            },
            (Interpretation::Strings, codec) => Kind::Strings {
                out: Vec::new(),
                long: matches!(codec, Codec::CharStar),
            },
        };
        Self {
            kind,
            header,
            bits: matches!(codec, Codec::Bits),
            cx,
        }
    }

    fn append(slot: &mut Option<Array>, a: Array) -> Result<()> {
        if let Some(dst) = slot {
            return extend_array(dst, a);
        }
        *slot = Some(a);
        Ok(())
    }

    fn push(&mut self, basket: &basket::Basket, lo: usize, hi: usize) -> Result<()> {
        let header = self.header;
        match &mut self.kind {
            Kind::Jagged {
                offsets, values, ..
            } if self.bits => push_bits(basket, lo..hi, offsets, values),
            Kind::Values {
                scalar,
                len,
                values,
            } => push_values(basket, lo..hi, *scalar, *len, values),
            Kind::Jagged {
                scalar,
                inner,
                offsets,
                values,
            } => push_jagged(basket, lo..hi, header, *scalar, *inner, offsets, values),
            Kind::Strings { out, long } => {
                for e in lo..hi {
                    let mut r = RBuffer::new(entry_body(basket, e, header)?, 0);
                    out.push(if *long {
                        let n = usize::try_from(r.read_len32("char* length")?)
                            .map_err(|_| Error::malformed("string too long"))?;
                        String::from_utf8_lossy(r.take(n)?).into_owned()
                    } else {
                        r.read_string()?
                    });
                }
                Ok(())
            }
            Kind::Objects { codec, out } => {
                for e in lo..hi {
                    let v = decode_entry(basket, e, codec, &mut self.cx)?;
                    out.push(v);
                }
                Ok(())
            }
        }
    }

    fn finish(self) -> (Column, Vec<crate::Diagnostic>) {
        let empty = streamer::empty_array;
        let column = match self.kind {
            Kind::Values {
                scalar,
                len: 1,
                values,
            } => Column::Values(values.unwrap_or_else(|| empty(scalar))),
            Kind::Values {
                scalar,
                len,
                values,
            } => Column::Fixed {
                len,
                values: values.unwrap_or_else(|| empty(scalar)),
            },
            Kind::Jagged {
                scalar,
                offsets,
                values,
                ..
            } => Column::Jagged {
                offsets,
                values: values.unwrap_or_else(|| empty(scalar)),
            },
            Kind::Strings { out, .. } => Column::Strings(out),
            Kind::Objects { out, .. } => Column::Objects(out),
        };
        (column, self.cx.diagnostics)
    }
}

/// Entry `e` of a basket without its per-entry header.
fn entry_body(basket: &basket::Basket, e: usize, header: usize) -> Result<&[u8]> {
    let bytes = basket.entry(e)?;
    bytes.get(header..).ok_or_else(|| {
        Error::malformed(format!(
            "entry of {} bytes is shorter than its header",
            bytes.len()
        ))
    })
}

fn push_values(
    basket: &basket::Basket,
    entries: Range<usize>,
    scalar: Scalar,
    len: usize,
    values: &mut Option<Array>,
) -> Result<()> {
    let n = entries.len() * len;
    let bytes = if basket.offsets.is_some() {
        basket.entries(entries.start, entries.end)?
    } else {
        let size = scalar.size() * len;
        basket
            .data
            .get(entries.start * size..entries.end * size)
            .ok_or_else(|| Error::malformed("basket too short for its entries"))?
    };
    if bytes.len() != n * scalar.size() {
        return Err(Error::malformed(format!(
            "{} bytes for {n} values of {} bytes",
            bytes.len(),
            scalar.size()
        )));
    }
    let a = streamer::read_array(&mut RBuffer::new(bytes, 0), scalar, n)?;
    Builder::append(values, a)
}

fn push_jagged(
    basket: &basket::Basket,
    entries: Range<usize>,
    header: usize,
    scalar: Scalar,
    inner: usize,
    offsets: &mut Vec<usize>,
    values: &mut Option<Array>,
) -> Result<()> {
    let size = scalar.size();
    let element = size * inner;
    let mut total = offsets.last().copied().unwrap_or(0);
    // No per-entry headers: the entries are contiguous, their lengths the
    // differences of the basket's offsets.
    if header == 0
        && let Some(o) = &basket.offsets
    {
        let starts = o
            .get(entries.start..=entries.end)
            .ok_or_else(|| Error::malformed("entry outside its basket"))?;
        // The basket's offsets never go backwards (checked when it was
        // decoded); each entry is a whole number of elements when every
        // offset is, counted from the first. Scalars' sizes are powers of
        // two: shifts and masks, no division per entry.
        let first = starts.first().copied().unwrap_or(0);
        let shift = size.trailing_zeros();
        let mask = element.is_power_of_two().then(|| element - 1);
        let mut whole = true;
        offsets.reserve(entries.len());
        offsets.extend(starts.iter().skip(1).map(|&s| {
            let bytes = s.wrapping_sub(first);
            whole &= s >= first
                && match mask {
                    Some(m) => bytes & m == 0,
                    None => bytes % element == 0,
                };
            total + (bytes >> shift)
        }));
        if !whole {
            return Err(Error::malformed(format!(
                "an entry is not a whole number of {element}-byte elements"
            )));
        }
        let body = basket.entries(entries.start, entries.end)?;
        let a = streamer::read_array(&mut RBuffer::new(body, 0), scalar, body.len() / size)?;
        return Builder::append(values, a);
    }
    let mut bodies = Vec::with_capacity(entries.len());
    for e in entries.clone() {
        let body = entry_body(basket, e, header)?;
        if body.len() % element != 0 {
            return Err(Error::malformed(format!(
                "entry of {} bytes is not a whole number of {element}-byte elements",
                body.len()
            )));
        }
        total += body.len() / size;
        offsets.push(total);
        bodies.push(body);
    }
    if header == 0 {
        // No per-entry headers: the entries are contiguous.
        bodies = vec![basket.entries(entries.start, entries.end)?];
    }
    for body in bodies {
        let a = streamer::read_array(&mut RBuffer::new(body, 0), scalar, body.len() / size)?;
        Builder::append(values, a)?;
    }
    if values.is_none() {
        *values = Some(streamer::empty_array(scalar));
    }
    Ok(())
}

/// `TObject::fBits` of each element: `u32`, then a `u16` process ID when
/// bit `0x10` (`kIsReferenced`) is set.
fn push_bits(
    basket: &basket::Basket,
    entries: Range<usize>,
    offsets: &mut Vec<usize>,
    values: &mut Option<Array>,
) -> Result<()> {
    let mut out = Vec::new();
    let mut total = offsets.last().copied().unwrap_or(0);
    for e in entries {
        let mut r = RBuffer::new(basket.entry(e)?, 0);
        while r.remaining() > 0 {
            let bits = r.read_u32()?;
            if bits & 0x10 != 0 {
                r.skip(2)?;
            }
            out.push(bits);
            total += 1;
        }
        offsets.push(total);
    }
    Builder::append(values, Array::U32(out))
}

/// Decodes entry `e` as one value, with a fresh class-tag map: class tags
/// count from the start of the basket buffer, and ROOT resets the map for
/// every entry.
fn decode_entry(
    basket: &basket::Basket,
    e: usize,
    codec: &Codec,
    cx: &mut DecodeCx,
) -> Result<Value> {
    let bytes = basket.entry(e)?;
    let origin = u32::try_from(basket.key_len + basket.entry_start(e)?)
        .map_err(|_| Error::malformed("basket too large"))?;
    let mut reader = RBuffer::new(bytes, origin);
    let mut decoder = Decoder::new(&cx.set, streamer::builtin(), cx.on_mismatch);
    // Some writers store nothing for a member (an empty entry).
    if bytes.is_empty() && matches!(codec, Codec::Typed(_) | Codec::Object(_) | Codec::Pointer) {
        return Ok(Value::Null);
    }
    let value = match codec {
        Codec::Object(class) => Value::Object(Box::new(decoder.object(&mut reader, class)?)),
        Codec::Typed(t) => decoder.value_of_type(&mut reader, t, true)?,
        Codec::Pointer => decoder.object_any(&mut reader)?,
        Codec::Members { class, version } => Value::Object(Box::new(decoder.object_members(
            &mut reader,
            class,
            *version,
        )?)),
        Codec::LeafList(fields) => {
            let mut obj = DynObject::new("leaf list");
            for (name, scalar, n) in fields {
                let a = streamer::read_array(&mut reader, *scalar, *n)?;
                let v = if *n == 1 {
                    a.get(0).unwrap_or(Value::Null)
                } else {
                    Value::Array(a)
                };
                obj.set(name.as_str(), v);
            }
            Value::Object(Box::new(obj))
        }
        Codec::NestedVar { scalar, .. } => {
            let counts = cx
                .counts
                .pop_front()
                .ok_or_else(|| Error::malformed("fewer count entries than entries"))?;
            let mut arrays = Vec::with_capacity(counts.len());
            for n in counts {
                let n = if reader.read_u8()? == 0 { 0 } else { n };
                arrays.push(Value::Array(streamer::read_array(&mut reader, *scalar, n)?));
            }
            if reader.remaining() != 0 {
                return Err(Error::malformed(format!(
                    "{} bytes left after the elements of an entry",
                    reader.remaining()
                )));
            }
            Value::List(arrays)
        }
        Codec::Elements { item } => {
            let mut items = Vec::new();
            if reader.remaining() > 0 {
                reader.skip(6)?; // byte count and version
                while reader.remaining() > 0 {
                    items.push(decoder.value_of_type(&mut reader, item, false)?);
                }
            }
            Value::List(items)
        }
        Codec::Plain { .. } | Codec::CharStar | Codec::Bits => {
            return Err(Error::malformed("plain entries as objects"));
        }
    };
    cx.diagnostics.extend(decoder.into_diagnostics());
    Ok(value)
}

/// The clusters, as ROOT's `TClusterIterator` walks them: the explicit
/// ranges (`fClusterRangeEnd` is inclusive), then steps of `fAutoFlush`
/// entries; when `fAutoFlush` counts bytes, the basket boundaries shared by
/// every branch.
fn clusters(tree: &DynObject, entries: u64, branches: &[Branch]) -> Result<Vec<Range<u64>>> {
    let ends = i64_array(tree, "fClusterRangeEnd")?;
    let sizes = i64_array(tree, "fClusterSize")?;
    let auto_flush = tree.get("fAutoFlush").and_then(Value::as_i64).unwrap_or(0);
    let mut out = Vec::new();
    let mut start = 0_u64;
    let step = |size: u64, start: &mut u64, end: u64, out: &mut Vec<Range<u64>>| {
        let size = size.max(1);
        while *start < end {
            let e = start.saturating_add(size).min(end);
            out.push(*start..e);
            *start = e;
        }
    };
    for (i, &e) in ends.iter().enumerate() {
        let end = u64::try_from(e)
            .ok()
            .map_or(entries, |e| e.saturating_add(1))
            .min(entries);
        let size = sizes
            .get(i)
            .and_then(|&s| u64::try_from(s).ok())
            .filter(|&s| s > 0);
        step(
            size.unwrap_or(end.saturating_sub(start)),
            &mut start,
            end,
            &mut out,
        );
    }
    if start < entries {
        if let Some(size) = u64::try_from(auto_flush).ok().filter(|&s| s > 0) {
            step(size, &mut start, entries, &mut out);
        } else {
            let mut cuts: Vec<u64> = Vec::new();
            let mut first = true;
            for b in branches.iter().filter(|b| !b.baskets.is_empty()) {
                let these: Vec<u64> = b.baskets.iter().map(|k| k.entries.end).collect();
                if first {
                    cuts = these;
                    first = false;
                } else {
                    cuts.retain(|c| these.contains(c));
                }
            }
            cuts.retain(|&c| c > start && c < entries);
            cuts.push(entries);
            for c in cuts {
                out.push(start..c);
                start = c;
            }
        }
    }
    Ok(out)
}

/// A basket, for the fuzz targets.
#[cfg(feature = "_fuzz")]
pub(crate) fn fuzz_basket(data: &[u8]) {
    let _ = basket::decode(data, 16 << 20);
    let _ = basket::decode_embedded(data);
}
