//! `TAxis`.

use crate::core::error::{Error, ErrorKind, Result};
use crate::streamer::{Array, DynObject, FromDyn, RootClass, ToDyn, Value};

/// A histogram axis: uniform or variable bins, plus under- and overflow.
///
/// Bins are numbered as in ROOT: 0 is the underflow, `1..=nbins` the bins,
/// `nbins + 1` the overflow.
#[derive(Clone, Debug, PartialEq)]
pub struct TAxis {
    name: String,
    title: String,
    nbins: u32,
    min: f64,
    max: f64,
    /// Bin edges of a variable axis; empty for a uniform one.
    bins: Vec<f64>,
    labels: Vec<(u32, String)>,
    origin: Option<Box<DynObject>>,
}

impl TAxis {
    /// A uniform axis of `nbins` bins over `[min, max)`.
    ///
    /// # Errors
    ///
    /// `InvalidInput` if `nbins` is 0 or `min < max` does not hold for
    /// finite bounds.
    pub fn uniform(nbins: u32, min: f64, max: f64) -> Result<Self> {
        // ROOT stores the bin count as an `i32`.
        let too_many = i32::try_from(nbins).is_err();
        if nbins == 0 || too_many || !(min.is_finite() && max.is_finite() && min < max) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("invalid axis: {nbins} bins over [{min}, {max})"),
            ));
        }
        Ok(Self {
            name: String::new(),
            title: String::new(),
            nbins,
            min,
            max,
            bins: Vec::new(),
            labels: Vec::new(),
            origin: None,
        })
    }

    /// A variable axis with the given bin edges.
    ///
    /// # Errors
    ///
    /// `InvalidInput` unless there are at least two finite, strictly
    /// increasing edges.
    pub fn variable(edges: &[f64]) -> Result<Self> {
        let ok = edges.len() >= 2
            && edges.iter().all(|e| e.is_finite())
            && edges.windows(2).all(|w| matches!(w, [a, b] if a < b));
        let (Some(&min), Some(&max), true) = (edges.first(), edges.last(), ok) else {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "axis edges must be at least two finite, strictly increasing values",
            ));
        };
        let nbins = u32::try_from(edges.len() - 1)
            .map_err(|_| Error::new(ErrorKind::InvalidInput, "too many bins"))?;
        Ok(Self {
            bins: edges.to_vec(),
            ..Self::uniform(nbins, min, max)?
        })
    }

    /// One bin over `[0, 1)`: the unused axes of a histogram.
    pub(crate) fn unit(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            title: String::new(),
            nbins: 1,
            min: 0.0,
            max: 1.0,
            bins: Vec::new(),
            labels: Vec::new(),
            origin: None,
        }
    }

    pub(crate) fn named(mut self, name: &str) -> Self {
        name.clone_into(&mut self.name);
        self
    }

    /// Sets the title.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// The axis name (`xaxis`, `yaxis`, `zaxis`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The axis title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Sets the axis title.
    pub fn set_title(&mut self, title: impl Into<String>) {
        self.title = title.into();
    }

    /// The number of bins, without under- and overflow.
    pub fn nbins(&self) -> u32 {
        self.nbins
    }

    /// The lower edge of the first bin.
    pub fn min(&self) -> f64 {
        self.min
    }

    /// The upper edge of the last bin.
    pub fn max(&self) -> f64 {
        self.max
    }

    /// The bin labels (`fLabels`), as `(bin, label)`; empty for an axis
    /// without labels. Read from files and written back.
    pub fn labels(&self) -> &[(u32, String)] {
        &self.labels
    }

    /// Whether the bins have different widths.
    pub fn is_variable(&self) -> bool {
        !self.bins.is_empty()
    }

    /// The `nbins + 1` bin edges, as ROOT computes them (`GetBinLowEdge`,
    /// and `GetBinUpEdge` of the last bin).
    pub fn edges(&self) -> Vec<f64> {
        if self.is_variable() {
            return self.bins.clone();
        }
        // As `GetBinLowEdge(i + 1)`, and `GetBinUpEdge(n)` for the last:
        // `min + i·width` (not `max`, which may differ in the last bit).
        let width = (self.max - self.min) / f64::from(self.nbins);
        (0..=self.nbins)
            .map(|i| self.min + f64::from(i) * width)
            .collect()
    }
}

impl RootClass for TAxis {
    const CLASS_NAME: &'static str = "TAxis";
}

