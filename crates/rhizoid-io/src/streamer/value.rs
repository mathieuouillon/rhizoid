//! Objects without a Rust model: [`DynObject`], [`Value`], [`Array`].

use super::StreamerSet;
use core::fmt;
use std::sync::Arc;

use crate::core::error::{Error, ErrorKind, Result};

/// An object of any class, decoded from the class description in the file.
///
/// Members are looked up by their on-disk names (`fEntries`), through base
/// classes too. To write one, build it with [`DynObject::new`] and
/// [`with`](DynObject::with): members not set are written as zero or empty,
/// and members are matched to the class description by name, whichever
/// base they belong to.
#[derive(Clone, PartialEq)]
pub struct DynObject {
    class: String,
    version: i16,
    bases: Vec<DynObject>,
    members: Vec<(String, Value)>,
    source: Source,
}

/// The class descriptions of the file an object was read from: writing it
/// back uses them, so classes the crate does not describe itself (a fitted
/// `TF1`) are written as the file had them. Not part of the object's value.
#[derive(Clone, Default)]
struct Source(Option<Arc<StreamerSet>>);

impl PartialEq for Source {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

// The derived output, without `source`, which is not part of the value.
#[allow(
    clippy::missing_fields_in_debug,
    reason = "`source` is bookkeeping, not part of the object"
)]
impl fmt::Debug for DynObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DynObject")
            .field("class", &self.class)
            .field("version", &self.version)
            .field("bases", &self.bases)
            .field("members", &self.members)
            .finish()
    }
}

impl DynObject {
    /// An empty object of `class`, to be written with the newest class
    /// description known.
    pub fn new(class: impl Into<String>) -> Self {
        Self {
            class: class.into(),
            version: 0,
            bases: Vec::new(),
            members: Vec::new(),
            source: Source::default(),
        }
    }

    /// The descriptions of the file the object was read from.
    pub(crate) fn source(&self) -> Option<&Arc<StreamerSet>> {
        self.source.0.as_ref()
    }

    /// Records the descriptions of the file the object was read from.
    pub(crate) fn set_source(&mut self, set: Arc<StreamerSet>) {
        self.source = Source(Some(set));
    }

    pub(crate) fn with_version(class: impl Into<String>, version: i16) -> Self {
        Self {
            version,
            ..Self::new(class)
        }
    }

    /// The class name.
    pub fn class(&self) -> &str {
        &self.class
    }

    /// The class version it was read with (0 for objects built in code).
    pub fn version(&self) -> i16 {
        self.version
    }

    /// The base-class parts, in streaming order.
    pub fn bases(&self) -> &[DynObject] {
        &self.bases
    }

    /// The base-class part of class `class`, searched through every level.
    pub fn base(&self, class: &str) -> Option<&DynObject> {
        self.bases.iter().find_map(|b| {
            if b.class == class {
                Some(b)
            } else {
                b.base(class)
            }
        })
    }

    /// This class's own members (not those of its bases), in streaming order.
    pub fn members(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.members.iter().map(|(n, v)| (n.as_str(), v))
    }

    /// A member by name, searched in this class, then in its bases.
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.members
            .iter()
            .find_map(|(n, v)| (n == name).then_some(v))
            .or_else(|| self.bases.iter().find_map(|b| b.get(name)))
    }

    /// A member by name, as [`get`](Self::get), or a `NotFound` error naming
    /// the class.
    ///
    /// # Errors
    ///
    /// `NotFound` if no member has that name.
    pub fn member(&self, name: &str) -> Result<&Value> {
        self.get(name).ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                format!("class {} has no member {name}", self.class),
            )
        })
    }

    fn get_mut(&mut self, name: &str) -> Option<&mut Value> {
        if let Some(i) = self.members.iter().position(|(n, _)| n == name) {
            return self.members.get_mut(i).map(|(_, v)| v);
        }
        self.bases.iter_mut().find_map(|b| b.get_mut(name))
    }

    /// Sets a member: replaces it where it is (in this class or a base), or
    /// adds it to this class.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<Value>) -> &mut Self {
        let name = name.into();
        let value = value.into();
        match self.get_mut(&name) {
            Some(slot) => *slot = value,
            None => self.members.push((name, value)),
        }
        self
    }

    /// [`set`](Self::set), by value, for building.
    #[must_use]
    pub fn with(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.set(name, value);
        self
    }

    /// Adds a base-class part, for building.
    #[must_use]
    pub fn with_base(mut self, base: DynObject) -> Self {
        self.bases.push(base);
        self
    }

    pub(crate) fn push_base(&mut self, base: DynObject) {
        self.bases.push(base);
    }

    pub(crate) fn push_member(&mut self, name: impl Into<String>, value: Value) {
        self.members.push((name.into(), value));
    }

    pub(crate) fn own_members(&self) -> &[(String, Value)] {
        &self.members
    }

    /// A numeric member as `f64`.
    ///
    /// # Errors
    ///
    /// `NotFound`, or `Malformed` if the member is not numeric.
    pub fn f64(&self, name: &str) -> Result<f64> {
        self.member(name)?.try_into()
    }

    /// A string member.
    ///
    /// # Errors
    ///
    /// `NotFound`, or `Malformed` if the member is not a string.
    pub fn str(&self, name: &str) -> Result<&str> {
        let v = self.member(name)?;
        v.as_str().ok_or_else(|| v.mismatch("a string"))
    }
}

