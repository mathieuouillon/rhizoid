//! Histograms as data: [`TH1`], [`TH2`], [`TH3`] in each cell type ROOT
//! stores (`C`, `S`, `I`, `L`, `F`, `D`), named as ROOT names them
//! (`TH1D` = `TH1<f64>`, `TH2C` = `TH2<i8>`, …).
//!
//! Cells are kept as ROOT stores them: every cell, with under- and
//! overflow, `x` fastest (`cell = bx + (nx + 2)·(by + (ny + 2)·bz)`).

use core::fmt;

use crate::core::error::{Error, ErrorKind, Result};
use crate::streamer::{Array, DynObject, FromDyn, RootClass, ToDyn, Value};

use super::axis::TAxis;
use super::function::TF1;

mod sealed {
    pub trait Sealed {}
}

/// The type of a histogram's cells: `i8` (`C`), `i16` (`S`), `i32` (`I`),
/// `i64` (`L`), `f32` (`F`), `f64` (`D`). Sealed.
pub trait Cell: sealed::Sealed + Copy + Default + PartialEq + fmt::Debug + 'static {
    /// ROOT's classes of each dimension (`["TH1D", "TH2D", "TH3D"]`).
    const CLASSES: [&'static str; 3];
    #[doc(hidden)]
    fn into_array(v: Vec<Self>) -> Array;
    #[doc(hidden)]
    fn from_array(a: &Array) -> Option<Vec<Self>>;
    /// The cell as `f64` (exact but for `i64` beyond 2⁵³).
    fn to_f64(self) -> f64;
}

macro_rules! cell {
    ($t:ty, $s:literal, $variant:ident, $conv:expr) => {
        impl sealed::Sealed for $t {}
        impl Cell for $t {
            const CLASSES: [&'static str; 3] =
                [concat!("TH1", $s), concat!("TH2", $s), concat!("TH3", $s)];
            fn into_array(v: Vec<Self>) -> Array {
                Array::$variant(v)
            }
            fn from_array(a: &Array) -> Option<Vec<Self>> {
                match a {
                    Array::$variant(v) => Some(v.clone()),
                    _ => None,
                }
            }
            fn to_f64(self) -> f64 {
                $conv(self)
            }
        }
    };
}

cell!(i8, "C", I8, f64::from);
cell!(i16, "S", I16, f64::from);
cell!(i32, "I", I32, f64::from);
#[expect(clippy::cast_precision_loss, reason = "documented: exact up to 2^53")]
fn i64_to_f64(x: i64) -> f64 {
    x as f64
}
cell!(i64, "L", I64, i64_to_f64);
cell!(f32, "F", F32, f64::from);
cell!(f64, "D", F64, core::convert::identity);

/// The fill statistics a histogram stores, as stored: ROOT's `fEntries`
/// and `fTsumw…` members. Those of axes a histogram does not have stay 0.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct HistStats {
    /// `fEntries`: the number of fills.
    pub entries: f64,
    /// `fTsumw`: Σw of in-range fills.
    pub tsumw: f64,
    /// `fTsumw2`: Σw².
    pub tsumw2: f64,
    /// `fTsumwx`: Σw·x.
    pub tsumwx: f64,
    /// `fTsumwx2`: Σw·x².
    pub tsumwx2: f64,
    /// `fTsumwy`: Σw·y (2D, 3D).
    pub tsumwy: f64,
    /// `fTsumwy2`: Σw·y² (2D, 3D).
    pub tsumwy2: f64,
    /// `fTsumwxy`: Σw·x·y (2D, 3D).
    pub tsumwxy: f64,
    /// `fTsumwz`: Σw·z (3D).
    pub tsumwz: f64,
    /// `fTsumwz2`: Σw·z² (3D).
    pub tsumwz2: f64,
    /// `fTsumwxz`: Σw·x·z (3D).
    pub tsumwxz: f64,
    /// `fTsumwyz`: Σw·y·z (3D).
    pub tsumwyz: f64,
}

