//! Data models of ROOT's common classes: histograms ([`Hist1`],
//! [`Hist2`], [`Hist3`] in every cell type), profiles ([`Profile1`],
//! [`Profile2`], [`Profile3`]), graphs ([`Graph`], [`GraphErrors`],
//! [`GraphAsymErrors`]), functions ([`Function1`]), [`Axis`], [`Object`],
//! [`Named`] and [`StringObject`].
//!
//! **Models, not a histogram library**, as uproot's are: each holds what
//! the file stores (cells with under- and overflow, sums of squared
//! weights, the stored fill statistics, axes, points, parameters) and gives
//! it back. Nothing fills, rebins or computes. Converting to and from a
//! histogram, graph or function library (boost-histogram and the like)
//! belongs in adapter crates; [`Histogram`] is what every histogram offers
//! them.
//!
//! Each model reads any version of its class through the class description
//! in the file ([`FromDyn`](crate::FromDyn)) and writes the version embedded
//! in this crate, captured from ROOT 6.42 ([`ToDyn`](crate::ToDyn)). A model
//! read from a file keeps the members it does not expose (line colours,
//! axis labels, fitted functions…) and writes them back unchanged.
//!
//! ```
//! use rhizoid_io::objects::{Axis, Hist1};
//!
//! let mut h = Hist1::<f64>::new("h_pt", "p_{T}", Axis::uniform(4, 0.0, 200.0)?)
//!     .with_values_flow(vec![0.0, 2.0, 5.0, 1.0, 0.0, 1.0])?; // under, 4 bins, over
//! h.stats_mut().entries = 9.0;
//! assert_eq!(h.values(), [2.0, 5.0, 1.0, 0.0]);
//! assert_eq!(h.values_flow().last(), Some(&1.0)); // overflow
//! # Ok::<(), rhizoid_io::Error>(())
//! ```

mod axis;
mod function;
mod graph;
mod hist;
mod named;
mod object;
mod profile;
mod string;

pub use axis::Axis;
pub use function::Function1;
pub use graph::{Graph, GraphAsymErrors, GraphErrors};
pub use hist::{Cell, Hist1, Hist2, Hist3, HistStats, Histogram};
pub use named::Named;
pub use object::Object;
pub use profile::{ErrorMode, Profile1, Profile2, Profile3};
pub use string::StringObject;

use crate::core::error::{Error, ErrorKind, Result};
use crate::streamer::DynObject;

/// Checks the class of an object a model is built from.
fn expect_class(obj: &DynObject, class: &str) -> Result<()> {
    if obj.class() == class {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::ClassMismatch,
            format!("the object is a {}, not a {class}", obj.class()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streamer::RootClass;

    /// The Rust names are ours; the class names are ROOT's and in every file.
    #[test]
    fn models_keep_roots_class_names() {
        let names = [
            (Axis::CLASS_NAME, "TAxis"),
            (Function1::CLASS_NAME, "TF1"),
            (Graph::CLASS_NAME, "TGraph"),
            (GraphErrors::CLASS_NAME, "TGraphErrors"),
            (GraphAsymErrors::CLASS_NAME, "TGraphAsymmErrors"),
            (Named::CLASS_NAME, "TNamed"),
            (StringObject::CLASS_NAME, "TObjString"),
            (Profile1::CLASS_NAME, "TProfile"),
            (Profile2::CLASS_NAME, "TProfile2D"),
            (Profile3::CLASS_NAME, "TProfile3D"),
            (Hist1::<i8>::CLASS_NAME, "TH1C"),
            (Hist1::<i16>::CLASS_NAME, "TH1S"),
            (Hist1::<i32>::CLASS_NAME, "TH1I"),
            (Hist1::<i64>::CLASS_NAME, "TH1L"),
            (Hist1::<f32>::CLASS_NAME, "TH1F"),
            (Hist1::<f64>::CLASS_NAME, "TH1D"),
            (Hist2::<f32>::CLASS_NAME, "TH2F"),
            (Hist2::<f64>::CLASS_NAME, "TH2D"),
            (Hist3::<i32>::CLASS_NAME, "TH3I"),
            (Hist3::<f64>::CLASS_NAME, "TH3D"),
        ];
        for (ours, root) in names {
            assert_eq!(ours, root);
        }
        // Written objects carry it too.
        let p = Profile1::new("p", "t", Axis::uniform(2, 0.0, 1.0).expect("axis"));
        assert_eq!(crate::ToDyn::to_dyn(&p).class(), "TProfile");
    }
}
