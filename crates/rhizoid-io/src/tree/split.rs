//! Split branches of user classes (`TBranchElement`), written as
//! ROOT 6.42 writes them (`tests/cross/user_classes.C`, `user_tree_write`).
//!
//! An object field (`event.`) is a container branch (`fType` 0, `fID` −2)
//! whose sub-branches follow the class description: a base class is a
//! container (`fType` 1), an embedded object one too (`fType` 2), every
//! other member a branch of its own (`fType` 0, `fID` its index) holding,
//! per entry, the member as it is streamed in the object. A `std::vector`
//! of objects (`hits`) is a counter branch (`fType` 4) and one branch per
//! member of the class, its bases flattened (`fType` 41), holding the
//! member of every element back to back.

use std::sync::Arc;

use super::write::{BASKET_SIZE, OFFSETS_CAPACITY};
use crate::core::buffer::WBuffer;
use crate::core::error::{Error, ErrorKind, Result};
use crate::streamer::{
    self, Array, DynObject, ElementKind, Encoder, StreamerElement, StreamerInfo, Value,
};
use crate::user::ClassSpec;
use crate::write::WritableFile;

/// `TObject::fBits` of container branches, member branches, collection
/// counters, and of a variable array's leaf, as ROOT 6.42 writes them.
const NODE_BITS: u32 = 0x0050_1000;
const MEMBER_BITS: u32 = 0x0050_0000;
const COUNTER_BITS: u32 = 0x0052_0000;
const VAR_LEAF_BITS: u32 = 0x0400_0000;
/// `fEntryOffsetLen` of containers, and of members of variable size.
const NODE_OFFSET_LEN: i32 = 1000;
const MEMBER_OFFSET_LEN: i32 = 20;
/// `TClass::GetClassVersion` of `std::vector`.
const VECTOR_VERSION: i16 = 6;
/// The bits ROOT does not store in a split `fBits` (`kIsOnHeap`,
/// `kNotDeleted`).
const HEAP_BITS: u32 = 0x0300_0000;

fn unsupported(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Unsupported, message)
}

/// How to get from an entry's object to the object owning a member.
#[derive(Clone, Debug)]
enum Step {
    /// A base class part.
    Base(String),
    /// An embedded object.
    Member(String),
}

/// What fills a branch.
#[derive(Clone, Debug)]
enum Fill {
    /// Nothing: a container.
    None,
    /// A member of the object.
    Member {
        path: Vec<Step>,
        element: StreamerElement,
    },
    /// The size of the collection.
    Count,
    /// A member of every element of the collection.
    Items {
        path: Vec<Step>,
        element: StreamerElement,
    },
}

#[derive(Clone, Debug)]
struct LeafSpec {
    name: String,
    title: String,
    len: i32,
    len_type: i32,
    unsigned: bool,
    id: i32,
    kind: i32,
    bits: u32,
    /// The branch (a path of child indices from the field's branch) whose
    /// leaf counts this one.
    count: Option<Vec<usize>>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Basket {
    pub(crate) nbytes: i32,
    pub(crate) entry: i64,
    pub(crate) seek: i64,
}

/// One branch of a split field.
#[derive(Debug)]
pub(crate) struct Node {
    name: String,
    title: String,
    kind: i32,
    id: i32,
    streamer_type: i32,
    class_name: String,
    parent_name: String,
    clones_name: String,
    checksum: u32,
    class_version: i16,
    split_level: i32,
    entry_offset_len: i32,
    bits: u32,
    maximum: usize,
    children: Vec<Node>,
    leaf: Option<LeafSpec>,
    branch_count: Option<Vec<usize>>,
    /// A member of a collection: `fBranchCount` refers to its counter, the
    /// collection's branch, by name.
    counted_by: Option<String>,
    fill: Fill,
    /// Entries buffered: bytes, and where each starts (variable sizes).
    data: Vec<u8>,
    starts: Vec<usize>,
    baskets: Vec<Basket>,
    tot_bytes: i64,
    zip_bytes: i64,
}

/// The descriptions a split field is built from: its own, then ROOT's.
struct Infos<'a>(&'a ClassSpec);

