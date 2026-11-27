//! Choosing what to read: [`Projection`] (which branches or
//! fields), [`EntrySelection`] (which entries), and the reader that
//! applies them, [`ReaderBuilder`].
//!
//! ```
//! use rhizoid_io::tree::EntrySelection;
//! # let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/uproot_trees.root");
//! let file = rhizoid_io::File::open(path)?;
//! let tree = file.tree("events")?;
//!
//! // Entries 0..10 and 590..610 of two branches, one batch per cluster.
//! let selection = EntrySelection::from_ranges([0..10, 590..610]);
//! for batch in tree.reader().projection(["f32", "jet_pt"])?.selection(selection)?.build() {
//!     let batch = batch?;
//!     let f32s: Vec<f32> = batch.get::<f32>("f32")?;
//!     assert_eq!(f32s.len(), batch.num_entries());
//! }
//! # Ok::<(), rhizoid_io::Error>(())
//! ```

use core::iter::FusedIterator;
use core::num::NonZeroU64;
use core::ops::{Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive};
use std::sync::Arc;

use rhizoid_source::{FileOffset, ReadAt};

use super::Tree;
use crate::columnar::{Column, ColumnType, Interpretation};
use crate::core::error::{Error, ErrorKind, Result};
use crate::ntuple::RNTuple;
use crate::user::Row;

/// Which entries to read: sorted, disjoint runs, built from ranges or entry
/// lists and combined with [`union`](Self::union),
/// [`intersect`](Self::intersect) and [`and_then`](Self::and_then).
///
/// Every read accepts `impl Into<EntrySelection>`, so `..`, `0..1000`,
/// `5..=9` and `100..` work wherever a selection does. A selection is
/// checked against the branch or tree when it is used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntrySelection {
    /// Sorted, disjoint, non-empty, before `tail`.
    ranges: Vec<Range<u64>>,
    /// Every entry from this one on.
    tail: Option<u64>,
    /// A range given backwards, reported when the selection is used.
    invalid: Option<(u64, u64)>,
}

impl Default for EntrySelection {
    /// Every entry.
    fn default() -> Self {
        Self::all()
    }
}

/// `(start, end)` intervals; `u64::MAX` ends an open tail.
type Intervals = Vec<(u64, u64)>;

impl EntrySelection {
    /// Every entry.
    pub fn all() -> Self {
        Self {
            ranges: Vec::new(),
            tail: Some(0),
            invalid: None,
        }
    }

    /// No entry.
    pub fn none() -> Self {
        Self {
            ranges: Vec::new(),
            tail: None,
            invalid: None,
        }
    }

    /// The entries of `ranges`, in any order, overlapping or not.
    pub fn from_ranges(ranges: impl IntoIterator<Item = Range<u64>>) -> Self {
        let mut invalid = None;
        let intervals: Intervals = ranges
            .into_iter()
            .filter_map(|r| {
                if r.start > r.end {
                    invalid.get_or_insert((r.start, r.end));
                }
                (r.start < r.end).then_some((r.start, r.end))
            })
            .collect();
        Self {
            invalid,
            ..Self::from_intervals(intervals)
        }
    }

    /// The listed entries (a ROOT `TEntryList`, or the output of a cut), in
    /// any order.
    pub fn from_entry_list(entries: &[u64]) -> Self {
        Self::from_intervals(entries.iter().map(|&e| (e, e.saturating_add(1))).collect())
    }

    fn intervals(&self) -> Intervals {
        let mut v: Intervals = self.ranges.iter().map(|r| (r.start, r.end)).collect();
        if let Some(t) = self.tail {
            v.push((t, u64::MAX));
        }
        v
    }

    fn from_intervals(mut v: Intervals) -> Self {
        v.retain(|(a, b)| a < b);
        v.sort_unstable();
        let mut merged: Intervals = Vec::with_capacity(v.len());
        for (a, b) in v {
            match merged.last_mut() {
                Some(last) if a <= last.1 => last.1 = last.1.max(b),
                _ => merged.push((a, b)),
            }
        }
        let tail = match merged.last() {
            Some(&(a, u64::MAX)) => {
                merged.pop();
                Some(a)
            }
            _ => None,
        };
        Self {
            ranges: merged.into_iter().map(|(a, b)| a..b).collect(),
            tail,
            invalid: None,
        }
    }

