//! `TObjString`.

use core::fmt;

use crate::core::error::Result;
use crate::streamer::{DynObject, FromDyn, RootClass, ToDyn};

/// A string stored as an object (`TObjString`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct TObjString(String);

impl TObjString {
    /// Wraps a string.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// The string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The string, by value.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl RootClass for TObjString {
    const CLASS_NAME: &'static str = "TObjString";
}

impl FromDyn for TObjString {
    fn from_dyn(obj: &DynObject) -> Result<Self> {
        super::expect_class(obj, Self::CLASS_NAME)?;
        Ok(Self(obj.str("fString")?.to_owned()))
    }
}

impl ToDyn for TObjString {
    fn to_dyn(&self) -> DynObject {
        DynObject::new(Self::CLASS_NAME).with("fString", self.0.as_str())
    }
}

impl fmt::Display for TObjString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for TObjString {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for TObjString {
    fn from(s: String) -> Self {
        Self(s)
    }
}