impl FromDyn for TAxis {
    fn from_dyn(obj: &DynObject) -> Result<Self> {
        super::expect_class(obj, Self::CLASS_NAME)?;
        let nbins = u32::try_from(obj.member("fNbins")?)?;
        let min = obj.f64("fXmin")?;
        let max = obj.f64("fXmax")?;
        let bins = match obj.get("fXbins") {
            Some(Value::Object(a)) => Vec::<f64>::try_from(a.member("fArray")?)?,
            _ => Vec::new(),
        };
        if !bins.is_empty() && bins.len() != usize::try_from(nbins).unwrap_or(0) + 1 {
            return Err(Error::malformed(format!(
                "axis with {nbins} bins has {} edges",
                bins.len()
            )));
        }
        let mut origin = obj.clone();
        origin.set(
            "fXbins",
            DynObject::new("TArrayD").with("fArray", Array::F64(Vec::new())),
        );
        // Labels: TObjStrings whose fUniqueID is the bin.
        let labels = match obj.get("fLabels") {
            Some(Value::Object(list)) => list
                .get("fObjects")
                .and_then(Value::as_list)
                .unwrap_or_default()
                .iter()
                .filter_map(Value::as_object)
                .filter_map(|s| {
                    let bin = u32::try_from(s.member("fUniqueID").ok()?).ok()?;
                    Some((bin, s.str("fString").ok()?.to_owned()))
                })
                .collect(),
            _ => Vec::new(),
        };
        Ok(Self {
            name: obj.str("fName").unwrap_or_default().to_owned(),
            title: obj.str("fTitle").unwrap_or_default().to_owned(),
            nbins,
            min,
            max,
            bins,
            labels,
            origin: Some(Box::new(origin)),
        })
    }
}

impl ToDyn for TAxis {
    fn to_dyn(&self) -> DynObject {
        let mut o = self.origin.as_deref().cloned().unwrap_or_else(|| {
            // ROOT's TAttAxis defaults.
            DynObject::new(Self::CLASS_NAME)
                .with("fNdivisions", 510)
                .with("fAxisColor", 1)
                .with("fLabelColor", 1)
                .with("fLabelFont", 42)
                .with("fLabelOffset", 0.005_f32)
                .with("fLabelSize", 0.035_f32)
                .with("fTickLength", 0.03_f32)
                .with("fTitleOffset", 1.0_f32)
                .with("fTitleSize", 0.035_f32)
                .with("fTitleColor", 1)
                .with("fTitleFont", 42)
        });
        o.set("fName", self.name.as_str())
            .set("fTitle", self.title.as_str())
            .set("fNbins", i32::try_from(self.nbins).unwrap_or(i32::MAX))
            .set("fXmin", self.min)
            .set("fXmax", self.max)
            .set(
                "fXbins",
                DynObject::new("TArrayD").with("fArray", self.bins.clone()),
            );
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_axes_refuse_what_root_cannot_store() {
        assert!(TAxis::uniform(1, 0.0, 1.0).is_ok());
        assert!(TAxis::uniform(0x7FFF_FFFF, 0.0, 1.0).is_ok());
        for (n, lo, hi) in [
            (0, 0.0, 1.0),
            (0x8000_0000, 0.0, 1.0),
            (u32::MAX, 0.0, 1.0),
            (3, 1.0, 1.0),
            (3, 2.0, 1.0),
            (3, f64::NAN, 1.0),
            (3, 0.0, f64::INFINITY),
        ] {
            let err = TAxis::uniform(n, lo, hi).expect_err("refused");
            assert_eq!(err.kind(), ErrorKind::InvalidInput, "{n} {lo} {hi}");
        }
    }

    #[test]
    fn edges_are_root_s() {
        let a = TAxis::uniform(10, 0.0, 10.0).expect("axis");
        assert_eq!(a.edges().len(), 11);
        let v = TAxis::variable(&[-1.0, -0.5, 0.0, 0.25, 0.5, 1.0]).expect("axis");
        assert_eq!(v.nbins(), 5);
        assert_eq!(v.edges(), [-1.0, -0.5, 0.0, 0.25, 0.5, 1.0]);
    }

    #[test]
    fn invalid_axes_are_rejected() {
        assert!(TAxis::uniform(0, 0.0, 1.0).is_err());
        assert!(TAxis::uniform(1, 1.0, 1.0).is_err());
        assert!(TAxis::uniform(1, 0.0, f64::INFINITY).is_err());
        assert!(TAxis::variable(&[0.0]).is_err());
        assert!(TAxis::variable(&[0.0, 0.0]).is_err());
    }
}