/// The members of each dimension's statistics.
const STATS_1D: [&str; 4] = ["fTsumw", "fTsumw2", "fTsumwx", "fTsumwx2"];
const STATS_2D: [&str; 3] = ["fTsumwy", "fTsumwy2", "fTsumwxy"];
const STATS_3D: [&str; 4] = ["fTsumwz", "fTsumwz2", "fTsumwxz", "fTsumwyz"];

impl HistStats {
    fn slot(&mut self, member: &str) -> Option<&mut f64> {
        Some(match member {
            "fTsumw" => &mut self.tsumw,
            "fTsumw2" => &mut self.tsumw2,
            "fTsumwx" => &mut self.tsumwx,
            "fTsumwx2" => &mut self.tsumwx2,
            "fTsumwy" => &mut self.tsumwy,
            "fTsumwy2" => &mut self.tsumwy2,
            "fTsumwxy" => &mut self.tsumwxy,
            "fTsumwz" => &mut self.tsumwz,
            "fTsumwz2" => &mut self.tsumwz2,
            "fTsumwxz" => &mut self.tsumwxz,
            "fTsumwyz" => &mut self.tsumwyz,
            _ => return None,
        })
    }

    fn members(dim: usize) -> impl Iterator<Item = &'static str> {
        STATS_1D
            .into_iter()
            .chain(STATS_2D.into_iter().filter(move |_| dim >= 2))
            .chain(STATS_3D.into_iter().filter(move |_| dim >= 3))
    }
}

/// The data of any histogram (and the histogram part of a profile).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Core<T> {
    pub(crate) class: String,
    pub(crate) dim: usize,
    pub(crate) name: String,
    pub(crate) title: String,
    /// x, y, z; the axes a histogram does not have are one bin over
    /// `[0, 1)`, as ROOT writes them.
    pub(crate) axes: [TAxis; 3],
    pub(crate) values: Vec<T>,
    pub(crate) sumw2: Vec<f64>,
    pub(crate) stats: HistStats,
    /// The object read, without its bulky arrays: what the model does not
    /// expose is written back unchanged.
    pub(crate) origin: Option<Box<DynObject>>,
}

fn nbins(a: &TAxis) -> usize {
    usize::try_from(a.nbins()).unwrap_or(usize::MAX)
}

impl<T: Cell> Core<T> {
    pub(crate) fn new(
        class: String,
        name: String,
        title: String,
        axes: [TAxis; 3],
        dim: usize,
    ) -> Self {
        let [x, y, z] = axes;
        let mut c = Self {
            class,
            dim,
            name,
            title,
            axes: [
                x.named("xaxis"),
                if dim >= 2 {
                    y.named("yaxis")
                } else {
                    TAxis::unit("yaxis")
                },
                if dim >= 3 {
                    z.named("zaxis")
                } else {
                    TAxis::unit("zaxis")
                },
            ],
            values: Vec::new(),
            sumw2: Vec::new(),
            stats: HistStats::default(),
            origin: None,
        };
        c.values = vec![T::default(); c.ncells()];
        c
    }

    /// The number of cells, with under- and overflow.
    pub(crate) fn ncells(&self) -> usize {
        self.axes
            .iter()
            .take(self.dim)
            .map(|a| nbins(a).saturating_add(2))
            .fold(1, usize::saturating_mul)
    }

    pub(crate) fn set_values(&mut self, values: Vec<T>) -> Result<()> {
        check_len("cells", values.len(), self.ncells(), false)?;
        self.values = values;
        Ok(())
    }

    pub(crate) fn set_sumw2(&mut self, sumw2: Vec<f64>) -> Result<()> {
        check_len("sums of squared weights", sumw2.len(), self.ncells(), true)?;
        self.sumw2 = sumw2;
        Ok(())
    }

