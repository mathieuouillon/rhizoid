//! `TNamed`.

use crate::core::error::Result;
use crate::streamer::{DynObject, FromDyn, RootClass, ToDyn};

/// A name and a title (`TNamed`), stored on their own.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct TNamed {
    name: String,
    title: String,
}

impl TNamed {
    /// A name and a title.
    pub fn new(name: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            title: title.into(),
        }
    }

    /// The name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The title.
    pub fn title(&self) -> &str {
        &self.title
    }
}

impl RootClass for TNamed {
    const CLASS_NAME: &'static str = "TNamed";
}

impl FromDyn for TNamed {
    fn from_dyn(obj: &DynObject) -> Result<Self> {
        super::expect_class(obj, Self::CLASS_NAME)?;
        Ok(Self::new(obj.str("fName")?, obj.str("fTitle")?))
    }
}

impl ToDyn for TNamed {
    fn to_dyn(&self) -> DynObject {
        DynObject::new(Self::CLASS_NAME)
            .with("fName", self.name.as_str())
            .with("fTitle", self.title.as_str())
    }
}
