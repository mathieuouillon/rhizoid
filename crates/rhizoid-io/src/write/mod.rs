//! Writing files: [`create`], [`update`], [`WritableFile`].
//!
//! Objects are written with [`put`](FileScope::put), flat trees with
//! [`tree`](FileScope::tree), `RNTuple`s with [`rntuple`](FileScope::rntuple),
//! into the root directory or a subdirectory ([`mkdir`](FileScope::mkdir)).
//! [`update`] adds to an existing file with one commit point: the file
//! reads as before until the update is complete.

mod scope;
mod update;

use core::fmt;
use std::path::Path;
use std::sync::Arc;

use rhizoid_source::{FileOffset, WriteAt};

pub use scope::{DirWriter, FileScope, create, create_with, update, update_with};
pub use update::UpdateOptions;

use crate::compress::{self, Compression};
use crate::core::buffer::WBuffer;
use crate::core::directory::{
    DirectoryRecord, FreeSegment, encode_free_list, encode_key_list, free_list_len,
};
use crate::core::error::{Error, ErrorKind, Result};
use crate::core::header::FileHeader;
use crate::core::key::{KeyForm, KeyHeader};
use crate::core::time::{Datime, Uuid};
use crate::ntuple::write::NTupleState;
use crate::ntuple::{RNTupleOptions, RNTupleWriter};
use crate::streamer::{self, DynObject, Encoder, StreamerInfo, StreamerSet, ToDyn, Value};
use crate::tree::write::{BASKET_SIZE, Target, TreeState};
use crate::tree::{TreeOptions, TreeWriter};
use crate::user::Row;

/// The basket header after the key: version, `fBufferSize`,
/// `fNevBufSize`, `fNevBuf`, `fLast`, and a flag byte.
const BASKET_HEADER_LEN: u16 = 19;

/// The ROOT version the files we write claim, matching the compatibility
/// target: readable by ROOT ≥ 6.26.
const CLAIMED_ROOT_VERSION: i32 = 62_600;

/// The class of a subdirectory's key and key list.
const DIRECTORY_CLASS: &str = "TDirectory";

/// An `RNTuple` envelope written: where its bytes start, how many are
/// stored, and its length unpacked.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EnvelopeRecord {
    pub(crate) offset: u64,
    pub(crate) nbytes: u64,
    pub(crate) len: u64,
}

/// A basket record written.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BasketRecord {
    pub(crate) seek: u64,
    pub(crate) key_len: u16,
    pub(crate) obj_len: u32,
    pub(crate) nbytes: u32,
}

/// Options for writing.
#[derive(Clone, Debug, Default)]
pub struct WriteOptions {
    compression: Compression,
    reproducible: bool,
    title: String,
}

impl WriteOptions {
    /// Sets the default compression. The default is zlib level 1, as ROOT.
    #[must_use]
    pub fn compression(mut self, c: Compression) -> Self {
        self.compression = c;
        self
    }

    /// Writes ROOT's `kReproducible` values (datime 1, zero UUIDs), so that
    /// outputs compare byte for byte. The default is `false`.
    #[must_use]
    pub fn reproducible(mut self, yes: bool) -> Self {
        self.reproducible = yes;
        self
    }

    /// Sets the file's title. The default is empty.
    #[must_use]
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }
}

/// A directory of a file being written: its record, its keys, and whether
/// this session changed it.
#[derive(Clone, Debug)]
pub(crate) struct DirState {
    pub(crate) name: String,
    pub(crate) title: String,
    pub(crate) parent: Option<usize>,
    pub(crate) record: DirectoryRecord,
    /// A subdirectory's own key (its record follows the key header).
    pub(crate) key: Option<KeyHeader>,
    pub(crate) keys: Vec<KeyHeader>,
    /// Changed by this session.
    pub(crate) dirty: bool,
    /// Created by this session (its records are not yet referenced).
    pub(crate) new: bool,
}

impl DirState {
    /// Where the directory record is.
    fn record_at(&self) -> Result<FileOffset> {
        let at = match &self.key {
            Some(k) => k.seek_key.checked_add(u64::from(k.key_len)),
            None => self
                .record
                .seek_dir
                .checked_add(u64::from(self.record.nbytes_name)),
        };
        at.ok_or_else(|| Error::malformed("directory record past the end"))
    }
}

/// A handle on a directory of a [`WritableFile`], from
/// [`mkdir`](WritableFile::mkdir). It belongs to the file that made it:
/// another file refuses it ([`DirId::ROOT`] excepted, every file's root).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DirId {
    /// The [`WritableFile`]'s id; 0 for any file.
    file: u64,
    index: usize,
}

impl DirId {
    /// The root directory, of whichever file it is used with.
    pub const ROOT: Self = Self { file: 0, index: 0 };
}

