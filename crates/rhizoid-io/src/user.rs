//! User classes and typed rows: the traits behind
//! `#[derive(RootObject)]` and `#[derive(Row)]`.
//!
//! - [`RootObject`]: a class with a description of its own. Values are
//!   written with it and read back from any version of the class: members
//!   are matched by name (and aliases), members missing from the file read
//!   as zero or empty, members the struct does not have are skipped, and
//!   numbers convert between types (schema evolution).
//! - [`RootType`]: what a member of such a class can be.
//! - [`Row`]: a typed row of a tree or an `RNTuple`, one field per column;
//!   implemented by `#[derive(Row)]` and by tuples of up to 16
//!   [`RowField`]s.

use core::fmt;
use std::sync::Arc;

use crate::columnar::{Column, ColumnType, Interpretation, Jagged, NativeType};
use crate::core::error::{Error, ErrorKind, Result};
use crate::objects::{TNamed, TObject};
use crate::streamer::{
    self, Array, FromDyn, RootClass, Scalar, StreamerElement, StreamerInfo, StreamerSet, ToDyn,
    Value,
};
use crate::tree::{LeafBuffer, LeafKind, LeafShape};

/// A class with a description of its own: what `#[derive(RootObject)]`
/// implements. [`FileScope::put`](crate::FileScope::put) writes it with
/// that description, [`File::get`](crate::File::get) reads it back from
/// any version of the class.
pub trait RootObject: RootClass + FromDyn + ToDyn {
    /// The class version (`ClassDef`'s), 1 to `0x3FFF`.
    const CLASS_VERSION: i16;
    /// Whether the class derives from `TObject` (it is then a `kObject`
    /// member of other classes, else `kAny`).
    const INHERITS_TOBJECT: bool;

    /// The class's description, with its checksum.
    fn streamer_info() -> Arc<StreamerInfo>;

    /// The descriptions of the user classes it contains (bases, members),
    /// its own excluded.
    #[doc(hidden)]
    fn __dependencies(out: &mut Vec<Arc<StreamerInfo>>) {
        let _ = out;
    }
}

/// What a member of a [`RootObject`] can be: a number, `bool`, `String`
/// (a `TString`, or a `std::string` with `#[root(std_string)]`), an array
/// `[T; N]` of numbers, a `Vec` of numbers, strings or classes
/// (`std::vector`), a `Vec` of numbers counted by another member
/// (`#[root(count = n)]`: `T* fX; //[fN]`), or another class.
#[diagnostic::on_unimplemented(
    message = "`{Self}` has no ROOT representation",
    note = "a member can be a number, `bool`, `String`, `[T; N]` of numbers, `Vec<T>` of numbers, strings or classes, or a type deriving `RootObject`"
)]
pub trait RootType: Sized {
    /// The member's description.
    #[doc(hidden)]
    fn __element(name: &str) -> StreamerElement;
    #[doc(hidden)]
    fn __to_value(&self) -> Value;
    /// Reads the member, converting numbers between types.
    #[doc(hidden)]
    fn __from_value(v: &Value) -> Result<Self>;
    /// The value of a member the file does not have.
    #[doc(hidden)]
    fn __missing() -> Self;
    #[doc(hidden)]
    fn __dependencies(out: &mut Vec<Arc<StreamerInfo>>) {
        let _ = out;
    }
}

mod sealed {
    pub trait Sealed {}
}

/// A number or `bool` as a member. Sealed.
pub trait RootScalar: sealed::Sealed + Copy + Default + 'static {
    /// The on-disk type.
    const SCALAR: Scalar;
    #[doc(hidden)]
    fn __value(self) -> Value;
    #[doc(hidden)]
    fn __from(v: &Value) -> Result<Self>;
    #[doc(hidden)]
    fn __array(v: Vec<Self>) -> Array;
}

fn mismatch(expected: &str, v: &Value) -> Error {
    Error::new(
        ErrorKind::TypeMismatch,
        format!("expected {expected}, found {}", v.type_name()),
    )
}

