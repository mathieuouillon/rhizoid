//! The class dictionary: [`StreamerInfo`] and its elements, with the
//! hand-written bootstrap codec for the `StreamerInfo` list.

use core::fmt;
use std::collections::HashMap;
use std::sync::Arc;

use crate::core::buffer::{RBuffer, WBuffer};
use crate::core::error::{Context, Diagnostic, Error, ErrorKind, Result, ResultExt};

use super::frame::{
    self, ClassMap, ClassTags, OnMismatch, TOBJECT_DEFAULT_BITS, Tag, begin_object, read_header,
    read_tag, read_tobject, write_tagged, write_tobject,
};

/// A basic value type, as stored in a member or an array.
#[derive(Copy, Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Scalar {
    /// `Bool_t` (1 byte).
    Bool,
    /// `Char_t`.
    I8,
    /// `UChar_t`.
    U8,
    /// `Short_t`.
    I16,
    /// `UShort_t`.
    U16,
    /// `Int_t` and counters.
    I32,
    /// `UInt_t` and `TObject` bits.
    U32,
    /// `Long_t` and `Long64_t` (always 8 bytes on disk).
    I64,
    /// `ULong_t` and `ULong64_t`.
    U64,
    /// `Float_t`.
    F32,
    /// `Double_t`.
    F64,
    /// `Float16_t`, with its packing.
    Float16(Packing),
    /// `Double32_t`, with its packing.
    Double32(Packing),
}

impl Scalar {
    fn from_code(code: i32, title: &str) -> Option<Self> {
        Some(match code {
            1 => Self::I8,
            2 => Self::I16,
            3 | 6 => Self::I32,
            4 | 16 => Self::I64,
            5 => Self::F32,
            8 => Self::F64,
            9 => Self::Double32(Packing::from_title(title, false)),
            11 => Self::U8,
            12 => Self::U16,
            13 | 15 => Self::U32,
            14 | 17 => Self::U64,
            18 => Self::Bool,
            19 => Self::Float16(Packing::from_title(title, true)),
            _ => return None,
        })
    }

    /// Maps a C++ type name to a scalar (for STL value types).
    pub(crate) fn from_type_name(name: &str) -> Option<Self> {
        Some(match name {
            "bool" | "Bool_t" => Self::Bool,
            "char" | "Char_t" | "signed char" | "int8_t" => Self::I8,
            "unsigned char" | "UChar_t" | "uint8_t" => Self::U8,
            "short" | "Short_t" | "int16_t" => Self::I16,
            "unsigned short" | "UShort_t" | "uint16_t" => Self::U16,
            "int" | "Int_t" | "int32_t" => Self::I32,
            "unsigned int" | "unsigned" | "UInt_t" | "uint32_t" => Self::U32,
            "long" | "Long_t" | "long long" | "Long64_t" | "int64_t" => Self::I64,
            "unsigned long" | "ULong_t" | "unsigned long long" | "ULong64_t" | "uint64_t" => {
                Self::U64
            }
            "float" | "Float_t" => Self::F32,
            "double" | "Double_t" => Self::F64,
            "Double32_t" => Self::Double32(Packing::None),
            "Float16_t" => Self::Float16(Packing::Bits(12)),
            _ => return None,
        })
    }

    /// Bytes on disk.
    pub(crate) fn size(self) -> usize {
        match self {
            Self::Bool | Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::I64 | Self::U64 | Self::F64 => 8,
            Self::Float16(p) | Self::Double32(p) => match p {
                Packing::None | Packing::Range { .. } => 4,
                Packing::Bits(_) => 3,
            },
        }
    }
}

/// How a `Float16_t` or `Double32_t` is stored.
#[derive(Copy, Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Packing {
    /// A plain `float` (`Double32_t` without range or bits).
    None,
    /// `u32(0.5 + factor·(x − min))`, clamped to `[min, max]`.
    Range {
        /// Lower bound.
        min: f64,
        /// Upper bound.
        max: f64,
        /// `2^bits / (max − min)`.
        factor: f64,
    },
    /// A 1-byte exponent and a mantissa truncated to this many bits.
    Bits(u32),
}

impl Packing {
    /// Ports `TStreamerElement::GetRange`: `[min, max]` or `[min, max, bits]`
    /// in the member's comment.
    pub(crate) fn from_title(title: &str, float16: bool) -> Self {
        let default = if float16 { Self::Bits(12) } else { Self::None };
        let Some((min, max, bits)) = parse_range(title) else {
            return default;
        };
        let bigint = if bits < 32 {
            f64::from(1_u32 << bits)
        } else {
            f64::from(u32::MAX)
        };
        if min < max {
            Self::Range {
                min,
                max,
                factor: bigint / (max - min),
            }
        } else if bits < 15 {
            Self::Bits(bits)
        } else {
            default
        }
    }
}

