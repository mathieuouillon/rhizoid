//! The scoped writing API: [`create`], [`update`], [`FileScope`],
//! [`DirWriter`].

use core::fmt;
use core::marker::PhantomData;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use super::{UpdateOptions, WritableFile, WriteOptions, row_names};
use crate::core::error::{Error, Result};
use crate::ntuple::{RNTupleOptions, RNTupleWriter};
use crate::streamer::ToDyn;
use crate::tree::write::Target;
use crate::tree::{TreeOptions, TreeWriter};
use crate::user::Row;

/// A file being written inside [`create`] or [`update`]. Writers borrow it,
/// so none can outlive the file; trees and `RNTuple`s not finished are
/// finished when it closes.
pub struct FileScope<'s> {
    pub(super) file: Mutex<Option<WritableFile>>,
    _scope: PhantomData<&'s ()>,
}

impl fmt::Debug for FileScope<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileScope").finish_non_exhaustive()
    }
}

/// A directory of a file being written: from [`FileScope::mkdir`] or
/// [`DirWriter::mkdir`]. What it writes goes into that directory.
#[derive(Clone, Copy)]
pub struct DirWriter<'s> {
    file: &'s Mutex<Option<WritableFile>>,
    dir: usize,
}

impl fmt::Debug for DirWriter<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirWriter")
            .field("path", &self.path())
            .finish_non_exhaustive()
    }
}

fn with<T>(
    file: &Mutex<Option<WritableFile>>,
    f: impl FnOnce(&mut WritableFile) -> Result<T>,
) -> Result<T> {
    f(file
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
        .ok_or_else(|| Error::malformed("file already closed"))?)
}

impl<'s> DirWriter<'s> {
    /// The directory's path from the root (`""` for the root).
    pub fn path(&self) -> String {
        self.file
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|f| f.dir_path(self.dir))
            .unwrap_or_default()
    }

    /// Writes an object into this directory; see [`WritableFile::put`].
    ///
    /// # Errors
    ///
    /// As [`WritableFile::put`].
    pub fn put<T: ToDyn + ?Sized>(&self, name: &str, object: &T) -> Result<()> {
        let object = object.to_dyn();
        with(self.file, |f| f.put_dyn(self.dir, name, &object))
    }

    /// Creates a subdirectory and the missing ones above it (`"b/c"`), or
    /// finds it; see [`WritableFile::mkdir`].
    ///
    /// # Errors
    ///
    /// As [`WritableFile::mkdir`].
    pub fn mkdir(&self, path: &str) -> Result<DirWriter<'s>> {
        let dir = with(self.file, |f| f.mkdir_in(self.dir, path))?;
        Ok(DirWriter {
            file: self.file,
            dir,
        })
    }

    /// Starts a tree of `R` rows in this directory; see [`TreeWriter`]. In
    /// [`update`] mode, a tree of that name with the same branches is
    /// extended: its new entries follow the old ones.
    ///
    /// # Errors
    ///
    /// As [`WritableFile::tree`]; in update mode, `InvalidInput` if the
    /// existing tree has other branches.
    pub fn tree<R: Row>(&self, name: &str) -> Result<TreeWriter<'s, R>> {
        self.tree_with(name, TreeOptions::default())
    }

    /// [`tree`](Self::tree) with options.
    ///
    /// # Errors
    ///
    /// As [`tree`](Self::tree).
    pub fn tree_with<R: Row>(&self, name: &str, options: TreeOptions) -> Result<TreeWriter<'s, R>> {
        let names = row_names::<R>()?;
        TreeWriter::new(Target::scope(self.file, self.dir), name, &names, options)
    }

    /// Starts a tree of tuple rows, naming the branches; see
    /// [`WritableFile::tree_of`].
    ///
    /// # Errors
    ///
    /// As [`tree`](Self::tree).
    pub fn tree_of<R: Row, const N: usize>(
        &self,
        name: &str,
        names: [&str; N],
    ) -> Result<TreeWriter<'s, R>> {
        let names = names.map(str::to_owned);
        TreeWriter::new(
            Target::scope(self.file, self.dir),
            name,
            &names,
            TreeOptions::default(),
        )
    }

    /// Starts an `RNTuple` of `R` entries in this directory; see
    /// [`RNTupleWriter`].
    ///
    /// # Errors
    ///
    /// As [`WritableFile::rntuple`].
    pub fn rntuple<R: Row>(&self, name: &str) -> Result<RNTupleWriter<'s, R>> {
        self.rntuple_with(name, RNTupleOptions::default())
    }

    /// [`rntuple`](Self::rntuple) with options.
    ///
    /// # Errors
    ///
    /// As [`WritableFile::rntuple`].
    pub fn rntuple_with<R: Row>(
        &self,
        name: &str,
        options: RNTupleOptions,
    ) -> Result<RNTupleWriter<'s, R>> {
        let names = row_names::<R>()?;
        RNTupleWriter::new(Target::scope(self.file, self.dir), name, &names, options)
    }

    /// Starts an `RNTuple` of tuple entries, naming the fields; see
    /// [`WritableFile::rntuple_of`].
    ///
    /// # Errors
    ///
    /// As [`WritableFile::rntuple`].
    pub fn rntuple_of<R: Row, const N: usize>(
        &self,
        name: &str,
        names: [&str; N],
    ) -> Result<RNTupleWriter<'s, R>> {
        let names = names.map(str::to_owned);
        RNTupleWriter::new(
            Target::scope(self.file, self.dir),
            name,
            &names,
            RNTupleOptions::default(),
        )
    }
}

