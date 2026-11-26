//! `TObject`.

use crate::core::error::Result;
use crate::streamer::{DynObject, FromDyn, RootClass, ToDyn};

/// `TObject`'s own members: a unique ID and status bits. As a model, the
/// base of user classes that derive from `TObject`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct TObject {
    /// `fUniqueID`.
    pub unique_id: u32,
    /// `fBits`, as stored.
    pub bits: u32,
}

impl Default for TObject {
    /// As ROOT constructs one: ID 0, `kNotDeleted` and `kIsOnHeap` set.
    fn default() -> Self {
        Self {
            unique_id: 0,
            bits: 0x0300_0000,
        }
    }
}

impl RootClass for TObject {
    const CLASS_NAME: &'static str = "TObject";
}

impl FromDyn for TObject {
    fn from_dyn(obj: &DynObject) -> Result<Self> {
        super::expect_class(obj, Self::CLASS_NAME)?;
        Ok(Self {
            unique_id: u32::try_from(obj.member("fUniqueID")?)?,
            bits: u32::try_from(obj.member("fBits")?)?,
        })
    }
}

impl ToDyn for TObject {
    fn to_dyn(&self) -> DynObject {
        DynObject::new(Self::CLASS_NAME)
            .with("fUniqueID", self.unique_id)
            .with("fBits", self.bits)
    }
}