fn parse_range(title: &str) -> Option<(f64, f64, u32)> {
    fn bracket(s: &str) -> Option<&str> {
        let left = s.find('[')?;
        let rest = s.get(left + 1..)?;
        let right = rest.find(']')?;
        rest.get(..right)
    }
    let mut inner = bracket(title)?;
    if !inner.contains(',') {
        // The first bracket was an array dimension; the range is the next one.
        let after = title.get(title.find(']')? + 1..)?;
        inner = bracket(after)?;
        if !inner.contains(',') {
            return None;
        }
    }
    let mut parts = inner.split(',');
    let min = parse_bound(parts.next()?)?;
    let max = parse_bound(parts.next()?)?;
    let bits = match parts.next() {
        Some(b) => b
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|b| (2..=32).contains(b))
            .unwrap_or(32),
        None => 32,
    };
    Some((min, max, bits))
}

/// A bound: a number, or a multiple or fraction of `pi` as ROOT allows.
fn parse_bound(s: &str) -> Option<f64> {
    let s: String = s
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase();
    let Some(at) = s.find("pi") else {
        return s.parse().ok();
    };
    let (before, after) = (s.get(..at)?, s.get(at + 2..)?);
    let sign = if before.starts_with('-') { -1.0 } else { 1.0 };
    let factor = match before.trim_start_matches('-').trim_end_matches('*') {
        "" => 1.0,
        f => f.parse::<f64>().ok()?,
    };
    let divisor = match after.strip_prefix('/') {
        Some(d) => d.parse::<f64>().ok()?,
        None if after.is_empty() => 1.0,
        None => return None,
    };
    Some(sign * factor * core::f64::consts::PI / divisor)
}

/// What a member is, and so how it is streamed.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ElementKind {
    /// A base class, streamed by its own streamer, with its version.
    Base {
        /// The base class's version.
        version: i32,
    },
    /// A basic value.
    Scalar(Scalar),
    /// A fixed-size array (`Int_t fX[3]`), flattened.
    FixedArray {
        /// The element type.
        scalar: Scalar,
        /// The total number of values.
        len: u32,
    },
    /// An array sized by an earlier member (`//[fN]`), preceded by a
    /// 1-byte presence flag.
    VarArray {
        /// The element type.
        scalar: Scalar,
        /// The member holding the length.
        count: String,
    },
    /// An embedded object with its own header (`kObject`, `kTObject`,
    /// `kTNamed`), or a `//->` pointer (`kObjectp`), which is streamed the
    /// same way.
    Object,
    /// An embedded object of a non-`TObject` class (`kAny`, and `kAnyp`
    /// for `//->` pointers).
    ObjectAny,
    /// A pointer that may be null (`kObjectP`, `kAnyP`), written with a
    /// class tag.
    ObjectPointer,
    /// A `TString`.
    TString,
    /// A `std::string`.
    StdString,
    /// A `char*`: `i32` length and bytes.
    CharStar,
    /// An STL collection.
    Stl {
        /// The container (`vector`, `map`, …).
        container: String,
        /// The value type as written in the file.
        value_type: String,
    },
    /// A kind this version cannot stream (the type code is kept).
    Unsupported(i32),
}

/// The ROOT class of an element description (`TStreamerBase`, …).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ElementClass {
    Base,
    BasicType,
    BasicPointer,
    Loop,
    Object,
    ObjectPointer,
    ObjectAny,
    ObjectAnyPointer,
    String,
    Stl,
    StlString,
    Artificial,
}

impl ElementClass {
    const ALL: [Self; 12] = [
        Self::Base,
        Self::BasicType,
        Self::BasicPointer,
        Self::Loop,
        Self::Object,
        Self::ObjectPointer,
        Self::ObjectAny,
        Self::ObjectAnyPointer,
        Self::String,
        Self::Stl,
        Self::StlString,
        Self::Artificial,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Base => "TStreamerBase",
            Self::BasicType => "TStreamerBasicType",
            Self::BasicPointer => "TStreamerBasicPointer",
            Self::Loop => "TStreamerLoop",
            Self::Object => "TStreamerObject",
            Self::ObjectPointer => "TStreamerObjectPointer",
            Self::ObjectAny => "TStreamerObjectAny",
            Self::ObjectAnyPointer => "TStreamerObjectAnyPointer",
            Self::String => "TStreamerString",
            Self::Stl => "TStreamerSTL",
            Self::StlString => "TStreamerSTLstring",
            Self::Artificial => "TStreamerArtificial",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }
}

/// The `TObject` and `TNamed` part of a dictionary entry, kept so that a
/// description re-encodes byte for byte.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Named {
    version: i16,
    unique_id: u32,
    bits: u32,
    name: String,
    title: String,
}

impl Named {
    fn decode(r: &mut RBuffer<'_>) -> Result<Self> {
        let h = read_header(r)?;
        let (unique_id, bits) = read_tobject(r)?;
        let name = r.read_string()?;
        let title = r.read_string()?;
        frame::finish(r, h.end, "TNamed", OnMismatch::Fail, &mut Vec::new())?;
        Ok(Self {
            version: h.version,
            unique_id,
            bits,
            name,
            title,
        })
    }