macro_rules! int_scalar {
    ($($t:ty => $v:ident),*) => {$(
        impl sealed::Sealed for $t {}
        impl RootScalar for $t {
            const SCALAR: Scalar = Scalar::$v;
            fn __value(self) -> Value {
                Value::$v(self)
            }
            fn __from(v: &Value) -> Result<Self> {
                let n = match v {
                    Value::U64(n) => i128::from(*n),
                    other => i128::from(other.as_i64().ok_or_else(|| mismatch("an integer", other))?),
                };
                <$t>::try_from(n).map_err(|_| {
                    Error::new(ErrorKind::TypeMismatch, format!("{n} does not fit {}", stringify!($t)))
                })
            }
            fn __array(v: Vec<Self>) -> Array {
                Array::$v(v)
            }
        }
    )*};
}

int_scalar!(i8 => I8, u8 => U8, i16 => I16, u16 => U16, i32 => I32, u32 => U32, i64 => I64, u64 => U64);

impl sealed::Sealed for bool {}
impl RootScalar for bool {
    const SCALAR: Scalar = Scalar::Bool;
    fn __value(self) -> Value {
        Value::Bool(self)
    }
    fn __from(v: &Value) -> Result<Self> {
        v.as_i64()
            .map(|n| n != 0)
            .ok_or_else(|| mismatch("a bool", v))
    }
    fn __array(v: Vec<Self>) -> Array {
        Array::Bool(v)
    }
}

impl sealed::Sealed for f64 {}
impl RootScalar for f64 {
    const SCALAR: Scalar = Scalar::F64;
    fn __value(self) -> Value {
        Value::F64(self)
    }
    fn __from(v: &Value) -> Result<Self> {
        v.as_f64().ok_or_else(|| mismatch("a number", v))
    }
    fn __array(v: Vec<Self>) -> Array {
        Array::F64(v)
    }
}

impl sealed::Sealed for f32 {}
impl RootScalar for f32 {
    const SCALAR: Scalar = Scalar::F32;
    fn __value(self) -> Value {
        Value::F32(self)
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "schema evolution narrows doubles to floats, as ROOT does"
    )]
    fn __from(v: &Value) -> Result<Self> {
        match v {
            Value::F32(x) => Ok(*x),
            other => other
                .as_f64()
                .map(|x| x as f32)
                .ok_or_else(|| mismatch("a number", other)),
        }
    }
    fn __array(v: Vec<Self>) -> Array {
        Array::F32(v)
    }
}

/// The scalars of an array value.
fn array_items<T: RootScalar>(v: &Value) -> Result<Vec<T>> {
    match v {
        Value::Array(a) => (0..a.len())
            .map(|i| a.get(i).map_or(Ok(T::default()), |x| T::__from(&x)))
            .collect(),
        Value::List(items) => items.iter().map(T::__from).collect(),
        Value::Null => Ok(Vec::new()),
        other => Err(mismatch("an array", other)),
    }
}

macro_rules! scalar_type {
    ($($t:ty),*) => {$(
        impl RootType for $t {
            fn __element(name: &str) -> StreamerElement {
                StreamerElement::new_scalar(name, <$t as RootScalar>::SCALAR)
            }
            fn __to_value(&self) -> Value {
                self.__value()
            }
            fn __from_value(v: &Value) -> Result<Self> {
                <$t as RootScalar>::__from(v)
            }
            fn __missing() -> Self {
                <$t>::default()
            }
        }
    )*};
}

scalar_type!(bool, i8, u8, i16, u16, i32, u32, i64, u64, f32, f64);

impl<T: RootScalar, const N: usize> RootType for [T; N] {
    fn __element(name: &str) -> StreamerElement {
        StreamerElement::new_fixed_array(name, T::SCALAR, i32::try_from(N).unwrap_or(i32::MAX))
    }
    fn __to_value(&self) -> Value {
        Value::Array(T::__array(self.to_vec()))
    }
    fn __from_value(v: &Value) -> Result<Self> {
        let mut items = array_items::<T>(v)?;
        items.resize(N, T::default());
        <[T; N]>::try_from(items).map_err(|_| Error::malformed("array length"))
    }
    fn __missing() -> Self {
        [T::default(); N]
    }
}

