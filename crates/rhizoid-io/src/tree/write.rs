//! Writing flat `TTree`s [§4.8, Writer]: [`TreeWriter`].
//!
//! A tree is written as ROOT writes one made with `TTree::Branch` and a
//! leaf list: one `TBranch` per column with one `TLeaf{O,B,S,I,L,F,D,C}`,
//! fixed arrays (`xyz[3]/F`), jagged arrays behind a counter branch
//! (`n_xs/I`, `xs[n_xs]/F`), and strings (`label/C`). Rows are buffered and
//! written a cluster at a time, one basket per branch per cluster; the tree
//! object (`TTree` v20) is written when the writer finishes.

use core::fmt;
use core::marker::PhantomData;
use std::sync::{Mutex, PoisonError};

use super::split::Node;
use crate::columnar::NativeType;
use crate::core::error::{Error, ErrorKind, Result};
use crate::core::key::KeyHeader;
use crate::streamer::{Array, DynObject, Scalar, Value};
use crate::user::ClassSpec;
use crate::user::Row;
use crate::write::WritableFile;

/// `TBranch::fBasketSize` and `TBasket::fBufferSize`, as ROOT and uproot
/// declare them (readers size their buffers from the key, not from this).
pub(crate) const BASKET_SIZE: i32 = 32_000;
/// `TBranch::fEntryOffsetLen` of branches with entry offsets, as ROOT 6.42
/// writes it for leaf-list branches. Not zero, or ROOT reads no offsets.
const ENTRY_OFFSET_LEN: i32 = 80;
/// `TBasket::fNevBufSize` of baskets with entry offsets, as ROOT 6.42
/// writes it (the capacity of its offset array).
pub(crate) const OFFSETS_CAPACITY: i32 = 1000;
/// `TBranch::fMaxBaskets` at least; ROOT starts at 10.
const MIN_MAX_BASKETS: usize = 10;
/// `TTree::fAutoFlush` when no cluster size was set or reached: ROOT's
/// default (flush every 30 MB of compressed data).
const DEFAULT_AUTO_FLUSH: i64 = -30_000_000;
/// `TObject::fBits` of the tree, its branches and its branch list, as
/// ROOT 6.42 writes them.
const TREE_BITS: u32 = 0x8;
const BRANCH_BITS: u32 = 0x0040_0000;
const OWNER_LIST_BITS: u32 = 0x4000;

#[cold]
fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// How a row field is laid out in a tree. Not part of the API.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeafKind {
    /// One value per entry.
    One,
    /// `N` values per entry (`x[N]/F`).
    Fixed(usize),
    /// Any number of values, counted by a counter branch (`x[n]/F`).
    Jagged,
    /// A string (`x/C`).
    String,
    /// An object of a user class: a split branch.
    Object,
    /// A `std::vector` of objects of a user class: a split collection.
    Objects,
}

/// A row field's type and layout in a tree. Not part of the API.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LeafShape {
    pub(crate) scalar: Scalar,
    pub(crate) kind: LeafKind,
}

impl LeafShape {
    /// A shape.
    #[must_use]
    pub const fn new(scalar: Scalar, kind: LeafKind) -> Self {
        Self { scalar, kind }
    }

    /// The leaf class, type letter, unsignedness and value size.
    fn leaf(self) -> (&'static str, char, bool, usize) {
        if self.kind == LeafKind::String {
            return ("TLeafC", 'C', false, 1);
        }
        match self.scalar {
            Scalar::Bool => ("TLeafO", 'O', false, 1),
            Scalar::I8 => ("TLeafB", 'B', false, 1),
            Scalar::U8 => ("TLeafB", 'b', true, 1),
            Scalar::I16 => ("TLeafS", 'S', false, 2),
            Scalar::U16 => ("TLeafS", 's', true, 2),
            Scalar::I32 => ("TLeafI", 'I', false, 4),
            Scalar::U32 => ("TLeafI", 'i', true, 4),
            Scalar::I64 => ("TLeafL", 'L', false, 8),
            Scalar::U64 => ("TLeafL", 'l', true, 8),
            Scalar::F32 => ("TLeafF", 'F', false, 4),
            _ => ("TLeafD", 'D', false, 8),
        }
    }
}

/// One field's buffered entries. Not part of the API.
#[doc(hidden)]
#[derive(Debug, Default)]
pub struct LeafBuffer {
    /// The values, as stored in a basket (big-endian).
    pub(crate) data: Vec<u8>,
    /// Where each entry starts in `data`, for jagged arrays and strings.
    pub(crate) starts: Vec<usize>,
    /// The length of each jagged array or string.
    pub(crate) counts: Vec<usize>,
    /// Objects, for fields of user classes.
    pub(crate) objects: Vec<Value>,
}

impl LeafBuffer {
    /// Appends an object (or a list of objects).
    pub fn put_object(&mut self, v: Value) {
        self.objects.push(v);
    }

    /// Appends one value.
    pub fn put<T: NativeType>(&mut self, v: T) {
        v.put_be(&mut self.data);
    }

    /// Appends a fixed-size array.
    pub fn put_fixed<T: NativeType>(&mut self, v: &[T]) {
        for &x in v {
            x.put_be(&mut self.data);
        }
    }