    /// The in-range cells, `x` fastest.
    pub(crate) fn in_range<U: Copy>(&self, cells: &[U]) -> Vec<U> {
        let n: Vec<usize> = (0..3)
            .map(|d| {
                if d < self.dim {
                    nbins(&self.axes[d])
                } else {
                    1
                }
            })
            .collect();
        let stride = |d: usize| -> usize {
            (0..d)
                .map(|e| if e < self.dim { n[e] + 2 } else { 1 })
                .product()
        };
        let (sy, sz) = (stride(1), stride(2));
        let off = |d: usize| usize::from(d < self.dim);
        let mut out = Vec::with_capacity(n[0] * n[1] * n[2]);
        for z in 0..n[2] {
            for y in 0..n[1] {
                for x in 0..n[0] {
                    let i = (x + off(0)) + (y + off(1)) * sy + (z + off(2)) * sz;
                    if let Some(&v) = cells.get(i) {
                        out.push(v);
                    }
                }
            }
        }
        out
    }

    /// The fitted functions of the object read (`fFunctions`).
    pub(crate) fn functions(&self) -> Vec<TF1> {
        let Some(origin) = self.origin.as_deref() else {
            return Vec::new();
        };
        let items = match origin.get("fFunctions") {
            Some(Value::Object(list)) => list
                .get("fObjects")
                .and_then(Value::as_list)
                .unwrap_or_default(),
            _ => &[],
        };
        items
            .iter()
            .filter_map(Value::as_object)
            .filter_map(|o| TF1::from_dyn(o).ok())
            .collect()
    }

    pub(crate) fn from_dyn(obj: &DynObject, class: &str, dim: usize) -> Result<Self> {
        super::expect_class(obj, class)?;
        let axis = |name: &str| -> Result<TAxis> {
            let v = obj.member(name)?;
            TAxis::from_dyn(v.as_object().ok_or_else(|| v.mismatch("an axis"))?)
        };
        let array = obj.member("fArray")?;
        let values = array
            .as_array()
            .and_then(T::from_array)
            .ok_or_else(|| array.mismatch("the class's cell array"))?;
        let mut stats = HistStats {
            entries: obj.f64("fEntries")?,
            ..HistStats::default()
        };
        for m in HistStats::members(dim) {
            if let Some(slot) = stats.slot(m) {
                *slot = obj.f64(m)?;
            }
        }
        let mut c = Self {
            class: class.to_owned(),
            dim,
            name: obj.str("fName")?.to_owned(),
            title: obj.str("fTitle")?.to_owned(),
            axes: [axis("fXaxis")?, axis("fYaxis")?, axis("fZaxis")?],
            values,
            sumw2: tarray(obj, "fSumw2")?,
            stats,
            origin: None,
        };
        let n = c.ncells();
        if c.values.len() != n || !(c.sumw2.is_empty() || c.sumw2.len() == n) {
            return Err(Error::malformed(format!(
                "{class} with {n} cells has {} values and {} sums of squares",
                c.values.len(),
                c.sumw2.len()
            )));
        }
        // Keep the rest of the object, without the bulky arrays.
        let mut origin = obj.clone();
        origin
            .set("fArray", T::into_array(Vec::new()))
            .set("fSumw2", DynObject::new("TArrayD"));
        c.origin = Some(Box::new(origin));
        Ok(c)
    }