impl RootType for String {
    /// A `TString` (`#[root(std_string)]` makes it a `std::string`).
    fn __element(name: &str) -> StreamerElement {
        StreamerElement::new_tstring(name)
    }
    fn __to_value(&self) -> Value {
        Value::String(self.clone())
    }
    fn __from_value(v: &Value) -> Result<Self> {
        match v {
            Value::String(s) => Ok(s.clone()),
            Value::Null => Ok(String::new()),
            other => Err(mismatch("a string", other)),
        }
    }
    fn __missing() -> Self {
        String::new()
    }
}

/// What a `Vec` member (`std::vector`) can hold: numbers, strings
/// (`std::string`), or classes.
#[diagnostic::on_unimplemented(
    message = "a `Vec<{Self}>` member has no ROOT representation",
    note = "vectors can hold numbers, `bool`, `String`, or types deriving `RootObject`"
)]
pub trait VecItem: Sized {
    /// The item's C++ type name and `TStreamerSTL::fCtype`.
    #[doc(hidden)]
    fn __item() -> (String, i32);
    #[doc(hidden)]
    fn __vec_value(v: &[Self]) -> Value;
    #[doc(hidden)]
    fn __vec_from(v: &Value) -> Result<Vec<Self>>;
    #[doc(hidden)]
    fn __item_dependencies(out: &mut Vec<Arc<StreamerInfo>>) {
        let _ = out;
    }
}

impl<T: RootScalar> VecItem for T {
    fn __item() -> (String, i32) {
        let (code, _, name) = StreamerElement::scalar_info(T::SCALAR);
        (name.to_owned(), code)
    }
    fn __vec_value(v: &[Self]) -> Value {
        Value::Array(T::__array(v.to_vec()))
    }
    fn __vec_from(v: &Value) -> Result<Vec<Self>> {
        array_items(v)
    }
}

impl VecItem for String {
    fn __item() -> (String, i32) {
        ("string".to_owned(), 61)
    }
    fn __vec_value(v: &[Self]) -> Value {
        Value::List(v.iter().cloned().map(Value::String).collect())
    }
    fn __vec_from(v: &Value) -> Result<Vec<Self>> {
        match v {
            Value::List(items) => items.iter().map(String::__from_value).collect(),
            Value::Null => Ok(Vec::new()),
            other => Err(mismatch("a list of strings", other)),
        }
    }
}

impl<T: VecItem> RootType for Vec<T> {
    fn __element(name: &str) -> StreamerElement {
        let (item, ctype) = T::__item();
        StreamerElement::new_vector(name, &item, ctype)
    }
    fn __to_value(&self) -> Value {
        T::__vec_value(self)
    }
    fn __from_value(v: &Value) -> Result<Self> {
        T::__vec_from(v)
    }
    fn __missing() -> Self {
        Vec::new()
    }
    fn __dependencies(out: &mut Vec<Arc<StreamerInfo>>) {
        T::__item_dependencies(out);
    }
}

/// The only type of a member counting a `//[fN]` array: `i32`.
#[diagnostic::on_unimplemented(
    message = "a `count` member must be an `i32`, not `{Self}`",
    note = "ROOT counts `//[fN]` arrays with an `Int_t`"
)]
pub trait CountType {}
impl CountType for i32 {}