    /// The entries in either selection.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        let mut v = self.intervals();
        v.extend(other.intervals());
        Self {
            invalid: self.invalid.or(other.invalid),
            ..Self::from_intervals(v)
        }
    }

    /// The entries in both selections.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self {
        let (a, b) = (self.intervals(), other.intervals());
        let (mut i, mut j, mut out) = (0, 0, Vec::new());
        while let (Some(&(a0, a1)), Some(&(b0, b1))) = (a.get(i), b.get(j)) {
            let (lo, hi) = (a0.max(b0), a1.min(b1));
            if lo < hi {
                out.push((lo, hi));
            }
            if a1 < b1 { i += 1 } else { j += 1 }
        }
        Self {
            invalid: self.invalid.or(other.invalid),
            ..Self::from_intervals(out)
        }
    }

    /// Selects among the entries this selection keeps: entry `k` of `other`
    /// is the `k`-th entry selected here (parquet's `RowSelection::and_then`).
    #[must_use]
    pub fn and_then(&self, other: &Self) -> Self {
        let outer = self.intervals();
        let mut out = Vec::new();
        for (r0, r1) in other.intervals() {
            // Walk the outer intervals, counting selected entries.
            let mut seen = 0_u64;
            for &(a, b) in &outer {
                let len = b - a;
                let (lo, hi) = (r0.max(seen), r1.min(seen.saturating_add(len)));
                if lo < hi {
                    let end = if b == u64::MAX && hi == r1 && r1 == u64::MAX {
                        u64::MAX
                    } else {
                        a + (hi - seen)
                    };
                    out.push((a + (lo - seen), end));
                }
                seen = seen.saturating_add(len);
                if seen >= r1 {
                    break;
                }
            }
        }
        Self {
            invalid: self.invalid.or(other.invalid),
            ..Self::from_intervals(out)
        }
    }

    /// The number of entries selected among `0..total`.
    pub fn selected(&self, total: u64) -> u64 {
        self.intervals()
            .iter()
            .map(|&(a, b)| b.min(total).saturating_sub(a))
            .sum()
    }

    /// The runs within `0..total`.
    pub(crate) fn resolve(&self, total: u64) -> Result<Vec<Range<u64>>> {
        if let Some((a, b)) = self.invalid {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("entries {a}..{b} go backwards"),
            ));
        }
        let mut out = self.ranges.clone();
        if let Some(r) = out.last().filter(|r| r.end > total) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("entries {}..{} are outside 0..{total}", r.start, r.end),
            ));
        }
        if let Some(t) = self.tail {
            if t > total {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("entry {t} is outside 0..{total}"),
                ));
            }
            if t < total {
                out.push(t..total);
            }
        }
        Ok(out)
    }
}

impl From<Range<u64>> for EntrySelection {
    fn from(r: Range<u64>) -> Self {
        Self::from_ranges(core::iter::once(r))
    }
}

impl From<RangeInclusive<u64>> for EntrySelection {
    fn from(r: RangeInclusive<u64>) -> Self {
        let (a, b) = r.into_inner();
        Self::from_ranges(core::iter::once(a..b.saturating_add(1).max(a)))
    }
}

impl From<RangeFrom<u64>> for EntrySelection {
    fn from(r: RangeFrom<u64>) -> Self {
        Self {
            ranges: Vec::new(),
            tail: Some(r.start),
            invalid: None,
        }
    }
}

impl From<RangeTo<u64>> for EntrySelection {
    fn from(r: RangeTo<u64>) -> Self {
        Self::from_ranges(core::iter::once(0..r.end))
    }
}

impl From<RangeToInclusive<u64>> for EntrySelection {
    fn from(r: RangeToInclusive<u64>) -> Self {
        Self::from_ranges(core::iter::once(0..r.end.saturating_add(1)))
    }
}

impl From<RangeFull> for EntrySelection {
    fn from(_: RangeFull) -> Self {
        Self::all()
    }
}

impl From<&EntrySelection> for EntrySelection {
    fn from(s: &EntrySelection) -> Self {
        s.clone()
    }
}

impl From<Vec<Range<u64>>> for EntrySelection {
    fn from(v: Vec<Range<u64>>) -> Self {
        Self::from_ranges(v)
    }
}

/// Which branches to read, by name. A group (a split object) stands for
/// its readable sub-branches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Projection {
    /// `None` = every readable branch.
    names: Option<Vec<String>>,
}

impl Projection {
    /// Every readable branch.
    pub fn all() -> Self {
        Self { names: None }
    }