    /// Appends a jagged entry.
    pub fn put_jagged<T: NativeType>(&mut self, v: &[T]) {
        self.starts.push(self.data.len());
        self.counts.push(v.len());
        self.put_fixed(v);
    }

    /// Appends a string, as `TLeafC` stores it: a length byte (255 and a
    /// 4-byte length for long strings), then the bytes.
    pub fn put_string(&mut self, s: &str) {
        self.starts.push(self.data.len());
        self.counts.push(s.len());
        match u8::try_from(s.len()) {
            Ok(n) if n < 255 => self.data.push(n),
            _ => {
                self.data.push(255);
                // Lengths beyond i32 are refused when the basket is written.
                let n = u32::try_from(s.len()).unwrap_or(u32::MAX);
                self.data.extend_from_slice(&n.to_be_bytes());
            }
        }
        self.data.extend_from_slice(s.as_bytes());
    }

    pub(crate) fn mark(&self) -> (usize, usize, usize) {
        (self.data.len(), self.starts.len(), self.counts.len())
    }

    pub(crate) fn truncate(&mut self, (data, starts, counts): (usize, usize, usize)) {
        self.data.truncate(data);
        self.starts.truncate(starts);
        self.counts.truncate(counts);
    }

    pub(crate) fn clear(&mut self) {
        self.data.clear();
        self.starts.clear();
        self.counts.clear();
    }

    pub(crate) fn bytes(&self) -> usize {
        self.data.len() + 4 * self.starts.len()
    }
}

/// Options for a tree.
#[derive(Clone, Debug)]
pub struct TreeOptions {
    title: String,
    cluster_entries: Option<u64>,
    cluster_bytes: usize,
}

impl Default for TreeOptions {
    fn default() -> Self {
        Self {
            title: String::new(),
            cluster_entries: None,
            cluster_bytes: 30_000_000,
        }
    }
}

impl TreeOptions {
    /// Sets the tree's title. The default is empty.
    #[must_use]
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Writes a cluster (one basket per branch) every `n` entries
    /// (`TTree::fAutoFlush`). By default the first cluster ends once the
    /// buffered rows reach [`cluster_bytes`](Self::cluster_bytes), and every
    /// later one holds as many entries, as ROOT does.
    #[must_use]
    pub fn cluster_entries(mut self, n: u64) -> Self {
        self.cluster_entries = Some(n.max(1));
        self
    }

    /// The uncompressed size of the rows buffered before the first cluster
    /// is written. The default is 30 MB. Memory use is about this much per
    /// tree being written.
    #[must_use]
    pub fn cluster_bytes(mut self, bytes: usize) -> Self {
        self.cluster_bytes = bytes.max(1);
        self
    }
}

/// Where a branch's values come from.
#[derive(Clone, Copy, Debug)]
enum Source {
    /// A row field.
    Field(usize),
    /// The lengths of a field's jagged arrays.
    Counts(usize),
}

/// A basket written.
#[derive(Clone, Copy, Debug)]
struct Basket {
    nbytes: i32,
    entry: i64,
    seek: i64,
}

#[derive(Debug)]
struct BranchState {
    name: String,
    shape: LeafShape,
    source: Source,
    /// The counter branch of a jagged one.
    counter: Option<usize>,
    baskets: Vec<Basket>,
    tot_bytes: i64,
    zip_bytes: i64,
    /// The longest string, or the largest count of a counter.
    longest: usize,
    /// A field of a user class: its split branch.
    split: Option<Box<Node>>,
}

/// A tree being written, independent of its row type.
#[derive(Debug)]
pub(crate) struct TreeState {
    name: String,
    options: TreeOptions,
    bufs: Vec<LeafBuffer>,
    branches: Vec<BranchState>,
    /// Fields whose counter is shared; their lengths must agree.
    groups: Vec<Vec<usize>>,
    entries: u64,
    /// The first entry of the cluster being buffered.
    cluster_start: u64,
    /// The cluster size, once set or reached.
    cluster_entries: Option<u64>,
    /// Whether a cluster was written before the last.
    flushed: bool,
    flushed_bytes: i64,
    /// The directory it is written in.
    dir: usize,
    /// Cluster ranges before the clusters of this session
    /// (`fClusterRangeEnd`, inclusive, and `fClusterSize`).
    ranges: Vec<(i64, i64)>,
    /// The tree this one extends (update mode): its key, replaced at finish.
    extends: Option<KeyHeader>,
    /// The entries before this session.
    first_new: u64,
}

