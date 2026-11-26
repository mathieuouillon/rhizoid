//! Comparing our values with uproot's: values reduced to scalar leaves
//! (numbers, booleans, JSON strings), as `uproot_leaves*.py` print them.

#![allow(dead_code, reason = "each test file uses part of it")]

use std::fmt::Write as _;

use rhizoid_io::Value;
use rhizoid_io::columnar::{Column, Interpretation};
use rhizoid_io::streamer::Scalar;

/// JSON string with ASCII escapes, as Python's `json.dumps`.
pub(crate) fn json(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The scalar leaves of a value; `None` if it holds undecoded bytes.
pub(crate) fn leaves(v: &Value, out: &mut Vec<Leaf>) -> Option<()> {
    match v {
        Value::Null => {}
        Value::String(s) => out.push(Leaf::Str(json(s))),
        Value::Array(a) => {
            for i in 0..a.len() {
                leaves(&a.get(i)?, out)?;
            }
        }
        Value::List(items) => {
            for item in items {
                leaves(item, out)?;
            }
        }
        // uproot exposes only the referenced object's id.
        Value::Object(o) if o.class() == "TRef" => {
            out.push(Leaf::Int(i128::from(o.member("fUniqueID").ok()?.as_i64()?)));
        }
        Value::Object(o) => {
            // uproot's objects leave out their TObject part.
            for b in o.bases().iter().filter(|b| b.class() != "TObject") {
                leaves(&Value::Object(Box::new(b.clone())), out)?;
            }
            for (_, m) in o.members() {
                leaves(m, out)?;
            }
        }
        Value::Opaque { .. } => return None,
        Value::Bool(b) => out.push(Leaf::Bool(*b)),
        Value::F32(_) | Value::F64(_) => out.push(Leaf::Float(v.as_f64()?)),
        Value::U64(u) => out.push(Leaf::Int(i128::from(*u))),
        v => out.push(Leaf::Int(i128::from(v.as_i64()?))),
    }
    Some(())
}

#[derive(Debug, PartialEq)]
pub(crate) enum Leaf {
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
}

impl Leaf {
    /// Floats compare exactly, or within `tolerance` (relative) for packed
    /// types, where uproot approximates ROOT's formulas.
    #[expect(
        clippy::float_cmp,
        reason = "exact equality is the point, tolerance aside"
    )]
    pub(crate) fn matches(&self, theirs: &str, tolerance: f64) -> bool {
        match self {
            Self::Bool(b) => theirs == if *b { "True" } else { "False" },
            Self::Int(i) => theirs.parse::<i128>().ok() == Some(*i),
            Self::Float(f) => theirs.parse::<f64>().is_ok_and(|t| {
                t == *f
                    || (t.is_nan() && f.is_nan())
                    || (t - f).abs() <= tolerance * t.abs().max(1.0)
            }),
            Self::Str(s) => theirs == s,
        }
    }
}

/// The relative tolerance for a branch: packed floats (`Float16_t`,
/// `Double32_t`) and objects, which may hold them.
pub(crate) fn tolerance(i: &Interpretation) -> f64 {
    let packed = |s: &Scalar| matches!(s, Scalar::Float16(_) | Scalar::Double32(_));
    match i {
        Interpretation::Primitive(s)
        | Interpretation::FixedArray { scalar: s, .. }
        | Interpretation::Jagged { scalar: s, .. }
            if packed(s) =>
        {
            1e-6
        }
        Interpretation::Objects { .. } => 1e-6,
        _ => 0.0,
    }
}

/// Per-entry leaves of a column; `None` if some value is undecoded.
pub(crate) fn column_leaves(c: &Column) -> Option<Vec<Vec<Leaf>>> {
    let mut out = Vec::with_capacity(c.len());
    for i in 0..c.len() {
        let mut entry = Vec::new();
        match c {
            Column::Values(a) => leaves(&a.get(i)?, &mut entry)?,
            Column::Fixed { len, values } => {
                for j in i * len..(i + 1) * len {
                    leaves(&values.get(j)?, &mut entry)?;
                }
            }
            Column::Jagged { offsets, values } => {
                for j in *offsets.get(i)?..*offsets.get(i + 1)? {
                    leaves(&values.get(j)?, &mut entry)?;
                }
            }
            Column::Strings(s) => entry.push(Leaf::Str(json(s.get(i)?))),
            Column::Objects(v) => leaves(v.get(i)?, &mut entry)?,
            _ => return None,
        }
        out.push(entry);
    }
    Some(out)
}
