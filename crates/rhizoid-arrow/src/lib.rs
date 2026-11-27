//! Apache Arrow for **rhizoid**: `TTree` columns as Arrow arrays and
//! record batches, to hand to query engines, data-frame libraries, Parquet
//! writers or Python.
//!
//! | rhizoid [`Column`] | Arrow |
//! |---|---|
//! | `Values` | `Boolean`, `Int8`…`UInt64`, `Float32`, `Float64` |
//! | `Fixed` (`x[3]`) | `FixedSizeList` |
//! | `Jagged` (`x[n]`, `vector<T>`) | `List` (`LargeList` beyond 2³¹ values) |
//! | `Strings` | `Utf8` |
//! | `Objects` | inferred from the values: `Struct` for objects (without their `TObject` part), `List` for collections, `Struct` of `_0`, `_1`, … for tuples, pairs and map entries, a dense `Union` for variants, `Binary` for members kept undecoded |
//!
//! ```
//! use rhizoid_arrow::TreeExt;
//! use rhizoid_io::tree::BatchSize;
//! # let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../rhizoid-io/tests/data/uproot_trees.root");
//!
//! let file = rhizoid_io::File::open(path)?;
//! let tree = file.tree("events")?;
//! for batch in tree.record_batches(["f32", "jet_pt"], BatchSize::Cluster)? {
//!     let batch: arrow_array::RecordBatch = batch?;      // one per cluster
//!     assert_eq!(batch.num_columns(), 2);
//! }
//! # Ok::<(), rhizoid_io::Error>(())
//! ```
//!
//! Versioned with `arrow`: this release uses `arrow-array` 60.

use std::sync::Arc;

use arrow_array::types::{
    Float32Type, Float64Type, Int8Type, Int16Type, Int32Type, Int64Type, UInt8Type, UInt16Type,
    UInt32Type, UInt64Type,
};
use arrow_array::{
    Array as _, ArrayRef, BinaryArray, BooleanArray, FixedSizeListArray, LargeListArray, ListArray,
    NullArray, PrimitiveArray, RecordBatch, StringArray, StructArray, UnionArray,
};
use arrow_buffer::{NullBuffer, OffsetBuffer, ScalarBuffer};
use arrow_schema::{Field, Fields, Schema, UnionFields};
use rhizoid_io::columnar::Column;
use rhizoid_io::tree::{Batch, BatchSize, Projection, TreeReader};
use rhizoid_io::{Array, DynObject, Error, ErrorKind, Result, Tree, Value};

#[cold]
fn arrow_error(e: &arrow_schema::ArrowError) -> Error {
    Error::custom(format!("arrow: {e}"))
}

#[cold]
fn unsupported(what: impl Into<String>) -> Error {
    Error::new(ErrorKind::Unsupported, what.into())
}

/// A rhizoid array as an Arrow array.
fn array_to_arrow(a: &Array) -> ArrayRef {
    match a {
        Array::Bool(v) => Arc::new(BooleanArray::from(v.clone())),
        Array::I8(v) => Arc::new(PrimitiveArray::<Int8Type>::from(v.clone())),
        Array::U8(v) => Arc::new(PrimitiveArray::<UInt8Type>::from(v.clone())),
        Array::I16(v) => Arc::new(PrimitiveArray::<Int16Type>::from(v.clone())),
        Array::U16(v) => Arc::new(PrimitiveArray::<UInt16Type>::from(v.clone())),
        Array::I32(v) => Arc::new(PrimitiveArray::<Int32Type>::from(v.clone())),
        Array::U32(v) => Arc::new(PrimitiveArray::<UInt32Type>::from(v.clone())),
        Array::I64(v) => Arc::new(PrimitiveArray::<Int64Type>::from(v.clone())),
        Array::U64(v) => Arc::new(PrimitiveArray::<UInt64Type>::from(v.clone())),
        Array::F32(v) => Arc::new(PrimitiveArray::<Float32Type>::from(v.clone())),
        Array::F64(v) => Arc::new(PrimitiveArray::<Float64Type>::from(v.clone())),
        _ => Arc::new(NullArray::new(a.len())),
    }
}

