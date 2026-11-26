//! Graphs as data: [`TGraph`], [`TGraphErrors`], [`TGraphAsymmErrors`].

use crate::core::error::{Error, ErrorKind, Result};
use crate::streamer::{DynObject, FromDyn, RootClass, ToDyn, Value};

/// The points of any graph, and what the model does not expose.
#[derive(Clone, Debug, PartialEq)]
struct Points {
    name: String,
    title: String,
    x: Vec<f64>,
    y: Vec<f64>,
    /// `fMinimum`, `fMaximum`: the plotting range of y, -1111 when unset.
    minimum: f64,
    maximum: f64,
    origin: Option<Box<DynObject>>,
}

fn same_len(what: &str, n: usize, arrays: &[&[f64]]) -> Result<()> {
    if arrays.iter().all(|a| a.len() == n) {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::InvalidInput,
            format!("a {what} needs arrays of {n} values, one per point"),
        ))
    }
}

/// A `Double_t*` member as a vector (empty when absent).
fn doubles(obj: &DynObject, member: &str) -> Result<Vec<f64>> {
    match obj.get(member) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(v) => Vec::<f64>::try_from(v),
    }
}

impl Points {
    fn new(name: String, title: String, x: Vec<f64>, y: Vec<f64>) -> Result<Self> {
        same_len("graph", x.len(), &[&y])?;
        Ok(Self {
            name,
            title,
            x,
            y,
            minimum: -1111.0,
            maximum: -1111.0,
            origin: None,
        })
    }

    fn from_dyn(obj: &DynObject, class: &str) -> Result<Self> {
        super::expect_class(obj, class)?;
        let n = usize::try_from(i32::try_from(obj.member("fNpoints")?)?.max(0)).unwrap_or(0);
        let (x, y) = (doubles(obj, "fX")?, doubles(obj, "fY")?);
        if x.len() != n || y.len() != n {
            return Err(Error::malformed(format!(
                "a {class} of {n} points has {} x and {} y",
                x.len(),
                y.len()
            )));
        }
        let mut origin = obj.clone();
        origin
            .set("fX", Vec::<f64>::new())
            .set("fY", Vec::<f64>::new());
        Ok(Self {
            name: obj.str("fName")?.to_owned(),
            title: obj.str("fTitle")?.to_owned(),
            x,
            y,
            minimum: obj.f64("fMinimum")?,
            maximum: obj.f64("fMaximum")?,
            origin: Some(Box::new(origin)),
        })
    }

    fn to_dyn(&self, class: &str) -> DynObject {
        let mut o = self.origin.as_deref().cloned().unwrap_or_else(|| {
            // What ROOT's constructors set: TAttLine(), TAttFill(0, 1000),
            // TAttMarker(), an empty list of functions.
            DynObject::new(class)
                .with("fLineColor", 1)
                .with("fLineStyle", 1)
                .with("fLineWidth", 1)
                .with("fFillColor", 0)
                .with("fFillStyle", 1000)
                .with("fMarkerColor", 1)
                .with("fMarkerStyle", 1)
                .with("fMarkerSize", 1.0_f32)
                .with("fFunctions", DynObject::new("TList"))
        });
        o.set("fName", self.name.as_str())
            .set("fTitle", self.title.as_str())
            .set("fNpoints", i32::try_from(self.x.len()).unwrap_or(i32::MAX))
            .set("fX", self.x.clone())
            .set("fY", self.y.clone())
            .set("fMinimum", self.minimum)
            .set("fMaximum", self.maximum);
        o
    }
}

/// Methods every graph has.
macro_rules! graph_common {
    () => {
        /// The name.
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

        /// The number of points.
        pub fn len(&self) -> usize {
            self.0.x.len()
        }

        /// Whether the graph has no points.
        pub fn is_empty(&self) -> bool {
            self.0.x.is_empty()
        }

        /// The x of every point.
        pub fn x(&self) -> &[f64] {
            &self.0.x
        }

        /// The y of every point.
        pub fn y(&self) -> &[f64] {
            &self.0.y
        }

        /// The plotting range of y (`fMinimum`, `fMaximum`); ROOT writes
        /// -1111 for "not set".
        pub fn y_range(&self) -> (f64, f64) {
            (self.0.minimum, self.0.maximum)
        }

        /// Sets the plotting range of y.
        pub fn set_y_range(&mut self, minimum: f64, maximum: f64) {
            self.0.minimum = minimum;
            self.0.maximum = maximum;
        }
    };
}

/// A graph: points `(x, y)` (`TGraph`). A data model: it holds what the
/// file stores.
///
/// ```
/// use rhizoid_io::objects::TGraph;
///
/// let g = TGraph::new("g", "a graph", vec![0.0, 1.0, 2.0], vec![1.0, 3.0, 2.0])?;
/// assert_eq!(g.len(), 3);
/// assert_eq!(g.y(), [1.0, 3.0, 2.0]);
/// # Ok::<(), rhizoid_io::Error>(())
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct TGraph(Points);

impl TGraph {
    /// A graph of the points `(x[i], y[i])`.
    ///
    /// # Errors
    ///
    /// `InvalidInput` unless `x` and `y` have the same length.
    pub fn new(
        name: impl Into<String>,
        title: impl Into<String>,
        x: Vec<f64>,
        y: Vec<f64>,
    ) -> Result<Self> {
        Points::new(name.into(), title.into(), x, y).map(Self)
    }

    graph_common!();
}

/// A graph with symmetric errors (`TGraphErrors`).
#[derive(Clone, Debug, PartialEq)]
pub struct TGraphErrors(Points, Vec<f64>, Vec<f64>);