impl<'s> FileScope<'s> {
    /// The root directory.
    fn root(&'s self) -> DirWriter<'s> {
        DirWriter {
            file: &self.file,
            dir: 0,
        }
    }

    /// Writes an object into the root directory; see [`WritableFile::put`].
    ///
    /// # Errors
    ///
    /// As [`WritableFile::put`].
    pub fn put<T: ToDyn + ?Sized>(&'s self, name: &str, object: &T) -> Result<()> {
        self.root().put(name, object)
    }

    /// Creates a directory and the missing ones above it (`"a/b"`), as
    /// `std::fs::create_dir_all` does, or finds it if it exists.
    ///
    /// # Errors
    ///
    /// As [`WritableFile::mkdir`].
    pub fn mkdir(&'s self, path: &str) -> Result<DirWriter<'s>> {
        self.root().mkdir(path)
    }

    /// Starts a tree of `R` rows, a `#[derive(Row)]` type; see
    /// [`DirWriter::tree`].
    ///
    /// # Errors
    ///
    /// As [`DirWriter::tree`].
    pub fn tree<R: Row>(&'s self, name: &str) -> Result<TreeWriter<'s, R>> {
        self.root().tree(name)
    }

    /// [`tree`](Self::tree) with options.
    ///
    /// # Errors
    ///
    /// As [`DirWriter::tree`].
    pub fn tree_with<R: Row>(
        &'s self,
        name: &str,
        options: TreeOptions,
    ) -> Result<TreeWriter<'s, R>> {
        self.root().tree_with(name, options)
    }

    /// Starts a tree of tuple rows, naming the branches; see
    /// [`WritableFile::tree_of`].
    ///
    /// # Errors
    ///
    /// As [`DirWriter::tree`].
    pub fn tree_of<R: Row, const N: usize>(
        &'s self,
        name: &str,
        names: [&str; N],
    ) -> Result<TreeWriter<'s, R>> {
        self.root().tree_of(name, names)
    }

    /// Starts an `RNTuple` of `R` entries, a `#[derive(Row)]` type; see
    /// [`RNTupleWriter`].
    ///
    /// # Errors
    ///
    /// As [`WritableFile::rntuple`].
    pub fn rntuple<R: Row>(&'s self, name: &str) -> Result<RNTupleWriter<'s, R>> {
        self.root().rntuple(name)
    }

    /// [`rntuple`](Self::rntuple) with options.
    ///
    /// # Errors
    ///
    /// As [`WritableFile::rntuple`].
    pub fn rntuple_with<R: Row>(
        &'s self,
        name: &str,
        options: RNTupleOptions,
    ) -> Result<RNTupleWriter<'s, R>> {
        self.root().rntuple_with(name, options)
    }

    /// Starts an `RNTuple` of tuple entries, naming the fields; see
    /// [`WritableFile::rntuple_of`].
    ///
    /// # Errors
    ///
    /// As [`WritableFile::rntuple`].
    pub fn rntuple_of<R: Row, const N: usize>(
        &'s self,
        name: &str,
        names: [&str; N],
    ) -> Result<RNTupleWriter<'s, R>> {
        self.root().rntuple_of(name, names)
    }