/// A list array over `values` with these offsets (`List`, or `LargeList`
/// when they do not fit `i32`).
fn list(offsets: &[usize], values: ArrayRef, nulls: Option<NullBuffer>) -> Result<ArrayRef> {
    let field = Arc::new(Field::new("item", values.data_type().clone(), true));
    if let Ok(small) = offsets
        .iter()
        .map(|&o| i32::try_from(o))
        .collect::<core::result::Result<Vec<i32>, _>>()
    {
        let offsets = OffsetBuffer::new(ScalarBuffer::from(small));
        return Ok(Arc::new(
            ListArray::try_new(field, offsets, values, nulls).map_err(|e| arrow_error(&e))?,
        ));
    }
    let large = offsets
        .iter()
        .map(|&o| i64::try_from(o))
        .collect::<core::result::Result<Vec<i64>, _>>()
        .map_err(|_| unsupported("more than 2^63 values"))?;
    let offsets = OffsetBuffer::new(ScalarBuffer::from(large));
    Ok(Arc::new(
        LargeListArray::try_new(field, offsets, values, nulls).map_err(|e| arrow_error(&e))?,
    ))
}

/// A column as an Arrow array, one element per entry.
///
/// # Errors
///
/// `Unsupported` for decoded objects whose values do not share one Arrow
/// type (maps with keys and values of different types, undecoded members).
pub fn to_arrow(column: &Column) -> Result<ArrayRef> {
    match column {
        Column::Values(a) => Ok(array_to_arrow(a)),
        Column::Fixed { len, values } => {
            let values = array_to_arrow(values);
            let field = Arc::new(Field::new("item", values.data_type().clone(), false));
            let size =
                i32::try_from(*len).map_err(|_| unsupported("fixed arrays longer than 2^31"))?;
            Ok(Arc::new(
                FixedSizeListArray::try_new(field, size, values, None)
                    .map_err(|e| arrow_error(&e))?,
            ))
        }
        Column::Jagged { offsets, values } => list(offsets, array_to_arrow(values), None),
        Column::Strings(s) => Ok(Arc::new(StringArray::from_iter_values(s))),
        Column::Objects(v) => values_to_arrow(v),
        _ => Err(unsupported("this kind of column")),
    }
}

/// The members of an object, flattened through its bases, leaving out the
/// `TObject` part (as uproot does).
fn members(o: &DynObject, out: &mut Vec<(String, Value)>) {
    for b in o.bases().iter().filter(|b| b.class() != "TObject") {
        members(b, out);
    }
    out.extend(o.members().map(|(n, v)| (n.to_owned(), v.clone())));
}

macro_rules! primitive {
    ($values:expr, $variant:ident, $t:ty) => {{
        let v = $values
            .iter()
            .map(|x| match x {
                Value::$variant(n) => Ok(Some(*n)),
                Value::Null => Ok(None),
                other => Err(unsupported(format!(
                    "mixed {} and {}",
                    stringify!($variant),
                    other.type_name()
                ))),
            })
            .collect::<Result<Vec<_>>>()?;
        Arc::new(PrimitiveArray::<$t>::from(v)) as ArrayRef
    }};
}

/// Decoded values as one Arrow array, its type inferred from the values.
///
/// # Errors
///
/// `Unsupported` when the values do not share one Arrow type.
pub fn values_to_arrow(values: &[Value]) -> Result<ArrayRef> {
    let Some(first) = values.iter().find(|v| !v.is_null()) else {
        return Ok(Arc::new(NullArray::new(values.len())));
    };
    if kinds(values).len() > 1 {
        return union(values);
    }
    let validity = || {
        values
            .iter()
            .any(Value::is_null)
            .then(|| NullBuffer::from(values.iter().map(|v| !v.is_null()).collect::<Vec<_>>()))
    };
    Ok(match first {
        Value::Bool(_) => {
            let v = values
                .iter()
                .map(|x| match x {
                    Value::Bool(b) => Ok(Some(*b)),
                    Value::Null => Ok(None),
                    other => Err(unsupported(format!("mixed bool and {}", other.type_name()))),
                })
                .collect::<Result<Vec<_>>>()?;
            Arc::new(BooleanArray::from(v))
        }
        Value::I8(_) => primitive!(values, I8, Int8Type),
        Value::U8(_) => primitive!(values, U8, UInt8Type),
        Value::I16(_) => primitive!(values, I16, Int16Type),
        Value::U16(_) => primitive!(values, U16, UInt16Type),
        Value::I32(_) => primitive!(values, I32, Int32Type),
        Value::U32(_) => primitive!(values, U32, UInt32Type),
        Value::I64(_) => primitive!(values, I64, Int64Type),
        Value::U64(_) => primitive!(values, U64, UInt64Type),
        Value::F32(_) => primitive!(values, F32, Float32Type),
        Value::F64(_) => primitive!(values, F64, Float64Type),
        Value::String(_) => {
            let v = values
                .iter()
                .map(|x| match x {
                    Value::String(s) => Ok(Some(s.as_str())),
                    Value::Null => Ok(None),
                    other => Err(unsupported(format!(
                        "mixed string and {}",
                        other.type_name()
                    ))),
                })
                .collect::<Result<Vec<_>>>()?;
            Arc::new(StringArray::from(v))
        }
        Value::List(_) if is_tuple(values) => tuples_to_arrow(values, validity())?,
        Value::Array(_) | Value::List(_) => lists_to_arrow(values, validity())?,
        Value::Object(_) => objects_to_arrow(values, validity())?,
        // Members this version cannot decode keep their streamed bytes.
        Value::Opaque { .. } => {
            let v = values
                .iter()
                .map(|x| match x {
                    Value::Opaque { bytes, .. } => Ok(Some(bytes.as_slice())),
                    Value::Null => Ok(None),
                    other => Err(unsupported(format!(
                        "mixed undecoded and {}",
                        other.type_name()
                    ))),
                })
                .collect::<Result<Vec<_>>>()?;
            Arc::new(BinaryArray::from(v))
        }
        other => return Err(unsupported(format!("values of type {}", other.type_name()))),
    })
}