    /// The named branches.
    pub fn names<S: AsRef<str>>(names: impl IntoIterator<Item = S>) -> Self {
        let mut out: Vec<String> = Vec::new();
        for n in names {
            if !out.iter().any(|o| o == n.as_ref()) {
                out.push(n.as_ref().to_owned());
            }
        }
        Self { names: Some(out) }
    }

    /// Adds the branches of `other`.
    pub fn union(&mut self, other: &Self) {
        match (&mut self.names, &other.names) {
            (Some(mine), Some(theirs)) => {
                for n in theirs {
                    if !mine.contains(n) {
                        mine.push(n.clone());
                    }
                }
            }
            _ => self.names = None,
        }
    }

    /// Keeps only the branches also in `other`.
    pub fn intersect(&mut self, other: &Self) {
        match (&mut self.names, &other.names) {
            (Some(mine), Some(theirs)) => mine.retain(|n| theirs.contains(n)),
            (None, Some(theirs)) => self.names = Some(theirs.clone()),
            (_, None) => {}
        }
    }
}

impl<const N: usize> From<[&str; N]> for Projection {
    fn from(names: [&str; N]) -> Self {
        Self::names(names)
    }
}

impl From<&[&str]> for Projection {
    fn from(names: &[&str]) -> Self {
        Self::names(names)
    }
}

impl<const N: usize> From<&[&str; N]> for Projection {
    fn from(names: &[&str; N]) -> Self {
        Self::names(names)
    }
}

impl From<&str> for Projection {
    fn from(name: &str) -> Self {
        Self::names([name])
    }
}

impl From<Vec<String>> for Projection {
    fn from(names: Vec<String>) -> Self {
        Self::names(names)
    }
}

/// How many entries a batch holds.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum BatchSize {
    /// One cluster per batch: every basket is read once, whole. The default.
    #[default]
    Cluster,
    /// At most this many selected entries per batch.
    Entries(NonZeroU64),
}

impl BatchSize {
    /// At most `n` entries per batch (at least 1).
    pub fn entries(n: u64) -> Self {
        Self::Entries(NonZeroU64::new(n).unwrap_or(NonZeroU64::MIN))
    }
}

/// The columns of one batch of entries.
#[derive(Clone, Debug, PartialEq)]
pub struct Batch {
    entries: Vec<Range<u64>>,
    columns: Vec<(String, Column)>,
}

impl Batch {
    /// The number of entries.
    pub fn num_entries(&self) -> usize {
        self.entries
            .iter()
            .map(|r| usize::try_from(r.end - r.start).unwrap_or(usize::MAX))
            .sum()
    }

    /// The entry numbers held, as runs.
    pub fn entry_ranges(&self) -> &[Range<u64>] {
        &self.entries
    }

    /// A column by branch name.
    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns
            .iter()
            .find_map(|(n, c)| (n == name).then_some(c))
    }

    /// The columns, in projection order.
    pub fn columns(&self) -> impl Iterator<Item = (&str, &Column)> {
        self.columns.iter().map(|(n, c)| (n.as_str(), c))
    }

    /// The columns, by value.
    pub fn into_columns(self) -> Vec<(String, Column)> {
        self.columns
    }

    /// A column as `T` (a copy).
    ///
    /// # Errors
    ///
    /// `NotFound` if the batch has no such column; `TypeMismatch` if it is
    /// not of type `T`.
    pub fn get<T: ColumnType>(&self, name: &str) -> Result<T::Output> {
        let c = self.column(name).ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                format!("the batch has no column `{name}`"),
            )
        })?;
        T::from_column(c.clone())
    }
}

/// Configures a read of several columns (branches of a [`Tree`], fields of
/// an [`RNTuple`]) over a selection of entries; made by [`Tree::reader`]
/// and [`RNTuple::reader`], and by their async twins. Projection and
/// selection are checked when they are set.
///
/// One builder serves sync and async reading (as parquet's
/// `ArrowReaderBuilder` does): the setters exist once, and `build` gives an
/// iterator of batches for a sync handle and a stream for an async one.
#[derive(Clone, Debug)]
#[must_use]
pub struct ReaderBuilder<H> {
    handle: H,
    columns: Vec<String>,
    selection: Vec<Range<u64>>,
    batch_size: BatchSize,
    offset: u64,
    limit: Option<u64>,
}

/// The reader builder of a [`Tree`] (or, with `H`, of an async tree).
pub type TreeReaderBuilder<H = Tree> = ReaderBuilder<H>;

/// The reader builder of an [`RNTuple`] (or, with `H`, of an async one).
pub type RNTupleReaderBuilder<H = RNTuple> = ReaderBuilder<H>;

