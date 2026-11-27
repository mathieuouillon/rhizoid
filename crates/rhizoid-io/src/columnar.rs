//! Columns: what reading a branch produces, shared by `TTree` and
//! (later) `RNTuple`.
//!
//! Reading untyped gives a [`Column`]. Reading typed picks the Rust type at
//! compile time ([`ColumnType`]: a [`NativeType`] such as `f32`, a fixed
//! array `[T; N]`, [`Jagged<T>`], or `String`) and checks once, when the
//! branch is opened, that it matches what the branch holds.

use core::fmt;
use core::marker::PhantomData;

use crate::core::error::{Error, ErrorKind, Result};
use crate::streamer::{Array, Scalar};

/// How the values of a branch are laid out.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Interpretation {
    /// One value per entry.
    Primitive(Scalar),
    /// `len` values per entry (`x[3]/F`).
    FixedArray {
        /// The value type.
        scalar: Scalar,
        /// Values per entry.
        len: usize,
    },
    /// A variable number of values per entry (`x[n]/F`), each made of
    /// `inner` values (`x[n][3]/F` has `inner = 3`).
    Jagged {
        /// The value type.
        scalar: Scalar,
        /// Values per element.
        inner: usize,
    },
    /// One string per entry (`TLeafC`, `TString`, `std::string`).
    Strings,
    /// One value per entry, decoded from the class description in the file:
    /// whole objects, and collections that are not flat (`vector<string>`,
    /// nested vectors, sets, maps).
    Objects {
        /// The C++ type of each entry.
        class: String,
    },
}

/// The type values are decoded to (`Double32_t` → `f64`, `Float16_t` → `f32`).
pub(crate) fn decoded(s: Scalar) -> Scalar {
    match s {
        Scalar::Double32(_) => Scalar::F64,
        Scalar::Float16(_) => Scalar::F32,
        s => s,
    }
}

fn scalar_name(s: Scalar) -> &'static str {
    match decoded(s) {
        Scalar::Bool => "bool",
        Scalar::I8 => "i8",
        Scalar::U8 => "u8",
        Scalar::I16 => "i16",
        Scalar::U16 => "u16",
        Scalar::I32 => "i32",
        Scalar::U32 => "u32",
        Scalar::I64 => "i64",
        Scalar::U64 => "u64",
        Scalar::F32 => "f32",
        _ => "f64",
    }
}

impl fmt::Display for Interpretation {
    /// As a Rust type: `f32`, `[f32; 3]`, `Jagged<f32>`, `String`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Primitive(s) => f.write_str(scalar_name(*s)),
            Self::FixedArray { scalar, len } => write!(f, "[{}; {len}]", scalar_name(*scalar)),
            Self::Jagged { scalar, inner: 1 } => write!(f, "Jagged<{}>", scalar_name(*scalar)),
            Self::Jagged { scalar, inner } => {
                write!(f, "Jagged<{}> (×{inner})", scalar_name(*scalar))
            }
            Self::Strings => f.write_str("String"),
            Self::Objects { class } => write!(f, "Value ({class})"),
        }
    }
}

/// The values of a branch over a range of entries.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Column {
    /// One value per entry.
    Values(Array),
    /// `len` values per entry, back to back.
    Fixed {
        /// Values per entry.
        len: usize,
        /// The values.
        values: Array,
    },
    /// Entry `i` holds `values[offsets[i]..offsets[i + 1]]`.
    Jagged {
        /// `entries + 1` offsets into `values`, starting at 0.
        offsets: Vec<usize>,
        /// The values.
        values: Array,
    },
    /// One string per entry.
    Strings(Vec<String>),
    /// One decoded value per entry.
    Objects(Vec<crate::Value>),
}

impl Column {
    /// The number of entries.
    pub fn len(&self) -> usize {
        match self {
            Self::Values(a) => a.len(),
            Self::Fixed { len, values } => values.len().checked_div(*len).unwrap_or(0),
            Self::Jagged { offsets, .. } => offsets.len().saturating_sub(1),
            Self::Strings(s) => s.len(),
            Self::Objects(v) => v.len(),
        }
    }

    /// Whether there are no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Variable-length entries, stored flat: entry `i` is
/// `values[offsets[i]..offsets[i + 1]]`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JaggedVec<T> {
    offsets: Vec<usize>,
    values: Vec<T>,
}

impl<T> JaggedVec<T> {
    pub(crate) fn from_parts(offsets: Vec<usize>, values: Vec<T>) -> Result<Self> {
        let ok = offsets.first() == Some(&0)
            && offsets.last() == Some(&values.len())
            && offsets.windows(2).all(|w| matches!(w, [a, b] if a <= b));
        if ok {
            Ok(Self { offsets, values })
        } else {
            Err(Error::malformed("inconsistent jagged offsets"))
        }
    }