/// The kinds of value in a column, in order of first appearance (arrays
/// and lists are one kind: both become Arrow lists).
fn kinds(values: &[Value]) -> Vec<&'static str> {
    let mut out = Vec::new();
    for v in values.iter().filter(|v| !v.is_null()) {
        let k = match v {
            Value::Array(_) => "list",
            v => v.type_name(),
        };
        if !out.contains(&k) {
            out.push(k);
        }
    }
    out
}

/// Values of several kinds (`std::variant`), as a dense union with one
/// child per kind; nulls go to a null child.
fn union(values: &[Value]) -> Result<ArrayRef> {
    let mut order = kinds(values);
    if values.iter().any(Value::is_null) {
        order.push("null");
    }
    let mut groups: Vec<Vec<Value>> = vec![Vec::new(); order.len()];
    let (mut type_ids, mut offsets) = (
        Vec::with_capacity(values.len()),
        Vec::with_capacity(values.len()),
    );
    for v in values {
        let k = match v {
            Value::Null => "null",
            Value::Array(_) => "list",
            v => v.type_name(),
        };
        let i = order.iter().position(|o| *o == k).unwrap_or(0);
        let group = groups.get_mut(i).ok_or_else(|| unsupported("union kind"))?;
        type_ids.push(i8::try_from(i).map_err(|_| unsupported("a union of more than 127 kinds"))?);
        offsets.push(
            i32::try_from(group.len())
                .map_err(|_| unsupported("a union child of more than 2^31 values"))?,
        );
        group.push(v.clone());
    }
    let children: Vec<ArrayRef> = groups
        .iter()
        .map(|g| values_to_arrow(g))
        .collect::<Result<_>>()?;
    let fields = UnionFields::try_new(
        (0..children.len()).map(|i| i8::try_from(i).unwrap_or(i8::MAX)),
        order
            .iter()
            .zip(&children)
            .map(|(name, c)| Field::new(*name, c.data_type().clone(), true)),
    )
    .map_err(|e| arrow_error(&e))?;
    Ok(Arc::new(
        UnionArray::try_new(fields, type_ids.into(), Some(offsets.into()), children)
            .map_err(|e| arrow_error(&e))?,
    ))
}

/// Whether every list has the same length and, position by position, one
/// type, but not one type overall: tuples, pairs, map entries.
fn is_tuple(values: &[Value]) -> bool {
    let lists: Vec<&[Value]> = values.iter().filter_map(Value::as_list).collect();
    let Some(first) = lists.first() else {
        return false;
    };
    let mixed = first
        .windows(2)
        .any(|w| w[0].type_name() != w[1].type_name());
    mixed
        && lists.len() == values.iter().filter(|v| !v.is_null()).count()
        && lists.iter().all(|l| {
            l.len() == first.len()
                && l.iter()
                    .zip(*first)
                    .all(|(a, b)| a.type_name() == b.type_name() || a.is_null() || b.is_null())
        })
}

/// Tuples (fixed-length lists of mixed types) as a struct with fields
/// `_0`, `_1`, …
fn tuples_to_arrow(values: &[Value], validity: Option<NullBuffer>) -> Result<ArrayRef> {
    let width = values
        .iter()
        .find_map(Value::as_list)
        .map_or(0, <[Value]>::len);
    let mut fields = Vec::with_capacity(width);
    let mut arrays = Vec::with_capacity(width);
    for i in 0..width {
        let column: Vec<Value> = values
            .iter()
            .map(|v| {
                v.as_list()
                    .and_then(|l| l.get(i))
                    .cloned()
                    .unwrap_or(Value::Null)
            })
            .collect();
        let array = values_to_arrow(&column)?;
        fields.push(Field::new(format!("_{i}"), array.data_type().clone(), true));
        arrays.push(array);
    }
    Ok(Arc::new(
        StructArray::try_new(Fields::from(fields), arrays, validity)
            .map_err(|e| arrow_error(&e))?,
    ))
}

