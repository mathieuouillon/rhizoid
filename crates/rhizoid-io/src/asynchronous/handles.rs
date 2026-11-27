//! [`AsyncTree`] and [`AsyncRNTuple`]: the sync handles' metadata, with
//! async reads.

use core::fmt;
use core::marker::PhantomData;
use core::ops::Range;
use std::sync::Arc;

use rhizoid_source::AsyncReadAt;

use super::{AsyncSource, detached, drive};
use crate::columnar::{Column, ColumnType};
use crate::core::error::Result;
use crate::ntuple::{Field, RNTuple};
use crate::tree::{
    Branch, EntrySelection, ReadStats, Readable, ReaderBuilder, SyncReadable, Tree, sealed,
};

/// Reads one column through `handle`: the bytes the plan names first, in
/// one request, then whatever the decoding still misses.
async fn read_column<H: SyncReadable, S: AsyncSource>(
    handle: &H,
    source: &S,
    name: &str,
    selection: EntrySelection,
    read: impl Fn(&H, &str, EntrySelection) -> Result<Column>,
) -> Result<Column> {
    let runs = selection.resolve(handle.num_entries_()).unwrap_or_default();
    let first = handle.plan(name, &runs);
    drive(source, handle.file_len(), first, |s| {
        read(&handle.with_source(s), name, selection.clone())
    })
    .await
}

/// A `TTree` of an [`AsyncFile`](super::AsyncFile): metadata as in
/// [`Tree`], reads `async`. Cheap to clone.
#[derive(Clone)]
pub struct AsyncTree<S = Arc<dyn AsyncReadAt>> {
    tree: Tree,
    source: S,
}

impl<S> fmt::Debug for AsyncTree<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AsyncTree").field(&self.tree).finish()
    }
}

impl<S: AsyncSource> AsyncTree<S> {
    pub(crate) fn new(tree: &Tree, source: S) -> Self {
        Self {
            tree: tree.with_source(detached()),
            source,
        }
    }

    pub(crate) fn sync(&self) -> &Tree {
        &self.tree
    }

    pub(crate) fn source(&self) -> &S {
        &self.source
    }

    /// The tree's name.
    pub fn name(&self) -> &str {
        self.tree.name()
    }

    /// The tree's title.
    pub fn title(&self) -> &str {
        self.tree.title()
    }

    /// The number of entries.
    pub fn num_entries(&self) -> u64 {
        self.tree.num_entries()
    }

    /// The top-level branches.
    pub fn branches(&self) -> &[Branch] {
        self.tree.branches()
    }

    /// Every branch that holds values (not groups), depth first.
    pub fn readable_branches(&self) -> Vec<&Branch> {
        self.tree.readable_branches()
    }

    /// A branch by name, at any depth.
    pub fn get_branch(&self, name: &str) -> Option<&Branch> {
        self.tree.get_branch(name)
    }

    /// The entry ranges of the clusters.
    pub fn clusters(&self) -> &[Range<u64>] {
        self.tree.clusters()
    }

    /// What reading through this handle has cost so far.
    pub fn read_stats(&self) -> ReadStats {
        self.tree.read_stats()
    }

    /// Opens a branch for typed reading; the type is checked here, once.
    ///
    /// # Errors
    ///
    /// As [`Tree::branch`].
    pub fn branch<T: ColumnType>(&self, name: &str) -> Result<AsyncBranchReader<'_, T, S>> {
        self.tree.branch::<T>(name)?;
        Ok(AsyncBranchReader {
            tree: self,
            name: name.to_owned(),
            _type: PhantomData,
        })
    }

    /// Reads a branch over a selection of entries, untyped; see
    /// [`Tree::read`]. The baskets are fetched in one request.
    ///
    /// # Errors
    ///
    /// As [`Tree::read`], plus the source's errors.
    pub async fn read(&self, name: &str, selection: impl Into<EntrySelection>) -> Result<Column> {
        read_column(
            &self.tree,
            &self.source,
            name,
            selection.into(),
            |t: &Tree, n: &str, s: EntrySelection| t.read(n, s),
        )
        .await
    }

    /// A reader of several branches: the same builder as [`Tree::reader`],
    /// whose `build` gives a [`TreeStream`](super::TreeStream).
    pub fn reader(&self) -> ReaderBuilder<Self> {
        ReaderBuilder::new(self.clone())
    }
}

/// Reads one branch as `T`; made by [`AsyncTree::branch`].
pub struct AsyncBranchReader<'t, T, S = Arc<dyn AsyncReadAt>> {
    tree: &'t AsyncTree<S>,
    name: String,
    _type: PhantomData<fn() -> T>,
}

impl<T, S> fmt::Debug for AsyncBranchReader<'_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AsyncBranchReader")
            .field("branch", &self.name)
            .finish_non_exhaustive()
    }
}

impl<T: ColumnType, S: AsyncSource> AsyncBranchReader<'_, T, S> {
    /// Reads a selection of entries.
    ///
    /// # Errors
    ///
    /// As [`BranchReader::read`](crate::tree::BranchReader::read), plus
    /// the source's errors.
    pub async fn read(&self, selection: impl Into<EntrySelection>) -> Result<T::Output> {
        T::from_column(self.tree.read(&self.name, selection).await?)
    }
}

impl<S: AsyncSource> sealed::Sealed for AsyncTree<S> {}

