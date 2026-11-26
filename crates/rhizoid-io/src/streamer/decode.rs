//! The dynamic decoder: any class, interpreted from its description.

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::buffer::RBuffer;
use crate::core::error::{Context, Diagnostic, DiagnosticKind, Error, ErrorKind, Result};

use super::frame::{
    self, ClassMap, MEMBER_WISE, OnMismatch, Tag, read_header, read_tag, read_tobject,
};
use super::info::{ElementKind, Packing, Scalar, StreamerElement, StreamerInfo, StreamerSet};
use super::value::{Array, DynObject, Value};

/// Objects nested deeper than this are refused (untrusted input).
const MAX_DEPTH: u32 = 64;

/// Classes whose hand-written streamers this version does not implement.
/// Their objects are kept as [`Value::Opaque`] when they have a byte count.
const NOT_IMPLEMENTED: &[&str] = &[
    "TClonesArray",
    "TMap",
    "TRefArray",
    "TBits",
    "TExMap",
    "TProcessID",
    "TProcessUUID",
    "TDirectory",
    "TDirectoryFile",
    "TBasket",
];

/// The class of a `TArray*`, by element type.
pub(crate) fn tarray_scalar(class: &str) -> Option<Scalar> {
    Some(match class {
        "TArrayC" => Scalar::I8,
        "TArrayS" => Scalar::I16,
        "TArrayI" => Scalar::I32,
        "TArrayL" | "TArrayL64" => Scalar::I64,
        "TArrayF" => Scalar::F32,
        "TArrayD" => Scalar::F64,
        _ => return None,
    })
}

pub(crate) fn is_not_implemented(class: &str) -> bool {
    NOT_IMPLEMENTED.contains(&class)
}

/// Classes the decoder reads by hand but the encoder cannot write.
pub(crate) fn is_not_writable(class: &str) -> bool {
    is_not_implemented(class) || class == "TRef"
}

/// Decodes objects of one key.
pub(crate) struct Decoder<'s> {
    file: &'s StreamerSet,
    builtin: &'s StreamerSet,
    classes: ClassMap,
    objects: HashMap<u32, Value>,
    on_mismatch: OnMismatch,
    diagnostics: Vec<Diagnostic>,
    depth: u32,
}

impl<'s> Decoder<'s> {
    pub(crate) fn new(
        file: &'s StreamerSet,
        builtin: &'s StreamerSet,
        on_mismatch: OnMismatch,
    ) -> Self {
        Self {
            file,
            builtin,
            classes: ClassMap::default(),
            objects: HashMap::new(),
            on_mismatch,
            diagnostics: Vec::new(),
            depth: 0,
        }
    }

    pub(crate) fn into_diagnostics(self) -> Vec<Diagnostic> {
        self.diagnostics
    }

    /// The description of `class` at `version`: the file's own, else the
    /// built-in one. Foreign classes (version ≤ 0) take the newest.
    fn info(&self, class: &str, version: i16) -> Option<Arc<StreamerInfo>> {
        let v = i32::from(version);
        self.file
            .get(class, v)
            .or_else(|| self.builtin.get(class, v))
            .or_else(|| {
                (version <= 0).then(|| {
                    self.file
                        .latest(class)
                        .or_else(|| self.builtin.latest(class))
                })?
            })
            .cloned()
    }

    fn note(&mut self, r: &RBuffer<'_>, message: String) {
        self.diagnostics.push(Diagnostic::new(
            DiagnosticKind::Irregular,
            message,
            r.file_offset(),
        ));
    }

    /// An object of a known class, streamed by its class's streamer (the
    /// object of a key, a base, an embedded member).
    pub(crate) fn object(&mut self, r: &mut RBuffer<'_>, class: &str) -> Result<DynObject> {
        if self.depth >= MAX_DEPTH {
            return Err(Error::new(
                ErrorKind::LimitExceeded,
                format!("objects nested more than {MAX_DEPTH} deep"),
            ));
        }
        self.depth += 1;
        let out = self.object_inner(r, class);
        self.depth -= 1;
        out.map_err(|e| {
            if e.context().any(|c| matches!(c, Context::Class(_))) {
                e
            } else {
                e.with_context(Context::Class(class.to_owned()))
            }
        })
    }