impl Infos<'_> {
    fn get(&self, class: &str) -> Result<Arc<StreamerInfo>> {
        self.0
            .infos
            .iter()
            .find(|i| i.class_name() == class)
            .cloned()
            .or_else(|| streamer::builtin().latest(class).cloned())
            .ok_or_else(|| unsupported(format!("no description of class {class}")))
    }
}

/// `TClass::GetCheckSum` of a class without members of its own, such as an
/// STL collection: the hash of its name.
fn name_checksum(name: &str) -> u32 {
    name.bytes()
        .fold(0u32, |id, c| id.wrapping_mul(3).wrapping_add(u32::from(c)))
}

/// The leaf's value size, unsignedness and length of a member.
fn leaf_shape(el: &StreamerElement) -> (i32, bool, i32) {
    let code = el.type_code();
    match el.kind() {
        // kCounter and kBits have no size in their leaf.
        ElementKind::Scalar(s) if !matches!(code, 6 | 15) => {
            (size_of_scalar(*s), is_unsigned(*s), 1)
        }
        ElementKind::FixedArray { scalar, len } => (
            size_of_scalar(*scalar),
            is_unsigned(*scalar),
            i32::try_from(*len).unwrap_or(1),
        ),
        ElementKind::VarArray { scalar, .. } => (size_of_scalar(*scalar), is_unsigned(*scalar), -1),
        _ => (0, false, 1),
    }
}

fn size_of_scalar(s: streamer::Scalar) -> i32 {
    i32::try_from(s.size()).unwrap_or(0)
}

fn is_unsigned(s: streamer::Scalar) -> bool {
    use streamer::Scalar::{U8, U16, U32, U64};
    matches!(s, U8 | U16 | U32 | U64)
}

/// The type code a branch gives a member: STL members (`std::string`,
/// vectors), described as 500 in the class, are 300 (`kSTL`) in branches.
fn branch_type(el: &StreamerElement) -> i32 {
    match (el.kind(), el.type_code()) {
        (ElementKind::Stl { .. } | ElementKind::StdString, 500) => 300,
        (_, code) => code,
    }
}

/// Whether a member's entries have a fixed size (no entry offsets).
fn fixed_size(el: &StreamerElement) -> Option<usize> {
    match el.kind() {
        ElementKind::Scalar(s) if el.type_code() != 15 => Some(s.size()),
        _ => None,
    }
}

impl Node {
    fn container(
        name: String,
        kind: i32,
        id: i32,
        el_code: i32,
        owner: &StreamerInfo,
        split: i32,
    ) -> Self {
        Self {
            title: name.clone(),
            name,
            kind,
            id,
            streamer_type: el_code,
            class_name: owner.class_name().to_owned(),
            parent_name: owner.class_name().to_owned(),
            clones_name: String::new(),
            checksum: owner.checksum(),
            class_version: i16::try_from(owner.class_version()).unwrap_or(0),
            split_level: split,
            entry_offset_len: NODE_OFFSET_LEN,
            bits: NODE_BITS,
            maximum: 0,
            children: Vec::new(),
            leaf: None,
            branch_count: None,
            counted_by: None,
            fill: Fill::None,
            data: Vec::new(),
            starts: Vec::new(),
            baskets: Vec::new(),
            tot_bytes: 0,
            zip_bytes: 0,
        }
    }

    /// The branch of an object field: `name` ends with a dot.
    pub(crate) fn object(name: &str, spec: &ClassSpec) -> Result<Self> {
        let infos = Infos(spec);
        let info = spec
            .infos
            .first()
            .cloned()
            .ok_or_else(|| unsupported("a class without a description"))?;
        let mut top = Self::container(name.to_owned(), 0, -2, -1, &info, 99);
        top.parent_name = String::new();
        top.leaf = Some(LeafSpec {
            name: name.to_owned(),
            title: name.to_owned(),
            len: 1,
            len_type: 0,
            unsigned: false,
            id: -2,
            kind: -1,
            bits: 0,
            count: None,
        });
        top.children = members(&infos, &info, name, &[], true, &[])?;
        Ok(top)
    }