    fn encode(&self, w: &mut WBuffer) -> Result<()> {
        let s = begin_object(w, self.version);
        write_tobject(w, self.unique_id, self.bits);
        w.write_string(&self.name)?;
        w.write_string(&self.title)?;
        w.patch_byte_count(s)
    }
}

/// One member of a class description (`TStreamerElement`).
#[derive(Clone, Debug, PartialEq)]
pub struct StreamerElement {
    class: ElementClass,
    version: i16,
    element_version: i16,
    stl_version: i16,
    named: Named,
    type_code: i32,
    size: i32,
    array_length: i32,
    array_dim: i32,
    max_index: [i32; 5],
    type_name: String,
    base_version: i32,
    count_version: i32,
    count_name: String,
    count_class: String,
    stl_type: i32,
    ctype: i32,
    kind: ElementKind,
}

impl StreamerElement {
    /// The member's name (for a base class, the base class's name).
    pub fn name(&self) -> &str {
        &self.named.name
    }

    /// The member's comment, which carries ranges and counts.
    pub fn title(&self) -> &str {
        &self.named.title
    }

    /// The C++ type name.
    pub fn type_name(&self) -> &str {
        &self.type_name
    }

    /// ROOT's type code (`TStreamerElement::fType`), with `Bool_t` members
    /// stored as `kUChar` reported as `kBool`, as ROOT does.
    pub fn type_code(&self) -> i32 {
        if self.type_code == 11 && matches!(self.type_name.as_str(), "Bool_t" | "bool") {
            18
        } else {
            self.type_code
        }
    }

    /// How the member is streamed.
    pub fn kind(&self) -> &ElementKind {
        &self.kind
    }

    /// Whether this is a base class (`TStreamerElement::IsBase`): a
    /// `TStreamerBase`, or an STL element named as its type (a class
    /// deriving from a collection).
    fn is_base(&self) -> bool {
        match self.class {
            ElementClass::Base => true,
            ElementClass::Stl => {
                let short = self.type_name.trim().trim_start_matches("std::");
                self.named.name == self.type_name || self.named.name == short
            }
            _ => false,
        }
    }

    /// Whether ROOT takes this `int` member for an enum: type code 3 under
    /// a type name that is not a known integer type.
    fn is_enum(&self) -> bool {
        self.type_code == 3
            && !matches!(
                self.type_name.as_str(),
                "int" | "Int_t" | "Ssiz_t" | "Seek_t" | "signed int" | "unsigned int"
            )
    }

    /// The type name ROOT hashes: collections without `std::` and default
    /// arguments, `long long` as `Long64_t`.
    fn checksum_type_name(&self) -> String {
        let t = self.type_name.trim();
        let is_stl = matches!(self.class, ElementClass::Stl | ElementClass::StlString)
            || [
                "vector<",
                "list<",
                "deque<",
                "set<",
                "map<",
                "multiset<",
                "multimap<",
            ]
            .iter()
            .any(|p| t.trim_start_matches("std::").starts_with(p));
        if is_stl {
            t.replace("std::", "")
                .replace("unsigned long long", "ULong64_t")
                .replace("long long", "Long64_t")
        } else {
            t.to_owned()
        }
    }

    /// The class named by an object, pointer or base member, without `*`.
    pub(crate) fn class_name(&self) -> &str {
        self.type_name.trim_end_matches('*').trim()
    }

    fn classify(&mut self) {
        self.kind = match self.class {
            ElementClass::Base => ElementKind::Base {
                version: self.base_version,
            },
            ElementClass::StlString => ElementKind::StdString,
            ElementClass::Stl => {
                let t = self.type_name.trim().trim_start_matches("std::");
                match (t.find('<'), t.rfind('>')) {
                    _ if t == "string" || self.type_code == 365 => ElementKind::StdString,
                    (Some(l), Some(r)) if l < r => ElementKind::Stl {
                        container: t.get(..l).unwrap_or_default().trim().to_owned(),
                        value_type: t.get(l + 1..r).unwrap_or_default().trim().to_owned(),
                    },
                    _ => ElementKind::Unsupported(self.type_code),
                }
            }
            _ => {
                let title = self.named.title.as_str();
                match self.type_code() {
                    7 => ElementKind::CharStar,
                    // Some writers mark fixed arrays only by fArrayLength,
                    // keeping the basic code (ROOT adds kOffsetL = 20).
                    c @ 1..=19 => match (
                        Scalar::from_code(c, title),
                        u32::try_from(self.array_length),
                    ) {
                        (Some(scalar), Ok(len)) if len > 1 => {
                            ElementKind::FixedArray { scalar, len }
                        }
                        (Some(scalar), _) => ElementKind::Scalar(scalar),
                        (None, _) => ElementKind::Unsupported(c),
                    },
                    c @ 21..=39 => match (
                        Scalar::from_code(c - 20, title),
                        u32::try_from(self.array_length),
                    ) {
                        (Some(scalar), Ok(len)) => ElementKind::FixedArray { scalar, len },
                        _ => ElementKind::Unsupported(c),
                    },
                    c @ 41..=59 => match Scalar::from_code(c - 40, title) {
                        Some(scalar) if !self.count_name.is_empty() => ElementKind::VarArray {
                            scalar,
                            count: self.count_name.clone(),
                        },
                        _ => ElementKind::Unsupported(c),
                    },
                    // `//->` pointers (kObjectp, kAnyp) are never null and
                    // are streamed in place, without a class tag.
                    61 | 63 | 66 | 67 => ElementKind::Object,
                    62 | 68 => ElementKind::ObjectAny,
                    64 | 69 => ElementKind::ObjectPointer,
                    65 => ElementKind::TString,
                    365 => ElementKind::StdString,
                    c => ElementKind::Unsupported(c),
                }
            }
        };
    }