/// Bases and members of ROOT's own classes in user classes.
macro_rules! builtin_object {
    ($t:ty, $class:literal, $title:literal) => {
        impl RootObject for $t {
            const CLASS_VERSION: i16 = 1;
            const INHERITS_TOBJECT: bool = true;
            fn streamer_info() -> Arc<StreamerInfo> {
                streamer::builtin()
                    .latest($class)
                    .cloned()
                    .unwrap_or_else(|| {
                        Arc::new(StreamerInfo::new_class(
                            format_version(),
                            $class,
                            1,
                            Vec::new(),
                            |_, _| None,
                        ))
                    })
            }
        }

        impl RootType for $t {
            fn __element(name: &str) -> StreamerElement {
                StreamerElement::new_object(name, $class, 16, true)
            }
            fn __to_value(&self) -> Value {
                Value::Object(Box::new(self.to_dyn()))
            }
            fn __from_value(v: &Value) -> Result<Self> {
                match v {
                    Value::Object(o) => <$t>::from_dyn(o),
                    other => Err(mismatch(concat!("a ", $class), other)),
                }
            }
            fn __missing() -> Self {
                <$t>::default()
            }
        }

        impl BaseTitle for $t {
            const TITLE: &'static str = $title;
        }
    };
}

/// The title ROOT gives a base class element: the class's own title.
#[doc(hidden)]
pub trait BaseTitle {
    const TITLE: &'static str = "";
}

builtin_object!(TObject, "TObject", "Basic ROOT object");
builtin_object!(
    TNamed,
    "TNamed",
    "The basis for a named object (name, title)"
);

/// The format version of the descriptions ROOT writes.
fn format_version() -> i16 {
    streamer::builtin()
        .latest("TNamed")
        .map_or(9, |i| i.format_version())
}

/// A typed row of a tree or an `RNTuple`: one field per column. What
/// `#[derive(Row)]` implements, and tuples of up to 16 [`RowField`]s with
/// column names given at read time.
pub trait Row: Sized {
    /// The number of columns.
    #[doc(hidden)]
    const __WIDTH: usize;
    /// The columns, by name; empty for tuples.
    #[doc(hidden)]
    fn __names() -> Vec<&'static str>;
    /// Whether field `i` reads a column of that interpretation; if not,
    /// the type it needs.
    #[doc(hidden)]
    fn __check(i: usize, interpretation: &Interpretation) -> core::result::Result<(), String>;
    /// The rows of a batch, from its columns in field order.
    #[doc(hidden)]
    fn __rows(columns: Vec<Column>) -> Result<Vec<Self>>;
    /// How each field is written to a tree; `None` for a field that cannot be.
    #[doc(hidden)]
    fn __shapes() -> Vec<Option<LeafShape>>;
    /// The counter each field of variable length is written with, when
    /// named (`#[row(count = "nMuon")]`); empty for tuples.
    #[doc(hidden)]
    fn __counts() -> Vec<Option<&'static str>>;
    /// The class each field holds, for fields of user classes (split
    /// branches).
    #[doc(hidden)]
    fn __classes() -> Vec<Option<ClassSpec>>;
    /// Appends the row's fields to their buffers, one per field.
    #[doc(hidden)]
    fn __put(&self, out: &mut [LeafBuffer]);
}

/// A field of a [`Row`]: one entry of a column. Numbers and `bool`
/// (a column of them), `[T; N]` (fixed arrays), `Vec<T>` of numbers
/// (jagged arrays, `std::vector`, `RVec`), `String`, and [`Value`] for
/// columns of objects.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be a field of a row",
    note = "row fields can be numbers, `bool`, `[T; N]`, `Vec<T>` of numbers, `String`, or `Value`"
)]
pub trait RowField: Sized {
    /// The column type read.
    #[doc(hidden)]
    type Column: ColumnType;
    /// One value per entry.
    #[doc(hidden)]
    fn __split(out: <Self::Column as ColumnType>::Output) -> Vec<Self>;
    /// One value per entry, when converting can fail (objects).
    #[doc(hidden)]
    fn __try_split(out: <Self::Column as ColumnType>::Output) -> Result<Vec<Self>> {
        Ok(Self::__split(out))
    }
    /// How it is written to a tree; `None` if it cannot be.
    #[doc(hidden)]
    fn __shape() -> Option<LeafShape>;
    /// Appends one entry.
    #[doc(hidden)]
    fn __put(&self, out: &mut LeafBuffer);
    /// The user class it holds (`#[derive(RootObject)]` types and `Vec`s
    /// of them): read from, and written as, split branches.
    #[doc(hidden)]
    fn __class() -> Option<ClassSpec> {
        None
    }
}