    /// The branch of a `std::vector` field of objects.
    pub(crate) fn collection(name: &str, spec: &ClassSpec) -> Result<Self> {
        let infos = Infos(spec);
        let info = spec
            .infos
            .first()
            .cloned()
            .ok_or_else(|| unsupported("a class without a description"))?;
        let class = format!("vector<{}>", info.class_name());
        let count = format!("{name}_");
        let mut top = Self::container(name.to_owned(), 4, -1, -1, &info, 99);
        top.title.clone_from(&count);
        top.class_name.clone_from(&class);
        top.parent_name = String::new();
        info.class_name().clone_into(&mut top.clones_name);
        top.checksum = name_checksum(&class);
        top.class_version = VECTOR_VERSION;
        top.entry_offset_len = MEMBER_OFFSET_LEN;
        top.bits = COUNTER_BITS;
        top.fill = Fill::Count;
        top.leaf = Some(LeafSpec {
            name: count.clone(),
            title: count.clone(),
            len: 1,
            len_type: 0,
            unsigned: false,
            id: -1,
            kind: -1,
            bits: 0,
            count: None,
        });
        top.children = items(&infos, &info, info.class_name(), name, &count, &[])?;
        Ok(top)
    }

    /// The branch's name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Appends one entry: an object, or a list of them.
    pub(crate) fn fill(&mut self, value: &Value, encoder: &mut Encoder<'_>) -> Result<()> {
        match value {
            Value::Object(o) => self.fill_object(o, encoder),
            Value::List(items) => {
                let objects: Vec<&DynObject> = items.iter().filter_map(Value::as_object).collect();
                self.fill_items(&objects, encoder)
            }
            v => Err(v.mismatch("an object")),
        }
    }

    fn start(&mut self) {
        if self.entry_offset_len > 0 {
            self.starts.push(self.data.len());
        }
    }

    fn fill_object(&mut self, obj: &DynObject, encoder: &mut Encoder<'_>) -> Result<()> {
        if let Fill::Member { path, element } = &self.fill {
            let chain = navigate(obj, path)?;
            let element = element.clone();
            self.start();
            self.write_member(&element, &chain, encoder)?;
        }
        for c in &mut self.children {
            c.fill_object(obj, encoder)?;
        }
        Ok(())
    }

    fn fill_items(&mut self, items: &[&DynObject], encoder: &mut Encoder<'_>) -> Result<()> {
        match &self.fill {
            Fill::Count => {
                self.start();
                let n = i32::try_from(items.len()).map_err(|_| unsupported("too many items"))?;
                self.data.extend_from_slice(&n.to_be_bytes());
                self.maximum = self.maximum.max(items.len());
            }
            Fill::Items { path, element } => {
                let (path, element) = (path.clone(), element.clone());
                self.start();
                for item in items {
                    let chain = navigate(item, &path)?;
                    self.write_member(&element, &chain, encoder)?;
                }
            }
            _ => {}
        }
        for c in &mut self.children {
            c.fill_items(items, encoder)?;
        }
        Ok(())
    }

