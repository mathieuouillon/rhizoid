//! Objects as bytes and back: class descriptions ([`StreamerInfo`]),
//! the dynamic representation ([`DynObject`]), and the traits typed models
//! implement.
//!
//! Reading interprets the class description stored in the file, so any
//! class ROOT wrote can be read without a Rust model. Writing uses the
//! descriptions embedded in this crate (captured from ROOT-written files)
//! and stores the ones it used in the file, as ROOT does.

mod decode;
mod encode;
mod frame;
mod info;
mod value;

use std::sync::{Arc, OnceLock};

pub use info::{ElementKind, Packing, Scalar, StreamerElement, StreamerInfo};
pub use value::{Array, DynObject, Value};

pub(crate) use decode::{Decoder, read_array, split_template};
pub(crate) use encode::{Encoder, REFERENCE};
pub(crate) use frame::OnMismatch;
pub(crate) use info::{StreamerSet, append_to_list, decode_list, encode_list};

use crate::core::error::Result;

/// A Rust type that models a ROOT class.
pub trait RootClass {
    /// The ROOT class name, as written in keys and class tags.
    const CLASS_NAME: &'static str;
}

/// Builds a typed model from an object decoded from its class description.
///
/// This is the evolution path of [`File::get`](crate::File::get): it works
/// for every version of the class whose members it reads by name.
pub trait FromDyn: Sized {
    /// Converts the object.
    ///
    /// # Errors
    ///
    /// `ClassMismatch` if the object is of another class; `NotFound` or
    /// `Malformed` if a member is missing or of an unexpected type.
    fn from_dyn(obj: &DynObject) -> Result<Self>;
}

/// Converts a value into the object written to a file.
pub trait ToDyn {
    /// The object to write; its class must have a known description.
    fn to_dyn(&self) -> DynObject;
}

impl FromDyn for DynObject {
    fn from_dyn(obj: &DynObject) -> Result<Self> {
        Ok(obj.clone())
    }
}

impl ToDyn for DynObject {
    fn to_dyn(&self) -> DynObject {
        self.clone()
    }
}

/// An empty array of the type `s` decodes to.
pub(crate) fn empty_array(s: Scalar) -> Array {
    match s {
        Scalar::Bool => Array::Bool(Vec::new()),
        Scalar::I8 => Array::I8(Vec::new()),
        Scalar::U8 => Array::U8(Vec::new()),
        Scalar::I16 => Array::I16(Vec::new()),
        Scalar::U16 => Array::U16(Vec::new()),
        Scalar::I32 => Array::I32(Vec::new()),
        Scalar::U32 => Array::U32(Vec::new()),
        Scalar::I64 => Array::I64(Vec::new()),
        Scalar::U64 => Array::U64(Vec::new()),
        Scalar::F32 | Scalar::Float16(_) => Array::F32(Vec::new()),
        Scalar::F64 | Scalar::Double32(_) => Array::F64(Vec::new()),
    }
}

/// The class descriptions of files written by ROOT 6.42
/// (`tests/cross/root_make.C` and `root_tree.C`, merged by the test
/// `regenerate_builtin`): every class the data models and the tree writer
/// write, and their dependencies.
const BUILTIN: &[u8] = include_bytes!("builtin.bin");
/// The key length of the record the descriptions were captured from; class
/// tags in [`BUILTIN`] count from the start of that key.
const BUILTIN_ORIGIN: u32 = 64;

/// The built-in class descriptions, decoded once.
pub(crate) fn builtin() -> &'static StreamerSet {
    static SET: OnceLock<StreamerSet> = OnceLock::new();
    SET.get_or_init(|| {
        decode_list(BUILTIN, BUILTIN_ORIGIN)
            .map(|(infos, _)| StreamerSet::new(infos))
            .unwrap_or_default()
    })
}