impl TreeState {
    #[expect(clippy::too_many_lines, reason = "one arm per kind of field")]
    pub(crate) fn new(
        name: &str,
        options: TreeOptions,
        names: &[String],
        shapes: &[Option<LeafShape>],
        counts: &[Option<&str>],
        classes: &[Option<ClassSpec>],
        dir: usize,
    ) -> Result<Self> {
        if name.is_empty() || name.contains(['/', ';']) {
            return Err(Error::new(
                ErrorKind::InvalidPath,
                format!("invalid tree name `{name}`"),
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
        let mut branches: Vec<BranchState> = Vec::new();
        let mut groups: Vec<Vec<usize>> = Vec::new();
        let new_branch = |name: String, shape: LeafShape, source: Source| BranchState {
            name,
            shape,
            source,
            counter: None,
            baskets: Vec::new(),
            tot_bytes: 0,
            zip_bytes: 0,
            longest: 0,
            split: None,
        };
        for (i, (name, shape)) in names.iter().zip(shapes).enumerate() {
            let shape = shape.ok_or_else(|| {
                Error::new(
                    ErrorKind::Unsupported,
                    format!("field {name}: objects cannot be written to a tree yet"),
                )
            })?;
            check_name(name)?;
            if branches.iter().any(|b| b.name == *name) {
                return Err(invalid(format!("two branches are named `{name}`")));
            }
            if names[i + 1..].contains(name) {
                return Err(invalid(format!("two fields are named `{name}`")));
            }
            if matches!(shape.kind, LeafKind::Object | LeafKind::Objects) {
                let spec = classes.get(i).cloned().flatten().ok_or_else(|| {
                    invalid(format!("field {name}: a class without its description"))
                })?;
                // An object's sub-branches are named after it: `name.`.
                let node = if shape.kind == LeafKind::Object {
                    let dotted = if name.ends_with('.') {
                        name.clone()
                    } else {
                        format!("{name}.")
                    };
                    Node::object(&dotted, &spec)?
                } else {
                    Node::collection(name, &spec)?
                };
                let mut branch = new_branch(node.name().to_owned(), shape, Source::Field(i));
                branch.split = Some(Box::new(node));
                branches.push(branch);
                continue;
            }
            let mut branch = new_branch(name.clone(), shape, Source::Field(i));
            if shape.kind == LeafKind::Jagged {
                let count = counts
                    .get(i)
                    .copied()
                    .flatten()
                    .map_or_else(|| format!("n_{name}"), str::to_owned);
                check_name(&count)?;
                if names.contains(&count) {
                    return Err(invalid(format!(
                        "the counter of {name}, `{count}`, is also a field"
                    )));
                }
                let at = if let Some(at) = branches.iter().position(|b| b.name == count) {
                    // A counter shared with an earlier field.
                    let Source::Counts(first) = branches[at].source else {
                        return Err(invalid(format!("`{count}` is not a counter")));
                    };
                    match groups.iter_mut().find(|g| g.first() == Some(&first)) {
                        Some(g) => g.push(i),
                        None => groups.push(vec![first, i]),
                    }
                    at
                } else {
                    let shape = LeafShape::new(Scalar::I32, LeafKind::One);
                    branches.push(new_branch(count, shape, Source::Counts(i)));
                    branches.len() - 1
                };
                branch.counter = Some(at);
            }
            branches.push(branch);
        }
        Ok(Self {
            name: name.to_owned(),
            cluster_entries: options.cluster_entries,
            options,
            bufs: names.iter().map(|_| LeafBuffer::default()).collect(),
            branches,
            groups,
            entries: 0,
            cluster_start: 0,
            flushed: false,
            flushed_bytes: 0,
            dir,
            ranges: Vec::new(),
            extends: None,
            first_new: 0,
        })
    }

    /// Continues the tree `old` (update mode): same branches, its baskets
    /// kept, its entries as the first ones; its clusters become an explicit
    /// range, and its key is replaced by the new cycle at finish.
    #[expect(clippy::too_many_lines, reason = "checks, then the old tree's state")]
    fn extend(mut self, old: &DynObject, key: KeyHeader) -> Result<Self> {
        let name = self.name.clone();
        let mismatch = |what: String| {
            invalid(format!(
                "tree `{name}` cannot be extended with these rows: {what}"
            ))
        };
        let list = |o: &DynObject, member: &str| -> Vec<DynObject> {
            o.get(member)
                .and_then(Value::as_object)
                .and_then(|a| a.get("fObjects"))
                .and_then(Value::as_list)
                .map(|l| l.iter().filter_map(Value::as_object).cloned().collect())
                .unwrap_or_default()
        };
        if self.branches.iter().any(|b| b.split.is_some()) {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("tree `{name}` has split branches: extending it is not supported yet"),
            ));
        }
        let branches = list(old, "fBranches");
        if branches.len() != self.branches.len() {
            return Err(mismatch(format!(
                "it has {} branches, the rows {}",
                branches.len(),
                self.branches.len()
            )));
        }
        let int = |o: &DynObject, m: &str| o.get(m).and_then(Value::as_i64).unwrap_or(0);
        let ints = |o: &DynObject, m: &str| -> Vec<i64> {
            match o.get(m).and_then(Value::as_array) {
                Some(a) => (0..a.len())
                    .filter_map(|i| a.get(i).and_then(|v| v.as_i64()))
                    .collect(),
                None => Vec::new(),
            }
        };
        for (i, o) in branches.iter().enumerate() {
            let (title, class) = {
                let b = &self.branches[i];
                (self.branch_title(b), b.shape.leaf().0)
            };
            let leaves = list(o, "fLeaves");
            let leaf = leaves.first();
            let found = (
                o.get("fName").and_then(Value::as_str).unwrap_or_default(),
                o.get("fTitle").and_then(Value::as_str).unwrap_or_default(),
                leaf.map_or("", DynObject::class),
            );
            let b = &mut self.branches[i];
            if found != (b.name.as_str(), title.as_str(), class) {
                return Err(mismatch(format!(
                    "branch {} is `{}` ({}, {}), the rows write `{}` ({title}, {class})",
                    i, found.0, found.1, found.2, b.name
                )));
            }
            let n = usize::try_from(int(o, "fWriteBasket")).unwrap_or(0);
            let (bytes, first, seeks) = (
                ints(o, "fBasketBytes"),
                ints(o, "fBasketEntry"),
                ints(o, "fBasketSeek"),
            );
            if bytes.len() < n || first.len() < n || seeks.len() < n {
                return Err(Error::malformed(format!(
                    "branch {}: basket tables too short",
                    b.name
                )));
            }
            b.baskets = (0..n)
                .map(|k| Basket {
                    nbytes: i32::try_from(bytes[k]).unwrap_or(0),
                    entry: first[k],
                    seek: seeks[k],
                })
                .collect();
            b.tot_bytes = int(o, "fTotBytes");
            b.zip_bytes = int(o, "fZipBytes");
            if let Some(leaf) = leaf {
                let longest = if leaf.class() == "TLeafC" {
                    int(leaf, "fLen") - 1
                } else {
                    int(leaf, "fMaximum")
                };
                b.longest = usize::try_from(longest).unwrap_or(0);
            }
        }
        let entries = u64::try_from(int(old, "fEntries")).unwrap_or(0);
        // The old clusters: its ranges, then its tail as one more range.
        let ends = ints(old, "fClusterRangeEnd");
        let sizes = ints(old, "fClusterSize");
        let n = usize::try_from(int(old, "fNClusterRange"))
            .unwrap_or(0)
            .min(ends.len())
            .min(sizes.len());
        self.ranges = ends[..n]
            .iter()
            .copied()
            .zip(sizes[..n].iter().copied())
            .collect();
        let start = self.ranges.last().map_or(0, |&(e, _)| e + 1);
        let end = i64::try_from(entries).map_err(|_| invalid("too many entries"))?;
        if start < end {
            let auto = int(old, "fAutoFlush");
            let size = if auto > 0 { auto } else { end - start };
            self.ranges.push((end - 1, size));
        }
        self.entries = entries;
        self.cluster_start = entries;
        self.first_new = entries;
        self.flushed_bytes = int(old, "fFlushedBytes");
        self.extends = Some(key);
        Ok(self)
    }

    /// Checks the row just appended: fields sharing a counter must have as
    /// many values. On error the row is removed.
    fn check_row(&mut self, marks: &[(usize, usize, usize)]) -> Result<()> {
        for g in &self.groups {
            let len = |f: usize| self.bufs.get(f).and_then(|b| b.counts.last()).copied();
            let first = len(g[0]);
            if let Some(&other) = g.iter().find(|&&f| len(f) != first) {
                let name = |f: usize| {
                    self.branches
                        .iter()
                        .find(|b| matches!(b.source, Source::Field(x) if x == f))
                        .map_or("?", |b| b.name.as_str())
                };
                let e = invalid(format!(
                    "{} has {} values but {} has {}, and they share a counter",
                    name(g[0]),
                    first.unwrap_or(0),
                    name(other),
                    len(other).unwrap_or(0)
                ));
                for (b, &m) in self.bufs.iter_mut().zip(marks) {
                    b.truncate(m);
                }
                return Err(e);
            }
        }
        Ok(())
    }

    /// Encodes the objects of the row just appended into their split
    /// branches; on error the row is removed.
    fn encode_objects(&mut self, marks: &[(usize, usize, usize)]) -> Result<()> {
        let mut node_marks = Vec::new();
        for b in &self.branches {
            if let Some(n) = &b.split {
                n.mark(&mut node_marks);
            }
        }
        let mut encoder = crate::streamer::Encoder::new(crate::streamer::builtin());
        let mut result = Ok(());
        for b in &mut self.branches {
            if let (Some(node), Source::Field(f)) = (b.split.as_mut(), b.source) {
                let Some(value) = self.bufs[f].objects.pop() else {
                    continue;
                };
                if let Err(e) = node.fill(&value, &mut encoder) {
                    result = Err(e.with_context(crate::Context::Key(b.name.clone())));
                    break;
                }
            }
        }
        if result.is_err() {
            for (b, &m) in self.bufs.iter_mut().zip(marks) {
                b.truncate(m);
                b.objects.clear();
            }
            let mut it = node_marks.into_iter();
            for b in &mut self.branches {
                if let Some(n) = b.split.as_mut() {
                    n.truncate(&mut it);
                }
            }
        }
        result
    }

    fn should_flush(&self) -> bool {
        let n = self.entries - self.cluster_start;
        if let Some(c) = self.cluster_entries {
            n >= c
        } else {
            let split: usize = self
                .branches
                .iter()
                .filter_map(|b| b.split.as_ref())
                .map(|n| n.buffered())
                .sum();
            self.bufs.iter().map(LeafBuffer::bytes).sum::<usize>() + split
                >= self.options.cluster_bytes
        }
    }

    /// Writes the buffered entries: one basket per branch.
    fn flush(&mut self, file: &mut WritableFile, last: bool) -> Result<()> {
        let n = self.entries - self.cluster_start;
        if n == 0 {
            return Ok(());
        }
        let entry = i64::try_from(self.cluster_start).map_err(|_| invalid("too many entries"))?;
        let nev = i32::try_from(n).map_err(|_| invalid("too many entries in a cluster"))?;
        for branch in &mut self.branches {
            if let Some(node) = branch.split.as_mut() {
                node.flush(file, self.dir, &self.name, entry, nev)?;
                continue;
            }
            let (data, starts, size): (std::borrow::Cow<'_, [u8]>, Option<&[usize]>, i32) =
                match branch.source {
                    Source::Counts(f) => {
                        let buf = &self.bufs[f];
                        let mut data = Vec::with_capacity(4 * buf.counts.len());
                        for &c in &buf.counts {
                            let n = i32::try_from(c).map_err(|_| {
                                invalid(format!("{}: too many values", branch.name))
                            })?;
                            data.extend_from_slice(&n.to_be_bytes());
                            branch.longest = branch.longest.max(c);
                        }
                        (data.into(), None, 4)
                    }
                    Source::Field(f) => {
                        let buf = &self.bufs[f];
                        let (_, _, _, size) = branch.shape.leaf();
                        match branch.shape.kind {
                            // Split fields are flushed by their nodes.
                            LeafKind::One | LeafKind::Object | LeafKind::Objects => (
                                buf.data.as_slice().into(),
                                None,
                                i32::try_from(size).unwrap_or(1),
                            ),
                            LeafKind::Fixed(len) => {
                                let size = i32::try_from(size * len)
                                    .map_err(|_| invalid("array too long"))?;
                                (buf.data.as_slice().into(), None, size)
                            }
                            LeafKind::Jagged => (
                                buf.data.as_slice().into(),
                                Some(buf.starts.as_slice()),
                                OFFSETS_CAPACITY,
                            ),
                            LeafKind::String => {
                                if let Some(&m) = buf.counts.iter().max() {
                                    if i32::try_from(m).is_err() {
                                        return Err(invalid(format!(
                                            "{}: a string is too long",
                                            branch.name
                                        )));
                                    }
                                    branch.longest = branch.longest.max(m);
                                }
                                (
                                    buf.data.as_slice().into(),
                                    Some(buf.starts.as_slice()),
                                    OFFSETS_CAPACITY,
                                )
                            }
                        }
                    }
                };
            let size = match starts {
                Some(s) => OFFSETS_CAPACITY.max(i32::try_from(s.len() + 1).unwrap_or(i32::MAX)),
                None => size,
            };
            let written =
                file.append_basket(self.dir, &branch.name, &self.name, &data, starts, nev, size)?;
            branch.tot_bytes += i64::from(written.key_len) + i64::from(written.obj_len);
            branch.zip_bytes += i64::from(written.nbytes);
            branch.baskets.push(Basket {
                nbytes: i32::try_from(written.nbytes).map_err(|_| invalid("basket too large"))?,
                entry,
                seek: i64::try_from(written.seek).map_err(|_| invalid("file too large"))?,
            });
        }
        for b in &mut self.bufs {
            b.clear();
        }
        self.cluster_start = self.entries;
        if !last {
            self.flushed = true;
            self.flushed_bytes = self.branches.iter().map(|b| b.zip_bytes).sum();
            if self.cluster_entries.is_none() {
                self.cluster_entries = Some(n);
            }
        }
        Ok(())
    }

    /// Writes the last cluster and the tree object.
    pub(crate) fn finish(mut self, file: &mut WritableFile) -> Result<()> {
        if self.extends.is_some() && self.entries == self.first_new {
            return Ok(()); // nothing added: the tree stays as it is
        }
        self.flush(file, true)?;
        let tree = self.object(file.compression_setting())?;
        file.put_dyn(self.dir, &self.name, &tree)?;
        if let Some(old) = self.extends.take() {
            file.drop_key(self.dir, &old);
        }
        Ok(())
    }

    fn branch_title(&self, b: &BranchState) -> String {
        let (_, letter, _, _) = b.shape.leaf();
        match b.shape.kind {
            LeafKind::One | LeafKind::String | LeafKind::Object | LeafKind::Objects => {
                format!("{}/{letter}", b.name)
            }
            LeafKind::Fixed(n) => format!("{}[{n}]/{letter}", b.name),
            LeafKind::Jagged => {
                let count = b
                    .counter
                    .and_then(|c| self.branches.get(c))
                    .map_or("", |c| c.name.as_str());
                format!("{}[{count}]/{letter}", b.name)
            }
        }
    }

    fn leaf(&self, b: &BranchState) -> Result<DynObject> {
        let (class, _, unsigned, size) = b.shape.leaf();
        let (title, len) = match b.shape.kind {
            LeafKind::One | LeafKind::Object | LeafKind::Objects => (b.name.clone(), 1),
            LeafKind::Fixed(n) => (format!("{}[{n}]", b.name), n),
            LeafKind::Jagged => {
                let counter = b.counter.and_then(|c| self.branches.get(c));
                let count = counter.map_or("", |c| c.name.as_str());
                (format!("{}[{count}]", b.name), 1)
            }
            LeafKind::String => (b.name.clone(), b.longest + 1),
        };
        let is_range = matches!(b.source, Source::Counts(_));
        let leaf_count = match b.counter.and_then(|c| self.branches.get(c)) {
            Some(c) => Value::Object(Box::new(self.leaf(c)?)),
            None => Value::Null,
        };
        let int = |x: usize| i32::try_from(x).map_err(|_| invalid("leaf too long"));
        let (min, max) = match (class, b.shape.kind) {
            ("TLeafO", _) => (Value::Bool(false), Value::Bool(false)),
            ("TLeafC", _) => (Value::I32(0), Value::I32(int(len)?)),
            (_, _) if is_range => (Value::I32(0), Value::I32(int(b.longest)?)),
            _ => (Value::I32(0), Value::I32(0)),
        };
        Ok(DynObject::new(class)
            .with("fUniqueID", 0u32)
            .with("fBits", 0u32)
            .with("fName", b.name.as_str())
            .with("fTitle", title)
            .with("fLen", int(len)?)
            .with("fLenType", int(size)?)
            .with("fOffset", 0i32)
            .with("fIsRange", is_range)
            .with("fIsUnsigned", unsigned)
            .with("fLeafCount", leaf_count)
            .with("fMinimum", min)
            .with("fMaximum", max))
    }

    fn branch(&self, b: &BranchState, compress: i32) -> Result<DynObject> {
        let title = self.branch_title(b);
        let offsets = matches!(b.shape.kind, LeafKind::Jagged | LeafKind::String)
            && matches!(b.source, Source::Field(_));
        let entries = i64::try_from(self.entries).map_err(|_| invalid("too many entries"))?;
        let nbaskets = b.baskets.len();
        let max_baskets = (nbaskets + 1).max(MIN_MAX_BASKETS);
        let mut bytes = vec![0i32; max_baskets];
        let mut first = vec![0i64; max_baskets];
        let mut seeks = vec![0i64; max_baskets];
        for (i, basket) in b.baskets.iter().enumerate() {
            bytes[i] = basket.nbytes;
            first[i] = basket.entry;
            seeks[i] = basket.seek;
        }
        // fBasketEntry[fWriteBasket] holds the entry count.
        first[nbaskets] = entries;
        let n = |x: usize| i32::try_from(x).map_err(|_| invalid("too many baskets"));
        Ok(DynObject::new("TBranch")
            .with("fUniqueID", 0u32)
            .with("fBits", BRANCH_BITS)
            .with("fName", b.name.as_str())
            .with("fTitle", title)
            .with("fFillColor", 0i16)
            .with("fFillStyle", 1001i16)
            .with("fCompress", compress)
            .with("fBasketSize", BASKET_SIZE)
            .with(
                "fEntryOffsetLen",
                if offsets { ENTRY_OFFSET_LEN } else { 0 },
            )
            .with("fWriteBasket", n(nbaskets)?)
            .with("fEntryNumber", entries)
            .with("fIOFeatures", io_features())
            .with("fOffset", 0i32)
            .with("fMaxBaskets", n(max_baskets)?)
            .with("fSplitLevel", 0i32)
            .with("fEntries", entries)
            .with("fFirstEntry", 0i64)
            .with("fTotBytes", b.tot_bytes)
            .with("fZipBytes", b.zip_bytes)
            .with("fBranches", obj_array(0, Vec::new()))
            .with(
                "fLeaves",
                obj_array(0, vec![Value::Object(Box::new(self.leaf(b)?))]),
            )
            .with("fBaskets", obj_array(0, Vec::new()))
            .with("fBasketBytes", Array::I32(bytes))
            .with("fBasketEntry", Array::I64(first))
            .with("fBasketSeek", Array::I64(seeks))
            .with("fFileName", ""))
    }

    /// The `TTree` object, with ROOT 6.42's values for what we do not set.
    fn object(&self, compress: i32) -> Result<DynObject> {
        let mut branches = Vec::with_capacity(self.branches.len());
        let mut leaves = Vec::with_capacity(self.branches.len());
        let entries = i64::try_from(self.entries).map_err(|_| invalid("too many entries"))?;
        let (mut tot, mut zip) = (0i64, 0i64);
        for b in &self.branches {
            if let Some(node) = &b.split {
                branches.push(Value::Object(Box::new(
                    node.branch_object(node, entries, compress),
                )));
                node.leaves(node, &mut leaves);
                let (t, z) = node.bytes();
                tot += t;
                zip += z;
                continue;
            }
            branches.push(Value::Object(Box::new(self.branch(b, compress)?)));
            leaves.push(Value::Object(Box::new(self.leaf(b)?)));
            tot += b.tot_bytes;
            zip += b.zip_bytes;
        }
        let auto_flush = match (self.flushed, self.options.cluster_entries) {
            // After explicit ranges, fAutoFlush is the size of what follows.
            _ if !self.ranges.is_empty() => self
                .cluster_entries
                .or(Some(self.entries - self.first_new))
                .and_then(|c| i64::try_from(c.max(1)).ok())
                .unwrap_or(1),
            (false, None) => DEFAULT_AUTO_FLUSH,
            _ => self
                .cluster_entries
                .and_then(|c| i64::try_from(c).ok())
                .unwrap_or(DEFAULT_AUTO_FLUSH),
        };
        let n_ranges = i32::try_from(self.ranges.len()).map_err(|_| invalid("too many ranges"))?;
        let range_ends: Vec<i64> = self.ranges.iter().map(|r| r.0).collect();
        let range_sizes: Vec<i64> = self.ranges.iter().map(|r| r.1).collect();
        Ok(DynObject::new("TTree")
            .with("fUniqueID", 0u32)
            .with("fBits", TREE_BITS)
            .with("fName", self.name.as_str())
            .with("fTitle", self.options.title.as_str())
            .with("fLineColor", 602i16)
            .with("fLineStyle", 1i16)
            .with("fLineWidth", 1i16)
            .with("fFillColor", 0i16)
            .with("fFillStyle", 1001i16)
            .with("fMarkerColor", 1i16)
            .with("fMarkerStyle", 1i16)
            .with("fMarkerSize", 1.0f32)
            .with("fEntries", entries)
            .with("fTotBytes", tot)
            .with("fZipBytes", zip)
            .with("fSavedBytes", 0i64)
            .with("fFlushedBytes", self.flushed_bytes)
            .with("fWeight", 1.0f64)
            .with("fTimerInterval", 0i32)
            .with("fScanField", 25i32)
            .with("fUpdate", 0i32)
            .with("fDefaultEntryOffsetLen", OFFSETS_CAPACITY)
            .with("fNClusterRange", n_ranges)
            .with("fMaxEntries", 1_000_000_000_000i64)
            .with("fMaxEntryLoop", 1_000_000_000_000i64)
            .with("fMaxVirtualSize", 0i64)
            .with("fAutoSave", -300_000_000i64)
            .with("fAutoFlush", auto_flush)
            .with("fEstimate", 1_000_000i64)
            .with("fClusterRangeEnd", Array::I64(range_ends))
            .with("fClusterSize", Array::I64(range_sizes))
            .with("fIOFeatures", io_features())
            .with("fBranches", obj_array(OWNER_LIST_BITS, branches))
            .with("fLeaves", obj_array(0, leaves))
            .with("fAliases", Value::Null)
            .with("fIndexValues", DynObject::new("TArrayD"))
            .with("fIndex", DynObject::new("TArrayI"))
            .with("fTreeIndex", Value::Null)
            .with("fFriends", Value::Null)
            .with("fUserInfo", Value::Null)
            .with("fBranchRef", Value::Null))
    }
}

fn check_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains(['/', '[', ']', ':', ';']) {
        return Err(invalid(format!("invalid branch name `{name}`")));
    }
    Ok(())
}

fn io_features() -> DynObject {
    DynObject::new("ROOT::TIOFeatures").with("fIOBits", 0u8)
}

fn obj_array(bits: u32, objects: Vec<Value>) -> DynObject {
    DynObject::new("TObjArray")
        .with("fUniqueID", 0u32)
        .with("fBits", bits)
        .with("fName", "")
        .with("fLowerBound", 0i32)
        .with("fObjects", Value::List(objects))
}

/// Where a writer's file is, and the directory it writes in.
pub(crate) struct Target<'a> {
    access: Access<'a>,
    pub(crate) dir: usize,
}