    /// The file's name.
    pub fn name(&self) -> String {
        self.file
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|f| f.name.clone())
            .unwrap_or_default()
    }
}

/// Creates a file, runs `body`, then closes it, returning `body`'s result.
///
/// If `body` returns an error, the file is still closed so that what was
/// written is readable, and `body`'s error is returned (decision D7).
///
/// # Errors
///
/// `body`'s own error, or an error creating or closing the file.
pub fn create<T, E>(
    path: impl AsRef<Path>,
    body: impl for<'s> FnOnce(&'s FileScope<'s>) -> core::result::Result<T, E>,
) -> core::result::Result<T, E>
where
    E: From<Error>,
{
    create_with(path, WriteOptions::default(), body)
}

/// [`create`] with options.
///
/// # Errors
///
/// As [`create`].
pub fn create_with<T, E>(
    path: impl AsRef<Path>,
    options: WriteOptions,
    body: impl for<'s> FnOnce(&'s FileScope<'s>) -> core::result::Result<T, E>,
) -> core::result::Result<T, E>
where
    E: From<Error>,
{
    let file = WritableFile::create_with(path, options)?;
    let (result, file) = run(file, body);
    let closed = file.and_then(WritableFile::close);
    match (result, closed) {
        (Ok(v), Ok(())) => Ok(v),
        (Ok(_), Err(e)) => Err(e.into()),
        (Err(e), _) => Err(e),
    }
}

/// Opens an existing file, runs `body`, then commits what it added,
/// returning `body`'s result: objects, directories, trees (an existing tree
/// is extended), `RNTuple`s.
///
/// **All or nothing** (decisions D11, D13): the file reads as it was until
/// the update is complete. If `body` returns an error or panics, or the
/// commit fails, nothing is committed; only unreferenced bytes past the old
/// end of the file remain. The file is locked against other writers while
/// it is updated. See [`UpdateOptions`] for files on storage without
/// atomic sector writes.
///
/// # Errors
///
/// `body`'s own error; an error opening the file (`Unsupported` for a file
/// that was not closed, or whose layout the update cannot commit
/// atomically without [`UpdateOptions::copy_on_write`]; `Io` if another
/// writer holds it); or an error committing.
pub fn update<T, E>(
    path: impl AsRef<Path>,
    body: impl for<'s> FnOnce(&'s FileScope<'s>) -> core::result::Result<T, E>,
) -> core::result::Result<T, E>
where
    E: From<Error>,
{
    update_with(path, UpdateOptions::default(), body)
}

/// [`update`] with options.
///
/// # Errors
///
/// As [`update`].
pub fn update_with<T, E>(
    path: impl AsRef<Path>,
    options: UpdateOptions,
    body: impl for<'s> FnOnce(&'s FileScope<'s>) -> core::result::Result<T, E>,
) -> core::result::Result<T, E>
where
    E: From<Error>,
{
    let file = WritableFile::update_with(path, options)?;
    let (result, file) = run(file, body);
    match result {
        // Nothing is committed: the file is dropped (no I/O).
        Err(e) => Err(e),
        Ok(v) => {
            file.and_then(WritableFile::close)?;
            Ok(v)
        }
    }
}

/// Runs `body` on a scope over `file`, and gives the file back.
pub(crate) fn run<T, E>(
    file: WritableFile,
    body: impl for<'s> FnOnce(&'s FileScope<'s>) -> core::result::Result<T, E>,
) -> (core::result::Result<T, E>, Result<WritableFile>) {
    let scope = FileScope {
        file: Mutex::new(Some(file)),
        _scope: PhantomData,
    };
    let result = body(&scope);
    let file = scope
        .file
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
        .ok_or_else(|| Error::malformed("file already closed"));
    (result, file)
}