/// The descriptions to store for `classes`: each, then what it depends on,
/// without duplicates. A class takes the description it was written with
/// (`written`), else the newest in the first of `sets` that has one.
pub(crate) fn closure(
    written: &[Arc<StreamerInfo>],
    sets: &[&StreamerSet],
    classes: &[String],
) -> Vec<Arc<StreamerInfo>> {
    let mut out: Vec<Arc<StreamerInfo>> = Vec::new();
    let mut stack: Vec<String> = classes.iter().rev().cloned().collect();
    while let Some(class) = stack.pop() {
        if out.iter().any(|i| i.class_name() == class) {
            continue;
        }
        let found = written
            .iter()
            .find(|i| i.class_name() == class)
            .or_else(|| sets.iter().find_map(|s| s.latest(&class)));
        if let Some(info) = found {
            let deps: Vec<String> = info.dependencies().map(str::to_owned).collect();
            out.push(Arc::clone(info));
            stack.extend(deps.into_iter().rev());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With `RHIZOID_BLESS=1`, rebuilds `builtin.bin` from the files ROOT
    /// writes (`tests/cross/root_make.C`, `root_tree.C`): their descriptions,
    /// merged and encoded at [`BUILTIN_ORIGIN`].
    #[test]
    fn regenerate_builtin() {
        if std::env::var_os("RHIZOID_BLESS").is_none() {
            return;
        }
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
        let mut all: Vec<StreamerInfo> = Vec::new();
        // From the split tree, only the branch classes (not its user classes).
        let only = |name: &str, class: &str| {
            name != "root_user_tree.root" || matches!(class, "TBranchElement" | "TLeafElement")
        };
        for name in [
            "root_hists.root",
            "root_tree.root",
            "root_ntuple.root",
            "root_user_tree.root",
        ] {
            let file = crate::File::open(data.join(name)).expect("ROOT-written file");
            for info in file.streamer_infos().expect("descriptions") {
                if !only(name, info.class_name()) {
                    continue;
                }
                let seen = all.iter().any(|i| {
                    i.class_name() == info.class_name() && i.class_version() == info.class_version()
                });
                if !seen {
                    all.push(info);
                }
            }
        }
        let refs: Vec<&StreamerInfo> = all.iter().collect();
        let bytes = encode_list(&refs, BUILTIN_ORIGIN).expect("encode");
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/streamer/builtin.bin");
        std::fs::write(out, bytes).expect("write builtin.bin");
    }

    #[test]
    fn builtin_descriptions_round_trip() {
        let (infos, diags) = decode_list(BUILTIN, BUILTIN_ORIGIN).expect("decode");
        assert_eq!(diags, []);
        let names: Vec<&str> = infos.iter().map(StreamerInfo::class_name).collect();
        for class in [
            "TH1C",
            "TH1S",
            "TH1I",
            "TH1L",
            "TH1F",
            "TH1D",
            "TH2C",
            "TH2D",
            "TH3C",
            "TH3D",
            "TH1",
            "TH2",
            "TH3",
            "TProfile",
            "TProfile2D",
            "TProfile3D",
            "TGraph",
            "TGraphErrors",
            "TGraphAsymmErrors",
            "TF1",
            "TFormula",
            "TNamed",
            "TAxis",
            "TObjString",
        ] {
            assert!(names.contains(&class), "{class} missing from {names:?}");
        }
        // Compare decoded descriptions, and check our encoding is a fixed
        // point.
        let refs: Vec<&StreamerInfo> = infos.iter().collect();
        let ours = encode_list(&refs, 70).expect("encode");
        let (again, diags) = decode_list(&ours, 70).expect("decode ours");
        assert_eq!(diags, []);
        assert_eq!(again, infos);
        let refs: Vec<&StreamerInfo> = again.iter().collect();
        assert_eq!(encode_list(&refs, 70).expect("encode again"), ours);
        assert_eq!(builtin().len(), infos.len());
    }

    #[test]
    fn closure_follows_bases_and_members() {
        let set = builtin();
        let infos = closure(&[], &[set], &["TH1D".to_owned()]);
        let names: Vec<&str> = infos.iter().map(|i| i.class_name()).collect();
        assert_eq!(names.first(), Some(&"TH1D"));
        for class in [
            "TH1",
            "TNamed",
            "TObject",
            "TAttLine",
            "TAxis",
            "TAttAxis",
            "THashList",
        ] {
            assert!(names.contains(&class), "{class} missing from {names:?}");
        }
    }
}
