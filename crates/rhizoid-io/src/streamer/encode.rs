//! The dynamic encoder: a [`DynObject`] written by its class description.
//!
//! Members are matched to the description by name. A base class absent
//! from the object is built on the fly, and takes its members from the
//! derived object, so objects built in code can stay flat:
//! `DynObject::new("TH1D").with("fEntries", 3.0)` puts `fEntries` in `TH1`.
//! Members not set are written as zero, empty or null.

use std::collections::HashSet;
use std::ptr;
use std::sync::Arc;

use crate::core::buffer::WBuffer;
use crate::core::error::{Context, Error, ErrorKind, Result};

use super::decode::{is_not_writable, split_args, split_template, tarray_scalar};
use super::frame::{
    ClassTags, HasClassTags, MAP_OFFSET, TOBJECT_DEFAULT_BITS, begin_object, write_tagged,
    write_tobject,
};
use super::info::{ElementKind, Packing, Scalar, StreamerElement, StreamerInfo, StreamerSet};
use super::value::{Array, DynObject, Value, for_each_array};

/// The version in the header of an STL member: what ROOT 6.42 writes
/// (readers ignore it but for the member-wise bit).
const STL_VERSION: i16 = 10;

/// Classes with no `ClassDef` that ROOT writes as version 0 followed by
/// their checksum ("foreign" classes, `TBufferFile::WriteVersion`).
const FOREIGN: &[&str] = &["ROOT::TIOFeatures"];

/// Whether pointed-to objects of `class` are shared within a key, as ROOT
/// shares them by address: a tree's leaves sit in their branch's
/// `fLeaves` and again in `TTree::fLeaves` and `TLeaf::fLeafCount`; a
/// branch counting a variable array is in its parent's `fBranches` and in
/// `TBranchElement::fBranchCount`. The second time, a reference to the
/// first is written.
fn is_shared(class: &str) -> bool {
    class.starts_with("TLeaf") || class == "TBranchElement"
}

/// The class of a [`Value::Opaque`] standing for a reference to a shared
/// object of the same key written (or being written) earlier, named by
/// its bytes: a branch of a split collection refers to its counter, its
/// parent, through `fBranchCount`.
pub(crate) const REFERENCE: &str = "@reference:";

#[cold]
fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// Encodes the objects of one key.
/// What writing one object used.
pub(crate) struct Used {
    /// The classes written, in first-use order.
    pub(crate) classes: Vec<String>,
    /// The descriptions they were written with.
    pub(crate) infos: Vec<Arc<StreamerInfo>>,
    /// The descriptions of the file the object was read from.
    pub(crate) source: Option<Arc<StreamerSet>>,
}

pub(crate) struct Encoder<'s> {
    set: &'s StreamerSet,
    source: Option<Arc<StreamerSet>>,
    infos: Vec<Arc<StreamerInfo>>,
    tags: ClassTags,
    used: Vec<String>,
    consumed: HashSet<(usize, usize)>,
    /// Shared objects written so far, by class and name, with their map
    /// positions.
    shared: std::collections::HashMap<(String, String), Vec<(u32, DynObject)>>,
    /// Where each shared object starts, by class and name, from the moment
    /// it starts (references to it may come from inside it).
    positions: std::collections::HashMap<(String, String), u32>,
}

impl HasClassTags for Encoder<'_> {
    fn class_tags(&mut self) -> &mut ClassTags {
        &mut self.tags
    }
}

impl<'s> Encoder<'s> {
    pub(crate) fn new(set: &'s StreamerSet) -> Self {
        Self {
            set,
            source: None,
            infos: Vec::new(),
            tags: ClassTags::default(),
            used: Vec::new(),
            consumed: HashSet::new(),
            shared: std::collections::HashMap::new(),
            positions: std::collections::HashMap::new(),
        }
    }

    /// The classes written, in first-use order; the descriptions they were
    /// written with; and the descriptions of the file the object came from.
    pub(crate) fn into_used(self) -> Used {
        Used {
            classes: self.used,
            infos: self.infos,
            source: self.source,
        }
    }

    fn note_used(&mut self, class: &str) {
        if !self.used.iter().any(|c| c == class) {
            self.used.push(class.to_owned());
        }
    }

    /// Encodes the object of a key, and checks that every member of objects
    /// built in code was written.
    pub(crate) fn top(&mut self, w: &mut WBuffer, obj: &DynObject) -> Result<()> {
        self.source = obj.source().cloned();
        self.object(w, obj, &[])?;
        self.check_consumed(obj)
    }