    fn decode(r: &mut RBuffer<'_>, class_name: &str) -> Result<Self> {
        let class = ElementClass::from_name(class_name).ok_or_else(|| {
            Error::new(
                ErrorKind::Unsupported,
                format!("unknown streamer element class {class_name}"),
            )
        })?;
        let outer = read_header(r)?;
        let mut stl_version = 0;
        let has_element_header = !matches!(class, ElementClass::StlString);
        let inner_end = if has_element_header {
            None
        } else {
            // TStreamerSTLstring wraps a TStreamerSTL.
            let h = read_header(r)?;
            stl_version = h.version;
            h.end
        };
        let eh = read_header(r)?;
        let named = Named::decode(r)?;
        let type_code = r.read_i32()?;
        let size = r.read_i32()?;
        let array_length = r.read_i32()?;
        let array_dim = r.read_i32()?;
        let mut max_index = [0; 5];
        if eh.version == 1 {
            let n = r.read_len32("fMaxIndex length")?;
            for i in 0..n {
                let v = r.read_i32()?;
                if let Some(slot) = max_index.get_mut(usize::try_from(i).unwrap_or(usize::MAX)) {
                    *slot = v;
                }
            }
        } else {
            for slot in &mut max_index {
                *slot = r.read_i32()?;
            }
        }
        let type_name = r.read_string()?;
        frame::finish(
            r,
            eh.end,
            "TStreamerElement",
            OnMismatch::Fail,
            &mut Vec::new(),
        )?;
        let mut e = Self {
            class,
            version: outer.version,
            element_version: eh.version,
            stl_version,
            named,
            type_code,
            size,
            array_length,
            array_dim,
            max_index,
            type_name,
            base_version: 0,
            count_version: 0,
            count_name: String::new(),
            count_class: String::new(),
            stl_type: 0,
            ctype: 0,
            kind: ElementKind::Unsupported(type_code),
        };
        match class {
            ElementClass::Base if outer.version > 2 => e.base_version = r.read_i32()?,
            ElementClass::BasicPointer | ElementClass::Loop => {
                e.count_version = r.read_i32()?;
                e.count_name = r.read_string()?;
                e.count_class = r.read_string()?;
            }
            ElementClass::Stl | ElementClass::StlString => {
                e.stl_type = r.read_i32()?;
                e.ctype = r.read_i32()?;
            }
            _ => {}
        }
        frame::finish(
            r,
            inner_end,
            "TStreamerSTL",
            OnMismatch::Fail,
            &mut Vec::new(),
        )?;
        frame::finish(
            r,
            outer.end,
            class.name(),
            OnMismatch::Fail,
            &mut Vec::new(),
        )?;
        e.classify();
        Ok(e)
    }

    fn encode(&self, w: &mut WBuffer) -> Result<()> {
        let outer = begin_object(w, self.version);
        let inner =
            (self.class == ElementClass::StlString).then(|| begin_object(w, self.stl_version));
        let s = begin_object(w, self.element_version);
        self.named.encode(w)?;
        w.write_i32(self.type_code);
        w.write_i32(self.size);
        w.write_i32(self.array_length);
        w.write_i32(self.array_dim);
        if self.element_version == 1 {
            w.write_i32(5);
        }
        for v in self.max_index {
            w.write_i32(v);
        }
        w.write_string(&self.type_name)?;
        w.patch_byte_count(s)?;
        match self.class {
            ElementClass::Base if self.version > 2 => w.write_i32(self.base_version),
            ElementClass::BasicPointer | ElementClass::Loop => {
                w.write_i32(self.count_version);
                w.write_string(&self.count_name)?;
                w.write_string(&self.count_class)?;
            }
            ElementClass::Stl | ElementClass::StlString => {
                w.write_i32(self.stl_type);
                w.write_i32(self.ctype);
            }
            _ => {}
        }
        if let Some(i) = inner {
            w.patch_byte_count(i)?;
        }
        w.patch_byte_count(outer)
    }
}

