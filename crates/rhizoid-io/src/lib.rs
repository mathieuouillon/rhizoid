//! Read and write CERN ROOT files in pure Rust: the I/O member of the
//! **rhizoid** ecosystem.
//!
//! # Overview
//!
//! `rhizoid-io` opens ROOT files (local or from any [`ReadAt`] source), lists
//! their directories and keys, reads objects, checks a file's integrity, and
//! writes new files.
//!
//! - **Any object** reads into a [`DynObject`] through the class description
//!   stored in the file ([`File::get_dyn`]).
//! - **Data models** of common classes live in [`objects`] (histograms of
//!   every dimension and cell type, profiles, graphs, functions, axes,
//!   strings), read with [`File::get`] and written with
//!   [`FileScope::put`]. Like uproot's, they hold what the file stores and
//!   do not fill or compute.
//! - **User classes**: `#[derive(RootObject)]` gives a struct its own class
//!   description (checksum included, as ROOT computes it); it is written
//!   with it and read back from any version of the class, by member name
//!   ([`RootObject`](trait@RootObject)).
//! - **Typed rows**: `#[derive(Row)]`, or a tuple, reads a tree or an
//!   `RNTuple` entry by entry (`tree.rows::<Event>()`), and writes a flat
//!   `TTree` ([`FileScope::tree`], [`tree::TreeWriter`]) or an `RNTuple`
//!   ([`FileScope::rntuple`], [`ntuple::RNTupleWriter`]): numbers, `bool`,
//!   fixed arrays, variable-size arrays, strings, laid out as ROOT lays
//!   them out. In trees, a field of a `#[derive(RootObject)]` class, or a
//!   `Vec` of one, is a split branch (`TBranchElement`), written as ROOT
//!   splits it and read back whole.
//!
//! - **Trees**: [`File::tree`] opens a `TTree`; [`tree`] reads its branches
//!   (flat or split objects) into typed vectors or untyped
//!   [`columnar::Column`]s, over any [`tree::EntrySelection`], several at a
//!   time with [`Tree::reader`].
//! - **`RNTuple`s**: [`File::rntuple`] opens one; [`ntuple`] reads its
//!   fields the same way, several at a time with [`RNTuple::reader`].
//! - **Async** (feature `async`): `File::open_async` gives an `AsyncFile`
//!   whose trees and `RNTuple`s read with `.await`, and streams of batches;
//!   it builds for WASM, where `File::open_local_async` reads through the
//!   browser's `fetch`.
//!
//! - **Directories and updates**: [`FileScope::mkdir`] writes into
//!   subdirectories; [`update`] adds objects, directories, trees (extending
//!   an existing tree) and `RNTuple`s to an existing file, all or nothing:
//!   the file reads as it was until the update is complete, even after a
//!   crash, and other writers are locked out meanwhile.
//!
//! # Usage
//!
//! ```toml
//! [dependencies]
//! rhizoid-io = "1.0"
//! ```
//!
//! # Examples
//!
//! Write a histogram, then read it back:
//!
//! ```
//! use rhizoid_io::objects::{TAxis, TH1D};
//! # let dir = std::env::temp_dir().join(format!("rhizoid-doc-{}", std::process::id()));
//! # std::fs::create_dir_all(&dir)?;
//! # let path = dir.join("h.root");
//! // Under- and overflow, then 10 bins.
//! let mut cells = vec![0.0; 12];
//! cells[3] = 1.0;
//! let mut h = TH1D::new("h", "a histogram", TAxis::uniform(10, 0.0, 1.0)?)
//!     .with_values_flow(cells)?;
//! h.stats_mut().entries = 1.0;
//! rhizoid_io::create(&path, |f| f.put("h", &h))?;
//!
//! let file = rhizoid_io::File::open(&path)?;
//! let back: TH1D = file.get("h")?;
//! assert_eq!(back.values()[2], 1.0);
//! assert_eq!(file.get_dyn("h")?.f64("fEntries")?, 1.0);
//! assert!(file.check()?.is_ok());
//! # std::fs::remove_dir_all(&dir).ok();
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Untrusted input
//!
//! Decoding never panics and never indexes past a buffer; lengths read from
//! the file are checked before allocating (see [`ReadLimits`]).
//!
//! # Crate features
//!
//! - `derive` (default): `#[derive(RootObject)]` and `#[derive(Row)]`
//!   (crate `rhizoid-derive`).
//! - `rayon`: `tree::TreeReaderBuilder::par_batches`, batches read in
//!   parallel on the caller's rayon pool; `RNTuple` pages compressed in
//!   parallel when written.
//! - `async`: async reading — `File::open_async`, `AsyncFile`,
//!   `AsyncTree`, `AsyncRNTuple` and streams of batches, over any
//!   `AsyncReadAt` (or `LocalAsyncReadAt`, for the browser).
//! - `http`: files over HTTP(S) — `File::open_url`, `io::HttpSource`,
//!   `io::HttpPool` (reqwest with rustls).
//!
//! [`io`] has the source adapters for any byte source: bounded
//! [`io::Coalesce`], [`io::ReadCache`], [`io::Counted`]. Object stores
//! (S3, GCS, Azure) are in the `rhizoid-object-store` crate.
//!
//! Every codec used for reading (zlib, LZ4, Zstandard, LZMA) is pure Rust
//! and always enabled.
//!
//! # Minimum supported Rust version
//!
//! 1.89.

#[cfg(feature = "async")]
pub mod asynchronous;
pub mod columnar;
mod compress;
mod core;
mod file;
pub mod io;
pub mod ntuple;
pub mod objects;
pub mod streamer;
pub mod tree;
mod user;
mod write;

#[cfg(feature = "_fuzz")]
#[doc(hidden)]
#[path = "fuzz.rs"]
pub mod __fuzz;

/// Whether checksums are verified: always, except in fuzz builds, where a
/// checksum would stop the fuzzer before the decoder it protects.
pub(crate) const CHECKSUMS: bool = !cfg!(fuzzing);

pub use rhizoid_source::{FileOffset, ReadAt, WriteAt};

#[cfg(feature = "async")]
pub use crate::asynchronous::{AsyncFile, AsyncRNTuple, AsyncTree, LocalAsyncFile};
pub use crate::compress::{Algorithm, Compression};
pub use crate::core::error::{Context, Diagnostic, DiagnosticKind, Error, ErrorKind, Result};
pub use crate::core::header::FileHeader;
pub use crate::core::time::{Datime, Uuid};
pub use crate::file::{CheckReport, Directory, File, KeyInfo, ReadLimits, ReadOptions, Strictness};
pub use crate::ntuple::RNTuple;
pub use crate::streamer::{Array, DynObject, FromDyn, RootClass, ToDyn, Value};
pub use crate::tree::Tree;
pub use crate::user::{
    __private, CountType, RootObject, RootScalar, RootType, Row, RowField, VecItem,
};
pub use crate::write::{
    DirId, DirWriter, FileScope, UpdateOptions, WritableFile, WriteOptions, create, create_with,
    update, update_with,
};
/// `#[derive(RootObject)]`: a user class, read and written with its own
/// description; see [`RootObject`](trait@RootObject).
#[cfg(feature = "derive")]
pub use rhizoid_derive::RootObject;
/// `#[derive(Row)]`: a typed row of a tree or an `RNTuple`; see
/// [`Row`](trait@Row).
#[cfg(feature = "derive")]
pub use rhizoid_derive::Row;