/// Lists or arrays per value, as a list array of their items.
fn lists_to_arrow(values: &[Value], validity: Option<NullBuffer>) -> Result<ArrayRef> {
    let mut offsets = Vec::with_capacity(values.len() + 1);
    offsets.push(0);
    let mut children = Vec::new();
    for v in values {
        match v {
            Value::Array(a) => children.extend((0..a.len()).filter_map(|i| a.get(i))),
            Value::List(items) => children.extend(items.iter().cloned()),
            Value::Null => {}
            other => return Err(unsupported(format!("mixed list and {}", other.type_name()))),
        }
        offsets.push(children.len());
    }
    list(&offsets, values_to_arrow(&children)?, validity)
}

/// Objects, as a struct array with a field per member (in the order of the
/// first object's members).
fn objects_to_arrow(values: &[Value], validity: Option<NullBuffer>) -> Result<ArrayRef> {
    let rows: Vec<Vec<(String, Value)>> = values
        .iter()
        .map(|v| {
            let mut m = Vec::new();
            if let Value::Object(o) = v {
                members(o, &mut m);
            }
            m
        })
        .collect();
    let names: Vec<String> = rows
        .iter()
        .find(|r| !r.is_empty())
        .map(|r| r.iter().map(|(n, _)| n.clone()).collect())
        .unwrap_or_default();
    if names.is_empty() {
        return Ok(Arc::new(StructArray::new_empty_fields(
            values.len(),
            validity,
        )));
    }
    let mut fields = Vec::with_capacity(names.len());
    let mut arrays = Vec::with_capacity(names.len());
    for name in &names {
        let column: Vec<Value> = rows
            .iter()
            .map(|r| {
                r.iter()
                    .find(|(n, _)| n == name)
                    .map_or(Value::Null, |(_, v)| v.clone())
            })
            .collect();
        let array = values_to_arrow(&column)?;
        fields.push(Field::new(name, array.data_type().clone(), true));
        arrays.push(array);
    }
    Ok(Arc::new(
        StructArray::try_new(Fields::from(fields), arrays, validity)
            .map_err(|e| arrow_error(&e))?,
    ))
}

/// A batch as an Arrow record batch, one column per branch.
///
/// # Errors
///
/// As [`to_arrow`].
pub fn to_record_batch(batch: &Batch) -> Result<RecordBatch> {
    let mut fields = Vec::new();
    let mut arrays = Vec::new();
    for (name, column) in batch.columns() {
        let array = to_arrow(column).map_err(|e| e.with_context_message(name))?;
        fields.push(Field::new(
            name,
            array.data_type().clone(),
            array.null_count() > 0,
        ));
        arrays.push(array);
    }
    RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays).map_err(|e| arrow_error(&e))
}

/// Record batches, read on demand from a [`TreeReader`].
#[derive(Debug)]
pub struct RecordBatches {
    reader: TreeReader,
}

impl From<TreeReader> for RecordBatches {
    fn from(reader: TreeReader) -> Self {
        Self { reader }
    }
}

impl Iterator for RecordBatches {
    type Item = Result<RecordBatch>;

    fn next(&mut self) -> Option<Result<RecordBatch>> {
        Some(self.reader.next()?.and_then(|b| to_record_batch(&b)))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.reader.size_hint()
    }
}

impl ExactSizeIterator for RecordBatches {}

mod sealed {
    pub trait Sealed {}
    impl Sealed for rhizoid_io::Tree {}
}

/// Arrow methods on [`Tree`]. Sealed: implemented for [`Tree`] only, so
/// methods can be added to it.
pub trait TreeExt: sealed::Sealed {
    /// Record batches of the projected branches over every entry. For a
    /// selection, an offset or a limit, build the reader with
    /// [`Tree::reader`] and convert it with `RecordBatches::from`.
    ///
    /// # Errors
    ///
    /// As [`rhizoid_io::tree::TreeReaderBuilder::projection`].
    fn record_batches(
        &self,
        projection: impl Into<Projection>,
        size: BatchSize,
    ) -> Result<RecordBatches>;
}

impl TreeExt for Tree {
    fn record_batches(
        &self,
        projection: impl Into<Projection>,
        size: BatchSize,
    ) -> Result<RecordBatches> {
        Ok(self
            .reader()
            .projection(projection)?
            .batch_size(size)
            .build()
            .into())
    }
}