/// A user class a row field holds (the class of an object field, or the
/// element class of a `Vec` field): its descriptions, its own first. Not
/// part of the API.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct ClassSpec {
    pub(crate) infos: Vec<Arc<StreamerInfo>>,
}

impl ClassSpec {
    /// The spec of `T`.
    #[must_use]
    pub fn of<T: RootObject>() -> Self {
        let mut infos = vec![T::streamer_info()];
        T::__dependencies(&mut infos);
        Self { infos }
    }
}

impl<T: NativeType> RowField for T {
    type Column = T;
    fn __split(out: Vec<T>) -> Vec<T> {
        out
    }
    fn __shape() -> Option<LeafShape> {
        Some(LeafShape::new(T::SCALAR, LeafKind::One))
    }
    fn __put(&self, out: &mut LeafBuffer) {
        out.put(*self);
    }
}

impl<T: NativeType, const N: usize> RowField for [T; N] {
    type Column = [T; N];
    fn __split(out: Vec<[T; N]>) -> Vec<[T; N]> {
        out
    }
    fn __shape() -> Option<LeafShape> {
        Some(LeafShape::new(T::SCALAR, LeafKind::Fixed(N)))
    }
    fn __put(&self, out: &mut LeafBuffer) {
        out.put_fixed(self);
    }
}

/// What a `Vec` row field holds: numbers (a jagged array), or objects of
/// a user class (a split collection; `#[derive(RootObject)]` implements
/// it). Not part of the API.
#[doc(hidden)]
pub trait RowItem: Sized {
    /// The column a `Vec` of it reads.
    type Column: ColumnType;
    /// One `Vec` per entry.
    fn __split_vecs(out: <Self::Column as ColumnType>::Output) -> Result<Vec<Vec<Self>>>;
    /// How a `Vec` of it is written to a tree.
    fn __vec_shape() -> Option<LeafShape>;
    /// Appends one entry.
    fn __put_vec(v: &[Self], out: &mut LeafBuffer);
    /// The user class it is, if it is one.
    fn __item_class() -> Option<ClassSpec> {
        None
    }
}

impl<T: NativeType> RowItem for T {
    type Column = Jagged<T>;
    fn __split_vecs(out: crate::columnar::JaggedVec<T>) -> Result<Vec<Vec<T>>> {
        Ok(out.iter().map(<[T]>::to_vec).collect())
    }
    fn __vec_shape() -> Option<LeafShape> {
        Some(LeafShape::new(T::SCALAR, LeafKind::Jagged))
    }
    fn __put_vec(v: &[T], out: &mut LeafBuffer) {
        out.put_jagged(v);
    }
}

impl<T: RowItem> RowField for Vec<T> {
    type Column = T::Column;
    fn __split(_: <T::Column as ColumnType>::Output) -> Vec<Vec<T>> {
        Vec::new()
    }
    fn __try_split(out: <T::Column as ColumnType>::Output) -> Result<Vec<Vec<T>>> {
        T::__split_vecs(out)
    }
    fn __shape() -> Option<LeafShape> {
        T::__vec_shape()
    }
    fn __put(&self, out: &mut LeafBuffer) {
        T::__put_vec(self, out);
    }
    fn __class() -> Option<ClassSpec> {
        T::__item_class()
    }
}

impl RowField for String {
    type Column = String;
    fn __split(out: Vec<String>) -> Vec<String> {
        out
    }
    fn __shape() -> Option<LeafShape> {
        Some(LeafShape::new(Scalar::I8, LeafKind::String))
    }
    fn __put(&self, out: &mut LeafBuffer) {
        out.put_string(self);
    }
}