    /// Marks every member of `obj` written.
    fn mark_consumed(&mut self, obj: &DynObject) {
        let at = ptr::from_ref(obj).addr();
        for (i, (_, value)) in obj.own_members().iter().enumerate() {
            self.consumed.insert((at, i));
            match value {
                Value::Object(o) => self.mark_consumed(o),
                Value::List(items) => {
                    for o in items.iter().filter_map(Value::as_object) {
                        self.mark_consumed(o);
                    }
                }
                _ => {}
            }
        }
        for b in obj.bases() {
            self.mark_consumed(b);
        }
    }

    fn check_consumed(&self, obj: &DynObject) -> Result<()> {
        let at = ptr::from_ref(obj).addr();
        for (i, (name, value)) in obj.own_members().iter().enumerate() {
            if obj.version() == 0 && !self.consumed.contains(&(at, i)) {
                return Err(invalid(format!(
                    "member {name} is not part of class {} as described",
                    obj.class()
                )));
            }
            match value {
                Value::Object(o) => self.check_consumed(o)?,
                Value::List(items) => {
                    for o in items.iter().filter_map(Value::as_object) {
                        self.check_consumed(o)?;
                    }
                }
                _ => {}
            }
        }
        obj.bases().iter().try_for_each(|b| self.check_consumed(b))
    }

    /// Writes one member of an object, as it is streamed in it (a member of
    /// a split branch); `chain` is the owning object, then those it is a
    /// base part of.
    pub(crate) fn member(
        &mut self,
        w: &mut WBuffer,
        el: &StreamerElement,
        chain: &[&DynObject],
    ) -> Result<()> {
        if self.source.is_none() {
            self.source = chain.first().and_then(|o| o.source().cloned());
        }
        self.element(w, el, chain)
    }