enum Access<'a> {
    Scope(&'a Mutex<Option<WritableFile>>),
    File(&'a mut WritableFile),
}

impl<'a> Target<'a> {
    pub(crate) fn scope(file: &'a Mutex<Option<WritableFile>>, dir: usize) -> Self {
        Self {
            access: Access::Scope(file),
            dir,
        }
    }

    pub(crate) fn file(file: &'a mut WritableFile, dir: usize) -> Self {
        Self {
            access: Access::File(file),
            dir,
        }
    }

    pub(crate) fn with<T>(&mut self, f: impl FnOnce(&mut WritableFile) -> Result<T>) -> Result<T> {
        match &mut self.access {
            Access::Scope(m) => f(m
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_mut()
                .ok_or_else(|| Error::malformed("file already closed"))?),
            Access::File(file) => f(file),
        }
    }
}

/// In update mode, the tree of that name the file had: its key and object.
fn existing_tree(
    file: &WritableFile,
    dir: usize,
    name: &str,
) -> Result<Option<(KeyHeader, DynObject)>> {
    let Some(reader) = file.reader() else {
        return Ok(None);
    };
    let Some(key) = file
        .find_key(dir, name)
        .filter(|k| k.class_name == "TTree")
        .cloned()
    else {
        return Ok(None);
    };
    if !file.is_old(key.seek_key) {
        return Ok(None); // written by this session: a new cycle
    }
    let path = match file.dir_path(dir) {
        p if p.is_empty() => format!("{name};{}", key.cycle),
        p => format!("{p}/{name};{}", key.cycle),
    };
    let tree = reader.get_dyn(&path)?;
    Ok(Some((key, tree)))
}

/// Writes rows of `R` to a flat `TTree`, from
/// [`FileScope::tree`](crate::FileScope::tree) or
/// [`WritableFile::tree`](crate::WritableFile::tree).
///
/// Rows are buffered and written a cluster at a time. The tree is complete
/// once [`finish`](Self::finish) returns; a writer dropped unfinished is
/// finished when its file closes (where errors are reported).
///
/// ```
/// #[derive(rhizoid_io::Row)]
/// struct Hit {
///     run: i32,      // run/I
///     xyz: [f32; 3], // xyz[3]/F
///     e: Vec<f64>,   // n_e/I, e[n_e]/D
///     label: String, // label/C
/// }
/// # let dir = std::env::temp_dir().join(format!("rhizoid-doc-{}", std::process::id()));
/// # std::fs::create_dir_all(&dir)?;
/// # let path = dir.join("hits.root");
/// rhizoid_io::create(&path, |f| {
///     let mut t = f.tree::<Hit>("hits")?;
///     for run in 0..3 {
///         t.fill(&Hit { run, xyz: [1.0, 2.0, 3.0], e: vec![0.5; run as usize], label: "hit".into() })?;
///     }
///     t.finish()
/// })?;
/// let tree = rhizoid_io::File::open(&path)?.tree("hits")?;
/// assert_eq!(tree.num_entries(), 3);
/// # Ok::<(), rhizoid_io::Error>(())
/// ```
pub struct TreeWriter<'a, R> {
    state: Option<TreeState>,
    target: Target<'a>,
    marks: Vec<(usize, usize, usize)>,
    _row: PhantomData<fn(&R)>,
}