impl TGraphErrors {
    /// A graph of the points `(x[i], y[i])` with errors `ex[i]`, `ey[i]`.
    ///
    /// # Errors
    ///
    /// `InvalidInput` unless all four have the same length.
    pub fn new(
        name: impl Into<String>,
        title: impl Into<String>,
        x: Vec<f64>,
        y: Vec<f64>,
        ex: Vec<f64>,
        ey: Vec<f64>,
    ) -> Result<Self> {
        same_len("graph with errors", x.len(), &[&ex, &ey])?;
        Ok(Self(Points::new(name.into(), title.into(), x, y)?, ex, ey))
    }

    graph_common!();

    /// The x error of every point.
    pub fn ex(&self) -> &[f64] {
        &self.1
    }

    /// The y error of every point.
    pub fn ey(&self) -> &[f64] {
        &self.2
    }
}

/// A graph with asymmetric errors (`TGraphAsymmErrors`).
#[derive(Clone, Debug, PartialEq)]
pub struct TGraphAsymmErrors(Points, [Vec<f64>; 4]);

impl TGraphAsymmErrors {
    /// A graph of the points `(x[i], y[i])` with errors `[x low, x high,
    /// y low, y high]`.
    ///
    /// # Errors
    ///
    /// `InvalidInput` unless all six have the same length.
    pub fn new(
        name: impl Into<String>,
        title: impl Into<String>,
        x: Vec<f64>,
        y: Vec<f64>,
        errors: [Vec<f64>; 4],
    ) -> Result<Self> {
        let arrays: Vec<&[f64]> = errors.iter().map(Vec::as_slice).collect();
        same_len("graph with asymmetric errors", x.len(), &arrays)?;
        Ok(Self(Points::new(name.into(), title.into(), x, y)?, errors))
    }

    graph_common!();

    /// The lower x error of every point.
    pub fn ex_low(&self) -> &[f64] {
        &self.1[0]
    }

    /// The upper x error of every point.
    pub fn ex_high(&self) -> &[f64] {
        &self.1[1]
    }

    /// The lower y error of every point.
    pub fn ey_low(&self) -> &[f64] {
        &self.1[2]
    }

    /// The upper y error of every point.
    pub fn ey_high(&self) -> &[f64] {
        &self.1[3]
    }
}

const ASYMM: [&str; 4] = ["fEXlow", "fEXhigh", "fEYlow", "fEYhigh"];

impl RootClass for TGraph {
    const CLASS_NAME: &'static str = "TGraph";
}

impl FromDyn for TGraph {
    fn from_dyn(obj: &DynObject) -> Result<Self> {
        Points::from_dyn(obj, Self::CLASS_NAME).map(Self)
    }
}

impl ToDyn for TGraph {
    fn to_dyn(&self) -> DynObject {
        self.0.to_dyn(Self::CLASS_NAME)
    }
}

impl RootClass for TGraphErrors {
    const CLASS_NAME: &'static str = "TGraphErrors";
}

impl FromDyn for TGraphErrors {
    fn from_dyn(obj: &DynObject) -> Result<Self> {
        let mut p = Points::from_dyn(obj, Self::CLASS_NAME)?;
        let (ex, ey) = (doubles(obj, "fEX")?, doubles(obj, "fEY")?);
        same_len("TGraphErrors", p.x.len(), &[&ex, &ey])
            .map_err(|_| Error::malformed("a TGraphErrors with errors not one per point"))?;
        if let Some(o) = p.origin.as_deref_mut() {
            o.set("fEX", Vec::<f64>::new())
                .set("fEY", Vec::<f64>::new());
        }
        Ok(Self(p, ex, ey))
    }
}

impl ToDyn for TGraphErrors {
    fn to_dyn(&self) -> DynObject {
        let mut o = self.0.to_dyn(Self::CLASS_NAME);
        o.set("fEX", self.1.clone()).set("fEY", self.2.clone());
        o
    }
}

impl RootClass for TGraphAsymmErrors {
    const CLASS_NAME: &'static str = "TGraphAsymmErrors";
}

impl FromDyn for TGraphAsymmErrors {
    fn from_dyn(obj: &DynObject) -> Result<Self> {
        let mut p = Points::from_dyn(obj, Self::CLASS_NAME)?;
        let mut errors: [Vec<f64>; 4] = Default::default();
        for (e, m) in errors.iter_mut().zip(ASYMM) {
            *e = doubles(obj, m)?;
        }
        same_len(
            "TGraphAsymmErrors",
            p.x.len(),
            &[&errors[0], &errors[1], &errors[2], &errors[3]],
        )
        .map_err(|_| Error::malformed("a TGraphAsymmErrors with errors not one per point"))?;
        if let Some(o) = p.origin.as_deref_mut() {
            for m in ASYMM {
                o.set(m, Vec::<f64>::new());
            }
        }
        Ok(Self(p, errors))
    }
}

impl ToDyn for TGraphAsymmErrors {
    fn to_dyn(&self) -> DynObject {
        let mut o = self.0.to_dyn(Self::CLASS_NAME);
        for (e, m) in self.1.iter().zip(ASYMM) {
            o.set(m, e.clone());
        }
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphs_round_trip_through_dyn() {
        let g = TGraphAsymmErrors::new(
            "g",
            "t",
            vec![1.0, 2.0],
            vec![3.0, 4.0],
            [vec![0.1; 2], vec![0.2; 2], vec![0.3; 2], vec![0.4; 2]],
        )
        .expect("graph");
        let back = TGraphAsymmErrors::from_dyn(&g.to_dyn()).expect("back");
        assert_eq!(back.ey_high(), [0.4, 0.4]);
        assert_eq!(back.x(), [1.0, 2.0]);
        assert!(TGraph::new("g", "", vec![1.0], vec![]).is_err());
    }
}