pub(crate) mod sealed {
    /// Seals [`Readable`](super::Readable) and the async source traits.
    pub trait Sealed {}
}

/// What a [`ReaderBuilder`] reads: a [`Tree`] or an [`RNTuple`], or their
/// async handles. Sealed; its methods are internal.
pub trait Readable: sealed::Sealed + Clone {
    #[doc(hidden)]
    fn num_entries_(&self) -> u64;
    #[doc(hidden)]
    fn clusters_(&self) -> &[Range<u64>];
    /// Every column read by default.
    #[doc(hidden)]
    fn every_column(&self) -> Vec<String>;
    /// The columns a name stands for (a group: its sub-branches), checked
    /// to be readable.
    #[doc(hidden)]
    fn expand(&self, name: &str) -> Result<Vec<String>>;
    /// How a column reads.
    #[doc(hidden)]
    fn interpretation_of(&self, name: &str) -> Result<Interpretation>;
}

/// A [`Readable`] that reads synchronously from its source.
pub trait SyncReadable: Readable {
    /// Reads one column over sorted, disjoint runs of entries.
    #[doc(hidden)]
    fn read_column(&self, name: &str, runs: &[Range<u64>]) -> Result<Column>;
    /// The same handle, reading through `source`.
    #[doc(hidden)]
    #[must_use]
    fn with_source(&self, source: Arc<dyn ReadAt>) -> Self;
    /// The byte ranges reading `name` over `runs` fetches.
    #[doc(hidden)]
    fn plan(&self, name: &str, runs: &[Range<u64>]) -> Vec<Range<FileOffset>>;
    /// The size of the file.
    #[doc(hidden)]
    fn file_len(&self) -> u64;
}

impl sealed::Sealed for Tree {}

impl Readable for Tree {
    fn num_entries_(&self) -> u64 {
        self.num_entries()
    }
    fn clusters_(&self) -> &[Range<u64>] {
        self.clusters()
    }
    fn every_column(&self) -> Vec<String> {
        self.readable_branches()
            .iter()
            .map(|b| b.name().to_owned())
            .collect()
    }
    fn interpretation_of(&self, name: &str) -> Result<Interpretation> {
        if let Some(name) = name.strip_prefix(super::objects::OBJECTS) {
            return self.objects_interpretation(name);
        }
        self.branch_or_err(name)?.interpretation().cloned()
    }
    fn expand(&self, name: &str) -> Result<Vec<String>> {
        if name.starts_with(super::objects::OBJECTS) {
            return Ok(vec![name.to_owned()]);
        }
        let b = self.branch_or_err(name)?;
        let expanded = if b.is_group() { b.readable() } else { vec![b] };
        expanded
            .into_iter()
            .map(|b| {
                b.interpretation()?;
                Ok(b.name().to_owned())
            })
            .collect()
    }
}

impl SyncReadable for Tree {
    fn read_column(&self, name: &str, runs: &[Range<u64>]) -> Result<Column> {
        if let Some(name) = name.strip_prefix(super::objects::OBJECTS) {
            return self.read_objects(name, runs);
        }
        let b = self.branch_or_err(name)?;
        self.read_runs(b, runs)
    }
    fn with_source(&self, source: Arc<dyn ReadAt>) -> Self {
        Tree::with_source(self, source)
    }
    fn plan(&self, name: &str, runs: &[Range<u64>]) -> Vec<Range<FileOffset>> {
        self.get_branch(name)
            .map(|b| Tree::plan(self, b, runs))
            .unwrap_or_default()
    }
    fn file_len(&self) -> u64 {
        self.file().len()
    }
}

impl Tree {
    /// A reader of every readable branch over every entry, one batch per
    /// cluster; narrow it with the builder's methods.
    pub fn reader(&self) -> TreeReaderBuilder {
        ReaderBuilder::new(self.clone())
    }
}

impl<H: Readable> ReaderBuilder<H> {
    pub(crate) fn new(handle: H) -> Self {
        let total = handle.num_entries_();
        Self {
            columns: handle.every_column(),
            selection: if total > 0 {
                core::iter::once(0..total).collect()
            } else {
                Vec::new()
            },
            handle,
            batch_size: BatchSize::Cluster,
            offset: 0,
            limit: None,
        }
    }