impl RowField for Value {
    type Column = Value;
    fn __split(out: Vec<Value>) -> Vec<Value> {
        out
    }
    /// An untyped value has no class to split by: not written to trees.
    fn __shape() -> Option<LeafShape> {
        None
    }
    fn __put(&self, _: &mut LeafBuffer) {}
}

/// Field `i` of a row, from its column.
#[doc(hidden)]
pub fn row_field<F: RowField>(c: Column) -> Result<std::vec::IntoIter<F>> {
    Ok(F::__try_split(F::Column::from_column(c)?)?.into_iter())
}

/// Checks a field's column.
#[doc(hidden)]
pub fn row_check<F: RowField>(i: &Interpretation) -> core::result::Result<(), String> {
    if F::Column::accepts(i) {
        Ok(())
    } else {
        Err(F::Column::describe())
    }
}

macro_rules! tuple_row {
    ($n:literal: $($t:ident $i:tt),+) => {
        impl<$($t: RowField),+> Row for ($($t,)+) {
            const __WIDTH: usize = $n;
            fn __names() -> Vec<&'static str> {
                Vec::new()
            }
            fn __check(i: usize, interpretation: &Interpretation) -> core::result::Result<(), String> {
                match i {
                    $($i => row_check::<$t>(interpretation),)+
                    _ => Err("no such field".to_owned()),
                }
            }
            #[allow(non_snake_case, reason = "one iterator per type parameter")]
            fn __rows(columns: Vec<Column>) -> Result<Vec<Self>> {
                let mut columns = columns.into_iter();
                $(let mut $t = row_field::<$t>(columns.next().ok_or_else(missing_column)?)?;)+
                let n = columns.len();
                let _ = n;
                let mut out = Vec::new();
                loop {
                    let row = ($(match $t.next() { Some(v) => v, None => return Ok(out) },)+);
                    out.push(row);
                }
            }
            fn __shapes() -> Vec<Option<LeafShape>> {
                Vec::from([$($t::__shape()),+])
            }
            fn __counts() -> Vec<Option<&'static str>> {
                Vec::new()
            }
            fn __classes() -> Vec<Option<ClassSpec>> {
                Vec::from([$($t::__class()),+])
            }
            fn __put(&self, out: &mut [LeafBuffer]) {
                $(if let Some(b) = out.get_mut($i) { self.$i.__put(b); })+
            }
        }
    };
}

#[doc(hidden)]
pub fn missing_column() -> Error {
    Error::new(
        ErrorKind::InvalidInput,
        "a row has more fields than columns",
    )
}

tuple_row!(1: A 0);
tuple_row!(2: A 0, B 1);
tuple_row!(3: A 0, B 1, C 2);
tuple_row!(4: A 0, B 1, C 2, D 3);
tuple_row!(5: A 0, B 1, C 2, D 3, E 4);
tuple_row!(6: A 0, B 1, C 2, D 3, E 4, F 5);
tuple_row!(7: A 0, B 1, C 2, D 3, E 4, F 5, G 6);
tuple_row!(8: A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7);
tuple_row!(9: A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8);
tuple_row!(10: A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9);
tuple_row!(11: A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10);
tuple_row!(12: A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10, L 11);
tuple_row!(13: A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10, L 11, M 12);
tuple_row!(14: A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10, L 11, M 12, N 13);
tuple_row!(15: A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10, L 11, M 12, N 13, O 14);
tuple_row!(16: A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10, L 11, M 12, N 13, O 14, P 15);

/// The descriptions a user class writes itself with, built once.
pub struct Descriptions(Arc<StreamerSet>);

impl fmt::Debug for Descriptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Descriptions")
    }
}

/// What the derives generate code against. Not part of the API.
#[doc(hidden)]
pub mod __private {
    pub use core::option::Option::{self, None, Some};
    pub use core::result::Result::{self, Err, Ok};
    pub use std::boxed::Box;
    pub use std::string::String;
    pub use std::sync::{Arc, OnceLock};
    pub use std::vec::Vec;

    pub use super::{
        BaseTitle, ClassSpec, CountType, Descriptions, RootObject, RootScalar, RootType, Row,
        RowField, RowItem, VecItem, missing_column, row_check, row_field,
    };