    /// The number of entries.
    pub fn len(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    /// Whether there are no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Entry `i`.
    pub fn get(&self, i: usize) -> Option<&[T]> {
        let (&a, &b) = (self.offsets.get(i)?, self.offsets.get(i + 1)?);
        self.values.get(a..b)
    }

    /// The entries, in order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &[T]> + '_ {
        self.offsets.windows(2).map(|w| match w {
            [a, b] => self.values.get(*a..*b).unwrap_or_default(),
            _ => &[],
        })
    }

    /// Every value, all entries back to back.
    pub fn values(&self) -> &[T] {
        &self.values
    }

    /// The `len() + 1` entry boundaries.
    pub fn offsets(&self) -> &[usize] {
        &self.offsets
    }

    /// The offsets and the values.
    pub fn into_parts(self) -> (Vec<usize>, Vec<T>) {
        (self.offsets, self.values)
    }
}

impl<'a, T> IntoIterator for &'a JaggedVec<T> {
    type Item = &'a [T];
    type IntoIter = Box<dyn ExactSizeIterator<Item = &'a [T]> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

mod sealed {
    pub trait Sealed {}
}

/// A primitive type a column can hold. Sealed.
pub trait NativeType: sealed::Sealed + Copy + 'static {
    /// The on-disk type it reads.
    const SCALAR: Scalar;
    #[doc(hidden)]
    fn from_array(a: Array) -> Option<Vec<Self>>;
    /// Appends the value as stored (big-endian).
    #[doc(hidden)]
    fn put_be(self, out: &mut Vec<u8>);
}

macro_rules! native {
    ($($t:ty => $v:ident),*) => {$(
        impl sealed::Sealed for $t {}
        impl NativeType for $t {
            const SCALAR: Scalar = Scalar::$v;
            fn from_array(a: Array) -> Option<Vec<Self>> {
                match a { Array::$v(v) => Some(v), _ => None }
            }
            fn put_be(self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_be_bytes());
            }
        }
    )*};
}
native!(i8 => I8, u8 => U8, i16 => I16, u16 => U16, i32 => I32, u32 => U32,
    i64 => I64, u64 => U64, f32 => F32, f64 => F64);

impl sealed::Sealed for bool {}
impl NativeType for bool {
    const SCALAR: Scalar = Scalar::Bool;
    fn from_array(a: Array) -> Option<Vec<Self>> {
        match a {
            Array::Bool(v) => Some(v),
            _ => None,
        }
    }
    fn put_be(self, out: &mut Vec<u8>) {
        out.push(u8::from(self));
    }
}

/// Selects variable-length entries of `T`: `tree.branch::<Jagged<f32>>(…)`.
#[derive(Debug)]
pub struct Jagged<T>(PhantomData<T>);

/// What a branch can be read as. Sealed; implemented for every
/// [`NativeType`], `[T; N]`, [`Jagged<T>`], `String`, and
/// [`Value`](crate::Value) for branches of objects.
pub trait ColumnType: sealed::Sealed {
    /// What a read returns.
    type Output;
    #[doc(hidden)]
    fn accepts(i: &Interpretation) -> bool;
    #[doc(hidden)]
    fn describe() -> String;
    #[doc(hidden)]
    fn from_column(c: Column) -> Result<Self::Output>;
}

#[cold]
fn mismatch(c: &Column, wanted: &str) -> Error {
    let found = match c {
        Column::Values(a) | Column::Fixed { values: a, .. } | Column::Jagged { values: a, .. } => {
            a.type_name()
        }
        Column::Strings(_) => "strings",
        Column::Objects(_) => "objects",
    };
    Error::new(
        ErrorKind::TypeMismatch,
        format!("expected {wanted}, decoded {found}"),
    )
}

impl<T: NativeType> ColumnType for T {
    type Output = Vec<T>;
    fn accepts(i: &Interpretation) -> bool {
        matches!(i, Interpretation::Primitive(s) if decoded(*s) == T::SCALAR)
    }
    fn describe() -> String {
        scalar_name(T::SCALAR).to_owned()
    }
    fn from_column(c: Column) -> Result<Vec<T>> {
        match c {
            Column::Values(a) => {
                T::from_array(a).ok_or_else(|| Error::new(ErrorKind::TypeMismatch, "value type"))
            }
            c => Err(mismatch(&c, &Self::describe())),
        }
    }
}

