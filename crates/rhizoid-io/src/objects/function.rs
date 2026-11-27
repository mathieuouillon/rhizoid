//! Functions as data: [`Function1`].

use crate::core::error::Result;
use crate::streamer::{DynObject, FromDyn, RootClass, ToDyn, Value};

/// A one-dimensional function (`TF1`), as stored: its formula, parameters
/// and their errors and names, its domain, and the result of the fit that
/// set them. A data model: it does not evaluate the formula.
///
/// Read from files (on its own, or with [`Hist1::functions`](super::Hist1::functions)),
/// and written back unchanged. Building one from scratch needs ROOT's
/// formula machinery, which this crate does not reproduce.
#[derive(Clone, Debug, PartialEq)]
pub struct Function1 {
    name: String,
    title: String,
    formula: String,
    parameters: Vec<f64>,
    par_errors: Vec<f64>,
    par_names: Vec<String>,
    domain: (f64, f64),
    chisquare: f64,
    ndf: i32,
    origin: Box<DynObject>,
}

impl Function1 {
    /// The name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The formula, as ROOT stores it (`[p0]*x+[p1]`); empty for
    /// functions defined in C++ code.
    pub fn formula(&self) -> &str {
        &self.formula
    }

    /// The parameter values.
    pub fn parameters(&self) -> &[f64] {
        &self.parameters
    }

    /// The parameter errors (0 where not set).
    pub fn par_errors(&self) -> &[f64] {
        &self.par_errors
    }

    /// The parameter names.
    pub fn par_names(&self) -> &[String] {
        &self.par_names
    }

    /// The domain `[xmin, xmax]`.
    pub fn domain(&self) -> (f64, f64) {
        self.domain
    }

    /// The χ² of the fit that set the parameters (0 if none).
    pub fn chisquare(&self) -> f64 {
        self.chisquare
    }

    /// The degrees of freedom of that fit.
    pub fn ndf(&self) -> i32 {
        self.ndf
    }
}

impl RootClass for Function1 {
    const CLASS_NAME: &'static str = "TF1";
}

impl FromDyn for Function1 {
    fn from_dyn(obj: &DynObject) -> Result<Self> {
        super::expect_class(obj, Self::CLASS_NAME)?;
        let floats = |o: &DynObject, m: &str| -> Result<Vec<f64>> {
            match o.get(m) {
                None | Some(Value::Null) => Ok(Vec::new()),
                Some(v) => Vec::<f64>::try_from(v),
            }
        };
        let (mut formula, mut parameters, mut par_names) = (String::new(), Vec::new(), Vec::new());
        if let Some(Value::Object(f)) = obj.get("fFormula") {
            // A formula: its expression, values, and names by index.
            f.str("fFormula")
                .unwrap_or_default()
                .clone_into(&mut formula);
            parameters = floats(f, "fClingParameters")?;
            let mut names: Vec<(i64, String)> = f
                .get("fParams")
                .and_then(Value::as_list)
                .unwrap_or_default()
                .iter()
                .filter_map(|pair| match pair.as_list()? {
                    [k, v] => Some((v.as_i64()?, k.as_str()?.to_owned())),
                    _ => None,
                })
                .collect();
            names.sort_by_key(|(i, _)| *i);
            par_names = names.into_iter().map(|(_, n)| n).collect();
        } else if let Some(Value::Object(p)) = obj.get("fParams") {
            // A function in C++ code: its parameters only.
            parameters = floats(p, "fParameters")?;
            par_names = p
                .get("fParNames")
                .and_then(Value::as_list)
                .unwrap_or_default()
                .iter()
                .filter_map(|n| n.as_str().map(str::to_owned))
                .collect();
        }
        Ok(Self {
            name: obj.str("fName")?.to_owned(),
            title: obj.str("fTitle")?.to_owned(),
            formula,
            parameters,
            par_errors: floats(obj, "fParErrors")?,
            par_names,
            domain: (obj.f64("fXmin")?, obj.f64("fXmax")?),
            chisquare: obj.f64("fChisquare")?,
            ndf: i32::try_from(obj.member("fNDF")?)?,
            origin: Box::new(obj.clone()),
        })
    }
}

impl ToDyn for Function1 {
    /// The function as read.
    fn to_dyn(&self) -> DynObject {
        (*self.origin).clone()
    }
}