/// Descriptions built for user classes, element by element, as ROOT 6.42
/// writes them (checked against ACLiC-compiled classes).
impl StreamerElement {
    fn blank(
        class: ElementClass,
        version: i16,
        name: &str,
        type_code: i32,
        size: i32,
        type_name: &str,
    ) -> Self {
        Self {
            class,
            version,
            element_version: 4,
            stl_version: 0,
            named: Named {
                version: 1,
                unique_id: 0,
                bits: 0,
                name: name.to_owned(),
                title: String::new(),
            },
            type_code,
            size,
            array_length: 0,
            array_dim: 0,
            max_index: [0; 5],
            type_name: type_name.to_owned(),
            base_version: 0,
            count_version: 0,
            count_name: String::new(),
            count_class: String::new(),
            stl_type: 0,
            ctype: 0,
            kind: ElementKind::Unsupported(type_code),
        }
    }

    fn done(mut self) -> Self {
        self.classify();
        self
    }

    /// ROOT's type code, size and type name of a scalar.
    pub(crate) fn scalar_info(s: Scalar) -> (i32, i32, &'static str) {
        match s {
            Scalar::Bool => (18, 1, "bool"),
            Scalar::I8 => (1, 1, "char"),
            Scalar::U8 => (11, 1, "unsigned char"),
            Scalar::I16 => (2, 2, "short"),
            Scalar::U16 => (12, 2, "unsigned short"),
            Scalar::I32 => (3, 4, "int"),
            Scalar::U32 => (13, 4, "unsigned int"),
            Scalar::I64 => (16, 8, "Long64_t"),
            Scalar::U64 => (17, 8, "ULong64_t"),
            Scalar::F32 | Scalar::Float16(_) => (5, 4, "float"),
            Scalar::F64 | Scalar::Double32(_) => (8, 8, "double"),
        }
    }

    /// A base class (`TStreamerBase`).
    pub(crate) fn new_base(class: &str, version: i32, title: &str) -> Self {
        let code = match class {
            "TObject" => 66,
            "TNamed" => 67,
            _ => 0,
        };
        let mut e = Self::blank(ElementClass::Base, 3, class, code, 0, "BASE");
        title.clone_into(&mut e.named.title);
        e.base_version = version;
        e.done()
    }

    /// A scalar member (`TStreamerBasicType`).
    pub(crate) fn new_scalar(name: &str, s: Scalar) -> Self {
        let (code, size, type_name) = Self::scalar_info(s);
        Self::blank(ElementClass::BasicType, 2, name, code, size, type_name).done()
    }

    /// An `int` member counting a `//[fN]` array (`kCounter`).
    pub(crate) fn new_counter(name: &str) -> Self {
        Self::blank(ElementClass::BasicType, 2, name, 6, 4, "int").done()
    }

    /// A fixed array of scalars (`T name[len]`).
    pub(crate) fn new_fixed_array(name: &str, s: Scalar, len: i32) -> Self {
        let (code, size, type_name) = Self::scalar_info(s);
        let mut e = Self::blank(
            ElementClass::BasicType,
            2,
            name,
            code + 20,
            size.saturating_mul(len),
            type_name,
        );
        e.array_length = len;
        e.array_dim = 1;
        e.max_index[0] = len;
        e.done()
    }

    /// A `//[count]` array of scalars (`T* name; //[count]`).
    pub(crate) fn new_var_array(
        name: &str,
        s: Scalar,
        count: &str,
        class: &str,
        version: i32,
    ) -> Self {
        let (code, size, type_name) = Self::scalar_info(s);
        let mut e = Self::blank(
            ElementClass::BasicPointer,
            2,
            name,
            code + 40,
            size,
            &format!("{type_name}*"),
        );
        e.named.title = format!("[{count}]");
        e.count_version = version;
        count.clone_into(&mut e.count_name);
        class.clone_into(&mut e.count_class);
        e.done()
    }

    /// A `TString` member.
    pub(crate) fn new_tstring(name: &str) -> Self {
        Self::blank(ElementClass::String, 2, name, 65, 24, "TString").done()
    }

    /// A `std::string` member.
    pub(crate) fn new_std_string(name: &str) -> Self {
        let mut e = Self::blank(ElementClass::StlString, 2, name, 500, 32, "string");
        e.stl_version = 3;
        e.stl_type = 365;
        e.ctype = 365;
        e.done()
    }

    /// A `std::vector<item>` member; `ctype` is the item's type code (61
    /// for strings and classes).
    pub(crate) fn new_vector(name: &str, item: &str, ctype: i32) -> Self {
        let mut e = Self::blank(
            ElementClass::Stl,
            3,
            name,
            500,
            24,
            &format!("vector<{item}>"),
        );
        e.stl_type = 1;
        e.ctype = ctype;
        e.done()
    }

    /// An object member of class `class`: `kObject` for classes deriving
    /// from `TObject`, else `kAny`.
    pub(crate) fn new_object(name: &str, class: &str, size: i32, tobject: bool) -> Self {
        let (kind, code) = if tobject {
            (ElementClass::Object, 61)
        } else {
            (ElementClass::ObjectAny, 62)
        };
        Self::blank(kind, 2, name, code, size, class).done()
    }