    /// One member, as it is streamed in its object; `fBits` without the
    /// bits ROOT does not store; a counter raises `fMaximum`.
    fn write_member(
        &mut self,
        el: &StreamerElement,
        chain: &[&DynObject],
        encoder: &mut Encoder<'_>,
    ) -> Result<()> {
        let lookup = |name: &str| chain.iter().find_map(|o| o.get(name));
        if el.type_code() == 15 {
            let bits = lookup(el.name()).and_then(Value::as_i64).unwrap_or(0);
            let bits = u32::try_from(bits).unwrap_or(0) & !HEAP_BITS;
            self.data.extend_from_slice(&bits.to_be_bytes());
            return Ok(());
        }
        if el.type_code() == 6 {
            let n = lookup(el.name()).and_then(Value::as_i64).unwrap_or(0);
            self.maximum = self.maximum.max(usize::try_from(n).unwrap_or(0));
        }
        let mut w = WBuffer::new();
        encoder.member(&mut w, el, chain)?;
        self.data.extend_from_slice(&w.into_inner());
        Ok(())
    }

    /// Writes the buffered entries: a basket per branch that holds data.
    pub(crate) fn flush(
        &mut self,
        file: &mut WritableFile,
        dir: usize,
        tree: &str,
        first: i64,
        nev: i32,
    ) -> Result<()> {
        if !matches!(self.fill, Fill::None) {
            let starts = (self.entry_offset_len > 0).then_some(self.starts.as_slice());
            let size = match (&self.fill, starts) {
                (_, Some(s)) => {
                    OFFSETS_CAPACITY.max(i32::try_from(s.len() + 1).unwrap_or(i32::MAX))
                }
                (Fill::Member { element, .. }, None) => {
                    i32::try_from(fixed_size(element).unwrap_or(1)).unwrap_or(1)
                }
                _ => 4,
            };
            let written =
                file.append_basket(dir, &self.name, tree, &self.data, starts, nev, size)?;
            self.tot_bytes += i64::from(written.key_len) + i64::from(written.obj_len);
            self.zip_bytes += i64::from(written.nbytes);
            self.baskets.push(Basket {
                nbytes: i32::try_from(written.nbytes).unwrap_or(i32::MAX),
                entry: first,
                seek: i64::try_from(written.seek).unwrap_or(i64::MAX),
            });
            self.data.clear();
            self.starts.clear();
        }
        for c in &mut self.children {
            c.flush(file, dir, tree, first, nev)?;
        }
        Ok(())
    }

    /// Where every branch's buffers end, depth first, to undo an entry.
    pub(crate) fn mark(&self, out: &mut Vec<(usize, usize, usize)>) {
        out.push((self.data.len(), self.starts.len(), self.maximum));
        for c in &self.children {
            c.mark(out);
        }
    }

    /// Undoes what was appended after `mark`.
    pub(crate) fn truncate(&mut self, marks: &mut impl Iterator<Item = (usize, usize, usize)>) {
        if let Some((d, s, m)) = marks.next() {
            self.data.truncate(d);
            self.starts.truncate(s);
            self.maximum = m;
        }
        for c in &mut self.children {
            c.truncate(marks);
        }
    }

    /// Bytes buffered, to decide when a cluster is full.
    pub(crate) fn buffered(&self) -> usize {
        self.data.len()
            + 4 * self.starts.len()
            + self.children.iter().map(Node::buffered).sum::<usize>()
    }

    /// Bytes written, before and after compression.
    pub(crate) fn bytes(&self) -> (i64, i64) {
        self.children
            .iter()
            .fold((self.tot_bytes, self.zip_bytes), |(t, z), c| {
                let (ct, cz) = c.bytes();
                (t + ct, z + cz)
            })
    }

    fn at(&self, path: &[usize]) -> Option<&Node> {
        match path.split_first() {
            None => Some(self),
            Some((&i, rest)) => self.children.get(i)?.at(rest),
        }
    }

    fn leaf_object(&self, root: &Node) -> Option<DynObject> {
        let l = self.leaf.as_ref()?;
        let count = l
            .count
            .as_ref()
            .and_then(|p| root.at(p))
            .and_then(|n| n.leaf_object(root));
        Some(
            DynObject::new("TLeafElement")
                .with("fUniqueID", 0u32)
                .with("fBits", l.bits)
                .with("fName", l.name.as_str())
                .with("fTitle", l.title.as_str())
                .with("fLen", l.len)
                .with("fLenType", l.len_type)
                .with("fOffset", 0i32)
                .with("fIsRange", false)
                .with("fIsUnsigned", l.unsigned)
                .with(
                    "fLeafCount",
                    count.map_or(Value::Null, |c| Value::Object(Box::new(c))),
                )
                .with("fID", l.id)
                .with("fType", l.kind),
        )
    }