/// A typed array, as stored in the file.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Array {
    /// `Bool_t[]`.
    Bool(Vec<bool>),
    /// `Char_t[]`.
    I8(Vec<i8>),
    /// `UChar_t[]`.
    U8(Vec<u8>),
    /// `Short_t[]`.
    I16(Vec<i16>),
    /// `UShort_t[]`.
    U16(Vec<u16>),
    /// `Int_t[]`.
    I32(Vec<i32>),
    /// `UInt_t[]`.
    U32(Vec<u32>),
    /// `Long64_t[]`.
    I64(Vec<i64>),
    /// `ULong64_t[]`.
    U64(Vec<u64>),
    /// `Float_t[]` (and `Float16_t[]`).
    F32(Vec<f32>),
    /// `Double_t[]` (and `Double32_t[]`).
    F64(Vec<f64>),
}

macro_rules! for_each_array {
    ($a:expr, $v:ident => $e:expr) => {
        match $a {
            Array::Bool($v) => $e,
            Array::I8($v) => $e,
            Array::U8($v) => $e,
            Array::I16($v) => $e,
            Array::U16($v) => $e,
            Array::I32($v) => $e,
            Array::U32($v) => $e,
            Array::I64($v) => $e,
            Array::U64($v) => $e,
            Array::F32($v) => $e,
            Array::F64($v) => $e,
        }
    };
}
pub(crate) use for_each_array;

impl Array {
    /// The number of values.
    pub fn len(&self) -> usize {
        for_each_array!(self, v => v.len())
    }

    /// Whether there are no values.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The value at `i` as `f64` (exact for every type but 64-bit integers
    /// beyond 2^53).
    #[expect(
        clippy::cast_precision_loss,
        reason = "documented: 64-bit integers convert like `as f64`"
    )]
    pub fn get_f64(&self, i: usize) -> Option<f64> {
        Some(match self {
            Self::Bool(v) => f64::from(u8::from(*v.get(i)?)),
            Self::I8(v) => f64::from(*v.get(i)?),
            Self::U8(v) => f64::from(*v.get(i)?),
            Self::I16(v) => f64::from(*v.get(i)?),
            Self::U16(v) => f64::from(*v.get(i)?),
            Self::I32(v) => f64::from(*v.get(i)?),
            Self::U32(v) => f64::from(*v.get(i)?),
            Self::I64(v) => *v.get(i)? as f64,
            Self::U64(v) => *v.get(i)? as f64,
            Self::F32(v) => f64::from(*v.get(i)?),
            Self::F64(v) => *v.get(i)?,
        })
    }

    /// The value at `i`.
    pub fn get(&self, i: usize) -> Option<Value> {
        Some(match self {
            Self::Bool(v) => Value::Bool(*v.get(i)?),
            Self::I8(v) => Value::I8(*v.get(i)?),
            Self::U8(v) => Value::U8(*v.get(i)?),
            Self::I16(v) => Value::I16(*v.get(i)?),
            Self::U16(v) => Value::U16(*v.get(i)?),
            Self::I32(v) => Value::I32(*v.get(i)?),
            Self::U32(v) => Value::U32(*v.get(i)?),
            Self::I64(v) => Value::I64(*v.get(i)?),
            Self::U64(v) => Value::U64(*v.get(i)?),
            Self::F32(v) => Value::F32(*v.get(i)?),
            Self::F64(v) => Value::F64(*v.get(i)?),
        })
    }

    /// All values as `f64`.
    pub fn to_f64_vec(&self) -> Vec<f64> {
        (0..self.len()).filter_map(|i| self.get_f64(i)).collect()
    }

    /// The element type's name, for messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Bool(_) => "bool",
            Self::I8(_) => "i8",
            Self::U8(_) => "u8",
            Self::I16(_) => "i16",
            Self::U16(_) => "u16",
            Self::I32(_) => "i32",
            Self::U32(_) => "u32",
            Self::I64(_) => "i64",
            Self::U64(_) => "u64",
            Self::F32(_) => "f32",
            Self::F64(_) => "f64",
        }
    }
}