impl<T: NativeType, const N: usize> sealed::Sealed for [T; N] {}
impl<T: NativeType, const N: usize> ColumnType for [T; N] {
    type Output = Vec<[T; N]>;
    fn accepts(i: &Interpretation) -> bool {
        matches!(i, Interpretation::FixedArray { scalar, len } if decoded(*scalar) == T::SCALAR && *len == N)
    }
    fn describe() -> String {
        format!("[{}; {N}]", scalar_name(T::SCALAR))
    }
    fn from_column(c: Column) -> Result<Vec<[T; N]>> {
        match c {
            Column::Fixed { len, values } if len == N => {
                let v = T::from_array(values)
                    .ok_or_else(|| Error::new(ErrorKind::TypeMismatch, "value type"))?;
                Ok(v.as_chunks::<N>().0.to_vec())
            }
            c => Err(mismatch(&c, &Self::describe())),
        }
    }
}

impl<T: NativeType> sealed::Sealed for Jagged<T> {}
impl<T: NativeType> ColumnType for Jagged<T> {
    type Output = JaggedVec<T>;
    fn accepts(i: &Interpretation) -> bool {
        matches!(i, Interpretation::Jagged { scalar, .. } if decoded(*scalar) == T::SCALAR)
    }
    fn describe() -> String {
        format!("Jagged<{}>", scalar_name(T::SCALAR))
    }
    fn from_column(c: Column) -> Result<JaggedVec<T>> {
        match c {
            Column::Jagged { offsets, values } => {
                let v = T::from_array(values)
                    .ok_or_else(|| Error::new(ErrorKind::TypeMismatch, "value type"))?;
                JaggedVec::from_parts(offsets, v)
            }
            c => Err(mismatch(&c, &Self::describe())),
        }
    }
}

impl sealed::Sealed for String {}
impl ColumnType for String {
    type Output = Vec<String>;
    fn accepts(i: &Interpretation) -> bool {
        matches!(i, Interpretation::Strings)
    }
    fn describe() -> String {
        "String".to_owned()
    }
    fn from_column(c: Column) -> Result<Vec<String>> {
        match c {
            Column::Strings(s) => Ok(s),
            c => Err(mismatch(&c, "String")),
        }
    }
}

impl sealed::Sealed for crate::Value {}
impl ColumnType for crate::Value {
    type Output = Vec<crate::Value>;
    fn accepts(i: &Interpretation) -> bool {
        matches!(i, Interpretation::Objects { .. })
    }
    fn describe() -> String {
        "Value".to_owned()
    }
    fn from_column(c: Column) -> Result<Vec<crate::Value>> {
        match c {
            Column::Objects(v) => Ok(v),
            c => Err(mismatch(&c, "Value")),
        }
    }
}

/// Appends `src` to `dst`; both must hold the same type.
pub(crate) fn extend_array(dst: &mut Array, src: Array) -> Result<()> {
    macro_rules! ext {
        ($($v:ident),*) => {
            match (dst, src) {
                $((Array::$v(d), Array::$v(s)) => { d.extend(s); Ok(()) })*
                _ => Err(Error::malformed("baskets of one branch decode to different types")),
            }
        };
    }
    ext!(Bool, I8, U8, I16, U16, I32, U32, I64, U64, F32, F64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jagged_vecs_check_their_offsets() {
        let j = JaggedVec::from_parts(vec![0, 2, 2, 3], vec![1, 2, 3]).expect("ok");
        assert_eq!(j.len(), 3);
        assert_eq!(j.get(0), Some(&[1, 2][..]));
        assert_eq!(j.get(1), Some(&[][..]));
        assert_eq!(j.iter().map(<[i32]>::len).collect::<Vec<_>>(), [2, 0, 1]);
        assert!(JaggedVec::from_parts(vec![0, 3, 2], vec![1, 2, 3]).is_err());
        assert!(JaggedVec::<i32>::from_parts(vec![1], vec![]).is_err());
    }

    #[test]
    fn types_accept_their_interpretations() {
        assert!(f32::accepts(&Interpretation::Primitive(Scalar::F32)));
        assert!(!f64::accepts(&Interpretation::Primitive(Scalar::F32)));
        assert!(<[f32; 3]>::accepts(&Interpretation::FixedArray {
            scalar: Scalar::F32,
            len: 3
        }));
        assert!(!<[f32; 2]>::accepts(&Interpretation::FixedArray {
            scalar: Scalar::F32,
            len: 3
        }));
        assert!(Jagged::<i32>::accepts(&Interpretation::Jagged {
            scalar: Scalar::I32,
            inner: 1
        }));
        assert!(String::accepts(&Interpretation::Strings));
        let fixed = Column::Fixed {
            len: 2,
            values: Array::F32(vec![1.0, 2.0, 3.0, 4.0]),
        };
        assert_eq!(
            <[f32; 2]>::from_column(fixed).expect("ok"),
            [[1.0, 2.0], [3.0, 4.0]]
        );
    }
}