    /// The leaves, as `TTree::fLeaves` lists them: this branch's, then its
    /// branches'.
    pub(crate) fn leaves(&self, root: &Node, out: &mut Vec<Value>) {
        if let Some(l) = self.leaf_object(root) {
            out.push(Value::Object(Box::new(l)));
        }
        for c in &self.children {
            c.leaves(root, out);
        }
    }

    /// The `TBranchElement` object.
    pub(crate) fn branch_object(&self, root: &Node, entries: i64, compress: i32) -> DynObject {
        let holds = !matches!(self.fill, Fill::None);
        let nbaskets = self.baskets.len();
        let max_baskets = (nbaskets + 1).max(10);
        let mut bytes = vec![0i32; max_baskets];
        let mut first = vec![0i64; max_baskets];
        let mut seeks = vec![0i64; max_baskets];
        for (i, b) in self.baskets.iter().enumerate() {
            bytes[i] = b.nbytes;
            first[i] = b.entry;
            seeks[i] = b.seek;
        }
        if holds {
            first[nbaskets] = entries;
        }
        let children: Vec<Value> = self
            .children
            .iter()
            .map(|c| Value::Object(Box::new(c.branch_object(root, entries, compress))))
            .collect();
        let leaves: Vec<Value> = self
            .leaf_object(root)
            .map(|l| vec![Value::Object(Box::new(l))])
            .unwrap_or_default();
        let count = match (&self.counted_by, &self.branch_count) {
            (Some(parent), _) => Value::Opaque {
                class: format!("{}TBranchElement", streamer::REFERENCE),
                bytes: parent.as_bytes().to_vec(),
            },
            (None, Some(p)) => root.at(p).map_or(Value::Null, |n| {
                Value::Object(Box::new(n.branch_object(root, entries, compress)))
            }),
            (None, None) => Value::Null,
        };
        let n = |x: usize| i32::try_from(x).unwrap_or(i32::MAX);
        DynObject::new("TBranchElement")
            .with("fUniqueID", 0u32)
            .with("fBits", self.bits)
            .with("fName", self.name.as_str())
            .with("fTitle", self.title.as_str())
            .with("fFillColor", 0i16)
            .with("fFillStyle", 1001i16)
            .with("fCompress", compress)
            .with("fBasketSize", BASKET_SIZE)
            .with("fEntryOffsetLen", self.entry_offset_len)
            .with("fWriteBasket", n(nbaskets))
            .with("fEntryNumber", if holds { entries } else { 0 })
            .with(
                "fIOFeatures",
                DynObject::new("ROOT::TIOFeatures").with("fIOBits", 0u8),
            )
            .with("fOffset", 0i32)
            .with("fMaxBaskets", n(max_baskets))
            .with("fSplitLevel", self.split_level)
            .with("fEntries", entries)
            .with("fFirstEntry", 0i64)
            .with("fTotBytes", self.tot_bytes)
            .with("fZipBytes", self.zip_bytes)
            .with("fBranches", obj_array(children))
            .with("fLeaves", obj_array(leaves))
            // ROOT keeps a slot per basket written, and one for the next.
            .with(
                "fBaskets",
                obj_array(if holds {
                    vec![Value::Null; nbaskets + 1]
                } else {
                    Vec::new()
                }),
            )
            .with("fBasketBytes", Array::I32(bytes))
            .with("fBasketEntry", Array::I64(first))
            .with("fBasketSeek", Array::I64(seeks))
            .with("fFileName", "")
            .with("fClassName", self.class_name.as_str())
            .with("fParentName", self.parent_name.as_str())
            .with("fClonesName", self.clones_name.as_str())
            .with("fCheckSum", self.checksum)
            .with("fClassVersion", self.class_version)
            .with("fID", self.id)
            .with("fType", self.kind)
            .with("fStreamerType", self.streamer_type)
            .with("fMaximum", n(self.maximum))
            .with("fBranchCount", count)
            .with("fBranchCount2", Value::Null)
    }
}