    pub(crate) fn to_dyn(&self) -> DynObject {
        let mut out = self.origin.as_deref().cloned().unwrap_or_else(|| {
            // What ROOT's constructors set (TH1::Build with the default
            // style).
            let fresh = DynObject::new(self.class.as_str())
                .with("fBits", 0x0300_0008_u32)
                .with("fLineColor", 602)
                .with("fLineStyle", 1)
                .with("fLineWidth", 1)
                .with("fFillColor", 0)
                .with("fFillStyle", 1001)
                .with("fMarkerColor", 1)
                .with("fMarkerStyle", 1)
                .with("fMarkerSize", 1.0_f32)
                .with("fBarOffset", 0)
                .with("fBarWidth", 1000)
                .with("fMaximum", -1111.0)
                .with("fMinimum", -1111.0)
                .with("fContour", DynObject::new("TArrayD"))
                .with("fFunctions", DynObject::new("TList"))
                .with("fStatOverflows", 2);
            if self.dim == 2 {
                fresh.with("fScalefactor", 1.0)
            } else {
                fresh
            }
        });
        let [x_axis, y_axis, z_axis] = &self.axes;
        out.set("fName", self.name.as_str())
            .set("fTitle", self.title.as_str())
            .set("fNcells", i32::try_from(self.ncells()).unwrap_or(i32::MAX))
            .set("fXaxis", x_axis.to_dyn())
            .set("fYaxis", y_axis.to_dyn())
            .set("fZaxis", z_axis.to_dyn())
            .set("fEntries", self.stats.entries)
            .set(
                "fSumw2",
                DynObject::new("TArrayD").with("fArray", self.sumw2.clone()),
            )
            .set("fArray", T::into_array(self.values.clone()));
        let mut stats = self.stats;
        for member in HistStats::members(self.dim) {
            if let Some(value) = stats.slot(member) {
                out.set(member, *value);
            }
        }
        out
    }
}

/// A `TArrayD` member as a vector (empty when absent).
pub(crate) fn tarray(obj: &DynObject, member: &str) -> Result<Vec<f64>> {
    match obj.get(member) {
        Some(Value::Object(a)) => match a.get("fArray") {
            Some(v) => Vec::<f64>::try_from(v),
            None => Ok(Vec::new()),
        },
        _ => Ok(Vec::new()),
    }
}

/// Checks the length of an array given for a histogram's cells.
pub(crate) fn check_len(what: &str, got: usize, cells: usize, may_be_empty: bool) -> Result<()> {
    if got == cells || (may_be_empty && got == 0) {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{got} {what} for {cells} cells (with under- and overflow)"),
        ))
    }
}

/// Methods every histogram has.
macro_rules! common {
    () => {
        /// The name (also its key name when written under the same name).
        pub fn name(&self) -> &str {
            &self.0.name
        }

        /// The title.
        pub fn title(&self) -> &str {
            &self.0.title
        }

        /// Sets the title.
        pub fn set_title(&mut self, title: impl Into<String>) {
            self.0.title = title.into();
        }

        /// The x axis.
        pub fn x_axis(&self) -> &TAxis {
            &self.0.axes[0]
        }

        /// The x axis, to change its title.
        pub fn x_axis_mut(&mut self) -> &mut TAxis {
            &mut self.0.axes[0]
        }

        /// Every cell as stored: with under- and overflow, `x` fastest.
        pub fn values_flow(&self) -> &[T] {
            &self.0.values
        }

        /// Replaces every cell (with under- and overflow, `x` fastest).
        ///
        /// # Errors
        ///
        /// `InvalidInput` unless there is one value per cell.
        pub fn set_values_flow(&mut self, values: Vec<T>) -> Result<()> {
            self.0.set_values(values)
        }

        /// [`set_values_flow`](Self::set_values_flow), by value.
        ///
        /// # Errors
        ///
        /// As [`set_values_flow`](Self::set_values_flow).
        pub fn with_values_flow(mut self, values: Vec<T>) -> Result<Self> {
            self.0.set_values(values)?;
            Ok(self)
        }

        /// The sum of squared weights of every cell (`fSumw2`), as
        /// stored: empty when the histogram has none (unweighted fills).
        pub fn sumw2(&self) -> &[f64] {
            &self.0.sumw2
        }

        /// Replaces the sums of squared weights (one per cell, or none).
        ///
        /// # Errors
        ///
        /// `InvalidInput` unless there is one per cell, or none.
        pub fn set_sumw2(&mut self, sumw2: Vec<f64>) -> Result<()> {
            self.0.set_sumw2(sumw2)
        }

        /// [`set_sumw2`](Self::set_sumw2), by value.
        ///
        /// # Errors
        ///
        /// As [`set_sumw2`](Self::set_sumw2).
        pub fn with_sumw2(mut self, sumw2: Vec<f64>) -> Result<Self> {
            self.0.set_sumw2(sumw2)?;
            Ok(self)
        }

        /// The stored fill statistics.
        pub fn stats(&self) -> &HistStats {
            &self.0.stats
        }

        /// The stored fill statistics, to set them.
        pub fn stats_mut(&mut self) -> &mut HistStats {
            &mut self.0.stats
        }

        /// The stored number of fills (`fEntries`).
        pub fn entries(&self) -> f64 {
            self.0.stats.entries
        }

        /// The functions stored with the histogram (fits, `fFunctions`)
        /// that are `TF1`s.
        pub fn functions(&self) -> Vec<TF1> {
            self.0.functions()
        }
    };
}