/// One member's value.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Value {
    /// A null pointer.
    Null,
    /// `Bool_t`.
    Bool(bool),
    /// `Char_t`.
    I8(i8),
    /// `UChar_t`.
    U8(u8),
    /// `Short_t`.
    I16(i16),
    /// `UShort_t`.
    U16(u16),
    /// `Int_t`.
    I32(i32),
    /// `UInt_t`.
    U32(u32),
    /// `Long64_t`.
    I64(i64),
    /// `ULong64_t`.
    U64(u64),
    /// `Float_t` (and `Float16_t`).
    F32(f32),
    /// `Double_t` (and `Double32_t`).
    F64(f64),
    /// `TString`, `std::string` or `char*`.
    String(String),
    /// An array of a basic type.
    Array(Array),
    /// A collection of values (`TList`, `std::vector<T>` of non-basic `T`, …).
    List(Vec<Value>),
    /// An object.
    Object(Box<DynObject>),
    /// A member this version cannot decode, kept as its bytes so that it can
    /// be written back unchanged.
    Opaque {
        /// The member's class or type name.
        class: String,
        /// The streamed bytes, header included.
        bytes: Vec<u8>,
    },
}

impl Value {
    /// The variant's name, for messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "bool",
            Self::I8(_) => "i8",
            Self::U8(_) => "u8",
            Self::I16(_) => "i16",
            Self::U16(_) => "u16",
            Self::I32(_) => "i32",
            Self::U32(_) => "u32",
            Self::I64(_) => "i64",
            Self::U64(_) => "u64",
            Self::F32(_) => "f32",
            Self::F64(_) => "f64",
            Self::String(_) => "string",
            Self::Array(_) => "array",
            Self::List(_) => "list",
            Self::Object(_) => "object",
            Self::Opaque { .. } => "opaque",
        }
    }

    /// A number as `f64` (integers beyond 2^53 lose precision).
    #[expect(
        clippy::cast_precision_loss,
        reason = "documented: 64-bit integers convert like `as f64`"
    )]
    pub fn as_f64(&self) -> Option<f64> {
        Some(match *self {
            Self::Bool(v) => f64::from(u8::from(v)),
            Self::I8(v) => f64::from(v),
            Self::U8(v) => f64::from(v),
            Self::I16(v) => f64::from(v),
            Self::U16(v) => f64::from(v),
            Self::I32(v) => f64::from(v),
            Self::U32(v) => f64::from(v),
            Self::I64(v) => v as f64,
            Self::U64(v) => v as f64,
            Self::F32(v) => f64::from(v),
            Self::F64(v) => v,
            _ => return None,
        })
    }

    /// An integer (or `bool`) as `i64`; `None` for floats and for `u64`
    /// values above `i64::MAX`.
    pub fn as_i64(&self) -> Option<i64> {
        Some(match *self {
            Self::Bool(v) => i64::from(v),
            Self::I8(v) => i64::from(v),
            Self::U8(v) => i64::from(v),
            Self::I16(v) => i64::from(v),
            Self::U16(v) => i64::from(v),
            Self::I32(v) => i64::from(v),
            Self::U32(v) => i64::from(v),
            Self::I64(v) => v,
            Self::U64(v) => i64::try_from(v).ok()?,
            _ => return None,
        })
    }

    /// A string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// An array.
    pub fn as_array(&self) -> Option<&Array> {
        match self {
            Self::Array(a) => Some(a),
            _ => None,
        }
    }

    /// A list.
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Self::List(l) => Some(l),
            _ => None,
        }
    }

    /// An object.
    pub fn as_object(&self) -> Option<&DynObject> {
        match self {
            Self::Object(o) => Some(o),
            _ => None,
        }
    }

    /// Whether this is a null pointer.
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    #[cold]
    pub(crate) fn mismatch(&self, expected: &str) -> Error {
        Error::malformed(format!("expected {expected}, found {}", self.type_name()))
    }
}

macro_rules! value_from {
    ($($t:ty => $v:ident),* $(,)?) => {$(
        impl From<$t> for Value {
            fn from(v: $t) -> Self { Self::$v(v) }
        }
        impl From<Vec<$t>> for Array {
            fn from(v: Vec<$t>) -> Self { Self::$v(v) }
        }
        impl From<Vec<$t>> for Value {
            fn from(v: Vec<$t>) -> Self { Self::Array(Array::$v(v)) }
        }
    )*};
}
value_from!(bool => Bool, i8 => I8, u8 => U8, i16 => I16, u16 => U16, i32 => I32, u32 => U32,
    i64 => I64, u64 => U64, f32 => F32, f64 => F64);