impl<S: AsyncSource> Readable for AsyncTree<S> {
    fn num_entries_(&self) -> u64 {
        self.tree.num_entries_()
    }
    fn clusters_(&self) -> &[Range<u64>] {
        self.tree.clusters_()
    }
    fn every_column(&self) -> Vec<String> {
        self.tree.every_column()
    }
    fn expand(&self, name: &str) -> Result<Vec<String>> {
        self.tree.expand(name)
    }
    fn interpretation_of(&self, name: &str) -> Result<crate::columnar::Interpretation> {
        self.tree.interpretation_of(name)
    }
}

/// An `RNTuple` of an [`AsyncFile`](super::AsyncFile): metadata as in
/// [`RNTuple`], reads `async`. Cheap to clone.
#[derive(Clone)]
pub struct AsyncRNTuple<S = Arc<dyn AsyncReadAt>> {
    ntuple: RNTuple,
    source: S,
}

impl<S> fmt::Debug for AsyncRNTuple<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AsyncRNTuple").field(&self.ntuple).finish()
    }
}

impl<S: AsyncSource> AsyncRNTuple<S> {
    pub(crate) fn new(ntuple: &RNTuple, source: S) -> Self {
        Self {
            ntuple: ntuple.with_source(detached()),
            source,
        }
    }

    pub(crate) fn sync(&self) -> &RNTuple {
        &self.ntuple
    }

    pub(crate) fn source(&self) -> &S {
        &self.source
    }

    /// The `RNTuple`'s name.
    pub fn name(&self) -> &str {
        self.ntuple.name()
    }

    /// Its description.
    pub fn description(&self) -> &str {
        self.ntuple.description()
    }

    /// What wrote it.
    pub fn written_by(&self) -> &str {
        self.ntuple.written_by()
    }

    /// The format version: epoch, major, minor, patch.
    pub fn format_version(&self) -> [u16; 4] {
        self.ntuple.format_version()
    }

    /// The number of entries.
    pub fn num_entries(&self) -> u64 {
        self.ntuple.num_entries()
    }

    /// The entry ranges of the clusters.
    pub fn clusters(&self) -> &[Range<u64>] {
        self.ntuple.clusters()
    }

    /// The top-level fields.
    pub fn fields(&self) -> Vec<Field<'_>> {
        self.ntuple.fields()
    }

    /// A field by name; sub-fields of records by dotted path.
    pub fn get_field(&self, path: &str) -> Option<Field<'_>> {
        self.ntuple.get_field(path)
    }

    /// Opens a field for typed reading; the type is checked here, once.
    ///
    /// # Errors
    ///
    /// As [`RNTuple::field`].
    pub fn field<T: ColumnType>(&self, path: &str) -> Result<AsyncFieldReader<'_, T, S>> {
        self.ntuple.field::<T>(path)?;
        Ok(AsyncFieldReader {
            ntuple: self,
            path: path.to_owned(),
            _type: PhantomData,
        })
    }

    /// Reads a field over a selection of entries, untyped; see
    /// [`RNTuple::read`]. The pages are fetched in one request.
    ///
    /// # Errors
    ///
    /// As [`RNTuple::read`], plus the source's errors.
    pub async fn read(&self, path: &str, selection: impl Into<EntrySelection>) -> Result<Column> {
        read_column(
            &self.ntuple,
            &self.source,
            path,
            selection.into(),
            |n: &RNTuple, p: &str, s: EntrySelection| n.read(p, s),
        )
        .await
    }

    /// A reader of several fields: the same builder as
    /// [`RNTuple::reader`], whose `build` gives an
    /// [`RNTupleStream`](super::RNTupleStream).
    pub fn reader(&self) -> ReaderBuilder<Self> {
        ReaderBuilder::new(self.clone())
    }
}

/// Reads one field as `T`; made by [`AsyncRNTuple::field`].
pub struct AsyncFieldReader<'a, T, S = Arc<dyn AsyncReadAt>> {
    ntuple: &'a AsyncRNTuple<S>,
    path: String,
    _type: PhantomData<fn() -> T>,
}

impl<T, S> fmt::Debug for AsyncFieldReader<'_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AsyncFieldReader")
            .field("field", &self.path)
            .finish_non_exhaustive()
    }
}

impl<T: ColumnType, S: AsyncSource> AsyncFieldReader<'_, T, S> {
    /// Reads a selection of entries.
    ///
    /// # Errors
    ///
    /// As [`RNTuple::read`], plus the source's errors.
    pub async fn read(&self, selection: impl Into<EntrySelection>) -> Result<T::Output> {
        T::from_column(self.ntuple.read(&self.path, selection).await?)
    }
}

impl<S: AsyncSource> sealed::Sealed for AsyncRNTuple<S> {}

impl<S: AsyncSource> Readable for AsyncRNTuple<S> {
    fn num_entries_(&self) -> u64 {
        self.ntuple.num_entries_()
    }
    fn clusters_(&self) -> &[Range<u64>] {
        self.ntuple.clusters_()
    }
    fn every_column(&self) -> Vec<String> {
        self.ntuple.every_column()
    }
    fn expand(&self, name: &str) -> Result<Vec<String>> {
        self.ntuple.expand(name)
    }
    fn interpretation_of(&self, name: &str) -> Result<crate::columnar::Interpretation> {
        self.ntuple.interpretation_of(name)
    }
}