    /// The shape of a field of a user class, or of a `Vec` of one.
    #[must_use]
    pub fn object_shape(collection: bool) -> LeafShape {
        LeafShape::new(
            Scalar::I32,
            if collection {
                crate::tree::LeafKind::Objects
            } else {
                crate::tree::LeafKind::Object
            },
        )
    }

    /// Objects of a column, as `T`.
    ///
    /// # Errors
    ///
    /// As `T::from_dyn`; `TypeMismatch` for a value that is not an object.
    pub fn objects_split<T: FromDyn>(out: &[Value]) -> crate::Result<Vec<T>> {
        out.iter()
            .map(|v| match v {
                Value::Object(o) => T::from_dyn(o),
                v => Err(v.mismatch("an object")),
            })
            .collect()
    }

    /// Lists of objects of a column, as `Vec<T>`.
    ///
    /// # Errors
    ///
    /// As [`objects_split`].
    pub fn object_lists_split<T: FromDyn>(out: Vec<Value>) -> crate::Result<Vec<Vec<T>>> {
        out.into_iter()
            .map(|v| match v {
                Value::List(items) => objects_split(&items),
                v => Err(v.mismatch("a list of objects")),
            })
            .collect()
    }
    pub use crate::columnar::{Column, Interpretation};
    pub use crate::streamer::{
        DynObject, FromDyn, RootClass, StreamerElement, StreamerInfo, ToDyn, Value,
    };
    pub use crate::tree::{LeafBuffer, LeafShape};

    use super::{
        Error, ErrorKind, Scalar, StreamerSet, array_items, format_version, mismatch, streamer,
    };

    /// A base class element.
    pub fn base_element<B: RootObject + BaseTitle>() -> StreamerElement {
        StreamerElement::new_base(B::CLASS_NAME, i32::from(B::CLASS_VERSION), B::TITLE)
    }

    /// A `std::string` member.
    pub fn std_string_element(name: &str) -> StreamerElement {
        StreamerElement::new_std_string(name)
    }

    /// An `int` counting a `//[fN]` array.
    pub fn counter_element<C: CountType>(name: &str) -> StreamerElement {
        StreamerElement::new_counter(name)
    }

    /// A `//[count]` array of scalars.
    pub fn var_array_element<T: RootScalar>(
        name: &str,
        count: &str,
        class: &str,
        version: i16,
    ) -> StreamerElement {
        StreamerElement::new_var_array(name, T::SCALAR, count, class, i32::from(version))
    }

    /// An object member of a user class.
    pub fn object_element<T: RootObject>(name: &str) -> StreamerElement {
        let size = T::streamer_info()
            .elements()
            .iter()
            .map(StreamerElement::size)
            .sum::<i32>()
            + if T::INHERITS_TOBJECT { 16 } else { 0 };
        StreamerElement::new_object(name, T::CLASS_NAME, size, T::INHERITS_TOBJECT)
    }

    /// A user class's description, its checksum computed with its bases'
    /// (user classes in `bases`, ROOT's from the built-in descriptions).
    pub fn class_info(
        class: &str,
        version: i16,
        elements: Vec<StreamerElement>,
        bases: &[Arc<StreamerInfo>],
    ) -> Arc<StreamerInfo> {
        let lookup = |name: &str, _: i32| {
            bases
                .iter()
                .find(|b| b.class_name() == name)
                .map(|b| b.checksum())
                .or_else(|| streamer::builtin().latest(name).map(|b| b.checksum()))
        };
        Arc::new(StreamerInfo::new_class(
            format_version(),
            class,
            i32::from(version),
            elements,
            lookup,
        ))
    }

    /// The descriptions of `T` and of the user classes it contains.
    pub fn descriptions<T: RootObject>() -> Descriptions {
        let mut all = vec![T::streamer_info()];
        T::__dependencies(&mut all);
        let mut seen: Vec<Arc<StreamerInfo>> = Vec::new();
        for info in all {
            if !seen.iter().any(|s| s.class_name() == info.class_name()) {
                seen.push(info);
            }
        }
        Descriptions(Arc::new(StreamerSet::from_arcs(seen)))
    }

