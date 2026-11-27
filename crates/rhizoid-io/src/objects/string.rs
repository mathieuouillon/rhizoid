//! [`StringObject`]: ROOT's `TObjString`.

use core::fmt;

use crate::core::error::Result;
use crate::streamer::{DynObject, FromDyn, RootClass, ToDyn};

/// A string stored as an object (`TObjString`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct StringObject(String);

impl StringObject {
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

impl RootClass for StringObject {
    const CLASS_NAME: &'static str = "TObjString";
}

impl FromDyn for StringObject {
    fn from_dyn(obj: &DynObject) -> Result<Self> {
        super::expect_class(obj, Self::CLASS_NAME)?;
        Ok(Self(obj.str("fString")?.to_owned()))
    }
}

impl ToDyn for StringObject {
    fn to_dyn(&self) -> DynObject {
        DynObject::new(Self::CLASS_NAME).with("fString", self.0.as_str())
    }
}

impl fmt::Display for StringObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for StringObject {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for StringObject {
    fn from(s: String) -> Self {
        Self(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_convert_and_round_trip() {
        let s = StringObject::from("written");
        assert_eq!(s.as_str(), "written");
        assert_eq!(s.to_string(), "written");
        assert_eq!(StringObject::from(String::from("x")).into_string(), "x");
        let back = StringObject::from_dyn(&s.to_dyn()).expect("model");
        assert_eq!(back.as_str(), "written");
        assert_eq!(StringObject::CLASS_NAME, "TObjString");
        let named = DynObject::new("TNamed");
        assert!(StringObject::from_dyn(&named).is_err(), "another class");
    }
}