pub(crate) use common;

/// A one-dimensional histogram, as ROOT stores it: `TH1C`, `TH1S`, `TH1I`,
/// `TH1L`, `TH1F`, `TH1D` (`T` = `i8`, `i16`, `i32`, `i64`, `f32`, `f64`).
///
/// A data model, as uproot's: it holds what the file stores and gives it
/// back; it does not fill or compute. Build one from data, or read one.
///
/// ```
/// use rhizoid_io::objects::{TAxis, TH1D};
///
/// let mut h = TH1D::new("h_pt", "p_{T}", TAxis::uniform(3, 0.0, 30.0)?)
///     .with_values_flow(vec![0.0, 5.0, 7.0, 2.0, 1.0])?;  // under, 3 bins, over
/// h.stats_mut().entries = 15.0;
/// assert_eq!(h.values(), [5.0, 7.0, 2.0]);
/// assert_eq!(h.edges(), [0.0, 10.0, 20.0, 30.0]);
/// # Ok::<(), rhizoid_io::Error>(())
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct TH1<T>(pub(crate) Core<T>);

/// A two-dimensional histogram: `TH2C` … `TH2D`; see [`TH1`].
#[derive(Clone, Debug, PartialEq)]
pub struct TH2<T>(pub(crate) Core<T>);

/// A three-dimensional histogram: `TH3C` … `TH3D`; see [`TH1`].
#[derive(Clone, Debug, PartialEq)]
pub struct TH3<T>(pub(crate) Core<T>);

impl<T: Cell> TH1<T> {
    /// A histogram over `x`, every cell 0.
    pub fn new(name: impl Into<String>, title: impl Into<String>, x: TAxis) -> Self {
        let unit = || TAxis::unit("");
        Self(Core::new(
            T::CLASSES[0].to_owned(),
            name.into(),
            title.into(),
            [x, unit(), unit()],
            1,
        ))
    }

    common!();

    /// The in-range bins (without under- and overflow).
    pub fn values(&self) -> &[T] {
        let n = nbins(&self.0.axes[0]);
        self.0.values.get(1..=n).unwrap_or_default()
    }

    /// The `nbins + 1` bin edges.
    pub fn edges(&self) -> Vec<f64> {
        self.0.axes[0].edges()
    }
}

impl<T: Cell> TH2<T> {
    /// A histogram over `x` and `y`, every cell 0.
    pub fn new(name: impl Into<String>, title: impl Into<String>, x: TAxis, y: TAxis) -> Self {
        Self(Core::new(
            T::CLASSES[1].to_owned(),
            name.into(),
            title.into(),
            [x, y, TAxis::unit("")],
            2,
        ))
    }

    common!();

    /// The y axis.
    pub fn y_axis(&self) -> &TAxis {
        &self.0.axes[1]
    }

