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

/// The class-description list and objects decoded with it, for the fuzz
/// targets. The first byte chooses: a list (its descriptions then decode
/// the same bytes as objects), or an object of a built-in class.
#[cfg(feature = "_fuzz")]
pub(crate) fn fuzz_decode(data: &[u8]) {
    const CLASSES: [&str; 12] = [
        "TH1D",
        "TH2F",
        "TProfile",
        "TGraphAsymmErrors",
        "TF1",
        "TTree",
        "TBranchElement",
        "TLeafF",
        "TList",
        "TObjArray",
        "TNamed",
        "TObjString",
    ];
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let decode = |set: &StreamerSet, class: &str| {
        let mut d = Decoder::new(set, builtin(), OnMismatch::Reposition);
        let _ = d.object(&mut crate::core::buffer::RBuffer::new(rest, 0), class);
    };
    if mode & 1 == 0 {
        if let Ok((infos, _)) = decode_list(rest, 0) {
            let names: Vec<String> = infos
                .iter()
                .take(8)
                .map(|i| i.class_name().to_owned())
                .collect();
            let set = StreamerSet::new(infos);
            for class in &names {
                decode(&set, class);
            }
        }
    } else {
        let class = CLASSES[usize::from(mode >> 1) % CLASSES.len()];
        decode(&StreamerSet::default(), class);
    }
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

    /// Objects encoded with the built-in descriptions decode back to the
    /// same model (strict: any byte-count mismatch fails).
    mod round_trip {
        use proptest::prelude::*;

        use super::*;
        use crate::core::buffer::{RBuffer, WBuffer};
        use crate::objects::{Axis, Hist1, Named, StringObject};
        use crate::streamer::info::Packing;

        fn trip(obj: &DynObject) -> DynObject {
            let mut w = WBuffer::with_origin(100);
            Encoder::new(builtin()).top(&mut w, obj).expect("encode");
            let bytes = w.into_inner();
            let set = StreamerSet::default();
            let mut d = Decoder::new(&set, builtin(), OnMismatch::Fail);
            d.object(&mut RBuffer::new(&bytes, 100), obj.class())
                .expect("decode")
        }

        fn encode(obj: &DynObject) -> Result<Vec<u8>> {
            let mut w = WBuffer::with_origin(100);
            Encoder::new(builtin()).top(&mut w, obj)?;
            Ok(w.into_inner())
        }

        fn refused(obj: &DynObject, member: &str) {
            let err = encode(obj).expect_err("refused");
            assert_eq!(err.kind(), crate::ErrorKind::InvalidInput, "{err}");
            assert!(err.to_string().contains(member), "{member}: {err}");
        }

        #[test]
        fn malformed_objects_are_refused_naming_the_member() {
            use crate::objects::Graph;
            // An array whose count member says otherwise.
            let mut g = Graph::new("g", "t", vec![1.0, 2.0, 3.0], vec![4.0, 5.0, 6.0])
                .expect("graph")
                .to_dyn();
            g.set("fNpoints", 5_i32);
            refused(&g, "fX");
            let mut g = g.clone();
            g.set("fNpoints", "three");
            refused(&g, "fNpoints");
            // A string member holding a number.
            let n = Named::new("n", "t").to_dyn().with("fName", 3_i32);
            refused(&n, "fName");
            // A map entry that is not a [key, value] pair.
            let f = DynObject::new("TFormula").with("fParams", Value::List(vec![Value::I32(1)]));
            refused(&f, "fParams");
            // An array where a number is expected, and the other way round.
            let h = Hist1::<f64>::new("h", "t", Axis::uniform(2, 0.0, 1.0).expect("axis"))
                .to_dyn()
                .with("fEntries", Value::Array(Array::F64(vec![1.0])));
            refused(&h, "fEntries");
            // Every member valid: written.
            assert!(encode(&Named::new("n", "t").to_dyn()).is_ok());
        }

        /// One value written packed, then read back.
        fn packed(p: Packing, v: f64) -> f64 {
            let mut w = WBuffer::with_origin(0);
            encode::write_packed(&mut w, p, v);
            let bytes = w.into_inner();
            let mut r = RBuffer::new(&bytes, 0);
            let back = decode::read_packed(&mut r, p).expect("read");
            assert_eq!(r.remaining(), 0, "{p:?} read what it wrote");
            back
        }

        #[test]
        fn packing_edge_values() {
            let p = Packing::from_title("[0, 10, 8]", false);
            assert_eq!(packed(p, 0.0), 0.0);
            // Outside the range: clamped, as ROOT does.
            assert_eq!(packed(p, -5.0), 0.0);
            assert!((packed(p, 99.0) - 10.0).abs() < 0.05);
            let f16 = Packing::from_title("", true);
            assert_eq!(packed(f16, 0.0), 0.0);
            assert_eq!(packed(f16, 1.0), 1.0);
            assert_eq!(packed(f16, -2.0), -2.0);
            assert_eq!(packed(Packing::None, 0.1), f64::from(0.1_f32));
            // Few or no mantissa bits: rough, but never a panic.
            for bits in 0..2 {
                let p = Packing::from_title(&format!("[0, 0, {bits}]"), false);
                assert!(packed(p, 3.0).is_finite());
            }
        }

        proptest! {
            #[test]
            fn range_packing_keeps_values_within_a_step(
                min in -1e6_f64..1e6,
                width in 1e-3_f64..1e6,
                bits in 2_u32..=32,
                at in -0.5_f64..1.5,
            ) {
                let max = min + width;
                let p = Packing::from_title(&format!("[{min:e}, {max:e}, {bits}]"), false);
                let Packing::Range { factor, .. } = p else {
                    return Err(TestCaseError::fail(format!("{p:?}")));
                };
                let v = min + at * width;
                let back = packed(p, v);
                let want = v.clamp(min, max);
                // Half a step of rounding, one of saturation, and f64 noise.
                let step = 1.0 / factor;
                prop_assert!(
                    (back - want).abs() <= step + 1e-9 * (min.abs() + max.abs()),
                    "{v} came back {back} ({p:?})"
                );
            }

            #[test]
            fn bit_packing_keeps_the_sign_and_nbits_of_mantissa(
                magnitude in 1e-30_f32..1e30,
                negative in any::<bool>(),
                bits in 2_u32..15,
            ) {
                let p = Packing::from_title(&format!("[0, 0, {bits}]"), false);
                prop_assert_eq!(p, Packing::Bits(bits));
                let v = f64::from(if negative { -magnitude } else { magnitude });
                let back = packed(p, v);
                prop_assert_eq!(back.is_sign_negative(), negative);
                let relative = ((back - v) / v).abs();
                prop_assert!(
                    relative <= 2_f64.powi(-i32::try_from(bits).expect("small")),
                    "{v} came back {back} with {bits} bits"
                );
            }

            #[test]
            fn named_objects_and_strings(name in ".{0,300}", title in ".{0,300}") {
                let n = Named::new(name.clone(), title.clone());
                prop_assert_eq!(Named::from_dyn(&trip(&n.to_dyn())).expect("model"), n);
                let s = StringObject::new(title.clone());
                let back = StringObject::from_dyn(&trip(&s.to_dyn())).expect("model");
                prop_assert_eq!(back.as_str(), title.as_str());
            }

            #[test]
            fn histograms_keep_every_cell(
                bins in 1_u32..40,
                min in -1e6_f64..1e6,
                width in 1e-3_f64..1e6,
                cells in prop::collection::vec(any::<f64>(), 42),
                entries in any::<f64>(),
            ) {
                let axis = Axis::uniform(bins, min, min + width).expect("axis");
                let n = usize::try_from(bins).expect("small") + 2;
                let mut h = Hist1::<f64>::new("h", "t", axis)
                    .with_values_flow(cells[..n].to_vec())
                    .expect("cells");
                h.stats_mut().entries = entries;
                let back = Hist1::<f64>::from_dyn(&trip(&h.to_dyn())).expect("model");
                let bits = |v: &[f64]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
                prop_assert_eq!(bits(back.values_flow()), bits(h.values_flow()));
                prop_assert_eq!(bits(&back.edges()), bits(&h.edges()));
                prop_assert_eq!(back.stats().entries.to_bits(), entries.to_bits());
            }
        }
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
