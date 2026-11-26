//! The sans-IO wire format: buffers, header, keys, directories, free list.
//!
//! Nothing in this module performs I/O except the record walker, which takes
//! a source as an argument.

pub(crate) mod buffer;
pub(crate) mod directory;
pub(crate) mod error;
pub(crate) mod header;
pub(crate) mod key;
pub(crate) mod time;
pub(crate) mod walk;