impl<R> fmt::Debug for TreeWriter<'_, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("TreeWriter");
        if let Some(s) = &self.state {
            d.field("name", &s.name).field("entries", &s.entries);
        }
        d.finish_non_exhaustive()
    }
}

impl<'a, R: Row> TreeWriter<'a, R> {
    pub(crate) fn new(
        target: Target<'a>,
        name: &str,
        names: &[String],
        options: TreeOptions,
    ) -> Result<Self> {
        let shapes = R::__shapes();
        let counts = R::__counts();
        let classes = R::__classes();
        let mut target = target;
        let mut state =
            TreeState::new(name, options, names, &shapes, &counts, &classes, target.dir)?;
        // The descriptions of the classes split go into the StreamerInfo.
        let infos: Vec<_> = classes
            .iter()
            .flatten()
            .flat_map(|c| c.infos.clone())
            .collect();
        if !infos.is_empty() {
            target.with(|f| {
                f.register_infos(&infos);
                Ok(())
            })?;
        }
        let dir = target.dir;
        if let Some((key, old)) = target.with(|f| existing_tree(f, dir, name))? {
            state = state.extend(&old, key)?;
        }
        Ok(Self {
            state: Some(state),
            target,
            marks: Vec::new(),
            _row: PhantomData,
        })
    }