fn obj_array(objects: Vec<Value>) -> DynObject {
    DynObject::new("TObjArray")
        .with("fUniqueID", 0u32)
        .with("fBits", 0u32)
        .with("fName", "")
        .with("fLowerBound", 0i32)
        .with("fObjects", Value::List(objects))
}

/// The objects owning a member: the object at `path`, then those it is a
/// base part of (whose members a missing base takes, as the encoder does).
fn navigate<'o>(obj: &'o DynObject, path: &[Step]) -> Result<Vec<&'o DynObject>> {
    let mut chain = vec![obj];
    for step in path {
        let here = chain[0];
        match step {
            Step::Base(class) => {
                if let Some(b) = here.base(class) {
                    chain.insert(0, b);
                }
            }
            Step::Member(name) => match here.get(name) {
                Some(Value::Object(o)) => chain = vec![o],
                _ => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("member {name} of {} is not an object", here.class()),
                    ));
                }
            },
        }
    }
    Ok(chain)
}

/// The branches of the members of `info` (a split object): `prefix` names
/// them; `top` for the members of the field's own class; `at` is the path
/// of child indices to the branch they go under, for counters.
#[expect(clippy::too_many_lines, reason = "one arm per kind of member")]
fn members(
    infos: &Infos<'_>,
    info: &StreamerInfo,
    prefix: &str,
    path: &[Step],
    top: bool,
    at: &[usize],
) -> Result<Vec<Node>> {
    let mut out: Vec<Node> = Vec::new();
    for (id, el) in info.elements().iter().enumerate() {
        let id = i32::try_from(id).unwrap_or(i32::MAX);
        let here: Vec<usize> = at.iter().copied().chain([out.len()]).collect();
        let node = match el.kind() {
            ElementKind::Base { .. } => {
                let base = infos.get(el.name())?;
                let name = format!("{prefix}{}", el.name());
                let mut node = Node::container(name.clone(), 1, id, el.type_code(), info, 98);
                // The bases of the field's class prefix their members with
                // their own name; deeper bases keep their parent's prefix.
                let inner = if top {
                    format!("{name}.")
                } else {
                    prefix.to_owned()
                };
                let mut p = path.to_vec();
                p.push(Step::Base(el.name().to_owned()));
                node.children = members(infos, &base, &inner, &p, false, &here)?;
                node
            }
            ElementKind::Object | ElementKind::ObjectAny => {
                let class = infos.get(el.class_name())?;
                let name = format!("{prefix}{}", el.name());
                let mut node = Node::container(name.clone(), 2, id, el.type_code(), info, 98);
                let mut p = path.to_vec();
                p.push(Step::Member(el.name().to_owned()));
                node.children = members(infos, &class, &format!("{name}."), &p, false, &here)?;
                node
            }
            ElementKind::Scalar(_)
            | ElementKind::FixedArray { .. }
            | ElementKind::VarArray { .. }
            | ElementKind::TString
            | ElementKind::StdString
            | ElementKind::Stl { .. } => {
                let base = format!("{prefix}{}", el.name());
                let (len_type, unsigned, len) = leaf_shape(el);
                let (name, title, leaf_name) = match el.kind() {
                    ElementKind::FixedArray { len, .. } => (
                        format!("{base}[{len}]"),
                        format!("{base}[{len}]"),
                        base.clone(),
                    ),
                    ElementKind::VarArray { count, .. } => (
                        base.clone(),
                        format!("{base}[{count}]"),
                        format!("{base}[{count}]"),
                    ),
                    _ => (base.clone(), base.clone(), base.clone()),
                };
                let counter = match el.kind() {
                    ElementKind::VarArray { count, .. } => {
                        let i = out
                            .iter()
                            .position(|n| n.name == format!("{prefix}{count}"))
                            .ok_or_else(|| unsupported(format!("no counter {count} for {base}")))?;
                        Some(at.iter().copied().chain([i]).collect::<Vec<usize>>())
                    }
                    _ => None,
                };
                let simple = matches!(
                    el.kind(),
                    ElementKind::Scalar(_)
                        | ElementKind::FixedArray { .. }
                        | ElementKind::VarArray { .. }
                );
                let mut node = Node::container(
                    name,
                    0,
                    id,
                    branch_type(el),
                    info,
                    if top && simple { 98 } else { 0 },
                );
                node.title.clone_from(&title);
                node.entry_offset_len = if fixed_size(el).is_some() {
                    0
                } else {
                    MEMBER_OFFSET_LEN
                };
                node.bits = MEMBER_BITS;
                node.branch_count.clone_from(&counter);
                node.fill = Fill::Member {
                    path: path.to_vec(),
                    element: el.clone(),
                };
                node.leaf = Some(LeafSpec {
                    name: leaf_name,
                    title,
                    len,
                    len_type,
                    unsigned,
                    id,
                    kind: branch_type(el),
                    bits: if counter.is_some() { VAR_LEAF_BITS } else { 0 },
                    count: counter,
                });
                node
            }
            _ => {
                return Err(unsupported(format!(
                    "member {} of {} ({}) cannot be written in a split branch yet",
                    el.name(),
                    info.class_name(),
                    el.type_name()
                )));
            }
        };
        out.push(node);
    }
    Ok(out)
}