    /// The member's size in memory, as described.
    pub(crate) fn size(&self) -> i32 {
        self.size
    }
}

impl StreamerInfo {
    /// The description of a user class, with its checksum computed from
    /// it (`base_checksum` gives the checksums of the bases).
    pub(crate) fn new_class(
        format_version: i16,
        class: &str,
        version: i32,
        elements: Vec<StreamerElement>,
        base_checksum: impl Fn(&str, i32) -> Option<u32>,
    ) -> Self {
        let mut info = Self {
            version: format_version,
            named: Named {
                version: 1,
                unique_id: 0,
                bits: 0x0001_0000,
                name: class.to_owned(),
                title: String::new(),
            },
            checksum: 0,
            class_version: version,
            elements,
        };
        info.checksum = info.compute_checksum(base_checksum);
        info
    }
}

/// The counter of a `//[fN]` comment (`fN`): the text inside a bracket
/// that opens the comment, after spaces and `*`
/// (`TVirtualStreamerInfo::GetElementCounterStart`).
fn counter_of(title: &str) -> Option<&str> {
    let start = title.find(|c: char| c != '*' && !c.is_whitespace())?;
    let rest = title.get(start..)?.strip_prefix('[')?;
    rest.find(']').and_then(|end| rest.get(..end))
}

/// The description of one version of a class (`TStreamerInfo`).
#[derive(Clone, Debug, PartialEq)]
pub struct StreamerInfo {
    version: i16,
    named: Named,
    checksum: u32,
    class_version: i32,
    elements: Vec<StreamerElement>,
}

impl StreamerInfo {
    /// The class's name.
    pub fn class_name(&self) -> &str {
        &self.named.name
    }

    /// The class version described.
    pub fn class_version(&self) -> i32 {
        self.class_version
    }

    /// The class checksum.
    pub fn checksum(&self) -> u32 {
        self.checksum
    }

    /// The version of the description's own format (`TStreamerInfo`'s).
    pub(crate) fn format_version(&self) -> i16 {
        self.version
    }

    /// The members, bases first, in streaming order.
    pub fn elements(&self) -> &[StreamerElement] {
        &self.elements
    }

    /// The class checksum as ROOT computes it from a description
    /// (`TStreamerInfo::GetCheckSum`, latest variant), given the checksums
    /// of the base classes (`base_checksum(name, version)`; a base without
    /// one counts as 0).
    ///
    /// It hashes, with `id = 3·id + c`: the class name; each base's name
    /// and checksum; then each member's name, an extra 1 for enums, its
    /// type name, its array dimensions, and the counter of a `//[fN]`
    /// array. ROOT compares it with the checksum of its own dictionary
    /// before trusting a description.
    pub fn compute_checksum(&self, base_checksum: impl Fn(&str, i32) -> Option<u32>) -> u32 {
        fn hash(id: u32, s: &str) -> u32 {
            // ROOT hashes `char`s, which are signed.
            s.bytes().fold(id, |id, b| {
                id.wrapping_mul(3)
                    .wrapping_add(i32::from(b.cast_signed()).cast_unsigned())
            })
        }
        let class = self.class_name();
        let mut id = hash(0, class);
        let is_collection = class.starts_with("pair<")
            || [
                "vector<",
                "list<",
                "deque<",
                "set<",
                "multiset<",
                "map<",
                "multimap<",
            ]
            .iter()
            .any(|p| class.trim_start_matches("std::").starts_with(p));
        if !is_collection {
            for el in self.elements.iter().filter(|e| e.is_base()) {
                id = hash(id, el.name());
                if el.class == ElementClass::Base {
                    let sum = base_checksum(el.name(), el.base_version).unwrap_or(0);
                    id = id.wrapping_mul(3).wrapping_add(sum);
                }
            }
        }
        for el in self.elements.iter().filter(|e| !e.is_base()) {
            if el.is_enum() {
                id = id.wrapping_mul(3).wrapping_add(1);
            }
            id = hash(id, el.name());
            id = hash(id, &el.checksum_type_name());
            let dims = usize::try_from(el.array_dim.clamp(0, 5)).unwrap_or(0);
            for &m in el.max_index.iter().take(dims) {
                id = id.wrapping_mul(3).wrapping_add(m.cast_unsigned());
            }
            if let Some(counter) = counter_of(el.title()) {
                id = hash(id, counter);
            }
        }
        id
    }

    /// The classes this one needs: its bases and the classes of its object
    /// and pointer members.
    pub(crate) fn dependencies(&self) -> impl Iterator<Item = &str> {
        self.elements.iter().filter_map(|e| match e.kind {
            ElementKind::Base { .. } => Some(e.name()),
            ElementKind::Object | ElementKind::ObjectAny | ElementKind::ObjectPointer => {
                Some(e.class_name())
            }
            _ => None,
        })
    }