    /// A new object of a user class, to fill member by member.
    pub fn object(class: &str, version: i16) -> DynObject {
        DynObject::with_version(class, version)
    }

    /// Adds a base part to an object.
    pub fn push_base(obj: &mut DynObject, base: DynObject) {
        obj.push_base(base);
    }

    /// Marks the object as described by `d`, for writing.
    pub fn attach(obj: &mut DynObject, d: &Descriptions) {
        obj.set_source(Arc::clone(&d.0));
    }

    /// Checks the class of an object read.
    pub fn expect_class(obj: &DynObject, class: &str) -> Result<(), Error> {
        if obj.class() == class {
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::ClassMismatch,
                format!("the object is a {}, not a {class}", obj.class()),
            ))
        }
    }

    /// A member read by name (the first of `names` found), or its missing
    /// value when the file's version of the class does not have it.
    pub fn field<T: RootType>(obj: &DynObject, names: &[&str]) -> Result<T, Error> {
        let found = names
            .iter()
            .find_map(|n| obj.own_members().iter().find(|(m, _)| m == n));
        match found {
            None => Ok(T::__missing()),
            Some((name, v)) => T::__from_value(v).map_err(|e| {
                Error::new(e.kind(), format!("member {name} of {}: {e}", obj.class()))
            }),
        }
    }

    /// A base part read, or its missing value.
    pub fn base<T: RootType + FromDyn>(obj: &DynObject, class: &str) -> Result<T, Error> {
        match obj.bases().iter().find(|b| b.class() == class) {
            Some(b) => T::from_dyn(b),
            None => Ok(T::__missing()),
        }
    }

    /// A `//[count]` array as a value.
    pub fn scalars_value<T: RootScalar>(v: &[T]) -> Value {
        Value::Array(T::__array(v.to_vec()))
    }

    /// A `//[count]` array read.
    pub fn scalars_from<T: RootScalar>(v: &Value) -> Result<Vec<T>, Error> {
        array_items(v)
    }

    /// A `//[count]` array member read by name, or empty.
    pub fn counted<T: RootScalar>(obj: &DynObject, names: &[&str]) -> Result<Vec<T>, Error> {
        let found = names
            .iter()
            .find_map(|n| obj.own_members().iter().find(|(m, _)| m == n));
        match found {
            None => Ok(Vec::new()),
            Some((name, v)) => array_items(v).map_err(|e| {
                Error::new(e.kind(), format!("member {name} of {}: {e}", obj.class()))
            }),
        }
    }

    /// The length of a `//[count]` array as its counter.
    pub fn count_of(len: usize) -> Value {
        Value::I32(i32::try_from(len).unwrap_or(i32::MAX))
    }

    /// An object member as a value.
    pub fn object_value<T: ToDyn>(v: &T) -> Value {
        Value::Object(Box::new(v.to_dyn()))
    }

    /// An object member read.
    pub fn object_from<T: FromDyn>(v: &Value, class: &str) -> Result<T, Error> {
        match v {
            Value::Object(o) => T::from_dyn(o),
            other => Err(mismatch(&format!("an object of class {class}"), other)),
        }
    }

    /// A list of objects as a value.
    pub fn objects_value<T: ToDyn>(v: &[T]) -> Value {
        Value::List(
            v.iter()
                .map(|x| Value::Object(Box::new(x.to_dyn())))
                .collect(),
        )
    }

    /// A list of objects read.
    pub fn objects_from<T: FromDyn>(v: &Value, class: &str) -> Result<Vec<T>, Error> {
        match v {
            Value::List(items) => items.iter().map(|x| object_from(x, class)).collect(),
            Value::Null => Ok(Vec::new()),
            other => Err(mismatch(&format!("a list of {class}"), other)),
        }
    }
}
