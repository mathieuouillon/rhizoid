//! Reading split branches whole: the objects of a split object branch, and
//! the lists of objects of a split collection, assembled from their
//! sub-branches [RB§8.8]. Typed rows of user classes read through this.

use core::ops::Range;
use std::collections::HashMap;

use super::{Branch, Tree};
use crate::columnar::{Column, Interpretation};
use crate::core::error::{Error, ErrorKind, Result};
use crate::streamer::{DynObject, ElementKind, StreamerInfo, StreamerSet, Value};

/// The prefix of a column a typed row reads as objects.
pub(crate) const OBJECTS: &str = "@objects:";

/// `kIsOnHeap | kNotDeleted`: split branches do not store them in `fBits`;
/// ROOT sets them again when it reads, and so do we.
const HEAP_BITS: i64 = 0x0300_0000;

/// A member value as read, `fBits` (`kBits`, type 15) with the heap bits.
fn restored(type_code: i32, v: Value) -> Value {
    match (type_code, v.as_i64()) {
        (15, Some(bits)) => Value::U32(u32::try_from(bits | HEAP_BITS).unwrap_or(0)),
        _ => v,
    }
}

fn unsupported(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Unsupported, message)
}

/// One value of a column.
fn value_at(column: &Column, i: usize) -> Value {
    match column {
        Column::Values(a) => a.get(i).unwrap_or(Value::Null),
        Column::Fixed { len, values } => {
            Value::Array(crate::ntuple::slice(values, i * len..(i + 1) * len))
        }
        Column::Jagged { offsets, values } => {
            let r = offsets.get(i).copied().unwrap_or(0)..offsets.get(i + 1).copied().unwrap_or(0);
            Value::Array(crate::ntuple::slice(values, r))
        }
        Column::Strings(s) => s.get(i).cloned().map_or(Value::Null, Value::String),
        Column::Objects(v) => v.get(i).cloned().unwrap_or(Value::Null),
    }
}

/// Element `k` of entry `i` of a collection member's column: `n` elements
/// in that entry, each a scalar (`scalar`) or an array.
fn item_at(column: &Column, entry: usize, k: usize, n: usize, scalar: bool) -> Value {
    let Column::Jagged { offsets, values } = column else {
        return Value::Null;
    };
    let (first, end) = (
        offsets.get(entry).copied().unwrap_or(0),
        offsets.get(entry + 1).copied().unwrap_or(0),
    );
    let width = (end - first).checked_div(n).unwrap_or(0);
    let start = first + k * width;
    if scalar && width == 1 {
        values.get(start).unwrap_or(Value::Null)
    } else {
        Value::Array(crate::ntuple::slice(values, start..start + width))
    }
}