    fn decode(r: &mut RBuffer<'_>, classes: &mut ClassMap) -> Result<Self> {
        let header = read_header(r)?;
        let named = Named::decode(r)?;
        let checksum = r.read_u32()?;
        let class_version = r.read_i32()?;
        let elements = match read_tag(r, classes)? {
            Tag::Null => Vec::new(),
            Tag::Ref(_) => return Err(Error::malformed("shared element array")),
            Tag::Object { class, end, .. } if class == "TObjArray" => {
                let array = read_header(r)?;
                if array.version > 2 {
                    read_tobject(r)?;
                }
                if array.version > 1 {
                    r.read_string()?;
                }
                let n = r.read_len32("element count")?;
                r.read_i32()?; // fLowerBound
                let mut elements = Vec::new();
                for _ in 0..n {
                    match read_tag(r, classes)? {
                        Tag::Object { class, end, .. } => {
                            let element = StreamerElement::decode(r, &class)
                                .context(Context::Class(class.clone()))?;
                            frame::finish(r, end, &class, OnMismatch::Fail, &mut Vec::new())?;
                            elements.push(element);
                        }
                        Tag::Null | Tag::Ref(_) => {
                            return Err(Error::malformed("null or shared streamer element"));
                        }
                    }
                }
                frame::finish(r, array.end, "TObjArray", OnMismatch::Fail, &mut Vec::new())?;
                frame::finish(r, end, "TObjArray", OnMismatch::Fail, &mut Vec::new())?;
                elements
            }
            Tag::Object { class, .. } => {
                return Err(Error::malformed(format!(
                    "element list of class {class}, expected TObjArray"
                )));
            }
        };
        frame::finish(
            r,
            header.end,
            "TStreamerInfo",
            OnMismatch::Fail,
            &mut Vec::new(),
        )?;
        Ok(Self {
            version: header.version,
            named,
            checksum,
            class_version,
            elements,
        })
    }

    fn encode(&self, w: &mut WBuffer, tags: &mut ClassTags) -> Result<()> {
        let s = begin_object(w, self.version);
        self.named.encode(w)?;
        w.write_u32(self.checksum);
        w.write_i32(self.class_version);
        write_tagged(w, tags, "TObjArray", |w, tags| {
            let a = begin_object(w, 3);
            write_tobject(w, 0, TOBJECT_DEFAULT_BITS);
            w.write_string("")?;
            w.write_len32(
                u32::try_from(self.elements.len())
                    .map_err(|_| Error::malformed("too many elements"))?,
            )?;
            w.write_i32(0);
            for e in &self.elements {
                write_tagged(w, tags, e.class.name(), |w, _| e.encode(w))?;
            }
            w.patch_byte_count(a)
        })?;
        w.patch_byte_count(s)
    }
}