    /// The y axis, to change its title.
    pub fn y_axis_mut(&mut self) -> &mut TAxis {
        &mut self.0.axes[1]
    }

    /// The in-range cells, `x` fastest: cell `(ix, iy)` (from 0) is at
    /// `ix + nx·iy`.
    pub fn values(&self) -> Vec<T> {
        self.0.in_range(&self.0.values)
    }
}

impl<T: Cell> TH3<T> {
    /// A histogram over `x`, `y` and `z`, every cell 0.
    pub fn new(
        name: impl Into<String>,
        title: impl Into<String>,
        x: TAxis,
        y: TAxis,
        z: TAxis,
    ) -> Self {
        Self(Core::new(
            T::CLASSES[2].to_owned(),
            name.into(),
            title.into(),
            [x, y, z],
            3,
        ))
    }

    common!();

    /// The y axis.
    pub fn y_axis(&self) -> &TAxis {
        &self.0.axes[1]
    }

    /// The y axis, to change its title.
    pub fn y_axis_mut(&mut self) -> &mut TAxis {
        &mut self.0.axes[1]
    }

    /// The z axis.
    pub fn z_axis(&self) -> &TAxis {
        &self.0.axes[2]
    }

    /// The z axis, to change its title.
    pub fn z_axis_mut(&mut self) -> &mut TAxis {
        &mut self.0.axes[2]
    }

    /// The in-range cells, `x` fastest, then `y`, then `z`.
    pub fn values(&self) -> Vec<T> {
        self.0.in_range(&self.0.values)
    }
}

/// What every histogram model offers a histogram library converting from
/// or to it: the axes, the cells and sums of squares as stored, and the
/// stored statistics.
pub trait Histogram {
    /// 1, 2 or 3.
    fn dimension(&self) -> usize;
    /// The axes, x first.
    fn axes(&self) -> &[TAxis];
    /// Every cell as `f64`, with under- and overflow, `x` fastest.
    fn values_flow_f64(&self) -> Vec<f64>;
    /// The sums of squared weights per cell; empty when not stored.
    fn sumw2(&self) -> &[f64];
    /// The stored fill statistics.
    fn stats(&self) -> &HistStats;
}

macro_rules! dims {
    ($($ty:ident = $dim:literal),*) => {$(
        impl<T: Cell> RootClass for $ty<T> {
            const CLASS_NAME: &'static str = T::CLASSES[$dim - 1];
        }

        impl<T: Cell> FromDyn for $ty<T> {
            fn from_dyn(obj: &DynObject) -> Result<Self> {
                Core::from_dyn(obj, Self::CLASS_NAME, $dim).map(Self)
            }
        }

        impl<T: Cell> ToDyn for $ty<T> {
            fn to_dyn(&self) -> DynObject {
                self.0.to_dyn()
            }
        }

        impl<T: Cell> Histogram for $ty<T> {
            fn dimension(&self) -> usize {
                $dim
            }
            fn axes(&self) -> &[TAxis] {
                self.0.axes.get(..$dim).unwrap_or_default()
            }
            fn values_flow_f64(&self) -> Vec<f64> {
                self.0.values.iter().map(|v| v.to_f64()).collect()
            }
            fn sumw2(&self) -> &[f64] {
                &self.0.sumw2
            }
            fn stats(&self) -> &HistStats {
                &self.0.stats
            }
        }
    )*};
}

dims!(TH1 = 1, TH2 = 2, TH3 = 3);