/// A new id for each [`WritableFile`], for its [`DirId`]s (0 is none).
pub(crate) fn next_file_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// A file being written. Finish it with [`close`](WritableFile::close):
/// dropping it performs no I/O. A new file dropped unclosed reads as "not
/// closed", which readers (ROOT, and this crate) recover; an update dropped
/// uncommitted leaves the file as it was.
#[must_use = "a WritableFile must be closed with `close()` to be valid"]
pub struct WritableFile {
    sink: Box<dyn WriteAt>,
    options: WriteOptions,
    name: String,
    end: FileOffset,
    nbytes_name: u32,
    uuid: Uuid,
    pub(crate) dirs: Vec<DirState>,
    classes: Vec<String>,
    /// The descriptions objects were written with.
    infos: Vec<Arc<StreamerInfo>>,
    /// The descriptions of the files objects were read from.
    sources: Vec<Arc<StreamerSet>>,
    /// Trees whose writers were dropped unfinished, finished at close.
    pending: Vec<TreeState>,
    /// The same for `RNTuple`s.
    pending_ntuples: Vec<NTupleState>,
    /// The file as it was, in update mode.
    update: Option<Box<update::UpdateState>>,
    /// Ties this file's [`DirId`]s to it.
    pub(crate) id: u64,
}

impl fmt::Debug for WritableFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WritableFile")
            .field("name", &self.name)
            .field("end", &self.end)
            .field("update", &self.update.is_some())
            .finish_non_exhaustive()
    }
}

fn invalid_path(name: &str) -> Error {
    Error::new(ErrorKind::InvalidPath, format!("invalid key name `{name}`"))
}

fn check_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains(['/', ';']) {
        return Err(invalid_path(name));
    }
    Ok(())
}