impl From<String> for Value {
    fn from(v: String) -> Self {
        Self::String(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Self::String(v.to_owned())
    }
}

impl From<Array> for Value {
    fn from(v: Array) -> Self {
        Self::Array(v)
    }
}

impl From<DynObject> for Value {
    fn from(v: DynObject) -> Self {
        Self::Object(Box::new(v))
    }
}

impl From<Vec<Value>> for Value {
    fn from(v: Vec<Value>) -> Self {
        Self::List(v)
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map_or(Self::Null, Into::into)
    }
}

impl TryFrom<&Value> for f64 {
    type Error = Error;
    fn try_from(v: &Value) -> Result<Self> {
        v.as_f64().ok_or_else(|| v.mismatch("a number"))
    }
}

impl TryFrom<&Value> for i64 {
    type Error = Error;
    fn try_from(v: &Value) -> Result<Self> {
        v.as_i64().ok_or_else(|| v.mismatch("an integer"))
    }
}

macro_rules! try_from_int {
    ($($t:ty),*) => {$(
        impl TryFrom<&Value> for $t {
            type Error = Error;
            fn try_from(v: &Value) -> Result<Self> {
                v.as_i64()
                    .and_then(|i| <$t>::try_from(i).ok())
                    .ok_or_else(|| v.mismatch(concat!("an integer that fits ", stringify!($t))))
            }
        }
    )*};
}
try_from_int!(i32, u32, i16, u16, u8, usize);

impl TryFrom<&Value> for bool {
    type Error = Error;
    fn try_from(v: &Value) -> Result<Self> {
        match v {
            Value::Bool(b) => Ok(*b),
            _ => v
                .as_i64()
                .map(|i| i != 0)
                .ok_or_else(|| v.mismatch("a bool")),
        }
    }
}

impl TryFrom<&Value> for String {
    type Error = Error;
    fn try_from(v: &Value) -> Result<Self> {
        v.as_str()
            .map(str::to_owned)
            .ok_or_else(|| v.mismatch("a string"))
    }
}

impl TryFrom<&Value> for Vec<f64> {
    type Error = Error;
    fn try_from(v: &Value) -> Result<Self> {
        match v {
            Value::Array(a) => Ok(a.to_f64_vec()),
            Value::Null => Ok(Vec::new()),
            _ => Err(v.mismatch("an array")),
        }
    }
}

impl fmt::Display for Value {
    /// A compact rendering: arrays longer than 8 values are elided.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("null"),
            Self::Bool(v) => write!(f, "{v}"),
            Self::I8(v) => write!(f, "{v}"),
            Self::U8(v) => write!(f, "{v}"),
            Self::I16(v) => write!(f, "{v}"),
            Self::U16(v) => write!(f, "{v}"),
            Self::I32(v) => write!(f, "{v}"),
            Self::U32(v) => write!(f, "{v}"),
            Self::I64(v) => write!(f, "{v}"),
            Self::U64(v) => write!(f, "{v}"),
            Self::F32(v) => write!(f, "{v}"),
            Self::F64(v) => write!(f, "{v}"),
            Self::String(s) => write!(f, "{s:?}"),
            Self::Array(a) => {
                write!(f, "[{}; {}] [", a.type_name(), a.len())?;
                for i in 0..a.len().min(8) {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    for_each_array!(a, v => if let Some(x) = v.get(i) { write!(f, "{x}")?; });
                }
                if a.len() > 8 {
                    f.write_str(", …")?;
                }
                f.write_str("]")
            }
            Self::List(l) => write!(f, "list of {}", l.len()),
            Self::Object(o) => write!(f, "{}", o.class()),
            Self::Opaque { class, bytes } => {
                write!(f, "<{class}: {} undecoded bytes>", bytes.len())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn members_are_found_through_bases() {
        let named = DynObject::new("TNamed")
            .with("fName", "h")
            .with("fTitle", "t");
        let mut h = DynObject::new("TH1").with_base(named).with("fEntries", 3.0);
        assert_eq!(h.str("fName").expect("name"), "h");
        assert_eq!(h.f64("fEntries").expect("entries"), 3.0);
        h.set("fTitle", "new");
        assert_eq!(
            h.base("TNamed").and_then(|b| b.get("fTitle")),
            Some(&Value::from("new"))
        );
        assert_eq!(
            h.member("fNope").expect_err("missing").kind(),
            ErrorKind::NotFound
        );
        assert_eq!(
            i32::try_from(&Value::I64(1 << 40))
                .expect_err("overflow")
                .kind(),
            ErrorKind::Malformed
        );
    }
}