    /// Finds a member in the object or, failing that, in the derived objects
    /// it was built from, and marks it written.
    fn lookup<'o>(&mut self, name: &str, chain: &[&'o DynObject]) -> Option<&'o Value> {
        chain.iter().find_map(|o| {
            let i = o.own_members().iter().position(|(n, _)| n == name)?;
            self.consumed.insert((ptr::from_ref(*o).addr(), i));
            o.own_members().get(i).map(|(_, v)| v)
        })
    }

    /// The description to write `obj` with: the one it was read with (from
    /// its file's descriptions, else the crate's), else the newest known.
    fn info(&self, obj: &DynObject) -> Option<Arc<StreamerInfo>> {
        fn exact<'a>(
            s: &'a StreamerSet,
            class: &str,
            version: i32,
        ) -> Option<&'a Arc<StreamerInfo>> {
            (version > 0).then(|| s.get(class, version)).flatten()
        }
        let (class, version) = (obj.class(), i32::from(obj.version()));
        let source = self.source.as_deref();
        source
            .and_then(|s| exact(s, class, version))
            .or_else(|| exact(self.set, class, version))
            .or_else(|| source.and_then(|s| s.latest(class)))
            .or_else(|| self.set.latest(class))
            .cloned()
    }

    /// Writes `obj` with its class's streamer. `loose` are the objects it is
    /// a base part of, whose members it may take.
    pub(crate) fn object(
        &mut self,
        w: &mut WBuffer,
        obj: &DynObject,
        loose: &[&DynObject],
    ) -> Result<()> {
        let class = obj.class();
        let mut chain = Vec::with_capacity(loose.len() + 1);
        chain.push(obj);
        chain.extend_from_slice(loose);
        let out = self.object_inner(w, obj, &chain);
        out.map_err(|e| {
            if e.context().any(|c| matches!(c, Context::Class(_))) {
                e
            } else {
                e.with_context(Context::Class(class.to_owned()))
            }
        })
    }

    fn object_inner(
        &mut self,
        w: &mut WBuffer,
        obj: &DynObject,
        chain: &[&DynObject],
    ) -> Result<()> {
        let class = obj.class();
        if self.hand_written(w, class, chain)? {
            return Ok(());
        }
        if is_not_writable(class) {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("writing class {class} is not supported yet"),
            ));
        }
        let info = self.info(obj).ok_or_else(|| {
            Error::new(
                ErrorKind::Unsupported,
                format!("no description of class {class} is known, so it cannot be written"),
            )
        })?;
        self.note_used(class);
        if !self
            .infos
            .iter()
            .any(|i| i.class_name() == class && i.class_version() == info.class_version())
        {
            self.infos.push(Arc::clone(&info));
        }
        let version = i16::try_from(info.class_version())
            .map_err(|_| Error::malformed("class version out of range"))?;
        let s = if version <= 1 && FOREIGN.contains(&class) {
            let s = begin_object(w, 0);
            w.write_u32(info.checksum());
            s
        } else {
            begin_object(w, version)
        };
        for el in info.elements() {
            self.element(w, el, chain)?;
        }
        w.patch_byte_count(s)
    }

    #[expect(clippy::too_many_lines, reason = "one arm per kind of member")]
    fn element(
        &mut self,
        w: &mut WBuffer,
        el: &StreamerElement,
        chain: &[&DynObject],
    ) -> Result<()> {
        let name = el.name();
        let Some(&obj) = chain.first() else {
            return Err(Error::malformed("no object to write"));
        };
        let in_member = |e: Error| {
            if e.kind() == ErrorKind::InvalidInput && !e.to_string().contains("member ") {
                invalid(format!("member {name}: {e}"))
            } else {
                e
            }
        };
        match el.kind() {
            ElementKind::Base { .. } => match obj.bases().iter().find(|b| b.class() == name) {
                Some(b) => self.object(w, b, chain),
                None => self.object(w, &DynObject::new(name), chain),
            },
            ElementKind::Scalar(s) => {
                let v = self.lookup(name, chain);
                write_scalar(w, *s, v).map_err(in_member)
            }
            ElementKind::FixedArray { scalar, len } => {
                let v = self.lookup(name, chain);
                let n = usize::try_from(*len).map_err(|_| Error::malformed("array too long"))?;
                write_array(w, *scalar, v, Some(n)).map_err(in_member)
            }
            ElementKind::VarArray { scalar, count } => {
                let v = self.lookup(name, chain);
                let n = array_len(v).map_err(in_member)?;
                // The count may sit in a base of an object read from a file
                // (`fNpoints` of `TGraph`, for `TGraphErrors::fEX`).
                let found = self.lookup(count, chain);
                let found = found.or_else(|| chain.first().and_then(|o| o.get(count)));
                let declared = match found {
                    None | Some(Value::Null) => 0,
                    Some(c) => c
                        .as_i64()
                        .ok_or_else(|| invalid(format!("{count} is not an integer")))?,
                };
                if usize::try_from(declared).ok() != Some(n) {
                    return Err(invalid(format!(
                        "{name} has {n} values but {count} is {declared}"
                    )));
                }
                if n == 0 {
                    w.write_u8(0);
                    Ok(())
                } else {
                    w.write_u8(1);
                    write_array(w, *scalar, v, None).map_err(in_member)
                }
            }
            ElementKind::Object | ElementKind::ObjectAny => match self.lookup(name, chain) {
                None | Some(Value::Null) => self.object(w, &DynObject::new(el.class_name()), &[]),
                Some(Value::Object(o)) => self.object(w, o, &[]),
                Some(Value::Opaque { bytes, .. }) => {
                    w.write_bytes(bytes);
                    Ok(())
                }
                Some(v) => Err(invalid(format!(
                    "member {name}: expected an object, found {}",
                    v.type_name()
                ))),
            },
            ElementKind::ObjectPointer => {
                let v = self.lookup(name, chain);
                self.pointer(w, v).map_err(in_member)
            }
            // A std::string streamed as an STL member: byte count and
            // version around it, as ROOT writes it.
            ElementKind::StdString if matches!(el.type_code(), 300 | 500) => {
                let s = match self.lookup(name, chain) {
                    None | Some(Value::Null) => "",
                    Some(Value::String(s)) => s,
                    Some(v) => {
                        return Err(invalid(format!(
                            "member {name}: expected a string, found {}",
                            v.type_name()
                        )));
                    }
                };
                let start = begin_object(w, STL_VERSION);
                w.write_string(s)?;
                w.patch_byte_count(start)
            }
            ElementKind::TString | ElementKind::StdString => {
                let s = match self.lookup(name, chain) {
                    None | Some(Value::Null) => "",
                    Some(Value::String(s)) => s,
                    Some(v) => {
                        return Err(invalid(format!(
                            "member {name}: expected a string, found {}",
                            v.type_name()
                        )));
                    }
                };
                w.write_string(s)
            }
            ElementKind::CharStar => {
                let s = self
                    .lookup(name, chain)
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                w.write_len32(u32::try_from(s.len()).map_err(|_| invalid("string too long"))?)?;
                w.write_bytes(s.as_bytes());
                Ok(())
            }
            ElementKind::Stl {
                container,
                value_type,
            } => {
                let v = self.lookup(name, chain);
                self.stl(w, container, value_type, v, true)
                    .map_err(in_member)
            }
            ElementKind::Unsupported(code) => Err(Error::new(
                ErrorKind::Unsupported,
                format!("member {name} has type code {code}, which this version cannot write"),
            )),
        }
    }

    /// An STL collection, object-wise as ROOT writes members: with a header
    /// (byte count, version) unless nested in another collection, then the
    /// size and the elements. One kept undecoded is written as read.
    fn stl(
        &mut self,
        w: &mut WBuffer,
        container: &str,
        args: &str,
        v: Option<&Value>,
        header: bool,
    ) -> Result<()> {
        if let Some(Value::Opaque { bytes, .. }) = v {
            w.write_bytes(bytes);
            return Ok(());
        }
        let start = header.then(|| begin_object(w, STL_VERSION));
        let parts = split_args(args);
        let items: &[Value] = match v {
            None | Some(Value::Null) => &[],
            Some(Value::List(items)) => items,
            Some(Value::Array(a)) => {
                let item = parts.first().copied().unwrap_or_default();
                let s = Scalar::from_type_name(item.trim().trim_start_matches("std::"))
                    .ok_or_else(|| invalid(format!("an array for std::{container}<{args}>")))?;
                w.write_len32(u32::try_from(a.len()).map_err(|_| invalid("collection too long"))?)?;
                write_array(w, s, v, None)?;
                return start.map_or(Ok(()), |s| w.patch_byte_count(s));
            }
            Some(v) => {
                return Err(invalid(format!(
                    "expected a collection, found {}",
                    v.type_name()
                )));
            }
        };
        w.write_len32(u32::try_from(items.len()).map_err(|_| invalid("collection too long"))?)?;
        match (container, parts.as_slice()) {
            (
                "vector" | "list" | "deque" | "set" | "multiset" | "unordered_set"
                | "unordered_multiset" | "bitset",
                [item],
            ) => {
                for x in items {
                    self.stl_item(w, item, x)?;
                }
            }
            ("map" | "multimap" | "unordered_map" | "unordered_multimap", [key, value, ..]) => {
                for pair in items {
                    let Some([k, x]) = pair.as_list().and_then(|p| <&[Value; 2]>::try_from(p).ok())
                    else {
                        return Err(invalid("a map entry is not a [key, value] pair"));
                    };
                    self.stl_item(w, key, k)?;
                    self.stl_item(w, value, x)?;
                }
            }
            _ => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    format!("writing std::{container}<{args}> is not supported yet"),
                ));
            }
        }
        start.map_or(Ok(()), |s| w.patch_byte_count(s))
    }

    /// One element of an STL collection, as the decoder reads it.
    fn stl_item(&mut self, w: &mut WBuffer, type_name: &str, v: &Value) -> Result<()> {
        let t = type_name.trim().trim_start_matches("std::");
        if let Some(s) = Scalar::from_type_name(t) {
            return write_scalar(w, s, Some(v));
        }
        if matches!(t, "string" | "TString") {
            return match v {
                Value::String(s) => w.write_string(s),
                Value::Null => w.write_string(""),
                other => Err(invalid(format!(
                    "expected a string, found {}",
                    other.type_name()
                ))),
            };
        }
        if t.ends_with('*') {
            return self.pointer(w, Some(v));
        }
        if let Some((container, args)) = split_template(t) {
            return self.stl(w, container, args, Some(v), false);
        }
        match v {
            Value::Object(o) => self.object(w, o, &[]),
            Value::Opaque { bytes, .. } => {
                w.write_bytes(bytes);
                Ok(())
            }
            other => Err(invalid(format!(
                "expected an object of class {t}, found {}",
                other.type_name()
            ))),
        }
    }

    /// A pointer member or collection entry: null, or a tagged object.
    fn pointer(&mut self, w: &mut WBuffer, v: Option<&Value>) -> Result<()> {
        match v {
            None | Some(Value::Null) => {
                w.write_u32(0);
                Ok(())
            }
            Some(Value::Object(o)) if is_shared(o.class()) => {
                let id = (
                    o.class().to_owned(),
                    o.get("fName")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                );
                if let Some((at, _)) = self
                    .shared
                    .get(&id)
                    .and_then(|v| v.iter().find(|(_, s)| s == &**o))
                {
                    w.write_u32(*at);
                    // Its members were written the first time.
                    self.mark_consumed(o);
                    return Ok(());
                }
                let at = w.key_position()? + MAP_OFFSET;
                self.positions.entry(id.clone()).or_insert(at);
                write_tagged(w, self, o.class(), |w, me| me.object(w, o, &[]))?;
                self.shared.entry(id).or_default().push((at, (**o).clone()));
                Ok(())
            }
            Some(Value::Opaque { class, bytes }) if class.starts_with(REFERENCE) => {
                let id = (
                    class[REFERENCE.len()..].to_owned(),
                    String::from_utf8_lossy(bytes).into_owned(),
                );
                let at = self.positions.get(&id).copied().ok_or_else(|| {
                    invalid(format!(
                        "a reference to {} `{}`, not written before",
                        id.0, id.1
                    ))
                })?;
                w.write_u32(at);
                Ok(())
            }
            Some(Value::Object(o)) => {
                write_tagged(w, self, o.class(), |w, me| me.object(w, o, &[]))
            }
            Some(Value::Opaque { class, bytes }) => write_tagged(w, self, class, |w, _| {
                w.write_bytes(bytes);
                Ok(())
            }),
            Some(v) => Err(invalid(format!(
                "expected an object or null, found {}",
                v.type_name()
            ))),
        }
    }

    /// Classes whose streamers ROOT writes by hand.
    fn hand_written(&mut self, w: &mut WBuffer, class: &str, chain: &[&DynObject]) -> Result<bool> {
        if let Some(s) = tarray_scalar(class) {
            self.note_used(class);
            let v = self.lookup("fArray", chain);
            let _ = self.lookup("fN", chain);
            let n = array_len(v)?;
            w.write_len32(u32::try_from(n).map_err(|_| invalid("array too long"))?)?;
            write_array(w, s, v, None)?;
            return Ok(true);
        }
        match class {
            "TObject" => {
                self.note_used(class);
                let unique_id = match self.lookup("fUniqueID", chain) {
                    Some(v) => u32::try_from(v).map_err(|e| invalid(format!("fUniqueID: {e}")))?,
                    None => 0,
                };
                let bits = match self.lookup("fBits", chain) {
                    Some(v) => u32::try_from(v).map_err(|e| invalid(format!("fBits: {e}")))?,
                    None => TOBJECT_DEFAULT_BITS,
                };
                write_tobject(w, unique_id, bits);
                Ok(true)
            }
            "TList" | "THashList" | "TObjArray" => {
                self.note_used(class);
                self.collection(w, class, chain)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn collection(&mut self, w: &mut WBuffer, class: &str, chain: &[&DynObject]) -> Result<()> {
        let Some(&obj) = chain.first() else {
            return Err(Error::malformed("no object to write"));
        };
        let is_array = class == "TObjArray";
        let s = begin_object(w, if is_array { 3 } else { 5 });
        match obj.bases().iter().find(|b| b.class() == "TObject") {
            Some(b) => self.object(w, b, chain)?,
            None => self.object(w, &DynObject::new("TObject"), chain)?,
        }
        let name = self
            .lookup("fName", chain)
            .and_then(Value::as_str)
            .unwrap_or_default();
        w.write_string(name)?;
        let items = match self.lookup("fObjects", chain) {
            None | Some(Value::Null) => &[][..],
            Some(Value::List(items)) => items,
            Some(v) => {
                return Err(invalid(format!(
                    "fObjects: expected a list, found {}",
                    v.type_name()
                )));
            }
        };
        w.write_len32(u32::try_from(items.len()).map_err(|_| invalid("too many objects"))?)?;
        if is_array {
            let lower = self
                .lookup("fLowerBound", chain)
                .and_then(Value::as_i64)
                .unwrap_or(0);
            w.write_i32(i32::try_from(lower).map_err(|_| invalid("fLowerBound out of range"))?);
        }
        let options = self
            .lookup("fOptions", chain)
            .and_then(Value::as_list)
            .unwrap_or_default();
        for (i, item) in items.iter().enumerate() {
            self.pointer(w, Some(item))?;
            if !is_array {
                let option = options.get(i).and_then(Value::as_str).unwrap_or_default();
                w.write_string(option)?;
            }
        }
        w.patch_byte_count(s)
    }
}

fn array_len(v: Option<&Value>) -> Result<usize> {
    match v {
        None | Some(Value::Null) => Ok(0),
        Some(Value::Array(a)) => Ok(a.len()),
        Some(v) => Err(invalid(format!(
            "expected an array, found {}",
            v.type_name()
        ))),
    }
}

fn int<T: TryFrom<i64>>(v: &Value) -> Result<T> {
    v.as_i64()
        .and_then(|i| T::try_from(i).ok())
        .ok_or_else(|| invalid(format!("{v} does not fit the member's integer type")))
}

fn float(v: &Value) -> Result<f64> {
    v.as_f64()
        .ok_or_else(|| invalid(format!("expected a number, found {}", v.type_name())))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "Float_t members store floats"
)]
fn narrow(x: f64) -> f32 {
    x as f32
}

fn write_scalar(w: &mut WBuffer, s: Scalar, v: Option<&Value>) -> Result<()> {
    let zero = Value::I32(0);
    let v = match v {
        None | Some(Value::Null) => &zero,
        Some(v) => v,
    };
    match s {
        Scalar::Bool => w.write_u8(u8::from(
            TryInto::<bool>::try_into(v).map_err(|e| invalid(e.to_string()))?,
        )),
        Scalar::I8 => w.write_i8(int(v)?),
        Scalar::U8 => w.write_u8(int(v)?),
        Scalar::I16 => w.write_i16(int(v)?),
        Scalar::U16 => w.write_u16(int(v)?),
        Scalar::I32 => w.write_i32(int(v)?),
        Scalar::U32 => w.write_u32(int(v)?),
        Scalar::I64 => w.write_i64(int(v)?),
        Scalar::U64 => match v {
            Value::U64(u) => w.write_u64(*u),
            v => w.write_u64(int(v)?),
        },
        Scalar::F32 => w.write_f32(narrow(float(v)?)),
        Scalar::F64 => w.write_f64(float(v)?),
        Scalar::Float16(p) | Scalar::Double32(p) => write_packed(w, p, float(v)?),
    }
    Ok(())
}

/// `WriteWithFactor` / `WriteWithNbits` / plain float.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "ROOT's packing truncates to UInt_t by design"
)]
fn write_packed(w: &mut WBuffer, packing: Packing, value: f64) {
    match packing {
        Packing::None => w.write_f32(narrow(value)),
        Packing::Range { min, max, factor } => {
            let clamped = value.clamp(min, max);
            w.write_u32((0.5 + factor * (clamped - min)) as u32);
        }
        Packing::Bits(nbits) => {
            let single = narrow(value);
            let bits = single.to_bits();
            let exp = ((bits << 1) >> 24) & 0xff;
            let mut man = ((1 << (nbits + 1)) - 1) & (bits >> (23 - nbits - 1));
            man += 1;
            man >>= 1;
            if man & (1 << nbits) != 0 {
                man = (1 << nbits) - 1;
            }
            if single < 0.0 {
                man |= 1 << (nbits + 1);
            }
            w.write_u8(exp as u8);
            w.write_u16(man as u16);
        }
    }
}