/// `TH1C`: one-dimensional, `i8` cells.
pub type TH1C = TH1<i8>;
/// `TH1S`: one-dimensional, `i16` cells.
pub type TH1S = TH1<i16>;
/// `TH1I`: one-dimensional, `i32` cells.
pub type TH1I = TH1<i32>;
/// `TH1L`: one-dimensional, `i64` cells.
pub type TH1L = TH1<i64>;
/// `TH1F`: one-dimensional, `f32` cells.
pub type TH1F = TH1<f32>;
/// `TH1D`: one-dimensional, `f64` cells.
pub type TH1D = TH1<f64>;
/// `TH2C`: two-dimensional, `i8` cells.
pub type TH2C = TH2<i8>;
/// `TH2S`: two-dimensional, `i16` cells.
pub type TH2S = TH2<i16>;
/// `TH2I`: two-dimensional, `i32` cells.
pub type TH2I = TH2<i32>;
/// `TH2L`: two-dimensional, `i64` cells.
pub type TH2L = TH2<i64>;
/// `TH2F`: two-dimensional, `f32` cells.
pub type TH2F = TH2<f32>;
/// `TH2D`: two-dimensional, `f64` cells.
pub type TH2D = TH2<f64>;
/// `TH3C`: three-dimensional, `i8` cells.
pub type TH3C = TH3<i8>;
/// `TH3S`: three-dimensional, `i16` cells.
pub type TH3S = TH3<i16>;
/// `TH3I`: three-dimensional, `i32` cells.
pub type TH3I = TH3<i32>;
/// `TH3L`: three-dimensional, `i64` cells.
pub type TH3L = TH3<i64>;
/// `TH3F`: three-dimensional, `f32` cells.
pub type TH3F = TH3<f32>;
/// `TH3D`: three-dimensional, `f64` cells.
pub type TH3D = TH3<f64>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_are_stored_with_flow_x_fastest() {
        let x = TAxis::uniform(3, 0.0, 3.0).expect("x");
        let y = TAxis::variable(&[0.0, 1.0, 10.0]).expect("y");
        let h = TH2F::new("h2", "", x, y);
        assert_eq!(h.values_flow().len(), 5 * 4);
        let cells: Vec<f32> = (0..20_u8).map(f32::from).collect();
        let h = h.with_values_flow(cells).expect("cells");
        // Row by = 1 starts at 5; in-range x are 1..=3.
        assert_eq!(h.values(), [6.0, 7.0, 8.0, 11.0, 12.0, 13.0]);
        assert!(h.clone().with_values_flow(vec![0.0; 3]).is_err());
        assert!(h.clone().with_sumw2(vec![1.0; 2]).is_err());
        assert!(h.with_sumw2(Vec::new()).is_ok());
    }

    #[test]
    fn three_d_in_range_cells() {
        let a = || TAxis::uniform(2, 0.0, 2.0).expect("axis");
        let cells: Vec<i64> = (0..64).collect();
        let h = TH3L::new("h3", "", a(), a(), a())
            .with_values_flow(cells)
            .expect("cells");
        // Strides 1, 4, 16; in-range indices 1..=2 on each axis.
        assert_eq!(h.values(), [21, 22, 25, 26, 37, 38, 41, 42]);
    }

    #[test]
    fn models_convert_to_and_from_dyn() {
        let mut h = TH1F::new("h", "title", TAxis::variable(&[0.0, 1.0, 3.0]).expect("x"))
            .with_values_flow(vec![1.0, 2.0, 3.0, 4.0])
            .expect("cells")
            .with_sumw2(vec![1.0, 2.0, 3.0, 4.0])
            .expect("sumw2");
        h.stats_mut().entries = 9.0;
        h.stats_mut().tsumwx = 2.5;
        h.x_axis_mut().set_title("x [GeV]");
        let back = TH1F::from_dyn(&h.to_dyn()).expect("back");
        assert_eq!(back.values_flow(), h.values_flow());
        assert_eq!(back.sumw2(), h.sumw2());
        assert_eq!(back.stats(), h.stats());
        assert_eq!(back.x_axis().title(), "x [GeV]");
        assert_eq!(back.edges(), [0.0, 1.0, 3.0]);
        let e = TH1D::from_dyn(&h.to_dyn()).expect_err("wrong class");
        assert_eq!(e.kind(), ErrorKind::ClassMismatch);
    }
}