    /// Which columns to read: branch names for a tree (a group stands for
    /// its sub-branches), field names for an `RNTuple`.
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown column; `Unsupported` for one this version
    /// cannot read.
    pub fn projection(mut self, projection: impl Into<Projection>) -> Result<Self> {
        let Some(names) = projection.into().names else {
            self.columns = self.handle.every_column();
            return Ok(self);
        };
        let mut out: Vec<String> = Vec::new();
        for name in &names {
            for c in self.handle.expand(name)? {
                if !out.contains(&c) {
                    out.push(c);
                }
            }
        }
        self.columns = out;
        Ok(self)
    }

    /// Which entries to read.
    ///
    /// # Errors
    ///
    /// `InvalidInput` if it selects entries the tree or `RNTuple` does not
    /// have.
    pub fn selection(mut self, selection: impl Into<EntrySelection>) -> Result<Self> {
        self.selection = selection.into().resolve(self.handle.num_entries_())?;
        Ok(self)
    }

    /// How many entries a batch holds. The default is one cluster.
    pub fn batch_size(mut self, size: BatchSize) -> Self {
        self.batch_size = size;
        self
    }

    /// Skips the first `n` selected entries.
    pub fn offset(mut self, n: u64) -> Self {
        self.offset = n;
        self
    }

    /// Reads at most `n` selected entries (after the offset).
    pub fn limit(mut self, n: u64) -> Self {
        self.limit = Some(n);
        self
    }

    /// The columns that will be read, in order.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// Reads the columns `R` names (`#[derive(Row)]`), checking once that
    /// each field can read its column. For tuples, see
    /// [`with_row_names`](Self::with_row_names).
    ///
    /// # Errors
    ///
    /// `NotFound` for a missing column; `TypeMismatch` if a field cannot
    /// read its column; `InvalidInput` if `R` names no columns (a tuple).
    pub fn with_row<R: Row>(self) -> Result<Self> {
        let names = R::__names();
        if names.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "a tuple row names no columns: use `rows_of` with the names",
            ));
        }
        self.with_row_names::<R>(&names)
    }

    /// As [`with_row`](Self::with_row), with the columns named here.
    ///
    /// # Errors
    ///
    /// As [`with_row`](Self::with_row), and `InvalidInput` if there are
    /// not as many names as fields.
    pub fn with_row_names<R: Row>(mut self, names: &[&str]) -> Result<Self> {
        if names.len() != R::__WIDTH {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "{} column names for a row of {} fields",
                    names.len(),
                    R::__WIDTH
                ),
            ));
        }
        // Fields of user classes read their branches whole, as objects.
        let classes = R::__classes();
        let columns: Vec<String> = names
            .iter()
            .enumerate()
            .map(|(i, n)| match classes.get(i) {
                Some(Some(_)) => format!("{}{n}", super::objects::OBJECTS),
                _ => (*n).to_owned(),
            })
            .collect();
        for (i, (name, column)) in names.iter().zip(&columns).enumerate() {
            let interpretation = self.handle.interpretation_of(column)?;
            R::__check(i, &interpretation).map_err(|wants| {
                Error::new(
                    ErrorKind::TypeMismatch,
                    format!("column `{name}` holds {interpretation}, not {wants}"),
                )
            })?;
        }
        self.columns = columns;
        Ok(self)
    }

    /// The handle, the columns and the runs of each batch.
    pub(crate) fn into_plan(self) -> (H, Vec<String>, Vec<Vec<Range<u64>>>) {
        let plan = self.plan();
        (self.handle, self.columns, plan)
    }

    /// The runs of each batch.
    fn plan(&self) -> Vec<Vec<Range<u64>>> {
        // Offset and limit apply to selected entries.
        let mut skip = self.offset;
        let mut left = self.limit.unwrap_or(u64::MAX);
        let mut runs = Vec::new();
        for r in &self.selection {
            let start = r.start + skip.min(r.end - r.start);
            skip -= start - r.start;
            let end = start + left.min(r.end - start);
            left -= end - start;
            if start < end {
                runs.push(start..end);
            }
        }
        match self.batch_size {
            BatchSize::Cluster => {
                let mut clusters: Vec<Range<u64>> = self.handle.clusters_().to_vec();
                let covered = clusters.last().map_or(0, |c| c.end);
                if runs.last().is_some_and(|r| r.end > covered) {
                    clusters.push(covered..u64::MAX);
                }
                clusters
                    .iter()
                    .map(|c| {
                        runs.iter()
                            .filter_map(|r| {
                                let (a, b) = (r.start.max(c.start), r.end.min(c.end));
                                (a < b).then_some(a..b)
                            })
                            .collect::<Vec<_>>()
                    })
                    .filter(|b| !b.is_empty())
                    .collect()
            }
            BatchSize::Entries(n) => {
                let n = n.get();
                let (mut out, mut batch, mut size) = (Vec::new(), Vec::new(), 0_u64);
                for mut r in runs {
                    while r.start < r.end {
                        let take = (n - size).min(r.end - r.start);
                        batch.push(r.start..r.start + take);
                        r.start += take;
                        size += take;
                        if size == n {
                            out.push(core::mem::take(&mut batch));
                            size = 0;
                        }
                    }
                }
                if !batch.is_empty() {
                    out.push(batch);
                }
                out
            }
        }
    }
}