/// The branches of the members of the elements of a collection: one per
/// member of `info` and of its bases (flattened).
fn items(
    infos: &Infos<'_>,
    info: &StreamerInfo,
    item_class: &str,
    prefix: &str,
    count: &str,
    path: &[Step],
) -> Result<Vec<Node>> {
    let mut out = Vec::new();
    for (id, el) in info.elements().iter().enumerate() {
        let id = i32::try_from(id).unwrap_or(i32::MAX);
        match el.kind() {
            ElementKind::Base { .. } => {
                let base = infos.get(el.name())?;
                let mut p = path.to_vec();
                p.push(Step::Base(el.name().to_owned()));
                out.extend(items(infos, &base, item_class, prefix, count, &p)?);
            }
            ElementKind::Scalar(_) | ElementKind::FixedArray { .. } => {
                let (len_type, unsigned, len) = leaf_shape(el);
                let base = format!("{prefix}.{}", el.name());
                let name = match el.kind() {
                    ElementKind::FixedArray { len, .. } => format!("{base}[{len}]"),
                    _ => base.clone(),
                };
                let title = format!("{}[{count}]", el.name());
                let mut node = Node::container(name, 41, id, el.type_code(), info, 0);
                node.title.clone_from(&title);
                item_class.clone_into(&mut node.parent_name);
                node.counted_by = Some(prefix.to_owned());
                node.entry_offset_len = MEMBER_OFFSET_LEN;
                node.bits = MEMBER_BITS;
                node.fill = Fill::Items {
                    path: path.to_vec(),
                    element: el.clone(),
                };
                node.leaf = Some(LeafSpec {
                    name: base,
                    title,
                    len,
                    len_type,
                    unsigned,
                    id,
                    kind: el.type_code(),
                    bits: 0,
                    count: Some(Vec::new()),
                });
                out.push(node);
            }
            _ => {
                return Err(unsupported(format!(
                    "member {} of {} ({}) cannot be written in a split collection yet: \
                     only numbers and arrays of them",
                    el.name(),
                    info.class_name(),
                    el.type_name()
                )));
            }
        }
    }
    Ok(out)
}