    fn object_inner(&mut self, r: &mut RBuffer<'_>, class: &str) -> Result<DynObject> {
        if let Some(obj) = self.hand_written(r, class)? {
            return Ok(obj);
        }
        if is_not_implemented(class) {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!(
                    "class {class} has a hand-written streamer this version does not implement"
                ),
            ));
        }
        let h = read_header(r)?;
        let info = self.info(class, h.version).ok_or_else(|| {
            Error::new(
                ErrorKind::Unsupported,
                format!("no description of class {class} version {}", h.version),
            )
        })?;
        let mut obj = DynObject::with_version(class, h.version);
        for el in info.elements() {
            self.element(r, el, &mut obj)?;
        }
        frame::finish(r, h.end, class, self.on_mismatch, &mut self.diagnostics)?;
        Ok(obj)
    }

    /// The members of an object of `class` at `version`, without the
    /// object's header: how a `TTree` stores a whole unsplit object per
    /// entry (`FillLeavesMember`).
    pub(crate) fn object_members(
        &mut self,
        r: &mut RBuffer<'_>,
        class: &str,
        version: i16,
    ) -> Result<DynObject> {
        let info = self.info(class, version).ok_or_else(|| {
            Error::new(
                ErrorKind::Unsupported,
                format!("no description of class {class} version {version}"),
            )
        })?;
        let mut obj = DynObject::with_version(class, version);
        for el in info.elements() {
            self.element(r, el, &mut obj)
                .map_err(|e| e.with_context(Context::Class(class.to_owned())))?;
        }
        Ok(obj)
    }

    /// [`object`](Self::object) as a value, falling back to
    /// [`Value::Opaque`] for undecodable objects that carry a byte count.
    fn object_value(&mut self, r: &mut RBuffer<'_>, class: &str) -> Result<Value> {
        let start = r.position();
        match self.object(r, class) {
            Ok(o) => Ok(Value::Object(Box::new(o))),
            Err(e) if e.kind() == ErrorKind::Unsupported && tarray_scalar(class).is_none() => {
                r.seek(start)?;
                let Some(end) = read_header(r)?.end else {
                    return Err(e);
                };
                r.seek(end)?;
                self.note(r, format!("kept an object of class {class} undecoded: {e}"));
                Ok(Value::Opaque {
                    class: class.to_owned(),
                    bytes: r.since(start).to_vec(),
                })
            }
            Err(e) => Err(e),
        }
    }

    /// A pointed-to object, behind a class tag (`ReadObjectAny`).
    pub(crate) fn object_any(&mut self, r: &mut RBuffer<'_>) -> Result<Value> {
        match read_tag(r, &mut self.classes)? {
            Tag::Null => Ok(Value::Null),
            Tag::Ref(at) => {
                if let Some(v) = self.objects.get(&at) {
                    return Ok(v.clone());
                }
                self.note(
                    r,
                    format!("reference to an unknown object ({at}) read as null"),
                );
                Ok(Value::Null)
            }
            Tag::Object {
                class,
                position,
                end,
            } => {
                let start = r.position();
                let v = match (self.object(r, &class), end) {
                    (Ok(o), _) => Value::Object(Box::new(o)),
                    // Skip by the tag's byte count: some hand-written
                    // streamers (TBasket) write no header of their own.
                    (Err(e), Some(end)) if e.kind() == ErrorKind::Unsupported => {
                        r.seek(end)?;
                        self.note(r, format!("kept an object of class {class} undecoded: {e}"));
                        Value::Opaque {
                            class: class.clone(),
                            bytes: r.since(start).to_vec(),
                        }
                    }
                    (Err(e), _) => return Err(e),
                };
                frame::finish(r, end, &class, self.on_mismatch, &mut self.diagnostics)?;
                self.objects.insert(position, v.clone());
                Ok(v)
            }
        }
    }

    fn element(
        &mut self,
        r: &mut RBuffer<'_>,
        el: &StreamerElement,
        obj: &mut DynObject,
    ) -> Result<()> {
        let name = el.name();
        let value = match el.kind() {
            ElementKind::Base { .. } => {
                let base = self.object(r, name)?;
                obj.push_base(base);
                return Ok(());
            }
            ElementKind::Scalar(s) => read_scalar(r, *s)?,
            ElementKind::FixedArray { scalar, len } => {
                let n = usize::try_from(*len).map_err(|_| Error::malformed("array too long"))?;
                Value::Array(read_array(r, *scalar, n)?)
            }
            ElementKind::VarArray { scalar, count } => {
                if r.read_u8()? == 0 {
                    Value::Array(read_array(r, *scalar, 0)?)
                } else {
                    let n = obj.get(count).and_then(Value::as_i64).ok_or_else(|| {
                        Error::malformed(format!("count member {count} of {name} not read"))
                    })?;
                    let n = usize::try_from(n)
                        .map_err(|_| Error::malformed(format!("negative count {count} = {n}")))?;
                    Value::Array(read_array(r, *scalar, n)?)
                }
            }
            ElementKind::Object | ElementKind::ObjectAny => {
                self.object_value(r, el.class_name())?
            }
            ElementKind::ObjectPointer => self.object_any(r)?,
            // A std::string streamed as an STL member (kSTL 300, kStreamer
            // 500) has a byte count and version around it [RB§8.9].
            ElementKind::StdString if matches!(el.type_code(), 300 | 500) => {
                let h = read_header(r)?;
                let s = r.read_string()?;
                frame::finish(r, h.end, "string", self.on_mismatch, &mut self.diagnostics)?;
                Value::String(s)
            }
            ElementKind::TString | ElementKind::StdString => Value::String(r.read_string()?),
            ElementKind::CharStar => {
                let n = usize::try_from(r.read_len32("char* length")?)
                    .map_err(|_| Error::malformed("string too long"))?;
                Value::String(String::from_utf8_lossy(r.take(n)?).into_owned())
            }
            ElementKind::Stl {
                container,
                value_type,
            } => self.stl(r, container, value_type, true)?,
            // A custom-streamed member (kStreamer) or a pointer to an STL
            // collection (kSTLp) has a byte count and version around it
            // [RB§8.9]: keep its bytes.
            ElementKind::Unsupported(code @ (71 | 500)) => {
                let start = r.position();
                let h = read_header(r)?;
                let Some(end) = h.end else {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        format!("member {name} (type code {code}) without a byte count"),
                    ));
                };
                r.seek(end)?;
                self.note(
                    r,
                    format!("kept member {name} ({}) undecoded", el.type_name()),
                );
                Value::Opaque {
                    class: el.type_name().to_owned(),
                    bytes: r.since(start).to_vec(),
                }
            }
            ElementKind::Unsupported(code) => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    format!("member {name} has type code {code}, which this version cannot read"),
                ));
            }
        };
        obj.push_member(name, value);
        Ok(())
    }

    /// A value of a C++ type given by name: a scalar, a string, an STL
    /// collection (preceded by its own header when `header` is set), or an
    /// object of a class.
    ///
    /// Collections nested in a vector or set have no header; collections
    /// in a map's keys and values have one (as uproot reads them).
    pub(crate) fn value_of_type(
        &mut self,
        r: &mut RBuffer<'_>,
        type_name: &str,
        header: bool,
    ) -> Result<Value> {
        let t = type_name.trim().trim_start_matches("std::");
        if let Some(s) = Scalar::from_type_name(t) {
            return read_scalar(r, s);
        }
        if matches!(t, "string" | "TString") {
            return Ok(Value::String(r.read_string()?));
        }
        if let Some((container, args)) = split_template(t) {
            return self.stl(r, container, args, header);
        }
        self.object_value(r, t)
    }

    /// An STL collection: optional header, then the elements.
    fn stl(
        &mut self,
        r: &mut RBuffer<'_>,
        container: &str,
        args: &str,
        header: bool,
    ) -> Result<Value> {
        let start = r.position();
        let (end, member_wise) = if header {
            let h = read_header(r)?;
            (h.end, h.version & MEMBER_WISE != 0)
        } else {
            (None, false)
        };
        match self.stl_body(r, container, args, member_wise) {
            Ok(v) => {
                frame::finish(r, end, container, self.on_mismatch, &mut self.diagnostics)?;
                Ok(v)
            }
            Err(e) if e.kind() == ErrorKind::Unsupported => {
                let Some(end) = end else { return Err(e) };
                r.seek(end)?;
                let class = format!("{container}<{args}>");
                self.note(r, format!("kept a {class} undecoded: {e}"));
                Ok(Value::Opaque {
                    class,
                    bytes: r.since(start).to_vec(),
                })
            }
            Err(e) => Err(e),
        }
    }

    fn stl_body(
        &mut self,
        r: &mut RBuffer<'_>,
        container: &str,
        args: &str,
        member_wise: bool,
    ) -> Result<Value> {
        let unsupported = |what: String| Err(Error::new(ErrorKind::Unsupported, what));
        let parts = split_args(args);
        match (container, parts.as_slice()) {
            (
                "vector" | "list" | "deque" | "set" | "multiset" | "unordered_set"
                | "unordered_multiset",
                [item],
            ) => {
                if member_wise {
                    return unsupported(format!("member-wise std::{container}<{args}>"));
                }
                let n = usize::try_from(r.read_len32("collection size")?)
                    .map_err(|_| Error::malformed("collection too large"))?;
                if let Some(s) = Scalar::from_type_name(item) {
                    return Ok(Value::Array(read_array(r, s, n)?));
                }
                // Every element takes at least one byte: bound the allocation.
                let mut items = Vec::with_capacity(n.min(r.remaining()));
                for _ in 0..n {
                    items.push(self.value_of_type(r, item, false)?);
                }
                Ok(Value::List(items))
            }
            // A third argument is a comparator: it changes nothing on disk.
            ("map" | "multimap" | "unordered_map" | "unordered_multimap", [key, value, ..]) => {
                // Object-wise (a member of an object, as ROOT writes
                // `TFormula::fParams`): the size, then key, value, key, …
                if !member_wise {
                    let n = usize::try_from(r.read_len32("map size")?)
                        .map_err(|_| Error::malformed("map too large"))?;
                    let mut items = Vec::with_capacity(n.min(r.remaining()));
                    for _ in 0..n {
                        let k = self.value_of_type(r, key, false)?;
                        let v = self.value_of_type(r, value, false)?;
                        items.push(Value::List(vec![k, v]));
                    }
                    return Ok(Value::List(items));
                }
                // Member-wise (a branch of a tree): all keys, then all
                // values. A column of collections has one 6-byte header, and
                // its elements none (uproot's `AsMap`).
                r.skip(6)?; // the pair class's version and checksum
                let n = usize::try_from(r.read_len32("map size")?)
                    .map_err(|_| Error::malformed("map too large"))?;
                let mut column = |r: &mut RBuffer<'_>, t: &str| -> Result<Vec<Value>> {
                    let t0 = t.trim().trim_start_matches("std::");
                    if t0 == "string" || split_template(t0).is_some() {
                        r.skip(6)?;
                    }
                    let mut out = Vec::with_capacity(n.min(r.remaining()));
                    for _ in 0..n {
                        out.push(self.value_of_type(r, t, false)?);
                    }
                    Ok(out)
                };
                let keys = column(r, key)?;
                let values = column(r, value)?;
                Ok(Value::List(
                    keys.into_iter()
                        .zip(values)
                        .map(|(k, v)| Value::List(vec![k, v]))
                        .collect(),
                ))
            }
            // std::bitset<N>: the number of bits, then one byte per bit.
            ("bitset", [_]) => {
                let n = usize::try_from(r.read_len32("bitset size")?)
                    .map_err(|_| Error::malformed("bitset too large"))?;
                Ok(Value::Array(read_array(r, Scalar::Bool, n)?))
            }
            _ => unsupported(format!("std::{container}<{args}>")),
        }
    }

    /// Classes whose streamers ROOT writes by hand.
    fn hand_written(&mut self, r: &mut RBuffer<'_>, class: &str) -> Result<Option<DynObject>> {
        if let Some(s) = tarray_scalar(class) {
            let n = r.read_len32("array length")?;
            let values = read_array(
                r,
                s,
                usize::try_from(n).map_err(|_| Error::malformed("array too long"))?,
            )?;
            let n = i32::try_from(n).map_err(|_| Error::malformed("array too long"))?;
            return Ok(Some(
                DynObject::with_version(class, 1)
                    .with("fN", n)
                    .with("fArray", values),
            ));
        }
        Ok(Some(match class {
            "TObject" => {
                let (unique_id, bits) = read_tobject(r)?;
                DynObject::with_version(class, 1)
                    .with("fUniqueID", unique_id)
                    .with("fBits", bits)
            }
            // TRef: TObject (fUniqueID is the referenced object's id), then
            // the process-ID index.
            "TRef" => {
                let (unique_id, bits) = read_tobject(r)?;
                let pid = r.read_u16()?;
                DynObject::with_version(class, 1)
                    .with_base(
                        DynObject::with_version("TObject", 1)
                            .with("fUniqueID", unique_id)
                            .with("fBits", bits),
                    )
                    .with("fPID", pid)
            }
            "TList" | "THashList" | "TObjArray" => self.collection(r, class)?,
            _ => return Ok(None),
        }))
    }

    /// `TList`, `THashList` and `TObjArray`.
    fn collection(&mut self, r: &mut RBuffer<'_>, class: &str) -> Result<DynObject> {
        let h = read_header(r)?;
        let mut obj = DynObject::with_version(class, h.version);
        if h.version > 2 {
            let (unique_id, bits) = read_tobject(r)?;
            obj.push_base(
                DynObject::with_version("TObject", 1)
                    .with("fUniqueID", unique_id)
                    .with("fBits", bits),
            );
        }
        let name = if h.version > 1 {
            r.read_string()?
        } else {
            String::new()
        };
        let n = r.read_len32("collection size")?;
        let is_array = class == "TObjArray";
        let lower_bound = if is_array { r.read_i32()? } else { 0 };
        let n = usize::try_from(n).map_err(|_| Error::malformed("collection too large"))?;
        // Each entry takes at least four bytes: bound the allocation.
        let mut items = Vec::with_capacity(n.min(r.remaining() / 4));
        let mut options = Vec::new();
        for _ in 0..n {
            items.push(self.object_any(r)?);
            if !is_array && h.version > 4 {
                options.push(Value::String(r.read_string()?));
            }
        }
        frame::finish(r, h.end, class, self.on_mismatch, &mut self.diagnostics)?;
        obj.push_member("fName", Value::String(name));
        if is_array {
            obj.push_member("fLowerBound", Value::I32(lower_bound));
        }
        obj.push_member("fObjects", Value::List(items));
        if options
            .iter()
            .any(|o| o.as_str().is_some_and(|s| !s.is_empty()))
        {
            obj.push_member("fOptions", Value::List(options));
        }
        Ok(obj)
    }
}