macro_rules! sync_build {
    ($handle:ty) => {
        impl ReaderBuilder<$handle> {
            /// The reader: an iterator of batches, read on demand.
            pub fn build(self) -> Reader<$handle> {
                let (handle, columns, plan) = self.into_plan();
                Reader {
                    handle,
                    columns,
                    plan: plan.into_iter(),
                }
            }

            /// Typed rows: the columns `R` names, entry by entry, read one
            /// batch at a time.
            ///
            /// # Errors
            ///
            /// As [`with_row`](Self::with_row).
            pub fn rows<R: Row>(self) -> Result<Rows<$handle, R>> {
                Ok(Rows::new(self.with_row::<R>()?.build()))
            }

            /// Typed rows of a tuple of fields, read from the named columns:
            /// `rows_of::<(f32, Vec<f32>), 2>(["MET_pt", "Muon_pt"])`.
            ///
            /// # Errors
            ///
            /// As [`with_row_names`](Self::with_row_names).
            pub fn rows_of<R: Row, const N: usize>(
                self,
                names: [&str; N],
            ) -> Result<Rows<$handle, R>> {
                Ok(Rows::new(self.with_row_names::<R>(&names)?.build()))
            }

            /// The batches, read in parallel on the current rayon pool, in
            /// order.
            #[cfg(feature = "rayon")]
            pub fn par_batches(
                self,
            ) -> impl rayon::iter::IndexedParallelIterator<Item = Result<Batch>> {
                use rayon::iter::{IntoParallelIterator, ParallelIterator};
                let (handle, columns, plan) = self.into_plan();
                plan.into_par_iter()
                    .map(move |runs| read_batch(&handle, &columns, runs))
            }
        }
    };
}

sync_build!(Tree);
sync_build!(RNTuple);

/// Reads one batch: every column over `runs`.
pub(crate) fn read_batch<H: SyncReadable>(
    handle: &H,
    columns: &[String],
    runs: Vec<Range<u64>>,
) -> Result<Batch> {
    let mut out = Vec::with_capacity(columns.len());
    for name in columns {
        out.push((name.clone(), handle.read_column(name, &runs)?));
    }
    Ok(Batch {
        entries: runs,
        columns: out,
    })
}

/// Batches of entries, read one at a time; made by
/// [`ReaderBuilder::build`] for a sync handle.
#[derive(Debug)]
pub struct Reader<H> {
    handle: H,
    columns: Vec<String>,
    plan: std::vec::IntoIter<Vec<Range<u64>>>,
}

/// The reader of a [`Tree`].
pub type TreeReader = Reader<Tree>;

/// The reader of an [`RNTuple`].
pub type RNTupleReader = Reader<RNTuple>;

impl<H: SyncReadable> Iterator for Reader<H> {
    type Item = Result<Batch>;

    fn next(&mut self) -> Option<Result<Batch>> {
        let runs = self.plan.next()?;
        Some(read_batch(&self.handle, &self.columns, runs))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.plan.size_hint()
    }
}

impl<H: SyncReadable> ExactSizeIterator for Reader<H> {}

/// Typed rows, read one batch at a time; made by `rows` and `rows_of` of a
/// [`ReaderBuilder`], a [`Tree`] or an [`RNTuple`].
pub struct Rows<H, R> {
    reader: Reader<H>,
    batch: std::vec::IntoIter<R>,
}

impl<H, R> core::fmt::Debug for Rows<H, R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Rows")
            .field("columns", &self.reader.columns)
            .finish_non_exhaustive()
    }
}

impl<H, R> Rows<H, R> {
    fn new(reader: Reader<H>) -> Self {
        Self {
            reader,
            batch: Vec::new().into_iter(),
        }
    }
}

impl<H: SyncReadable, R: Row> Iterator for Rows<H, R> {
    type Item = Result<R>;

