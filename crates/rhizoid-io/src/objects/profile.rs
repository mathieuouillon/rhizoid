//! Profiles as data: [`TProfile`], [`TProfile2D`], [`TProfile3D`].
//!
//! A profile is a histogram whose cells hold sums over the entries that
//! fell in them: the cells (`fArray`) are Σw·v of the profiled value `v`,
//! `fSumw2` is Σw·v², the bin entries are Σw and the bin sums of squared
//! weights Σw². The model keeps these sums; it does not divide them.

use crate::core::error::Result;
use crate::streamer::{DynObject, FromDyn, RootClass, ToDyn};

use super::axis::TAxis;
use super::function::TF1;
use super::hist::{Core, HistStats, check_len, common, tarray};

/// How a profile computes its errors (`EErrorType`, `fErrorMode`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorMode {
    /// The error of the mean (option `""`). ROOT's default.
    #[default]
    Mean,
    /// The spread (option `"s"`).
    Spread,
    /// The spread, for integer values (option `"i"`).
    SpreadI,
    /// Gaussian errors from the weights (option `"g"`).
    SpreadG,
    /// A value this version does not know.
    Other(i32),
}

impl ErrorMode {
    fn from_i32(v: i32) -> Self {
        match v {
            0 => Self::Mean,
            1 => Self::Spread,
            2 => Self::SpreadI,
            3 => Self::SpreadG,
            other => Self::Other(other),
        }
    }

    fn to_i32(self) -> i32 {
        match self {
            Self::Mean => 0,
            Self::Spread => 1,
            Self::SpreadI => 2,
            Self::SpreadG => 3,
            Self::Other(v) => v,
        }
    }

    /// ROOT's option letter: `""`, `"s"`, `"i"`, `"g"`.
    pub fn option(self) -> &'static str {
        match self {
            Self::Spread => "s",
            Self::SpreadI => "i",
            Self::SpreadG => "g",
            Self::Mean | Self::Other(_) => "",
        }
    }
}

/// What a profile adds to its histogram.
#[derive(Clone, Debug, Default, PartialEq)]
struct Sums {
    bin_entries: Vec<f64>,
    bin_sumw2: Vec<f64>,
    error_mode: ErrorMode,
    /// The range of the profiled value (`fYmin, fYmax` …); `(0, 0)` when
    /// not set.
    range: (f64, f64),
    /// Σw·v and Σw·v² over in-range entries (`fTsumwy, fTsumwy2` …).
    tsumwv: f64,
    tsumwv2: f64,
}

/// The members of each dimension's profile: range, then sums.
const MEMBERS: [[&str; 4]; 3] = [
    ["fYmin", "fYmax", "fTsumwy", "fTsumwy2"],
    ["fZmin", "fZmax", "fTsumwz", "fTsumwz2"],
    ["fTmin", "fTmax", "fTsumwt", "fTsumwt2"],
];

impl Sums {
    fn from_dyn(obj: &DynObject, dim: usize, cells: usize) -> Result<Self> {
        let [min, max, sum, sum2] = MEMBERS[dim - 1];
        let s = Self {
            bin_entries: tarray(obj, "fBinEntries")?,
            bin_sumw2: tarray(obj, "fBinSumw2")?,
            error_mode: ErrorMode::from_i32(i32::try_from(obj.member("fErrorMode")?)?),
            range: (obj.f64(min)?, obj.f64(max)?),
            tsumwv: obj.f64(sum)?,
            tsumwv2: obj.f64(sum2)?,
        };
        check_len("bin entries", s.bin_entries.len(), cells, true)?;
        check_len(
            "bin sums of squared weights",
            s.bin_sumw2.len(),
            cells,
            true,
        )?;
        Ok(s)
    }

    fn write(&self, o: &mut DynObject, dim: usize) {
        let [min, max, sum, sum2] = MEMBERS[dim - 1];
        o.set(
            "fBinEntries",
            DynObject::new("TArrayD").with("fArray", self.bin_entries.clone()),
        )
        .set(
            "fBinSumw2",
            DynObject::new("TArrayD").with("fArray", self.bin_sumw2.clone()),
        )
        .set("fErrorMode", self.error_mode.to_i32())
        .set(min, self.range.0)
        .set(max, self.range.1)
        .set(sum, self.tsumwv)
        .set(sum2, self.tsumwv2);
    }
}