/// Adds the column name to an error message.
trait WithName {
    fn with_context_message(self, name: &str) -> Error;
}

impl WithName for Error {
    fn with_context_message(self, name: &str) -> Error {
        Error::new(self.kind(), format!("column `{name}`: {self}"))
    }
}

#[cfg(test)]
mod tests {
    use arrow_array::types::{Float32Type, Int16Type};
    use arrow_array::{
        BooleanArray, FixedSizeListArray, Float64Array, Int32Array, ListArray, StringArray,
        UInt64Array,
    };
    use arrow_schema::DataType;

    use super::*;

    fn downcast<T: 'static>(a: &ArrayRef) -> &T {
        a.as_any().downcast_ref::<T>().expect("array type")
    }

    #[test]
    fn values_become_primitive_arrays() {
        let a = to_arrow(&Column::Values(Array::F64(vec![1.5, -2.0, f64::NAN]))).expect("f64");
        let f = downcast::<Float64Array>(&a);
        assert_eq!((f.value(0), f.value(1)), (1.5, -2.0));
        assert!(f.value(2).is_nan());
        let a = to_arrow(&Column::Values(Array::I32(vec![7, -8]))).expect("i32");
        assert_eq!(downcast::<Int32Array>(&a).values().to_vec(), [7, -8]);
        let a = to_arrow(&Column::Values(Array::U64(vec![u64::MAX]))).expect("u64");
        assert_eq!(downcast::<UInt64Array>(&a).value(0), u64::MAX);
        let a = to_arrow(&Column::Values(Array::Bool(vec![true, false]))).expect("bool");
        let b = downcast::<BooleanArray>(&a);
        assert!(b.value(0) && !b.value(1));
        assert_eq!(a.null_count(), 0);
    }

    #[test]
    fn fixed_arrays_become_fixed_size_lists() {
        let column = Column::Fixed {
            len: 3,
            values: Array::F32((0..6_u8).map(f32::from).collect()),
        };
        let a = to_arrow(&column).expect("fixed");
        let l = downcast::<FixedSizeListArray>(&a);
        assert_eq!((l.len(), l.value_length()), (2, 3));
        let second = l.value(1);
        let second = second
            .as_any()
            .downcast_ref::<PrimitiveArray<Float32Type>>()
            .expect("f32");
        assert_eq!(second.values().to_vec(), [3.0, 4.0, 5.0]);
    }

    #[test]
    fn jagged_arrays_become_lists_with_their_offsets() {
        let column = Column::Jagged {
            offsets: vec![0, 2, 2, 5],
            values: Array::I16(vec![1, 2, 3, 4, 5]),
        };
        let a = to_arrow(&column).expect("jagged");
        let l = downcast::<ListArray>(&a);
        assert_eq!(l.len(), 3);
        assert_eq!(l.value_offsets().to_vec(), [0, 2, 2, 5]);
        assert_eq!(l.value(1).len(), 0, "an empty entry");
        let last = l.value(2);
        let last = last
            .as_any()
            .downcast_ref::<PrimitiveArray<Int16Type>>()
            .expect("i16");
        assert_eq!(last.values().to_vec(), [3, 4, 5]);
    }

    #[test]
    fn strings_become_utf8() {
        let a = to_arrow(&Column::Strings(vec![
            String::new(),
            "é".into(),
            "long".repeat(100),
        ]))
        .expect("strings");
        assert_eq!(a.data_type(), &DataType::Utf8);
        let s = downcast::<StringArray>(&a);
        assert_eq!((s.value(0), s.value(1)), ("", "é"));
        assert_eq!(s.value(2).len(), 400);
    }

    #[test]
    fn values_with_nulls_are_nullable_and_mixed_kinds_are_unions() {
        let a = values_to_arrow(&[Value::I32(1), Value::Null, Value::I32(3)]).expect("nullable");
        let i = downcast::<Int32Array>(&a);
        assert_eq!(a.null_count(), 1);
        assert!(i.is_null(1));
        assert_eq!(i.value(2), 3);
        let mixed = values_to_arrow(&[Value::I32(1), Value::String("x".into())]).expect("mixed");
        assert!(
            matches!(mixed.data_type(), DataType::Union(..)),
            "{:?}",
            mixed.data_type()
        );
        assert_eq!(mixed.len(), 2);
        let nulls = values_to_arrow(&[Value::Null, Value::Null]).expect("nulls");
        assert_eq!(nulls.data_type(), &DataType::Null);
        let empty = values_to_arrow(&[]).expect("empty");
        assert_eq!(empty.len(), 0);
    }
}