/// Writes an array member: `expected` values for fixed arrays (zeros when
/// absent), all of them otherwise.
fn write_array(
    w: &mut WBuffer,
    s: Scalar,
    v: Option<&Value>,
    expected: Option<usize>,
) -> Result<()> {
    let array = match v {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(a),
        Some(v) => {
            return Err(invalid(format!(
                "expected an array, found {}",
                v.type_name()
            )));
        }
    };
    let Some(a) = array else {
        for _ in 0..expected.unwrap_or(0) {
            write_scalar(w, s, None)?;
        }
        return Ok(());
    };
    if let Some(e) = expected.filter(|&e| e != a.len()) {
        return Err(invalid(format!("expected {e} values, found {}", a.len())));
    }
    // Same type: write directly; otherwise convert value by value.
    match (s, a) {
        (Scalar::F64, Array::F64(v)) => v.iter().for_each(|&x| w.write_f64(x)),
        (Scalar::F32, Array::F32(v)) => v.iter().for_each(|&x| w.write_f32(x)),
        (Scalar::I32, Array::I32(v)) => v.iter().for_each(|&x| w.write_i32(x)),
        _ => {
            for_each_array!(a, values => {
                for x in values {
                    write_scalar(w, s, Some(&Value::from(*x)))?;
                }
            });
        }
    }
    Ok(())
}