fn read_scalar(r: &mut RBuffer<'_>, s: Scalar) -> Result<Value> {
    Ok(match s {
        Scalar::Bool => Value::Bool(r.read_u8()? != 0),
        Scalar::I8 => Value::I8(r.read_i8()?),
        Scalar::U8 => Value::U8(r.read_u8()?),
        Scalar::I16 => Value::I16(r.read_i16()?),
        Scalar::U16 => Value::U16(r.read_u16()?),
        Scalar::I32 => Value::I32(r.read_i32()?),
        Scalar::U32 => Value::U32(r.read_u32()?),
        Scalar::I64 => Value::I64(r.read_i64()?),
        Scalar::U64 => Value::U64(r.read_u64()?),
        Scalar::F32 => Value::F32(r.read_f32()?),
        Scalar::F64 => Value::F64(r.read_f64()?),
        Scalar::Float16(p) => Value::F32(narrow(read_packed(r, p)?)),
        Scalar::Double32(p) => Value::F64(read_packed(r, p)?),
    })
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "Float16_t is a float in memory"
)]
fn narrow(x: f64) -> f32 {
    x as f32
}

/// `ReadWithFactor` / `ReadWithNbits` / plain float [RB§6].
fn read_packed(r: &mut RBuffer<'_>, p: Packing) -> Result<f64> {
    Ok(match p {
        Packing::None => f64::from(r.read_f32()?),
        Packing::Range { min, factor, .. } => f64::from(r.read_u32()?) / factor + min,
        Packing::Bits(nbits) => {
            let exp = r.read_u8()?;
            let man = u32::from(r.read_u16()?);
            let mut bits = u32::from(exp) << 23;
            bits |= (man & ((1 << (nbits + 1)) - 1)) << (23 - nbits);
            let f = f32::from_bits(bits);
            f64::from(if man & (1 << (nbits + 1)) != 0 { -f } else { f })
        }
    })
}