    /// Appends a row.
    ///
    /// # Errors
    ///
    /// `InvalidInput` if fields sharing a counter have different lengths
    /// (the row is not written); I/O errors writing a cluster.
    pub fn fill(&mut self, row: &R) -> Result<()> {
        let Some(state) = self.state.as_mut() else {
            return Err(Error::malformed("tree already finished"));
        };
        let split = state.branches.iter().any(|b| b.split.is_some());
        if state.groups.is_empty() && !split {
            row.__put(&mut state.bufs);
        } else {
            self.marks.clear();
            self.marks.extend(state.bufs.iter().map(LeafBuffer::mark));
            row.__put(&mut state.bufs);
            state.check_row(&self.marks)?;
            if split {
                state.encode_objects(&self.marks)?;
            }
        }
        state.entries += 1;
        if state.should_flush() {
            self.target.with(|f| state.flush(f, false))?;
        }
        Ok(())
    }

    /// Appends rows.
    ///
    /// # Errors
    ///
    /// As [`fill`](Self::fill); the rows before the failing one are written.
    pub fn extend<'r>(&mut self, rows: impl IntoIterator<Item = &'r R>) -> Result<()>
    where
        R: 'r,
    {
        rows.into_iter().try_for_each(|r| self.fill(r))
    }

    /// The number of rows appended.
    #[must_use]
    pub fn entries(&self) -> u64 {
        self.state.as_ref().map_or(0, |s| s.entries)
    }

    /// Writes the last cluster and the tree object.
    ///
    /// # Errors
    ///
    /// I/O errors; `InvalidInput` if a count or size exceeds ROOT's limits.
    pub fn finish(mut self) -> Result<()> {
        match self.state.take() {
            Some(state) => self.target.with(|f| state.finish(f)),
            None => Ok(()),
        }
    }
}

impl<R> Drop for TreeWriter<'_, R> {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            // No I/O here: the file finishes it when it closes.
            let _ = self.target.with(|f| {
                f.defer_tree(state);
                Ok(())
            });
        }
    }
}