    fn next(&mut self) -> Option<Result<R>> {
        loop {
            if let Some(row) = self.batch.next() {
                return Some(Ok(row));
            }
            let batch = match self.reader.next()? {
                Ok(b) => b,
                Err(e) => return Some(Err(e)),
            };
            let columns = batch.into_columns().into_iter().map(|(_, c)| c).collect();
            match R::__rows(columns) {
                Ok(rows) => self.batch = rows.into_iter(),
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

impl<H: SyncReadable, R: Row> FusedIterator for Rows<H, R> {}

impl Tree {
    /// Typed rows: `tree.rows::<Event>()` for a `#[derive(Row)]` type.
    ///
    /// # Errors
    ///
    /// As [`ReaderBuilder::with_row`].
    pub fn rows<R: Row>(&self) -> Result<TreeRows<R>> {
        self.reader().rows::<R>()
    }

    /// Typed rows of a tuple:
    /// `tree.rows_of::<(f32, Vec<f32>), 2>(["MET_pt", "Muon_pt"])`.
    ///
    /// # Errors
    ///
    /// As [`ReaderBuilder::with_row_names`].
    pub fn rows_of<R: Row, const N: usize>(&self, names: [&str; N]) -> Result<TreeRows<R>> {
        self.reader().rows_of::<R, N>(names)
    }
}

impl RNTuple {
    /// Typed rows: `ntuple.rows::<Event>()` for a `#[derive(Row)]` type.
    ///
    /// # Errors
    ///
    /// As [`ReaderBuilder::with_row`].
    pub fn rows<R: Row>(&self) -> Result<RNTupleRows<R>> {
        self.reader().rows::<R>()
    }

    /// Typed rows of a tuple, from the named fields.
    ///
    /// # Errors
    ///
    /// As [`ReaderBuilder::with_row_names`].
    pub fn rows_of<R: Row, const N: usize>(&self, names: [&str; N]) -> Result<RNTupleRows<R>> {
        self.reader().rows_of::<R, N>(names)
    }
}

/// The typed rows of a [`Tree`].
pub type TreeRows<R> = Rows<Tree, R>;

/// The typed rows of an [`RNTuple`].
pub type RNTupleRows<R> = Rows<RNTuple, R>;
impl<H: SyncReadable> FusedIterator for Reader<H> {}

#[cfg(test)]
#[expect(clippy::single_range_in_vec_init, reason = "selections of one run")]
mod tests {
    use super::*;

    fn runs(s: &EntrySelection, total: u64) -> Vec<Range<u64>> {
        s.resolve(total).expect("resolve")
    }

    #[test]
    fn selections_normalize_and_combine() {
        let s = EntrySelection::from_ranges([5..8, 0..2, 1..3, 8..9, 4..4]);
        assert_eq!(runs(&s, 10), [0..3, 5..9]);
        assert_eq!(s.selected(10), 7);
        assert_eq!(
            runs(&EntrySelection::from_entry_list(&[3, 1, 2, 7]), 10),
            [1..4, 7..8]
        );
        assert_eq!(runs(&EntrySelection::from(4..), 10), [4..10]);
        assert_eq!(runs(&EntrySelection::from(..), 0), []);

        let a = EntrySelection::from_ranges([0..4, 6..10]);
        let b = EntrySelection::from(3..7);
        assert_eq!(runs(&a.union(&b), 10), [0..10]);
        assert_eq!(runs(&a.intersect(&b), 10), [3..4, 6..7]);
        // The 2nd to 5th selected entries of `a`: 2, 3, 6, 7.
        assert_eq!(
            runs(&a.and_then(&EntrySelection::from(2..6)), 10),
            [2..4, 6..8]
        );
        assert_eq!(
            runs(
                &EntrySelection::from(5..).and_then(&EntrySelection::from(2..)),
                10
            ),
            [7..10]
        );
        assert_eq!(runs(&EntrySelection::all().intersect(&b), 10), [3..7]);
    }

    #[test]
    fn bad_selections_fail_when_used() {
        let (hi, lo) = (5, 3);
        let backwards = EntrySelection::from_ranges([hi..lo]);
        assert_eq!(
            backwards.resolve(10).expect_err("backwards").kind(),
            ErrorKind::InvalidInput
        );
        assert_eq!(
            EntrySelection::from(8..12)
                .resolve(10)
                .expect_err("beyond")
                .kind(),
            ErrorKind::InvalidInput
        );
        assert_eq!(
            EntrySelection::from(11..)
                .resolve(10)
                .expect_err("tail")
                .kind(),
            ErrorKind::InvalidInput
        );
    }

    /// Selections against a model: the set of entries, as booleans.
    mod model {
        use super::*;
        use proptest::prelude::*;

        const N: u64 = 64;
        const WIDE: u64 = 256;

        /// A selection and the entries it holds below `WIDE`.
        fn selection() -> impl Strategy<Value = (EntrySelection, Vec<bool>)> {
            (
                prop::collection::vec((0..=N, 0..=N), 0..6),
                prop::collection::vec(0..N, 0..6),
                prop::option::of(0..=N),
            )
                .prop_map(|(ranges, list, tail)| {
                    let ranges: Vec<Range<u64>> = ranges
                        .into_iter()
                        .map(|(a, b)| a.min(b)..a.max(b))
                        .collect();
                    let mut set = vec![false; usize::try_from(WIDE).unwrap_or(0)];
                    let mut mark = |e: u64| {
                        if let Some(s) = set.get_mut(usize::try_from(e).unwrap_or(usize::MAX)) {
                            *s = true;
                        }
                    };
                    ranges.iter().flat_map(Clone::clone).for_each(&mut mark);
                    list.iter().copied().for_each(&mut mark);
                    let mut sel = EntrySelection::from_ranges(ranges)
                        .union(&EntrySelection::from_entry_list(&list));
                    if let Some(t) = tail {
                        (t..WIDE).for_each(&mut mark);
                        sel = sel.union(&EntrySelection::from(t..));
                    }
                    (sel, set)
                })
        }

        fn members(s: &EntrySelection, total: u64) -> Vec<bool> {
            let mut set = vec![false; usize::try_from(WIDE).unwrap_or(0)];
            for r in s.resolve(total).expect("resolve") {
                for e in r {
                    set[usize::try_from(e).expect("small")] = true;
                }
            }
            set
        }

        fn below(set: &[bool], n: u64) -> Vec<bool> {
            set.iter()
                .enumerate()
                .map(|(i, &x)| x && (i as u64) < n)
                .collect()
        }

        proptest! {
            #[test]
            fn runs_are_normal_and_hold_the_entries((s, set) in selection()) {
                let runs = s.resolve(N).expect("resolve");
                // Sorted, non-empty, and neither overlapping nor touching.
                for w in runs.windows(2) {
                    prop_assert!(w[0].end < w[1].start, "{runs:?}");
                }
                prop_assert!(runs.iter().all(|r| r.start < r.end));
                prop_assert_eq!(members(&s, N), below(&set, N));
                let count = below(&set, N).iter().filter(|&&x| x).count() as u64;
                prop_assert_eq!(s.selected(N), count);
            }

            #[test]
            fn union_and_intersection_are_the_set_operations(
                (a, sa) in selection(),
                (b, sb) in selection(),
            ) {
                let union: Vec<bool> = sa.iter().zip(&sb).map(|(x, y)| *x || *y).collect();
                let both: Vec<bool> = sa.iter().zip(&sb).map(|(x, y)| *x && *y).collect();
                prop_assert_eq!(members(&a.union(&b), N), below(&union, N));
                prop_assert_eq!(members(&a.intersect(&b), N), below(&both, N));
                prop_assert_eq!(a.union(&b), b.union(&a));
                prop_assert_eq!(a.intersect(&b), b.intersect(&a));
            }

            #[test]
            fn and_then_picks_among_the_selected(
                (a, sa) in selection(),
                (b, sb) in selection(),
            ) {
                // Entry k of `b` is the k-th entry `a` selects.
                let picked: Vec<u64> =
                    sa.iter().enumerate().filter(|(_, x)| **x).map(|(i, _)| i as u64).collect();
                let mut want = vec![false; usize::try_from(WIDE).unwrap_or(0)];
                for (k, _) in sb.iter().enumerate().filter(|(_, x)| **x) {
                    if let Some(&e) = picked.get(k) {
                        want[usize::try_from(e).expect("small")] = true;
                    }
                }
                prop_assert_eq!(members(&a.and_then(&b), WIDE), want);
            }
        }
    }

    #[test]
    fn projections_combine() {
        let mut p = Projection::from(["a", "b"]);
        p.union(&Projection::from(["b", "c"]));
        assert_eq!(p, Projection::names(["a", "b", "c"]));
        p.intersect(&Projection::from(["c", "a", "z"]));
        assert_eq!(p, Projection::names(["a", "c"]));
        let mut all = Projection::all();
        all.intersect(&Projection::from("x"));
        assert_eq!(all, Projection::from("x"));
    }
}