/// Reads `n` values, checking first that they fit in what is left.
pub(crate) fn read_array(r: &mut RBuffer<'_>, s: Scalar, n: usize) -> Result<Array> {
    if n.checked_mul(s.size()).is_none_or(|b| b > r.remaining()) {
        return Err(Error::new(
            ErrorKind::UnexpectedEof,
            format!("array of {n} values runs past the end of the object"),
        ));
    }
    macro_rules! collect {
        ($variant:ident, $read:expr) => {{
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push($read);
            }
            Array::$variant(v)
        }};
    }
    Ok(match s {
        Scalar::Bool => collect!(Bool, r.read_u8()? != 0),
        Scalar::I8 => collect!(I8, r.read_i8()?),
        Scalar::U8 => collect!(U8, r.read_u8()?),
        Scalar::I16 => collect!(I16, r.read_i16()?),
        Scalar::U16 => collect!(U16, r.read_u16()?),
        Scalar::I32 => collect!(I32, r.read_i32()?),
        Scalar::U32 => collect!(U32, r.read_u32()?),
        Scalar::I64 => collect!(I64, r.read_i64()?),
        Scalar::U64 => collect!(U64, r.read_u64()?),
        Scalar::F32 => collect!(F32, r.read_f32()?),
        Scalar::F64 => collect!(F64, r.read_f64()?),
        Scalar::Float16(p) => collect!(F32, narrow(read_packed(r, p)?)),
        Scalar::Double32(p) => collect!(F64, read_packed(r, p)?),
    })
}

/// `vector<int>` → `("vector", "int")`.
pub(crate) fn split_template(t: &str) -> Option<(&str, &str)> {
    let l = t.find('<')?;
    let r = t.rfind('>')?;
    (l < r).then(|| {
        (
            t.get(..l).unwrap_or_default().trim(),
            t.get(l + 1..r).unwrap_or_default().trim(),
        )
    })
}

/// Splits template arguments at top-level commas.
pub(crate) fn split_args(args: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut from) = (0_i32, 0);
    for (i, c) in args.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                out.push(args.get(from..i).unwrap_or_default().trim());
                from = i + 1;
            }
            _ => {}
        }
    }
    out.push(args.get(from..).unwrap_or_default().trim());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_split_at_the_top_level() {
        assert_eq!(
            split_template("vector<vector<int> >"),
            Some(("vector", "vector<int>"))
        );
        assert_eq!(split_args("int,vector<short> "), ["int", "vector<short>"]);
        assert_eq!(
            split_args("map<int,short>, float"),
            ["map<int,short>", "float"]
        );
        assert_eq!(split_template("TLorentzVector"), None);
    }
}