impl Tree {
    /// The branch a typed row's object field reads: `name`, or `name.`.
    fn objects_branch(&self, name: &str) -> Result<&Branch> {
        self.get_branch(name)
            .or_else(|| self.get_branch(&format!("{name}.")))
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::NotFound,
                    format!("tree `{}` has no branch `{name}`", self.inner.path),
                )
            })
    }

    /// How a column read as objects reads.
    pub(crate) fn objects_interpretation(&self, name: &str) -> Result<Interpretation> {
        let b = self.objects_branch(name)?;
        match &b.element {
            Some(e) if e.kind == 4 => Ok(Interpretation::Objects {
                class: e.class_name.clone(),
            }),
            Some(e) if b.is_group() => Ok(Interpretation::Objects {
                class: e.class_name.clone(),
            }),
            _ => b.interpretation().cloned(),
        }
    }

    /// Reads a column as objects: a split object or collection assembled,
    /// any other branch as it reads.
    pub(crate) fn read_objects(&self, name: &str, runs: &[Range<u64>]) -> Result<Column> {
        let b = self.objects_branch(name)?;
        let set = self.file.streamer_set()?;
        match &b.element {
            Some(e) if e.kind == 4 => Ok(Column::Objects(self.collection(b, runs, &set)?)),
            Some(_) if b.is_group() => Ok(Column::Objects(
                self.assemble(b, runs, &set)?
                    .into_iter()
                    .map(|o| Value::Object(Box::new(o)))
                    .collect(),
            )),
            _ => self.read_runs(b, runs),
        }
    }

    /// The class a node of a split object stands for, and its version.
    fn node_class(b: &Branch, set: &StreamerSet) -> Result<(String, i16)> {
        let e = b
            .element
            .as_ref()
            .ok_or_else(|| unsupported("not a split branch"))?;
        let class = match e.kind {
            // A base part or an embedded object: named by the owner's element.
            1 | 2 => {
                let owner = info(set, &e.class_name, e.class_version)?;
                let el = usize::try_from(e.id)
                    .ok()
                    .and_then(|i| owner.elements().get(i))
                    .ok_or_else(|| {
                        unsupported(format!("no member {} in {}", e.id, e.class_name))
                    })?;
                if e.kind == 1 {
                    el.name().to_owned()
                } else {
                    el.class_name().to_owned()
                }
            }
            _ => e.class_name.clone(),
        };
        let version = set
            .latest(&class)
            .map_or(0, |i| i16::try_from(i.class_version()).unwrap_or(0));
        Ok((class, version))
    }

    /// The member a sub-branch holds: its name and type code.
    fn member(b: &Branch, set: &StreamerSet) -> Result<(String, i32)> {
        let e = b
            .element
            .as_ref()
            .ok_or_else(|| unsupported("not a split branch"))?;
        let owner = info(set, &e.class_name, e.class_version)?;
        usize::try_from(e.id)
            .ok()
            .and_then(|i| owner.elements().get(i))
            .map(|el| (el.name().to_owned(), el.type_code()))
            .ok_or_else(|| unsupported(format!("no member {} in {}", e.id, e.class_name)))
    }

    /// The objects of a split object branch (or of a node of one).
    fn assemble(
        &self,
        b: &Branch,
        runs: &[Range<u64>],
        set: &StreamerSet,
    ) -> Result<Vec<DynObject>> {
        let n = usize::try_from(runs.iter().map(|r| r.end - r.start).sum::<u64>())
            .map_err(|_| Error::malformed("too many entries"))?;
        let (class, version) = Self::node_class(b, set)?;
        let mut out: Vec<DynObject> = (0..n)
            .map(|_| DynObject::with_version(&*class, version))
            .collect();
        for c in &b.branches {
            let kind = c.element.as_ref().map_or(0, |e| e.kind);
            if c.is_group() {
                let parts = self.assemble(c, runs, set)?;
                let (name, _) = Self::member(c, set)?;
                for (o, part) in out.iter_mut().zip(parts) {
                    if kind == 1 {
                        o.push_base(part);
                    } else {
                        o.push_member(name.clone(), Value::Object(Box::new(part)));
                    }
                }
            } else {
                let column = self.read_runs(c, runs)?;
                let (name, code) = Self::member(c, set)?;
                for (i, o) in out.iter_mut().enumerate() {
                    o.push_member(name.clone(), restored(code, value_at(&column, i)));
                }
            }
        }
        Ok(out)
    }

    /// The lists of objects of a split collection.
    fn collection(&self, b: &Branch, runs: &[Range<u64>], set: &StreamerSet) -> Result<Vec<Value>> {
        let e = b
            .element
            .as_ref()
            .ok_or_else(|| unsupported("not a split branch"))?;
        let item = info(set, &e.clones_name, 0)
            .map_err(|_| unsupported(format!("no description of {}", e.clones_name)))?;
        let counts = self.read_runs(b, runs)?;
        let mut members = Vec::new();
        for c in &b.branches {
            if c.is_group() || !c.branches.is_empty() {
                return Err(unsupported(format!(
                    "`{}`: collections of objects with split members cannot be read whole yet",
                    b.name
                )));
            }
            let ce = c
                .element
                .as_ref()
                .ok_or_else(|| unsupported("not a split branch"))?;
            let (name, code) = Self::member(c, set)?;
            let scalar = info(set, &ce.class_name, ce.class_version)?
                .elements()
                .iter()
                .find(|el| el.name() == name)
                .is_some_and(|el| matches!(el.kind(), ElementKind::Scalar(_)));
            members.push((
                ce.class_name.clone(),
                name,
                scalar,
                code,
                self.read_runs(c, runs)?,
            ));
        }
        let entries = counts.len();
        let mut out = Vec::with_capacity(entries);
        for i in 0..entries {
            let n = usize::try_from(value_at(&counts, i).as_i64().unwrap_or(0)).unwrap_or(0);
            let mut items = Vec::with_capacity(n);
            for k in 0..n {
                let values: HashMap<(&str, &str), Value> = members
                    .iter()
                    .map(|(owner, name, scalar, code, col)| {
                        let v = restored(*code, item_at(col, i, k, n, *scalar));
                        ((owner.as_str(), name.as_str()), v)
                    })
                    .collect();
                items.push(Value::Object(Box::new(build(&item, set, &values))));
            }
            out.push(Value::List(items));
        }
        Ok(out)
    }
}

/// A class's description: `version`, else the newest.
fn info(set: &StreamerSet, class: &str, version: i32) -> Result<std::sync::Arc<StreamerInfo>> {
    set.get(class, version)
        .or_else(|| set.latest(class))
        .cloned()
        .ok_or_else(|| unsupported(format!("no description of class {class}")))
}

/// An object of `info`'s class from member values keyed by owner class and
/// name, its bases built the same way.
fn build(
    info: &StreamerInfo,
    set: &StreamerSet,
    values: &HashMap<(&str, &str), Value>,
) -> DynObject {
    let mut o = DynObject::with_version(
        info.class_name(),
        i16::try_from(info.class_version()).unwrap_or(0),
    );
    for el in info.elements() {
        if let ElementKind::Base { .. } = el.kind() {
            if let Some(base) = set.latest(el.name()) {
                o.push_base(build(base, set, values));
            }
        } else if let Some(v) = values.get(&(info.class_name(), el.name())) {
            o.push_member(el.name(), v.clone());
        }
    }
    o
}
