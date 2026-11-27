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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_convert_and_round_trip() {
        let s = TObjString::from("written");
        assert_eq!(s.as_str(), "written");
        assert_eq!(s.to_string(), "written");
        assert_eq!(TObjString::from(String::from("x")).into_string(), "x");
        let back = TObjString::from_dyn(&s.to_dyn()).expect("model");
        assert_eq!(back.as_str(), "written");
        assert_eq!(TObjString::CLASS_NAME, "TObjString");
        let named = DynObject::new("TNamed");
        assert!(TObjString::from_dyn(&named).is_err(), "another class");
    }
}