impl fmt::Display for StreamerInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{} (version {}, checksum {:#010x})",
            self.class_name(),
            self.class_version,
            self.checksum
        )?;
        for e in &self.elements {
            let what = match &e.kind {
                ElementKind::Base { version } => format!("base, version {version}"),
                k => format!("{k:?}"),
            };
            write!(f, "  {:<24} {:<28} {what}", e.type_name, e.name())?;
            if !e.title().is_empty() {
                write!(f, "  // {}", e.title())?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

/// Decodes the `StreamerInfo` list (a `TList` of `TStreamerInfo`, plus
/// optional schema rules, which are skipped).
pub(crate) fn decode_list(
    data: &[u8],
    origin: u32,
) -> Result<(Vec<StreamerInfo>, Vec<Diagnostic>)> {
    let mut r = RBuffer::new(data, origin);
    let mut diagnostics = Vec::new();
    let h = read_header(&mut r)?;
    read_tobject(&mut r)?;
    r.read_string()?;
    let n = r.read_len32("StreamerInfo count")?;
    let mut classes = ClassMap::default();
    let mut infos = Vec::new();
    for _ in 0..n {
        match read_tag(&mut r, &mut classes)? {
            Tag::Object { class, end, .. } if class == "TStreamerInfo" => {
                let info = StreamerInfo::decode(&mut r, &mut classes);
                match info {
                    Ok(info) => infos.push(info),
                    Err(e) => {
                        // One undecodable entry must not hide the others.
                        let Some(end) = end else { return Err(e) };
                        diagnostics.push(Diagnostic::new(
                            crate::DiagnosticKind::Irregular,
                            format!("skipped a class description: {e}"),
                            None,
                        ));
                        r.seek(end)?;
                    }
                }
                frame::finish(
                    &mut r,
                    end,
                    &class,
                    OnMismatch::Reposition,
                    &mut diagnostics,
                )?;
            }
            Tag::Object { end: Some(end), .. } => r.seek(end)?,
            Tag::Object {
                class, end: None, ..
            } => {
                return Err(Error::malformed(format!(
                    "entry of class {class} without a byte count"
                )));
            }
            Tag::Null | Tag::Ref(_) => {}
        }
        if h.version > 4 {
            r.read_string()?; // the link's option
        }
    }
    frame::finish(
        &mut r,
        h.end,
        "TList",
        OnMismatch::Reposition,
        &mut diagnostics,
    )?;
    Ok((infos, diagnostics))
}

/// Encodes a `StreamerInfo` list.
pub(crate) fn encode_list(infos: &[&StreamerInfo], origin: u32) -> Result<Vec<u8>> {
    let mut w = WBuffer::with_origin(origin);
    let mut tags = ClassTags::default();
    let s = begin_object(&mut w, 5);
    write_tobject(&mut w, 0, TOBJECT_DEFAULT_BITS);
    w.write_string("")?;
    w.write_len32(u32::try_from(infos.len()).map_err(|_| Error::malformed("too many classes"))?)?;
    for info in infos {
        write_tagged(&mut w, &mut tags, "TStreamerInfo", |w, tags| {
            info.encode(w, tags)
        })?;
        w.write_string("")?;
    }
    w.patch_byte_count(s)?;
    Ok(w.into_inner())
}

/// Appends descriptions to an encoded `StreamerInfo` list (a `TList`), whose
/// key header is `origin` bytes long: the old entries stay byte for byte,
/// at the same positions, so their class tags stay valid.
pub(crate) fn append_to_list(old: &[u8], infos: &[&StreamerInfo], origin: u32) -> Result<Vec<u8>> {
    let mut r = RBuffer::new(old, origin);
    let count = r.read_u32()?;
    if count & 0x4000_0000 == 0 {
        return Err(Error::malformed("StreamerInfo list without a byte count"));
    }
    r.read_i16()?; // TList version
    read_tobject(&mut r)?;
    r.read_string()?; // fName
    let n_at = r.position();
    let n = r.read_u32()?;
    let mut w = WBuffer::with_origin(origin);
    w.write_bytes(old);
    let added = u32::try_from(infos.len()).map_err(|_| Error::malformed("too many classes"))?;
    w.patch_u32(
        n_at,
        n.checked_add(added)
            .ok_or_else(|| Error::malformed("too many classes"))?,
    )?;
    let mut tags = ClassTags::default();
    for info in infos {
        write_tagged(&mut w, &mut tags, "TStreamerInfo", |w, tags| {
            info.encode(w, tags)
        })?;
        w.write_string("")?;
    }
    w.patch_byte_count(0)?;
    Ok(w.into_inner())
}

/// Class descriptions indexed by class name and version.
#[derive(Debug, Default)]
pub(crate) struct StreamerSet {
    infos: Vec<Arc<StreamerInfo>>,
    by_class: HashMap<String, Vec<usize>>,
}

impl StreamerSet {
    /// A set of descriptions already shared.
    pub(crate) fn from_arcs(infos: Vec<Arc<StreamerInfo>>) -> Self {
        let mut set = Self::default();
        for info in infos {
            set.by_class
                .entry(info.class_name().to_owned())
                .or_default()
                .push(set.infos.len());
            set.infos.push(info);
        }
        set
    }

    pub(crate) fn new(infos: Vec<StreamerInfo>) -> Self {
        let mut set = Self::default();
        for info in infos {
            set.by_class
                .entry(info.class_name().to_owned())
                .or_default()
                .push(set.infos.len());
            set.infos.push(Arc::new(info));
        }
        set
    }

    /// The description of `class` at `version`.
    pub(crate) fn get(&self, class: &str, version: i32) -> Option<&Arc<StreamerInfo>> {
        self.by_class
            .get(class)?
            .iter()
            .filter_map(|&i| self.infos.get(i))
            .find(|info| info.class_version == version)
    }

    /// The highest version of `class` described.
    pub(crate) fn latest(&self, class: &str) -> Option<&Arc<StreamerInfo>> {
        self.by_class
            .get(class)?
            .iter()
            .filter_map(|&i| self.infos.get(i))
            .max_by_key(|info| info.class_version)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Arc<StreamerInfo>> {
        self.infos.iter()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.infos.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_follow_get_range() {
        assert_eq!(Packing::from_title("no range", false), Packing::None);
        assert_eq!(Packing::from_title("no range", true), Packing::Bits(12));
        assert_eq!(Packing::from_title("[0,0,10]", false), Packing::Bits(10));
        assert_eq!(Packing::from_title("[0,0,20]", false), Packing::None);
        match Packing::from_title("energy [0, 100, 16]", false) {
            Packing::Range { min, max, factor } => {
                assert_eq!((min, max), (0.0, 100.0));
                assert!((factor - 655.36).abs() < 1e-9);
            }
            p => unreachable!("{p:?}"),
        }
        // A dimension bracket first, then the range.
        assert!(matches!(
            Packing::from_title("[3][-pi,pi]", false),
            Packing::Range { min, .. } if (min + core::f64::consts::PI).abs() < 1e-12
        ));
        assert_eq!(parse_bound("2*pi/4"), Some(core::f64::consts::PI / 2.0));
    }
}
