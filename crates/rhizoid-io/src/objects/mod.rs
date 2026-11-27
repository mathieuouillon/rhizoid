//! Data models of ROOT's common classes: histograms ([`TH1`],
//! [`TH2`], [`TH3`] in every cell type), profiles ([`TProfile`],
//! [`TProfile2D`], [`TProfile3D`]), graphs ([`TGraph`], [`TGraphErrors`],
//! [`TGraphAsymmErrors`]), functions ([`TF1`]), [`TAxis`], [`TObject`],
//! [`TNamed`] and [`TObjString`].
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
//! use rhizoid_io::objects::{TAxis, TH1D};
//!
//! let mut h = TH1D::new("h_pt", "p_{T}", TAxis::uniform(4, 0.0, 200.0)?)
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

pub use axis::TAxis;
pub use function::TF1;
pub use graph::{TGraph, TGraphAsymmErrors, TGraphErrors};
pub use hist::{
    Cell, HistStats, Histogram, TH1, TH1C, TH1D, TH1F, TH1I, TH1L, TH1S, TH2, TH2C, TH2D, TH2F,
    TH2I, TH2L, TH2S, TH3, TH3C, TH3D, TH3F, TH3I, TH3L, TH3S,
};
pub use named::TNamed;
pub use object::TObject;
pub use profile::{ErrorMode, TProfile, TProfile2D, TProfile3D};
pub use string::TObjString;

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