/// Methods every profile has.
macro_rules! profile_common {
    () => {
        common!();

        /// Σw per cell (`fBinEntries`), with under- and overflow.
        pub fn bin_entries(&self) -> &[f64] {
            &self.1.bin_entries
        }

        /// Replaces Σw per cell.
        ///
        /// # Errors
        ///
        /// `InvalidInput` unless there is one per cell.
        pub fn set_bin_entries(&mut self, v: Vec<f64>) -> Result<()> {
            check_len("bin entries", v.len(), self.0.ncells(), false)?;
            self.1.bin_entries = v;
            Ok(())
        }

        /// Σw² per cell (`fBinSumw2`); empty when not stored (unit weights).
        pub fn bin_sumw2(&self) -> &[f64] {
            &self.1.bin_sumw2
        }

        /// Replaces Σw² per cell (one per cell, or none).
        ///
        /// # Errors
        ///
        /// `InvalidInput` unless there is one per cell, or none.
        pub fn set_bin_sumw2(&mut self, v: Vec<f64>) -> Result<()> {
            check_len(
                "bin sums of squared weights",
                v.len(),
                self.0.ncells(),
                true,
            )?;
            self.1.bin_sumw2 = v;
            Ok(())
        }

        /// How errors are computed from the sums.
        pub fn error_mode(&self) -> ErrorMode {
            self.1.error_mode
        }

        /// Sets how errors are computed.
        pub fn set_error_mode(&mut self, mode: ErrorMode) {
            self.1.error_mode = mode;
        }

        /// The range of the profiled value; `(0, 0)` when not set.
        pub fn value_range(&self) -> (f64, f64) {
            self.1.range
        }

        /// Sets the range of the profiled value.
        pub fn set_value_range(&mut self, min: f64, max: f64) {
            self.1.range = (min, max);
        }

        /// Σw·v and Σw·v² of the profiled value over in-range entries.
        pub fn value_sums(&self) -> (f64, f64) {
            (self.1.tsumwv, self.1.tsumwv2)
        }

        /// Sets Σw·v and Σw·v².
        pub fn set_value_sums(&mut self, sum: f64, sum2: f64) {
            self.1.tsumwv = sum;
            self.1.tsumwv2 = sum2;
        }
    };
}

macro_rules! profile {
    ($(#[$doc:meta])* $name:ident, $dim:literal, ($($axis:ident),*)) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq)]
        pub struct $name(Core<f64>, Sums);

        impl $name {
            /// A profile over the given axes, every sum 0.
            pub fn new(name: impl Into<String>, title: impl Into<String>, $($axis: TAxis),*) -> Self {
                let mut axes = [$($axis),*].into_iter();
                let mut next = || axes.next().unwrap_or_else(|| TAxis::unit(""));
                let axes = [next(), next(), next()];
                let core = Core::new(stringify!($name).to_owned(), name.into(), title.into(), axes, $dim);
                let cells = core.ncells();
                Self(core, Sums { bin_entries: vec![0.0; cells], ..Sums::default() })
            }

            profile_common!();
        }

        impl RootClass for $name {
            const CLASS_NAME: &'static str = stringify!($name);
        }

        impl FromDyn for $name {
            fn from_dyn(obj: &DynObject) -> Result<Self> {
                let core = Core::<f64>::from_dyn(obj, Self::CLASS_NAME, $dim)?;
                let sums = Sums::from_dyn(obj, $dim, core.ncells())?;
                let mut core = core;
                if let Some(o) = core.origin.as_deref_mut() {
                    o.set("fBinEntries", DynObject::new("TArrayD"))
                        .set("fBinSumw2", DynObject::new("TArrayD"));
                }
                Ok(Self(core, sums))
            }
        }

        impl ToDyn for $name {
            fn to_dyn(&self) -> DynObject {
                let mut o = self.0.to_dyn();
                self.1.write(&mut o, $dim);
                o
            }
        }
    };
}

profile!(
    /// A profile over one axis (`TProfile`, based on `TH1D`): per x cell,
    /// the sums of a value `y`.
    TProfile,
    1,
    (x)
);
profile!(
    /// A profile over two axes (`TProfile2D`, based on `TH2D`).
    TProfile2D,
    2,
    (x, y)
);
profile!(
    /// A profile over three axes (`TProfile3D`, based on `TH3D`).
    TProfile3D,
    3,
    (x, y, z)
);

impl TProfile2D {
    /// The y axis.
    pub fn y_axis(&self) -> &TAxis {
        &self.0.axes[1]
    }
}

impl TProfile3D {
    /// The y axis.
    pub fn y_axis(&self) -> &TAxis {
        &self.0.axes[1]
    }

    /// The z axis.
    pub fn z_axis(&self) -> &TAxis {
        &self.0.axes[2]
    }
}

// `T` in `common!` is the cell type: profiles store `f64`.
type T = f64;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_keep_their_sums() {
        let mut p = TProfile::new("p", "t", TAxis::uniform(2, 0.0, 2.0).expect("x"));
        p.set_values_flow(vec![0.0, 3.0, 4.0, 0.0]).expect("sums");
        p.set_bin_entries(vec![0.0, 1.0, 2.0, 0.0])
            .expect("entries");
        p.set_error_mode(ErrorMode::Spread);
        p.set_value_range(-1.0, 5.0);
        let back = TProfile::from_dyn(&p.to_dyn()).expect("back");
        assert_eq!(back.values_flow(), p.values_flow());
        assert_eq!(back.bin_entries(), [0.0, 1.0, 2.0, 0.0]);
        assert_eq!(back.error_mode(), ErrorMode::Spread);
        assert_eq!(back.value_range(), (-1.0, 5.0));
        assert!(p.set_bin_entries(vec![1.0]).is_err());
        assert_eq!(ErrorMode::SpreadG.option(), "g");
    }
}