impl WritableFile {
    /// Creates (or truncates) a file with default options.
    ///
    /// # Errors
    ///
    /// I/O errors creating or writing the file.
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        Self::create_with(path, WriteOptions::default())
    }

    /// Creates (or truncates) a file with options.
    ///
    /// # Errors
    ///
    /// I/O errors creating or writing the file.
    pub fn create_with(path: impl AsRef<Path>, options: WriteOptions) -> Result<Self> {
        fn inner(path: &Path, options: WriteOptions) -> Result<WritableFile> {
            let f = std::fs::File::options()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)?;
            let name = path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            WritableFile::to_sink(Box::new(f), name, options)
        }
        inner(path.as_ref(), options)
    }

    /// Opens an existing file to add to it; see [`update`]. Nothing is
    /// changed until [`close`](Self::close) commits.
    ///
    /// # Errors
    ///
    /// As [`update`].
    pub fn update(path: impl AsRef<Path>) -> Result<Self> {
        Self::update_with(path, UpdateOptions::default())
    }

    /// [`update`](Self::update) with options.
    ///
    /// # Errors
    ///
    /// As [`update`].
    pub fn update_with(path: impl AsRef<Path>, options: UpdateOptions) -> Result<Self> {
        update::open(path.as_ref(), options)
    }

    /// Updates a file read through `source` and written through `sink`,
    /// which must be the same bytes (an in-memory file, a test double).
    /// The sink must support [`WriteAt::sync`]; nothing is locked, and
    /// `copy_on_write` does not apply.
    ///
    /// # Errors
    ///
    /// As [`update`].
    pub fn update_in(
        source: impl rhizoid_source::ReadAt + 'static,
        sink: Box<dyn WriteAt>,
        options: UpdateOptions,
    ) -> Result<Self> {
        let reader = crate::File::from_source(source, update::strict())?;
        update::open_sink(reader, sink, options, false)
    }

    /// Starts a file in any [`WriteAt`] sink, under the given file name.
    ///
    /// # Errors
    ///
    /// I/O errors of the sink.
    pub fn to_sink(
        sink: Box<dyn WriteAt>,
        name: impl Into<String>,
        options: WriteOptions,
    ) -> Result<Self> {
        let name = name.into();
        let now = if options.reproducible {
            Datime::REPRODUCIBLE
        } else {
            Datime::now()
        };
        let uuid = if options.reproducible {
            Uuid::NIL
        } else {
            Uuid::new_random()
        };
        let begin = FileOffset::BEGIN;
        // TFile key at fBEGIN: key, TNamed (name, title), directory record.
        let named_len = crate::core::buffer::string_len(&name)
            + crate::core::buffer::string_len(&options.title);
        let obj_len = u32::try_from(named_len + DirectoryRecord::LEN)
            .map_err(|_| Error::malformed("file name too long"))?;
        let key = KeyHeader::for_record(
            KeyForm::ByPosition,
            begin,
            FileOffset::new(0),
            "TFile",
            &name,
            &options.title,
            obj_len,
            obj_len,
            1,
            now,
        )?;
        let nbytes_name = u32::from(key.key_len)
            + u32::try_from(named_len).map_err(|_| Error::malformed("name too long"))?;
        let record = DirectoryRecord::new(now, nbytes_name, begin, FileOffset::new(0), uuid);
        let end = begin
            .checked_add(u64::from(key.nbytes))
            .ok_or_else(|| Error::malformed("file key too large"))?;
        let root = DirState {
            name: name.clone(),
            title: options.title.clone(),
            parent: None,
            record,
            key: None,
            keys: Vec::new(),
            dirty: true,
            new: true,
        };
        let mut file = Self {
            sink,
            options,
            name,
            end,
            nbytes_name,
            uuid,
            dirs: vec![root],
            classes: Vec::new(),
            infos: Vec::new(),
            sources: Vec::new(),
            pending: Vec::new(),
            pending_ntuples: Vec::new(),
            update: None,
            id: next_file_id(),
        };
        let mut w = WBuffer::new();
        key.encode(&mut w)?;
        w.write_string(&file.name)?;
        w.write_string(&file.options.title)?;
        file.dirs[0].record.encode(&mut w)?;
        // Header first with placeholders, rewritten at close.
        let header = file.header(FileOffset::new(0), 0, FileOffset::new(0), 0, 0)?;
        file.sink.write_all_at(FileOffset::new(0), &header)?;
        file.sink.write_all_at(begin, &w.into_inner())?;
        Ok(file)
    }

    fn header(
        &self,
        seek_free: FileOffset,
        nbytes_free: u32,
        seek_info: FileOffset,
        nbytes_info: u32,
        nfree: u32,
    ) -> Result<Vec<u8>> {
        // An update keeps the file's version, fBEGIN and default compression.
        let (version, begin, compress) = match &self.update {
            Some(u) => (u.header.version, u.header.begin, u.header.compress),
            None => (
                CLAIMED_ROOT_VERSION,
                100,
                self.options.compression.setting(),
            ),
        };
        FileHeader {
            version,
            begin,
            end: self.end,
            seek_free,
            nbytes_free,
            nfree,
            nbytes_name: self.nbytes_name,
            units: 4,
            compress,
            seek_info,
            nbytes_info,
            uuid: self.uuid,
        }
        .encode()
    }

    fn now(&self) -> Datime {
        if self.options.reproducible {
            Datime::REPRODUCIBLE
        } else {
            Datime::now()
        }
    }

    /// Writes an object into the root directory under `name`. Writing the
    /// same name again adds a new cycle (`h;2` next to `h;1`), as ROOT does.
    ///
    /// The key's title is the object's title (`fTitle`), when it has one.
    ///
    /// # Errors
    ///
    /// `InvalidPath` for an empty name or one with `/` or `;`;
    /// `Unsupported` if the object's class has no known description;
    /// `InvalidInput` if a member does not fit it; I/O errors.
    pub fn put<T: ToDyn + ?Sized>(&mut self, name: &str, object: &T) -> Result<()> {
        self.put_dyn(DirId::ROOT.index, name, &object.to_dyn())
    }

    /// Writes an object into a directory from [`mkdir`](Self::mkdir).
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a [`DirId`] of another file; as [`put`](Self::put).
    pub fn put_in<T: ToDyn + ?Sized>(&mut self, dir: DirId, name: &str, object: &T) -> Result<()> {
        let index = self.dir_index(dir)?;
        self.put_dyn(index, name, &object.to_dyn())
    }

    /// The directory a [`DirId`] names in this file.
    fn dir_index(&self, dir: DirId) -> Result<usize> {
        if dir.file != 0 && dir.file != self.id {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "this directory belongs to another file",
            ));
        }
        if dir.index >= self.dirs.len() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "no such directory in this file",
            ));
        }
        Ok(dir.index)
    }

    /// Creates a directory and the missing ones above it (`"a/b"`), as
    /// `std::fs::create_dir_all` does, or finds it if it exists.
    ///
    /// # Errors
    ///
    /// `InvalidPath` for an empty component or one with `;`; `ClassMismatch`
    /// if a component names an object that is not a directory; I/O errors.
    pub fn mkdir(&mut self, path: &str) -> Result<DirId> {
        let index = self.mkdir_in(DirId::ROOT.index, path)?;
        Ok(DirId {
            file: self.id,
            index,
        })
    }

    pub(crate) fn mkdir_in(&mut self, from: usize, path: &str) -> Result<usize> {
        let mut at = from;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            check_name(part)?;
            at = self.child_dir(at, part)?;
        }
        Ok(at)
    }

    /// The subdirectory `name` of directory `parent`: loaded, found in the
    /// file (update mode), or created.
    fn child_dir(&mut self, parent: usize, name: &str) -> Result<usize> {
        if let Some(i) = self
            .dirs
            .iter()
            .position(|d| d.parent == Some(parent) && d.name == name)
        {
            return Ok(i);
        }
        let existing = self.dirs[parent]
            .keys
            .iter()
            .filter(|k| k.name == name)
            .max_by_key(|k| k.cycle)
            .cloned();
        if let Some(key) = existing {
            if key.class_name != DIRECTORY_CLASS && key.class_name != "TDirectoryFile" {
                return Err(Error::new(
                    ErrorKind::ClassMismatch,
                    format!("`{name}` is a {}, not a directory", key.class_name),
                ));
            }
            let state = update::load_dir(self, parent, &key)?;
            self.dirs.push(state);
            return Ok(self.dirs.len() - 1);
        }
        self.new_dir(parent, name, "")
    }

    /// Writes a new subdirectory's key, with its record (rewritten when the
    /// file closes), and lists it in its parent.
    fn new_dir(&mut self, parent: usize, name: &str, title: &str) -> Result<usize> {
        let now = self.now();
        let len = u32::try_from(DirectoryRecord::LEN).map_err(|_| Error::malformed("record"))?;
        let mut key = KeyHeader::for_record(
            KeyForm::ByPosition,
            self.end,
            self.dirs[parent].record.seek_dir,
            DIRECTORY_CLASS,
            name,
            title,
            len,
            len,
            1,
            now,
        )?;
        key.cycle = self.next_cycle(parent, name)?;
        let uuid = if self.options.reproducible {
            Uuid::NIL
        } else {
            Uuid::new_random()
        };
        let record = DirectoryRecord::new(
            now,
            u32::from(key.key_len),
            key.seek_key,
            self.dirs[parent].record.seek_dir,
            uuid,
        );
        let mut w = WBuffer::new();
        key.encode(&mut w)?;
        record.encode(&mut w)?;
        self.write_at_end(&w.into_inner(), key.nbytes)?;
        self.dirs[parent].keys.push(key.clone());
        self.dirs[parent].dirty = true;
        self.dirs.push(DirState {
            name: name.to_owned(),
            title: title.to_owned(),
            parent: Some(parent),
            record,
            key: Some(key),
            keys: Vec::new(),
            dirty: true,
            new: true,
        });
        Ok(self.dirs.len() - 1)
    }

    fn write_at_end(&mut self, bytes: &[u8], nbytes: u32) -> Result<()> {
        self.sink.write_all_at(self.end, bytes)?;
        self.end = self
            .end
            .checked_add(u64::from(nbytes))
            .ok_or_else(|| Error::new(ErrorKind::LimitExceeded, "file too large"))?;
        Ok(())
    }

    /// The key of `name` in a directory, the highest cycle.
    pub(crate) fn find_key(&self, dir: usize, name: &str) -> Option<&KeyHeader> {
        self.dirs[dir]
            .keys
            .iter()
            .filter(|k| k.name == name)
            .max_by_key(|k| k.cycle)
    }

    /// The path of a directory from the root, for messages.
    pub(crate) fn dir_path(&self, dir: usize) -> String {
        let mut parts = Vec::new();
        let mut at = Some(dir);
        while let Some(i) = at {
            if self.dirs[i].parent.is_some() {
                parts.push(self.dirs[i].name.as_str());
            }
            at = self.dirs[i].parent;
        }
        parts.reverse();
        parts.join("/")
    }

    /// Whether a record was in the file before this update (always false
    /// for a new file).
    pub(crate) fn is_old(&self, at: FileOffset) -> bool {
        self.update.as_ref().is_some_and(|u| at < u.old_end)
    }

    /// Removes a key from a directory (an older cycle replaced by this
    /// session); its record is freed once the update is committed.
    pub(crate) fn drop_key(&mut self, dir: usize, key: &KeyHeader) {
        let d = &mut self.dirs[dir];
        if let Some(i) = d
            .keys
            .iter()
            .position(|k| k.seek_key == key.seek_key && k.name == key.name)
        {
            d.keys.remove(i);
            d.dirty = true;
            if let Some(u) = self.update.as_mut()
                && key.seek_key < u.old_end
            {
                u.freed.push((key.seek_key, key.nbytes));
            }
        }
    }

    /// The reader of the file as it was, in update mode.
    pub(crate) fn reader(&self) -> Option<&crate::File> {
        self.update.as_ref().map(|u| &u.reader)
    }

    /// Starts a tree of `R` rows, a `#[derive(Row)]` type; see [`TreeWriter`].
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a tuple row (use [`tree_of`](Self::tree_of)),
    /// bad or duplicate branch names; `Unsupported` for fields that cannot
    /// be written (`Value`).
    pub fn tree<R: Row>(&mut self, name: &str) -> Result<TreeWriter<'_, R>> {
        self.tree_with(name, TreeOptions::default())
    }

    /// [`tree`](Self::tree) with options.
    ///
    /// # Errors
    ///
    /// As [`tree`](Self::tree).
    pub fn tree_with<R: Row>(
        &mut self,
        name: &str,
        options: TreeOptions,
    ) -> Result<TreeWriter<'_, R>> {
        let names = row_names::<R>()?;
        TreeWriter::new(Target::file(self, 0), name, &names, options)
    }

    /// Starts a tree of tuple rows, naming the branches:
    /// `tree_of::<(i32, Vec<f32>), 2>("t", ["run", "pt"])`.
    ///
    /// # Errors
    ///
    /// As [`tree`](Self::tree).
    pub fn tree_of<R: Row, const N: usize>(
        &mut self,
        name: &str,
        names: [&str; N],
    ) -> Result<TreeWriter<'_, R>> {
        let names = names.map(str::to_owned);
        TreeWriter::new(Target::file(self, 0), name, &names, TreeOptions::default())
    }

    pub(crate) fn defer_tree(&mut self, tree: TreeState) {
        self.pending.push(tree);
    }

    /// Starts an `RNTuple` of `R` entries, a `#[derive(Row)]` type; see
    /// [`RNTupleWriter`].
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a tuple row (use [`rntuple_of`](Self::rntuple_of))
    /// or bad or duplicate field names; `Unsupported` for fields that
    /// cannot be written (`Value`), or for an `RNTuple` that exists already
    /// (extending one is not supported); I/O errors writing the header.
    pub fn rntuple<R: Row>(&mut self, name: &str) -> Result<RNTupleWriter<'_, R>> {
        self.rntuple_with(name, RNTupleOptions::default())
    }

    /// [`rntuple`](Self::rntuple) with options.
    ///
    /// # Errors
    ///
    /// As [`rntuple`](Self::rntuple).
    pub fn rntuple_with<R: Row>(
        &mut self,
        name: &str,
        options: RNTupleOptions,
    ) -> Result<RNTupleWriter<'_, R>> {
        let names = row_names::<R>()?;
        RNTupleWriter::new(Target::file(self, 0), name, &names, options)
    }

    /// Starts an `RNTuple` of tuple entries, naming the fields:
    /// `rntuple_of::<(i32, Vec<f32>), 2>("t", ["run", "pt"])`.
    ///
    /// # Errors
    ///
    /// As [`rntuple`](Self::rntuple).
    pub fn rntuple_of<R: Row, const N: usize>(
        &mut self,
        name: &str,
        names: [&str; N],
    ) -> Result<RNTupleWriter<'_, R>> {
        let names = names.map(str::to_owned);
        RNTupleWriter::new(
            Target::file(self, 0),
            name,
            &names,
            RNTupleOptions::default(),
        )
    }

    pub(crate) fn defer_ntuple(&mut self, ntuple: NTupleState) {
        self.pending_ntuples.push(ntuple);
    }

    /// Registers descriptions of classes split into branches, for the
    /// `StreamerInfo` record.
    pub(crate) fn register_infos(&mut self, infos: &[Arc<StreamerInfo>]) {
        for i in infos {
            if !self.classes.iter().any(|c| c == i.class_name()) {
                self.classes.push(i.class_name().to_owned());
            }
            if !self
                .infos
                .iter()
                .any(|j| j.class_name() == i.class_name() && j.class_version() == i.class_version())
            {
                self.infos.push(Arc::clone(i));
            }
        }
    }

    /// The file's compression.
    pub(crate) fn compression_setting_value(&self) -> Compression {
        self.options.compression
    }

    /// The file's compression setting (`fCompress`).
    pub(crate) fn compression_setting(&self) -> i32 {
        self.options.compression.setting()
    }

    /// Appends an `RBlob` record: a large key, cycle 1, no name,
    /// holding `stored` as is, with `ObjLen` = `obj_len`. Returns where
    /// `stored` starts.
    pub(crate) fn append_blob(&mut self, dir: usize, stored: &[u8], obj_len: u32) -> Result<u64> {
        let data_len = u32::try_from(stored.len())
            .map_err(|_| Error::new(ErrorKind::LimitExceeded, "blob too large"))?;
        let key = KeyHeader::for_record(
            KeyForm::AlwaysLarge,
            self.end,
            self.dirs[dir].record.seek_dir,
            "RBlob",
            "",
            "",
            obj_len,
            data_len,
            1,
            self.now(),
        )?;
        let mut w = WBuffer::new();
        key.encode(&mut w)?;
        let at = self.end.get() + u64::from(key.key_len);
        w.write_bytes(stored);
        self.write_at_end(&w.into_inner(), key.nbytes)?;
        Ok(at)
    }

    /// Appends an `RNTuple` envelope as an `RBlob`, compressed (whatever
    /// its size, as ROOT does) when that makes it smaller.
    pub(crate) fn append_envelope(
        &mut self,
        dir: usize,
        envelope: &[u8],
    ) -> Result<EnvelopeRecord> {
        let stored = compress::compress_basket(self.options.compression, envelope)?;
        let stored = stored.as_deref().unwrap_or(envelope);
        let len = u32::try_from(envelope.len())
            .map_err(|_| Error::new(ErrorKind::LimitExceeded, "envelope too large"))?;
        let offset = self.append_blob(dir, stored, len)?;
        Ok(EnvelopeRecord {
            offset,
            nbytes: stored.len() as u64,
            len: u64::from(len),
        })
    }

    /// Writes an `RNTuple`'s anchor (`ROOT::RNTuple`, already encoded)
    /// into a directory under `name`.
    pub(crate) fn put_anchor(&mut self, dir: usize, name: &str, anchor: &[u8]) -> Result<()> {
        let class = "ROOT::RNTuple";
        if !self.classes.iter().any(|c| c == class) {
            self.classes.push(class.to_owned());
        }
        let cycle = self.next_cycle(dir, name)?;
        let key = self.append_record(dir, class, name, "", anchor, Compression::NONE, cycle)?;
        self.dirs[dir].keys.push(key);
        self.dirs[dir].dirty = true;
        Ok(())
    }

    fn next_cycle(&self, dir: usize, name: &str) -> Result<i16> {
        self.dirs[dir]
            .keys
            .iter()
            .filter(|k| k.name == name)
            .map(|k| k.cycle)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorKind::LimitExceeded, "too many cycles"))
    }

    /// Appends a basket: a large key with the basket header, then
    /// the entries and, for entries of variable size, their offsets
    /// (counted from the key's start; the last is written as 0, as ROOT
    /// does: readers take the end from `fLast`).
    #[expect(clippy::too_many_arguments, reason = "the basket header's fields")]
    pub(crate) fn append_basket(
        &mut self,
        dir: usize,
        branch: &str,
        tree: &str,
        data: &[u8],
        starts: Option<&[usize]>,
        nev_buf: i32,
        nev_buf_size: i32,
    ) -> Result<BasketRecord> {
        let too_large = || Error::new(ErrorKind::LimitExceeded, "basket too large");
        let key_len = u16::try_from(KeyHeader::encoded_len("TBasket", branch, tree, true))
            .ok()
            .and_then(|k| k.checked_add(BASKET_HEADER_LEN))
            .ok_or_else(|| Error::malformed("key header too long"))?;
        let mut object = Vec::with_capacity(data.len() + starts.map_or(0, |s| 4 * (s.len() + 2)));
        object.extend_from_slice(data);
        let last = i32::try_from(usize::from(key_len) + data.len()).map_err(|_| too_large())?;
        if let Some(starts) = starts {
            let n = i32::try_from(starts.len() + 1).map_err(|_| too_large())?;
            object.extend_from_slice(&n.to_be_bytes());
            for &s in starts {
                let at = i32::try_from(usize::from(key_len) + s).map_err(|_| too_large())?;
                object.extend_from_slice(&at.to_be_bytes());
            }
            object.extend_from_slice(&0i32.to_be_bytes());
        }
        let stored = compress::compress_basket(self.options.compression, &object)?;
        let stored = stored.as_deref().unwrap_or(&object);
        let obj_len = u32::try_from(object.len()).map_err(|_| too_large())?;
        let data_len = u32::try_from(stored.len()).map_err(|_| too_large())?;
        let mut key = KeyHeader::for_record(
            KeyForm::AlwaysLarge,
            self.end,
            self.dirs[dir].record.seek_dir,
            "TBasket",
            branch,
            tree,
            obj_len,
            data_len,
            0,
            self.now(),
        )?;
        key.key_len = key_len;
        key.nbytes = u32::from(key_len)
            .checked_add(data_len)
            .ok_or_else(too_large)?;
        let mut w = WBuffer::new();
        key.encode(&mut w)?;
        w.write_i16(3);
        w.write_i32(BASKET_SIZE);
        w.write_i32(nev_buf_size);
        w.write_i32(nev_buf);
        w.write_i32(last);
        w.write_u8(0);
        debug_assert_eq!(w.len(), usize::from(key_len), "basket key length");
        w.write_bytes(stored);
        let seek = self.end;
        self.write_at_end(&w.into_inner(), key.nbytes)?;
        Ok(BasketRecord {
            seek: seek.get(),
            key_len,
            obj_len,
            nbytes: key.nbytes,
        })
    }

    pub(crate) fn put_dyn(&mut self, dir: usize, name: &str, object: &DynObject) -> Result<()> {
        check_name(name)?;
        let class = object.class();
        let title = object
            .get("fTitle")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let key_len = KeyHeader::encoded_len(class, name, title, self.end.is_large());
        let mut w = WBuffer::with_origin(
            u32::try_from(key_len).map_err(|_| Error::malformed("key header too long"))?,
        );
        let mut encoder = Encoder::new(streamer::builtin());
        encoder
            .top(&mut w, object)
            .map_err(|e| e.with_context(crate::Context::Key(name.to_owned())))?;
        let used = encoder.into_used();
        for c in used.classes {
            if !self.classes.contains(&c) {
                self.classes.push(c);
            }
        }
        for i in used.infos {
            if !self
                .infos
                .iter()
                .any(|j| j.class_name() == i.class_name() && j.class_version() == i.class_version())
            {
                self.infos.push(i);
            }
        }
        if let Some(s) = used.source
            && !self.sources.iter().any(|t| Arc::ptr_eq(&s, t))
        {
            self.sources.push(s);
        }
        let cycle = self.next_cycle(dir, name)?;
        let compression = self.options.compression;
        let key =
            self.append_record(dir, class, name, title, &w.into_inner(), compression, cycle)?;
        self.dirs[dir].keys.push(key);
        self.dirs[dir].dirty = true;
        Ok(())
    }

    /// Appends a record at the end of the file and returns its key.
    #[expect(clippy::too_many_arguments, reason = "a key's fields")]
    fn append_record(
        &mut self,
        dir: usize,
        class: &str,
        name: &str,
        title: &str,
        object: &[u8],
        compression: Compression,
        cycle: i16,
    ) -> Result<KeyHeader> {
        let stored = compress::compress(compression, object)?;
        let data = stored.as_deref().unwrap_or(object);
        let obj_len = u32::try_from(object.len())
            .map_err(|_| Error::new(ErrorKind::LimitExceeded, "object too large"))?;
        let data_len = u32::try_from(data.len())
            .map_err(|_| Error::new(ErrorKind::LimitExceeded, "object too large"))?;
        let key = KeyHeader::for_record(
            KeyForm::ByPosition,
            self.end,
            self.dirs[dir].record.seek_dir,
            class,
            name,
            title,
            obj_len,
            data_len,
            cycle,
            self.now(),
        )?;
        let mut w = WBuffer::new();
        key.encode(&mut w)?;
        w.write_bytes(data);
        self.write_at_end(&w.into_inner(), key.nbytes)?;
        Ok(key)
    }

    /// Writes a directory's key list (if it has keys) and returns where it
    /// is and its size; `(0, 0)` for an empty directory, as ROOT writes it.
    fn write_key_list(&mut self, dir: usize, pdir: FileOffset) -> Result<(FileOffset, u32)> {
        let d = &self.dirs[dir];
        if d.keys.is_empty() && d.parent.is_some() {
            return Ok((FileOffset::new(0), 0));
        }
        let mut w = WBuffer::new();
        encode_key_list(&d.keys, &mut w)?;
        let (class, name, title) = if d.parent.is_some() {
            (DIRECTORY_CLASS, d.name.clone(), d.title.clone())
        } else {
            ("TFile", self.name.clone(), self.options.title.clone())
        };
        let mut key = self.append_record(
            dir,
            class,
            &name,
            &title,
            &w.into_inner(),
            Compression::NONE,
            1,
        )?;
        if key.seek_pdir != pdir {
            // A directory moved by an update: its list names the new place.
            key.seek_pdir = pdir;
            let mut w = WBuffer::new();
            key.encode(&mut w)?;
            self.sink.write_all_at(key.seek_key, &w.into_inner())?;
        }
        Ok((key.seek_key, key.nbytes))
    }

    /// The class descriptions this session's objects need, with what they
    /// depend on.
    fn needed_infos(&self) -> Vec<Arc<StreamerInfo>> {
        let mut sets: Vec<&StreamerSet> = self.sources.iter().map(AsRef::as_ref).collect();
        sets.push(streamer::builtin());
        streamer::closure(&self.infos, &sets, &self.classes)
    }

    /// Finishes trees and `RNTuple`s whose writers were dropped.
    fn finish_pending(&mut self) -> Result<()> {
        for tree in core::mem::take(&mut self.pending) {
            tree.finish(self)?;
        }
        for ntuple in core::mem::take(&mut self.pending_ntuples) {
            ntuple.finish(self)?;
        }
        Ok(())
    }

    /// Writes the bookkeeping records and the final header, then syncs; in
    /// update mode, commits the update.
    ///
    /// For a new file the order is ROOT's: `StreamerInfo`, key lists
    /// and directory records, free list, header.
    ///
    /// # Errors
    ///
    /// I/O errors of the sink. On error a new file stays marked "not
    /// closed", and an update is not committed.
    pub fn close(mut self) -> Result<()> {
        self.finish_pending()?;
        if self.update.is_some() {
            return update::commit(self);
        }
        // StreamerInfo: the descriptions of every class written, and of
        // what they depend on.
        let (info_class, info_name, info_title) = ("TList", "StreamerInfo", "Doubly linked list");
        let info = if self.classes.is_empty() {
            empty_tlist()?
        } else {
            let origin =
                KeyHeader::encoded_len(info_class, info_name, info_title, self.end.is_large());
            let infos = self.needed_infos();
            let infos: Vec<&StreamerInfo> = infos.iter().map(AsRef::as_ref).collect();
            streamer::encode_list(
                &infos,
                u32::try_from(origin).map_err(|_| Error::malformed("key header too long"))?,
            )?
        };
        let compression = self.options.compression;
        let info_key =
            self.append_record(0, info_class, info_name, info_title, &info, compression, 1)?;

        // Key lists, then directory records, rewritten in place.
        for dir in 0..self.dirs.len() {
            let pdir = self.dirs[dir].record.seek_dir;
            let (seek, nbytes) = self.write_key_list(dir, pdir)?;
            let now = self.now();
            let d = &mut self.dirs[dir];
            d.record.seek_keys = seek;
            d.record.nbytes_keys = nbytes;
            d.record.modified = now;
            let mut w = WBuffer::new();
            d.record.encode(&mut w)?;
            let at = d.record_at()?;
            self.sink.write_all_at(at, &w.into_inner())?;
        }

        // Free list: one segment from the end of the file to kStartBigFile.
        let (seek_free, nbytes_free, nfree) = self.write_free_list(&[])?;
        let header = self.header(
            seek_free,
            nbytes_free,
            info_key.seek_key,
            info_key.nbytes,
            nfree,
        )?;
        self.sink.write_all_at(FileOffset::new(0), &header)?;
        self.sink.flush()?;
        match self.sink.sync() {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Writes the free list at the end of the file: `segments`, then one
    /// from the end of the file (the free list included) to
    /// `kStartBigFile`. Returns where it is, its size and the segment
    /// count.
    fn write_free_list(&mut self, segments: &[FreeSegment]) -> Result<(FileOffset, u32, u32)> {
        let seek_free = self.end;
        let (name, title) = (self.name.clone(), self.options.title.clone());
        let tail = |first: FileOffset| FreeSegment {
            first,
            last: FileOffset::START_BIG_FILE.max(first),
        };
        let mut all = segments.to_vec();
        all.push(tail(seek_free));
        let key_len = KeyHeader::encoded_len("TFile", &name, &title, seek_free.is_large());
        let free_end = seek_free
            .checked_add(
                u64::try_from(key_len + free_list_len(&all))
                    .map_err(|_| Error::malformed("free list"))?,
            )
            .ok_or_else(|| Error::malformed("free list"))?;
        let last = all.len() - 1;
        all[last] = tail(free_end);
        let mut w = WBuffer::new();
        encode_free_list(&all, &mut w)?;
        let key = self.append_record(
            0,
            "TFile",
            &name,
            &title,
            &w.into_inner(),
            Compression::NONE,
            1,
        )?;
        debug_assert_eq!(self.end, free_end, "free-list fixed point");
        Ok((
            key.seek_key,
            key.nbytes,
            u32::try_from(all.len()).map_err(|_| Error::malformed("free list"))?,
        ))
    }
}

/// The branch names of a derived row.
fn row_names<R: Row>() -> Result<Vec<String>> {
    let names = R::__names();
    if names.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "a tuple row needs branch names: use `tree_of`",
        ));
    }
    Ok(names.into_iter().map(str::to_owned).collect())
}

/// An empty `TList`: byte count + version 5, `TObject`, name, `nobjects = 0`.
fn empty_tlist() -> Result<Vec<u8>> {
    let mut w = WBuffer::new();
    w.object(5, |w| {
        w.write_i16(1); // TObject version
        w.write_u32(0); // fUniqueID
        w.write_u32(0x0300_0000); // fBits: kIsOnHeap | kNotDeleted
        w.write_string("")?; // fName
        w.write_i32(0); // nobjects
        Ok(())
    })?;
    Ok(w.into_inner())
}
